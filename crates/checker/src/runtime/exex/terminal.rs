//! Sticky fail-open policy for terminal checker failures.

use reth_exex::{ExExContext, ExExNotification};
use reth_node_api::{FullNodeComponents, NodeTypes};
use tempo_primitives::TempoPrimitives;
use tracing::{error, warn};

use super::{
    driver::{RetainedNotificationDriver, STREAM_POLL_RETRY_DELAY, StreamEvent},
    status::RuntimeStatus,
};
use crate::runtime::{
    PersistentChecker, RuntimeError, RuntimeResult,
    chain::{ValidatedChain, validate_reorg},
};

pub(super) struct RuntimeExit {
    failure: eyre::Report,
    acknowledge: Option<alloy_eips::BlockNumHash>,
    driver: RetainedNotificationDriver<ExExNotification<TempoPrimitives>>,
}

impl RuntimeExit {
    pub(super) fn terminal(failure: impl Into<eyre::Report>) -> Self {
        Self::after_startup(failure, None)
    }

    pub(super) fn after_startup(
        failure: impl Into<eyre::Report>,
        acknowledge: Option<alloy_eips::BlockNumHash>,
    ) -> Self {
        Self {
            failure: failure.into(),
            acknowledge,
            driver: RetainedNotificationDriver::new(),
        }
    }

    pub(super) fn with_ack_and_driver(
        failure: impl Into<eyre::Report>,
        acknowledge: alloy_eips::BlockNumHash,
        driver: RetainedNotificationDriver<ExExNotification<TempoPrimitives>>,
    ) -> Self {
        Self {
            failure: failure.into(),
            acknowledge: Some(acknowledge),
            driver,
        }
    }

    pub(super) fn with_driver(
        failure: impl Into<eyre::Report>,
        driver: RetainedNotificationDriver<ExExNotification<TempoPrimitives>>,
    ) -> Self {
        Self {
            failure: failure.into(),
            acknowledge: None,
            driver,
        }
    }

    pub(super) fn after_notification(
        failure: impl Into<eyre::Report>,
        notification: &ExExNotification<TempoPrimitives>,
        driver: RetainedNotificationDriver<ExExNotification<TempoPrimitives>>,
    ) -> Self {
        Self {
            failure: failure.into(),
            acknowledge: acknowledgement_tip(notification).ok(),
            driver,
        }
    }
}

#[cfg(test)]
impl RuntimeExit {
    pub(super) const fn acknowledgement_for_test(&self) -> Option<alloy_eips::BlockNumHash> {
        self.acknowledge
    }

    pub(super) fn pop_buffered_for_test(&mut self) -> Option<ExExNotification<TempoPrimitives>> {
        self.driver.pop_front()
    }
}

pub(super) async fn run_disabled<Node>(
    ctx: &mut ExExContext<Node>,
    status: &mut RuntimeStatus,
    exit: RuntimeExit,
) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    let alerting = status.is_alerting();
    status.record_terminal(alerting);
    error!(
        target: "zone::checker",
        error = %exit.failure,
        "Checker disabled after a terminal failure; continuing fail-open acknowledgement"
    );
    if let Some(tip) = exit.acknowledge
        && let Err(failure) = ctx.send_finished_height(tip)
    {
        error!(target: "zone::checker", %failure, ?tip, "Checker fail-open acknowledgement failed");
    }

    let mut driver = exit.driver;
    loop {
        let event = match driver.pop_front() {
            Some(notification) => StreamEvent::Notification(notification),
            None => driver.poll(&mut ctx.notifications).await,
        };
        match event {
            StreamEvent::Notification(notification) => match acknowledgement_tip(&notification) {
                Ok(tip) => {
                    if let Err(failure) = ctx.send_finished_height(tip) {
                        error!(target: "zone::checker", %failure, ?tip, "Checker fail-open acknowledgement failed");
                    }
                }
                Err(failure) => {
                    error!(target: "zone::checker", %failure, "Checker discarded malformed notification while disabled");
                }
            },
            StreamEvent::Failed { attempt, source } => {
                status.record_retry(alerting);
                warn!(
                    target: "zone::checker",
                    attempt,
                    retry_ms = STREAM_POLL_RETRY_DELAY.as_millis(),
                    error = %source,
                    "Disabled checker notification stream unavailable; continuing to poll"
                );
            }
            StreamEvent::Closed => {
                error!(target: "zone::checker", "Checker notification stream closed; parking critical ExEx task");
                std::future::pending::<()>().await;
            }
        }
    }
}

pub(super) fn stream_closed(
    checker: &PersistentChecker,
    live: bool,
    catching_up: bool,
    driver: RetainedNotificationDriver<ExExNotification<TempoPrimitives>>,
) -> RuntimeExit {
    if !live || catching_up || driver.stream_unavailable() {
        return match checker.store.load_progress() {
            Ok(progress) => RuntimeExit::with_driver(
                RuntimeError::BootstrapStreamClosed {
                    verified: progress.verified_zone_tip,
                },
                driver,
            ),
            Err(failure) => RuntimeExit::with_driver(failure, driver),
        };
    }
    RuntimeExit::with_driver(
        eyre::eyre!("checker ExEx notification stream closed"),
        driver,
    )
}

/// Preserve the item whose acquisition was interrupted by stream closure so
/// terminal fail-open acknowledges it before the already-buffered FIFO.
pub(super) fn stream_closed_after_notification(
    checker: &PersistentChecker,
    live: bool,
    catching_up: bool,
    notification: &ExExNotification<TempoPrimitives>,
    driver: RetainedNotificationDriver<ExExNotification<TempoPrimitives>>,
) -> RuntimeExit {
    let mut exit = stream_closed(checker, live, catching_up, driver);
    exit.acknowledge = acknowledgement_tip(notification).ok();
    exit
}

pub(super) fn acknowledgement_tip(
    notification: &ExExNotification<TempoPrimitives>,
) -> RuntimeResult<alloy_eips::BlockNumHash> {
    match notification {
        ExExNotification::ChainCommitted { new } => {
            Ok(ValidatedChain::new(new, "committed")?.tip())
        }
        ExExNotification::ChainReverted { old } => Ok(ValidatedChain::new(old, "reverted")?.base()),
        ExExNotification::ChainReorged { old, new } => Ok(validate_reorg(old, new)?.1.tip()),
    }
}

#[cfg(test)]
mod tests;
