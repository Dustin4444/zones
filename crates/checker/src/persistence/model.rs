//! Durable checker records persisted in the local database.

use crate::{
    CheckerBlockedReason,
    kernel::{Finding as FindingDetails, State, StateDelta},
};
use alloy_primitives::{Address, B256};
use serde::{Deserialize, Serialize};

/// A block coordinate used in durable chain records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BlockNumHash {
    pub number: u64,
    pub hash: B256,
}

impl From<BlockNumHash> for alloy_eips::BlockNumHash {
    fn from(value: BlockNumHash) -> Self {
        Self::new(value.number, value.hash)
    }
}
/// The Zone and imported Tempo tips represented by a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChainCut {
    pub zone: BlockNumHash,
    pub tempo: BlockNumHash,
}
/// Immutable chain and Portal identity bound to one persistence database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Identity {
    pub l1_chain_id: u64,
    pub zone_chain_id: u64,
    pub zone_id: u32,
    pub portal: Address,
    pub creation_block: B256,
    pub creation_height: u64,
}
/// Stable checkpoint key derived from its Zone tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CheckpointId {
    pub height: u64,
    pub hash: B256,
}
impl From<BlockNumHash> for CheckpointId {
    fn from(v: BlockNumHash) -> Self {
        Self {
            height: v.number,
            hash: v.hash,
        }
    }
}
/// Why a contiguous range of Zone blocks could not be checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CoverageGapReason {
    MissingReceipts,
    MissingTempoData,
    ProviderUnavailable,
    /// The notification is a descendant of a retained finding and therefore
    /// cannot be checked until that finding is removed by a reorg.
    NotCheckedAncestorDivergence,
}

/// Whether checker coverage reaches the acknowledged Zone tip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Coverage {
    Complete,
    Gap {
        first_unchecked: BlockNumHash,
        acknowledged_through: BlockNumHash,
        reason: CoverageGapReason,
    },
}
/// Mutable durable pointers and coverage state for the active checker history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Metadata {
    pub identity: Identity,
    pub active_checkpoint: CheckpointId,
    pub verified_zone_tip: BlockNumHash,
    pub imported_tempo_tip: BlockNumHash,
    pub acknowledged_zone_tip: BlockNumHash,
    pub active_finding: Option<FindingKey>,
    pub coverage: Coverage,
    /// Why the checker stopped acknowledging new work.
    pub blocked: Option<CheckerBlockedReason>,
}
/// One value stored in the metadata table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum MetaValue {
    Version(u32),
    Metadata(Box<Metadata>),
}
/// A durable state snapshot at a chain cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub cut: ChainCut,
    pub state: State,
}
/// One verified Zone transition and its imported Tempo advancement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JournalEntry {
    pub zone: BlockNumHash,
    pub parent: BlockNumHash,
    pub imported_tempo: BlockNumHash,
    pub imported_tempo_parent: BlockNumHash,
    pub delta: StateDelta,
}
/// Stable coordinate for one durable checker finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct FindingKey {
    pub zone: BlockNumHash,
    pub operation: u32,
    pub code: u16,
}
/// Durable evidence for a checker divergence at one Zone coordinate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Finding {
    pub zone: BlockNumHash,
    pub parent: BlockNumHash,
    pub imported_tempo: Option<BlockNumHash>,
    pub imported_tempo_parent: Option<BlockNumHash>,
    pub details: FindingDetails,
    pub evidence_len: u32,
    pub evidence_digest: B256,
    pub summary: String,
}
/// The currently reconstructed durable checker state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub meta: Metadata,
    pub state: State,
}
