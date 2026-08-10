use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use crate::kernel::{State, StateKey, StateValue, TokenPhase, apply_imported};
use alloy_consensus::BlockHeader as _;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder};
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
    observe::{
        AcquisitionError, ExactStateLookup, ObservationError, acquire_portal_collateral,
        acquire_zone_post_state, observe_l1_range, observe_l2_block_with_context,
    },
    persistence::{BlockNumHash, CoverageGapReason, Identity, Persistence},
    runtime::{
        AuthenticatedBlock, Failure, FailureClass, NotificationPlan, ObservationPipeline,
        PlannedNotification, RetryBudget, Runtime, RuntimeAction,
    },
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
    let finding = match &error {
        ObservationError::MalformedAuthenticatedData {
            transaction,
            evidence,
            ..
        } => Some(Box::new(crate::kernel::Finding {
            category: crate::kernel::FindingCategory::Observation,
            code: 110,
            location: Some(crate::kernel::FindingLocation::Operation(
                transaction.transaction_index() as u32,
            )),
            expected: None,
            actual: Some(crate::kernel::Datum::Bytes {
                length: evidence.length(),
                digest: evidence.digest(),
            }),
        })),
        ObservationError::InvalidEnvelope { .. } => Some(Box::new(kernel_failure(
            120,
            crate::kernel::FindingCategory::Observation,
        ))),
        ObservationError::ProtocolEvent {
            transaction_index, ..
        } => Some(Box::new(crate::kernel::Finding {
            category: crate::kernel::FindingCategory::Observation,
            code: 130,
            location: Some(crate::kernel::FindingLocation::Operation(
                *transaction_index as u32,
            )),
            expected: None,
            actual: Some(crate::kernel::Datum::Code(130)),
        })),
        ObservationError::PortalCall(_) => Some(Box::new(kernel_failure(
            140,
            crate::kernel::FindingCategory::Observation,
        ))),
        ObservationError::Acquisition(_) => None,
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
        finding,
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
            Self::ChainCommitted { new } => finish_plan(
                Vec::new(),
                validate(new, "committed")?,
                fragment_ancestor(new)?,
            ),
            Self::ChainReverted { old } => finish_plan(
                validate(old, "reverted")?,
                Vec::new(),
                fragment_ancestor(old)?,
            ),
            Self::ChainReorged { old, new } => {
                let reverted = validate(old, "reverted")?;
                let applied = validate(new, "replacement")?;
                let old_first = old.blocks().values().next().expect("validated nonempty");
                let new_first = new.blocks().values().next().expect("validated nonempty");
                if old_first.number() != new_first.number()
                    || old_first.parent_hash() != new_first.parent_hash()
                {
                    return Err(malformed("reorg fragments have different common ancestors"));
                }
                finish_plan(reverted, applied, fragment_ancestor(old)?)
            }
        }
    }
}

fn fragment_ancestor(chain: &Chain<TempoPrimitives>) -> Result<BlockNumHash, Failure> {
    let first = chain.blocks().values().next().expect("validated nonempty");
    Ok(BlockNumHash {
        number: first
            .number()
            .checked_sub(1)
            .ok_or_else(|| malformed("fragment starts at genesis"))?,
        hash: first.parent_hash(),
    })
}

fn finish_plan(
    reverted: Vec<BlockNumHash>,
    applied: Vec<BlockNumHash>,
    ancestor: BlockNumHash,
) -> Result<NotificationPlan, Failure> {
    let acknowledge = applied.last().copied().unwrap_or(ancestor);
    NotificationPlan {
        reverted,
        ancestor,
        applied,
        acknowledge,
    }
    .validate()
}

fn applied_chain(
    notification: &ExExNotification<TempoPrimitives>,
) -> Option<&Chain<TempoPrimitives>> {
    match notification {
        ExExNotification::ChainCommitted { new } | ExExNotification::ChainReorged { new, .. } => {
            Some(new)
        }
        ExExNotification::ChainReverted { .. } => None,
    }
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
/// in-memory candidate. Persistence remains the caller's commit point.
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
    let imported_headers = l2.inputs().advance_tempo().imported_headers();
    let l1 = observe_l1_range(
        l1_provider,
        imported_headers,
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
            .ok_or_else(|| malformed("checker state has no portal identity"))?,
    )
    .await
    .map_err(observation_failure)?;

    // Include tokens enabled by this import in the post-block supply reads.
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

    let observation = AuthenticatedObservation {
        l2,
        l1,
        state,
        portal_creation_block_hash,
        zone_id,
    };
    let result = adapt(&observation)?;
    let imported_candidate =
        apply_imported(&parent, &result.imported).map_err(|error| Failure {
            class: FailureClass::AuthenticatedDivergence,
            gap_reason: CoverageGapReason::NotCheckedAncestorDivergence,
            message: error.to_string(),
            finding: Some(Box::new(kernel_failure(
                2,
                crate::kernel::FindingCategory::Invariant,
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
                    crate::kernel::FindingCategory::CollateralMismatch,
                ))),
            })?;
    let imported_tip = observation.l1.last().expect("nonempty imported range");
    for (token, accounting) in imported_accounting {
        let balance = acquire_portal_collateral(
            l1_provider,
            token,
            imported_tip.portal_address(),
            imported_tip.block_hash(),
        )
        .await
        .map_err(|error| observation_failure(error.into()))?;
        if accounting
            .collateral()
            .is_none_or(|required| balance < required)
        {
            let required = accounting.collateral().unwrap_or(U256::ZERO);
            return Err(Failure {
                class: FailureClass::AuthenticatedDivergence,
                gap_reason: CoverageGapReason::NotCheckedAncestorDivergence,
                message: "imported collateral is insufficient".into(),
                finding: Some(Box::new(crate::kernel::Finding {
                    category: crate::kernel::FindingCategory::CollateralMismatch,
                    code: 4,
                    location: Some(crate::kernel::FindingLocation::State(
                        crate::kernel::StateKey::Token(token),
                    )),
                    expected: Some(crate::kernel::Datum::U256(required)),
                    actual: Some(crate::kernel::Datum::U256(balance)),
                })),
            });
        }
    }
    Ok(result)
}

