use std::time::Duration;

pub(super) const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);
pub(super) const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Saturating exponential retry state shared by runtime acquisition loops.
pub(super) struct RetryBackoff {
    attempts: u64,
    next_delay: Duration,
}

impl RetryBackoff {
    pub(super) const fn new() -> Self {
        Self {
            attempts: 0,
            next_delay: INITIAL_RETRY_DELAY,
        }
    }

    pub(super) fn fail(&mut self) -> (u64, Duration) {
        self.attempts = self.attempts.saturating_add(1);
        let delay = self.next_delay;
        self.next_delay = self.next_delay.saturating_mul(2).min(MAX_RETRY_DELAY);
        (self.attempts, delay)
    }

    pub(super) const fn attempts(&self) -> u64 {
        self.attempts
    }

    pub(super) fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::{INITIAL_RETRY_DELAY, MAX_RETRY_DELAY, RetryBackoff};

    #[test]
    fn retry_backoff_saturates_and_resets() {
        let mut retry = RetryBackoff::new();
        assert_eq!(retry.fail(), (1, INITIAL_RETRY_DELAY));

        let mut last = INITIAL_RETRY_DELAY;
        for _ in 0..32 {
            let (_, delay) = retry.fail();
            assert!(delay >= last);
            assert!(delay <= MAX_RETRY_DELAY);
            last = delay;
        }
        assert_eq!(last, MAX_RETRY_DELAY);

        retry.reset();
        assert_eq!(retry.attempts(), 0);
        assert_eq!(retry.fail(), (1, INITIAL_RETRY_DELAY));
    }
}
