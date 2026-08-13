//! Reth ExEx integration for acquiring and verifying canonical Zone blocks.

use std::{
    collections::BTreeSet,
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_consensus::BlockHeader as _;
use alloy_primitives::{Address, B256, U256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use futures::TryStreamExt;
use reth_chainspec::EthChainSpec as _;
use reth_execution_types::Chain;
use reth_exex::{ExExContext, ExExNotification};
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_storage_api::{
    BlockHashReader, BlockNumReader, BlockReader, StateProviderFactory, TransactionVariant,
};
use tempo_alloy::TempoNetwork;
use tempo_primitives::{Block, TempoPrimitives, TempoReceipt};

use crate::{
    CheckerBlockedReason, CheckerConfig,
    adapter::{AuthenticatedObservation, adapt},
    failure::Failure,
    kernel::{State, TokenPhase, apply_imported},
    observe::{
        L2BlockObservation, ZonePostStateOutputs, acquire_portal_token_balance,
        acquire_zone_post_state, observe_l1_range, observe_l2_block_with_context,
    },
    persistence::{BlockNumHash, Identity, Persistence},
    runtime::{
        AuthenticatedBlock, AuthenticationFailure, AuthenticationRequest, Runtime, RuntimeAction,
    },
};

/// Run the checker ExEx without allowing checker-local failures to finish it.
pub(super) async fn run<Node>(config: CheckerConfig, mut ctx: ExExContext<Node>) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider:
        BlockReader<Block = Block, Receipt = TempoReceipt> + BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    loop {
        match start_and_run(&config, &mut ctx).await {
            Ok(()) => std::future::pending::<()>().await,
            Err(error) => {
                tracing::error!(target: "zone::checker", %error, "checker recovery attempt failed");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Open checker state and drive it until a checker-local failure blocks progress.
async fn start_and_run<Node>(
    config: &CheckerConfig,
    ctx: &mut ExExContext<Node>,
) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider:
        BlockReader<Block = Block, Receipt = TempoReceipt> + BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    eyre::ensure!(
        !config.acquisition_timeout.is_zero(),
        "checker acquisition timeout must not be zero"
    );
    let path = config.database_path.as_path();
    let identity = Persistence::inspect_identity(path)?;
    validate_checkpoint_identity(config, ctx.config.chain.chain().id(), identity)?;
    let (store, snapshot) = Persistence::open(path, identity)?;
    let mut runtime = Runtime::new(snapshot);

    // The local node retains the replay journal; ExEx notifications only wake recovery.
    ctx.send_finished_height(runtime.snapshot().meta.verified_zone_tip.into())?;
    if runtime.snapshot().meta.blocked.is_some() {
        tracing::error!(target: "zone::checker", "checker remains blocked from a previous run");
        return drain_notifications(ctx).await;
    }
    let l2_provider = ctx.provider().clone();
    refresh_canonical_head(&mut runtime, &store, &l2_provider)?;
    if runtime.snapshot().meta.blocked.is_some() {
        return drain_notifications(ctx).await;
    }

    let (l1_provider, actual_l1_chain_id) =
        connect_l1_while_draining(config, ctx, &store, &mut runtime).await?;
    if actual_l1_chain_id != identity.l1_chain_id {
        runtime.block(&store, CheckerBlockedReason::TempoChainMismatch)?;
        tracing::error!(target: "zone::checker", expected = identity.l1_chain_id, actual = actual_l1_chain_id, "Tempo chain ID does not match the checker checkpoint");
        return drain_notifications(ctx).await;
    }
    run_loop(config, ctx, &store, identity, &mut runtime, &l1_provider).await?;
    Ok(())
}

/// Keep a blocked checker from applying notification-channel backpressure to the node.
async fn drain_notifications<Node>(ctx: &mut ExExContext<Node>) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    loop {
        match ctx.notifications.try_next().await {
            Ok(Some(_)) => {}
            Ok(None) => eyre::bail!("checker notification stream closed"),
            Err(error) => {
                tracing::error!(target: "zone::checker", %error, "checker notification stream failed; resuming direct delivery");
                ctx.set_notifications_without_head();
            }
        }
    }
}

/// Await checker work while consuming notification wakeups from the node.
async fn await_with_notifications<Node, F, T>(
    ctx: &mut ExExContext<Node>,
    store: &Persistence,
    runtime: &mut Runtime,
    future: F,
) -> eyre::Result<Option<T>>
where
    Node: FullNodeComponents,
    Node::Provider:
        BlockReader<Block = Block, Receipt = TempoReceipt> + BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
    F: Future<Output = T>,
{
    if runtime.snapshot().meta.blocked.is_some() {
        return Ok(None);
    }
    let provider = ctx.provider().clone();
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Ok(Some(output)),
            next = ctx.notifications.try_next() => {
                handle_notification(ctx, runtime, store, &provider, next)?;
                if runtime.snapshot().meta.blocked.is_some() {
                    return Ok(None);
                }
            }
        }
    }
}

/// Restore the newest retained checker checkpoint that remains canonical locally.
fn reconcile_canonical_head<P>(
    runtime: &mut Runtime,
    store: &Persistence,
    provider: &P,
) -> eyre::Result<()>
where
    P: BlockHashReader + BlockNumReader + ?Sized,
{
    let verified = runtime.snapshot().meta.verified_zone_tip;
    if provider.block_hash(verified.number)? == Some(verified.hash) {
        if let Some(finding) = runtime.snapshot().meta.active_finding
            && provider.block_hash(finding.zone.number)? != Some(finding.zone.hash)
        {
            runtime.reorg(store, verified)?;
        }
        return Ok(());
    }
    let head = provider.best_block_number()?;
    for retained in store.retained_zone_coordinates()?.into_iter().rev() {
        if retained.number > head {
            continue;
        }
        if provider.block_hash(retained.number)? == Some(retained.hash) {
            runtime.reorg(store, retained)?;
            return Ok(());
        }
    }
    tracing::error!(target: "zone::checker", verified_block = verified.number, local_head = head, "Zone reorg exceeds retained checker history");
    runtime.block(store, CheckerBlockedReason::DeepReorgBeyondRetention)?;
    Ok(())
}

/// Reconcile verified history and record the current local canonical head.
fn refresh_canonical_head<P>(
    runtime: &mut Runtime,
    store: &Persistence,
    provider: &P,
) -> eyre::Result<()>
where
    P: BlockHashReader + BlockNumReader + ?Sized,
{
    reconcile_canonical_head(runtime, store, provider)?;
    if runtime.snapshot().meta.blocked.is_some() {
        return Ok(());
    }
    let number = provider.best_block_number()?;
    let hash = provider
        .block_hash(number)?
        .ok_or_else(|| eyre::eyre!("canonical Zone block {number} is unavailable"))?;
    runtime.observe_tip(store, BlockNumHash { number, hash })?;
    Ok(())
}

/// Connect to Tempo once and authenticate its chain identity.
async fn connect_l1(config: &CheckerConfig) -> eyre::Result<(DynProvider<TempoNetwork>, u64)> {
    tokio::time::timeout(config.acquisition_timeout, async {
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect(&config.l1_rpc_url)
            .await?
            .erased();
        let chain_id = provider.get_chain_id().await?;
        Ok((provider, chain_id))
    })
    .await
    .map_err(|_| eyre::eyre!("Tempo connection attempt timed out"))?
}

/// Connect to Tempo while continuing to consume and coalesce Zone notifications.
async fn connect_l1_while_draining<Node>(
    config: &CheckerConfig,
    ctx: &mut ExExContext<Node>,
    store: &Persistence,
    runtime: &mut Runtime,
) -> eyre::Result<(DynProvider<TempoNetwork>, u64)>
where
    Node: FullNodeComponents,
    Node::Provider:
        BlockReader<Block = Block, Receipt = TempoReceipt> + BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    loop {
        let Some(result) =
            await_with_notifications(ctx, store, runtime, connect_l1(config)).await?
        else {
            eyre::bail!("checker blocked while connecting to Tempo");
        };
        let error = match result {
            Ok(connection) => return Ok(connection),
            Err(error) => error,
        };
        tracing::warn!(target: "zone::checker", %error, "checker could not connect to Tempo; retrying");
        if await_with_notifications(
            ctx,
            store,
            runtime,
            tokio::time::sleep(Duration::from_secs(1)),
        )
        .await?
        .is_none()
        {
            eyre::bail!("checker blocked while connecting to Tempo");
        }
    }
}

