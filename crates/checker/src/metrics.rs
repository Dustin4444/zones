//! Fixed-cardinality metrics for the checker.

use std::{fs, path::Path, time::Duration};

use reth_metrics::{
    Metrics,
    metrics::{Counter, Gauge, Histogram},
};

/// Whether a verified block arrived during archive catch-up or while following
/// the live canonical head. The two paths share transition logic but must not
/// share latency histograms: backfill would otherwise pollute live p50/p95.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockProcessingPhase {
    CatchUp,
    Live,
}

pub(crate) struct BlockMetricSample<'a> {
    pub(crate) phase: BlockProcessingPhase,
    pub(crate) block_duration: Duration,
    pub(crate) transaction_duration: Duration,
    pub(crate) changeset_bytes: usize,
    pub(crate) model_rows: usize,
    pub(crate) open_lifecycle_records: usize,
    pub(crate) database_path: &'a Path,
}

/// Checker measurements for exact acquisition and model evaluation. No field
/// carries token, address, or finding labels, so cardinality remains bounded.
#[derive(Metrics, Clone)]
#[metrics(scope = "tempo_zone_checker")]
pub(crate) struct CheckerMetrics {
    /// L2/L1 authenticated observation time in seconds.
    pub(crate) observation_duration_seconds: Histogram,
    /// Time spent fetching the complete L1 receipt vector from the archive RPC.
    pub(crate) receipt_fetch_duration_seconds: Histogram,
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
    /// Acquisition failures during staged model preparation.
    pub(crate) acquisition_failures_total: Counter,
    /// Deterministic protocol divergences detected.
    pub(crate) findings_total: Counter,
}

/// Durable-write and resource-growth measurements for verified blocks.
///
/// These measurements are fixed-cardinality. Catch-up throughput is derived
/// from `rate(catch_up_blocks_total[...])`; an in-process rate gauge would
/// depend on an arbitrary sampling window and reset on restart.
#[derive(Metrics, Clone)]
#[metrics(scope = "tempo_zone_checker")]
pub(crate) struct CheckerOperationalMetrics {
    /// End-to-end time for one live block from durable preflight through commit.
    pub(crate) live_block_duration_seconds: Histogram,
    /// End-to-end time for one archive catch-up block through commit.
    pub(crate) catch_up_block_duration_seconds: Histogram,
    /// Successful live blocks represented by the live latency histogram.
    pub(crate) live_blocks_total: Counter,
    /// Successful archive catch-up blocks represented by the catch-up histogram.
    pub(crate) catch_up_blocks_total: Counter,
    /// Time from opening the checker MDBX write transaction through commit.
    pub(crate) database_transaction_duration_seconds: Histogram,
    /// Canonically encoded changeset key and value bytes written by one block.
    pub(crate) changeset_bytes: Histogram,
    /// Cumulative canonically encoded changeset key and value bytes.
    pub(crate) changeset_bytes_total: Counter,
    /// Current authoritative model rows, including fixed state and open owners.
    pub(crate) model_rows: Gauge,
    /// Current nonterminal deposit, withdrawal, batch, fallback, and refund owners.
    pub(crate) open_lifecycle_records: Gauge,
    /// Allocated bytes of regular files in the dedicated checker MDBX directory.
    pub(crate) database_allocated_bytes: Gauge,
}

impl CheckerOperationalMetrics {
    pub(crate) fn record_resource_snapshot(
        &self,
        model_rows: usize,
        open_lifecycle_records: usize,
        database_path: &Path,
    ) {
        self.record_model_size(model_rows, open_lifecycle_records);
        self.refresh_database_allocation(database_path);
    }

    pub(crate) fn record_block(&self, sample: BlockMetricSample<'_>) {
        match sample.phase {
            BlockProcessingPhase::CatchUp => {
                self.catch_up_block_duration_seconds
                    .record(sample.block_duration.as_secs_f64());
                self.catch_up_blocks_total.increment(1);
            }
            BlockProcessingPhase::Live => {
                self.live_block_duration_seconds
                    .record(sample.block_duration.as_secs_f64());
                self.live_blocks_total.increment(1);
            }
        }
        self.database_transaction_duration_seconds
            .record(sample.transaction_duration.as_secs_f64());
        self.changeset_bytes.record(sample.changeset_bytes as f64);
        self.changeset_bytes_total
            .increment(sample.changeset_bytes as u64);
        self.record_model_size(sample.model_rows, sample.open_lifecycle_records);
        self.refresh_database_allocation(sample.database_path);
    }

    fn record_model_size(&self, model_rows: usize, open_lifecycle_records: usize) {
        self.model_rows.set(model_rows as f64);
        self.open_lifecycle_records
            .set(open_lifecycle_records as f64);
    }

