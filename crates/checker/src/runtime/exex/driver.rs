//! Ordered buffering and retry cadence for the canonical notification stream.

use std::{collections::VecDeque, fmt::Display, time::Duration};

use futures::{TryStream, TryStreamExt as _};
use reth_exex::{DEFAULT_EXEX_MANAGER_CAPACITY, ExExNotification};
use tempo_primitives::TempoPrimitives;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::observe::ExactStateLookup;

use super::{retry::RetryBackoff, status::RuntimeStatus};
use crate::runtime::{
    PersistentChecker, RuntimeError, RuntimeResult, apply::L1Client, state::ReadyToAcknowledge,
};

/// Match Reth's manager buffer: transient checker acquisition can be decoupled
/// from the raw capacity-one stream without creating an unbounded memory sink.
pub(super) const STREAM_POLL_RETRY_DELAY: Duration = Duration::from_millis(100);

pub(super) enum StreamEvent<T, E> {
    Notification(T),
    Closed,
    Failed { attempt: u64, source: E },
}

pub(super) enum RetainedOutcome {
    Ready {
        ready: ReadyToAcknowledge,
        recovered: bool,
    },
    Terminal(RuntimeError),
    StreamClosed,
}

/// Stable dependencies for every retry of one retained canonical item.
pub(super) struct RetainedContext<'a, Z: ?Sized> {
    checker: &'a mut PersistentChecker,
    zone_state: &'a Z,
    l1_client: &'a mut L1Client,
    status: &'a mut RuntimeStatus,
}

impl<'a, Z: ?Sized> RetainedContext<'a, Z> {
    pub(super) fn new(
        checker: &'a mut PersistentChecker,
        zone_state: &'a Z,
        l1_client: &'a mut L1Client,
        status: &'a mut RuntimeStatus,
    ) -> Self {
        Self {
            checker,
            zone_state,
            l1_client,
            status,
        }
    }
}

/// Sole owner of notifications delivered while an earlier item is retained.
/// The front item is never displaced, so recovery preserves Reth order.
pub(super) struct RetainedNotificationDriver<T> {
    pending: VecDeque<T>,
    stream_retry_at: Option<Instant>,
    stream_attempts: u64,
    stream_unavailable: bool,
}

impl<T> RetainedNotificationDriver<T> {
    pub(super) const fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            stream_retry_at: None,
            stream_attempts: 0,
            stream_unavailable: false,
        }
    }

    pub(super) fn pop_front(&mut self) -> Option<T> {
        self.pending.pop_front()
    }

    pub(super) fn retain(&mut self, notification: T) {
        debug_assert!(self.has_capacity());
        self.pending.push_back(notification);
    }

    pub(super) fn has_capacity(&self) -> bool {
        self.pending.len() < DEFAULT_EXEX_MANAGER_CAPACITY
    }

    pub(super) const fn stream_unavailable(&self) -> bool {
        self.stream_unavailable
    }

    /// Poll the same Reth stream in place. Stream failures use a short fixed
    /// cadence because each poll is also what drains Reth's raw capacity-one
    /// receiver during archive backfill.
    pub(super) async fn poll<S, E>(&mut self, stream: &mut S) -> StreamEvent<T, E>
    where
        S: TryStream<Ok = T, Error = E> + Unpin,
    {
        if let Some(deadline) = self.stream_retry_at {
            tokio::time::sleep_until(deadline).await;
        }

        match stream.try_next().await {
            Ok(Some(notification)) => {
                self.stream_retry_at = None;
                self.stream_attempts = 0;
                self.stream_unavailable = false;
                StreamEvent::Notification(notification)
            }
            Ok(None) => StreamEvent::Closed,
            Err(source) => {
                self.stream_attempts = self.stream_attempts.saturating_add(1);
                self.stream_retry_at = Some(Instant::now() + STREAM_POLL_RETRY_DELAY);
                self.stream_unavailable = true;
                StreamEvent::Failed {
                    attempt: self.stream_attempts,
                    source,
                }
            }
        }
    }

    /// Prefer a simultaneously-ready stream item over the retry deadline. A
    /// losing ready stream future may already have removed an item, so random
    /// select order would make cancellation lossy.
    async fn poll_before<S, E>(
        &mut self,
        stream: &mut S,
        deadline: Instant,
    ) -> Option<StreamEvent<T, E>>
    where
        S: TryStream<Ok = T, Error = E> + Unpin,
    {
        tokio::select! {
            biased;
            event = self.poll(stream) => Some(event),
            () = tokio::time::sleep_until(deadline) => None,
        }
    }
}

