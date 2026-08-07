//! Explicit failures at the checker database trust boundary.

use std::path::PathBuf;

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256};
use reth_db::DatabaseError;

use super::{
    model_state::ModelPersistenceError,
    schema::{FindingKey, MetaKey, ModelKey},
    value::{BootstrapState, FindingStatus, MetaValue, ModelValue},
};

pub(crate) type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParentTips {
    pub(crate) zone: BlockNumHash,
    pub(crate) tempo: BlockNumHash,
}

impl ParentTips {
    pub(crate) const fn new(zone: BlockNumHash, tempo: BlockNumHash) -> Self {
        Self { zone, tempo }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    #[error("failed to open checker database at {path}")]
    Open {
        path: PathBuf,
        #[source]
        source: eyre::Report,
    },
    #[error("checker database at {path} does not exist or contains no durable state")]
    EmptyExistingDatabase { path: PathBuf },
    #[error("refusing to initialize nonempty checker database path {path}")]
    NonEmptyFreshDatabase { path: PathBuf },
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Model(#[from] ModelPersistenceError),
    #[error("invalid checker database initialization: {0}")]
    InvalidInitialization(&'static str),
    #[error("nonempty checker database is missing required metadata {0:?}")]
    MissingMetadata(MetaKey),
    #[error("metadata key {key:?} contains the wrong value family {value:?}")]
    MetadataType { key: MetaKey, value: MetaValue },
    #[error(
        "checker database at {path} has version {actual}, expected {expected}; replay into {rebuild_path} and replace the old database only after verification"
    )]
    VersionMismatch {
        path: PathBuf,
        expected: u32,
        actual: u32,
        rebuild_path: PathBuf,
    },
    #[error(
        "checker database identity at {key:?} differs from the configured identity: expected {expected:?}, found {actual:?}"
    )]
    IdentityMismatch {
        key: MetaKey,
        expected: Box<MetaValue>,
        actual: Box<MetaValue>,
    },
    #[error("checker database has an active alert for {0:?}")]
    ActiveAlert(FindingKey),
    #[error("checker parent tips changed: expected {expected:?}, found {actual:?}")]
    ParentChanged {
        expected: Box<ParentTips>,
        actual: Box<ParentTips>,
    },
    #[error("cannot unwind Zone block {expected:?}: current verified tip is {actual:?}")]
    UnwindTipMismatch {
        expected: BlockNumHash,
        actual: BlockNumHash,
    },
    #[error("non-adjacent {chain} child: parent {parent:?}, child {child:?}")]
    NonAdjacent {
        chain: &'static str,
        parent: BlockNumHash,
        child: BlockNumHash,
    },
    #[error("model key {key:?} cannot contain value {value:?}")]
    ModelKeyValueMismatch {
        key: ModelKey,
        value: Box<ModelValue>,
    },
    #[error("{0} does not have a canonical checker database encoding")]
    InvalidPersistedValue(&'static str),
    #[error("a block contains more model mutations than the u32 changeset ordinal permits")]
    TooManyMutations,
    #[cfg(test)]
    #[error("raw test mutations contain duplicate model key {0:?}")]
    DuplicateMutation(ModelKey),
    #[error("canonical Zone height {height} is missing")]
    MissingCanonical { height: u64 },
    #[error("canonical Zone height {height} conflicts: expected {expected}, found {actual}")]
    CanonicalConflict {
        height: u64,
        expected: B256,
        actual: B256,
    },
    #[error(
        "Zone block {child:?} has parent hash {actual}, expected current verified hash {expected}"
    )]
    CandidateParentConflict {
        child: BlockNumHash,
        expected: B256,
        actual: B256,
    },
    #[error("canonical Zone sequence is invalid: {0}")]
    CanonicalSequence(&'static str),
    #[error("changeset for Zone {height}/{hash} is missing ordinal {ordinal}")]
    MissingChangeset {
        height: u64,
        hash: B256,
        ordinal: u32,
    },
    #[error("changeset for Zone {height}/{hash} is invalid: {reason}")]
    InvalidChangeset {
        height: u64,
        hash: B256,
        reason: &'static str,
    },
    #[error("historical target {target} is above current verified Zone height {current}")]
    #[cfg(test)]
    FutureTarget { target: u64, current: u64 },
    #[error("bootstrap guard changed: expected {expected:?}, found {actual:?}")]
    BootstrapChanged {
        expected: BootstrapState,
        actual: BootstrapState,
    },
    #[error("imported Tempo tip changed: expected {expected:?}, found {actual:?}")]
    ImportedTipChanged {
        expected: BlockNumHash,
        actual: BlockNumHash,
    },
    #[error("metadata table cardinality differs: expected {expected}, found {actual}")]
    MetadataCardinality { expected: usize, actual: usize },
    #[error("invalid bootstrap progress: {0}")]
    InvalidBootstrapProgress(&'static str),
    #[error(
        "L1 replay must remain at exact Zone genesis {expected:?}, found verified tip {actual:?}"
    )]
    L1ReplayZoneTipMismatch {
        expected: BlockNumHash,
        actual: BlockNumHash,
    },
    #[error(
        "unstarted L1 replay must begin immediately before Portal creation {creation:?}, found imported tip {actual:?}"
    )]
    L1ReplayStartHeightMismatch {
        creation: BlockNumHash,
        actual: BlockNumHash,
    },
    #[error(
        "L1 replay cursor must include authenticated Portal creation {creation:?}, found {cursor:?}"
    )]
    L1ReplayCursorOutsideCreationHistory {
        creation: BlockNumHash,
        cursor: BlockNumHash,
    },
    #[error("first L1 replay block must be Portal creation {expected:?}, found {actual:?}")]
    L1ReplayFirstBlockMismatch {
        expected: BlockNumHash,
        actual: BlockNumHash,
    },
    #[error(
        "Portal settlement Tempo height {settlement_height} is above imported Tempo tip {imported_tip:?}"
    )]
    PortalSettlementBeyondImportedTempoTip {
        settlement_height: u64,
        imported_tip: BlockNumHash,
    },
    #[error(
        "live Portal settlement Zone height {settlement_height} is above verified Zone tip {verified_tip:?}"
    )]
    LivePortalSettlementBeyondVerifiedZoneTip {
        settlement_height: u64,
        verified_tip: BlockNumHash,
    },
    #[error(
        "Portal settlement hash at Zone height {height} conflicts with the verified canonical hash: settlement {settlement_hash}, canonical {canonical_hash}"
    )]
    PortalSettlementCanonicalConflict {
        height: u64,
        settlement_hash: B256,
        canonical_hash: B256,
    },
    #[error("checker {bootstrap:?} state contains token {token} in an impossible enablement phase")]
    BootstrapTokenPhaseMismatch {
        bootstrap: BootstrapState,
        token: Address,
    },
    #[error(
        "Portal creation progress is impossible: configured creation {creation:?}, imported tip {imported_tip:?}, model created={portal_created}"
    )]
    PortalCreationProgressMismatch {
        creation: BlockNumHash,
        imported_tip: BlockNumHash,
        portal_created: bool,
    },
    #[error("finding {key:?} conflicts with an existing durable finding")]
    FindingConflict { key: FindingKey },
    #[error("finding {key:?} must be canonical, found {status:?}")]
    FindingStatus {
        key: FindingKey,
        status: FindingStatus,
    },
    #[error("finding {key:?} is not anchored to current verified parent {parent:?}")]
    FindingParent {
        key: FindingKey,
        parent: BlockNumHash,
    },
    #[error("no active finding matches {0:?}")]
    NoActiveFinding(FindingKey),
    #[error("active alert {0:?} points to a missing finding")]
    MissingActiveFinding(FindingKey),
    #[cfg(test)]
    #[error("injected checker database write failure")]
    InjectedWriteFailure,
}
