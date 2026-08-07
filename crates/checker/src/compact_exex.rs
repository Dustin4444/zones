//! Internal Reth acquisition adapter for the compact runtime.
//!
//! Kept separate from the launched ExEx until the Milestone-6 cutover.

#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::{Duration, Instant},
};

use alloy_consensus::BlockHeader as _;
use alloy_primitives::Address;
use alloy_provider::{Provider, ProviderBuilder};
use futures::TryStreamExt;
use reth_chainspec::EthChainSpec as _;
use reth_execution_types::Chain;
use reth_exex::ExExHead;
use reth_exex::{ExExContext, ExExNotification};
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_storage_api::{BlockNumReader, BlockReader, StateProviderFactory};
use tempo_alloy::TempoNetwork;
use tempo_primitives::{Block, TempoPrimitives, TempoReceipt};
use zone_checker_kernel::{State, StateKey, StateValue, TokenPhase, apply_imported, apply_zone};

use crate::{
    CheckerConfig,
    compact::{
        AuthenticatedBlock, CompactRuntime, Failure, FailureClass, NotificationPlan,
        ObservationPipeline, PlannedNotification, RetryBudget, RuntimeAction,
        compare_authenticated,
    },
    compact_observe::{PreauthenticatedObservation, adapt},
    observe::{
        AcquisitionError, ExactStateLookup, ObservationError, acquire_portal_collateral,
        acquire_zone_post_state, observe_l1, observe_l2_block_with_context,
    },
    persistence::{BlockNumHash, CoverageGapReason, Identity, Persistence},
};

fn observation_failure(error: ObservationError) -> Failure {
    let class = match &error {
        ObservationError::Acquisition(AcquisitionError::Unavailable { .. }) => {
            FailureClass::TransientRetry
        }
        ObservationError::Acquisition(
            AcquisitionError::Missing { .. } | AcquisitionError::Inconsistent { .. },
        ) => FailureClass::BoundedRetry,
        ObservationError::InvalidEnvelope { .. }
        | ObservationError::MalformedAuthenticatedData { .. } => {
            FailureClass::AuthenticatedDivergence
        }
        ObservationError::ProtocolEvent { .. } | ObservationError::PortalCall(_) => {
            FailureClass::AuthenticatedDivergence
        }
    };
    Failure {
        class,
        gap_reason: match class {
            FailureClass::TransientRetry => CoverageGapReason::ProviderUnavailable,
            FailureClass::BoundedRetry => CoverageGapReason::MissingTempoData,
            FailureClass::AuthenticatedDivergence => {
                CoverageGapReason::NotCheckedAncestorDivergence
            }
            FailureClass::ImmediateTerminal => CoverageGapReason::Other(2),
        },
        message: error.to_string(),
        finding: (class == FailureClass::AuthenticatedDivergence).then_some(Box::new(
            zone_checker_kernel::Finding {
                category: zone_checker_kernel::ViolationCategory::Observation,
                code: 1,
                location: Some(zone_checker_kernel::FindingLocation::Block),
                expected: None,
                actual: Some(zone_checker_kernel::FindingData::Code(1)),
            },
        )),
    }
}

fn malformed(message: impl Into<String>) -> Failure {
    Failure {
        class: FailureClass::ImmediateTerminal,
        gap_reason: CoverageGapReason::Other(2),
        message: message.into(),
        finding: None,
    }
}

fn validate(chain: &Chain<TempoPrimitives>, kind: &str) -> Result<Vec<BlockNumHash>, Failure> {
    let mut out: Vec<BlockNumHash> = Vec::with_capacity(chain.len());
    for block in chain.blocks().values() {
        let coordinate = BlockNumHash {
            number: block.number(),
            hash: block.hash(),
        };
        if let Some(previous) = out.last()
            && (previous.number.checked_add(1) != Some(coordinate.number)
                || block.parent_hash() != previous.hash)
        {
            return Err(malformed(format!("{kind} fragment is not contiguous")));
        }
        out.push(coordinate);
    }
    if out.is_empty() {
        return Err(malformed(format!("empty {kind} fragment")));
    }
    Ok(out)
}

