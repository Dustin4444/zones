//! Authenticated Portal replay and the atomic Zone-genesis handoff.

use std::path::Path;

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256};
use alloy_provider::{DynProvider, Provider as _};
use reth_storage_api::{BlockNumReader, StateProviderFactory};
use tempo_alloy::TempoNetwork;

use crate::{
    CheckerConfig,
    check::{
        finding::CheckError,
        pipeline::{InMemoryChecker, PreparedImportedBlock},
    },
    model::{
        adapter::project_imported,
        constants::ZONE_FACTORY_ADDRESS,
        state::{ModelState, PortalIdentity},
    },
    observe::{
        AcquisitionError, AcquisitionSource, ImportedTempoHeader, acquire_l1_header,
        acquire_zone_post_state, observe_l1,
    },
    runtime::{PersistentChecker, RuntimeResult},
    store::{
        db::{CheckerStore, FreshBootstrap, Initialization},
        operations::WriteOutcome,
        value::{BootstrapState, StoreIdentity},
    },
};

use super::{
    ancestry::{
        FreshHistory, acquire_anchor_header, classify_fresh_history, creation_parent, header_tip,
        prove_ancestry, prove_descendants_after,
    },
    error::BootstrapError,
    validate_local_configuration,
};

/// Fully authenticated inputs consumed immediately after fresh database
/// creation. Any later retry resumes from the durable L1 cursor instead.
struct FreshDatabasePlan {
    initialization: Initialization,
    l1_replay: Option<FreshL1Replay>,
}

struct FreshL1Replay {
    creation: PreparedImportedBlock,
    remaining_headers: Vec<ImportedTempoHeader>,
    anchor: BlockNumHash,
}

struct AuthenticatedCreation {
    header: ImportedTempoHeader,
    tip: BlockNumHash,
    parent: BlockNumHash,
    portal_identity: PortalIdentity,
    prepared: PreparedImportedBlock,
}

/// Authenticate all remote inputs needed for a fresh database, then persist
/// and finish any Portal-side replay through the Zone-genesis handoff.
pub(in crate::runtime) async fn create_fresh<P>(
    config: &CheckerConfig,
    zone_chain_id: u64,
    path: &Path,
    zone_provider: &P,
    l1_provider: &DynProvider<TempoNetwork>,
) -> RuntimeResult<PersistentChecker>
where
    P: BlockNumReader + StateProviderFactory + ?Sized,
{
    let local_zone = validate_local_configuration(config, zone_chain_id, zone_provider)?;
    let l1_chain_id = l1_provider
        .get_chain_id()
        .await
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Rpc, error))?;
    let plan = fresh_database_plan(
        config,
        zone_chain_id,
        local_zone.genesis(),
        local_zone.initial_token(),
        l1_chain_id,
        zone_provider,
        l1_provider,
    )
    .await?;
    let (store, snapshot) = CheckerStore::create_fresh_with_snapshot_at(path, plan.initialization)?;
    let mut checker = PersistentChecker::from_snapshot(store, snapshot);

    if let Some(replay) = plan.l1_replay {
        apply_fresh_l1_replay(config, zone_provider, l1_provider, &mut checker, replay).await?;
    }
    Ok(checker)
}

async fn fresh_database_plan<P>(
    config: &CheckerConfig,
    zone_chain_id: u64,
    zone_genesis: BlockNumHash,
    zone_initial_token: Address,
    l1_chain_id: u64,
    zone_provider: &P,
    l1_provider: &DynProvider<TempoNetwork>,
) -> RuntimeResult<FreshDatabasePlan>
where
    P: StateProviderFactory + ?Sized,
{
    let anchor = genesis_anchor(zone_provider, zone_genesis)?;
    let anchor_header = acquire_anchor_header(l1_provider, anchor).await?;
    let creation =
        authenticate_creation(config, zone_genesis, zone_initial_token, l1_provider).await?;
    let (start, l1_replay) = match classify_fresh_history(creation.tip, anchor) {
        FreshHistory::PortalPresentAtGenesisAnchor => {
            let path = prove_ancestry(l1_provider, anchor_header, &creation.header).await?;
            debug_assert_eq!(path.first().map(header_tip), Some(creation.tip));
            (
                FreshBootstrap::L1Replay {
                    creation_parent: creation.parent,
                },
                Some(FreshL1Replay {
                    creation: creation.prepared,
                    remaining_headers: path.into_iter().skip(1).collect(),
                    anchor,
                }),
            )
        }
        FreshHistory::PortalCreatedAfterGenesisAnchor => {
            prove_ancestry(l1_provider, creation.header, &anchor_header).await?;
            validate_zero_genesis_supply(
                zone_provider,
                zone_genesis.hash,
                std::iter::once(zone_initial_token),
            )?;
            (
                FreshBootstrap::ZoneReplay {
                    genesis_anchor: anchor,
                },
                None,
            )
        }
    };
    let identity = StoreIdentity::new(
        zone_chain_id,
        zone_genesis.hash,
        creation.portal_identity,
        l1_chain_id,
        ZONE_FACTORY_ADDRESS,
        creation.tip,
    );
    Ok(FreshDatabasePlan {
        initialization: Initialization::fresh(identity, start),
        l1_replay,
    })
}

