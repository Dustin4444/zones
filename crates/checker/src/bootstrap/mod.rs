//! Deterministic local checkpoint-builder identity checks.

pub(crate) mod ancestry;
mod error;
pub(crate) mod genesis;

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256};
use reth_storage_api::{BlockNumReader, StateProviderFactory};

use crate::{
    CheckerConfig,
    protocol::state_layout::{DEFAULT_FEE_TOKEN_ACCESS, decode_address_word},
};

use self::error::BootstrapError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalZoneIdentity {
    genesis: BlockNumHash,
    initial_token: Address,
}

impl LocalZoneIdentity {
    pub(crate) const fn genesis(self) -> BlockNumHash {
        self.genesis
    }

    pub(crate) const fn initial_token(self) -> Address {
        self.initial_token
    }
}

pub(crate) fn validate_local_configuration<P>(
    config: &CheckerConfig,
    zone_chain_id: u64,
    zone_provider: &P,
) -> eyre::Result<LocalZoneIdentity>
where
    P: BlockNumReader + StateProviderFactory + ?Sized,
{
    if config.zone_id == 0 {
        return Err(BootstrapError::MissingZoneId.into());
    }
    if config.portal_creation_block_hash.is_zero() {
        return Err(BootstrapError::MissingCreationBlockHash.into());
    }
    let expected = zone_primitives::constants::zone_chain_id(config.zone_id);
    if zone_chain_id != expected {
        return Err(BootstrapError::ZoneChainIdMismatch {
            zone_id: config.zone_id,
            expected,
            actual: zone_chain_id,
        }
        .into());
    }
    let genesis = local_canonical_tip(zone_provider, 0)?;
    let initial_token = local_genesis_initial_token(zone_provider, genesis.hash)?;
    Ok(LocalZoneIdentity {
        genesis,
        initial_token,
    })
}

fn local_canonical_tip<P>(provider: &P, number: u64) -> eyre::Result<BlockNumHash>
where
    P: BlockNumReader + ?Sized,
{
    let hash = provider
        .block_hash(number)
        .map_err(|source| BootstrapError::LocalCanonicalRead { number, source })?
        .ok_or(BootstrapError::MissingLocalCanonical { number })?;
    Ok(BlockNumHash::new(number, hash))
}

fn local_genesis_initial_token<P>(provider: &P, hash: B256) -> eyre::Result<Address>
where
    P: StateProviderFactory + ?Sized,
{
    let state = provider
        .state_by_block_hash(hash)
        .map_err(|source| BootstrapError::LocalGenesisStateRead { hash, source })?;
    let word = state
        .storage(
            DEFAULT_FEE_TOKEN_ACCESS.address,
            DEFAULT_FEE_TOKEN_ACCESS.storage_key(),
        )
        .map_err(|source| BootstrapError::LocalGenesisStateRead { hash, source })?
        .unwrap_or_default();
    let token = decode_address_word(word)
        .ok_or(BootstrapError::MalformedZoneGenesisInitialToken { word })?;
    if token.is_zero() {
        return Err(BootstrapError::MissingZoneGenesisInitialToken.into());
    }
    Ok(token)
}
