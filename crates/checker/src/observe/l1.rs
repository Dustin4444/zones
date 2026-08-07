//! Authenticated Tempo L1 observation adapter.
//!
//! The imported header is selected exclusively by `advanceTempo` calldata on
//! L2. This adapter authenticates the complete ordered receipt stream against
//! that header, authenticates every transaction envelope against the header's
//! transaction root, and decodes needed Portal calldata from those envelopes.

use std::time::Duration;

use alloy_primitives::{Address, B256};
use alloy_provider::Provider;
use tempo_alloy::TempoNetwork;

use crate::model::events::L1ProtocolEvent;

use super::{
    abi::{DecodedPortalCall, ImportedTempoHeader},
    error::ObservationError,
};

mod authentication;
mod calls;
mod collateral;
mod events;

pub(crate) use authentication::acquire_l1_header;
pub(crate) use collateral::acquire_portal_collateral;

#[cfg(test)]
mod tests;

/// Canonical coordinates retained for every model-driving L1 log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct L1EventPosition {
    transaction_index: usize,
    receipt_log_index: usize,
    block_log_index: usize,
    transaction_hash: B256,
}

impl L1EventPosition {
    pub(crate) fn transaction_index(&self) -> usize {
        self.transaction_index
    }

    pub(crate) fn receipt_log_index(&self) -> usize {
        self.receipt_log_index
    }

    pub(crate) fn block_log_index(&self) -> usize {
        self.block_log_index
    }

    pub(crate) fn transaction_hash(&self) -> B256 {
        self.transaction_hash
    }
}

/// One strictly decoded implementation outcome in canonical block order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OrderedL1Outcome {
    position: L1EventPosition,
    event: L1ProtocolEvent,
}

impl OrderedL1Outcome {
    pub(crate) fn position(&self) -> L1EventPosition {
        self.position
    }

    pub(crate) fn event(&self) -> &L1ProtocolEvent {
        &self.event
    }
}

/// Authenticated outcomes and any directly decoded Portal input for
/// one transaction. `direct_call` is absent when no model rule needs calldata.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct L1TransactionObservation {
    transaction_index: usize,
    transaction_hash: B256,
    direct_call: Option<DecodedPortalCall>,
    outcomes: Vec<OrderedL1Outcome>,
}

impl L1TransactionObservation {
    pub(crate) fn transaction_index(&self) -> usize {
        self.transaction_index
    }

    pub(crate) fn transaction_hash(&self) -> B256 {
        self.transaction_hash
    }

    pub(crate) fn direct_call(&self) -> Option<&DecodedPortalCall> {
        self.direct_call.as_ref()
    }

    pub(crate) fn outcomes(&self) -> &[OrderedL1Outcome] {
        &self.outcomes
    }
}

/// Complete ephemeral observation of the exact Tempo block imported by L2.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct L1BlockObservation {
    block_number: u64,
    block_hash: B256,
    portal_address: Address,
    protocol_transactions: Vec<L1TransactionObservation>,
}

/// Authenticated L1 block data paired with acquisition-only measurements.
///
/// Timings are deliberately kept outside [`L1BlockObservation`]: wall-clock
/// metadata is neither authenticated nor part of observation equality.
#[derive(Debug)]
pub(crate) struct L1BlockAcquisition {
    observation: L1BlockObservation,
    receipt_fetch_duration: Duration,
}

impl L1BlockAcquisition {
    pub(crate) const fn observation(&self) -> &L1BlockObservation {
        &self.observation
    }

    pub(crate) const fn receipt_fetch_duration(&self) -> Duration {
        self.receipt_fetch_duration
    }

    pub(crate) fn into_observation(self) -> L1BlockObservation {
        self.observation
    }
}

impl L1BlockObservation {
    pub(crate) fn block_number(&self) -> u64 {
        self.block_number
    }

    pub(crate) fn block_hash(&self) -> B256 {
        self.block_hash
    }

    pub(crate) fn portal_address(&self) -> Address {
        self.portal_address
    }

    pub(crate) fn protocol_transactions(&self) -> &[L1TransactionObservation] {
        &self.protocol_transactions
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        block_number: u64,
        block_hash: B256,
        portal_address: Address,
        transactions: Vec<(B256, Vec<L1ProtocolEvent>)>,
    ) -> Self {
        Self::with_calls_for_test(
            block_number,
            block_hash,
            portal_address,
            transactions
                .into_iter()
                .map(|(hash, events)| (hash, None, events))
                .collect(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_calls_for_test(
        block_number: u64,
        block_hash: B256,
        portal_address: Address,
        transactions: Vec<(B256, Option<DecodedPortalCall>, Vec<L1ProtocolEvent>)>,
    ) -> Self {
        let mut block_log_index = 0;
        let protocol_transactions = transactions
            .into_iter()
            .enumerate()
            .map(
                |(transaction_index, (transaction_hash, direct_call, events))| {
                    let outcomes = events
                        .into_iter()
                        .enumerate()
                        .map(|(receipt_log_index, event)| {
                            let position = L1EventPosition {
                                transaction_index,
                                receipt_log_index,
                                block_log_index,
                                transaction_hash,
                            };
                            block_log_index += 1;
                            OrderedL1Outcome { position, event }
                        })
                        .collect();
                    L1TransactionObservation {
                        transaction_index,
                        transaction_hash,
                        direct_call,
                        outcomes,
                    }
                },
            )
            .collect();
        Self {
            block_number,
            block_hash,
            portal_address,
            protocol_transactions,
        }
    }
}

/// Observe the exact L1 block selected by the authenticated `advanceTempo`
/// header.
///
/// Full transaction-root, receipt-root, and bloom authentication completes
/// before any envelope or event is used semantically.
pub(crate) async fn observe_l1<P>(
    provider: &P,
    imported: &ImportedTempoHeader,
    portal: Address,
) -> Result<L1BlockAcquisition, ObservationError>
where
    P: Provider<TempoNetwork>,
{
    let block = authentication::acquire_block(provider, imported).await?;
    let (receipts, receipt_fetch_duration) =
        authentication::acquire_receipts(provider, imported, &block).await?;
    let transaction_hashes = block
        .transactions
        .iter()
        .map(alloy_network::TransactionResponse::tx_hash)
        .collect::<Vec<_>>();
    let pending = events::ordered_transactions(portal, &transaction_hashes, &receipts)?;

    let mut protocol_transactions = Vec::with_capacity(pending.len());
    for transaction in pending {
        let direct_call = match transaction.required_call {
            Some(required) => {
                let envelope: &tempo_primitives::TempoTxEnvelope =
                    block.transactions[transaction.transaction_index].as_ref();
                Some(calls::decode_direct_portal_call(
                    envelope,
                    portal,
                    transaction.transaction_index,
                    transaction.transaction_hash,
                    required,
                )?)
            }
            None => None,
        };
        protocol_transactions.push(L1TransactionObservation {
            transaction_index: transaction.transaction_index,
            transaction_hash: transaction.transaction_hash,
            direct_call,
            outcomes: transaction.outcomes,
        });
    }

    Ok(L1BlockAcquisition {
        observation: L1BlockObservation {
            block_number: imported.number(),
            block_hash: imported.hash(),
            portal_address: portal,
            protocol_transactions,
        },
        receipt_fetch_duration,
    })
}