async fn authenticate_creation(
    config: &CheckerConfig,
    zone_genesis: BlockNumHash,
    zone_initial_token: Address,
    l1_provider: &DynProvider<TempoNetwork>,
) -> RuntimeResult<AuthenticatedCreation> {
    let header = acquire_l1_header(l1_provider, config.portal_creation_block_hash)
        .await
        .map_err(CheckError::from)?;
    let tip = BlockNumHash::new(header.number(), header.hash());
    let parent = creation_parent(&header)?;
    let observation = observe_l1(l1_provider, &header, config.portal_address)
        .await
        .map_err(CheckError::from)?;
    let projection = project_imported(&observation, &header, config.portal_creation_block_hash)
        .map_err(BootstrapError::from)?;
    let portal_identity = projection
        .sole_portal_creation_identity()
        .map_err(BootstrapError::from)?;
    let portal_identity = configured_portal_identity(config, zone_initial_token, portal_identity)?;

    // Exercise the same transition, output reconciliation, creation-anchor,
    // and collateral checks as live processing before authoritative state is
    // created at the requested path.
    let validator = InMemoryChecker::new(
        ModelState::awaiting_creation(portal_identity),
        tip,
        zone_genesis,
        parent,
    );
    let prepared = validator
        .prepare_imported_bootstrap(l1_provider, &observation, &header)
        .await?;
    Ok(AuthenticatedCreation {
        header,
        tip,
        parent,
        portal_identity,
        prepared,
    })
}

fn configured_portal_identity(
    config: &CheckerConfig,
    zone_initial_token: Address,
    identity: PortalIdentity,
) -> RuntimeResult<PortalIdentity> {
    if identity.portal() != config.portal_address {
        return Err(BootstrapError::PortalMismatch {
            expected: config.portal_address,
            actual: identity.portal(),
        }
        .into());
    }
    if identity.zone_id() != config.zone_id {
        return Err(BootstrapError::ZoneIdMismatch {
            expected: config.zone_id,
            actual: identity.zone_id(),
        }
        .into());
    }
    if identity.initial_token() != zone_initial_token {
        return Err(BootstrapError::InitialTokenMismatch {
            expected: zone_initial_token,
            actual: identity.initial_token(),
        }
        .into());
    }
    Ok(identity)
}

async fn apply_fresh_l1_replay<P>(
    config: &CheckerConfig,
    zone_provider: &P,
    l1_provider: &DynProvider<TempoNetwork>,
    checker: &mut PersistentChecker,
    replay: FreshL1Replay,
) -> RuntimeResult<()>
where
    P: StateProviderFactory + ?Sized,
{
    persist_imported_bootstrap(checker, replay.creation)?;
    replay_l1_headers(config, l1_provider, checker, replay.remaining_headers).await?;
    finish_l1_replay(zone_provider, checker, replay.anchor)
}

pub(in crate::runtime) async fn resume_l1_replay<P>(
    config: &CheckerConfig,
    zone_provider: &P,
    l1_provider: &DynProvider<TempoNetwork>,
    checker: &mut PersistentChecker,
) -> RuntimeResult<()>
where
    P: StateProviderFactory + ?Sized,
{
    let progress = checker.store.load_progress()?;
    let BootstrapState::L1Replay { cursor } = progress.bootstrap else {
        return Ok(());
    };
    let anchor = genesis_anchor(zone_provider, progress.verified_zone_tip)?;
    if cursor == Some(anchor) {
        return finish_l1_replay(zone_provider, checker, anchor);
    }
    let anchor_header = acquire_anchor_header(l1_provider, anchor).await?;
    let remaining = remaining_headers(
        l1_provider,
        progress.imported_tempo_tip,
        cursor,
        anchor_header,
    )
    .await?;

    replay_l1_headers(config, l1_provider, checker, remaining).await?;
    finish_l1_replay(zone_provider, checker, anchor)
}

