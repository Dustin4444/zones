//! Checker MDBX ownership, initialization, and current-state reads.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_eips::BlockNumHash;
use alloy_primitives::{B256, U256};
use reth_db::{
    Database, DatabaseEnv,
    cursor::{DbCursorRO, DbCursorRW},
    transaction::{DbTx, DbTxMut},
};

use crate::model::state::{ModelState, PortalIdentity, TokenPhase};

use super::{
    codec::validate_canonical,
    error::{StoreError, StoreResult},
    model_state::{ModelRows, assemble_model, flatten_model},
    schema::{
        CanonicalHash, CheckerCanonical, CheckerChangesets, CheckerFindings, CheckerMeta,
        CheckerModelState, MetaKey, ModelKey,
    },
    value::{
        ActiveAlert, BootstrapState, MetaValue, ModelValue, PortalSettlementValue, StoreIdentity,
    },
};

#[cfg(test)]
use {
    super::{schema::FindingKey, value::FindingRecord},
    alloy_primitives::Address,
};

mod open;

#[derive(Debug, Clone)]
pub(crate) struct Initialization {
    pub(crate) identity: StoreIdentity,
    pub(crate) bootstrap: BootstrapState,
    pub(crate) verified_zone_tip: BlockNumHash,
    pub(crate) imported_tempo_tip: BlockNumHash,
    pub(crate) model: ModelState,
}

/// Authenticated starting cut for one fresh checker database.
///
/// The type deliberately excludes `Live`: a fresh database must either replay
/// Portal history from the parent of the configured creation block or start at
/// Zone genesis while the development flow is still awaiting Portal creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FreshBootstrap {
    L1Replay { creation_parent: BlockNumHash },
    ZoneReplay { genesis_anchor: BlockNumHash },
}

impl Initialization {
    pub(crate) fn fresh(identity: StoreIdentity, start: FreshBootstrap) -> Self {
        let (bootstrap, imported_tempo_tip) = match start {
            FreshBootstrap::L1Replay { creation_parent } => {
                (BootstrapState::l1_replay(None), creation_parent)
            }
            FreshBootstrap::ZoneReplay { genesis_anchor } => {
                (BootstrapState::zone_replay(genesis_anchor), genesis_anchor)
            }
        };
        Self {
            identity,
            bootstrap,
            verified_zone_tip: BlockNumHash::new(0, identity.zone_genesis_hash()),
            imported_tempo_tip,
            model: ModelState::awaiting_creation(identity.portal_identity()),
        }
    }

