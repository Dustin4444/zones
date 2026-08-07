//! Atomic model commits and historical reconstruction.

mod plan;
mod retained;

use std::time::{Duration, Instant};

use alloy_eips::BlockNumHash;
use reth_db::{
    Database,
    cursor::DbCursorRW,
    transaction::{DbTx, DbTxMut},
};

use crate::model::transition::ModelStateUpdate;

use super::{
    codec::encoded,
    db::{
        CheckerStore, finish_read, read_bootstrap, read_snapshot, read_tip,
        validate_portal_settlement_change,
    },
    error::{StoreError, StoreResult},
    model_state::{
        ModelRows,
        update::{ModelRowChanges, lower_update},
    },
    operations::{
        WriteGate, WriteOutcome, finish_write, reject_active_alert, validate_metadata_and_findings,
        write_meta, write_model_value,
    },
    schema::{
        BLOCK_ORDINAL_KEY_LEN, CanonicalHash, ChangesetKey, CheckerCanonical, CheckerChangesets,
        CheckerModelState, MetaKey, ModelKey,
    },
    value::{BeforeImage, BootstrapState, MetaValue},
};

#[cfg(test)]
use super::operations::ModelMutation;

use crate::model::state::ModelState;

pub(super) use self::retained::{
    invalid_changeset, read_changeset_group, reconstruct_from, required_canonical,
    restore_changeset_rows,
};
use self::{
    plan::{
        BlockCandidate, BlockPlan, PreparedBlock, prepare_block_read, reject_child_canonical,
        require_parent_tips,
    },
    retained::{validate_all_changeset_keys, validate_canonical_table},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockCommit {
    expected_verified_zone_tip: BlockNumHash,
    expected_imported_tempo_tip: BlockNumHash,
    child_verified_zone_tip: BlockNumHash,
    child_imported_tempo_tip: BlockNumHash,
    mutations: ModelRowChanges,
}

/// Measurements known while applying one sparse block commit. Encoded
/// changeset bytes are computed from the journal already required for the
/// write; no historical table scan is added to steady-state processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppliedBlockMetrics {
    pub(crate) transaction_duration: Duration,
    pub(crate) changeset_bytes: usize,
    pub(crate) model_rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockWriteResult {
    Applied(AppliedBlockMetrics),
    AlreadyApplied,
}

impl BlockWriteResult {
    #[cfg(test)]
    const fn outcome(self) -> WriteOutcome {
        match self {
            Self::Applied(_) => WriteOutcome::Applied,
            Self::AlreadyApplied => WriteOutcome::AlreadyApplied,
        }
    }
}

impl BlockCommit {
    fn from_update(
        identity: crate::model::state::PortalIdentity,
        expected_verified_zone_tip: BlockNumHash,
        expected_imported_tempo_tip: BlockNumHash,
        child_verified_zone_tip: BlockNumHash,
        child_imported_tempo_tip: BlockNumHash,
        update: &ModelStateUpdate,
    ) -> StoreResult<Self> {
        Ok(Self {
            expected_verified_zone_tip,
            expected_imported_tempo_tip,
            child_verified_zone_tip,
            child_imported_tempo_tip,
            mutations: lower_update(identity, update)?,
        })
    }

    #[cfg(test)]
    pub(super) fn from_mutations(
        expected_verified_zone_tip: BlockNumHash,
        expected_imported_tempo_tip: BlockNumHash,
        child_verified_zone_tip: BlockNumHash,
        child_imported_tempo_tip: BlockNumHash,
        mutations: Vec<ModelMutation>,
    ) -> StoreResult<Self> {
        Ok(Self {
            expected_verified_zone_tip,
            expected_imported_tempo_tip,
            child_verified_zone_tip,
            child_imported_tempo_tip,
            mutations: super::operations::consolidate(mutations)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoricalSnapshot {
    pub(crate) verified_zone_tip: BlockNumHash,
    pub(crate) imported_tempo_tip: BlockNumHash,
    pub(crate) model: ModelState,
    pub(crate) model_rows: ModelRows,
}

impl CheckerStore {
    pub(crate) fn block_commit(
        &self,
        expected_verified_zone_tip: BlockNumHash,
        expected_imported_tempo_tip: BlockNumHash,
        child_verified_zone_tip: BlockNumHash,
        child_imported_tempo_tip: BlockNumHash,
        update: &ModelStateUpdate,
    ) -> StoreResult<BlockCommit> {
        BlockCommit::from_update(
            self.identity.portal_identity(),
            expected_verified_zone_tip,
            expected_imported_tempo_tip,
            child_verified_zone_tip,
            child_imported_tempo_tip,
            update,
        )
    }

    pub(crate) fn apply_block_measured(
        &self,
        commit: BlockCommit,
    ) -> StoreResult<BlockWriteResult> {
        self.apply_block_inner(commit, None)
    }

    #[cfg(test)]
    pub(crate) fn apply_block(&self, commit: BlockCommit) -> StoreResult<WriteOutcome> {
        self.apply_block_measured(commit)
            .map(BlockWriteResult::outcome)
    }

    #[cfg(test)]
    pub(crate) fn reconstruct(&self, target: u64) -> StoreResult<HistoricalSnapshot> {
        let tx = self.db.tx()?;
        let result = (|| {
            let current = read_snapshot(&tx, self.identity, self.path())?;
            reconstruct_from(&tx, self.identity, current, target)
        })();
        finish_read(tx, result)
    }

    #[cfg(test)]
    pub(crate) fn check_consistency(&self) -> StoreResult<()> {
        let tx = self.db.tx()?;
        let result = (|| {
            let current = read_snapshot(&tx, self.identity, self.path())?;
            validate_metadata_and_findings(&tx, current.verified_zone_tip)?;
            validate_canonical_table(
                &tx,
                current.verified_zone_tip,
                self.identity.zone_genesis_hash(),
            )?;
            validate_all_changeset_keys(&tx, current.verified_zone_tip.number)?;
            reconstruct_from(&tx, self.identity, current, 0).map(|_| ())
        })();
        finish_read(tx, result)
    }

    /// Decode every durable table and return the validated authoritative cut
    /// without replaying historical model states from genesis.
    pub(super) fn load_validated_snapshot(&self) -> StoreResult<super::db::StoreSnapshot> {
        let tx = self.db.tx()?;
        let result = (|| {
            let current = read_snapshot(&tx, self.identity, self.path())?;
            validate_metadata_and_findings(&tx, current.verified_zone_tip)?;
            validate_canonical_table(
                &tx,
                current.verified_zone_tip,
                self.identity.zone_genesis_hash(),
            )?;
            validate_all_changeset_keys(&tx, current.verified_zone_tip.number)?;
            Ok(current)
        })();
        finish_read(tx, result)
    }

    #[cfg(test)]
    pub(crate) fn apply_block_aborting_after(
        &self,
        commit: BlockCommit,
        writes: usize,
    ) -> StoreResult<WriteOutcome> {
        self.apply_block_inner(commit, Some(writes))
            .map(BlockWriteResult::outcome)
    }

    fn apply_block_inner(
        &self,
        commit: BlockCommit,
        fail_after: Option<usize>,
    ) -> StoreResult<BlockWriteResult> {
        let commit = BlockCandidate::new(commit)?;
        let read_tx = self.db.tx()?;
        let plan = prepare_block_read(&read_tx, self.identity, self.path(), commit);
        let plan = finish_read(read_tx, plan)?;
        let BlockPlan::Apply(prepared) = plan else {
            return Ok(BlockWriteResult::AlreadyApplied);
        };
        let changeset_bytes = changeset_encoded_bytes(&prepared.journal);
        let transaction_started = Instant::now();
        let tx = self.db.tx_mut()?;
        let mut gate = WriteGate::new(fail_after);
        let mut model_rows = None;
        let result = apply_block_transaction(&tx, &prepared, &mut gate).and_then(|outcome| {
            model_rows = Some(tx.entries::<CheckerModelState>()?);
            Ok(outcome)
        });
        let outcome = finish_write(tx, result)?;
        debug_assert_eq!(outcome, WriteOutcome::Applied);
        let model_rows = model_rows.expect("a successful apply measured the model table");
        Ok(BlockWriteResult::Applied(AppliedBlockMetrics {
            transaction_duration: transaction_started.elapsed(),
            changeset_bytes,
            model_rows,
        }))
    }
}

fn changeset_encoded_bytes(journal: &[(ChangesetKey, BeforeImage)]) -> usize {
    journal
        .iter()
        .map(|(_, image)| BLOCK_ORDINAL_KEY_LEN.saturating_add(encoded(image).len()))
        .fold(0_usize, usize::saturating_add)
}

fn apply_block_transaction<TX: DbTxMut + DbTx>(
    tx: &TX,
    commit: &PreparedBlock,
    gate: &mut WriteGate,
) -> StoreResult<WriteOutcome> {
    let block = &commit.candidate;
    let actual_zone = read_tip(tx, MetaKey::VerifiedZoneTip)?;
    let actual_tempo = read_tip(tx, MetaKey::ImportedTempoTip)?;
    reject_active_alert(tx)?;
    let expected_bootstrap = commit.bootstrap;
    if read_bootstrap(tx)? != expected_bootstrap {
        return Err(StoreError::InvalidBootstrapProgress(
            "bootstrap phase changed while preparing block commit",
        ));
    }
    let next_bootstrap = next_block_bootstrap(expected_bootstrap, block.child_tempo)?;
    require_parent_tips(block, actual_zone, actual_tempo)?;
    reject_child_canonical(tx, block)?;
    for (_, image) in &commit.journal {
        if let BeforeImage::Model { key, value } = image
            && tx.get::<CheckerModelState>(*key)?.as_ref() != value.as_deref()
        {
            return invalid_changeset(
                block.child_zone.number,
                block.child_zone.hash,
                "pre-block model row changed during commit preparation",
            );
        }
    }
    validate_portal_settlement_change(
        tx,
        next_bootstrap,
        block.child_zone,
        block.child_tempo,
        block.mutations.get(&ModelKey::PortalSettlement),
    )?;

    for (key, image) in &commit.journal {
        tx.cursor_write::<CheckerChangesets>()?
            .insert(*key, image)?;
        gate.wrote()?;
        if let BeforeImage::Model { key, .. } = image {
            let Some(mutation) = block.mutations.get(key) else {
                return invalid_changeset(
                    block.child_zone.number,
                    block.child_zone.hash,
                    "prepared journal key is missing its model mutation",
                );
            };
            write_model_value(tx, *key, mutation)?;
            gate.wrote()?;
        }
    }
    tx.cursor_write::<CheckerCanonical>()?.insert(
        block.child_zone.number,
        &CanonicalHash::new(block.child_zone.hash),
    )?;
    gate.wrote()?;
    if next_bootstrap != expected_bootstrap {
        write_meta(tx, MetaKey::Bootstrap, MetaValue::Bootstrap(next_bootstrap))?;
        gate.wrote()?;
    }
    write_meta(
        tx,
        MetaKey::VerifiedZoneTip,
        MetaValue::VerifiedZoneTip(block.child_zone),
    )?;
    gate.wrote()?;
    write_meta(
        tx,
        MetaKey::ImportedTempoTip,
        MetaValue::ImportedTempoTip(block.child_tempo),
    )?;
    gate.wrote()?;
    Ok(WriteOutcome::Applied)
}

fn next_block_bootstrap(
    current: BootstrapState,
    child_tempo: BlockNumHash,
) -> StoreResult<BootstrapState> {
    match current {
        BootstrapState::ZoneReplay { .. } => Ok(BootstrapState::zone_replay(child_tempo)),
        BootstrapState::Live => Ok(current),
        BootstrapState::L1Replay { .. } => Err(StoreError::InvalidBootstrapProgress(
            "ordinary block apply is disabled during L1 replay",
        )),
    }
}
