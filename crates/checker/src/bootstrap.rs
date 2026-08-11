use crate::kernel::{
    ImportedOperation, PortalIdentity, State, apply_genesis_handoff, apply_imported,
};
use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};
use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
use reth_storage_api::{BlockNumReader, StateProviderFactory, errors::provider::ProviderError};
use tempo_alloy::TempoNetwork;

use crate::{
    CheckerConfig,
    adapter::adapt_imported,
    observe::{
        ImportedTempoHeader, L1BlockObservation, acquire_l1_header, acquire_portal_collateral,
        acquire_zone_post_state, observe_l1,
    },
    persistence::{BlockNumHash as StoredBlockNumHash, ChainCut, Identity, Persistence},
};

/// Local Zone genesis facts required to authenticate bootstrap history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalZoneIdentity {
    genesis: BlockNumHash,
    initial_token: Address,
}

impl LocalZoneIdentity {
    /// Read and validate the local Zone identity at genesis.
    fn load<P>(
        config: &CheckerConfig,
        l1_chain_id: u64,
        zone_chain_id: u64,
        provider: &P,
    ) -> eyre::Result<Self>
    where
        P: BlockNumReader + StateProviderFactory + ?Sized,
    {
        if config.zone_id == 0 {
            return Err(BootstrapError::MissingZoneId.into());
        }
        if config.portal_creation_block_hash.is_zero() {
            return Err(BootstrapError::MissingCreationBlockHash.into());
        }
        let expected = zone_primitives::constants::zone_chain_id(l1_chain_id, config.zone_id)?;
        if zone_chain_id != expected {
            return Err(BootstrapError::ZoneChainIdMismatch {
                zone_id: config.zone_id,
                expected,
                actual: zone_chain_id,
            }
            .into());
        }
        let number = 0;
        let hash = provider
            .block_hash(number)
            .map_err(|source| BootstrapError::LocalCanonicalRead { number, source })?
            .ok_or(BootstrapError::MissingLocalCanonical { number })?;
        let initial_token = acquire_zone_post_state(provider, hash, &[])?.default_fee_token;
        if initial_token.is_zero() {
            return Err(BootstrapError::MissingZoneGenesisInitialToken.into());
        }
        Ok(Self {
            genesis: BlockNumHash::new(number, hash),
            initial_token,
        })
    }
}

/// Build and atomically publish a checkpoint at local Zone genesis.
pub async fn build_checkpoint<P>(
    config: CheckerConfig,
    zone_chain_id: u64,
    zone_provider: &P,
) -> eyre::Result<()>
where
    P: BlockNumReader + StateProviderFactory + ?Sized,
{
    let l1_provider = ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect(&config.l1_rpc_url)
        .await?
        .erased();
    let l1_chain_id = l1_provider.get_chain_id().await?;
    let LocalZoneIdentity {
        genesis,
        initial_token,
    } = LocalZoneIdentity::load(&config, l1_chain_id, zone_chain_id, zone_provider)?;
    let anchor = read_genesis_anchor(zone_provider, genesis)?;
    let anchor_header = acquire_anchor_header(&l1_provider, anchor).await?;
    let creation_header =
        acquire_l1_header(&l1_provider, config.portal_creation_block_hash).await?;
    let creation_tip = BlockNumHash::new(creation_header.number(), creation_header.hash());
    let creation_observation =
        observe_l1(&l1_provider, &creation_header, config.portal_address).await?;
    let (creation_facts, _) = adapt_imported(
        &creation_observation,
        &creation_header,
        config.portal_creation_block_hash,
        config.zone_id,
    )
    .map_err(|failure| eyre::eyre!(failure.message))?;
    let [
        ImportedOperation::Create {
            identity,
            initial_token: created_token,
        },
    ] = creation_facts.operations.as_slice()
    else {
        eyre::bail!("creation block must contain one portal creation operation");
    };
    let expected_identity = PortalIdentity {
        portal: config.portal_address,
        zone_id: config.zone_id,
        initial_token,
    };
    if *identity != expected_identity || created_token.token != initial_token {
        eyre::bail!("creation identity does not match configuration and Zone genesis");
    }

    let mut state = State::awaiting(expected_identity);
    if creation_tip.number <= anchor.number {
        for header in prove_ancestry(&l1_provider, &creation_header, anchor_header).await? {
            if header.hash() == creation_tip.hash {
                replay_one(
                    &mut state,
                    &creation_observation,
                    &header,
                    &config,
                    &l1_provider,
                )
                .await?;
            } else {
                let observation = observe_l1(&l1_provider, &header, config.portal_address).await?;
                replay_one(&mut state, &observation, &header, &config, &l1_provider).await?;
            }
        }
        validate_zero_genesis_supply(zone_provider, genesis.hash, state.tokens().map(|(t, _)| t))?;
        state.apply(&apply_genesis_handoff(&state)?)?;
    } else {
        prove_ancestry(&l1_provider, &anchor_header, creation_header).await?;
        validate_zero_genesis_supply(zone_provider, genesis.hash, [initial_token])?;
    }

    let identity = Identity {
        l1_chain_id,
        zone_chain_id,
        zone_id: expected_identity.zone_id,
        portal: expected_identity.portal,
        creation_block: creation_tip.hash,
        creation_height: creation_tip.number,
    };
    let cut = ChainCut {
        zone: StoredBlockNumHash {
            number: genesis.number,
            hash: genesis.hash,
        },
        tempo: StoredBlockNumHash {
            number: anchor.number,
            hash: anchor.hash,
        },
    };
    Persistence::create_atomic(&config.database_path, identity, cut, state)?;
    Ok(())
}

