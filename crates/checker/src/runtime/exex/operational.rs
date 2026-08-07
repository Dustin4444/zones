//! Steady-state checker state machine and acknowledgement boundary.

use reth_exex::{ExExContext, ExExNotification};
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_storage_api::{BlockReader, StateProviderFactory};
use tempo_primitives::{Block, TempoPrimitives};
use tokio::time::Instant;
use tracing::{info, warn};

#[cfg(feature = "test-utils")]
use crate::test_utils::TestProgressObserver;
use crate::{CheckerConfig, store::value::BootstrapState};

use super::{
    RuntimeStatus,
    driver::{
        RetainedContext, RetainedNotificationDriver, RetainedOutcome, StreamEvent,
        process_retained, report_stream_failure,
    },
    log_started,
    retry::RetryBackoff,
    startup::Initialized,
    terminal::{RuntimeExit, stream_closed, stream_closed_after_notification},
};
use crate::runtime::{
    PersistentChecker, RuntimeError, RuntimeResult, apply::L1Client, state::ReadyToAcknowledge,
};

struct LoopState {
    checker: PersistentChecker,
    l1_client: L1Client,
    last_ready: ReadyToAcknowledge,
    live: bool,
    catching_up: bool,
    head_retry: RetryBackoff,
    head_probe_at: Option<Instant>,
}

enum NextNotification {
    Notification(ExExNotification<TempoPrimitives>),
    StreamClosed,
    Terminal(RuntimeError),
}

enum NotificationFailure {
    StreamClosed,
    Terminal(RuntimeError),
    Acknowledgement {
        failure: eyre::Report,
        tip: alloy_eips::BlockNumHash,
    },
}

pub(super) async fn run_initialized<Node>(
    config: &CheckerConfig,
    #[cfg(feature = "test-utils")] test_progress: &TestProgressObserver,
    initialized: Initialized,
    ctx: &mut ExExContext<Node>,
    status: &mut RuntimeStatus,
) -> RuntimeExit
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    let Initialized {
        checker,
        l1_client,
        startup,
        path,
    } = initialized;
    let zone_provider = ctx.provider().clone();
    let mut state = match LoopState::new(checker, l1_client, startup, &zone_provider, status) {
        Ok(state) => state,
        Err(failure) => return RuntimeExit::terminal(failure),
    };
    let mut driver = RetainedNotificationDriver::new();

    if let Err(failure) = ctx.send_finished_height(startup.tip()) {
        return RuntimeExit::with_ack_and_driver(failure, startup.tip(), driver);
    }
    #[cfg(feature = "test-utils")]
    test_progress.publish(startup.tip());
    state.record_operational(status, false);
    log_started(config, ctx, startup, &path);

    loop {
        let notification = match driver.pop_front() {
            Some(notification) => notification,
            None => match next_notification(&mut state, &zone_provider, &mut driver, ctx, status)
                .await
            {
                NextNotification::Notification(notification) => notification,
                NextNotification::StreamClosed => {
                    return stream_closed(&state.checker, state.live, state.catching_up, driver);
                }
                NextNotification::Terminal(failure) => {
                    return RuntimeExit::with_driver(failure, driver);
                }
            },
        };

        match process_notification(
            &mut state,
            &zone_provider,
            &notification,
            &mut driver,
            ctx,
            status,
            #[cfg(feature = "test-utils")]
            test_progress,
        )
        .await
        {
            Ok(()) => {}
            Err(NotificationFailure::Terminal(failure)) => {
                return RuntimeExit::after_notification(failure, &notification, driver);
            }
            Err(NotificationFailure::StreamClosed) => {
                return stream_closed_after_notification(
                    &state.checker,
                    state.live,
                    true,
                    &notification,
                    driver,
                );
            }
            Err(NotificationFailure::Acknowledgement { failure, tip }) => {
                return RuntimeExit::with_ack_and_driver(failure, tip, driver);
            }
        }
    }
}

impl LoopState {
    fn new<P>(
        checker: PersistentChecker,
        l1_client: L1Client,
        startup: ReadyToAcknowledge,
        provider: &P,
        status: &mut RuntimeStatus,
    ) -> RuntimeResult<Self>
    where
        P: reth_storage_api::BlockNumReader + ?Sized,
    {
        let live = checker.store.load_progress()?.bootstrap == BootstrapState::Live;
        let mut state = Self {
            checker,
            l1_client,
            last_ready: startup,
            live,
            catching_up: true,
            head_retry: RetryBackoff::new(),
            head_probe_at: None,
        };
        if let Err(failure) = state.refresh_catch_up_state(provider) {
            if !failure.is_retryable() {
                return Err(failure);
            }
            state.head_probe_at = Some(schedule_head_retry(
                &mut state.head_retry,
                status,
                state.last_ready.is_alerting(),
                &failure,
            ));
        }
        Ok(state)
    }

    fn refresh_catch_up_state<P>(&mut self, provider: &P) -> RuntimeResult<()>
    where
        P: reth_storage_api::BlockNumReader + ?Sized,
    {
        let head = provider
            .chain_info()
            .map(Into::into)
            .map_err(|source| RuntimeError::LocalCanonicalHeadRead { source })?;
        self.catching_up = self.last_ready.tip() != head;
        if !self.live && promote_zone_replay_if_ready(&mut self.checker, self.last_ready, head)? {
            self.live = true;
        }
        Ok(())
    }

