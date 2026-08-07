//! Concrete archive bootstrap for the checker store.

use std::path::Path;

use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
use reth_storage_api::{BlockNumReader, StateProviderFactory};
use tempo_alloy::TempoNetwork;
use zone_checker_kernel::{
    ImportedOperation, PortalIdentity, State, apply_genesis_handoff, apply_imported,
};

use crate::{
    CheckerConfig,
    adapter::adapt_imported,
    bootstrap::{
        ancestry::{
            FreshHistory, acquire_anchor_header, classify_fresh_history, header_tip, prove_ancestry,
        },
        genesis::{genesis_anchor, validate_zero_genesis_supply},
        validate_local_configuration,
    },
    observe::{acquire_l1_header, acquire_portal_collateral, observe_l1},
    persistence::{BlockNumHash, ChainCut},
    runtime::{BuildConfig, publish_genesis_checkpoint},
};

/// Build and atomically publish a checkpoint at local Zone genesis.
///
/// Every Tempo block is authenticated by exact hash and replayed only through
/// the independent checker kernel.
pub async fn build_checkpoint<P>(
    config: CheckerConfig,
    zone_chain_id: u64,
    zone_provider: &P,
    target: impl AsRef<Path>,
) -> eyre::Result<()>
where
    P: BlockNumReader + StateProviderFactory + ?Sized,
{
    let l1_provider = ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect(&config.l1_rpc_url)
        .await?
        .erased();
    let local = validate_local_configuration(&config, zone_chain_id, zone_provider)?;
    let l1_chain_id = l1_provider.get_chain_id().await?;
    let anchor = genesis_anchor(zone_provider, local.genesis())?;
    let anchor_header = acquire_anchor_header(&l1_provider, anchor).await?;
    let creation_header =
        acquire_l1_header(&l1_provider, config.portal_creation_block_hash).await?;
    let creation_tip = header_tip(&creation_header);
    let creation_observation =
        observe_l1(&l1_provider, &creation_header, config.portal_address).await?;
    let creation_projection = adapt_imported(
        &creation_observation,
        &creation_header,
        config.portal_creation_block_hash,
        config.zone_id,
    )
    .map_err(|failure| eyre::eyre!(failure.message))?;
    let [
        ImportedOperation::Create {
            identity,
            initial_token,
        },
    ] = creation_projection.facts.operations.as_slice()
    else {
        eyre::bail!("creation block must contain exactly one Create operation");
    };
    let expected_identity = PortalIdentity {
        portal: config.portal_address,
        zone_id: config.zone_id,
        initial_token: local.initial_token(),
    };
    if *identity != expected_identity || initial_token.token != local.initial_token() {
        eyre::bail!(
            "authenticated creation identity does not match configuration and Zone genesis"
        );
    }

    let mut state = State::awaiting(expected_identity);
    match classify_fresh_history(creation_tip, anchor) {
        FreshHistory::PortalPresentAtGenesisAnchor => {
            let path = prove_ancestry(&l1_provider, anchor_header, &creation_header).await?;
            for header in path {
                let observation = if header.hash() == creation_tip.hash {
                    // Reuse the already authenticated creation body.
                    &creation_observation
                } else {
                    // The owned value must outlive projection and application.
                    let acquired = observe_l1(&l1_provider, &header, config.portal_address).await?;
                    let observation = acquired;
                    replay_one(&mut state, &observation, &header, &config, &l1_provider).await?;
                    continue;
                };
                replay_one(&mut state, observation, &header, &config, &l1_provider).await?;
            }
            let tokens = state.rows().keys().filter_map(|key| match key {
                zone_checker_kernel::StateKey::Token(token) => Some(*token),
                _ => None,
            });
            validate_zero_genesis_supply(zone_provider, local.genesis().hash, tokens)?;
            let handoff = apply_genesis_handoff(&state)?;
            state.apply(&handoff)?;
        }
        FreshHistory::PortalCreatedAfterGenesisAnchor => {
            // Reverse topology proof: the anchor must be an exact ancestor of
            // creation, while the checkpoint remains AwaitingCreation.
            prove_ancestry(&l1_provider, creation_header, &anchor_header).await?;
            validate_zero_genesis_supply(
                zone_provider,
                local.genesis().hash,
                [local.initial_token()],
            )?;
        }
    }

    publish_genesis_checkpoint(
        BuildConfig {
            path: target.as_ref(),
            l1_chain_id,
            zone_chain_id,
            creation_block: creation_tip.hash,
            creation_height: creation_tip.number,
            portal_identity: expected_identity,
            anchor: ChainCut {
                zone: BlockNumHash {
                    number: local.genesis().number,
                    hash: local.genesis().hash,
                },
                tempo: BlockNumHash {
                    number: anchor.number,
                    hash: anchor.hash,
                },
            },
        },
        state,
    )?;
    Ok(())
}

async fn replay_one(
    state: &mut State,
    observation: &crate::observe::L1BlockObservation,
    header: &crate::observe::ImportedTempoHeader,
    config: &CheckerConfig,
    provider: &DynProvider<TempoNetwork>,
) -> eyre::Result<()> {
    let projection = adapt_imported(
        observation,
        header,
        config.portal_creation_block_hash,
        config.zone_id,
    )
    .map_err(|failure| eyre::eyre!(failure.message))?;
    let candidate = apply_imported(state, &projection.facts)?;
    let expected_effects = candidate.expected_effects();
    if projection.effects != expected_effects {
        eyre::bail!("authenticated imported effects differ from checker candidate");
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
            eyre::bail!("imported-cut collateral is insufficient for token {token}");
        }
    }
    *state = candidate.into_state();
    Ok(())
}