/// Drive checker work after startup without allowing a checker failure to finish the ExEx.
async fn run_loop<Node, P>(
    config: &CheckerConfig,
    ctx: &mut ExExContext<Node>,
    store: &Persistence,
    identity: Identity,
    runtime: &mut Runtime,
    l1_provider: &P,
) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider:
        BlockReader<Block = Block, Receipt = TempoReceipt> + BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
    P: Provider<TempoNetwork>,
{
    loop {
        if runtime.snapshot().meta.blocked.is_some() {
            return drain_notifications(ctx).await;
        }
        let action = runtime.next_action(Instant::now());
        match action {
            RuntimeAction::Authenticate(request) => {
                let Some(result) =
                    authenticate_while_draining(config, ctx, store, runtime, request, l1_provider)
                        .await?
                else {
                    continue;
                };
                if let Some(verified) = runtime.complete_authentication(
                    store,
                    identity,
                    request,
                    result,
                    Instant::now(),
                )? {
                    ctx.send_finished_height(verified.into())?;
                }
                continue;
            }
            RuntimeAction::RetryAt(deadline) => {
                await_with_notifications(
                    ctx,
                    store,
                    runtime,
                    tokio::time::sleep_until(deadline.into()),
                )
                .await?;
                continue;
            }
            RuntimeAction::AwaitNotification => {}
        }

        let next = ctx.notifications.try_next().await;
        let l2_state_provider = ctx.provider().clone();
        handle_notification(ctx, runtime, store, &l2_state_provider, next)?;
    }
}

