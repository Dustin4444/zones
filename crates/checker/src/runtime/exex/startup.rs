use std::{path::PathBuf, time::Duration};

use alloy_eips::BlockNumHash;
use futures::TryStreamExt as _;
use reth_chainspec::EthChainSpec as _;
use reth_exex::{ExExContext, ExExHead};
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_storage_api::{BlockReader, StateProviderFactory};
use tempo_primitives::{Block, TempoPrimitives};
use tracing::{info, warn};

use crate::{
    CheckerConfig,
    runtime::{
        PersistentChecker, RuntimeError, RuntimeResult,
        apply::L1Client,
        bootstrap::{
            DatabaseState, create_fresh, database_path, inspect_database, open_existing,
            resume_l1_replay, validate_local_configuration,
        },
        state::ReadyToAcknowledge,
    },
    store::value::BootstrapState,
};

use super::{
    retry::RetryBackoff,
    status::RuntimeStatus,
    terminal::{RuntimeExit, acknowledgement_tip},
};

pub(super) struct Initialized {
    pub(super) checker: PersistentChecker,
    pub(super) l1_client: L1Client,
    pub(super) startup: ReadyToAcknowledge,
    pub(super) path: PathBuf,
}

#[derive(Default)]
struct DrainedNotifications {
    count: u64,
    acknowledge: Option<BlockNumHash>,
}

/// Reject deterministic local configuration, database-version, and Zone
/// identity failures from Reth's outer ExEx initializer. Remote bootstrap and
/// retryable acquisition remain owned by the non-resolving inner worker.
pub(super) fn preflight<Node>(config: &CheckerConfig, ctx: &ExExContext<Node>) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    let path = database_path(config, ctx.config.datadir().data_dir());
    inspect_database(&path)?;
    validate_local_configuration(config, ctx.config.chain.chain().id(), ctx.provider())?;
    Ok(())
}

/// Bootstrap and reconcile the durable cut while draining the raw live stream,
/// then atomically switch that stream to Reth's canonical catch-up mode.
/// Nothing may discard a notification after this function returns.
pub(super) async fn initialize<Node>(
    config: &CheckerConfig,
    ctx: &mut ExExContext<Node>,
    status: &mut RuntimeStatus,
) -> Result<Initialized, RuntimeExit>
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    let path = database_path(config, ctx.config.datadir().data_dir());
    let mut drained = DrainedNotifications::default();
    // Fail on version or codec incompatibility before touching the remote L1.
    inspect_database(&path).map_err(|failure| startup_exit(failure, &drained))?;

    let zone_chain_id = ctx.config.chain.chain().id();
    let zone_provider = ctx.provider().clone();
    let (checker, l1_client, startup) = {
        let prepare = prepare_durable_cut(config, zone_chain_id, &path, &zone_provider, status);
        tokio::pin!(prepare);

        loop {
            tokio::select! {
                result = &mut prepare => match result {
                    Ok(initialized) => break initialized,
                    Err(failure) => return Err(startup_exit(failure, &drained)),
                },
                notification = ctx.notifications.try_next() => {
                    if let Err(failure) = drain_one(notification, &mut drained) {
                        return Err(startup_exit(failure, &drained));
                    }
                }
            }
        }
    };

    let mut catch_up_retry = RetryBackoff::new();
    loop {
        match ctx.catch_up_notifications_with_head(ExExHead::new(startup.tip())) {
            Ok(()) => break,
            Err(source) => {
                let (attempt, delay) = catch_up_retry.fail();
                status.record_retry(startup.is_alerting());
                warn!(
                    target: "zone::checker",
                    attempt,
                    delay_ms = delay.as_millis(),
                    error = %source,
                    "Checker could not initialize canonical archive catch-up; retrying"
                );
                if let Err(failure) = drain_for(ctx, delay, &mut drained).await {
                    return Err(startup_exit(failure, &drained));
                }
            }
        }
    }

    if drained.count != 0 {
        info!(
            target: "zone::checker",
            discarded_notifications = drained.count,
            "Checker drained live notifications before canonical catch-up installation"
        );
    }

    Ok(Initialized {
        checker,
        l1_client,
        startup,
        path,
    })
}

async fn prepare_durable_cut<P>(
    config: &CheckerConfig,
    zone_chain_id: u64,
    path: &std::path::Path,
    zone_provider: &P,
    status: &mut RuntimeStatus,
) -> RuntimeResult<(PersistentChecker, L1Client, ReadyToAcknowledge)>
where
    P: BlockReader<Block = Block> + StateProviderFactory + ?Sized,
{
    let (mut checker, l1_client) =
        open_durable_cut(config, zone_chain_id, path, zone_provider, status).await?;
    let startup = reconcile_durable_cut(&mut checker, zone_provider, status).await?;
    Ok((checker, l1_client, startup))
}

