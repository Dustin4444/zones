//! Reth ExEx integration for acquiring, checking, and acknowledging Zone blocks.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use crate::kernel::{State, TokenPhase, apply_imported};
use alloy_consensus::BlockHeader as _;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder};
use eyre::WrapErr as _;
use futures::TryStreamExt;
use reth_chainspec::EthChainSpec as _;
use reth_execution_types::Chain;
use reth_exex::{ExExContext, ExExHead, ExExNotification};
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_storage_api::{BlockNumReader, BlockReader, StateProviderFactory};
use tempo_alloy::TempoNetwork;
use tempo_primitives::{Block, TempoPrimitives, TempoReceipt};

use crate::{
    CheckerConfig,
    adapter::{AuthenticatedObservation, adapt},
    failure::{Failure, FailureClass},
    notification::NotificationPlan,
    observe::{
        L2BlockObservation, ZonePostStateOutputs, acquire_portal_token_balance,
        acquire_zone_post_state, observe_l1_range, observe_l2_block_with_context,
    },
    persistence::{BlockNumHash, CoverageGapReason, Identity, Persistence},
    runtime::{
        AuthenticatedBlock, AuthenticationRequest, EnqueueAction, RetryBudget, Runtime,
        RuntimeAction, StreamFailureAction,
    },
};

/// A contiguous ExEx chain fragment and its common parent.
struct ChainFragment {
    coordinates: Vec<BlockNumHash>,
    parent: BlockNumHash,
}

/// Run the checker ExEx until the notification stream or runtime terminates.
pub(super) async fn run<Node>(config: CheckerConfig, mut ctx: ExExContext<Node>) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    eyre::ensure!(
        !config.acquisition_timeout.is_zero(),
        "checker acquisition timeout must not be zero"
    );
    let path = config.database_path.as_path();
    let identity = Persistence::inspect_identity(path)?;
    validate_checkpoint_identity(&config, ctx.config.chain.chain().id(), identity)?;
    let (store, snapshot) = Persistence::open(path, identity)?;
    let l1_provider = ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect(&config.l1_rpc_url)
        .await?
        .erased();
    let actual_l1_chain_id = l1_provider.get_chain_id().await?;
    if actual_l1_chain_id != identity.l1_chain_id {
        eyre::bail!("Tempo chain ID does not match the checker checkpoint");
    }

    // Resend the persisted acknowledgement, then catch up from the verified tip.
    ctx.send_finished_height(snapshot.meta.acknowledged_zone_tip.into())?;
    ctx.catch_up_notifications_with_head(ExExHead::new(snapshot.meta.verified_zone_tip.into()))?;

    let mut runtime = Runtime::new(snapshot, 32, RetryBudget::new(20, Duration::from_secs(30)));
    let mut authentication = None;
    let mut retry_at: Option<Instant> = None;

    loop {
        let action = match authentication.take() {
            Some((request, result)) => runtime.complete_authentication(
                &store,
                identity,
                request,
                result,
                Instant::now(),
            )?,
            None => runtime.next_action(&store, Instant::now())?,
        };
        match action {
            RuntimeAction::Authenticate(request) => {
                let result = authenticate_requested_block(
                    &config,
                    &mut ctx,
                    &store,
                    &mut runtime,
                    request,
                    &l1_provider,
                )
                .await?;
                authentication = Some((request, result));
                continue;
            }
            RuntimeAction::Acknowledge(height) => {
                ctx.send_finished_height(height.into())?;
                retry_at = None;
                continue;
            }
            RuntimeAction::AcknowledgeAndTerminate(height) => {
                ctx.send_finished_height(height.into())?;
                eyre::bail!("checker stopped after recording an unchecked range");
            }
            RuntimeAction::RetryAt(deadline) => retry_at = Some(deadline),
            RuntimeAction::Terminal => {
                eyre::bail!("checker stopped");
            }
            RuntimeAction::AwaitNotification => {}
            RuntimeAction::None if runtime.current().is_some() => continue,
            RuntimeAction::None => {}
        }

        let next = if let Some(deadline) = retry_at {
            tokio::select! {
                value = ctx.notifications.try_next() => Some(value),
                () = tokio::time::sleep_until(deadline.into()) => None,
            }
        } else {
            Some(ctx.notifications.try_next().await)
        };
        let Some(next) = next else { continue };
        let l2_state_provider = ctx.provider().clone();
        handle_notification(&mut ctx, &mut runtime, &store, &l2_state_provider, next)?;
    }
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