impl PlannedNotification for ExExNotification<TempoPrimitives> {
    fn plan(&self) -> Result<NotificationPlan, Failure> {
        match self {
            Self::ChainCommitted { new } => plan(None, Some(new)),
            Self::ChainReverted { old } => plan(Some(old), None),
            Self::ChainReorged { old, new } => {
                validate(old, "reverted")?;
                validate(new, "replacement")?;
                let old_first = old.blocks().values().next().expect("validated nonempty");
                let new_first = new.blocks().values().next().expect("validated nonempty");
                if old_first.number() != new_first.number()
                    || old_first.parent_hash() != new_first.parent_hash()
                {
                    return Err(malformed("reorg fragments have different common ancestors"));
                }
                plan(Some(old), Some(new))
            }
        }
    }
}

fn plan(
    old: Option<&Chain<TempoPrimitives>>,
    new: Option<&Chain<TempoPrimitives>>,
) -> Result<NotificationPlan, Failure> {
    let reverted = old
        .map(|c| validate(c, "reverted"))
        .transpose()?
        .unwrap_or_default();
    let applied = new
        .map(|c| validate(c, "applied"))
        .transpose()?
        .unwrap_or_default();
    let first = new
        .or(old)
        .and_then(|c| c.blocks().values().next())
        .ok_or_else(|| malformed("empty notification"))?;
    let ancestor = BlockNumHash {
        number: first
            .number()
            .checked_sub(1)
            .ok_or_else(|| malformed("fragment starts at genesis"))?,
        hash: first.parent_hash(),
    };
    let acknowledge = applied.last().copied().unwrap_or(ancestor);
    NotificationPlan {
        reverted,
        ancestor,
        applied,
        acknowledge,
    }
    .validate()
}

pub(crate) struct NotificationFragments<'a> {
    pub reverted: Option<&'a Chain<TempoPrimitives>>,
    pub ancestor: BlockNumHash,
    pub applied: Option<&'a Chain<TempoPrimitives>>,
}

pub(crate) fn fragments(
    notification: &ExExNotification<TempoPrimitives>,
) -> Result<NotificationFragments<'_>, Failure> {
    let (reverted, applied, chain, kind) = match notification {
        ExExNotification::ChainCommitted { new } => (None, Some(new), new, "committed"),
        ExExNotification::ChainReverted { old } => (Some(old), None, old, "reverted"),
        ExExNotification::ChainReorged { old, new } => {
            notification.plan()?;
            (Some(old), Some(new), new, "replacement")
        }
    };
    validate(chain, kind)?;
    let first = chain.blocks().values().next().expect("validated nonempty");
    let number = first
        .number()
        .checked_sub(1)
        .ok_or_else(|| malformed("fragment starts at genesis"))?;
    Ok(NotificationFragments {
        reverted: reverted.map(|chain| chain.as_ref()),
        ancestor: BlockNumHash {
            number,
            hash: first.parent_hash(),
        },
        applied: applied.map(|chain| chain.as_ref()),
    })
}

fn enabled_tokens(state: &State) -> BTreeSet<Address> {
    state
        .rows()
        .iter()
        .filter_map(|(key, value)| match (key, value) {
            (StateKey::Token(token), StateValue::Token(token_state))
                if token_state.phase == TokenPhase::ZoneEnabled =>
            {
                Some(*token)
            }
            _ => None,
        })
        .collect()
}

