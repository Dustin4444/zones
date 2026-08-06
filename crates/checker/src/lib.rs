//! Zone checker observation runtime and staged in-memory evaluator.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

#[allow(dead_code)]
mod check;
#[allow(dead_code)]
mod metrics;
#[allow(dead_code)]
mod model;
mod observe;
#[allow(dead_code)]
mod runtime;
#[allow(dead_code)]
mod store;

use std::{fmt, str::FromStr};

use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use alloy_primitives::Address;
use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
use futures::TryStreamExt as _;
use reth_execution_types::Chain;
use reth_exex::{ExExContext, ExExNotification};
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_primitives_traits::RecoveredBlock;
use reth_storage_api::StateProviderFactory;
use tempo_alloy::TempoNetwork;
use tempo_primitives::{Block, TempoPrimitives, TempoReceipt};
use tracing::{error, info};

use observe::{
    AcquisitionError, AcquisitionSource, ExactStateLookup, L1BlockObservation, L2BlockObservation,
    ObservationError, ZonePostStateOutputs, acquire_zone_post_state, observe_l1, observe_l2_block,
};

/// Runtime mode for the checker ExEx.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckerMode {
    /// Checker is not installed.
    #[default]
    Off,
    /// Checker authenticates ephemeral observations but does not enforce or
    /// persist model findings.
    Observe,
}

impl fmt::Display for CheckerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Off => "off",
            Self::Observe => "observe",
        })
    }
}

impl FromStr for CheckerMode {
    type Err = eyre::Report;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "observe" => Ok(Self::Observe),
            other => Err(eyre::eyre!(
                "unsupported checker mode `{other}`, expected `off` or `observe`"
            )),
        }
    }
}

impl CheckerMode {
    /// Parse a mode without coupling this crate to clap.
    pub fn parse(value: &str) -> Result<Self, eyre::Report> {
        value.parse()
    }
}

/// Authenticated-observation ExEx configuration.
pub struct CheckerExEx {
    l1_rpc_url: String,
    portal_address: Address,
}

/// Sticky acknowledgement state for the first incomplete observation gap.
///
/// Goal 1 has no durable retry loop, so a later successful notification must
/// never advance Reth's pruning watermark past an earlier unobserved block.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum AcknowledgementState {
    #[default]
    Open,
    Blocked,
}

impl AcknowledgementState {
    fn record(
        &mut self,
        observation: Result<BlockNumHash, ObservationError>,
    ) -> Result<Option<BlockNumHash>, ObservationError> {
        match observation {
            Err(error) => {
                *self = Self::Blocked;
                Err(error)
            }
            Ok(_) if *self == Self::Blocked => Ok(None),
            Ok(tip) => Ok(Some(tip)),
        }
    }
}

impl CheckerExEx {
    pub fn new(l1_rpc_url: String, portal_address: Address) -> Self {
        Self {
            l1_rpc_url,
            portal_address,
        }
    }

    /// Run until the notification stream closes.
    ///
    /// An observation failure keeps the pruning watermark behind the gap but
    /// does not terminate the Zone node. Retry and durable model state belong
    /// to later design goals.
    pub async fn run<Node>(self, mut ctx: ExExContext<Node>) -> eyre::Result<()>
    where
        Node: FullNodeComponents,
        Node::Provider: StateProviderFactory,
        Node::Types: NodeTypes<Primitives = TempoPrimitives>,
    {
        info!(target: "zone::checker", "Checker ExEx started");
        let zone_provider = ctx.provider().clone();
        let mut l1_provider: Option<DynProvider<TempoNetwork>> = None;
        let mut acknowledgements = AcknowledgementState::default();

        while let Some(notification) = ctx.notifications.try_next().await? {
            let observation = process_notification(
                &notification,
                &zone_provider,
                &mut l1_provider,
                &self.l1_rpc_url,
                self.portal_address,
            )
            .await;
            match acknowledgements.record(observation) {
                Ok(Some(tip)) => ctx.send_finished_height(tip)?,
                Ok(None) => {}
                Err(error) => {
                    error!(target: "zone::checker", %error, "authenticated observation failed");
                }
            }
        }

        info!(target: "zone::checker", "Checker ExEx notification stream closed");
        Ok(())
    }
}

async fn process_notification<P>(
    notification: &ExExNotification<TempoPrimitives>,
    zone_provider: &P,
    l1_provider: &mut Option<DynProvider<TempoNetwork>>,
    l1_rpc_url: &str,
    portal_address: Address,
) -> Result<BlockNumHash, ObservationError>
where
    P: ExactStateLookup + ?Sized,
{
    match notification {
        ExExNotification::ChainCommitted { new } => {
            observe_chain(new, zone_provider, l1_provider, l1_rpc_url, portal_address).await?;
            let tip = new.tip();
            Ok(BlockNumHash::new(tip.header().number(), tip.hash()))
        }
        ExExNotification::ChainReverted { old } => Ok(log_reverted_chain(old, "Reverted")),
        ExExNotification::ChainReorged { old, new } => {
            log_reverted_chain(old, "Reorged-out");
            observe_chain(new, zone_provider, l1_provider, l1_rpc_url, portal_address).await?;
            let tip = new.tip();
            Ok(BlockNumHash::new(tip.header().number(), tip.hash()))
        }
    }
}