    fn refresh_after_notification<P>(
        &mut self,
        provider: &P,
        status: &mut RuntimeStatus,
        stream_unavailable: bool,
    ) -> RuntimeResult<()>
    where
        P: reth_storage_api::BlockNumReader + ?Sized,
    {
        if stream_unavailable {
            self.catching_up = true;
            return Ok(());
        }
        if (self.live && !self.catching_up) || self.head_probe_at.is_some() {
            return Ok(());
        }
        if let Err(failure) = self.refresh_catch_up_state(provider) {
            if !failure.is_retryable() {
                return Err(failure);
            }
            self.head_probe_at = Some(schedule_head_retry(
                &mut self.head_retry,
                status,
                self.last_ready.is_alerting(),
                &failure,
            ));
        }
        Ok(())
    }

    fn retry_head_probe<P>(
        &mut self,
        provider: &P,
        status: &mut RuntimeStatus,
        stream_unavailable: bool,
    ) -> RuntimeResult<()>
    where
        P: reth_storage_api::BlockNumReader + ?Sized,
    {
        match self.refresh_catch_up_state(provider) {
            Ok(()) => {
                self.head_retry.reset();
                self.head_probe_at = None;
                self.record_operational(status, stream_unavailable);
                Ok(())
            }
            Err(failure) if failure.is_retryable() => {
                self.head_probe_at = Some(schedule_head_retry(
                    &mut self.head_retry,
                    status,
                    self.last_ready.is_alerting(),
                    &failure,
                ));
                Ok(())
            }
            Err(failure) => Err(failure),
        }
    }

    fn record_operational(&self, status: &mut RuntimeStatus, stream_unavailable: bool) {
        if self.live && !self.catching_up && !stream_unavailable {
            status.mark_started(self.last_ready.is_alerting());
        } else {
            status.mark_bootstrapping(self.last_ready.is_alerting());
        }
    }
}

async fn next_notification<Node>(
    state: &mut LoopState,
    provider: &Node::Provider,
    driver: &mut RetainedNotificationDriver<ExExNotification<TempoPrimitives>>,
    ctx: &mut ExExContext<Node>,
    status: &mut RuntimeStatus,
) -> NextNotification
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    loop {
        tokio::select! {
            biased;
            event = driver.poll(&mut ctx.notifications) => match event {
                StreamEvent::Notification(notification) => {
                    return NextNotification::Notification(notification);
                }
                StreamEvent::Closed => return NextNotification::StreamClosed,
                StreamEvent::Failed { attempt, source } => {
                    state.catching_up = true;
                    if let Err(failure) =
                        report_stream_failure(&state.checker, status, attempt, &source)
                    {
                        return NextNotification::Terminal(failure);
                    }
                    state.record_operational(status, true);
                }
            },
            () = wait_until(state.head_probe_at), if state.head_probe_at.is_some() => {
                if let Err(failure) = state.retry_head_probe(
                    provider,
                    status,
                    driver.stream_unavailable(),
                ) {
                    return NextNotification::Terminal(failure);
                }
            }
        }
    }
}

async fn process_notification<Node>(
    state: &mut LoopState,
    provider: &Node::Provider,
    notification: &ExExNotification<TempoPrimitives>,
    driver: &mut RetainedNotificationDriver<ExExNotification<TempoPrimitives>>,
    ctx: &mut ExExContext<Node>,
    status: &mut RuntimeStatus,
    #[cfg(feature = "test-utils")] test_progress: &TestProgressObserver,
) -> Result<(), NotificationFailure>
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    let retained = RetainedContext::new(&mut state.checker, provider, &mut state.l1_client, status);
    let ready = match process_retained(retained, driver, &mut ctx.notifications, notification).await
    {
        RetainedOutcome::Ready { ready, recovered } => (ready, recovered),
        RetainedOutcome::Terminal(failure) => {
            return Err(NotificationFailure::Terminal(failure));
        }
        RetainedOutcome::StreamClosed => return Err(NotificationFailure::StreamClosed),
    };

    let (ready, recovered) = ready;
    state.last_ready = ready;
    status.mark_bootstrapping(ready.is_alerting());
    state
        .refresh_after_notification(provider, status, driver.stream_unavailable())
        .map_err(NotificationFailure::Terminal)?;
    let operational = state.live && !state.catching_up && !driver.stream_unavailable();
    status.record_ready(ready, recovered, operational);
    ctx.send_finished_height(ready.tip()).map_err(|failure| {
        NotificationFailure::Acknowledgement {
            failure: failure.into(),
            tip: ready.tip(),
        }
    })?;
    #[cfg(feature = "test-utils")]
    test_progress.publish(ready.tip());
    Ok(())
}

fn schedule_head_retry(
    retry: &mut RetryBackoff,
    status: &mut RuntimeStatus,
    alerting: bool,
    failure: &RuntimeError,
) -> Instant {
    let (attempt, delay) = retry.fail();
    status.record_retry(alerting);
    warn!(
        target: "zone::checker",
        attempt,
        delay_ms = delay.as_millis(),
        error = %failure,
        "Checker canonical head unavailable; retrying without blocking notifications"
    );
    Instant::now() + delay
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

pub(in crate::runtime) fn promote_zone_replay_if_ready(
    checker: &mut PersistentChecker,
    ready: ReadyToAcknowledge,
    canonical_head: alloy_eips::BlockNumHash,
) -> RuntimeResult<bool> {
    let progress = checker.store.load_progress()?;
    let BootstrapState::ZoneReplay { .. } = progress.bootstrap else {
        return Ok(false);
    };
    if ready.is_alerting()
        || ready.tip() != canonical_head
        || progress.verified_zone_tip != canonical_head
    {
        return Ok(false);
    }

    let transition = checker
        .store
        .enter_live(progress.bootstrap, progress.imported_tempo_tip)?;
    checker.store.apply_bootstrap(transition)?;
    info!(
        target: "zone::checker",
        verified_tip = ?progress.verified_zone_tip,
        acknowledged_tip = ?ready.tip(),
        "Checker archive replay reached live handoff"
    );
    Ok(true)
}