    #[cfg(test)]
    pub(crate) const fn new(
        identity: StoreIdentity,
        bootstrap: BootstrapState,
        verified_zone_tip: BlockNumHash,
        imported_tempo_tip: BlockNumHash,
        model: ModelState,
    ) -> Self {
        Self {
            identity,
            bootstrap,
            verified_zone_tip,
            imported_tempo_tip,
            model,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoreSnapshot {
    pub(crate) verified_zone_tip: BlockNumHash,
    pub(crate) imported_tempo_tip: BlockNumHash,
    pub(crate) bootstrap: BootstrapState,
    pub(crate) active_alert: Option<ActiveAlert>,
    pub(crate) model: ModelState,
    pub(crate) model_rows: ModelRows,
}

/// Durable preflight classification of a canonical Zone block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalBlock {
    /// The exact next child, paired with the authoritative parent tips.
    Next {
        verified_zone_tip: BlockNumHash,
        imported_tempo_tip: BlockNumHash,
    },
    /// A retained canonical block that requires no model acquisition or write.
    AlreadyCanonical {
        /// The current durable tip to acknowledge, which may be newer than the child.
        verified_zone_tip: BlockNumHash,
    },
}

/// Fixed-size durable state needed to guard a sparse commit.
///
/// Opening and explicit snapshot reads validate the complete model. Once open,
/// typed model updates only need this head plus their touched rows; walking the
/// unbounded open-owner tables for every block would make commit cost depend on
/// historical backlog rather than the block's logical delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StoreProgress {
    pub(crate) verified_zone_tip: BlockNumHash,
    pub(crate) imported_tempo_tip: BlockNumHash,
    pub(crate) bootstrap: BootstrapState,
    pub(crate) active_alert: Option<ActiveAlert>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct RefundCredit {
    pub(crate) origin: u64,
    pub(crate) amount: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum RefundLedger {
    Portal,
    Inbox,
}

#[derive(Debug)]
pub(crate) struct CheckerStore {
    pub(super) db: Arc<DatabaseEnv>,
    pub(super) identity: StoreIdentity,
    path: PathBuf,
}

impl CheckerStore {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) const fn portal_creation_block(&self) -> BlockNumHash {
        self.identity.portal_creation_block()
    }

    pub(crate) const fn l1_chain_id(&self) -> u64 {
        self.identity.l1_chain_id()
    }

    pub(crate) fn load_current(&self) -> StoreResult<StoreSnapshot> {
        let tx = self.db.tx()?;
        let result = read_snapshot(&tx, self.identity, &self.path);
        finish_read(tx, result)
    }

    /// Read and validate the fixed-size durable progress cut without walking
    /// the unbounded model table.
    pub(crate) fn load_progress(&self) -> StoreResult<StoreProgress> {
        let tx = self.db.tx()?;
        let result = read_head(&tx, self.identity, &self.path);
        finish_read(tx, result)
    }

    /// Classify a canonical Zone candidate against one authoritative read transaction.
    ///
    /// Retained canonical blocks remain acknowledgeable while L1 replay or an
    /// alert prevents new work. Zone replay and live mode share the exact-next
    /// ordinary commit path.
    pub(crate) fn preflight_block(
        &self,
        child: BlockNumHash,
        parent_hash: B256,
    ) -> StoreResult<CanonicalBlock> {
        let tx = self.db.tx()?;
        let result = preflight_block(&tx, self.identity, &self.path, child, parent_hash);
        finish_read(tx, result)
    }

    #[cfg(test)]
    pub(crate) fn active_alert(&self) -> StoreResult<Option<ActiveAlert>> {
        let tx = self.db.tx()?;
        let result = read_active_alert(&tx);
        finish_read(tx, result)
    }

    #[cfg(test)]
    pub(crate) fn finding(&self, key: FindingKey) -> StoreResult<Option<FindingRecord>> {
        let tx = self.db.tx()?;
        let result = tx.get::<CheckerFindings>(key).map_err(StoreError::from);
        finish_read(tx, result)
    }

    #[cfg(test)]
    pub(crate) fn portal_refund_credits(
        &self,
        token: Address,
        recipient: Address,
    ) -> StoreResult<Vec<RefundCredit>> {
        self.refund_credits(token, recipient, RefundLedger::Portal)
    }

    #[cfg(test)]
    pub(crate) fn inbox_refund_credits(
        &self,
        token: Address,
        recipient: Address,
    ) -> StoreResult<Vec<RefundCredit>> {
        self.refund_credits(token, recipient, RefundLedger::Inbox)
    }

    #[cfg(test)]
    fn refund_credits(
        &self,
        token: Address,
        recipient: Address,
        ledger: RefundLedger,
    ) -> StoreResult<Vec<RefundCredit>> {
        let tx = self.db.tx()?;
        let result = read_refund_credits(&tx, token, recipient, ledger);
        finish_read(tx, result)
    }

    #[cfg(test)]
    pub(super) fn database(&self) -> &DatabaseEnv {
        &self.db
    }
}

fn preflight_block<TX: DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    path: &Path,
    child: BlockNumHash,
    parent_hash: B256,
) -> StoreResult<CanonicalBlock> {
    let head = read_head(tx, identity, path)?;
    if child.number <= head.verified_zone_tip.number {
        let canonical = tx
            .get::<CheckerCanonical>(child.number)?
            .ok_or(StoreError::MissingCanonical {
                height: child.number,
            })?
            .into_inner();
        if canonical != child.hash {
            return Err(StoreError::CanonicalConflict {
                height: child.number,
                expected: child.hash,
                actual: canonical,
            });
        }
        return Ok(CanonicalBlock::AlreadyCanonical {
            verified_zone_tip: head.verified_zone_tip,
        });
    }
    if matches!(head.bootstrap, BootstrapState::L1Replay { .. }) {
        return Err(StoreError::InvalidBootstrapProgress(
            "ordinary block preflight is disabled during L1 replay",
        ));
    }
    if let Some(alert) = head.active_alert {
        return Err(StoreError::ActiveAlert(alert.finding));
    }
    if head.verified_zone_tip.number.checked_add(1) != Some(child.number) {
        return Err(StoreError::NonAdjacent {
            chain: "Zone",
            parent: head.verified_zone_tip,
            child,
        });
    }
    if parent_hash != head.verified_zone_tip.hash {
        return Err(StoreError::CandidateParentConflict {
            child,
            expected: head.verified_zone_tip.hash,
            actual: parent_hash,
        });
    }

    Ok(CanonicalBlock::Next {
        verified_zone_tip: head.verified_zone_tip,
        imported_tempo_tip: head.imported_tempo_tip,
    })
}

pub(super) fn finish_read<T, TX: DbTx>(tx: TX, result: StoreResult<T>) -> StoreResult<T> {
    match result {
        Ok(value) => {
            tx.commit()?;
            Ok(value)
        }
        Err(error) => {
            tx.abort();
            Err(error)
        }
    }
}

pub(super) fn required_meta<TX: DbTx>(tx: &TX, key: MetaKey) -> StoreResult<MetaValue> {
    let value = tx
        .get::<CheckerMeta>(key)?
        .ok_or(StoreError::MissingMetadata(key))?;
    if value.matches_key(key) {
        Ok(value)
    } else {
        Err(StoreError::MetadataType { key, value })
    }
}

pub(super) fn read_tip<TX: DbTx>(tx: &TX, key: MetaKey) -> StoreResult<BlockNumHash> {
    let value = required_meta(tx, key)?;
    match value {
        MetaValue::VerifiedZoneTip(tip) | MetaValue::ImportedTempoTip(tip) => Ok(tip),
        value => Err(StoreError::MetadataType { key, value }),
    }
}

pub(super) fn read_bootstrap<TX: DbTx>(tx: &TX) -> StoreResult<BootstrapState> {
    let value = required_meta(tx, MetaKey::Bootstrap)?;
    match value {
        MetaValue::Bootstrap(state) => Ok(state),
        value => Err(StoreError::MetadataType {
            key: MetaKey::Bootstrap,
            value,
        }),
    }
}

pub(super) fn read_active_alert<TX: DbTx>(tx: &TX) -> StoreResult<Option<ActiveAlert>> {
    let Some(value) = tx.get::<CheckerMeta>(MetaKey::ActiveAlert)? else {
        return Ok(None);
    };
    match value {
        MetaValue::ActiveAlert(alert) => Ok(Some(alert)),
        value => Err(StoreError::MetadataType {
            key: MetaKey::ActiveAlert,
            value,
        }),
    }
}

pub(super) fn read_model_rows<TX: DbTx>(tx: &TX) -> StoreResult<ModelRows> {
    let mut rows = BTreeMap::new();
    let mut cursor = tx.cursor_read::<CheckerModelState>()?;
    for row in cursor.walk(None)? {
        let (key, value) = row?;
        rows.insert(key, value);
    }
    Ok(rows)
}

pub(super) fn read_snapshot<TX: DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    path: &Path,
) -> StoreResult<StoreSnapshot> {
    let head = read_head(tx, identity, path)?;
    let model_rows = read_model_rows(tx)?;
    let model = assemble_model(identity.portal_identity(), model_rows.clone())?;
    validate_model_cut_coherence(
        tx,
        identity,
        Some(head.bootstrap),
        head.verified_zone_tip,
        head.imported_tempo_tip,
        &model,
    )?;

    Ok(StoreSnapshot {
        verified_zone_tip: head.verified_zone_tip,
        imported_tempo_tip: head.imported_tempo_tip,
        bootstrap: head.bootstrap,
        active_alert: head.active_alert,
        model,
        model_rows,
    })
}

