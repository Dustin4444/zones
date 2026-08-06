//! Durable entry to and recovery from alert-triggered mode.

use alloy_eips::BlockNumHash;
use reth_primitives_traits::RecoveredBlock;
use tempo_primitives::Block;
use tracing::error;

use crate::{
    check::finding::Finding,
    store::value::{ActiveAlert, FindingRecord},
};

use super::{
    RuntimeResult,
    state::{LiveChecker, LivePhase},
};

impl LiveChecker {
    /// Persist the first deterministic finding before granting alert-mode
    /// acknowledgement capability.
    pub(super) fn activate_alert(
        &mut self,
        block: &RecoveredBlock<Block>,
        imported_tempo: Option<BlockNumHash>,
        finding: &Finding,
    ) -> RuntimeResult<()> {
        let (key, record) = FindingRecord::from_candidate(block, imported_tempo, finding)?;
        let parent = self.mirror.zone_tip();
        self.store.activate_finding(key, record, parent)?;
        let alert = ActiveAlert {
            finding: key,
            last_verified_parent: parent,
        };
        self.phase = LivePhase::Alerting(alert);
        error!(
            target: "zone::checker",
            zone_height = key.zone_height(),
            zone_hash = %key.zone_hash(),
            parent = ?parent,
            error = %finding,
            "Checker entered durable alert mode after an authenticated finding"
        );
        Ok(())
    }

    pub(super) fn orphan_alert(&mut self) -> RuntimeResult<()> {
        let Some(alert) = self.active_alert() else {
            return Ok(());
        };
        self.store.orphan_active_finding(alert.finding)?;
        self.phase = LivePhase::Verifying;
        Ok(())
    }
}