/// Acquire the authenticated Tempo header named by the Zone genesis checkpoint.
async fn acquire_anchor_header(
    provider: &DynProvider<TempoNetwork>,
    anchor: BlockNumHash,
) -> eyre::Result<ImportedTempoHeader> {
    let header = acquire_l1_header(provider, anchor.hash).await?;
    if header.number() != anchor.number {
        return Err(BootstrapError::GenesisAnchorNumberMismatch {
            hash: anchor.hash,
            checkpoint_number: anchor.number,
            header_number: header.number(),
        }
        .into());
    }
    Ok(header)
}

/// Prove and return the inclusive, hash-linked path from ancestor to descendant.
async fn prove_ancestry(
    provider: &DynProvider<TempoNetwork>,
    ancestor: &ImportedTempoHeader,
    descendant: ImportedTempoHeader,
) -> eyre::Result<Vec<ImportedTempoHeader>> {
    let ancestor_tip = BlockNumHash::new(ancestor.number(), ancestor.hash());
    let descendant_tip = BlockNumHash::new(descendant.number(), descendant.hash());
    if descendant_tip.number < ancestor_tip.number {
        return Err(BootstrapError::InvalidTempoAncestryRange {
            descendant: descendant_tip,
            ancestor: ancestor_tip,
        }
        .into());
    }
    if descendant_tip == ancestor_tip {
        return Ok(vec![ancestor.clone()]);
    }
    if descendant_tip.number == ancestor_tip.number {
        return Err(BootstrapError::TempoAncestryNotLinked {
            descendant: descendant_tip,
            expected_ancestor: ancestor_tip,
            reached: descendant_tip,
        }
        .into());
    }

    let mut current = descendant;
    let mut descending = Vec::new();
    while current.number() > ancestor_tip.number {
        let child = BlockNumHash::new(current.number(), current.hash());
        let parent_hash = current.header().parent_hash();
        descending.push(current.clone());
        if ancestor_tip.number.checked_add(1) == Some(current.number()) {
            if parent_hash != ancestor_tip.hash {
                return Err(BootstrapError::TempoAncestryNotLinked {
                    descendant: descendant_tip,
                    expected_ancestor: ancestor_tip,
                    reached: BlockNumHash::new(ancestor_tip.number, parent_hash),
                }
                .into());
            }
            descending.reverse();
            let mut path = Vec::with_capacity(descending.len() + 1);
            path.push(ancestor.clone());
            path.extend(descending);
            return Ok(path);
        }
        let parent = acquire_l1_header(provider, parent_hash).await?;
        if parent.number().checked_add(1) != Some(current.number()) {
            return Err(BootstrapError::NonConsecutiveTempoAncestry {
                child,
                expected_parent: BlockNumHash::new(current.number().saturating_sub(1), parent_hash),
                actual_parent: BlockNumHash::new(parent.number(), parent.hash()),
            }
            .into());
        }
        current = parent;
    }
    Err(BootstrapError::TempoAncestryNotLinked {
        descendant: descendant_tip,
        expected_ancestor: ancestor_tip,
        reached: BlockNumHash::new(current.number(), current.hash()),
    }
    .into())
}