/// Validate the already-decoded durable cut as if it were in `bootstrap`.
///
/// Bootstrap phase changes are rare, so they deliberately validate the whole
/// model both while preparing and inside the write transaction. Ordinary block
/// commits retain their sparse, touched-row validation path.
pub(super) fn validate_bootstrap_candidate<TX: DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    bootstrap: BootstrapState,
    snapshot: &StoreSnapshot,
) -> StoreResult<()> {
    validate_model_cut_coherence(
        tx,
        identity,
        Some(bootstrap),
        snapshot.verified_zone_tip,
        snapshot.imported_tempo_tip,
        &snapshot.model,
    )
}

/// Validate the relationships between one complete logical model cut and its
/// durable tips/canonical index.
///
/// Historical snapshots do not retain their bootstrap phase, so callers pass
/// `None` there. Universal Tempo progress and canonical-anchor checks still
/// apply; only the phase-specific Live bounds are skipped.
pub(super) fn validate_model_cut_coherence<TX: DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    bootstrap: Option<BootstrapState>,
    verified_zone_tip: BlockNumHash,
    imported_tempo_tip: BlockNumHash,
    model: &ModelState,
) -> StoreResult<()> {
    validate_portal_creation_progress(identity, imported_tempo_tip, model)?;
    if let Some(bootstrap) = bootstrap {
        validate_bootstrap_token_phases(bootstrap, model)?;
    }
    validate_settlement_position(
        tx,
        bootstrap,
        verified_zone_tip,
        imported_tempo_tip,
        SettlementPosition::from_model(model),
    )
}