/// Authenticate one canonical block while continuing to consume notification wakeups.
async fn authenticate_while_draining<Node, P>(
    config: &CheckerConfig,
    ctx: &mut ExExContext<Node>,
    store: &Persistence,
    runtime: &mut Runtime,
    request: AuthenticationRequest,
    l1_provider: &P,
) -> eyre::Result<Option<Result<AuthenticatedBlock, AuthenticationFailure>>>
where
    Node: FullNodeComponents,
    Node::Provider:
        BlockReader<Block = Block, Receipt = TempoReceipt> + BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
    P: Provider<TempoNetwork>,
{
    let provider = ctx.provider().clone();
    let parent = runtime.snapshot().meta.verified_zone_tip;
    let authentication = tokio::time::timeout(
        config.acquisition_timeout,
        authenticate_canonical_zone_block(
            &provider,
            request.height(),
            l1_provider,
            Arc::clone(&runtime.snapshot().state),
            config.portal_creation_block_hash,
            config.zone_id,
        ),
    );
    let Some(result) = await_with_notifications(ctx, store, runtime, authentication).await? else {
        return Ok(None);
    };
    let result = result.unwrap_or_else(|_| {
        Err(AuthenticationFailure::unlocated(Failure::retry(
            "checker acquisition timed out",
        )))
    });
    refresh_canonical_head(runtime, store, &provider)?;
    if runtime.snapshot().meta.blocked.is_some()
        || runtime.snapshot().meta.verified_zone_tip != parent
    {
        return Ok(None);
    }
    let coordinate = match &result {
        Ok(block) => Some(block.zone),
        Err(failure) => failure.coordinate(),
    };
    if let Some(coordinate) = coordinate
        && provider.block_hash(coordinate.number)? != Some(coordinate.hash)
    {
        reconcile_canonical_head(runtime, store, &provider)?;
        return Ok(None);
    }
    Ok(Some(result))
}

/// Ensure the persisted checker identity matches the active node configuration.
fn validate_checkpoint_identity(
    config: &CheckerConfig,
    zone_chain_id: u64,
    identity: Identity,
) -> eyre::Result<()> {
    if identity.zone_chain_id != zone_chain_id
        || identity.zone_id != config.zone_id
        || identity.portal != config.portal_address
        || identity.creation_block != config.portal_creation_block_hash
    {
        eyre::bail!("checker checkpoint identity does not match the node configuration");
    }
    Ok(())
}

/// Handle one notification-stream result and apply its ExEx lifecycle outcome.
fn handle_notification<Node, P, E>(
    ctx: &mut ExExContext<Node>,
    runtime: &mut Runtime,
    store: &Persistence,
    l2_state_provider: &P,
    next: Result<Option<ExExNotification<TempoPrimitives>>, E>,
) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    P: BlockNumReader + ?Sized,
    E: core::fmt::Display,
{
    let notification = match next {
        Ok(Some(notification)) => notification,
        Ok(None) => eyre::bail!("checker notification stream closed"),
        Err(error) => {
            tracing::error!(target: "zone::checker", %error, "checker notification stream failed; resuming direct notification delivery");
            ctx.set_notifications_without_head();
            refresh_canonical_head(runtime, store, l2_state_provider)?;
            return Ok(());
        }
    };
    if let Err(error) = validate_notification(&notification) {
        tracing::error!(target: "zone::checker", message = %error.message, "checker received an invalid notification");
        runtime.block(store, CheckerBlockedReason::InvalidNotificationSequence)?;
        return Ok(());
    }
    refresh_canonical_head(runtime, store, l2_state_provider)?;
    Ok(())
}

