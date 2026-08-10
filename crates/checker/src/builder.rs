use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::kernel::{
    ImportedOperation, PortalIdentity, State, apply_genesis_handoff, apply_imported,
};
use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
use reth_storage_api::{BlockNumReader, StateProviderFactory};
use tempo_alloy::TempoNetwork;

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
    persistence::{BlockNumHash, ChainCut, Identity, Persistence, PersistenceError, Snapshot},
};

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
    let local = validate_local_configuration(&config, zone_chain_id, zone_provider)?;
    let l1_chain_id = l1_provider.get_chain_id().await?;
    let anchor = genesis_anchor(zone_provider, local.genesis())?;
    let anchor_header = acquire_anchor_header(&l1_provider, anchor).await?;
    let creation_header =
        acquire_l1_header(&l1_provider, config.portal_creation_block_hash).await?;
    let creation_tip = header_tip(&creation_header);
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
            initial_token,
        },
    ] = creation_facts.operations.as_slice()
    else {
        eyre::bail!("creation block must contain one portal creation operation");
    };
    let expected_identity = PortalIdentity {
        portal: config.portal_address,
        zone_id: config.zone_id,
        initial_token: local.initial_token(),
    };
    if *identity != expected_identity || initial_token.token != local.initial_token() {
        eyre::bail!("creation identity does not match configuration and Zone genesis");
    }

    let mut state = State::awaiting(expected_identity);
    match classify_fresh_history(creation_tip, anchor) {
        FreshHistory::PortalPresentAtGenesisAnchor => {
            let path = prove_ancestry(&l1_provider, anchor_header, &creation_header).await?;
            for header in path {
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
                    let observation =
                        observe_l1(&l1_provider, &header, config.portal_address).await?;
                    replay_one(&mut state, &observation, &header, &config, &l1_provider).await?;
                }
            }
            let tokens = state.tokens().map(|(token, _)| token);
            validate_zero_genesis_supply(zone_provider, local.genesis().hash, tokens)?;
            let handoff = apply_genesis_handoff(&state)?;
            state.apply(&handoff)?;
        }
        FreshHistory::PortalCreatedAfterGenesisAnchor => {
            prove_ancestry(&l1_provider, creation_header, &anchor_header).await?;
            validate_zero_genesis_supply(
                zone_provider,
                local.genesis().hash,
                [local.initial_token()],
            )?;
        }
    }

    let identity = Identity {
        l1_chain_id,
        zone_chain_id,
        zone_id: expected_identity.zone_id,
        portal: expected_identity.portal,
        creation_block: creation_tip.hash,
        creation_height: creation_tip.number,
    };
    let anchor = ChainCut {
        zone: BlockNumHash {
            number: local.genesis().number,
            hash: local.genesis().hash,
        },
        tempo: BlockNumHash {
            number: anchor.number,
            hash: anchor.hash,
        },
    };
    publish_genesis_checkpoint(&config.database_path, identity, anchor, state)?;
    Ok(())
}

pub(crate) fn publish_genesis_checkpoint(
    target: &Path,
    identity: Identity,
    anchor: ChainCut,
    state: State,
) -> Result<Snapshot, PersistenceError> {
    let staging = prepare_staging(target)?;
    let result = (|| {
        let (store, snapshot) = Persistence::create(&staging, identity, anchor, state)?;
        drop(store);
        let (reopened, verified) = Persistence::open(&staging, identity)?;
        drop(reopened);
        if snapshot != verified {
            return Err(PersistenceError::Invalid(
                "genesis checkpoint changed across final reopen".into(),
            ));
        }
        Ok(verified)
    })();
    publish_staging(target, &staging, result)
}

fn prepare_staging(target: &Path) -> Result<PathBuf, PersistenceError> {
    if target.exists() {
        return Err(PersistenceError::Invalid(
            "checkpoint target already exists".into(),
        ));
    }
    let staging = staging_path(target)?;
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|error| {
            PersistenceError::Invalid(format!("cannot remove checkpoint staging: {error}"))
        })?;
    }
    fs::create_dir_all(&staging).map_err(|error| {
        PersistenceError::Invalid(format!("cannot create checkpoint staging: {error}"))
    })?;
    Ok(staging)
}

fn publish_staging(
    target: &Path,
    staging: &Path,
    result: Result<Snapshot, PersistenceError>,
) -> Result<Snapshot, PersistenceError> {
    match result {
        Ok(snapshot) => {
            fs::rename(staging, target).map_err(|error| {
                let _ = fs::remove_dir_all(staging);
                PersistenceError::Invalid(format!("cannot atomically publish checkpoint: {error}"))
            })?;
            Ok(snapshot)
        }
        Err(error) => {
            let _ = fs::remove_dir_all(staging);
            Err(error)
        }
    }
}

fn staging_path(target: &Path) -> Result<PathBuf, PersistenceError> {
    let parent = target.parent().ok_or_else(|| {
        PersistenceError::Invalid("checkpoint target has no sibling directory".into())
    })?;
    let name = target.file_name().ok_or_else(|| {
        PersistenceError::Invalid("checkpoint target has no directory name".into())
    })?;
    Ok(parent.join(format!(
        ".{}.staging-{}",
        name.to_string_lossy(),
        std::process::id()
    )))
}

async fn replay_one(
    state: &mut State,
    observation: &crate::observe::L1BlockObservation,
    header: &crate::observe::ImportedTempoHeader,
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
    let expected_effects = candidate.expected_effects();
    if effects != expected_effects {
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