/// Read the Tempo checkpoint embedded in Zone genesis and reject prior protocol progress.
fn read_genesis_anchor<P>(provider: &P, genesis: BlockNumHash) -> eyre::Result<BlockNumHash>
where
    P: StateProviderFactory + ?Sized,
{
    let outputs = acquire_zone_post_state(provider, genesis.hash, &[])?;
    if outputs.tempo_block_hash.is_zero() {
        return Err(BootstrapError::UnsupportedBootstrapStyle.into());
    }
    if !outputs.processed_deposit_queue_hash.is_zero()
        || outputs.processed_deposit_number != 0
        || !outputs.withdrawal_queue_hash.is_zero()
        || outputs.withdrawal_batch_index != 0
    {
        return Err(BootstrapError::NonzeroZoneGenesisProgress {
            processed_deposit_queue_hash: outputs.processed_deposit_queue_hash,
            processed_deposit_number: outputs.processed_deposit_number,
            withdrawal_queue_hash: outputs.withdrawal_queue_hash,
            withdrawal_batch_index: outputs.withdrawal_batch_index,
        }
        .into());
    }
    Ok(BlockNumHash::new(
        outputs.tempo_block_number,
        outputs.tempo_block_hash,
    ))
}

/// Verify that all enabled tokens have zero supply at Zone genesis.
fn validate_zero_genesis_supply<P>(
    provider: &P,
    genesis_hash: B256,
    tokens: impl IntoIterator<Item = Address>,
) -> eyre::Result<()>
where
    P: StateProviderFactory + ?Sized,
{
    let tokens = tokens.into_iter().collect::<Vec<_>>();
    let outputs = acquire_zone_post_state(provider, genesis_hash, &tokens)?;
    if let Some((&token, &actual)) = outputs.token_supplies.iter().find(|(_, s)| !s.is_zero()) {
        return Err(BootstrapError::NonzeroZoneGenesisSupply { token, actual }.into());
    }
    Ok(())
}

/// Apply one authenticated imported block and verify its effects and collateral.
async fn replay_one(
    state: &mut State,
    observation: &L1BlockObservation,
    header: &ImportedTempoHeader,
    config: &CheckerConfig,
    provider: &DynProvider<TempoNetwork>,
) -> eyre::Result<()> {
    let (facts, effects) = adapt_imported(
        observation,
        header,
        config.portal_creation_block_hash,
        config.zone_id,
    )
    .map_err(|failure| eyre::eyre!(failure.message))?;
    let candidate = apply_imported(state, &facts)?;
    if effects != candidate.expected_effects() {
        eyre::bail!("imported effects differ from expected effects");
    }
    for (token, accounting) in candidate.expected_accounting()? {
        let actual = acquire_portal_collateral(
            provider,
            token,
            observation.portal_address(),
            observation.block_hash(),
        )
        .await?;
        if accounting
            .collateral()
            .is_none_or(|required| actual < required)
        {
            eyre::bail!("imported collateral is insufficient for token {token}");
        }
    }
    *state = candidate.into_state();
    Ok(())
}

/// Failures specific to constructing the initial checker checkpoint.
#[derive(Debug, thiserror::Error)]
enum BootstrapError {
    #[error("Zone ID must not be zero")]
    MissingZoneId,
    #[error("Portal creation block hash must not be zero")]
    MissingCreationBlockHash,
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