/// Authenticate a requested block while continuing to buffer notifications.
async fn authenticate_requested_block<Node, P>(
    config: &CheckerConfig,
    ctx: &mut ExExContext<Node>,
    store: &Persistence,
    runtime: &mut Runtime<ExExNotification<TempoPrimitives>>,
    request: AuthenticationRequest,
    l1_provider: &P,
) -> eyre::Result<Result<AuthenticatedBlock, Failure>>
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
    P: Provider<TempoNetwork>,
{
    let (notification, plan) = runtime
        .current()
        .ok_or_else(|| eyre::eyre!("applied index has no current notification"))?;
    debug_assert!(!plan.applied.is_empty());
    let l2_chain = match notification {
        ExExNotification::ChainCommitted { new } | ExExNotification::ChainReorged { new, .. } => {
            new.clone()
        }
        ExExNotification::ChainReverted { .. } => {
            eyre::bail!("applied plan has no applied fragment");
        }
    };
    let l2_state_provider = ctx.provider().clone();
    let parent_state = runtime.snapshot().state.clone();
    let acquisition = authenticate_applied_l2_block(
        &l2_chain,
        request.index(),
        l1_provider,
        &l2_state_provider,
        parent_state,
        config.portal_creation_block_hash,
        config.zone_id,
    );
    tokio::pin!(acquisition);
    let timeout = tokio::time::sleep(config.acquisition_timeout);
    tokio::pin!(timeout);
    let acquired = loop {
        tokio::select! {
            result = &mut acquisition => break result,
            () = &mut timeout => {
                break Err(Failure::transient("checker acquisition timed out"));
            }
            next = ctx.notifications.try_next() => {
                handle_notification(ctx, runtime, store, &l2_state_provider, next)?;
            }
        }
    };
    Ok(acquired)
}

/// Handle one notification-stream result and apply its ExEx lifecycle outcome.
fn handle_notification<Node, P, E>(
    ctx: &mut ExExContext<Node>,
    runtime: &mut Runtime<ExExNotification<TempoPrimitives>>,
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
        Ok(None) => {
            return handle_notification_stream_failure(runtime, store, l2_state_provider);
        }
        Err(error) => {
            return handle_notification_stream_failure(runtime, store, l2_state_provider)
                .wrap_err_with(|| format!("notification stream error: {error}"));
        }
    };
    let plan = notification_plan(&notification)
        .map_err(|_| eyre::eyre!("checker rejected notification"))?;
    match runtime.push_planned_or_record_overflow(store, notification, plan)? {
        EnqueueAction::Queued => Ok(()),
        EnqueueAction::AcknowledgeAndTerminate(height) => {
            ctx.send_finished_height(height.into())?;
            eyre::bail!("checker stopped after recording a queue overflow gap");
        }
        EnqueueAction::Terminal => Err(eyre::eyre!("checker rejected notification")),
    }
}