/// Validate the only model row whose meaning is coupled directly to the
/// durable tips. An absent entry means the sparse update leaves settlement
/// unchanged; a present `None` removes the Portal and has no settlement
/// position to validate.
pub(super) fn validate_portal_settlement_change<TX: DbTx>(
    tx: &TX,
    bootstrap: BootstrapState,
    verified_zone_tip: BlockNumHash,
    imported_tempo_tip: BlockNumHash,
    change: Option<&Option<ModelValue>>,
) -> StoreResult<()> {
    let Some(Some(value)) = change else {
        return Ok(());
    };
    let ModelValue::PortalSettlement(settlement) = value else {
        return Err(StoreError::ModelKeyValueMismatch {
            key: ModelKey::PortalSettlement,
            value: Box::new(value.clone()),
        });
    };
    validate_settlement_position(
        tx,
        Some(bootstrap),
        verified_zone_tip,
        imported_tempo_tip,
        Some(SettlementPosition::from(*settlement)),
    )
}

pub(super) fn read_head<TX: DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    path: &Path,
) -> StoreResult<StoreProgress> {
    validate_identity(tx, identity, path)?;
    let bootstrap = read_bootstrap(tx)?;
    let verified_zone_tip = read_tip(tx, MetaKey::VerifiedZoneTip)?;
    let imported_tempo_tip = read_tip(tx, MetaKey::ImportedTempoTip)?;
    validate_bootstrap_coherence(bootstrap, imported_tempo_tip)?;
    validate_l1_replay_progress(identity, bootstrap, verified_zone_tip, imported_tempo_tip)?;
    let active_alert = read_active_alert(tx)?;

    let canonical = tx
        .get::<CheckerCanonical>(verified_zone_tip.number)?
        .ok_or(StoreError::MissingCanonical {
            height: verified_zone_tip.number,
        })?;
    let canonical = canonical.into_inner();
    if canonical != verified_zone_tip.hash {
        return Err(StoreError::CanonicalConflict {
            height: verified_zone_tip.number,
            expected: verified_zone_tip.hash,
            actual: canonical,
        });
    }
    if let Some(alert) = active_alert
        && tx.get::<CheckerFindings>(alert.finding)?.is_none()
    {
        return Err(StoreError::MissingActiveFinding(alert.finding));
    }

    Ok(StoreProgress {
        verified_zone_tip,
        imported_tempo_tip,
        bootstrap,
        active_alert,
    })
}

fn all_tables_empty<TX: DbTx>(tx: &TX) -> StoreResult<bool> {
    let entries = [
        tx.entries::<CheckerMeta>()?,
        tx.entries::<CheckerCanonical>()?,
        tx.entries::<CheckerModelState>()?,
        tx.entries::<CheckerChangesets>()?,
        tx.entries::<CheckerFindings>()?,
    ];
    Ok(entries.into_iter().all(|count| count == 0))
}

fn prepare_initial_model(initialization: &Initialization) -> StoreResult<ModelRows> {
    if initialization.verified_zone_tip.number != 0 {
        return Err(StoreError::InvalidInitialization(
            "verified Zone tip must begin at genesis height zero",
        ));
    }
    if initialization.verified_zone_tip.hash != initialization.identity.zone_genesis_hash() {
        return Err(StoreError::InvalidInitialization(
            "verified Zone tip hash must equal configured genesis hash",
        ));
    }
    validate_bootstrap_coherence(initialization.bootstrap, initialization.imported_tempo_tip)
        .map_err(|_| {
            StoreError::InvalidInitialization("bootstrap state and imported tip disagree")
        })?;
    validate_l1_replay_progress(
        initialization.identity,
        initialization.bootstrap,
        initialization.verified_zone_tip,
        initialization.imported_tempo_tip,
    )?;
    if initialization.model.portal().identity() != initialization.identity.portal_identity() {
        return Err(StoreError::InvalidInitialization(
            "model Portal identity differs from database identity",
        ));
    }
    validate_portal_creation_progress(
        initialization.identity,
        initialization.imported_tempo_tip,
        &initialization.model,
    )?;
    validate_bootstrap_token_phases(initialization.bootstrap, &initialization.model)?;
    validate_settlement_bounds(
        Some(initialization.bootstrap),
        initialization.verified_zone_tip,
        initialization.imported_tempo_tip,
        SettlementPosition::from_model(&initialization.model),
    )?;

    let rows = flatten_model(&initialization.model)?;
    assemble_model(initialization.identity.portal_identity(), rows.clone())?;
    Ok(rows)
}