/// Retain one canonical notification across operational acquisition retries
/// while continuing to poll and FIFO-buffer the same Reth catch-up stream.
pub(super) async fn process_retained<S, E, Z>(
    context: RetainedContext<'_, Z>,
    driver: &mut RetainedNotificationDriver<ExExNotification<TempoPrimitives>>,
    stream: &mut S,
    notification: &ExExNotification<TempoPrimitives>,
) -> RetainedOutcome
where
    S: TryStream<Ok = ExExNotification<TempoPrimitives>, Error = E> + Unpin,
    E: Display,
    Z: ExactStateLookup + ?Sized,
{
    let RetainedContext {
        checker,
        zone_state,
        l1_client,
        status,
    } = context;
    let mut retry = RetryBackoff::new();
    loop {
        match checker
            .process_notification_once(notification, zone_state, l1_client)
            .await
        {
            Ok(ready) => {
                let recovered = retry.attempts() != 0;
                if recovered && !ready.is_alerting() {
                    info!(
                        target: "zone::checker",
                        attempts = retry.attempts(),
                        tip = ?ready.tip(),
                        "Checker acquisition recovered"
                    );
                }
                return RetainedOutcome::Ready { ready, recovered };
            }
            Err(failure) if failure.is_retryable() => {
                let (attempt, delay) = retry.fail();
                status.record_retry(checker.is_alerting());
                warn!(
                    target: "zone::checker",
                    attempt,
                    delay_ms = delay.as_millis(),
                    error = %failure,
                    "Checker acquisition unavailable; retaining notification"
                );

                let deadline = Instant::now() + delay;
                loop {
                    if !driver.has_capacity() {
                        tokio::time::sleep_until(deadline).await;
                        break;
                    }
                    let Some(event) = driver.poll_before(stream, deadline).await else {
                        break;
                    };
                    match event {
                        StreamEvent::Notification(notification) => {
                            driver.retain(notification);
                        }
                        StreamEvent::Closed => return RetainedOutcome::StreamClosed,
                        StreamEvent::Failed { attempt, source } => {
                            if let Err(failure) =
                                report_stream_failure(checker, status, attempt, &source)
                            {
                                return RetainedOutcome::Terminal(failure);
                            }
                        }
                    }
                }
            }
            Err(failure) => {
                status.record_terminal(checker.is_alerting());
                return RetainedOutcome::Terminal(failure);
            }
        }
    }
}

pub(super) fn report_stream_failure(
    checker: &PersistentChecker,
    status: &mut RuntimeStatus,
    attempt: u64,
    source: &impl Display,
) -> RuntimeResult<()> {
    let progress = checker.store.load_progress()?;
    status.record_retry(checker.is_alerting());
    warn!(
        target: "zone::checker",
        attempt,
        retry_ms = STREAM_POLL_RETRY_DELAY.as_millis(),
        verified_tip = ?progress.verified_zone_tip,
        error = %source,
        "Zone archive backfill unavailable; polling the same Reth stream in place"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, time::Duration};

    use futures::stream;

    use super::{RetainedNotificationDriver, STREAM_POLL_RETRY_DELAY, StreamEvent};

    #[test]
    fn retained_notifications_remain_fifo() {
        let mut driver = RetainedNotificationDriver::new();
        driver.retain(3);
        driver.retain(5);
        driver.retain(8);

        assert_eq!(driver.pop_front(), Some(3));
        assert_eq!(driver.pop_front(), Some(5));
        assert_eq!(driver.pop_front(), Some(8));
        assert_eq!(driver.pop_front(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_failure_uses_short_poll_cadence_and_preserves_order() {
        let mut stream = stream::iter([Err::<u64, _>("archive unavailable"), Ok(11), Ok(12)]);
        let mut driver = RetainedNotificationDriver::new();

        assert!(matches!(
            driver.poll(&mut stream).await,
            StreamEvent::Failed { attempt: 1, .. }
        ));
        let second = {
            let poll = driver.poll(&mut stream);
            tokio::pin!(poll);
            assert!(
                tokio::time::timeout(Duration::ZERO, &mut poll)
                    .await
                    .is_err(),
                "a stream error must not hot-spin"
            );
            tokio::time::advance(STREAM_POLL_RETRY_DELAY).await;
            let StreamEvent::Notification(second) = poll.await else {
                panic!("stream did not recover at the short drain cadence");
            };
            second
        };
        driver.retain(second);
        let StreamEvent::Notification(third) = driver.poll(&mut stream).await else {
            panic!("recovered stream did not preserve its next item");
        };
        driver.retain(third);

        assert_eq!(driver.pop_front(), Some(11));
        assert_eq!(driver.pop_front(), Some(12));
        assert!(!driver.stream_unavailable());

        let mut closed = stream::empty::<Result<u64, Infallible>>();
        let event: StreamEvent<u64, Infallible> = driver.poll(&mut closed).await;
        assert!(matches!(event, StreamEvent::Closed));
    }

    #[tokio::test(start_paused = true)]
    async fn simultaneous_deadline_cannot_cancel_a_ready_stream_item() {
        let mut stream = stream::iter([Ok::<_, Infallible>(21)]);
        let mut driver = RetainedNotificationDriver::new();

        let event = driver
            .poll_before(&mut stream, tokio::time::Instant::now())
            .await;

        assert!(matches!(event, Some(StreamEvent::Notification(21))));
    }
}