/// Authenticates a replacement/commit oldest-first and advances only an
/// in-memory compact candidate. Persistence remains the caller's commit point.
pub(crate) async fn acquire_applied_at<P, S>(
    chain: &Chain<TempoPrimitives>,
    index: usize,
    l1_provider: &P,
    zone_state: &S,
    parent: State,
    portal_creation_block_hash: alloy_primitives::B256,
    zone_id: u32,
) -> Result<AuthenticatedBlock, Failure>
where
    P: Provider<TempoNetwork>,
    S: ExactStateLookup + ?Sized,
{
    validate(chain, "applied")?;
    let block = chain
        .blocks()
        .values()
        .nth(index)
        .ok_or_else(|| malformed("applied block index is out of bounds"))?;
    let receipts: Vec<TempoReceipt> = chain
        .receipts_by_block_hash(block.hash())
        .ok_or_else(|| Failure {
            class: FailureClass::BoundedRetry,
            gap_reason: CoverageGapReason::MissingReceipts,
            message: "notification is missing receipt set".into(),
            finding: None,
        })?
        .into_iter()
        .cloned()
        .collect();
    let l2 = observe_l2_block_with_context(block.as_ref(), &receipts)
        .map_err(|failure| observation_failure(failure.into_parts().0))?;
    let imported_header = l2.inputs().advance_tempo().imported_header();
    let l1 = observe_l1(
        l1_provider,
        imported_header,
        parent
            .rows()
            .values()
            .find_map(|v| {
                if let StateValue::Portal(p) = v {
                    Some(p.identity().portal)
                } else {
                    None
                }
            })
            .ok_or_else(|| malformed("compact state has no portal identity"))?,
    )
    .await
    .map_err(observation_failure)?;

    // The exact supply set is durable enabled tokens plus tokens enabled by
    // this authenticated advance. No legacy model helper participates.
    let mut supply_tokens = enabled_tokens(&parent);
    supply_tokens.extend(
        l2.inputs()
            .advance_tempo()
            .enabled_tokens()
            .iter()
            .map(|t| t.token),
    );
    let supplies = supply_tokens.into_iter().collect::<Vec<_>>();
    let state = acquire_zone_post_state(zone_state, block.hash(), &supplies)
        .map_err(|error| observation_failure(error.into()))?;

    let l1 = l1.into_observation();
    let observation = PreauthenticatedObservation {
        l2,
        l1,
        state,
        collateral: BTreeMap::new(),
        portal_creation_block_hash,
        zone_id,
    };
    let mut result = adapt(&observation)?;
    let imported_candidate =
        apply_imported(&parent, &result.imported).map_err(|error| Failure {
            class: FailureClass::AuthenticatedDivergence,
            gap_reason: CoverageGapReason::NotCheckedAncestorDivergence,
            message: error.to_string(),
            finding: Some(Box::new(kernel_failure(
                2,
                zone_checker_kernel::ViolationCategory::Invariant,
            ))),
        })?;
    // Collateral belongs to the exact post-import/pre-Zone cut. Zone
    // processing may burn or mint and therefore cannot select this set.
    let imported_accounting =
        imported_candidate
            .expected_accounting()
            .map_err(|error| Failure {
                class: FailureClass::AuthenticatedDivergence,
                gap_reason: CoverageGapReason::NotCheckedAncestorDivergence,
                message: error.to_string(),
                finding: Some(Box::new(kernel_failure(
                    3,
                    zone_checker_kernel::ViolationCategory::CollateralMismatch,
                ))),
            })?;
    let mut collateral = BTreeMap::new();
    for (token, accounting) in imported_accounting {
        let balance = acquire_portal_collateral(
            l1_provider,
            token,
            observation.l1.portal_address(),
            observation.l1.block_hash(),
        )
        .await
        .map_err(|error| observation_failure(error.into()))?;
        if accounting
            .collateral()
            .is_none_or(|required| balance < required)
        {
            return Err(Failure {
                class: FailureClass::AuthenticatedDivergence,
                gap_reason: CoverageGapReason::NotCheckedAncestorDivergence,
                message: "imported-cut collateral is insufficient".into(),
                finding: Some(Box::new(kernel_failure(
                    4,
                    zone_checker_kernel::ViolationCategory::CollateralMismatch,
                ))),
            });
        }
        collateral.insert(token, balance);
    }
    let candidate =
        apply_zone(imported_candidate, &result.zone_facts).map_err(|error| Failure {
            class: FailureClass::AuthenticatedDivergence,
            gap_reason: CoverageGapReason::NotCheckedAncestorDivergence,
            message: error.to_string(),
            finding: Some(Box::new(kernel_failure(
                5,
                zone_checker_kernel::ViolationCategory::Invariant,
            ))),
        })?;
    result.outputs.collateral = collateral;
    compare_authenticated(&result, &candidate)?;
    Ok(result)
}

fn kernel_failure(
    code: u16,
    category: zone_checker_kernel::ViolationCategory,
) -> zone_checker_kernel::Finding {
    zone_checker_kernel::Finding {
        category,
        code,
        location: Some(zone_checker_kernel::FindingLocation::Block),
        expected: None,
        actual: Some(zone_checker_kernel::FindingData::Code(code)),
    }
}

struct PreparedPipeline {
    block: Option<(usize, Result<AuthenticatedBlock, Failure>)>,
}