fn initialize_empty<TX: DbTxMut + DbTx>(
    tx: &TX,
    initialization: &Initialization,
    rows: ModelRows,
) -> StoreResult<()> {
    let mut metadata = initialization.identity.metadata().to_vec();
    metadata.extend([
        (
            MetaKey::Bootstrap,
            MetaValue::Bootstrap(initialization.bootstrap),
        ),
        (
            MetaKey::VerifiedZoneTip,
            MetaValue::VerifiedZoneTip(initialization.verified_zone_tip),
        ),
        (
            MetaKey::ImportedTempoTip,
            MetaValue::ImportedTempoTip(initialization.imported_tempo_tip),
        ),
    ]);
    for (_, value) in &metadata {
        validate_canonical(value)
            .map_err(|_| StoreError::InvalidPersistedValue("metadata value"))?;
    }
    for (key, value) in &rows {
        if !value.matches_key(*key) {
            return Err(StoreError::ModelKeyValueMismatch {
                key: *key,
                value: Box::new(value.clone()),
            });
        }
        validate_canonical(value).map_err(|_| StoreError::InvalidPersistedValue("model value"))?;
    }
    {
        let mut cursor = tx.cursor_write::<CheckerMeta>()?;
        for (key, value) in metadata {
            cursor.insert(key, &value)?;
        }
    }
    {
        let mut cursor = tx.cursor_write::<CheckerModelState>()?;
        for (key, value) in rows {
            cursor.insert(key, &value)?;
        }
    }
    tx.cursor_write::<CheckerCanonical>()?.insert(
        initialization.verified_zone_tip.number,
        &CanonicalHash::new(initialization.verified_zone_tip.hash),
    )?;
    Ok(())
}

fn validate_identity<TX: DbTx>(tx: &TX, identity: StoreIdentity, path: &Path) -> StoreResult<()> {
    validate_schema_version(tx, path)?;
    for (key, expected) in identity.metadata().into_iter().skip(1) {
        let actual = required_meta(tx, key)?;
        if actual != expected {
            return Err(StoreError::IdentityMismatch {
                key,
                expected: Box::new(expected),
                actual: Box::new(actual),
            });
        }
    }
    Ok(())
}

fn validate_schema_version<TX: DbTx>(tx: &TX, path: &Path) -> StoreResult<()> {
    let expected_version = u32::from(super::SCHEMA_VERSION);
    let actual_version = match required_meta(tx, MetaKey::Version)? {
        MetaValue::Version(version) => version,
        value => {
            return Err(StoreError::MetadataType {
                key: MetaKey::Version,
                value,
            });
        }
    };
    if actual_version != expected_version {
        return Err(StoreError::VersionMismatch {
            path: path.to_path_buf(),
            expected: expected_version,
            actual: actual_version,
            rebuild_path: versioned_rebuild_path(path, expected_version),
        });
    }
    Ok(())
}

fn versioned_rebuild_path(path: &Path, version: u32) -> PathBuf {
    let mut name = path
        .file_name()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| std::ffi::OsString::from("checker"));
    name.push(format!("-v{version}"));
    path.with_file_name(name)
}

