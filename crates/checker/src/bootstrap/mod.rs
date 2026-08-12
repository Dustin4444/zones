//! Builds the initial authenticated checker checkpoint from Zone genesis.

mod ancestry;
mod error;
mod replay;
mod zone_genesis;

use crate::kernel::{ImportedOperation, PortalIdentity, State, apply_genesis_handoff};
use alloy_eips::BlockNumHash;
use alloy_provider::{Provider as _, ProviderBuilder};
use reth_storage_api::{BlockNumReader, StateProviderFactory};
use tempo_alloy::TempoNetwork;

use crate::{
    CheckerConfig,
    adapter::adapt_imported,
    observe::{acquire_l1_header, observe_l1},
    persistence::{BlockNumHash as StoredBlockNumHash, ChainCut, Identity, Persistence},
};
use ancestry::{anchor_header, authenticated_path};
use error::BootstrapError;
use replay::imported_block;
use zone_genesis::{LocalZoneIdentity, genesis_anchor, validate_zero_supply};

/// Build and atomically publish a checkpoint at local Zone genesis.
pub async fn build_checkpoint<P>(
    config: CheckerConfig,
    zone_chain_id: u64,
    zone_provider: &P,
) -> eyre::Result<()>
where
    P: BlockNumReader + StateProviderFactory + ?Sized,
{
    // Read the local Zone identity and its authenticated Tempo anchor.
    let l1_provider = ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect(&config.l1_rpc_url)
        .await?
        .erased();
    let l1_chain_id = l1_provider.get_chain_id().await?;
    let LocalZoneIdentity {
        genesis,
        initial_token,
    } = LocalZoneIdentity::load(&config, l1_chain_id, zone_chain_id, zone_provider)?;
    let anchor = genesis_anchor(zone_provider, genesis)?;
    let anchor_header = anchor_header(&l1_provider, anchor).await?;
    let creation_header =
        acquire_l1_header(&l1_provider, config.portal_creation_block_hash).await?;
    let creation_tip = BlockNumHash::new(creation_header.number(), creation_header.hash());
    let creation_observation =
        observe_l1(&l1_provider, &creation_header, config.portal_address).await?;
    let creation_facts = adapt_imported(
        &creation_observation,
        &creation_header,
        config.portal_creation_block_hash,
        config.zone_id,
    )
    .map_err(|failure| eyre::eyre!(failure.message))?
    .facts;
    let expected_identity = PortalIdentity {
        portal: config.portal_address,
        zone_id: config.zone_id,
        initial_token,
    };
    validate_creation(&creation_facts.operations, expected_identity)?;

    // Authenticate and replay Tempo history into the Zone genesis state.
    let mut state = State::awaiting(expected_identity);
    if creation_tip.number <= anchor.number {
        for header in authenticated_path(&l1_provider, &creation_header, anchor_header).await? {
            if header.hash() == creation_tip.hash {
                imported_block(
                    &mut state,
                    &creation_observation,
                    &header,
                    &config,
                    &l1_provider,
                )
                .await?;
            } else {
                let observation = observe_l1(&l1_provider, &header, config.portal_address).await?;
                imported_block(&mut state, &observation, &header, &config, &l1_provider).await?;
            }
        }
        validate_zero_supply(zone_provider, genesis.hash, state.tokens().map(|(t, _)| t))?;
        state.apply(&apply_genesis_handoff(&state)?)?;
    } else {
        authenticated_path(&l1_provider, &anchor_header, creation_header).await?;
        validate_zero_supply(zone_provider, genesis.hash, [initial_token])?;
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
    // Persist the initial authenticated checker checkpoint.
    Persistence::create_atomic(&config.database_path, identity, cut, state)?;
    Ok(())
}

/// Validate the Portal creation operation against local Zone identity.
fn validate_creation(
    operations: &[ImportedOperation],
    expected_identity: PortalIdentity,
) -> eyre::Result<()> {
    let [
        ImportedOperation::Create {
            identity,
            initial_token,
        },
    ] = operations
    else {
        eyre::bail!("creation block must contain one portal creation operation");
    };
    if *identity != expected_identity || initial_token.token != expected_identity.initial_token {
        eyre::bail!("creation identity does not match configuration and Zone genesis");
    }
    Ok(())
}
