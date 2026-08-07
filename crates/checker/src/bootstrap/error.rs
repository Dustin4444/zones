//! Fail-closed archive bootstrap errors.

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};
use reth_storage_api::errors::provider::ProviderError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum BootstrapError {
    #[error("checker observe mode requires a nonzero configured Zone ID")]
    MissingZoneId,
    #[error("checker observe mode requires a nonzero Portal creation block hash")]
    MissingCreationBlockHash,
    #[error(
        "unsupported checker bootstrap: canonical Zone genesis has a zero TempoState checkpoint"
    )]
    UnsupportedBootstrapStyle,
    #[error(
        "unsupported checker bootstrap: canonical Zone genesis has nonzero protocol progress \
         (processed deposit cursor {processed_deposit_number}:{processed_deposit_queue_hash}, \
         last withdrawal batch {withdrawal_batch_index}:{withdrawal_queue_hash})"
    )]
    NonzeroZoneGenesisProgress {
        processed_deposit_queue_hash: B256,
        processed_deposit_number: u64,
        withdrawal_queue_hash: B256,
        withdrawal_batch_index: u64,
    },
    #[error(
        "unsupported checker bootstrap: token {token} has nonzero supply {actual} at canonical Zone genesis"
    )]
    NonzeroZoneGenesisSupply { token: Address, actual: U256 },
    #[error("failed to read exact local Zone genesis state at {hash}")]
    LocalGenesisStateRead {
        hash: B256,
        #[source]
        source: ProviderError,
    },
    #[error("Zone genesis default fee token word is not a canonically padded address: {word}")]
    MalformedZoneGenesisInitialToken { word: U256 },
    #[error("Zone genesis default fee token must be nonzero")]
    MissingZoneGenesisInitialToken,
    #[error("failed to read local canonical Zone block {number}")]
    LocalCanonicalRead {
        number: u64,
        #[source]
        source: ProviderError,
    },
    #[error("local canonical Zone block {number} is missing; archive history is required")]
    MissingLocalCanonical { number: u64 },
    #[error(
        "Zone genesis TempoState checkpoint number {checkpoint_number} does not match exact L1 header {header_number} at {hash}"
    )]
    GenesisAnchorNumberMismatch {
        hash: B256,
        checkpoint_number: u64,
        header_number: u64,
    },
    #[error(
        "cannot prove Tempo ancestry: proposed descendant {descendant:?} precedes proposed ancestor {ancestor:?}"
    )]
    InvalidTempoAncestryRange {
        descendant: BlockNumHash,
        ancestor: BlockNumHash,
    },
    #[error(
        "Tempo ancestry from descendant {descendant:?} did not reach expected ancestor {expected_ancestor:?}; reached {reached:?}"
    )]
    TempoAncestryNotLinked {
        descendant: BlockNumHash,
        expected_ancestor: BlockNumHash,
        reached: BlockNumHash,
    },
    #[error(
        "non-consecutive Tempo ancestry: child {child:?} expected parent {expected_parent:?}, fetched {actual_parent:?}"
    )]
    NonConsecutiveTempoAncestry {
        child: BlockNumHash,
        expected_parent: BlockNumHash,
        actual_parent: BlockNumHash,
    },
    #[error(
        "configured Zone ID {zone_id} requires chain ID {expected}, local genesis uses {actual}"
    )]
    ZoneChainIdMismatch {
        zone_id: u32,
        expected: u64,
        actual: u64,
    },
}
