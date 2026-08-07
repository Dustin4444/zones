//! Short atomic writes shared by live, bootstrap, and divergence paths.

use std::collections::BTreeMap;

use alloy_eips::BlockNumHash;
use reth_db::{
    Database,
    cursor::{DbCursorRO, DbCursorRW},
    transaction::{DbTx, DbTxMut},
};

use crate::model::transition::{ImportedTempoStateUpdate, ZoneGenesisStateUpdate};

use super::{
    codec::validate_canonical,
    db::{
        CheckerStore, StoreProgress, finish_read, read_active_alert, read_head, read_snapshot,
        read_tip, validate_bootstrap_candidate, validate_bootstrap_coherence,
        validate_portal_settlement_change,
    },
    error::{StoreError, StoreResult},
    model_state::update::{ModelRowChanges, lower_imported_update, lower_zone_genesis_update},
    schema::{
        CheckerCanonical, CheckerFindings, CheckerMeta, CheckerModelState, FindingKey, MetaKey,
        ModelKey,
    },
    value::{
        ActiveAlert, BootstrapState, FindingRecord, FindingStatus, MetaValue, ModelValue,
        StoreIdentity,
    },
};

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelMutation {
    key: ModelKey,
    value: Option<ModelValue>,
}

#[cfg(test)]
impl ModelMutation {
    pub(crate) fn put(key: ModelKey, value: ModelValue) -> StoreResult<Self> {
        validate_model_value(key, &value)?;
        Ok(Self {
            key,
            value: Some(value),
        })
    }

    pub(crate) const fn delete(key: ModelKey) -> Self {
        Self { key, value: None }
    }