fn read_stored_identity<TX: DbTx>(tx: &TX) -> StoreResult<StoreIdentity> {
    let MetaValue::ZoneIdentity {
        chain_id: zone_chain_id,
        genesis_hash: zone_genesis_hash,
        zone_id,
        initial_token,
    } = required_meta(tx, MetaKey::ZoneIdentity)?
    else {
        unreachable!("required metadata enforces its value family")
    };
    let MetaValue::L1ChainId(l1_chain_id) = required_meta(tx, MetaKey::L1ChainId)? else {
        unreachable!("required metadata enforces its value family")
    };
    let MetaValue::Contracts {
        zone_factory,
        portal,
    } = required_meta(tx, MetaKey::Contracts)?
    else {
        unreachable!("required metadata enforces its value family")
    };
    let MetaValue::PortalCreationBlock(portal_creation_block) =
        required_meta(tx, MetaKey::PortalCreationBlock)?
    else {
        unreachable!("required metadata enforces its value family")
    };
    Ok(StoreIdentity::new(
        zone_chain_id,
        zone_genesis_hash,
        PortalIdentity::new(portal, zone_id, initial_token),
        l1_chain_id,
        zone_factory,
        portal_creation_block,
    ))
}

#[cfg(test)]
fn read_refund_credits<TX: DbTx>(
    tx: &TX,
    token: Address,
    recipient: Address,
    ledger: RefundLedger,
) -> StoreResult<Vec<RefundCredit>> {
    let start = match ledger {
        RefundLedger::Portal => ModelKey::PortalRefundCredit {
            token,
            recipient,
            origin: 0,
        },
        RefundLedger::Inbox => ModelKey::InboxRefundCredit {
            token,
            recipient,
            origin: 0,
        },
    };
    let mut credits = Vec::new();
    let mut cursor = tx.cursor_read::<CheckerModelState>()?;
    for row in cursor.walk(Some(start))? {
        let (key, value) = row?;
        let origin = match (ledger, key) {
            (
                RefundLedger::Portal,
                ModelKey::PortalRefundCredit {
                    token: row_token,
                    recipient: row_recipient,
                    origin,
                },
            ) if row_token == token && row_recipient == recipient => origin,
            (
                RefundLedger::Inbox,
                ModelKey::InboxRefundCredit {
                    token: row_token,
                    recipient: row_recipient,
                    origin,
                },
            ) if row_token == token && row_recipient == recipient => origin,
            _ => break,
        };
        let amount = match (ledger, value) {
            (RefundLedger::Portal, ModelValue::PortalRefundCredit(amount))
            | (RefundLedger::Inbox, ModelValue::InboxRefundCredit(amount)) => amount,
            (_, value) => {
                return Err(StoreError::ModelKeyValueMismatch {
                    key,
                    value: Box::new(value),
                });
            }
        };
        credits.push(RefundCredit { origin, amount });
    }
    Ok(credits)
}

pub(super) fn validate_bootstrap_coherence(
    state: BootstrapState,
    imported_tempo_tip: BlockNumHash,
) -> StoreResult<()> {
    let coherent = match state {
        BootstrapState::L1Replay { cursor } => {
            cursor.is_none_or(|cursor| cursor == imported_tempo_tip)
        }
        BootstrapState::ZoneReplay { cursor } => cursor == imported_tempo_tip,
        BootstrapState::Live => true,
    };
    if coherent {
        Ok(())
    } else {
        Err(StoreError::InvalidBootstrapProgress(
            "bootstrap cursor and imported Tempo tip disagree",
        ))
    }
}

fn validate_l1_replay_progress(
    identity: StoreIdentity,
    bootstrap: BootstrapState,
    verified_zone_tip: BlockNumHash,
    imported_tempo_tip: BlockNumHash,
) -> StoreResult<()> {
    let BootstrapState::L1Replay { cursor } = bootstrap else {
        return Ok(());
    };

    let expected = BlockNumHash::new(0, identity.zone_genesis_hash());
    if verified_zone_tip != expected {
        return Err(StoreError::L1ReplayZoneTipMismatch {
            expected,
            actual: verified_zone_tip,
        });
    }

    let creation = identity.portal_creation_block();
    if cursor.is_none() && creation.number.checked_sub(1) != Some(imported_tempo_tip.number) {
        return Err(StoreError::L1ReplayStartHeightMismatch {
            creation,
            actual: imported_tempo_tip,
        });
    }
    if let Some(cursor) = cursor
        && (cursor.number < creation.number
            || (cursor.number == creation.number && cursor.hash != creation.hash))
    {
        return Err(StoreError::L1ReplayCursorOutsideCreationHistory { creation, cursor });
    }
    Ok(())
}

