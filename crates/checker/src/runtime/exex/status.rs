use crate::{metrics::CheckerRuntimeMetrics, runtime::state::ReadyToAcknowledge};

/// Health and alert metrics for one checker runtime lifetime.
pub(crate) struct RuntimeStatus {
    metrics: CheckerRuntimeMetrics,
    active_alert: bool,
}

impl RuntimeStatus {
    pub(crate) fn new() -> Self {
        let metrics = CheckerRuntimeMetrics::default();
        metrics.healthy.set(0.0);
        metrics.active_alert.set(0.0);
        Self {
            metrics,
            active_alert: false,
        }
    }

    pub(crate) fn mark_started(&mut self, alerting: bool) {
        self.record_ready_state(alerting, false, true);
    }

    pub(super) fn mark_bootstrapping(&mut self, alerting: bool) {
        self.record_ready_state(alerting, false, false);
    }

    pub(crate) fn record_retry(&mut self, alerting: bool) {
        self.record_unhealthy(alerting);
        self.metrics.operational_retries_total.increment(1);
    }

    pub(super) fn record_terminal(&mut self, alerting: bool) {
        self.record_unhealthy(alerting);
    }

    pub(super) fn record_ready(
        &mut self,
        ready: ReadyToAcknowledge,
        recovered: bool,
        operational: bool,
    ) {
        self.record_ready_state(ready.is_alerting(), recovered, operational);
    }

    fn record_ready_state(&mut self, alerting: bool, recovered: bool, operational: bool) {
        self.record_alert(alerting);
        self.metrics
            .healthy
            .set(if operational && !alerting { 1.0 } else { 0.0 });
        if recovered && operational && !alerting {
            self.metrics.operational_recoveries_total.increment(1);
        }
    }

    fn record_unhealthy(&mut self, alerting: bool) {
        self.record_alert(alerting);
        self.metrics.healthy.set(0.0);
    }

    fn record_alert(&mut self, alerting: bool) {
        self.active_alert = alerting;
        self.metrics
            .active_alert
            .set(if alerting { 1.0 } else { 0.0 });
    }

    pub(crate) const fn is_alerting(&self) -> bool {
        self.active_alert
    }
}

impl Drop for RuntimeStatus {
    fn drop(&mut self) {
        self.metrics.healthy.set(0.0);
    }
}
