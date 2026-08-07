//! Read-only checker inspection used by real-node integration tests.

use std::path::Path;

use alloy_eips::BlockNumHash;

use crate::persistence::{Coverage, Persistence};

/// Durable checker watermarks and alert state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckerSnapshot {
    pub verified_zone_tip: BlockNumHash,
    pub imported_tempo_tip: BlockNumHash,
    pub acknowledged_zone_tip: BlockNumHash,
    pub active_alert: bool,
    pub has_coverage_gap: bool,
}

/// Inspect a stopped checker database or a consistent copy.
pub fn inspect_database(path: impl AsRef<Path>) -> eyre::Result<CheckerSnapshot> {
    let snapshot = Persistence::inspect_snapshot(path)?;
    let result = CheckerSnapshot {
        verified_zone_tip: snapshot.meta.verified_zone_tip.into(),
        imported_tempo_tip: snapshot.meta.imported_tempo_tip.into(),
        acknowledged_zone_tip: snapshot.meta.acknowledged_zone_tip.into(),
        active_alert: snapshot.meta.active_finding.is_some(),
        has_coverage_gap: !matches!(snapshot.meta.coverage, Coverage::Complete),
    };
    Ok(result)
}
