use alloy_primitives::{Address, B256};
use serde::{Deserialize, Serialize};
use zone_checker_kernel::{Finding as FindingDetails, State, StateDelta};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChainCut {
    pub zone: BlockNumHash,
    pub tempo: BlockNumHash,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Identity {
    pub l1_chain_id: u64,
    pub zone_chain_id: u64,
    pub zone_id: u32,
    pub portal: Address,
    pub creation_block: B256,
    pub creation_height: u64,
}
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CoverageGapReason {
    MissingReceipts,
    MissingTempoData,
    ProviderUnavailable,
    /// The notification is a descendant of a retained finding and therefore
    /// cannot be checked until that finding is removed by a reorg.
    NotCheckedAncestorDivergence,
    Other(u16),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Coverage {
    Complete,
    Gap {
        first_unchecked: BlockNumHash,
        acknowledged_through: BlockNumHash,
        reason: CoverageGapReason,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Metadata {
    pub identity: Identity,
    pub active_checkpoint: CheckpointId,
    pub verified_zone_tip: BlockNumHash,
    pub imported_tempo_tip: BlockNumHash,
    pub acknowledged_zone_tip: BlockNumHash,
    pub active_finding: Option<FindingKey>,
    pub coverage: Coverage,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum MetaValue {
    Version(u32),
    Metadata(Box<Metadata>),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub cut: ChainCut,
    pub state: State,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JournalEntry {
    pub zone: BlockNumHash,
    pub parent: BlockNumHash,
    pub imported_tempo: BlockNumHash,
    pub imported_tempo_parent: BlockNumHash,
    pub delta: StateDelta,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct FindingKey {
    pub zone: BlockNumHash,
    pub operation: u32,
    pub code: u16,
}
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub meta: Metadata,
    pub state: State,
}
