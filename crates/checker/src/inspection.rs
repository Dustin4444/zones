//! Read-only inspection of durable checker progress and alert state.

use std::path::Path;

use alloy_eips::BlockNumHash;

use crate::{
    CheckerBlockedReason,
    persistence::{Coverage, Persistence},
};

/// Durable checker watermarks and alert state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckerSnapshot {
    /// Oldest Zone coordinate from which local reorg recovery is supported.
    pub recovery_zone_tip: BlockNumHash,
    pub verified_zone_tip: BlockNumHash,
    pub imported_tempo_tip: BlockNumHash,
    pub acknowledged_zone_tip: BlockNumHash,
    pub active_finding: bool,
    pub has_coverage_gap: bool,
    pub blocked_reason: Option<CheckerBlockedReason>,
}

/// Inspect a stopped checker database or a consistent copy.
pub fn inspect_database(path: impl AsRef<Path>) -> eyre::Result<CheckerSnapshot> {
    let snapshot = Persistence::inspect_snapshot(path)?;
    Ok(CheckerSnapshot {
        recovery_zone_tip: BlockNumHash::new(
            snapshot.meta.recovery_checkpoint.height,
            snapshot.meta.recovery_checkpoint.hash,
        ),
        verified_zone_tip: snapshot.meta.verified_zone_tip.into(),
        imported_tempo_tip: snapshot.meta.imported_tempo_tip.into(),
        acknowledged_zone_tip: snapshot.meta.acknowledged_zone_tip.into(),
        active_finding: snapshot.meta.active_finding.is_some(),
        has_coverage_gap: !matches!(snapshot.meta.coverage, Coverage::Complete),
        blocked_reason: snapshot.meta.blocked,
    })
}
