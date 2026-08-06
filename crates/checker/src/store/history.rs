//! Atomic model commits and historical reconstruction.

use std::collections::BTreeMap;

use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use reth_db::{
    Database,
    cursor::{DbCursorRO, DbCursorRW},
    transaction::{DbTx, DbTxMut},
};

use crate::model::{state::ModelState, transition::ModelStateUpdate};

use super::{
    codec::validate_canonical,
    db::{
        CheckerStore, finish_read, read_bootstrap, read_head, read_snapshot, read_tip,
        validate_model_cut_coherence, validate_portal_settlement_change,
    },
    error::{ParentTips, StoreError, StoreResult},
    model_state::{
        ModelRows, assemble_model,
        update::{ModelRowChanges, lower_update},
    },
    operations::{
        WriteGate, WriteOutcome, finish_write, reject_active_alert, require_adjacent,
        retain_changed_rows, validate_changes, validate_metadata_and_findings,
        validate_model_value, write_meta, write_model_value,
    },
    schema::{
        CanonicalHash, ChangesetKey, CheckerCanonical, CheckerChangesets, CheckerModelState,
        MetaKey, ModelKey,
    },
    value::{BeforeImage, BlockBeforeImage, BootstrapState, MetaValue, ModelValue},
};

#[cfg(test)]
use super::operations::ModelMutation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockCommit {
    expected_verified_zone_tip: BlockNumHash,
    expected_imported_tempo_tip: BlockNumHash,
    child_verified_zone_tip: BlockNumHash,
    child_imported_tempo_tip: BlockNumHash,
    mutations: ModelRowChanges,
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

    pub(crate) fn apply_block(&self, commit: BlockCommit) -> StoreResult<WriteOutcome> {
        self.apply_block_inner(commit, None)
    }

    pub(crate) fn reconstruct(&self, target: u64) -> StoreResult<HistoricalSnapshot> {
        let tx = self.db.tx()?;
        let result = (|| {
            let current = read_snapshot(&tx, self.identity, self.path())?;
            reconstruct_from(&tx, self.identity, current, target)
        })();
        finish_read(tx, result)
    }

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

    /// Decode every durable table and validate the authoritative cut without
    /// replaying historical model states from genesis.
    pub(super) fn validate_restart(&self) -> StoreResult<()> {
        let tx = self.db.tx()?;
        let result = (|| {
            let current = read_snapshot(&tx, self.identity, self.path())?;
            validate_metadata_and_findings(&tx, current.verified_zone_tip)?;
            validate_canonical_table(
                &tx,
                current.verified_zone_tip,
                self.identity.zone_genesis_hash(),
            )?;
            validate_all_changeset_keys(&tx, current.verified_zone_tip.number)
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
    }

    fn apply_block_inner(
        &self,
        commit: BlockCommit,
        fail_after: Option<usize>,
    ) -> StoreResult<WriteOutcome> {
        let commit = BlockCandidate::new(commit)?;
        let read_tx = self.db.tx()?;
        let plan = prepare_block_read(&read_tx, self.identity, self.path(), commit);
        let plan = finish_read(read_tx, plan)?;
        let BlockPlan::Apply(prepared) = plan else {
            return Ok(WriteOutcome::AlreadyApplied);
        };
        let tx = self.db.tx_mut()?;
        let mut gate = WriteGate::new(fail_after);
        let result = apply_block_transaction(&tx, &prepared, &mut gate);
        finish_write(tx, result)
    }
}

#[derive(Debug)]
struct BlockCandidate {
    expected_zone: BlockNumHash,
    expected_tempo: BlockNumHash,
    child_zone: BlockNumHash,
    child_tempo: BlockNumHash,
    mutations: BTreeMap<ModelKey, Option<ModelValue>>,
}

impl BlockCandidate {
    fn new(commit: BlockCommit) -> StoreResult<Self> {
        require_adjacent(
            "Zone",
            commit.expected_verified_zone_tip,
            commit.child_verified_zone_tip,
        )?;
        require_adjacent(
            "Tempo",
            commit.expected_imported_tempo_tip,
            commit.child_imported_tempo_tip,
        )?;
        Ok(Self {
            expected_zone: commit.expected_verified_zone_tip,
            expected_tempo: commit.expected_imported_tempo_tip,
            child_zone: commit.child_verified_zone_tip,
            child_tempo: commit.child_imported_tempo_tip,
            mutations: validate_changes(commit.mutations)?,
        })
    }
}

#[derive(Debug)]
struct PreparedBlock {
    candidate: BlockCandidate,
    bootstrap: BootstrapState,
    journal: Vec<(ChangesetKey, BeforeImage)>,
}

enum BlockPlan {
    Apply(Box<PreparedBlock>),
    AlreadyApplied,
}

fn prepare_block_read<TX: DbTx>(
    tx: &TX,
    identity: super::value::StoreIdentity,
    path: &std::path::Path,
    mut commit: BlockCandidate,
) -> StoreResult<BlockPlan> {
    let head = read_head(tx, identity, path)?;
    if let Some(alert) = head.active_alert {
        return Err(StoreError::ActiveAlert(alert.finding));
    }
    if matches!(head.bootstrap, BootstrapState::L1Replay { .. }) {
        return Err(StoreError::InvalidBootstrapProgress(
            "ordinary block apply is disabled during L1 replay",
        ));
    }
    if head.verified_zone_tip == commit.child_zone && head.imported_tempo_tip == commit.child_tempo
    {
        validate_committed_block(tx, &commit)?;
        return Ok(BlockPlan::AlreadyApplied);
    }
    require_parent_tips(&commit, head.verified_zone_tip, head.imported_tempo_tip)?;
    reject_child_canonical(tx, &commit)?;
    let before = retain_changed_rows(tx, &mut commit.mutations)?;
    let next_bootstrap = next_block_bootstrap(head.bootstrap, commit.child_tempo)?;
    validate_portal_settlement_change(
        tx,
        next_bootstrap,
        commit.child_zone,
        commit.child_tempo,
        commit.mutations.get(&ModelKey::PortalSettlement),
    )?;
    let journal = build_journal(&before, &commit)?;
    Ok(BlockPlan::Apply(Box::new(PreparedBlock {
        candidate: commit,
        bootstrap: head.bootstrap,
        journal,
    })))
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

fn reconstruct_from<TX: DbTx>(
    tx: &TX,
    identity: super::value::StoreIdentity,
    current: super::db::StoreSnapshot,
    target: u64,
) -> StoreResult<HistoricalSnapshot> {
    if target > current.verified_zone_tip.number {
        return Err(StoreError::FutureTarget {
            target,
            current: current.verified_zone_tip.number,
        });
    }
    let mut zone_tip = current.verified_zone_tip;
    let mut tempo_tip = current.imported_tempo_tip;
    let mut model = current.model;
    let mut rows = current.model_rows;
    while zone_tip.number > target {
        let canonical = required_canonical(tx, zone_tip.number)?;
        if canonical != zone_tip.hash {
            return Err(StoreError::CanonicalConflict {
                height: zone_tip.number,
                expected: zone_tip.hash,
                actual: canonical,
            });
        }
        let block = unwind_changeset(tx, zone_tip, tempo_tip, &mut rows)?;
        zone_tip = block.prior_verified_zone_tip;
        tempo_tip = block.prior_imported_tempo_tip;
        model = assemble_model(identity.portal_identity(), rows.clone())?;
        validate_model_cut_coherence(tx, None, zone_tip, tempo_tip, &model)?;
    }
    let canonical = required_canonical(tx, target)?;
    if canonical != zone_tip.hash {
        return Err(StoreError::CanonicalConflict {
            height: target,
            expected: zone_tip.hash,
            actual: canonical,
        });
    }
    Ok(HistoricalSnapshot {
        verified_zone_tip: zone_tip,
        imported_tempo_tip: tempo_tip,
        model,
        model_rows: rows,
    })
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

fn unwind_changeset<TX: DbTx>(
    tx: &TX,
    child_zone: BlockNumHash,
    child_tempo: BlockNumHash,
    rows: &mut ModelRows,
) -> StoreResult<BlockBeforeImage> {
    let height = child_zone.number;
    let hash = child_zone.hash;
    let group = read_changeset_group(tx, height, hash)?;
    restore_changeset_rows(tx, child_zone, child_tempo, &group, rows)
}

pub(super) fn restore_changeset_rows<TX: DbTx>(
    tx: &TX,
    child_zone: BlockNumHash,
    child_tempo: BlockNumHash,
    group: &[(ChangesetKey, BeforeImage)],
    rows: &mut ModelRows,
) -> StoreResult<BlockBeforeImage> {
    let height = child_zone.number;
    let hash = child_zone.hash;
    let Some((_, BeforeImage::Block(block))) = group.first() else {
        return invalid_changeset(height, hash, "ordinal zero is not block metadata");
    };
    let block = *block;
    require_parent_links(tx, child_zone, child_tempo, block)?;
    if group.len() != block.mutation_count as usize + 1 {
        return invalid_changeset(height, hash, "row count differs from block metadata");
    }

    let mut previous = None;
    for (expected_ordinal, (stored_key, image)) in (1..=block.mutation_count).zip(&group[1..]) {
        if stored_key.ordinal != expected_ordinal {
            return invalid_changeset(height, hash, "changeset ordinals are not consecutive");
        }
        let BeforeImage::Model { key, value } = image else {
            return invalid_changeset(height, hash, "mutation ordinal contains block metadata");
        };
        if previous.is_some_and(|prior| *key <= prior) {
            return invalid_changeset(height, hash, "model keys are duplicate or unordered");
        }
        if rows.get(key) == value.as_deref() {
            return invalid_changeset(height, hash, "before-image equals the child model row");
        }
        if let Some(value) = value {
            validate_model_value(*key, value)?;
        }
        match value {
            Some(value) => {
                rows.insert(*key, value.as_ref().clone());
            }
            None => {
                rows.remove(key);
            }
        }
        previous = Some(*key);
    }
    Ok(block)
}

pub(super) fn read_changeset_group<TX: DbTx>(
    tx: &TX,
    height: u64,
    hash: B256,
) -> StoreResult<Vec<(ChangesetKey, BeforeImage)>> {
    let mut cursor = tx.cursor_read::<CheckerChangesets>()?;
    if let Some((key, _)) = cursor.seek(ChangesetKey::new(height, B256::ZERO, 0))?
        && key.zone_height == height
        && key.block_hash != hash
    {
        return invalid_changeset(
            height,
            hash,
            "changeset height contains a conflicting block hash",
        );
    }
    let metadata_key = ChangesetKey::new(height, hash, 0);
    let Some((key, metadata)) = cursor.seek(metadata_key)? else {
        return Err(StoreError::MissingChangeset {
            height,
            hash,
            ordinal: 0,
        });
    };
    if key != metadata_key {
        return invalid_changeset(height, hash, "changeset metadata is missing");
    }

    let BeforeImage::Block(block) = &metadata else {
        return invalid_changeset(height, hash, "ordinal zero is not block metadata");
    };
    let mutation_count = block.mutation_count;
    let mut rows = vec![(key, metadata)];
    for expected_ordinal in 1..=mutation_count {
        let Some((key, value)) = cursor.next()? else {
            return invalid_changeset(height, hash, "changeset mutation row is missing");
        };
        if key.zone_height != height || key.block_hash != hash {
            return invalid_changeset(height, hash, "changeset mutation row is missing");
        }
        if key.ordinal != expected_ordinal {
            return invalid_changeset(height, hash, "changeset ordinals are not consecutive");
        }
        rows.push((key, value));
    }
    if let Some((key, _)) = cursor.next()?
        && key.zone_height == height
    {
        let reason = if key.block_hash == hash {
            "changeset has surplus mutation rows"
        } else {
            "changeset height contains a conflicting block hash"
        };
        return invalid_changeset(height, hash, reason);
    }
    Ok(rows)
}

fn build_journal(
    before: &BTreeMap<ModelKey, Option<ModelValue>>,
    commit: &BlockCandidate,
) -> StoreResult<Vec<(ChangesetKey, BeforeImage)>> {
    if before.len() != commit.mutations.len() {
        return invalid_changeset(
            commit.child_zone.number,
            commit.child_zone.hash,
            "prepared before-images differ from model mutations",
        );
    }
    let count = u32::try_from(commit.mutations.len()).map_err(|_| StoreError::TooManyMutations)?;
    if count == u32::MAX {
        return Err(StoreError::TooManyMutations);
    }
    let block = BeforeImage::Block(BlockBeforeImage {
        prior_verified_zone_tip: commit.expected_zone,
        prior_imported_tempo_tip: commit.expected_tempo,
        mutation_count: count,
    });
    validate_canonical(&block)
        .map_err(|_| StoreError::InvalidPersistedValue("block before-image"))?;
    let mut journal = vec![(
        ChangesetKey::new(commit.child_zone.number, commit.child_zone.hash, 0),
        block,
    )];
    for (offset, (key, value)) in before.iter().enumerate() {
        let image = BeforeImage::Model {
            key: *key,
            value: value.clone().map(Box::new),
        };
        validate_canonical(&image)
            .map_err(|_| StoreError::InvalidPersistedValue("model before-image"))?;
        let ordinal = u32::try_from(offset + 1).map_err(|_| StoreError::TooManyMutations)?;
        journal.push((
            ChangesetKey::new(commit.child_zone.number, commit.child_zone.hash, ordinal),
            image,
        ));
    }
    Ok(journal)
}

fn validate_committed_block<TX: DbTx>(tx: &TX, commit: &BlockCandidate) -> StoreResult<()> {
    let canonical = required_canonical(tx, commit.child_zone.number)?;
    if canonical != commit.child_zone.hash {
        return Err(StoreError::CanonicalConflict {
            height: commit.child_zone.number,
            expected: commit.child_zone.hash,
            actual: canonical,
        });
    }
    let group = read_changeset_group(tx, commit.child_zone.number, commit.child_zone.hash)?;
    let BeforeImage::Block(block) = &group[0].1 else {
        return invalid_changeset(
            commit.child_zone.number,
            commit.child_zone.hash,
            "missing block metadata",
        );
    };
    if group.len() != block.mutation_count as usize + 1
        || block.prior_verified_zone_tip != commit.expected_zone
        || block.prior_imported_tempo_tip != commit.expected_tempo
    {
        return invalid_changeset(
            commit.child_zone.number,
            commit.child_zone.hash,
            "duplicate replay metadata differs",
        );
    }
    let mut previous = None;
    for (_, image) in &group[1..] {
        let BeforeImage::Model { key, value } = image else {
            return invalid_changeset(
                commit.child_zone.number,
                commit.child_zone.hash,
                "duplicate replay mutation row is not a model before-image",
            );
        };
        if previous.is_some_and(|prior| *key <= prior) {
            return invalid_changeset(
                commit.child_zone.number,
                commit.child_zone.hash,
                "duplicate replay model keys are duplicate or unordered",
            );
        }
        if let Some(value) = value {
            validate_model_value(*key, value)?;
        }
        if !commit.mutations.contains_key(key) {
            return invalid_changeset(
                commit.child_zone.number,
                commit.child_zone.hash,
                "duplicate replay journal is not a subset of lowered mutations",
            );
        }
        if tx.get::<CheckerModelState>(*key)?.as_ref() == value.as_deref() {
            return invalid_changeset(
                commit.child_zone.number,
                commit.child_zone.hash,
                "duplicate replay before-image equals the child model row",
            );
        }
        previous = Some(*key);
    }
    for (key, value) in &commit.mutations {
        if tx.get::<CheckerModelState>(*key)? != *value {
            return invalid_changeset(
                commit.child_zone.number,
                commit.child_zone.hash,
                "duplicate replay model state differs",
            );
        }
    }
    Ok(())
}

fn require_parent_tips(
    commit: &BlockCandidate,
    actual_zone: BlockNumHash,
    actual_tempo: BlockNumHash,
) -> StoreResult<()> {
    if actual_zone == commit.expected_zone && actual_tempo == commit.expected_tempo {
        Ok(())
    } else {
        Err(StoreError::ParentChanged {
            expected: Box::new(ParentTips::new(commit.expected_zone, commit.expected_tempo)),
            actual: Box::new(ParentTips::new(actual_zone, actual_tempo)),
        })
    }
}

fn reject_child_canonical<TX: DbTx>(tx: &TX, commit: &BlockCandidate) -> StoreResult<()> {
    if let Some(actual) = tx.get::<CheckerCanonical>(commit.child_zone.number)? {
        Err(StoreError::CanonicalConflict {
            height: commit.child_zone.number,
            expected: commit.child_zone.hash,
            actual: actual.into_inner(),
        })
    } else {
        Ok(())
    }
}

fn validate_canonical_table<TX: DbTx>(
    tx: &TX,
    tip: BlockNumHash,
    genesis: B256,
) -> StoreResult<()> {
    let mut cursor = tx.cursor_read::<CheckerCanonical>()?;
    let mut expected = 0_u64;
    let mut last = None;
    for row in cursor.walk(None)? {
        let (height, hash) = row?;
        let hash = hash.into_inner();
        if height != expected {
            return Err(StoreError::CanonicalSequence(
                "height gap or row above verified tip",
            ));
        }
        if height == 0 && hash != genesis {
            return Err(StoreError::CanonicalConflict {
                height,
                expected: genesis,
                actual: hash,
            });
        }
        last = Some(BlockNumHash::new(height, hash));
        expected = expected
            .checked_add(1)
            .ok_or(StoreError::CanonicalSequence("height overflow"))?;
    }
    if last != Some(tip) {
        return Err(StoreError::CanonicalSequence(
            "canonical tail differs from verified tip",
        ));
    }
    Ok(())
}

fn validate_all_changeset_keys<TX: DbTx>(tx: &TX, tip: u64) -> StoreResult<()> {
    let mut cursor = tx.cursor_read::<CheckerChangesets>()?;
    for row in cursor.walk(None)? {
        let (key, _) = row?;
        if key.zone_height == 0 || key.zone_height > tip {
            return invalid_changeset(
                key.zone_height,
                key.block_hash,
                "row is outside canonical history",
            );
        }
        let canonical = required_canonical(tx, key.zone_height)?;
        if canonical != key.block_hash {
            return Err(StoreError::CanonicalConflict {
                height: key.zone_height,
                expected: canonical,
                actual: key.block_hash,
            });
        }
    }
    Ok(())
}

fn require_parent_links<TX: DbTx>(
    tx: &TX,
    child_zone: BlockNumHash,
    child_tempo: BlockNumHash,
    block: BlockBeforeImage,
) -> StoreResult<()> {
    if block.prior_verified_zone_tip.number.checked_add(1) != Some(child_zone.number)
        || block.prior_imported_tempo_tip.number.checked_add(1) != Some(child_tempo.number)
    {
        return invalid_changeset(
            child_zone.number,
            child_zone.hash,
            "parent numbers are not adjacent",
        );
    }
    let parent_hash = required_canonical(tx, block.prior_verified_zone_tip.number)?;
    if parent_hash != block.prior_verified_zone_tip.hash {
        return Err(StoreError::CanonicalConflict {
            height: block.prior_verified_zone_tip.number,
            expected: block.prior_verified_zone_tip.hash,
            actual: parent_hash,
        });
    }
    Ok(())
}

pub(super) fn required_canonical<TX: DbTx>(tx: &TX, height: u64) -> StoreResult<B256> {
    tx.get::<CheckerCanonical>(height)?
        .map(CanonicalHash::into_inner)
        .ok_or(StoreError::MissingCanonical { height })
}

pub(super) fn invalid_changeset<T>(
    height: u64,
    hash: B256,
    reason: &'static str,
) -> StoreResult<T> {
    Err(StoreError::InvalidChangeset {
        height,
        hash,
        reason,
    })
}
