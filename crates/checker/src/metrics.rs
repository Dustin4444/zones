//! Fixed-cardinality metrics for the checker pipeline.

use reth_metrics::{
    Metrics,
    metrics::{Counter, Gauge, Histogram},
};

/// Goal 5 measurements used to decide whether deferred acquisition
/// optimizations are warranted. No field carries token, address, or finding
/// labels, so cardinality remains bounded.
#[derive(Metrics, Clone)]
#[metrics(scope = "tempo_zone_checker")]
pub(crate) struct CheckerMetrics {
    /// L2/L1 authenticated observation time in seconds.
    pub(crate) observation_duration_seconds: Histogram,
    /// Latency of one exact-block Portal balance call in seconds.
    pub(crate) collateral_call_duration_seconds: Histogram,
    /// Latency of one exact-hash local Zone state read in seconds.
    pub(crate) exact_state_read_duration_seconds: Histogram,
    /// All checker work outside exact-data acquisition in seconds.
    pub(crate) transition_duration_seconds: Histogram,
    /// Exact-block Portal balance calls attempted.
    pub(crate) collateral_calls_total: Counter,
    /// Exact-block Portal balance calls that failed.
    pub(crate) collateral_call_failures_total: Counter,
    /// Exact-hash local Zone state reads attempted.
    pub(crate) exact_state_reads_total: Counter,
    /// Exact-hash local Zone state reads that failed.
    pub(crate) exact_state_read_failures_total: Counter,
    /// Token supply slots requested from exact Zone state.
    pub(crate) supply_tokens_requested_total: Counter,
    /// Most recent canonical Zone height offered to the checker.
    pub(crate) latest_observed_zone_height: Gauge,
    /// Most recent Zone height that passed every check.
    pub(crate) latest_checked_zone_height: Gauge,
    /// Difference between the latest observed and checked Zone heights.
    pub(crate) model_lag_blocks: Gauge,
    /// Blocks that passed every model and implementation-output check.
    pub(crate) passed_blocks_total: Counter,
    /// Blocks left unapplied because authenticated data could not be acquired.
    pub(crate) acquisition_failures_total: Counter,
    /// Deterministic protocol divergences detected.
    pub(crate) findings_total: Counter,
}
