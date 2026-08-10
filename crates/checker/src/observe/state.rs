//! Exact post-block Zone state acquisition.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, U256};
use reth_storage_api::StateProviderFactory;
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_precompiles::{storage::StorageActions, tip20::TIP20Token};
use tempo_revm::TempoStateAccess;
use zone_precompiles::zone_state::ZoneStateSnapshot;

use crate::observe::error::{AcquisitionError, AcquisitionSource};

/// Protocol commitments read from state after one exact Zone block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ZonePostStateOutputs {
    tempo_block_hash: B256,
    tempo_block_number: u64,
    processed_deposit_queue_hash: B256,
    processed_deposit_number: u64,
    withdrawal_queue_hash: B256,
    withdrawal_batch_index: u64,
    default_fee_token: Address,
    token_supplies: BTreeMap<Address, U256>,
}

impl ZonePostStateOutputs {
    pub(crate) fn tempo_block_hash(&self) -> B256 {
        self.tempo_block_hash
    }
    pub(crate) fn tempo_block_number(&self) -> u64 {
        self.tempo_block_number
    }
    pub(crate) fn processed_deposit_queue_hash(&self) -> B256 {
        self.processed_deposit_queue_hash
    }
    pub(crate) fn processed_deposit_number(&self) -> u64 {
        self.processed_deposit_number
    }
    pub(crate) fn withdrawal_queue_hash(&self) -> B256 {
        self.withdrawal_queue_hash
    }
    pub(crate) fn withdrawal_batch_index(&self) -> u64 {
        self.withdrawal_batch_index
    }
    pub(crate) fn default_fee_token(&self) -> Address {
        self.default_fee_token
    }
    pub(crate) fn token_supplies(&self) -> &BTreeMap<Address, U256> {
        &self.token_supplies
    }
}

/// Acquire protocol outputs from the state selected by `block_hash` exactly.
pub(crate) fn acquire_zone_post_state<P: StateProviderFactory + ?Sized>(
    provider: &P,
    block_hash: B256,
    tokens: &[Address],
) -> Result<ZonePostStateOutputs, AcquisitionError> {
    let mut state = provider
        .state_by_block_hash(block_hash)
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::ExactZoneState, error))?;

    state
        .with_read_only_storage_ctx(
            TempoHardfork::T10,
            StorageActions::disabled(),
            || -> tempo_precompiles::Result<ZonePostStateOutputs> {
                let snapshot = ZoneStateSnapshot::read()?;
                let token_supplies = tokens
                    .iter()
                    .copied()
                    .map(|token| Ok((token, TIP20Token::from_address(token)?.total_supply()?)))
                    .collect::<tempo_precompiles::Result<BTreeMap<_, _>>>()?;

                Ok(ZonePostStateOutputs {
                    tempo_block_hash: snapshot.tempo_block_hash,
                    tempo_block_number: snapshot.tempo_block_number,
                    processed_deposit_queue_hash: snapshot.processed_deposit_queue_hash,
                    processed_deposit_number: snapshot.processed_deposit_number,
                    withdrawal_queue_hash: snapshot.last_withdrawal_batch.withdrawalQueueHash,
                    withdrawal_batch_index: snapshot.last_withdrawal_batch.withdrawalBatchIndex,
                    default_fee_token: snapshot.default_fee_token,
                    token_supplies,
                })
            },
        )
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::ExactZoneState, error))
}
