//! Guarded one-block restoration of the current verified tip.

use std::path::Path;

use alloy_eips::BlockNumHash;
use reth_db::{
    Database,
    transaction::{DbTx, DbTxMut},
};

use super::{
    db::{CheckerStore, read_snapshot, validate_model_cut_coherence},
    error::{ParentTips, StoreError, StoreResult},
    history::{
        invalid_changeset, read_changeset_group, required_canonical, restore_changeset_rows,
    },
    model_state::{ModelRows, assemble_model},
    operations::{WriteGate, write_meta, write_model_value},
    schema::{ChangesetKey, CheckerCanonical, CheckerChangesets, MetaKey},
    value::{BeforeImage, BlockBeforeImage, BootstrapState, MetaValue, StoreIdentity},
};

impl CheckerStore {
    /// Atomically restore the exact parent of the current verified Zone tip.
    ///
    /// The expected child is an explicit reorg guard. The operation validates
    /// the complete child and restored parent model cuts in the same write
    /// transaction and never accepts an active alert.
    pub(crate) fn unwind_tip(&self, expected_child: BlockNumHash) -> StoreResult<ParentTips> {
        self.unwind_tip_inner(expected_child, None)
    }

    #[cfg(test)]
    pub(crate) fn unwind_tip_aborting_after(
        &self,
        expected_child: BlockNumHash,
        writes: usize,
    ) -> StoreResult<ParentTips> {
        self.unwind_tip_inner(expected_child, Some(writes))
    }

    fn unwind_tip_inner(
        &self,
        expected_child: BlockNumHash,
        fail_after: Option<usize>,
    ) -> StoreResult<ParentTips> {
        let tx = self.db.tx_mut()?;
        let mut gate = WriteGate::new(fail_after);
        let result =
            unwind_tip_transaction(&tx, self.identity, self.path(), expected_child, &mut gate);
        match result {
            Ok(parent) => {
                tx.commit()?;
                Ok(parent)
            }
            Err(error) => {
                tx.abort();
                Err(error)
            }
        }
    }
}

struct PreparedUnwind {
    child: BlockNumHash,
    parent_rows: ModelRows,
    block: BlockBeforeImage,
    group: Vec<(ChangesetKey, BeforeImage)>,
}

fn unwind_tip_transaction<TX: DbTxMut + DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    path: &Path,
    expected_child: BlockNumHash,
    gate: &mut WriteGate,
) -> StoreResult<ParentTips> {
    let prepared = prepare_tip_unwind(tx, identity, path, expected_child)?;
    apply_tip_unwind(tx, identity, path, prepared, gate)
}

fn prepare_tip_unwind<TX: DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    path: &Path,
    expected_child: BlockNumHash,
) -> StoreResult<PreparedUnwind> {
    let child = read_snapshot(tx, identity, path)?;
    if let Some(alert) = child.active_alert {
        return Err(StoreError::ActiveAlert(alert.finding));
    }
    if child.bootstrap != BootstrapState::Live {
        return Err(StoreError::InvalidBootstrapProgress(
            "tip unwind requires live bootstrap state",
        ));
    }
    if child.verified_zone_tip != expected_child {
        return Err(StoreError::UnwindTipMismatch {
            expected: expected_child,
            actual: child.verified_zone_tip,
        });
    }

    let canonical = required_canonical(tx, expected_child.number)?;
    if canonical != expected_child.hash {
        return Err(StoreError::CanonicalConflict {
            height: expected_child.number,
            expected: expected_child.hash,
            actual: canonical,
        });
    }

    let group = read_changeset_group(tx, expected_child.number, expected_child.hash)?;
    let mut parent_rows = child.model_rows;
    let block = restore_changeset_rows(
        tx,
        expected_child,
        child.imported_tempo_tip,
        &group,
        &mut parent_rows,
    )?;
    let parent_model = assemble_model(identity.portal_identity(), parent_rows.clone())?;
    validate_model_cut_coherence(
        tx,
        Some(child.bootstrap),
        block.prior_verified_zone_tip,
        block.prior_imported_tempo_tip,
        &parent_model,
    )?;

    Ok(PreparedUnwind {
        child: expected_child,
        parent_rows,
        block,
        group,
    })
}

fn apply_tip_unwind<TX: DbTxMut + DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    path: &Path,
    prepared: PreparedUnwind,
    gate: &mut WriteGate,
) -> StoreResult<ParentTips> {
    let PreparedUnwind {
        child,
        parent_rows,
        block,
        group,
    } = prepared;

    for (_, image) in &group[1..] {
        let BeforeImage::Model { key, value } = image else {
            return invalid_changeset(
                child.number,
                child.hash,
                "mutation ordinal contains block metadata",
            );
        };
        let value = value.as_ref().map(|value| value.as_ref().clone());
        write_model_value(tx, *key, &value)?;
        gate.wrote()?;
    }
    write_meta(
        tx,
        MetaKey::VerifiedZoneTip,
        MetaValue::VerifiedZoneTip(block.prior_verified_zone_tip),
    )?;
    gate.wrote()?;
    write_meta(
        tx,
        MetaKey::ImportedTempoTip,
        MetaValue::ImportedTempoTip(block.prior_imported_tempo_tip),
    )?;
    gate.wrote()?;
    if !tx.delete::<CheckerCanonical>(child.number, None)? {
        return Err(StoreError::MissingCanonical {
            height: child.number,
        });
    }
    gate.wrote()?;
    for (key, _) in &group {
        if !tx.delete::<CheckerChangesets>(*key, None)? {
            return Err(StoreError::MissingChangeset {
                height: key.zone_height,
                hash: key.block_hash,
                ordinal: key.ordinal,
            });
        }
        gate.wrote()?;
    }

    let parent = read_snapshot(tx, identity, path)?;
    if parent.verified_zone_tip != block.prior_verified_zone_tip
        || parent.imported_tempo_tip != block.prior_imported_tempo_tip
        || parent.model_rows != parent_rows
    {
        return invalid_changeset(
            child.number,
            child.hash,
            "restored parent cut differs from validated before-images",
        );
    }
    Ok(ParentTips::new(
        parent.verified_zone_tip,
        parent.imported_tempo_tip,
    ))
}