/// Validate the internal continuity of one ExEx notification.
fn validate_notification(notification: &ExExNotification<TempoPrimitives>) -> Result<(), Failure> {
    match notification {
        ExExNotification::ChainCommitted { new } => {
            validate_fragment(new, "committed")?;
        }
        ExExNotification::ChainReverted { old } => {
            validate_fragment(old, "reverted")?;
        }
        ExExNotification::ChainReorged { old, new } => {
            let reverted_parent = validate_fragment(old, "reverted")?;
            let applied_parent = validate_fragment(new, "replacement")?;
            if reverted_parent != applied_parent {
                return Err(Failure::terminal(
                    "reorg fragments have different common ancestors",
                ));
            }
        }
    }
    Ok(())
}

/// Validate a contiguous chain fragment and return its parent.
fn validate_fragment(chain: &Chain<TempoPrimitives>, kind: &str) -> Result<BlockNumHash, Failure> {
    let first = chain
        .blocks()
        .values()
        .next()
        .ok_or_else(|| Failure::terminal(format!("empty {kind} fragment")))?;
    let parent = BlockNumHash {
        number: first
            .number()
            .checked_sub(1)
            .ok_or_else(|| Failure::terminal("fragment starts at genesis"))?,
        hash: first.parent_hash(),
    };
    let mut previous: Option<BlockNumHash> = None;
    for block in chain.blocks().values() {
        if let Some(previous) = previous
            && (previous.number.checked_add(1) != Some(block.number())
                || block.parent_hash() != previous.hash)
        {
            return Err(Failure::terminal(format!(
                "{kind} fragment is not contiguous"
            )));
        }
        previous = Some(BlockNumHash {
            number: block.number(),
            hash: block.hash(),
        });
    }
    Ok(parent)
}

/// Acquire a canonical Zone block directly from the local node's retained history.
async fn authenticate_canonical_zone_block<P, S>(
    l2_provider: &S,
    height: u64,
    l1_provider: &P,
    parent_state: Arc<State>,
    portal_creation_block_hash: B256,
    zone_id: u32,
) -> Result<AuthenticatedBlock, AuthenticationFailure>
where
    P: Provider<TempoNetwork>,
    S: BlockReader<Block = Block, Receipt = TempoReceipt> + StateProviderFactory + ?Sized,
{
    let block = l2_provider
        .recovered_block(height.into(), TransactionVariant::WithHash)
        .map_err(|error| AuthenticationFailure::unlocated(Failure::retry(error.to_string())))?
        .ok_or_else(|| {
            AuthenticationFailure::unlocated(Failure::retry(format!(
                "canonical Zone block {height} is unavailable"
            )))
        })?;
    let zone = BlockNumHash {
        number: block.number(),
        hash: block.hash(),
    };
    let parent = BlockNumHash {
        number: block.number().checked_sub(1).ok_or_else(|| {
            AuthenticationFailure::at(
                zone,
                zone,
                Failure::terminal("cannot recover Zone genesis as a child block"),
            )
        })?,
        hash: block.parent_hash(),
    };
    let canonical = l2_provider.block_hash(height).map_err(|error| {
        AuthenticationFailure::at(zone, parent, Failure::retry(error.to_string()))
    })?;
    if canonical != Some(zone.hash) {
        return Err(AuthenticationFailure::at(
            zone,
            parent,
            Failure::retry("Zone block changed during checker acquisition"),
        ));
    }
    let receipts = l2_provider
        .receipts_by_block(zone.hash.into())
        .map_err(|error| {
            AuthenticationFailure::at(zone, parent, Failure::retry(error.to_string()))
        })?
        .ok_or_else(|| {
            AuthenticationFailure::at(
                zone,
                parent,
                Failure::retry("local Zone receipt set is unavailable"),
            )
        })?;
    let observation = observe_l2_block_with_context(&block, &receipts).map_err(|failure| {
        AuthenticationFailure::at(zone, parent, Failure::from(failure.into_parts().0))
    })?;
    authenticate_zone_observation(
        l1_provider,
        l2_provider,
        parent_state,
        observation,
        portal_creation_block_hash,
        zone_id,
    )
    .await
    .map_err(|failure| AuthenticationFailure::at(zone, parent, failure))
}