fn validate_portal_creation_progress(
    identity: StoreIdentity,
    imported_tip: BlockNumHash,
    model: &ModelState,
) -> StoreResult<()> {
    let creation = identity.portal_creation_block();
    let portal_created = model.portal().created().is_some();
    if imported_tip.number == creation.number && imported_tip != creation {
        return Err(StoreError::PortalCreationProgressMismatch {
            creation,
            imported_tip,
            portal_created,
        });
    }
    let expected_created = imported_tip.number >= creation.number;
    if portal_created != expected_created {
        return Err(StoreError::PortalCreationProgressMismatch {
            creation,
            imported_tip,
            portal_created,
        });
    }
    Ok(())
}

fn validate_bootstrap_token_phases(
    bootstrap: BootstrapState,
    model: &ModelState,
) -> StoreResult<()> {
    for (token, state) in model.tokens() {
        let valid = match bootstrap {
            BootstrapState::L1Replay { .. } => state.phase() == TokenPhase::PendingZoneEnable,
            BootstrapState::ZoneReplay { .. } | BootstrapState::Live => {
                state.phase() == TokenPhase::ZoneEnabled
            }
        };
        if !valid {
            return Err(StoreError::BootstrapTokenPhaseMismatch {
                bootstrap,
                token: *token,
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct SettlementPosition {
    tempo_height: u64,
    zone_height: U256,
    zone_hash: B256,
}

impl SettlementPosition {
    fn from_model(model: &ModelState) -> Option<Self> {
        model.portal().created().map(|portal| {
            let settlement = portal.settlement();
            Self {
                tempo_height: settlement.last_synced_tempo_block_number(),
                zone_height: settlement.zone_height(),
                zone_hash: settlement.block_hash(),
            }
        })
    }
}

impl From<PortalSettlementValue> for SettlementPosition {
    fn from(value: PortalSettlementValue) -> Self {
        Self {
            tempo_height: value.last_synced_tempo_block_number,
            zone_height: value.zone_height,
            zone_hash: value.block_hash,
        }
    }
}

fn validate_settlement_position<TX: DbTx>(
    tx: &TX,
    bootstrap: Option<BootstrapState>,
    verified_zone_tip: BlockNumHash,
    imported_tempo_tip: BlockNumHash,
    settlement: Option<SettlementPosition>,
) -> StoreResult<()> {
    let Some(settlement) = settlement else {
        return Ok(());
    };

    validate_settlement_bounds(
        bootstrap,
        verified_zone_tip,
        imported_tempo_tip,
        Some(settlement),
    )?;

    let height = u64::try_from(settlement.zone_height)
        .map_err(|_| StoreError::InvalidPersistedValue("Portal settlement Zone height"))?;

    // Height zero is the pre-submission sentinel, not a claim about Zone genesis.
    if height == 0 || height > verified_zone_tip.number {
        return Ok(());
    }

    // A sparse child commit validates before inserting its canonical row. The
    // supplied tip hash is already guarded against the parent and is therefore
    // the authoritative anchor at exactly the prospective child height.
    let canonical_hash = if height == verified_zone_tip.number {
        verified_zone_tip.hash
    } else {
        tx.get::<CheckerCanonical>(height)?
            .ok_or(StoreError::MissingCanonical { height })?
            .into_inner()
    };
    if settlement.zone_hash != canonical_hash {
        return Err(StoreError::PortalSettlementCanonicalConflict {
            height,
            settlement_hash: settlement.zone_hash,
            canonical_hash,
        });
    }
    Ok(())
}

fn validate_settlement_bounds(
    bootstrap: Option<BootstrapState>,
    verified_zone_tip: BlockNumHash,
    imported_tempo_tip: BlockNumHash,
    settlement: Option<SettlementPosition>,
) -> StoreResult<()> {
    let Some(settlement) = settlement else {
        return Ok(());
    };

    if settlement.tempo_height > imported_tempo_tip.number {
        return Err(StoreError::PortalSettlementBeyondImportedTempoTip {
            settlement_height: settlement.tempo_height,
            imported_tip: imported_tempo_tip,
        });
    }

    let height = u64::try_from(settlement.zone_height)
        .map_err(|_| StoreError::InvalidPersistedValue("Portal settlement Zone height"))?;
    if bootstrap == Some(BootstrapState::Live) && height > verified_zone_tip.number {
        return Err(StoreError::LivePortalSettlementBeyondVerifiedZoneTip {
            settlement_height: height,
            verified_tip: verified_zone_tip,
        });
    }
    Ok(())
}
