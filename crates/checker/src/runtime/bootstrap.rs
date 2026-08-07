//! Version-gated open, authenticated L1 bootstrap, and Zone-replay handoff.

pub(crate) mod ancestry;
pub(super) mod error;
pub(crate) mod genesis;
mod replay;

use std::path::{Path, PathBuf};

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256};
use reth_storage_api::{BlockNumReader, StateProviderFactory};

use crate::{
    CheckerConfig,
    model::{
        constants::ZONE_FACTORY_ADDRESS,
        state::PortalIdentity,
        state_layout::{DEFAULT_FEE_TOKEN_ACCESS, decode_address_word},
    },
    store::{db::CheckerStore, error::StoreError, value::StoreIdentity},
};

use self::error::BootstrapError;

#[cfg(test)]
use self::{
    ancestry::{
        FreshHistory, classify_fresh_history, header_tip, prove_ancestry, prove_descendants_after,
    },
    genesis::genesis_anchor,
};

pub(super) use self::replay::{create_fresh, resume_l1_replay};

use super::{PersistentChecker, RuntimeResult};

pub(super) enum DatabaseState {
    Fresh,
    Existing(StoreIdentity),
}

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

/// Resolve the explicit rebuild path or the canonical per-chain checker path.
pub(super) fn database_path(config: &CheckerConfig, data_dir: &Path) -> PathBuf {
    config
        .database_path
        .clone()
        .unwrap_or_else(|| CheckerStore::path_in(data_dir))
}

/// Version-gate an existing database before any remote dependency is touched.
pub(super) fn inspect_database(path: &Path) -> RuntimeResult<DatabaseState> {
    match CheckerStore::inspect_identity_at(path) {
        Ok(identity) => Ok(DatabaseState::Existing(identity)),
        Err(StoreError::EmptyExistingDatabase { .. }) => Ok(DatabaseState::Fresh),
        Err(error) => Err(error.into()),
    }
}

/// Validate checker configuration against the local Zone identity without
/// touching the remote Tempo endpoint.
pub(crate) fn validate_local_configuration<P>(
    config: &CheckerConfig,
    zone_chain_id: u64,
    zone_provider: &P,
) -> RuntimeResult<LocalZoneIdentity>
where
    P: BlockNumReader + StateProviderFactory + ?Sized,
{
    if config.zone_id == 0 {
        return Err(BootstrapError::MissingZoneId.into());
    }
    if config.portal_creation_block_hash.is_zero() {
        return Err(BootstrapError::MissingCreationBlockHash.into());
    }
    let expected_zone_chain_id = zone_primitives::constants::zone_chain_id(config.zone_id);
    if zone_chain_id != expected_zone_chain_id {
        return Err(BootstrapError::ZoneChainIdMismatch {
            zone_id: config.zone_id,
            expected: expected_zone_chain_id,
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

/// Open and validate an existing durable cut using only local configuration
/// and the identity already authenticated into that database.
pub(super) fn open_existing<P>(
    config: &CheckerConfig,
    zone_chain_id: u64,
    path: &Path,
    stored: StoreIdentity,
    zone_provider: &P,
) -> RuntimeResult<PersistentChecker>
where
    P: BlockNumReader + StateProviderFactory + ?Sized,
{
    let local = validate_local_configuration(config, zone_chain_id, zone_provider)?;
    let stored_creation = stored.portal_creation_block();
    let expected = StoreIdentity::new(
        zone_chain_id,
        local.genesis().hash,
        PortalIdentity::new(config.portal_address, config.zone_id, local.initial_token()),
        stored.l1_chain_id(),
        ZONE_FACTORY_ADDRESS,
        BlockNumHash::new(stored_creation.number, config.portal_creation_block_hash),
    );
    let (store, snapshot) = CheckerStore::open_existing_with_snapshot_at(path, expected)?;
    Ok(PersistentChecker::from_snapshot(store, snapshot))
}

fn local_canonical_tip<P>(provider: &P, number: u64) -> RuntimeResult<BlockNumHash>
where
    P: BlockNumReader + ?Sized,
{
    let hash = provider
        .block_hash(number)
        .map_err(|source| BootstrapError::LocalCanonicalRead { number, source })?
        .ok_or(BootstrapError::MissingLocalCanonical { number })?;
    Ok(BlockNumHash::new(number, hash))
}

fn local_genesis_initial_token<P>(provider: &P, hash: B256) -> RuntimeResult<Address>
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

#[cfg(test)]
mod tests;
