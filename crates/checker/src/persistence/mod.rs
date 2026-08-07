//! Compact checkpoint and canonical-journal persistence.
//!
//! This is intentionally independent of the legacy `store`, which remains the
//! production oracle during the compact rewrite.

#![allow(dead_code)] // Compact runtime remains an internal shadow path until cutover.

mod codec;
mod schema;
#[cfg(test)]
mod tests;
mod types;

pub(crate) use types::*;

use reth_db::{
    Database, DatabaseEnv, DatabaseEnvKind,
    cursor::{DbCursorRO, DbCursorRW},
    is_database_empty,
    mdbx::{DatabaseArguments, init_db_for},
    open_db_read_only,
    transaction::{DbTx, DbTxMut},
};
use schema::{Checkpoints, Findings, Journal, Meta, MetaKey, PersistenceTables};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use zone_checker_kernel::{State, validate};

pub(crate) const SCHEMA_VERSION: u32 = 2;
pub(crate) type Result<T> = std::result::Result<T, PersistenceError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PersistenceError {
    #[error("checker persistence open error: {0}")]
    Open(#[from] eyre::Report),
    #[error("checker persistence database error: {0}")]
    Database(#[from] reth_db::DatabaseError),
    #[error("checker persistence codec error: {0}")]
    Codec(#[from] codec::CodecError),
    #[error(
        "incompatible checker schema: expected {expected}, actual {actual}; rebuild at {rebuild_path}"
    )]
    Schema {
        expected: u32,
        actual: u32,
        rebuild_path: PathBuf,
    },
    #[error("checker identity mismatch")]
    Identity,
    #[error("invalid checker persistence: {0}")]
    Invalid(String),
    #[error("injected transaction abort")]
    InjectedAbort,
}

pub(crate) struct Persistence {
    db: Arc<DatabaseEnv>,
    path: PathBuf,
    #[cfg(test)]
    abort_next_write: AtomicBool,
}

impl Persistence {
    /// Read the authenticated identity from an existing compact database.
    /// This never creates or repairs a database and is intended for runtime
    /// preflight before opening the sole-writer handle.
    pub(crate) fn inspect_identity(path: impl AsRef<Path>) -> Result<Identity> {
        let path = path.as_ref();
        probe(path)?;
        let db = open_db_read_only(path, DatabaseArguments::default())?;
        let tx = db.tx()?;
        let value = tx
            .get::<Meta>(MetaKey::State)?
            .ok_or_else(|| invalid("missing Meta singleton"))?;
        let MetaValue::State(meta) = value else {
            return Err(invalid("metadata type mismatch"));
        };
        validate_metadata(&meta)?;
        tx.commit()?;
        Ok(meta.identity)
    }

    pub(crate) fn create(
        path: impl AsRef<Path>,
        identity: Identity,
        cut: ChainCut,
        state: State,
    ) -> Result<(Self, Snapshot)> {
        let path = path.as_ref().to_path_buf();
        if !is_database_empty(&path) {
            return Err(PersistenceError::Invalid("fresh path is not empty".into()));
        }
        validate_state(&state, identity)?;
        let db = init_db_for::<_, PersistenceTables>(&path, DatabaseArguments::default())?;
        let this = Self {
            db: Arc::new(db),
            path,
            #[cfg(test)]
            abort_next_write: AtomicBool::new(false),
        };
        let id = CheckpointId::from(cut.zone);
        let meta = Metadata {
            identity,
            active_checkpoint: id,
            verified_zone_tip: cut.zone,
            imported_tempo_tip: cut.tempo,
            acknowledged_zone_tip: cut.zone,
            active_finding: None,
            coverage: Coverage::Complete,
        };
        codec::encode(&Checkpoint {
            cut,
            state: state.clone(),
        })?;
        codec::encode(&meta)?;
        let tx = this.db.tx_mut()?;
        tx.put::<Checkpoints>(id, Checkpoint { cut, state })?;
        tx.put::<Meta>(MetaKey::Version, MetaValue::Version(SCHEMA_VERSION))?;
        tx.put::<Meta>(MetaKey::State, MetaValue::State(Box::new(meta)))?;
        tx.commit()?;
        this.load(identity).map(|snapshot| (this, snapshot))
    }

    pub(crate) fn open(path: impl AsRef<Path>, identity: Identity) -> Result<(Self, Snapshot)> {
        let path = path.as_ref().to_path_buf();
        probe(&path)?;
        let db = DatabaseEnv::open(&path, DatabaseEnvKind::RW, DatabaseArguments::default())?;
        let this = Self {
            db: Arc::new(db),
            path,
            #[cfg(test)]
            abort_next_write: AtomicBool::new(false),
        };
        this.load(identity).map(|snapshot| (this, snapshot))
    }

    pub(crate) fn load(&self, identity: Identity) -> Result<Snapshot> {
        let tx = self.db.tx()?;
        let meta = tx
            .get::<Meta>(MetaKey::State)?
            .ok_or_else(|| invalid("missing Meta singleton"))?;
        let MetaValue::State(meta) = meta else {
            return Err(invalid("metadata type mismatch"));
        };
        let meta = *meta;
        if meta.identity != identity {
            return Err(PersistenceError::Identity);
        }
        validate_metadata(&meta)?;
        let active = tx
            .get::<Checkpoints>(meta.active_checkpoint)?
            .ok_or_else(|| invalid("active checkpoint is missing"))?;
        validate_checkpoint(meta.active_checkpoint, &active, identity)?;
        let mut checkpoints = tx.cursor_read::<Checkpoints>()?;
        let (bootstrap_id, bootstrap) = checkpoints
            .first()?
            .ok_or_else(|| invalid("bootstrap checkpoint is missing"))?;
        validate_checkpoint(bootstrap_id, &bootstrap, identity)?;
        for row in checkpoints.walk(None)? {
            let (id, checkpoint) = row?;
            validate_checkpoint(id, &checkpoint, identity)?;
        }
        let mut state = bootstrap.state.clone();
        validate_state(&state, identity)?;
        let mut tip = bootstrap.cut.zone;
        let mut imported = bootstrap.cut.tempo;
        if tip.number > meta.verified_zone_tip.number {
            return Err(invalid("bootstrap exceeds verified tip"));
        }
        if meta.active_checkpoint == bootstrap_id && active != bootstrap {
            return Err(invalid("active bootstrap checkpoint mismatch"));
        }
        for height in tip.number.saturating_add(1)..=meta.verified_zone_tip.number {
            let entry = tx
                .get::<Journal>(height)?
                .ok_or_else(|| invalid(format!("missing journal height {height}")))?;
            if entry.zone.number != height
                || entry.parent != tip
                || entry.delta.validate().is_err()
                || entry.imported_tempo.number
                    != imported
                        .number
                        .checked_add(1)
                        .ok_or_else(|| invalid("tempo height overflow"))?
                || entry.imported_tempo_parent != imported
            {
                return Err(invalid(format!("conflicting journal height {height}")));
            }
            state
                .apply(&entry.delta)
                .map_err(|e| invalid(e.to_string()))?;
            tip = entry.zone;
            imported = entry.imported_tempo;
            if meta.active_checkpoint.height == height
                && (meta.active_checkpoint.hash != tip.hash
                    || active.cut.zone != tip
                    || active.cut.tempo != imported
                    || active.state != state)
            {
                return Err(invalid("active checkpoint is not on the canonical journal"));
            }
        }
        if tip != meta.verified_zone_tip || imported != meta.imported_tempo_tip {
            return Err(invalid("journal does not reach verified tip"));
        }
        let mut journal = tx.cursor_read::<Journal>()?;
        if journal
            .last()?
            .is_some_and(|(height, _)| height > meta.verified_zone_tip.number)
        {
            return Err(invalid("journal extends beyond verified tip"));
        }
        validate_state(&state, identity)?;
        if let Some(key) = meta.active_finding {
            let finding = tx
                .get::<Findings>(key)?
                .ok_or_else(|| invalid("active finding row is missing"))?;
            validate_finding(key, &finding, Some(&meta))?;
        }
        let mut findings = tx.cursor_read::<Findings>()?;
        for row in findings.walk(None)? {
            let (key, finding) = row?;
            // Orphaned findings are immutable audit records. Only the active
            // latch is required to sit at the next verified coordinate.
            validate_finding(key, &finding, None)?;
        }
        tx.commit()?;
        Ok(Snapshot { meta, state })
    }

    pub(crate) fn apply(
        &self,
        identity: Identity,
        entry: JournalEntry,
        acknowledged: BlockNumHash,
        coverage: Coverage,
    ) -> Result<Snapshot> {
        let prior = self.load(identity)?;
        let mut candidate = prior.state;
        candidate
            .apply(&entry.delta)
            .map_err(|error| invalid(error.to_string()))?;
        validate_state(&candidate, identity)?;
        codec::encode(&entry)?;
        self.write(identity, |tx, meta| {
            if entry.zone.number
                != meta
                    .verified_zone_tip
                    .number
                    .checked_add(1)
                    .ok_or_else(|| invalid("height overflow"))?
                || entry.parent != meta.verified_zone_tip
                || entry.imported_tempo.number
                    != meta
                        .imported_tempo_tip
                        .number
                        .checked_add(1)
                        .ok_or_else(|| invalid("tempo height overflow"))?
                || entry.imported_tempo_parent != meta.imported_tempo_tip
            {
                return Err(invalid("wrong next journal parent or height"));
            }
            validate_coverage_advance(meta, entry.zone, acknowledged, &coverage)?;
            if tx.get::<Journal>(entry.zone.number)?.is_some() {
                return Err(invalid("journal height conflict"));
            }
            tx.put::<Journal>(entry.zone.number, entry.clone())?;
            meta.verified_zone_tip = entry.zone;
            meta.imported_tempo_tip = entry.imported_tempo;
            meta.coverage = coverage;
            meta.acknowledged_zone_tip = acknowledged;
            Ok(())
        })
    }

    pub(crate) fn checkpoint(
        &self,
        identity: Identity,
        cut: ChainCut,
        state: State,
    ) -> Result<Snapshot> {
        validate_state(&state, identity)?;
        if self.load(identity)?.state != state {
            return Err(invalid(
                "checkpoint state is not the authoritative current state",
            ));
        }
        let checkpoint = Checkpoint { cut, state };
        codec::encode(&checkpoint)?;
        self.write(identity, |tx, meta| {
            if cut.zone != meta.verified_zone_tip || cut.tempo != meta.imported_tempo_tip {
                return Err(invalid("checkpoint is not current"));
            }
            let id = CheckpointId::from(cut.zone);
            if let Some(existing) = tx.get::<Checkpoints>(id)? {
                if existing != checkpoint {
                    return Err(invalid("checkpoint identity is immutable"));
                }
            } else {
                tx.put::<Checkpoints>(id, checkpoint.clone())?;
            }
            meta.active_checkpoint = id;
            Ok(())
        })
    }

    pub(crate) fn record_finding(
        &self,
        identity: Identity,
        key: FindingKey,
        finding: Finding,
    ) -> Result<Snapshot> {
        validate_finding(key, &finding, None)?;
        codec::encode(&finding)?;
        self.write(identity, |tx, meta| {
            if finding.zone.number
                != meta
                    .verified_zone_tip
                    .number
                    .checked_add(1)
                    .ok_or_else(|| invalid("height overflow"))?
                || finding.parent != meta.verified_zone_tip
                || finding.imported_tempo.is_some_and(|tempo| {
                    tempo.number != meta.imported_tempo_tip.number.saturating_add(1)
                })
            {
                return Err(invalid("finding is not at the next verified coordinate"));
            }
            validate_finding(key, &finding, Some(meta))?;
            if let Some(old) = tx.get::<Findings>(key)? {
                let mut old_identity = old.clone();
                old_identity.summary.clear();
                let mut new_identity = finding.clone();
                new_identity.summary.clear();
                if old_identity != new_identity {
                    return Err(invalid("conflicting same-height finding evidence"));
                }
                if old.summary != finding.summary {
                    tx.put::<Findings>(key, finding.clone())?;
                }
            } else {
                tx.put::<Findings>(key, finding.clone())?;
            }
            meta.active_finding = Some(key);
            Ok(())
        })
    }

    pub(crate) fn record_gap(
        &self,
        identity: Identity,
        first_unchecked: BlockNumHash,
        acknowledged_through: BlockNumHash,
        reason: CoverageGapReason,
    ) -> Result<Snapshot> {
        self.write(identity, |_tx, meta| {
            if meta.verified_zone_tip.number.checked_add(1) != Some(first_unchecked.number)
                || acknowledged_through.number < first_unchecked.number
                || acknowledged_through.number < meta.acknowledged_zone_tip.number
            {
                return Err(invalid("invalid coverage gap range"));
            }
            if let Coverage::Gap {
                first_unchecked: existing_first,
                reason: existing_reason,
                ..
            } = &meta.coverage
                && (*existing_first != first_unchecked || *existing_reason != reason)
            {
                return Err(invalid(
                    "coverage gap identity cannot change before recovery",
                ));
            }
            meta.acknowledged_zone_tip = acknowledged_through;
            meta.coverage = Coverage::Gap {
                first_unchecked,
                acknowledged_through,
                reason,
            };
            Ok(())
        })
    }

    pub(crate) fn reorg(&self, identity: Identity, ancestor: BlockNumHash) -> Result<Snapshot> {
        let current = self.load(identity)?;
        if ancestor.number > current.meta.verified_zone_tip.number {
            if ancestor.number > current.meta.acknowledged_zone_tip.number {
                return Err(invalid("reorg ancestor exceeds acknowledged history"));
            }
            return self.write(identity, |_tx, meta| {
                if let Some(key) = meta.active_finding {
                    if key.zone.number == ancestor.number && key.zone.hash != ancestor.hash {
                        return Err(invalid("conflicting evidence at finding height"));
                    }
                    if key.zone.number > ancestor.number {
                        meta.active_finding = None;
                    }
                }
                let Coverage::Gap {
                    first_unchecked,
                    reason,
                    ..
                } = &meta.coverage
                else {
                    return Err(invalid(
                        "unverified reorg ancestor has no durable coverage gap",
                    ));
                };
                if first_unchecked.number > ancestor.number {
                    return Err(invalid(
                        "unverified reorg ancestor precedes the coverage gap",
                    ));
                }
                meta.acknowledged_zone_tip = ancestor;
                meta.coverage = Coverage::Gap {
                    first_unchecked: *first_unchecked,
                    acknowledged_through: ancestor,
                    reason: reason.clone(),
                };
                Ok(())
            });
        }
        let snapshot = self.reconstruct_at(identity, ancestor)?;
        self.write(identity, |tx, meta| {
            let mut cursor = tx.cursor_write::<Journal>()?;
            while let Some((height, _)) = cursor.last()? {
                if height <= ancestor.number {
                    break;
                }
                cursor.delete_current()?;
            }
            meta.verified_zone_tip = ancestor;
            meta.imported_tempo_tip = snapshot.meta.imported_tempo_tip;
            meta.active_checkpoint = snapshot.meta.active_checkpoint;
            if meta.acknowledged_zone_tip.number > ancestor.number {
                meta.acknowledged_zone_tip = ancestor;
            }
            meta.coverage = match &meta.coverage {
                Coverage::Gap {
                    first_unchecked, ..
                } if first_unchecked.number <= ancestor.number => Coverage::Gap {
                    first_unchecked: *first_unchecked,
                    acknowledged_through: ancestor,
                    reason: match &meta.coverage {
                        Coverage::Gap { reason, .. } => reason.clone(),
                        Coverage::Complete => unreachable!(),
                    },
                },
                _ => Coverage::Complete,
            };
            if let Some(key) = meta.active_finding {
                if key.zone.number == ancestor.number && key.zone.hash != ancestor.hash {
                    return Err(invalid("conflicting evidence at finding height"));
                }
                if key.zone.number > ancestor.number {
                    meta.active_finding = None;
                }
            }
            Ok(())
        })
    }

    fn reconstruct_at(&self, identity: Identity, ancestor: BlockNumHash) -> Result<Snapshot> {
        let tx = self.db.tx()?;
        let meta = tx
            .get::<Meta>(MetaKey::State)?
            .ok_or_else(|| invalid("missing Meta"))?;
        let MetaValue::State(meta) = meta else {
            return Err(invalid("metadata type mismatch"));
        };
        let meta = *meta;
        if meta.identity != identity || ancestor.number > meta.verified_zone_tip.number {
            return Err(invalid("invalid reorg ancestor"));
        }
        let mut best = None;
        let mut cur = tx.cursor_read::<Checkpoints>()?;
        let bootstrap_id = cur
            .first()?
            .map(|(id, _)| id)
            .ok_or_else(|| invalid("bootstrap checkpoint is missing"))?;
        for row in cur.walk(None)? {
            let (id, cp) = row?;
            validate_checkpoint(id, &cp, identity)?;
            let canonical = if id == bootstrap_id {
                true
            } else {
                tx.get::<Journal>(id.height)?
                    .is_some_and(|entry| entry.zone == cp.cut.zone)
            };
            if canonical
                && cp.cut.zone.number <= ancestor.number
                && best
                    .as_ref()
                    .is_none_or(|(_, b): &(CheckpointId, Checkpoint)| {
                        b.cut.zone.number < cp.cut.zone.number
                    })
            {
                best = Some((id, cp));
            }
        }
        let (checkpoint_id, cp) = best.ok_or_else(|| invalid("no checkpoint before ancestor"))?;
        let mut state = cp.state;
        let mut tip = cp.cut.zone;
        let mut imported = cp.cut.tempo;
        for h in tip.number.saturating_add(1)..=ancestor.number {
            let e = tx
                .get::<Journal>(h)?
                .ok_or_else(|| invalid("missing reorg journal"))?;
            if e.zone.number != h
                || e.parent != tip
                || e.imported_tempo_parent != imported
                || e.imported_tempo.number
                    != imported
                        .number
                        .checked_add(1)
                        .ok_or_else(|| invalid("tempo height overflow"))?
            {
                return Err(invalid("non-contiguous reorg journal"));
            }
            if e.parent != tip {
                return Err(invalid("reorg journal parent mismatch"));
            }
            state.apply(&e.delta).map_err(|e| invalid(e.to_string()))?;
            tip = e.zone;
            imported = e.imported_tempo;
        }
        if tip != ancestor {
            return Err(invalid("ancestor hash conflict"));
        }
        validate_state(&state, identity)?;
        let mut out = meta;
        out.active_checkpoint = checkpoint_id;
        out.verified_zone_tip = ancestor;
        out.imported_tempo_tip = imported;
        Ok(Snapshot { meta: out, state })
    }

    fn write<F>(&self, identity: Identity, f: F) -> Result<Snapshot>
    where
        F: FnOnce(&<DatabaseEnv as Database>::TXMut, &mut Metadata) -> Result<()>,
    {
        let tx = self.db.tx_mut()?;
        let meta = tx
            .get::<Meta>(MetaKey::State)?
            .ok_or_else(|| invalid("missing Meta"))?;
        let MetaValue::State(meta) = meta else {
            return Err(invalid("metadata type mismatch"));
        };
        let mut meta = *meta;
        if meta.identity != identity {
            tx.abort();
            return Err(PersistenceError::Identity);
        }
        if let Err(e) = f(&tx, &mut meta) {
            tx.abort();
            return Err(e);
        }
        tx.put::<Meta>(MetaKey::State, MetaValue::State(Box::new(meta)))?;
        #[cfg(test)]
        if self.abort_next_write.swap(false, Ordering::SeqCst) {
            tx.abort();
            return Err(PersistenceError::InjectedAbort);
        }
        tx.commit()?;
        self.load(identity)
    }

    #[cfg(test)]
    pub(crate) fn inject_abort(&self) {
        self.abort_next_write.store(true, Ordering::SeqCst);
    }
}

fn probe(path: &Path) -> Result<()> {
    let db = open_db_read_only(path, DatabaseArguments::default())?;
    let tx = db.tx()?;
    let raw = tx
        .get::<Meta>(MetaKey::Version)?
        .ok_or_else(|| invalid("missing Meta"))?;
    let MetaValue::Version(actual) = raw else {
        return Err(invalid("schema version type mismatch"));
    };
    tx.commit()?;
    if actual != SCHEMA_VERSION {
        return Err(schema_error(actual, path));
    }
    Ok(())
}
fn schema_error(actual: u32, path: &Path) -> PersistenceError {
    PersistenceError::Schema {
        expected: SCHEMA_VERSION,
        actual,
        rebuild_path: path.with_extension("rebuild"),
    }
}
fn invalid(message: impl Into<String>) -> PersistenceError {
    PersistenceError::Invalid(message.into())
}
fn validate_state(state: &State, identity: Identity) -> Result<()> {
    State::from_rows(state.rows().clone()).map_err(|e| invalid(e.to_string()))?;
    validate(state).map_err(|e| invalid(format!("invariant {e:?}")))?;
    let Some(zone_checker_kernel::StateValue::Portal(portal)) =
        state.rows().get(&zone_checker_kernel::StateKey::Portal)
    else {
        return Err(invalid("missing Portal identity"));
    };
    let portal_identity = portal.identity();
    if portal_identity.portal != identity.portal || portal_identity.zone_id != identity.zone_id {
        return Err(PersistenceError::Identity);
    }
    Ok(())
}

fn validate_checkpoint(
    id: CheckpointId,
    checkpoint: &Checkpoint,
    identity: Identity,
) -> Result<()> {
    if id != CheckpointId::from(checkpoint.cut.zone) {
        return Err(invalid("checkpoint key does not match its embedded cut"));
    }
    validate_state(&checkpoint.state, identity)
}

fn finding_evidence(expected: &[u8], actual: &[u8]) -> Result<(u32, alloy_primitives::B256)> {
    let mut canonical = Vec::with_capacity(8 + expected.len() + actual.len());
    canonical.extend(
        u32::try_from(expected.len())
            .map_err(|_| invalid("expected too large"))?
            .to_be_bytes(),
    );
    canonical.extend(expected);
    canonical.extend(
        u32::try_from(actual.len())
            .map_err(|_| invalid("actual too large"))?
            .to_be_bytes(),
    );
    canonical.extend(actual);
    Ok((
        u32::try_from(canonical.len()).map_err(|_| invalid("evidence too large"))?,
        alloy_primitives::keccak256(canonical),
    ))
}

fn validate_finding(key: FindingKey, finding: &Finding, meta: Option<&Metadata>) -> Result<()> {
    let (evidence_len, evidence_digest) = finding_evidence(&finding.expected, &finding.actual)?;
    if finding.zone != key.zone
        || finding.operation != key.operation
        || finding.code != key.code
        || finding.expected.len() > 256
        || finding.actual.len() > 256
        || finding.summary.len() > 1_024
        || finding.evidence_len != evidence_len
        || finding.evidence_digest != evidence_digest
    {
        return Err(invalid("finding is inconsistent or exceeds compact bounds"));
    }
    if let Some(meta) = meta {
        let next = meta
            .verified_zone_tip
            .number
            .checked_add(1)
            .ok_or_else(|| invalid("height overflow"))?;
        if finding.zone.number != next
            || finding.parent != meta.verified_zone_tip
            || finding.imported_tempo.is_some_and(|tempo| {
                tempo.number != meta.imported_tempo_tip.number.saturating_add(1)
            })
        {
            return Err(invalid("finding is not at the next verified coordinate"));
        }
    }
    Ok(())
}

fn validate_metadata(meta: &Metadata) -> Result<()> {
    if meta.active_checkpoint.height > meta.verified_zone_tip.number
        || meta.acknowledged_zone_tip.number < meta.verified_zone_tip.number
    {
        return Err(invalid("metadata tips or active checkpoint are incoherent"));
    }
    match &meta.coverage {
        Coverage::Complete if meta.acknowledged_zone_tip != meta.verified_zone_tip => {
            Err(invalid("complete coverage tips differ"))
        }
        Coverage::Gap {
            first_unchecked,
            acknowledged_through,
            ..
        } if meta.verified_zone_tip.number.checked_add(1) != Some(first_unchecked.number)
            || *acknowledged_through != meta.acknowledged_zone_tip =>
        {
            Err(invalid("coverage gap coordinates are incoherent"))
        }
        _ => Ok(()),
    }
}

fn validate_coverage_advance(
    meta: &Metadata,
    child: BlockNumHash,
    acknowledged: BlockNumHash,
    next: &Coverage,
) -> Result<()> {
    if acknowledged.number < meta.acknowledged_zone_tip.number {
        return Err(invalid("acknowledged tip cannot regress"));
    }
    match (&meta.coverage, next) {
        (Coverage::Complete, Coverage::Complete) if acknowledged == child => Ok(()),
        (
            Coverage::Complete,
            Coverage::Gap {
                first_unchecked,
                acknowledged_through,
                ..
            },
        ) if child.number.checked_add(1) == Some(first_unchecked.number)
            && *acknowledged_through == acknowledged
            && acknowledged.number >= first_unchecked.number =>
        {
            Ok(())
        }
        (
            Coverage::Gap {
                first_unchecked,
                acknowledged_through,
                reason: _,
            },
            Coverage::Complete,
        ) if child == *first_unchecked
            && child == *acknowledged_through
            && acknowledged == *acknowledged_through =>
        {
            Ok(())
        }
        (
            Coverage::Gap {
                first_unchecked,
                acknowledged_through,
                reason,
            },
            Coverage::Gap {
                first_unchecked: next_first,
                acknowledged_through: next_through,
                reason: next_reason,
            },
        ) if child == *first_unchecked
            && child.number.checked_add(1) == Some(next_first.number)
            && *next_through == *acknowledged_through
            && acknowledged == *acknowledged_through
            && next_reason == reason =>
        {
            Ok(())
        }
        _ => Err(invalid("coverage transition is inconsistent")),
    }
}