async fn observe_chain<P>(
    chain: &Chain<TempoPrimitives>,
    zone_provider: &P,
    l1_provider: &mut Option<DynProvider<TempoNetwork>>,
    l1_rpc_url: &str,
    portal_address: Address,
) -> Result<(), ObservationError>
where
    P: ExactStateLookup + ?Sized,
{
    let receipt_sets = chain.block_receipts_iter().count();
    validate_notification_receipt_sets(chain.blocks().len(), receipt_sets)?;
    for (block, receipts) in chain.blocks_and_receipts() {
        if let Err(error) = process_canonical_block(
            zone_provider,
            l1_provider,
            l1_rpc_url,
            portal_address,
            block,
            receipts,
        )
        .await
        {
            error!(
                target: "zone::checker",
                number = block.header().number(),
                hash = %block.hash(),
                %error,
                "Canonical block observation failed"
            );
            return Err(error);
        }
        info!(
            target: "zone::checker",
            number = block.header().number(),
            hash = %block.hash(),
            parent_hash = %block.header().parent_hash(),
            "Canonical block observed"
        );
    }
    Ok(())
}

fn validate_notification_receipt_sets(
    block_count: usize,
    receipt_set_count: usize,
) -> Result<(), AcquisitionError> {
    if block_count != receipt_set_count {
        return Err(AcquisitionError::inconsistent(
            AcquisitionSource::ZoneNotificationReceipts,
            format_args!("{block_count} block receipt sets"),
            format_args!("{receipt_set_count} block receipt sets"),
        ));
    }
    Ok(())
}

fn log_reverted_chain(chain: &Chain<TempoPrimitives>, kind: &'static str) -> BlockNumHash {
    for (&number, block) in chain.blocks().iter().rev() {
        info!(
            target: "zone::checker",
            number,
            hash = %block.hash(),
            parent_hash = %block.header().parent_hash(),
            kind,
            "Noncanonical block observed"
        );
    }
    let (&lowest_number, lowest_block) = chain.blocks().iter().next().expect("non-empty chain");
    BlockNumHash::new(
        lowest_number.saturating_sub(1),
        lowest_block.header().parent_hash(),
    )
}

async fn process_canonical_block<P>(
    zone_provider: &P,
    l1_provider: &mut Option<DynProvider<TempoNetwork>>,
    l1_rpc_url: &str,
    portal_address: Address,
    block: &RecoveredBlock<Block>,
    receipts: &[TempoReceipt],
) -> Result<(), ObservationError>
where
    P: ExactStateLookup + ?Sized,
{
    let l2 = observe_l2_block(block, receipts)?;
    // Goal 1 keeps exact-hash fixed-state acquisition on the wired observe-only
    // path. The complete enabled-token set belongs to the Goal 5 model, so the
    // diagnostic runtime intentionally requests no supply slots yet.
    let post_state = acquire_zone_post_state(zone_provider, block.hash(), &[])?;

    if l1_provider.is_none() {
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect(l1_rpc_url)
            .await
            .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Rpc, error))?
            .erased();
        *l1_provider = Some(provider);
    }
    let l1_provider = l1_provider
        .as_ref()
        .expect("L1 provider initialized immediately above");
    let l1 = observe_l1(
        l1_provider,
        l2.inputs().advance_tempo().imported_header(),
        portal_address,
    )
    .await?;

    log_observation(&l2, &l1, &post_state);
    Ok(())
}

fn log_observation(
    l2: &L2BlockObservation,
    l1: &L1BlockObservation,
    post_state: &ZonePostStateOutputs,
) {
    let l2_inputs = l2.inputs();
    let advance = l2_inputs.advance_tempo();
    let finalization = l2_inputs.finalization();
    let l2_outputs = l2.outcomes();
    let l1_transactions = l1.protocol_transactions();
    let l1_outcomes = l1_transactions
        .iter()
        .map(|transaction| transaction.outcomes().len())
        .sum::<usize>();
    let submit_batch_calls = l1_transactions
        .iter()
        .filter_map(|transaction| transaction.direct_call())
        .filter(|call| call.as_submit_batch().is_some())
        .count();
    let process_withdrawal_calls = l1_transactions
        .iter()
        .filter_map(|transaction| transaction.direct_call())
        .filter(|call| call.as_process_withdrawals().is_some())
        .count();
    let ordinary_deposits = advance
        .deposits()
        .iter()
        .filter(|deposit| deposit.as_ordinary().is_some())
        .count();
    let withdrawal_bounce_backs = advance
        .deposits()
        .iter()
        .filter(|deposit| deposit.as_withdrawal_bounce_back().is_some())
        .count();
    info!(
        target: "zone::checker",
        zone_block_number = l2.block_number(),
        zone_block_hash = %l2.block_hash(),
        tempo_block_number = l1.block_number(),
        tempo_block_hash = %l1.block_hash(),
        advance_transaction_hash = %l2_inputs.advance_transaction_hash(),
        l2_outcomes = l2_outputs.events().len(),
        l1_outcomes,
        submit_batch_calls,
        process_withdrawal_calls,
        ordinary_deposits,
        withdrawal_bounce_backs,
        decryptions = advance.decryptions().len(),
        enabled_tokens = advance.enabled_tokens().len(),
        has_finalization = finalization.is_some(),
        finalized_withdrawals = finalization.map(|value| value.input().count()).unwrap_or(0),
        encrypted_senders = finalization
            .map(|value| value.input().encrypted_senders().len())
            .unwrap_or(0),
        finalization_transaction_hash = ?finalization.map(|value| value.transaction_hash()),
        exact_supply_count = post_state.token_supplies().len(),
        exact_tempo_hash = %post_state.tempo_block_hash(),
        exact_tempo_number = post_state.tempo_block_number(),
        exact_deposit_hash = %post_state.processed_deposit_queue_hash(),
        exact_deposit_number = post_state.processed_deposit_number(),
        exact_withdrawal_hash = %post_state.withdrawal_queue_hash(),
        exact_withdrawal_index = post_state.withdrawal_batch_index(),
        "Authenticated block observation complete"
    );
}

#[cfg(test)]
mod tests;
