//! Revert/reorg orchestration with crash-resumable progress classification.

use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use reth_exex::ExExNotification;
use tempo_primitives::TempoPrimitives;

use crate::{observe::ExactStateLookup, validate_notification_receipt_sets};

use super::{
    RuntimeError, RuntimeResult,
    apply::L1Client,
    chain::{ValidatedChain, validate_reorg},
    state::{PersistentChecker, ReadyToAcknowledge},
};

impl PersistentChecker {
    pub(crate) async fn process_notification_once<S>(
        &mut self,
        notification: &ExExNotification<TempoPrimitives>,
        zone_state: &S,
        l1_client: &mut L1Client,
    ) -> RuntimeResult<ReadyToAcknowledge>
    where
        S: ExactStateLookup + ?Sized,
    {
        match notification {
            ExExNotification::ChainCommitted { new } => {
                let new = ValidatedChain::new(new, "committed")?;
                if self.is_alerting() {
                    self.validate_alert_descendant(&new, "committed")?;
                    return Ok(ReadyToAcknowledge::alerted(new.tip()));
                }
                validate_receipts(&new)?;
                self.process_committed_chain_once(&new, zone_state, l1_client)
                    .await
            }
            ExExNotification::ChainReverted { old } => {
                let old = ValidatedChain::new(old, "reverted")?;
                self.process_revert(&old)
            }
            ExExNotification::ChainReorged { old, new } => {
                let (old, new) = validate_reorg(old, new)?;
                self.process_reorg(&old, &new, zone_state, l1_client).await
            }
        }
    }

    fn process_revert(&mut self, old: &ValidatedChain<'_>) -> RuntimeResult<ReadyToAcknowledge> {
        if self.alert_survives(old)? {
            return Ok(ReadyToAcknowledge::alerted(old.base()));
        }
        self.remove_reverted_alert(old)?;
        self.unwind_old_progress(old, None)?;
        let tip = self.store.load_progress()?.verified_zone_tip;
        if tip != old.base() {
            return Err(RuntimeError::ReorgProgressConflict { tip });
        }
        Ok(ReadyToAcknowledge::verified(tip))
    }

    async fn process_reorg<S>(
        &mut self,
        old: &ValidatedChain<'_>,
        new: &ValidatedChain<'_>,
        zone_state: &S,
        l1_client: &mut L1Client,
    ) -> RuntimeResult<ReadyToAcknowledge>
    where
        S: ExactStateLookup + ?Sized,
    {
        // A retained reorg can be delivered again after its replacement
        // branch produced the active finding. In that state the replacement
        // is already the acknowledged canonical branch; classifying only the
        // old fragment would mistake the alert for conflicting evidence.
        if self
            .active_finding()
            .is_some_and(|finding| new.contains(finding))
        {
            return Ok(ReadyToAcknowledge::alerted(new.tip()));
        }
        if self.alert_survives(old)? {
            return Ok(ReadyToAcknowledge::alerted(new.tip()));
        }

        // No destructive progress begins until replacement data needed for
        // verification has the expected block/receipt shape.
        validate_receipts(new)?;
        self.remove_reverted_alert(old)?;
        self.unwind_old_progress(old, Some(new))?;
        self.process_committed_chain_once(new, zone_state, l1_client)
            .await
    }

    /// An alert survives only when the entire reverted fragment is strictly
    /// above its finding block. Spanning the height with another hash is
    /// conflicting notification evidence, not permission to clear it.
    fn alert_survives(&self, old: &ValidatedChain<'_>) -> RuntimeResult<bool> {
        let Some(finding) = self.active_finding() else {
            return Ok(false);
        };
        if old.contains(finding) {
            return Ok(false);
        }
        Self::validate_fragment_above_finding(old, finding, "reverted")?;
        Ok(true)
    }

    fn validate_alert_descendant(
        &self,
        chain: &ValidatedChain<'_>,
        kind: &'static str,
    ) -> RuntimeResult<()> {
        let Some(finding) = self.active_finding() else {
            return Ok(());
        };
        if chain.contains(finding) {
            return Ok(());
        }
        Self::validate_fragment_above_finding(chain, finding, kind)
    }

    fn validate_fragment_above_finding(
        chain: &ValidatedChain<'_>,
        finding: BlockNumHash,
        kind: &'static str,
    ) -> RuntimeResult<()> {
        if chain.spans_height(finding.number) {
            return Err(RuntimeError::InvalidNotificationChain {
                kind,
                reason: "active finding height carries a different block hash",
            });
        }
        if chain.base().number < finding.number
            || (chain.base().number == finding.number && chain.base() != finding)
        {
            return Err(RuntimeError::InvalidNotificationChain {
                kind,
                reason: "notification fragment is not a descendant of the active finding",
            });
        }
        Ok(())
    }

    fn active_finding(&self) -> Option<BlockNumHash> {
        self.active_alert()
            .map(|alert| BlockNumHash::new(alert.finding.zone_height(), alert.finding.zone_hash()))
    }

    fn remove_reverted_alert(&mut self, old: &ValidatedChain<'_>) -> RuntimeResult<()> {
        let Some(alert) = self.active_alert() else {
            return Ok(());
        };
        let finding = BlockNumHash::new(alert.finding.zone_height(), alert.finding.zone_hash());
        if !old.contains(finding) {
            return Err(RuntimeError::InvalidNotificationChain {
                kind: "reverted",
                reason: "active finding is not present in the reverted fragment",
            });
        }
        self.orphan_alert()
    }

    /// Resume at whichever durable phase a prior attempt reached: old suffix,
    /// exact ancestor, or already-applied replacement prefix.
    fn unwind_old_progress(
        &mut self,
        old: &ValidatedChain<'_>,
        replacement: Option<&ValidatedChain<'_>>,
    ) -> RuntimeResult<()> {
        let mut current = self.store.load_progress()?.verified_zone_tip;
        if current == old.base()
            || replacement.is_some_and(|replacement| replacement.contains(current))
        {
            return Ok(());
        }
        if !old.contains(current) {
            return Err(RuntimeError::ReorgProgressConflict { tip: current });
        }

        let mut unwound = false;
        for block in old.blocks().rev() {
            let child = BlockNumHash::new(block.header().number(), block.hash());
            if child.number > current.number {
                continue;
            }
            if child != current {
                return Err(RuntimeError::ReorgProgressConflict { tip: current });
            }
            let parent = self.store.unwind_tip(child)?;
            current = parent.zone;
            unwound = true;
        }
        if current != old.base() {
            return Err(RuntimeError::ReorgProgressConflict { tip: current });
        }
        if unwound {
            self.reload_mirror()?;
        }
        Ok(())
    }
}

fn validate_receipts(chain: &ValidatedChain<'_>) -> RuntimeResult<()> {
    validate_notification_receipt_sets(
        chain.inner().blocks().len(),
        chain.inner().block_receipts_iter().count(),
    )?;
    Ok(())
}