async fn remaining_headers(
    l1_provider: &DynProvider<TempoNetwork>,
    imported_tip: BlockNumHash,
    cursor: Option<BlockNumHash>,
    anchor_header: ImportedTempoHeader,
) -> RuntimeResult<Vec<ImportedTempoHeader>> {
    if let Some(cursor) = cursor
        && cursor != imported_tip
    {
        return Err(BootstrapError::CursorNotOnBootstrapPath { cursor }.into());
    }
    prove_descendants_after(l1_provider, anchor_header, imported_tip).await
}

async fn replay_l1_headers(
    config: &CheckerConfig,
    l1_provider: &DynProvider<TempoNetwork>,
    checker: &mut PersistentChecker,
    headers: impl IntoIterator<Item = ImportedTempoHeader>,
) -> RuntimeResult<()> {
    for header in headers {
        let observation = observe_l1(l1_provider, &header, config.portal_address)
            .await
            .map_err(CheckError::from)?;
        let prepared = checker
            .mirror
            .prepare_imported_bootstrap(l1_provider, &observation, &header)
            .await?;
        persist_imported_bootstrap(checker, prepared)?;
    }
    Ok(())
}

fn persist_imported_bootstrap(
    checker: &mut PersistentChecker,
    prepared: PreparedImportedBlock,
) -> RuntimeResult<()> {
    let current = checker.store.load_progress()?;
    let commit = checker.store.bootstrap_l1_commit(
        current.bootstrap,
        prepared.parent_tempo_tip(),
        prepared.child_tempo_tip(),
        prepared.state_update(),
    )?;
    match checker.store.apply_bootstrap(commit)? {
        WriteOutcome::Applied => checker.mirror.apply_prepared_imported(prepared),
        WriteOutcome::AlreadyApplied => checker.reload_mirror()?,
    }
    Ok(())
}

fn finish_l1_replay<P>(
    zone_provider: &P,
    checker: &mut PersistentChecker,
    anchor: BlockNumHash,
) -> RuntimeResult<()>
where
    P: StateProviderFactory + ?Sized,
{
    let completed = checker.store.load_progress()?;
    if completed.imported_tempo_tip != anchor {
        return Err(BootstrapError::CursorNotOnBootstrapPath {
            cursor: completed.imported_tempo_tip,
        }
        .into());
    }
    let handoff = checker.mirror.prepare_zone_genesis_handoff();
    validate_zero_genesis_supply(
        zone_provider,
        completed.verified_zone_tip.hash,
        handoff.tokens(),
    )?;
    let transition = checker.store.enter_zone_replay(
        completed.bootstrap,
        completed.imported_tempo_tip,
        &handoff,
    )?;
    match checker.store.apply_bootstrap(transition)? {
        WriteOutcome::Applied => checker.mirror.apply_zone_genesis_handoff(handoff),
        WriteOutcome::AlreadyApplied => checker.reload_mirror()?,
    }
    Ok(())
}

fn validate_zero_genesis_supply<P>(
    zone_provider: &P,
    zone_genesis_hash: B256,
    tokens: impl IntoIterator<Item = Address>,
) -> RuntimeResult<()>
where
    P: StateProviderFactory + ?Sized,
{
    let tokens = tokens.into_iter().collect::<Vec<_>>();
    let outputs = acquire_zone_post_state(zone_provider, zone_genesis_hash, &tokens)?;
    if let Some((&token, &actual)) = outputs
        .token_supplies()
        .iter()
        .find(|(_, supply)| !supply.is_zero())
    {
        return Err(BootstrapError::NonzeroZoneGenesisSupply { token, actual }.into());
    }
    Ok(())
}

pub(super) fn genesis_anchor<P>(
    zone_provider: &P,
    zone_genesis: BlockNumHash,
) -> RuntimeResult<BlockNumHash>
where
    P: StateProviderFactory + ?Sized,
{
    let outputs = acquire_zone_post_state(zone_provider, zone_genesis.hash, &[])?;
    if outputs.tempo_block_hash().is_zero() {
        return Err(BootstrapError::UnsupportedBootstrapStyle.into());
    }
    if !outputs.processed_deposit_queue_hash().is_zero()
        || outputs.processed_deposit_number() != 0
        || !outputs.withdrawal_queue_hash().is_zero()
        || outputs.withdrawal_batch_index() != 0
    {
        return Err(BootstrapError::NonzeroZoneGenesisProgress {
            processed_deposit_queue_hash: outputs.processed_deposit_queue_hash(),
            processed_deposit_number: outputs.processed_deposit_number(),
            withdrawal_queue_hash: outputs.withdrawal_queue_hash(),
            withdrawal_batch_index: outputs.withdrawal_batch_index(),
        }
        .into());
    }
    Ok(BlockNumHash::new(
        outputs.tempo_block_number(),
        outputs.tempo_block_hash(),
    ))
}
