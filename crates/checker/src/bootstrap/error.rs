//! Typed failures emitted while building a checker checkpoint.

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};
use reth_storage_api::errors::provider::ProviderError;

/// Failures specific to constructing the initial checker checkpoint.
#[derive(Debug, thiserror::Error)]
pub(super) enum BootstrapError {
    #[error("Zone ID must not be zero")]
    MissingZoneId,
    #[error(
        "expected one ZoneCreated event for Portal {portal} and Zone ID {zone_id}, found {count}"
    )]
    CreationCandidates {
        portal: Address,
        zone_id: u32,
        count: usize,
    },
    #[error("unsupported bootstrap: Zone genesis has a zero TempoState checkpoint")]
    UnsupportedBootstrapStyle,
    #[error(
        "unsupported bootstrap: Zone genesis has nonzero protocol progress (processed deposit cursor {processed_deposit_number}:{processed_deposit_queue_hash}, last withdrawal batch {withdrawal_batch_index}:{withdrawal_queue_hash})"
    )]
    NonzeroZoneGenesisProgress {
        processed_deposit_queue_hash: B256,
        processed_deposit_number: u64,
        withdrawal_queue_hash: B256,
        withdrawal_batch_index: u64,
    },
    #[error("unsupported bootstrap: token {token} has nonzero supply {actual} at Zone genesis")]
    NonzeroZoneGenesisSupply { token: Address, actual: U256 },
    #[error("Zone genesis fee token must not be zero")]
    MissingZoneGenesisInitialToken,
    #[error("failed to read canonical Zone block {number}")]
    LocalCanonicalRead {
        number: u64,
        #[source]
        source: ProviderError,
    },
    #[error("canonical Zone block {number} is missing; archive history is required")]
    MissingLocalCanonical { number: u64 },
    #[error(
        "Zone genesis TempoState checkpoint {checkpoint_number} does not match Tempo header {header_number} at {hash}"
    )]
    GenesisAnchorNumberMismatch {
        hash: B256,
        checkpoint_number: u64,
        header_number: u64,
    },
    #[error(
        "invalid Tempo ancestry range: descendant {descendant:?} precedes ancestor {ancestor:?}"
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
    #[error("Zone ID {zone_id} requires chain ID {expected}, local genesis uses {actual}")]
    ZoneChainIdMismatch {
        zone_id: u32,
        expected: u64,
        actual: u64,
    },
}