impl ObservationPipeline<ExExNotification<TempoPrimitives>> for PreparedPipeline {
    fn authenticate_at(
        &mut self,
        _notification: &ExExNotification<TempoPrimitives>,
        index: usize,
        _parent_state: &State,
    ) -> Result<AuthenticatedBlock, Failure> {
        self.block
            .take()
            .filter(|(prepared_index, _)| *prepared_index == index)
            .map(|(_, block)| block)
            .unwrap_or_else(|| {
                Err(Failure {
                    class: FailureClass::TransientRetry,
                    gap_reason: CoverageGapReason::ProviderUnavailable,
                    message: "block has not yet been acquired".into(),
                    finding: None,
                })
            })
    }

    fn compare(
        &mut self,
        block: &AuthenticatedBlock,
        expected: &zone_checker_kernel::Candidate,
    ) -> Result<(), Failure> {
        compare_authenticated(block, expected)
    }
}

/// Construct the internal compact shadow worker. This deliberately is not
/// wired to [`crate::CheckerExEx::launch`].
pub(crate) fn launch_shadow<Node>(
    config: CheckerConfig,
    ctx: ExExContext<Node>,
) -> impl Future<Output = eyre::Result<()>> + Send
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    run(config, ctx)
}

/// Concrete one-loop compact ExEx runtime. It opens only an already complete
/// checkpoint and keeps the durable acknowledged watermark authoritative.
pub(crate) async fn run<Node>(config: CheckerConfig, mut ctx: ExExContext<Node>) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider: BlockReader<Block = Block> + BlockNumReader + StateProviderFactory,
    Node::Types: NodeTypes<Primitives = TempoPrimitives>,
{
    let path = config.database_path.as_deref().ok_or_else(|| {
        eyre::eyre!("compact shadow runtime requires an explicit checkpoint path")
    })?;
    let identity = Persistence::inspect_identity(path)?;
    validate_runtime_identity(&config, ctx.config.chain.chain().id(), identity)?;
    let (store, snapshot) = Persistence::open(path, identity)?;
    let l1 = ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect(&config.l1_rpc_url)
        .await?
        .erased();
    let actual_l1 = l1.get_chain_id().await?;
    if actual_l1 != identity.l1_chain_id {
        eyre::bail!("compact checkpoint L1 chain ID is incompatible");
    }

    // A failed send leaves this watermark durable; a restart reaches this
    // exact send before consuming another notification.
    ctx.send_finished_height(num_hash(snapshot.meta.acknowledged_zone_tip))?;
    // The acknowledgement watermark is resent exactly, while catch-up starts
    // at the verified cut so a durable gap can be reconstructed and closed.
    ctx.catch_up_notifications_with_head(ExExHead::new(num_hash(snapshot.meta.verified_zone_tip)))?;

    let mut runtime = CompactRuntime::new(32, RetryBudget::new(20, Duration::from_secs(30)));
    let mut prepared = PreparedPipeline { block: None };
    let mut retry_at: Option<Instant> = None;

    loop {
        if let Some(index) = runtime.next_applied_index(&store, identity)?
            && prepared.block.is_none()
        {
            let parts = fragments(
                runtime
                    .current()
                    .expect("applied index requires a current notification"),
            )
            .map_err(|f| eyre::eyre!(f.message))?;
            let chain = parts
                .applied
                .ok_or_else(|| eyre::eyre!("missing applied fragment"))?;
            let chain = chain.clone();
            let provider = ctx.provider().clone();
            let parent = store.load(identity)?.state;
            let acquisition = acquire_applied_at(
                &chain,
                index,
                &l1,
                &provider,
                parent,
                config.portal_creation_block_hash,
                config.zone_id,
            );
            tokio::pin!(acquisition);
            let timeout = tokio::time::sleep(Duration::from_secs(2));
            tokio::pin!(timeout);
            let acquired = loop {
                tokio::select! {
                    result = &mut acquisition => break result,
                    () = &mut timeout => {
                        break Err(Failure {
                            class: FailureClass::TransientRetry,
                            gap_reason: CoverageGapReason::ProviderUnavailable,
                            message: "authenticated acquisition attempt timed out".into(),
                            finding: None,
                        });
                    }
                    next = ctx.notifications.try_next() => match next {
                        Ok(Some(notification)) => match runtime.push_or_record_overflow(
                            &store,
                            identity,
                            notification,
                        )? {
                            RuntimeAction::None => {}
                            RuntimeAction::Acknowledge(height) => {
                                ctx.send_finished_height(num_hash(height))?;
                                eyre::bail!("compact runtime disabled after bounded FIFO overflow");
                            }
                            RuntimeAction::AcknowledgeAndTerminate(height) => {
                                ctx.send_finished_height(num_hash(height))?;
                                eyre::bail!("compact runtime disabled after bounded FIFO overflow");
                            }
                            RuntimeAction::Terminal => {
                                eyre::bail!("compact runtime rejected notification during acquisition");
                            }
                            RuntimeAction::AwaitNotification | RuntimeAction::RetryAt(_) => {
                                unreachable!("enqueue cannot request driver waiting")
                            }
                        },
                        Ok(None) | Err(_) => {
                            record_local_canonical_suffix(
                                &mut runtime,
                                &store,
                                identity,
                                &provider,
                            )?;
                            eyre::bail!("compact notification stream unavailable");
                        }
                    }
                }
            };
            prepared.block = Some((index, acquired));
        }

        match runtime.poll(&store, identity, &mut prepared, Instant::now())? {
            RuntimeAction::Acknowledge(height) => {
                ctx.send_finished_height(num_hash(height))?;
                prepared.block = None;
                retry_at = None;
                continue;
            }
            RuntimeAction::AcknowledgeAndTerminate(height) => {
                ctx.send_finished_height(num_hash(height))?;
                eyre::bail!("compact runtime disabled after durable terminal acknowledgement");
            }
            RuntimeAction::RetryAt(deadline) => retry_at = Some(deadline),
            RuntimeAction::Terminal => eyre::bail!("compact shadow runtime disabled"),
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
        let Some(next) = next else {
            prepared.block = None;
            continue;
        };
        match next {
            Ok(Some(notification)) => {
                match runtime.push_or_record_overflow(&store, identity, notification)? {
                    RuntimeAction::Acknowledge(height) => {
                        ctx.send_finished_height(num_hash(height))?;
                    }
                    RuntimeAction::AcknowledgeAndTerminate(height) => {
                        ctx.send_finished_height(num_hash(height))?;
                        eyre::bail!("compact runtime disabled after bounded FIFO overflow");
                    }
                    RuntimeAction::Terminal => eyre::bail!("compact runtime disabled"),
                    RuntimeAction::None => {}
                    RuntimeAction::AwaitNotification | RuntimeAction::RetryAt(_) => {
                        unreachable!("enqueue cannot request driver waiting")
                    }
                }
            }
            Ok(None) | Err(_) => {
                record_local_canonical_suffix(&mut runtime, &store, identity, ctx.provider())?;
                eyre::bail!("compact notification stream unavailable");
            }
        }
    }
}

fn validate_runtime_identity(
    config: &CheckerConfig,
    zone_chain_id: u64,
    identity: Identity,
) -> eyre::Result<()> {
    if identity.zone_chain_id != zone_chain_id
        || identity.zone_id != config.zone_id
        || identity.portal != config.portal_address
        || identity.creation_block != config.portal_creation_block_hash
    {
        eyre::bail!("compact checkpoint identity is incompatible");
    }
    Ok(())
}

const fn num_hash(value: BlockNumHash) -> alloy_eips::BlockNumHash {
    alloy_eips::BlockNumHash::new(value.number, value.hash)
}

fn record_local_canonical_suffix<P: BlockNumReader + ?Sized>(
    runtime: &mut CompactRuntime<ExExNotification<TempoPrimitives>>,
    store: &Persistence,
    identity: Identity,
    provider: &P,
) -> eyre::Result<()> {
    let tip = store.load(identity)?.meta.verified_zone_tip;
    let head = provider.best_block_number()?;
    if head <= tip.number {
        eyre::bail!("notification stream failed without a reconstructable unchecked suffix");
    }
    let mut suffix = Vec::with_capacity((head - tip.number) as usize);
    for number in tip.number + 1..=head {
        let hash = provider
            .block_hash(number)?
            .ok_or_else(|| eyre::eyre!("canonical block {number} unavailable"))?;
        suffix.push(BlockNumHash { number, hash });
    }
    if let RuntimeAction::AcknowledgeAndTerminate(_) =
        runtime.record_stream_failure(store, identity, &suffix)?
    {
        // Stream failure has no functioning acknowledgement path. The durable
        // watermark is intentionally retained for startup resend.
        return Ok(());
    }
    eyre::bail!("could not persist truthful canonical stream gap")
}