/// Authenticate a Zone observation against its imported Tempo block and post-state.
async fn authenticate_zone_observation<P, S>(
    l1_provider: &P,
    l2_state_provider: &S,
    parent_state: Arc<State>,
    l2_observation: L2BlockObservation,
    portal_creation_block_hash: B256,
    zone_id: u32,
) -> Result<AuthenticatedBlock, Failure>
where
    P: Provider<TempoNetwork>,
    S: StateProviderFactory + ?Sized,
{
    let imported_l1_header = l2_observation.inputs().advance_tempo().imported_header();
    let portal_address = parent_state
        .portal()
        .map(|portal| portal.identity().portal)
        .ok_or_else(|| Failure::terminal("checker state has no portal identity"))?;
    let l1_observations = observe_l1_range(
        l1_provider,
        core::slice::from_ref(imported_l1_header),
        portal_address,
    )
    .await
    .map_err(Failure::from)?;

    let l2_post_state = acquire_l2_post_state(l2_state_provider, &parent_state, &l2_observation)?;

    let authenticated_block = adapt(&AuthenticatedObservation {
        l2: l2_observation,
        l1: l1_observations,
        state: l2_post_state,
        portal_creation_block_hash,
        zone_id,
    })?;
    verify_l1_collateral(
        l1_provider,
        &parent_state,
        &authenticated_block,
        portal_address,
    )
    .await?;
    Ok(authenticated_block)
}

/// Read the post-block Zone state needed to verify the observation.
fn acquire_l2_post_state<S>(
    l2_state_provider: &S,
    parent_state: &State,
    l2_observation: &L2BlockObservation,
) -> Result<ZonePostStateOutputs, Failure>
where
    S: StateProviderFactory + ?Sized,
{
    // Include tokens enabled by this import in the post-block supply reads.
    let mut tokens_to_query = parent_state
        .tokens()
        .filter_map(|(token, state)| (state.phase == TokenPhase::ZoneEnabled).then_some(token))
        .collect::<BTreeSet<_>>();
    tokens_to_query.extend(
        l2_observation
            .inputs()
            .advance_tempo()
            .enabled_tokens()
            .iter()
            .map(|token| token.token),
    );
    let supply_tokens = tokens_to_query.into_iter().collect::<Vec<_>>();
    acquire_zone_post_state(
        l2_state_provider,
        l2_observation.block_hash(),
        &supply_tokens,
    )
    .map_err(Failure::from)
}
/// Verify portal collateral at the authenticated imported Tempo block.
async fn verify_l1_collateral<P>(
    l1_provider: &P,
    parent_state: &State,
    authenticated_block: &AuthenticatedBlock,
    portal_address: Address,
) -> Result<(), Failure>
where
    P: Provider<TempoNetwork>,
{
    let post_l1_import_state = apply_imported(parent_state, &authenticated_block.imported)
        .map_err(|error| {
            Failure::authenticated_divergence(
                error.to_string(),
                crate::kernel::Finding::coded(
                    crate::kernel::FindingCategory::Invariant,
                    2,
                    crate::kernel::FindingLocation::Block,
                ),
            )
        })?;
    // Collateral belongs to the exact post-import/pre-Zone cut. Zone
    // processing may burn or mint and therefore cannot select this set.
    let expected_l1_accounting = post_l1_import_state
        .expected_accounting()
        .map_err(|error| {
            Failure::authenticated_divergence(
                error.to_string(),
                crate::kernel::Finding::coded(
                    crate::kernel::FindingCategory::CollateralMismatch,
                    3,
                    crate::kernel::FindingLocation::Block,
                ),
            )
        })?;
    for (token, accounting) in expected_l1_accounting {
        let collateral_balance = acquire_portal_token_balance(
            l1_provider,
            token,
            portal_address,
            authenticated_block.tempo.hash,
        )
        .await
        .map_err(Failure::from)?;
        let required = accounting.collateral().unwrap_or(U256::ZERO);
        if collateral_balance < required {
            return Err(Failure::authenticated_divergence(
                "imported collateral is insufficient",
                crate::kernel::Finding {
                    category: crate::kernel::FindingCategory::CollateralMismatch,
                    code: 4,
                    location: Some(crate::kernel::FindingLocation::State(
                        crate::kernel::StateKey::Token(token),
                    )),
                    expected: Some(crate::kernel::Datum::U256(required)),
                    actual: Some(crate::kernel::Datum::U256(collateral_balance)),
                },
            ));
        }
    }
    Ok(())
}