async fn open_durable_cut<P>(
    config: &CheckerConfig,
    zone_chain_id: u64,
    path: &std::path::Path,
    zone_provider: &P,
    status: &mut RuntimeStatus,
) -> RuntimeResult<(PersistentChecker, L1Client)>
where
    P: BlockReader<Block = Block> + StateProviderFactory + ?Sized,
{
    let mut retry = RetryBackoff::new();
    loop {
        match open_durable_cut_once(config, zone_chain_id, path, zone_provider).await {
            Ok(initialized) => return Ok(initialized),
            Err(failure) if failure.is_retryable() => {
                let (attempt, delay) = retry.fail();
                status.record_retry(false);
                warn!(
                    target: "zone::checker",
                    attempt,
                    delay_ms = delay.as_millis(),
                    error = %failure,
                    "Checker bootstrap acquisition unavailable; retaining archive progress"
                );
                tokio::time::sleep(delay).await;
            }
            Err(failure) => return Err(failure),
        }
    }
}

async fn open_durable_cut_once<P>(
    config: &CheckerConfig,
    zone_chain_id: u64,
    path: &std::path::Path,
    zone_provider: &P,
) -> RuntimeResult<(PersistentChecker, L1Client)>
where
    P: BlockReader<Block = Block> + StateProviderFactory + ?Sized,
{
    match inspect_database(path)? {
        DatabaseState::Existing(stored) => {
            let mut checker = open_existing(config, zone_chain_id, path, stored, zone_provider)?;
            let mut l1_client =
                L1Client::for_chain(config.l1_rpc_url.clone(), stored.l1_chain_id());
            if matches!(
                checker.store.load_progress()?.bootstrap,
                BootstrapState::L1Replay { .. }
            ) {
                let l1_provider = l1_client.provider().await?;
                resume_l1_replay(config, zone_provider, &l1_provider, &mut checker).await?;
            }
            Ok((checker, l1_client))
        }
        DatabaseState::Fresh => {
            let mut l1_client = L1Client::new(config.l1_rpc_url.clone());
            let l1_provider = l1_client.provider().await?;
            let checker =
                create_fresh(config, zone_chain_id, path, zone_provider, &l1_provider).await?;
            l1_client.bind_validated_chain_id(checker.store.l1_chain_id());
            Ok((checker, l1_client))
        }
    }
}

async fn reconcile_durable_cut<P>(
    checker: &mut PersistentChecker,
    zone_provider: &P,
    status: &mut RuntimeStatus,
) -> RuntimeResult<ReadyToAcknowledge>
where
    P: reth_storage_api::BlockNumReader + ?Sized,
{
    status.mark_bootstrapping(checker.is_alerting());
    let mut retry = RetryBackoff::new();
    loop {
        match checker.reconcile_startup(zone_provider) {
            Ok(startup) => return Ok(startup),
            Err(failure) if failure.is_retryable() => {
                let (attempt, delay) = retry.fail();
                status.record_retry(checker.is_alerting());
                warn!(
                    target: "zone::checker",
                    attempt,
                    delay_ms = delay.as_millis(),
                    error = %failure,
                    "Checker startup reconciliation unavailable; retaining durable progress"
                );
                tokio::time::sleep(delay).await;
            }
            Err(failure) => return Err(failure),
        }
    }
}

async fn drain_for<Node>(
    ctx: &mut ExExContext<Node>,
    delay: Duration,
    drained: &mut DrainedNotifications,
) -> RuntimeResult<()>
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            () = &mut sleep => return Ok(()),
            notification = ctx.notifications.try_next() => drain_one(notification, drained)?,
        }
    }
}

fn drain_one(
    notification: eyre::Result<Option<reth_exex::ExExNotification<TempoPrimitives>>>,
    drained: &mut DrainedNotifications,
) -> RuntimeResult<()> {
    let notification =
        notification.map_err(|source| RuntimeError::BootstrapNotificationStream { source })?;
    let notification = notification.ok_or(RuntimeError::NotificationStreamClosedDuringBootstrap)?;
    drained.count = drained.count.saturating_add(1);
    drained.acknowledge = Some(acknowledgement_tip(&notification)?);
    Ok(())
}

fn startup_exit(failure: impl Into<eyre::Report>, drained: &DrainedNotifications) -> RuntimeExit {
    RuntimeExit::after_startup(failure, drained.acknowledge)
}

#[cfg(test)]
mod tests;
