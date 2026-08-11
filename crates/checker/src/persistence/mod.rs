mod codec;
mod schema;
#[cfg(test)]
mod tests;
mod types;

pub(crate) use types::*;

use crate::kernel::{Datum, Finding as FindingDetails, FindingLocation, State, validate};
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
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) const SCHEMA_VERSION: u32 = 3;
const CHECKPOINT_INTERVAL: u64 = 64;
pub(crate) type Result<T> = std::result::Result<T, PersistenceError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PersistenceError {
    #[error("database open failed: {0}")]
    Open(#[from] eyre::Report),
    #[error("database error: {0}")]
    Database(#[from] reth_db::DatabaseError),
    #[error("database codec error: {0}")]
    Codec(#[from] codec::CodecError),
    #[error(
        "incompatible checker schema: expected {expected}, found {actual}; rebuild at {rebuild_path}"
    )]
    Schema {
        expected: u32,
        actual: u32,
        rebuild_path: PathBuf,
    },
    #[error("database identity mismatch")]
    Identity,
    #[error("stale database snapshot")]
    StaleSnapshot,
    #[error("invalid checker database: {0}")]
    Invalid(String),
    #[cfg(test)]
    #[error("injected transaction abort")]
    InjectedAbort,
}

pub(crate) struct Persistence {
    db: Arc<DatabaseEnv>,
    identity: Identity,
    checkpoint_interval: u64,
    #[cfg(test)]
    abort_next_write: AtomicBool,
}

impl Persistence {
    /// Create, verify, and atomically publish an initial checker database.
    pub(crate) fn create_atomic(
        target: &Path,
        identity: Identity,
        cut: ChainCut,
        state: State,
    ) -> Result<Snapshot> {
        if target.exists() {
            return Err(invalid("checkpoint target already exists"));
        }
        let parent = target
            .parent()
            .ok_or_else(|| invalid("checkpoint target has no sibling directory"))?;
        let name = target
            .file_name()
            .ok_or_else(|| invalid("checkpoint target has no directory name"))?;
        let staging = parent.join(format!(
            ".{}.staging-{}",
            name.to_string_lossy(),
            std::process::id()
        ));
        if staging.exists() {
            fs::remove_dir_all(&staging)
                .map_err(|error| invalid(format!("cannot remove checkpoint staging: {error}")))?;
        }
        fs::create_dir_all(&staging)
            .map_err(|error| invalid(format!("cannot create checkpoint staging: {error}")))?;

        let result = (|| {
            let (store, snapshot) = Self::create(&staging, identity, cut, state)?;
            drop(store);
            let (reopened, verified) = Self::open(&staging, identity)?;
            drop(reopened);
            if snapshot != verified {
                return Err(invalid("genesis checkpoint changed across final reopen"));
            }
            Ok(verified)
        })();
        match result {
            Ok(snapshot) => {
                fs::rename(&staging, target).map_err(|error| {
                    let _ = fs::remove_dir_all(&staging);
                    invalid(format!("cannot atomically publish checkpoint: {error}"))
                })?;
                Ok(snapshot)
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                Err(error)
            }
        }
    }

    /// Read the authenticated identity from an existing checker database.
    /// This never creates or repairs a database and is intended for runtime
    /// preflight before opening the sole-writer handle.
    pub(crate) fn inspect_identity(path: impl AsRef<Path>) -> Result<Identity> {
        let path = path.as_ref();
        probe(path)?;
        let db = open_db_read_only(path, DatabaseArguments::default())?;
        let tx = db.tx()?;
        let value = tx
            .get::<Meta>(MetaKey::Metadata)?
            .ok_or_else(|| invalid("metadata row is missing"))?;
        let MetaValue::Metadata(meta) = value else {
            return Err(invalid("metadata type mismatch"));
        };
        validate_metadata(&meta)?;
        tx.commit()?;
        Ok(meta.identity)
    }

    pub(crate) fn inspect_snapshot(path: impl AsRef<Path>) -> Result<Snapshot> {
        let path = path.as_ref();
        let identity = Self::inspect_identity(path)?;
        let db = open_db_read_only(path, DatabaseArguments::default())?;
        Self {
            db: Arc::new(db),
            identity,
            checkpoint_interval: CHECKPOINT_INTERVAL,
            #[cfg(test)]
            abort_next_write: AtomicBool::new(false),
        }
        .load()
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
            identity,
            checkpoint_interval: CHECKPOINT_INTERVAL,
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
        let checkpoint = Checkpoint {
            cut,
            state: state.clone(),
        };
        codec::encode(&checkpoint)?;
        codec::encode(&meta)?;
        let tx = this.db.tx_mut()?;
        tx.put::<Checkpoints>(id, checkpoint)?;
        tx.put::<Meta>(MetaKey::Version, MetaValue::Version(SCHEMA_VERSION))?;
        tx.put::<Meta>(
            MetaKey::Metadata,
            MetaValue::Metadata(Box::new(meta.clone())),
        )?;
        tx.commit()?;
        Ok((this, Snapshot { meta, state }))
    }

    pub(crate) fn open(path: impl AsRef<Path>, identity: Identity) -> Result<(Self, Snapshot)> {
        let path = path.as_ref().to_path_buf();
        probe(&path)?;
        let db = DatabaseEnv::open(&path, DatabaseEnvKind::RW, DatabaseArguments::default())?;
        let this = Self {
            db: Arc::new(db),
            identity,
            checkpoint_interval: CHECKPOINT_INTERVAL,
            #[cfg(test)]
            abort_next_write: AtomicBool::new(false),
        };
        this.load().map(|snapshot| (this, snapshot))
    }

    pub(crate) fn load(&self) -> Result<Snapshot> {
        let identity = self.identity;
        let tx = self.db.tx()?;
        let meta = tx
            .get::<Meta>(MetaKey::Metadata)?
            .ok_or_else(|| invalid("metadata row is missing"))?;
        let MetaValue::Metadata(meta) = meta else {
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
        let bootstrap_id = checkpoints
            .first()?
            .map(|(id, _)| id)
            .ok_or_else(|| invalid("bootstrap checkpoint is missing"))?;
        let mut state = active.state.clone();
        validate_state(&state, identity)?;
        let mut tip = active.cut.zone;
        let mut imported = active.cut.tempo;
        if meta.active_checkpoint != bootstrap_id {
            let predecessor = if meta.active_checkpoint.height == bootstrap_id.height + 1 {
                tx.get::<Checkpoints>(bootstrap_id)?.map(|cp| cp.cut)
            } else {
                tx.get::<Journal>(meta.active_checkpoint.height - 1)?
                    .map(|entry| ChainCut {
                        zone: entry.zone,
                        tempo: entry.imported_tempo,
                    })
            };
            let canonical = predecessor.is_some_and(|parent| {
                tx.get::<Journal>(meta.active_checkpoint.height)
                    .ok()
                    .flatten()
                    .is_some_and(|entry| {
                        entry.zone == active.cut.zone
                            && entry.imported_tempo == active.cut.tempo
                            && entry.parent == parent.zone
                            && entry.imported_tempo_parent == parent.tempo
                    })
            });
            if !canonical {
                return Err(invalid("active checkpoint is not on the canonical journal"));
            }
        }
        for height in tip.number.saturating_add(1)..=meta.verified_zone_tip.number {
            let entry = tx
                .get::<Journal>(height)?
                .ok_or_else(|| invalid(format!("missing journal height {height}")))?;
            if entry.zone.number != height
                || entry.parent != tip
                || entry.delta.validate().is_err()
                || entry.imported_tempo.number <= imported.number
                || entry.imported_tempo_parent != imported
            {
                return Err(invalid(format!("conflicting journal height {height}")));
            }
            state
                .apply(&entry.delta)
                .map_err(|e| invalid(e.to_string()))?;
            tip = entry.zone;
            imported = entry.imported_tempo;
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
        tx.commit()?;
        Ok(Snapshot { meta, state })
    }

    pub(crate) fn apply(
        &self,
        prior: &Snapshot,
        entry: JournalEntry,
        acknowledged: BlockNumHash,
        coverage: Coverage,
    ) -> Result<Snapshot> {
        let mut candidate = prior.state.clone();
        candidate
            .apply(&entry.delta)
            .map_err(|error| invalid(error.to_string()))?;
        validate_state(&candidate, self.identity)?;
        codec::encode(&entry)?;
        let candidate_state = candidate.clone();
        self.write(prior, candidate_state, |tx, meta| {
            if entry.zone.number
                != meta
                    .verified_zone_tip
                    .number
                    .checked_add(1)
                    .ok_or_else(|| invalid("height overflow"))?
                || entry.parent != meta.verified_zone_tip
                || entry.imported_tempo.number <= meta.imported_tempo_tip.number
                || entry.imported_tempo_parent != meta.imported_tempo_tip
            {
                return Err(invalid("journal parent or height mismatch"));
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
            if entry
                .zone
                .number
                .saturating_sub(meta.active_checkpoint.height)
                >= self.checkpoint_interval
            {
                checkpoint_in_tx(tx, meta, &candidate)?;
            }
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn checkpoint_current(&self, prior: &Snapshot) -> Result<Snapshot> {
        validate_state(&prior.state, self.identity)?;
        self.write(prior, prior.state.clone(), |tx, meta| {
            checkpoint_in_tx(tx, meta, &prior.state)?;
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn checkpoint(
        &self,
        identity: Identity,
        cut: ChainCut,
        state: State,
    ) -> Result<Snapshot> {
        if identity != self.identity {
            return Err(PersistenceError::Identity);
        }
        let prior = self.load()?;
        if cut.zone != prior.meta.verified_zone_tip
            || cut.tempo != prior.meta.imported_tempo_tip
            || state != prior.state
        {
            return Err(invalid("checkpoint is not current"));
        }
        self.checkpoint_current(&prior)
    }

    pub(crate) fn record_finding(
        &self,
        prior: &Snapshot,
        key: FindingKey,
        finding: Finding,
    ) -> Result<Snapshot> {
        self.write(prior, prior.state.clone(), |tx, meta| {
            validate_finding(key, &finding, Some(meta))?;
            codec::encode(&finding)?;
            if let Some(old) = tx.get::<Findings>(key)? {
                let same_evidence = old.zone == finding.zone
                    && old.parent == finding.parent
                    && old.imported_tempo == finding.imported_tempo
                    && old.imported_tempo_parent == finding.imported_tempo_parent
                    && old.details == finding.details
                    && old.evidence_len == finding.evidence_len
                    && old.evidence_digest == finding.evidence_digest;
                if !same_evidence {
                    return Err(invalid("conflicting same-height finding evidence"));
                }
                if old.summary != finding.summary {
                    tx.put::<Findings>(key, finding)?;
                }
            } else {
                tx.put::<Findings>(key, finding)?;
            }
            meta.active_finding = Some(key);
            Ok(())
        })
    }

    pub(crate) fn record_gap(
        &self,
        prior: &Snapshot,
        first_unchecked: BlockNumHash,
        acknowledged_through: BlockNumHash,
        reason: CoverageGapReason,
    ) -> Result<Snapshot> {
        self.write(prior, prior.state.clone(), |_tx, meta| {
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

    pub(crate) fn reorg(&self, prior: &Snapshot, ancestor: BlockNumHash) -> Result<Snapshot> {
        if ancestor.number > prior.meta.verified_zone_tip.number {
            if ancestor.number > prior.meta.acknowledged_zone_tip.number {
                return Err(invalid("reorg ancestor exceeds acknowledged history"));
            }
            return self.write(prior, prior.state.clone(), |_tx, meta| {
                update_active_finding(meta, ancestor)?;
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
        let snapshot = self.reconstruct_at(ancestor)?;
        self.write(prior, snapshot.state.clone(), |tx, meta| {
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
                    first_unchecked,
                    reason,
                    ..
                } if first_unchecked.number <= ancestor.number => Coverage::Gap {
                    first_unchecked: *first_unchecked,
                    acknowledged_through: ancestor,
                    reason: reason.clone(),
                },
                _ => Coverage::Complete,
            };
            update_active_finding(meta, ancestor)?;
            Ok(())
        })
    }

    fn reconstruct_at(&self, ancestor: BlockNumHash) -> Result<Snapshot> {
        let identity = self.identity;
        let tx = self.db.tx()?;
        let meta = tx
            .get::<Meta>(MetaKey::Metadata)?
            .ok_or_else(|| invalid("metadata row is missing"))?;
        let MetaValue::Metadata(meta) = meta else {
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
                || e.imported_tempo.number <= imported.number
            {
                return Err(invalid("non-contiguous reorg journal"));
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

    fn write<F>(&self, prior: &Snapshot, state: State, f: F) -> Result<Snapshot>
    where
        F: FnOnce(&<DatabaseEnv as Database>::TXMut, &mut Metadata) -> Result<()>,
    {
        let tx = self.db.tx_mut()?;
        let meta = tx
            .get::<Meta>(MetaKey::Metadata)?
            .ok_or_else(|| invalid("metadata row is missing"))?;
        let MetaValue::Metadata(meta) = meta else {
            return Err(invalid("metadata type mismatch"));
        };
        let mut meta = *meta;
        if meta.identity != self.identity {
            tx.abort();
            return Err(PersistenceError::Identity);
        }
        if meta != prior.meta {
            tx.abort();
            return Err(PersistenceError::StaleSnapshot);
        }
        if let Err(e) = f(&tx, &mut meta) {
            tx.abort();
            return Err(e);
        }
        tx.put::<Meta>(
            MetaKey::Metadata,
            MetaValue::Metadata(Box::new(meta.clone())),
        )?;
        #[cfg(test)]
        if self.abort_next_write.swap(false, Ordering::SeqCst) {
            tx.abort();
            return Err(PersistenceError::InjectedAbort);
        }
        tx.commit()?;
        Ok(Snapshot { meta, state })
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
        .ok_or_else(|| invalid("schema version is missing"))?;
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

fn update_active_finding(meta: &mut Metadata, ancestor: BlockNumHash) -> Result<()> {
    if let Some(key) = meta.active_finding {
        if key.zone.number == ancestor.number && key.zone.hash != ancestor.hash {
            return Err(invalid("conflicting evidence at finding height"));
        }
        if key.zone.number > ancestor.number {
            meta.active_finding = None;
        }
    }
    Ok(())
}

fn validate_state(state: &State, identity: Identity) -> Result<()> {
    state
        .validate_families()
        .map_err(|e| invalid(e.to_string()))?;
    validate(state).map_err(|e| invalid(format!("invariant {e:?}")))?;
    let Some(portal) = state.portal() else {
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

fn checkpoint_in_tx(
    tx: &<DatabaseEnv as Database>::TXMut,
    meta: &mut Metadata,
    state: &State,
) -> Result<()> {
    let cut = ChainCut {
        zone: meta.verified_zone_tip,
        tempo: meta.imported_tempo_tip,
    };
    let id = CheckpointId::from(cut.zone);
    let checkpoint = Checkpoint {
        cut,
        state: state.clone(),
    };
    codec::encode(&checkpoint)?;
    if let Some(existing) = tx.get::<Checkpoints>(id)? {
        if existing != checkpoint {
            return Err(invalid("checkpoint identity is immutable"));
        }
    } else {
        tx.put::<Checkpoints>(id, checkpoint)?;
    }
    meta.active_checkpoint = id;
    Ok(())
}

fn canonical_datum(value: Option<&Datum>) -> Vec<u8> {
    value.map(Datum::canonical_bytes).unwrap_or_default()
}

fn finding_evidence(
    details: &FindingDetails,
) -> Result<(usize, usize, u32, alloy_primitives::B256)> {
    let expected = canonical_datum(details.expected.as_ref());
    let actual = canonical_datum(details.actual.as_ref());
    let mut canonical = Vec::with_capacity(8 + expected.len() + actual.len());
    canonical.extend(
        u32::try_from(expected.len())
            .map_err(|_| invalid("expected too large"))?
            .to_be_bytes(),
    );
    canonical.extend_from_slice(&expected);
    canonical.extend(
        u32::try_from(actual.len())
            .map_err(|_| invalid("actual too large"))?
            .to_be_bytes(),
    );
    canonical.extend_from_slice(&actual);
    Ok((
        expected.len(),
        actual.len(),
        u32::try_from(canonical.len()).map_err(|_| invalid("evidence too large"))?,
        alloy_primitives::keccak256(canonical),
    ))
}

fn finding_operation(location: Option<&FindingLocation>) -> u32 {
    match location {
        Some(FindingLocation::Operation(operation))
        | Some(FindingLocation::ImportedOperation(operation)) => *operation,
        Some(FindingLocation::State(_) | FindingLocation::Block) | None => 0,
    }
}

pub(crate) fn make_finding(
    zone: BlockNumHash,
    parent: BlockNumHash,
    imported: Option<(BlockNumHash, BlockNumHash)>,
    details: FindingDetails,
    summary: String,
) -> Result<(FindingKey, Finding)> {
    let operation = finding_operation(details.location.as_ref());
    let key = FindingKey {
        zone,
        operation,
        code: details.code,
    };
    let (_, _, evidence_len, evidence_digest) = finding_evidence(&details)?;
    let (imported_tempo, imported_tempo_parent) = imported.unzip();
    let finding = Finding {
        zone,
        parent,
        imported_tempo,
        imported_tempo_parent,
        details,
        evidence_len,
        evidence_digest,
        summary,
    };
    validate_finding(key, &finding, None)?;
    Ok((key, finding))
}

fn valid_imported_finding_coordinate(
    finding: &Finding,
    previous_imported_tempo_tip: BlockNumHash,
) -> bool {
    match (finding.imported_tempo, finding.imported_tempo_parent) {
        (None, None) => true,
        (Some(imported), Some(imported_parent)) => {
            imported.number > previous_imported_tempo_tip.number
                && imported_parent == previous_imported_tempo_tip
        }
        _ => false,
    }
}

fn validate_finding(key: FindingKey, finding: &Finding, meta: Option<&Metadata>) -> Result<()> {
    let (expected_len, actual_len, evidence_len, evidence_digest) =
        finding_evidence(&finding.details)?;
    if finding.zone != key.zone
        || finding_operation(finding.details.location.as_ref()) != key.operation
        || finding.details.code != key.code
        || expected_len > 256
        || actual_len > 256
        || finding.summary.len() > 1_024
        || finding.evidence_len != evidence_len
        || finding.evidence_digest != evidence_digest
    {
        return Err(invalid("finding is inconsistent or exceeds codec bounds"));
    }
    if let Some(meta) = meta {
        let next = meta
            .verified_zone_tip
            .number
            .checked_add(1)
            .ok_or_else(|| invalid("height overflow"))?;
        if finding.zone.number != next
            || finding.parent != meta.verified_zone_tip
            || !valid_imported_finding_coordinate(finding, meta.imported_tempo_tip)
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