/// Persist the unchecked suffix and terminate after the notification stream fails.
fn handle_notification_stream_failure<P: BlockNumReader + ?Sized>(
    runtime: &mut Runtime<ExExNotification<TempoPrimitives>>,
    store: &Persistence,
    provider: &P,
) -> eyre::Result<()> {
    let tip = runtime.snapshot().meta.verified_zone_tip;
    let head = provider.best_block_number()?;
    if head <= tip.number {
        eyre::bail!("notification stream failed without a reconstructable unchecked suffix");
    }
    let mut suffix = Vec::with_capacity((head - tip.number) as usize);
    for number in tip.number + 1..=head {
        let hash = provider
            .block_hash(number)?
            .ok_or_else(|| eyre::eyre!("canonical Zone block {number} is unavailable"))?;
        suffix.push(BlockNumHash { number, hash });
    }
    match runtime.record_stream_failure(store, &suffix)? {
        StreamFailureAction::GapRecorded(_) => {
            // The durable watermark remains available for startup recovery.
            Err(eyre::eyre!("checker notification stream unavailable"))
        }
        StreamFailureAction::Terminal => Err(eyre::eyre!("failed to record canonical stream gap")),
    }
}
/// Convert a contiguous ExEx notification into a checked runtime plan.
fn notification_plan(
    notification: &ExExNotification<TempoPrimitives>,
) -> Result<NotificationPlan, Failure> {
    match notification {
        ExExNotification::ChainCommitted { new } => {
            let applied = validate_fragment(new, "committed")?;
            NotificationPlan::new(Vec::new(), applied.coordinates, applied.parent)
        }
        ExExNotification::ChainReverted { old } => {
            let reverted = validate_fragment(old, "reverted")?;
            NotificationPlan::new(reverted.coordinates, Vec::new(), reverted.parent)
        }
        ExExNotification::ChainReorged { old, new } => {
            let reverted = validate_fragment(old, "reverted")?;
            let applied = validate_fragment(new, "replacement")?;
            if reverted.parent != applied.parent {
                return Err(Failure::terminal(
                    "reorg fragments have different common ancestors",
                ));
            }
            NotificationPlan::new(reverted.coordinates, applied.coordinates, reverted.parent)
        }
    }
}

/// Validate a contiguous chain fragment and derive its coordinates and parent.
fn validate_fragment(chain: &Chain<TempoPrimitives>, kind: &str) -> Result<ChainFragment, Failure> {
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
    let mut coordinates: Vec<BlockNumHash> = Vec::with_capacity(chain.len());
    for block in chain.blocks().values() {
        // Each block must directly extend the preceding block in the fragment.
        if let Some(previous) = coordinates.last()
            && (previous.number.checked_add(1) != Some(block.number())
                || block.parent_hash() != previous.hash)
        {
            return Err(Failure::terminal(format!(
                "{kind} fragment is not contiguous"
            )));
        }
        coordinates.push(BlockNumHash {
            number: block.number(),
            hash: block.hash(),
        });
    }
    Ok(ChainFragment {
        coordinates,
        parent,
    })
}

/// Authenticate one applied L2 block against its L1 import and parent state.
async fn authenticate_applied_l2_block<P, S>(
    l2_chain: &Chain<TempoPrimitives>,
    applied_index: usize,
    l1_provider: &P,
    l2_state_provider: &S,
    parent_state: State,
    portal_creation_block_hash: alloy_primitives::B256,
    zone_id: u32,
) -> Result<AuthenticatedBlock, Failure>
where
    P: Provider<TempoNetwork>,
    S: StateProviderFactory + ?Sized,
{
    let l2_observation = observe_applied_l2_block(l2_chain, applied_index)?;
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

/// Observe one applied L2 block and its receipts.
fn observe_applied_l2_block(
    l2_chain: &Chain<TempoPrimitives>,
    applied_index: usize,
) -> Result<L2BlockObservation, Failure> {
    validate_fragment(l2_chain, "applied")?;
    let l2_block = l2_chain
        .blocks()
        .values()
        .nth(applied_index)
        .ok_or_else(|| Failure::terminal("applied block index is out of bounds"))?;
    let l2_receipts: Vec<TempoReceipt> = l2_chain
        .receipts_by_block_hash(l2_block.hash())
        .ok_or_else(|| Failure {
            class: FailureClass::BoundedRetry,
            gap_reason: CoverageGapReason::MissingReceipts,
            message: "notification is missing receipt set".into(),
            finding: None,
        })?
        .into_iter()
        .cloned()
        .collect();
    observe_l2_block_with_context(l2_block.as_ref(), &l2_receipts)
        .map_err(|failure| Failure::from(failure.into_parts().0))
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
        if accounting
            .collateral()
            .is_none_or(|required| collateral_balance < required)
        {
            let required = accounting.collateral().unwrap_or(U256::ZERO);
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
