//! Reth ExEx startup, catch-up, retry, and acknowledgement loop.

use std::time::Duration;

use futures::TryStreamExt as _;
use reth_exex::{ExExContext, ExExHead};
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_storage_api::{BlockNumReader, StateProviderFactory};
use tempo_primitives::TempoPrimitives;
use tracing::{error, info, warn};

use crate::{metrics::CheckerRuntimeMetrics, observe::ExactStateLookup, store::db::CheckerStore};

use super::{LiveChecker, RuntimeResult, apply::L1Client, state::ReadyToAcknowledge};

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

pub(crate) struct RuntimeStatus {
    metrics: CheckerRuntimeMetrics,
    #[cfg(test)]
    active_alert: bool,
}

impl RuntimeStatus {
    pub(crate) fn new() -> Self {
        let metrics = CheckerRuntimeMetrics::default();
        metrics.healthy.set(0.0);
        metrics.active_alert.set(0.0);
        Self {
            metrics,
            #[cfg(test)]
            active_alert: false,
        }
    }

    pub(crate) fn mark_started(&mut self, alerting: bool) {
        self.record_ready_state(alerting, false);
    }

    fn mark_stopped(&self) {
        self.metrics.healthy.set(0.0);
    }

    pub(crate) fn record_retry(&mut self, alerting: bool) {
        self.record_unhealthy(alerting);
        self.metrics.operational_retries_total.increment(1);
    }

    fn record_terminal(&mut self, alerting: bool) {
        self.record_unhealthy(alerting);
    }

    fn record_ready(&mut self, ready: ReadyToAcknowledge, recovered: bool) {
        self.record_ready_state(ready.is_alerting(), recovered);
    }

    fn record_ready_state(&mut self, alerting: bool, recovered: bool) {
        self.record_alert(alerting);
        self.metrics.healthy.set(if alerting { 0.0 } else { 1.0 });
        if recovered && !alerting {
            self.metrics.operational_recoveries_total.increment(1);
        }
    }

    fn record_unhealthy(&mut self, alerting: bool) {
        self.record_alert(alerting);
        self.metrics.healthy.set(0.0);
    }

    fn record_alert(&mut self, alerting: bool) {
        #[cfg(test)]
        {
            self.active_alert = alerting;
        }
        self.metrics
            .active_alert
            .set(if alerting { 1.0 } else { 0.0 });
    }

    #[cfg(test)]
    pub(crate) fn is_alerting(&self) -> bool {
        self.active_alert
    }
}

impl Drop for RuntimeStatus {
    fn drop(&mut self) {
        self.mark_stopped();
    }
}

/// Retain one delivered notification until acquisition succeeds or a
/// deterministic failure stops the checker.
pub(crate) async fn process_retained_notification<S>(
    checker: &mut LiveChecker,
    notification: &reth_exex::ExExNotification<TempoPrimitives>,
    zone_state: &S,
    l1_client: &mut L1Client,
    status: &mut RuntimeStatus,
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
                let recovered = retry_attempt != 0;
                status.record_ready(ready, recovered);
                if recovered && !ready.is_alerting() {
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
                status.record_retry(checker.is_alerting());
                warn!(
                    target: "zone::checker",
                    attempt = retry_attempt,
                    delay_ms = delay.as_millis(),
                    error = %failure,
                    "Checker acquisition unavailable; retaining notification"
                );
                tokio::time::sleep(delay).await;
            }
            Err(failure) => {
                status.record_terminal(checker.is_alerting());
                return Err(failure);
            }
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
    Node::Provider: BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    let mut status = RuntimeStatus::new();
    let mut checker = LiveChecker::from_store(store)?;
    let startup = checker.reconcile_startup(ctx.provider())?;
    ctx.catch_up_notifications_with_head(ExExHead::new(startup.tip()))?;
    ctx.send_finished_height(startup.tip())?;
    status.mark_started(startup.is_alerting());

    info!(
        target: "zone::checker",
        tip = ?startup.tip(),
        alerting = startup.is_alerting(),
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
            &mut status,
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