    fn into_parts(self) -> (ModelKey, Option<ModelValue>) {
        (self.key, self.value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteOutcome {
    Applied,
    AlreadyApplied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BootstrapCommit {
    expected_state: BootstrapState,
    expected_tempo: BlockNumHash,
    next_state: BootstrapState,
    next_tempo: BlockNumHash,
    changes: ModelRowChanges,
}

impl CheckerStore {
    pub(crate) fn bootstrap_l1_commit(
        &self,
        expected_state: BootstrapState,
        expected_tempo: BlockNumHash,
        next_tempo: BlockNumHash,
        update: &ImportedTempoStateUpdate,
    ) -> StoreResult<BootstrapCommit> {
        let BootstrapState::L1Replay { cursor } = expected_state else {
            return Err(StoreError::InvalidBootstrapProgress(
                "L1 replay cursor does not match its expected tip",
            ));
        };
        validate_l1_replay_step(self.identity, cursor, expected_tempo, next_tempo)?;
        Ok(BootstrapCommit {
            expected_state,
            expected_tempo,
            next_state: BootstrapState::l1_replay(Some(next_tempo)),
            next_tempo,
            changes: validate_changes(lower_imported_update(
                self.identity.portal_identity(),
                update,
            )?)?,
        })
    }

    pub(crate) fn enter_zone_replay(
        &self,
        expected_state: BootstrapState,
        tempo_tip: BlockNumHash,
        update: &ZoneGenesisStateUpdate,
    ) -> StoreResult<BootstrapCommit> {
        if expected_state != BootstrapState::l1_replay(Some(tempo_tip)) {
            return Err(StoreError::InvalidBootstrapProgress(
                "Zone replay requires the completed L1 replay cursor",
            ));
        }
        Ok(BootstrapCommit {
            expected_state,
            expected_tempo: tempo_tip,
            next_state: BootstrapState::zone_replay(tempo_tip),
            next_tempo: tempo_tip,
            changes: validate_changes(lower_zone_genesis_update(
                self.identity.portal_identity(),
                update,
            )?)?,
        })
    }

    pub(crate) fn enter_live(
        &self,
        expected_state: BootstrapState,
        tempo_tip: BlockNumHash,
    ) -> StoreResult<BootstrapCommit> {
        if expected_state != BootstrapState::zone_replay(tempo_tip) {
            return Err(StoreError::InvalidBootstrapProgress(
                "live mode requires Zone replay",
            ));
        }
        Ok(BootstrapCommit {
            expected_state,
            expected_tempo: tempo_tip,
            next_state: BootstrapState::live(),
            next_tempo: tempo_tip,
            changes: BTreeMap::new(),
        })
    }

    pub(crate) fn apply_bootstrap(&self, commit: BootstrapCommit) -> StoreResult<WriteOutcome> {
        self.apply_bootstrap_inner(commit, None)
    }

    pub(crate) fn activate_finding(
        &self,
        key: FindingKey,
        record: FindingRecord,
        last_verified_parent: BlockNumHash,
    ) -> StoreResult<WriteOutcome> {
        self.activate_finding_inner(key, record, last_verified_parent, None)
    }

    pub(crate) fn orphan_active_finding(&self, key: FindingKey) -> StoreResult<WriteOutcome> {
        self.orphan_active_finding_inner(key, None)
    }

    #[cfg(test)]
    pub(super) fn apply_bootstrap_aborting_after(
        &self,
        commit: BootstrapCommit,
        writes: usize,
    ) -> StoreResult<WriteOutcome> {
        self.apply_bootstrap_inner(commit, Some(writes))
    }

    #[cfg(test)]
    pub(super) fn bootstrap_commit_from_mutations(
        &self,
        expected_state: BootstrapState,
        expected_tempo: BlockNumHash,
        next_tempo: BlockNumHash,
        mutations: Vec<ModelMutation>,
    ) -> StoreResult<BootstrapCommit> {
        let BootstrapState::L1Replay { cursor } = expected_state else {
            return Err(StoreError::InvalidBootstrapProgress(
                "raw L1 replay cursor does not match its expected tip",
            ));
        };
        validate_l1_replay_step(self.identity, cursor, expected_tempo, next_tempo)?;
        Ok(BootstrapCommit {
            expected_state,
            expected_tempo,
            next_state: BootstrapState::l1_replay(Some(next_tempo)),
            next_tempo,
            changes: consolidate(mutations)?,
        })
    }

    #[cfg(test)]
    pub(super) fn activate_finding_aborting_after(
        &self,
        key: FindingKey,
        record: FindingRecord,
        parent: BlockNumHash,
        writes: usize,
    ) -> StoreResult<WriteOutcome> {
        self.activate_finding_inner(key, record, parent, Some(writes))
    }

    #[cfg(test)]
    pub(super) fn orphan_finding_aborting_after(
        &self,
        key: FindingKey,
        writes: usize,
    ) -> StoreResult<WriteOutcome> {
        self.orphan_active_finding_inner(key, Some(writes))
    }

    fn apply_bootstrap_inner(
        &self,
        commit: BootstrapCommit,
        fail_after: Option<usize>,
    ) -> StoreResult<WriteOutcome> {
        let read_tx = self.db.tx()?;
        let plan = prepare_bootstrap(&read_tx, self.identity, self.path(), commit);
        let plan = finish_read(read_tx, plan)?;
        let BootstrapPlan::Apply(prepared) = plan else {
            return Ok(WriteOutcome::AlreadyApplied);
        };
        let tx = self.db.tx_mut()?;
        let mut gate = WriteGate::new(fail_after);
        let result =
            apply_bootstrap_transaction(&tx, self.identity, self.path(), &prepared, &mut gate);
        finish_write(tx, result)
    }

    fn activate_finding_inner(
        &self,
        key: FindingKey,
        record: FindingRecord,
        parent: BlockNumHash,
        fail_after: Option<usize>,
    ) -> StoreResult<WriteOutcome> {
        validate_canonical(&record)
            .map_err(|_| StoreError::InvalidPersistedValue("finding record"))?;
        let alert = ActiveAlert {
            finding: key,
            last_verified_parent: parent,
        };
        validate_canonical(&MetaValue::ActiveAlert(alert))
            .map_err(|_| StoreError::InvalidPersistedValue("active alert"))?;
        let tx = self.db.tx_mut()?;
        let mut gate = WriteGate::new(fail_after);
        let result = activate_finding_transaction(&tx, key, &record, alert, &mut gate);
        finish_write(tx, result)
    }

    fn orphan_active_finding_inner(
        &self,
        key: FindingKey,
        fail_after: Option<usize>,
    ) -> StoreResult<WriteOutcome> {
        let tx = self.db.tx_mut()?;
        let mut gate = WriteGate::new(fail_after);
        let result = orphan_finding_transaction(&tx, key, &mut gate);
        finish_write(tx, result)
    }
}

fn validate_l1_replay_step(
    identity: StoreIdentity,
    cursor: Option<BlockNumHash>,
    expected_tempo: BlockNumHash,
    next_tempo: BlockNumHash,
) -> StoreResult<()> {
    if cursor.is_some_and(|cursor| cursor != expected_tempo) {
        return Err(StoreError::InvalidBootstrapProgress(
            "L1 replay cursor does not match its expected tip",
        ));
    }
    require_adjacent("Tempo bootstrap", expected_tempo, next_tempo)?;
    let creation = identity.portal_creation_block();
    if cursor.is_none() && next_tempo != creation {
        return Err(StoreError::L1ReplayFirstBlockMismatch {
            expected: creation,
            actual: next_tempo,
        });
    }
    Ok(())
}

struct PreparedBootstrap {
    commit: BootstrapCommit,
    before: BTreeMap<ModelKey, Option<ModelValue>>,
}

enum BootstrapPlan {
    Apply(Box<PreparedBootstrap>),
    AlreadyApplied,
}

fn prepare_bootstrap<TX: DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    path: &std::path::Path,
    mut commit: BootstrapCommit,
) -> StoreResult<BootstrapPlan> {
    let head = read_bootstrap_candidate_head(tx, identity, path, commit.next_state)?;
    if let Some(alert) = head.active_alert {
        return Err(StoreError::ActiveAlert(alert.finding));
    }
    if head.bootstrap == commit.next_state
        && head.imported_tempo_tip == commit.next_tempo
        && mutations_match(tx, &commit.changes)?
    {
        return Ok(BootstrapPlan::AlreadyApplied);
    }
    require_bootstrap_guards(&commit, head.bootstrap, head.imported_tempo_tip)?;
    let before = retain_changed_rows(tx, &mut commit.changes)?;
    validate_portal_settlement_change(
        tx,
        commit.next_state,
        head.verified_zone_tip,
        commit.next_tempo,
        commit.changes.get(&ModelKey::PortalSettlement),
    )?;
    Ok(BootstrapPlan::Apply(Box::new(PreparedBootstrap {
        commit,
        before,
    })))
}

fn apply_bootstrap_transaction<TX: DbTxMut + DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    path: &std::path::Path,
    prepared: &PreparedBootstrap,
    gate: &mut WriteGate,
) -> StoreResult<WriteOutcome> {
    let commit = &prepared.commit;
    let head = read_bootstrap_candidate_head(tx, identity, path, commit.next_state)?;
    if let Some(alert) = head.active_alert {
        return Err(StoreError::ActiveAlert(alert.finding));
    }
    require_bootstrap_guards(commit, head.bootstrap, head.imported_tempo_tip)?;
    for (key, before) in &prepared.before {
        if tx.get::<CheckerModelState>(*key)? != *before {
            return Err(StoreError::InvalidBootstrapProgress(
                "model row changed while preparing bootstrap commit",
            ));
        }
    }
    validate_portal_settlement_change(
        tx,
        commit.next_state,
        head.verified_zone_tip,
        commit.next_tempo,
        commit.changes.get(&ModelKey::PortalSettlement),
    )?;
    for (key, value) in &commit.changes {
        write_model_value(tx, *key, value)?;
        gate.wrote()?;
    }
    write_meta(
        tx,
        MetaKey::Bootstrap,
        MetaValue::Bootstrap(commit.next_state),
    )?;
    gate.wrote()?;
    write_meta(
        tx,
        MetaKey::ImportedTempoTip,
        MetaValue::ImportedTempoTip(commit.next_tempo),
    )?;
    gate.wrote()?;
    let first_creation = matches!(
        commit.expected_state,
        BootstrapState::L1Replay { cursor: None }
    );
    let zone_genesis_handoff = matches!(
        (commit.expected_state, commit.next_state),
        (
            BootstrapState::L1Replay { cursor: Some(_) },
            BootstrapState::ZoneReplay { .. }
        )
    );
    if first_creation || zone_genesis_handoff {
        // Creation and the Zone-genesis handoff change complete persisted
        // lifecycle phases. Validate each candidate once before commit; other
        // L1 blocks retain the sparse O(changes) path.
        let _validated = read_snapshot(tx, identity, path)?;
    }
    Ok(WriteOutcome::Applied)
}

fn read_bootstrap_candidate_head<TX: DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    path: &std::path::Path,
    candidate: BootstrapState,
) -> StoreResult<StoreProgress> {
    if candidate != BootstrapState::Live {
        return read_head(tx, identity, path);
    }

    let snapshot = read_snapshot(tx, identity, path)?;
    validate_bootstrap_candidate(tx, identity, candidate, &snapshot)?;
    Ok(StoreProgress {
        verified_zone_tip: snapshot.verified_zone_tip,
        imported_tempo_tip: snapshot.imported_tempo_tip,
        bootstrap: snapshot.bootstrap,
        active_alert: snapshot.active_alert,
    })
}

fn activate_finding_transaction<TX: DbTxMut + DbTx>(
    tx: &TX,
    key: FindingKey,
    record: &FindingRecord,
    alert: ActiveAlert,
    gate: &mut WriteGate,
) -> StoreResult<WriteOutcome> {
    let zone = read_tip(tx, MetaKey::VerifiedZoneTip)?;
    validate_finding_anchor(key, record, alert.last_verified_parent, zone)?;
    if tx.get::<CheckerCanonical>(key.zone_height)?.is_some() {
        return Err(StoreError::FindingConflict { key });
    }
    if let Some(existing_alert) = read_active_alert(tx)? {
        let existing = tx.get::<CheckerFindings>(existing_alert.finding)?;
        return if existing_alert == alert && existing.as_ref() == Some(record) {
            Ok(WriteOutcome::AlreadyApplied)
        } else {
            Err(StoreError::FindingConflict {
                key: existing_alert.finding,
            })
        };
    }
    if let Some(mut existing) = tx.get::<CheckerFindings>(key)? {
        if existing.status() != FindingStatus::Orphaned {
            return Err(StoreError::FindingConflict { key });
        }
        existing.mark_canonical();
        if &existing != record {
            return Err(StoreError::FindingConflict { key });
        }
        tx.put::<CheckerFindings>(key, existing)?;
        gate.wrote()?;
        tx.cursor_write::<CheckerMeta>()?
            .insert(MetaKey::ActiveAlert, &MetaValue::ActiveAlert(alert))?;
        gate.wrote()?;
        return Ok(WriteOutcome::Applied);
    }
    tx.cursor_write::<CheckerFindings>()?.insert(key, record)?;
    gate.wrote()?;
    tx.cursor_write::<CheckerMeta>()?
        .insert(MetaKey::ActiveAlert, &MetaValue::ActiveAlert(alert))?;
    gate.wrote()?;
    Ok(WriteOutcome::Applied)
}

fn orphan_finding_transaction<TX: DbTxMut + DbTx>(
    tx: &TX,
    expected: FindingKey,
    gate: &mut WriteGate,
) -> StoreResult<WriteOutcome> {
    let Some(alert) = read_active_alert(tx)? else {
        return match tx.get::<CheckerFindings>(expected)? {
            Some(record) if record.status() == FindingStatus::Orphaned => {
                Ok(WriteOutcome::AlreadyApplied)
            }
            _ => Err(StoreError::NoActiveFinding(expected)),
        };
    };
    if alert.finding != expected {
        return Err(StoreError::NoActiveFinding(expected));
    }
    let mut record = tx
        .get::<CheckerFindings>(expected)?
        .ok_or(StoreError::MissingActiveFinding(expected))?;
    if record.status() != FindingStatus::Canonical {
        return Err(StoreError::FindingStatus {
            key: expected,
            status: record.status(),
        });
    }
    record.mark_orphaned();
    validate_canonical(&record)
        .map_err(|_| StoreError::InvalidPersistedValue("orphaned finding"))?;
    tx.put::<CheckerFindings>(expected, record)?;
    gate.wrote()?;
    if !tx.delete::<CheckerMeta>(MetaKey::ActiveAlert, None)? {
        return Err(StoreError::NoActiveFinding(expected));
    }
    gate.wrote()?;
    Ok(WriteOutcome::Applied)
}

pub(super) fn validate_metadata_and_findings<TX: DbTx>(
    tx: &TX,
    zone_tip: BlockNumHash,
) -> StoreResult<()> {
    let active = read_active_alert(tx)?;
    let expected_metadata = 8 + usize::from(active.is_some());
    let actual_metadata = tx.entries::<CheckerMeta>()?;
    if actual_metadata != expected_metadata {
        return Err(StoreError::MetadataCardinality {
            expected: expected_metadata,
            actual: actual_metadata,
        });
    }
    let mut meta_cursor = tx.cursor_read::<CheckerMeta>()?;
    for row in meta_cursor.walk(None)? {
        let (key, value) = row?;
        if !value.matches_key(key) || validate_canonical(&value).is_err() {
            return Err(StoreError::InvalidPersistedValue("metadata row"));
        }
    }
    let mut canonical_findings = 0_usize;
    let mut finding_cursor = tx.cursor_read::<CheckerFindings>()?;
    for row in finding_cursor.walk(None)? {
        let (key, record) = row?;
        validate_canonical(&record)
            .map_err(|_| StoreError::InvalidPersistedValue("finding row"))?;
        if record.status() == FindingStatus::Canonical {
            canonical_findings += 1;
            let alert = active.ok_or(StoreError::FindingConflict { key })?;
            if alert.finding != key {
                return Err(StoreError::FindingConflict { key });
            }
            validate_finding_anchor(key, &record, alert.last_verified_parent, zone_tip)?;
        }
    }
    if canonical_findings != usize::from(active.is_some()) {
        return Err(StoreError::CanonicalSequence(
            "active finding cardinality is inconsistent",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn consolidate(mutations: Vec<ModelMutation>) -> StoreResult<ModelRowChanges> {
    let mut changes = BTreeMap::new();
    for mutation in mutations {
        let (key, value) = mutation.into_parts();
        if changes.insert(key, value).is_some() {
            return Err(StoreError::DuplicateMutation(key));
        }
    }
    validate_changes(changes)
}

pub(super) fn validate_changes(changes: ModelRowChanges) -> StoreResult<ModelRowChanges> {
    if changes.len() >= u32::MAX as usize {
        return Err(StoreError::TooManyMutations);
    }
    for (key, value) in &changes {
        if let Some(value) = value {
            validate_model_value(*key, value)?;
        }
    }
    Ok(changes)
}

pub(super) fn validate_model_value(key: ModelKey, value: &ModelValue) -> StoreResult<()> {
    if !value.matches_key(key) {
        return Err(StoreError::ModelKeyValueMismatch {
            key,
            value: Box::new(value.clone()),
        });
    }
    validate_canonical(value).map_err(|_| StoreError::InvalidPersistedValue("model value"))
}

/// Drop physical no-ops and capture exact before-images using only rows named
/// by the typed logical delta.
pub(super) fn retain_changed_rows<TX: DbTx>(
    tx: &TX,
    changes: &mut ModelRowChanges,
) -> StoreResult<BTreeMap<ModelKey, Option<ModelValue>>> {
    let mut before = BTreeMap::new();
    for (key, next) in changes.iter() {
        let current = tx.get::<CheckerModelState>(*key)?;
        if let Some(value) = &current {
            validate_model_value(*key, value)?;
        }
        if current.as_ref() != next.as_ref() {
            before.insert(*key, current);
        }
    }
    changes.retain(|key, _| before.contains_key(key));
    Ok(before)
}

pub(super) fn reject_active_alert<TX: DbTx>(tx: &TX) -> StoreResult<()> {
    match read_active_alert(tx)? {
        Some(alert) => Err(StoreError::ActiveAlert(alert.finding)),
        None => Ok(()),
    }
}

fn mutations_match<TX: DbTx>(tx: &TX, changes: &ModelRowChanges) -> StoreResult<bool> {
    for (key, value) in changes {
        if tx.get::<CheckerModelState>(*key)? != *value {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn write_model_value<TX: DbTxMut>(
    tx: &TX,
    key: ModelKey,
    value: &Option<ModelValue>,
) -> StoreResult<()> {
    match value {
        Some(value) => tx.put::<CheckerModelState>(key, value.clone())?,
        None => {
            tx.delete::<CheckerModelState>(key, None)?;
        }
    }
    Ok(())
}

pub(super) fn write_meta<TX: DbTxMut>(tx: &TX, key: MetaKey, value: MetaValue) -> StoreResult<()> {
    validate_canonical(&value).map_err(|_| StoreError::InvalidPersistedValue("metadata value"))?;
    tx.put::<CheckerMeta>(key, value)?;
    Ok(())
}

pub(super) fn finish_write<TX: DbTx>(
    tx: TX,
    result: StoreResult<WriteOutcome>,
) -> StoreResult<WriteOutcome> {
    match result {
        Ok(WriteOutcome::Applied) => {
            tx.commit()?;
            Ok(WriteOutcome::Applied)
        }
        Ok(WriteOutcome::AlreadyApplied) => {
            tx.abort();
            Ok(WriteOutcome::AlreadyApplied)
        }
        Err(error) => {
            tx.abort();
            Err(error)
        }
    }
}

fn require_bootstrap_guards(
    commit: &BootstrapCommit,
    state: BootstrapState,
    tempo: BlockNumHash,
) -> StoreResult<()> {
    validate_bootstrap_coherence(state, tempo)?;
    if state != commit.expected_state {
        return Err(StoreError::BootstrapChanged {
            expected: commit.expected_state,
            actual: state,
        });
    }
    if tempo != commit.expected_tempo {
        return Err(StoreError::ImportedTipChanged {
            expected: commit.expected_tempo,
            actual: tempo,
        });
    }
    Ok(())
}

fn validate_finding_anchor(
    key: FindingKey,
    record: &FindingRecord,
    supplied_parent: BlockNumHash,
    zone: BlockNumHash,
) -> StoreResult<()> {
    if supplied_parent != zone
        || zone.number.checked_add(1) != Some(key.zone_height)
        || record.zone_parent_hash() != zone.hash
    {
        return Err(StoreError::FindingParent { key, parent: zone });
    }
    if record.status() != FindingStatus::Canonical {
        return Err(StoreError::FindingStatus {
            key,
            status: record.status(),
        });
    }
    Ok(())
}

pub(super) fn require_adjacent(
    chain: &'static str,
    parent: BlockNumHash,
    child: BlockNumHash,
) -> StoreResult<()> {
    if parent.number.checked_add(1) == Some(child.number) {
        Ok(())
    } else {
        Err(StoreError::NonAdjacent {
            chain,
            parent,
            child,
        })
    }
}

#[derive(Debug)]
pub(super) struct WriteGate {
    #[cfg(test)]
    fail_after: Option<usize>,
    writes: usize,
}

impl WriteGate {
    #[cfg(test)]
    pub(super) const fn new(fail_after: Option<usize>) -> Self {
        Self {
            fail_after,
            writes: 0,
        }
    }

    #[cfg(not(test))]
    pub(super) const fn new(_: Option<usize>) -> Self {
        Self { writes: 0 }
    }

    pub(super) fn wrote(&mut self) -> StoreResult<()> {
        self.writes += 1;
        #[cfg(test)]
        if self.fail_after == Some(self.writes) {
            return Err(StoreError::InjectedWriteFailure);
        }
        Ok(())
    }
}
