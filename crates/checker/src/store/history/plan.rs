//! Read-phase planning and duplicate validation for one canonical block append.

use std::collections::BTreeMap;

use alloy_eips::BlockNumHash;
use reth_db::transaction::DbTx;

use super::{invalid_changeset, next_block_bootstrap, read_changeset_group, required_canonical};
use crate::store::{
    codec::validate_canonical,
    db::{read_head, validate_portal_settlement_change},
    error::{ParentTips, StoreError, StoreResult},
    operations::{require_adjacent, retain_changed_rows, validate_changes, validate_model_value},
    schema::{ChangesetKey, CheckerModelState, ModelKey},
    value::{BeforeImage, BlockBeforeImage, BootstrapState, ModelValue},
};

use super::BlockCommit;

#[derive(Debug)]
pub(super) struct BlockCandidate {
    pub(super) expected_zone: BlockNumHash,
    pub(super) expected_tempo: BlockNumHash,
    pub(super) child_zone: BlockNumHash,
    pub(super) child_tempo: BlockNumHash,
    pub(super) mutations: BTreeMap<ModelKey, Option<ModelValue>>,
}

impl BlockCandidate {
    pub(super) fn new(commit: BlockCommit) -> StoreResult<Self> {
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
pub(super) struct PreparedBlock {
    pub(super) candidate: BlockCandidate,
    pub(super) bootstrap: BootstrapState,
    pub(super) journal: Vec<(ChangesetKey, BeforeImage)>,
}

pub(super) enum BlockPlan {
    Apply(Box<PreparedBlock>),
    AlreadyApplied,
}

pub(super) fn prepare_block_read<TX: DbTx>(
    tx: &TX,
    identity: crate::store::value::StoreIdentity,
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

pub(super) fn require_parent_tips(
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

pub(super) fn reject_child_canonical<TX: DbTx>(
    tx: &TX,
    commit: &BlockCandidate,
) -> StoreResult<()> {
    if let Some(actual) =
        tx.get::<crate::store::schema::CheckerCanonical>(commit.child_zone.number)?
    {
        Err(StoreError::CanonicalConflict {
            height: commit.child_zone.number,
            expected: commit.child_zone.hash,
            actual: actual.into_inner(),
        })
    } else {
        Ok(())
    }
}