fn kernel_failure(code: u16, category: crate::kernel::FindingCategory) -> crate::kernel::Finding {
    crate::kernel::Finding {
        category,
        code,
        location: Some(crate::kernel::FindingLocation::Block),
        expected: None,
        actual: Some(crate::kernel::Datum::Code(code)),
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
}

pub(crate) async fn run<Node>(config: CheckerConfig, mut ctx: ExExContext<Node>) -> eyre::Result<()>
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
    validate_runtime_identity(&config, ctx.config.chain.chain().id(), identity)?;
    let (store, snapshot) = Persistence::open(path, identity)?;
    let l1 = ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect(&config.l1_rpc_url)
        .await?
        .erased();
    let actual_l1 = l1.get_chain_id().await?;
    if actual_l1 != identity.l1_chain_id {
        eyre::bail!("Tempo chain ID does not match the checker checkpoint");
    }

    // Resend the persisted acknowledgement, then catch up from the verified tip.
    ctx.send_finished_height(num_hash(snapshot.meta.acknowledged_zone_tip))?;
    ctx.catch_up_notifications_with_head(ExExHead::new(num_hash(snapshot.meta.verified_zone_tip)))?;

    let mut runtime = Runtime::new(snapshot, 32, RetryBudget::new(20, Duration::from_secs(30)));
    let mut prepared = PreparedPipeline { block: None };
    let mut retry_at: Option<Instant> = None;

    loop {
        if let Some(index) = runtime.next_applied_index(&store)?
            && prepared.block.is_none()
        {
            let (notification, plan) = runtime
                .current()
                .expect("applied index requires a current notification");
            debug_assert!(!plan.applied.is_empty());
            let chain = applied_chain(notification)
                .ok_or_else(|| eyre::eyre!("missing applied fragment"))?;
            let chain = chain.clone();
            let provider = ctx.provider().clone();
            let parent = runtime.snapshot().state.clone();
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
            let timeout = tokio::time::sleep(config.acquisition_timeout);
            tokio::pin!(timeout);
            let acquired = loop {
                tokio::select! {
                    result = &mut acquisition => break result,
                    () = &mut timeout => {
                        break Err(Failure {
                            class: FailureClass::TransientRetry,
                            gap_reason: CoverageGapReason::ProviderUnavailable,
                            message: "checker acquisition timed out".into(),
                            finding: None,
                        });
                    }
                    next = ctx.notifications.try_next() => match next {
                        Ok(Some(notification)) => match runtime.push_or_record_overflow(
                            &store,
                            notification,
                        )? {
                            RuntimeAction::None => {}
                            RuntimeAction::AcknowledgeAndTerminate(height) => {
                                ctx.send_finished_height(num_hash(height))?;
                                eyre::bail!("checker stopped after recording a queue overflow gap");
                            }
                            RuntimeAction::Terminal => {
                                eyre::bail!("checker rejected notification during acquisition");
                            }
                            RuntimeAction::Acknowledge(_)
                            | RuntimeAction::AwaitNotification
                            | RuntimeAction::RetryAt(_) => unreachable!("invalid enqueue action"),
                        },
                        Ok(None) | Err(_) => {
                            record_local_canonical_suffix(
                                &mut runtime,
                                &store,
                                &provider,
                            )?;
                            eyre::bail!("checker notification stream unavailable");
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
        let Some(next) = next else {
            prepared.block = None;
            continue;
        };
        match next {
            Ok(Some(notification)) => {
                match runtime.push_or_record_overflow(&store, notification)? {
                    RuntimeAction::AcknowledgeAndTerminate(height) => {
                        ctx.send_finished_height(num_hash(height))?;
                        eyre::bail!("checker stopped after recording a queue overflow gap");
                    }
                    RuntimeAction::Terminal => {
                        eyre::bail!("checker stopped");
                    }
                    RuntimeAction::None => {}
                    RuntimeAction::Acknowledge(_)
                    | RuntimeAction::AwaitNotification
                    | RuntimeAction::RetryAt(_) => unreachable!("invalid enqueue action"),
                }
            }
            Ok(None) | Err(_) => {
                record_local_canonical_suffix(&mut runtime, &store, ctx.provider())?;
                eyre::bail!("checker notification stream unavailable");
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
        eyre::bail!("checker checkpoint identity does not match the node configuration");
    }
    Ok(())
}

const fn num_hash(value: BlockNumHash) -> alloy_eips::BlockNumHash {
    alloy_eips::BlockNumHash::new(value.number, value.hash)
}

fn record_local_canonical_suffix<P: BlockNumReader + ?Sized>(
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
    if let RuntimeAction::AcknowledgeAndTerminate(_) =
        runtime.record_stream_failure(store, &suffix)?
    {
        // The stream cannot accept an acknowledgement. Startup will resend the
        // durable watermark.
        return Ok(());
    }
    Err(eyre::eyre!("failed to record canonical stream gap"))
}