    pub(crate) fn refresh_database_allocation(&self, database_path: &Path) {
        if let Some(bytes) = database_allocated_bytes(database_path) {
            self.database_allocated_bytes.set(bytes as f64);
        }
    }
}

/// MDBX owns a fixed, small set of regular files in this dedicated directory.
/// Unix `st_blocks` reports actual filesystem allocation in 512-byte units,
/// which remains meaningful when MDBX preallocates a large sparse map. Other
/// platforms fall back to logical file length when allocation data is absent.
fn database_allocated_bytes(path: &Path) -> Option<u64> {
    fs::read_dir(path).ok()?.try_fold(0_u64, |total, entry| {
        let metadata = entry.ok()?.metadata().ok()?;
        Some(if metadata.is_file() {
            total.saturating_add(allocated_bytes(&metadata))
        } else {
            total
        })
    })
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;

    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use metrics_exporter_prometheus::PrometheusBuilder;

    use super::{BlockMetricSample, BlockProcessingPhase, CheckerOperationalMetrics};

    #[test]
    fn operational_metrics_preserve_phase_and_resource_semantics() {
        let recorder = PrometheusBuilder::new()
            .set_quantiles(&[0.5, 0.95])
            .unwrap()
            .build_recorder();
        let handle = recorder.handle();
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("data.mdb"), [0_u8; 7]).unwrap();
        fs::write(directory.path().join("lock.mdb"), [0_u8; 11]).unwrap();
        fs::create_dir(directory.path().join("not-a-database-file")).unwrap();
        let allocated_bytes = ["data.mdb", "lock.mdb"]
            .into_iter()
            .map(|name| super::allocated_bytes(&fs::metadata(directory.path().join(name)).unwrap()))
            .sum::<u64>();

        metrics::with_local_recorder(&recorder, || {
            let metrics = CheckerOperationalMetrics::default();
            metrics.record_resource_snapshot(7, 2, directory.path());
            metrics.record_block(BlockMetricSample {
                phase: BlockProcessingPhase::CatchUp,
                block_duration: Duration::from_millis(80),
                transaction_duration: Duration::from_millis(8),
                changeset_bytes: 100,
                model_rows: 8,
                open_lifecycle_records: 3,
                database_path: directory.path(),
            });
            metrics.record_block(BlockMetricSample {
                phase: BlockProcessingPhase::Live,
                block_duration: Duration::from_millis(20),
                transaction_duration: Duration::from_millis(2),
                changeset_bytes: 250,
                model_rows: 9,
                open_lifecycle_records: 4,
                database_path: directory.path(),
            });
        });

        handle.run_upkeep();
        let rendered = handle.render();
        assert_metric(&rendered, "tempo_zone_checker_catch_up_blocks_total", 1.0);
        assert_metric(&rendered, "tempo_zone_checker_live_blocks_total", 1.0);
        assert_metric(&rendered, "tempo_zone_checker_changeset_bytes_count", 2.0);
        assert_metric(&rendered, "tempo_zone_checker_changeset_bytes_sum", 350.0);
        assert_metric(
            &rendered,
            "tempo_zone_checker_database_transaction_duration_seconds_count",
            2.0,
        );
        assert_metric(&rendered, "tempo_zone_checker_model_rows", 9.0);
        assert_metric(&rendered, "tempo_zone_checker_open_lifecycle_records", 4.0);
        assert_metric(
            &rendered,
            "tempo_zone_checker_database_allocated_bytes",
            allocated_bytes as f64,
        );
        assert!(!rendered.contains("phase="), "{rendered}");
    }

    fn assert_metric(rendered: &str, name: &str, expected: f64) {
        let value = rendered
            .lines()
            .find_map(|line| {
                let (candidate, value) = line.split_once(' ')?;
                (candidate == name).then(|| value.parse::<f64>().unwrap())
            })
            .unwrap_or_else(|| panic!("missing metric {name} in:\n{rendered}"));
        assert_eq!(value, expected, "metric {name}");
    }
}

/// Sole-writer health and retry measurements.
#[derive(Metrics, Clone)]
#[metrics(scope = "tempo_zone_checker_runtime")]
pub(crate) struct CheckerRuntimeMetrics {
    /// Retry attempts after operational acquisition failures.
    pub(crate) operational_retries_total: Counter,
    /// Operational acquisition gaps that recovered after at least one retry.
    pub(crate) operational_recoveries_total: Counter,
    /// Whether the checker is currently healthy (`1`) or operationally blocked (`0`).
    pub(crate) healthy: Gauge,
    /// Whether one durable canonical finding currently freezes model progress.
    pub(crate) active_alert: Gauge,
}
