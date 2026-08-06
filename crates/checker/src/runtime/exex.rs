//! Reth ExEx startup, catch-up, retry, and acknowledgement loop.

use std::time::Duration;

use alloy_eips::BlockNumHash;
use futures::TryStreamExt as _;
use reth_exex::{ExExContext, ExExHead};
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_storage_api::{BlockHashReader, StateProviderFactory};
use tempo_primitives::TempoPrimitives;
use tracing::{error, info, warn};

use crate::{metrics::CheckerRuntimeMetrics, observe::ExactStateLookup, store::db::CheckerStore};

use super::{
    LiveChecker, RuntimeError, RuntimeResult,
    apply::{L1Client, ReadyToAcknowledge},
};

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

pub(crate) struct RuntimeStatus {
    metrics: CheckerRuntimeMetrics,
}

impl RuntimeStatus {
    pub(crate) fn new() -> Self {
        let metrics = CheckerRuntimeMetrics::default();
        metrics.healthy.set(0.0);
        Self { metrics }
    }

    fn mark_started(&self) {
        self.metrics.healthy.set(1.0);
    }

    fn mark_stopped(&self) {
        self.metrics.healthy.set(0.0);
    }

    fn record_retry(&self) {
        self.metrics.healthy.set(0.0);
        self.metrics.operational_retries_total.increment(1);
    }

    fn record_recovery(&self) {
        self.metrics.operational_recoveries_total.increment(1);
        self.metrics.healthy.set(1.0);
    }
}

impl Drop for RuntimeStatus {
    fn drop(&mut self) {
        self.mark_stopped();
    }
}

pub(crate) fn validate_local_canonical_tip<P>(provider: &P, tip: BlockNumHash) -> RuntimeResult<()>
where
    P: BlockHashReader + ?Sized,
{
    let actual = provider
        .block_hash(tip.number)
        .map_err(|source| RuntimeError::LocalCanonicalRead { tip, source })?;
    match actual {
        None => Err(RuntimeError::MissingLocalCanonical(tip)),
        Some(actual) if actual != tip.hash => {
            Err(RuntimeError::LocalCanonicalConflict { tip, actual })
        }
        Some(_) => Ok(()),
    }
}

/// Retain one delivered notification until acquisition succeeds or a
/// deterministic failure stops the checker.
pub(crate) async fn process_retained_notification<S>(
    checker: &mut LiveChecker,
    notification: &reth_exex::ExExNotification<TempoPrimitives>,
    zone_state: &S,
    l1_client: &mut L1Client,
    status: &RuntimeStatus,
) -> RuntimeResult<ReadyToAcknowledge>
where
    S: ExactStateLookup + ?Sized,
{
    let mut retry_attempt = 0_u64;
    let mut next_delay = INITIAL_RETRY_DELAY;
    loop {
        match checker
            .process_notification_once(notification, zone_state, l1_client)
            .await
        {
            Ok(ready) => {
                if retry_attempt != 0 {
                    status.record_recovery();
                    info!(
                        target: "zone::checker",
                        attempts = retry_attempt,
                        tip = ?ready.tip(),
                        "Checker acquisition recovered"
                    );
                }
                return Ok(ready);
            }
            Err(failure) if failure.is_acquisition() => {
                retry_attempt = retry_attempt.saturating_add(1);
                let delay = next_delay;
                next_delay = next_delay.saturating_mul(2).min(MAX_RETRY_DELAY);
                status.record_retry();
                warn!(
                    target: "zone::checker",
                    attempt = retry_attempt,
                    delay_ms = delay.as_millis(),
                    error = %failure,
                    "Checker acquisition unavailable; retaining notification"
                );
                tokio::time::sleep(delay).await;
            }
            Err(failure) => return Err(failure),
        }
    }
}

/// Run the already-bootstrapped durable checker until its stream closes.
///
/// The caller owns construction and authenticated bootstrap of `store`; this
/// entry point refuses to invent either when the database is absent.
pub(crate) async fn run_persistent<Node>(
    l1_rpc_url: String,
    mut ctx: ExExContext<Node>,
    store: CheckerStore,
) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider: BlockHashReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    let status = RuntimeStatus::new();
    let mut checker = LiveChecker::from_store(store)?;
    // Construction loaded the mirror from this exact durable store cut.
    let durable_tip = checker.mirror_tip();
    validate_local_canonical_tip(ctx.provider(), durable_tip)?;
    ctx.catch_up_notifications_with_head(ExExHead::new(durable_tip))?;
    ctx.send_finished_height(durable_tip)?;
    status.mark_started();

    info!(
        target: "zone::checker",
        tip = ?durable_tip,
        "Persistent checker resumed from durable state"
    );

    let zone_provider = ctx.provider().clone();
    let mut l1_client = L1Client::new(l1_rpc_url);
    while let Some(notification) = ctx.notifications.try_next().await? {
        match process_retained_notification(
            &mut checker,
            &notification,
            &zone_provider,
            &mut l1_client,
            &status,
        )
        .await
        {
            Ok(ready) => ctx.send_finished_height(ready.tip())?,
            Err(failure) => {
                error!(
                    target: "zone::checker",
                    error = %failure,
                    "Persistent checker stopped on a non-retryable failure"
                );
                return Err(failure.into());
            }
        }
    }

    info!(target: "zone::checker", "Checker ExEx notification stream closed");
    Ok(())
}
