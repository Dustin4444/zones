//! Reth ExEx catch-up and steady-state notification loop.

mod driver;
mod operational;
mod retry;
mod startup;
mod status;
mod terminal;

use std::future::Future;

use reth_chainspec::EthChainSpec as _;
use reth_exex::ExExContext;
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_storage_api::{BlockReader, StateProviderFactory};
use tempo_primitives::{Block, TempoPrimitives};
use tracing::info;

use crate::CheckerConfig;
#[cfg(test)]
use crate::metrics::BlockProcessingPhase;
#[cfg(feature = "test-utils")]
use crate::test_utils::CheckerTestHooks;

use super::state::ReadyToAcknowledge;
#[cfg(test)]
pub(super) use operational::promote_zone_replay_if_ready;
use operational::run_initialized;
pub(crate) use status::RuntimeStatus;
use terminal::run_disabled;

#[cfg(test)]
use {
    super::{PersistentChecker, RuntimeResult, apply::L1Client},
    driver::{RetainedContext, RetainedNotificationDriver, RetainedOutcome, process_retained},
    reth_exex::ExExNotification,
};

/// Perform deterministic local preflight in Reth's outer initializer and
/// return the non-resolving inner worker only after it succeeds.
pub(crate) fn launch<Node>(
    config: CheckerConfig,
    #[cfg(feature = "test-utils")] test_hooks: CheckerTestHooks,
    ctx: ExExContext<Node>,
) -> eyre::Result<impl Future<Output = eyre::Result<()>> + Send>
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    startup::preflight(&config, &ctx)?;
    Ok(run(
        config,
        #[cfg(feature = "test-utils")]
        test_hooks,
        ctx,
    ))
}

/// The future returned to Reth must never resolve: pinned Reth treats either
/// `Ok` or `Err` from an inner ExEx as a critical-task panic.
async fn run<Node>(
    config: CheckerConfig,
    #[cfg(feature = "test-utils")] mut test_hooks: CheckerTestHooks,
    mut ctx: ExExContext<Node>,
) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    let mut status = RuntimeStatus::new();
    let initialized = match startup::initialize(&config, &mut ctx, &mut status).await {
        Ok(initialized) => initialized,
        Err(exit) => {
            return run_disabled(&mut ctx, &mut status, exit).await;
        }
    };
    #[cfg(feature = "test-utils")]
    test_hooks.wait_for_start().await;
    let exit = run_initialized(
        &config,
        #[cfg(feature = "test-utils")]
        &test_hooks,
        initialized,
        &mut ctx,
        &mut status,
    )
    .await;
    run_disabled(&mut ctx, &mut status, exit).await
}

fn log_started<Node>(
    config: &CheckerConfig,
    ctx: &ExExContext<Node>,
    startup: ReadyToAcknowledge,
    path: &std::path::Path,
) where
    Node: FullNodeComponents,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    info!(
        target: "zone::checker",
        tip = ?startup.tip(),
        alerting = startup.is_alerting(),
        zone_id = config.zone_id,
        zone_chain_id = ctx.config.chain.chain().id(),
        portal = %config.portal_address,
        portal_creation_block_hash = %config.portal_creation_block_hash,
        database = %path.display(),
        "Durable checker started"
    );
}

#[cfg(test)]
pub(crate) async fn process_retained_notification<S>(
    checker: &mut PersistentChecker,
    notification: &ExExNotification<TempoPrimitives>,
    zone_state: &S,
    l1_client: &mut L1Client,
    operational: bool,
    status: &mut RuntimeStatus,
) -> RuntimeResult<ReadyToAcknowledge>
where
    S: crate::observe::ExactStateLookup + ?Sized,
{
    let mut driver = RetainedNotificationDriver::new();
    let mut stream = futures::stream::pending::<
        Result<ExExNotification<TempoPrimitives>, std::convert::Infallible>,
    >();
    let processing_phase = if operational {
        BlockProcessingPhase::Live
    } else {
        BlockProcessingPhase::CatchUp
    };
    let retained = RetainedContext::new(checker, zone_state, l1_client, status, processing_phase);
    match process_retained(retained, &mut driver, &mut stream, notification).await {
        RetainedOutcome::Ready { ready, recovered } => {
            status.record_ready(ready, recovered, operational);
            Ok(ready)
        }
        RetainedOutcome::Terminal(failure) => Err(failure),
        RetainedOutcome::StreamClosed => unreachable!("pending test stream cannot close"),
    }
}

#[cfg(test)]
mod tests;
