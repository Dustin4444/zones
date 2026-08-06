//! Checker MDBX ownership, initialization, and current-state reads.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};
use reth_db::{
    Database, DatabaseEnv,
    cursor::{DbCursorRO, DbCursorRW},
    mdbx::{DatabaseArguments, init_db_for},
    transaction::{DbTx, DbTxMut},
};

use crate::model::state::{ModelState, TokenPhase};

use super::{
    codec::validate_canonical,
    error::{StoreError, StoreResult},
    model_state::{ModelRows, assemble_model, flatten_model},
    schema::{
        CanonicalHash, CheckerCanonical, CheckerChangesets, CheckerFindings, CheckerMeta,
        CheckerModelState, CheckerTables, MetaKey, ModelKey,
    },
    value::{
        ActiveAlert, BootstrapState, MetaValue, ModelValue, PortalSettlementValue, StoreIdentity,
    },
};

const CHECKER_DIRECTORY: &str = "checker";

#[derive(Debug, Clone)]
pub(crate) struct Initialization {
    pub(crate) identity: StoreIdentity,
    pub(crate) bootstrap: BootstrapState,
    pub(crate) verified_zone_tip: BlockNumHash,
    pub(crate) imported_tempo_tip: BlockNumHash,
    pub(crate) model: ModelState,
}

impl Initialization {
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

/// Fixed-size durable state needed to guard a sparse commit.
///
/// Opening and explicit snapshot reads validate the complete model. Once open,
/// typed model updates only need this head plus their touched rows; walking the
/// unbounded open-owner tables for every block would make commit cost depend on
/// historical backlog rather than the block's logical delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StoreHead {
    pub(super) verified_zone_tip: BlockNumHash,
    pub(super) imported_tempo_tip: BlockNumHash,
    pub(super) bootstrap: BootstrapState,
    pub(super) active_alert: Option<ActiveAlert>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RefundCredit {
    pub(crate) origin: u64,
    pub(crate) amount: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Open the dedicated checker environment, initializing only a wholly empty schema.
    pub(crate) fn open(
        data_dir: impl AsRef<Path>,
        initialization: Initialization,
    ) -> StoreResult<Self> {
        let path = data_dir.as_ref().join(CHECKER_DIRECTORY);
        let db = init_db_for::<_, CheckerTables>(&path, DatabaseArguments::default()).map_err(
            |source| StoreError::Open {
                path: path.clone(),
                source,
            },
        )?;
        let store = Self {
            db: Arc::new(db),
            identity: initialization.identity,
            path,
        };

        let tx = store.db.tx_mut()?;
        let is_empty = match all_tables_empty(&tx) {
            Ok(is_empty) => is_empty,
            Err(error) => {
                tx.abort();
                return Err(error);
            }
        };
        if is_empty {
            let result = prepare_initial_model(&initialization)
                .and_then(|rows| initialize_empty(&tx, &initialization, rows));
            match result {
                Ok(()) => tx.commit()?,
                Err(error) => {
                    tx.abort();
                    return Err(error);
                }
            }
        } else {
            tx.abort();
        }
        // Restart validates the authoritative cut without replaying history.
        // The exhaustive changeset walk remains an explicit diagnostic.
        store.validate_restart()?;
        Ok(store)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn load_current(&self) -> StoreResult<StoreSnapshot> {
        let tx = self.db.tx()?;
        let result = read_snapshot(&tx, self.identity, &self.path);
        finish_read(tx, result)
    }

    pub(crate) fn active_alert(&self) -> StoreResult<Option<ActiveAlert>> {
        let tx = self.db.tx()?;
        let result = read_active_alert(&tx);
        finish_read(tx, result)
    }

    pub(crate) fn portal_refund_credits(
        &self,
        token: Address,
        recipient: Address,
    ) -> StoreResult<Vec<RefundCredit>> {
        self.refund_credits(token, recipient, RefundLedger::Portal)
    }

    pub(crate) fn inbox_refund_credits(
        &self,
        token: Address,
        recipient: Address,
    ) -> StoreResult<Vec<RefundCredit>> {
        self.refund_credits(token, recipient, RefundLedger::Inbox)
    }

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
    bootstrap: BootstrapState,
    snapshot: &StoreSnapshot,
) -> StoreResult<()> {
    validate_model_cut_coherence(
        tx,
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
    bootstrap: Option<BootstrapState>,
    verified_zone_tip: BlockNumHash,
    imported_tempo_tip: BlockNumHash,
    model: &ModelState,
) -> StoreResult<()> {
    if let Some(bootstrap) = bootstrap {
        validate_bootstrap_model(bootstrap, model)?;
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
) -> StoreResult<StoreHead> {
    validate_identity(tx, identity, path)?;
    let bootstrap = read_bootstrap(tx)?;
    let verified_zone_tip = read_tip(tx, MetaKey::VerifiedZoneTip)?;
    let imported_tempo_tip = read_tip(tx, MetaKey::ImportedTempoTip)?;
    validate_bootstrap_coherence(bootstrap, imported_tempo_tip)?;
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

    Ok(StoreHead {
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
    if initialization.model.portal().identity() != initialization.identity.portal_identity() {
        return Err(StoreError::InvalidInitialization(
            "model Portal identity differs from database identity",
        ));
    }
    validate_bootstrap_model(initialization.bootstrap, &initialization.model)?;
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
            rebuild_path: path.with_file_name(format!("checker-v{expected_version}")),
        });
    }

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

fn validate_bootstrap_model(bootstrap: BootstrapState, model: &ModelState) -> StoreResult<()> {
    if bootstrap == BootstrapState::Live
        && let Some(token) = model.tokens().iter().find_map(|(token, state)| {
            (state.phase() == TokenPhase::PendingZoneEnable).then_some(*token)
        })
    {
        return Err(StoreError::LiveModelHasPendingToken { token });
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
