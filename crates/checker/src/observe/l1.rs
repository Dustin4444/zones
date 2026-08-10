//! Authenticated Tempo L1 observation adapter.
//!
//! The imported header is selected exclusively by `advanceTempo` calldata on
//! L2. This adapter authenticates the complete ordered receipt stream against
//! that header, authenticates every transaction envelope against the header's
//! transaction root, and decodes needed Portal calldata from those envelopes.

use alloy_primitives::{Address, B256};
use alloy_provider::Provider;
use tempo_alloy::TempoNetwork;

use crate::observe::events::L1ProtocolEvent;

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

/// One strictly decoded implementation outcome in canonical block order.
#[derive(Debug)]
pub(crate) struct OrderedL1Outcome {
    event: L1ProtocolEvent,
}

impl OrderedL1Outcome {
    pub(crate) fn event(&self) -> &L1ProtocolEvent {
        &self.event
    }
}

/// Authenticated outcomes and any directly decoded Portal input for one
/// transaction. `direct_call` is absent when no transition needs calldata.
#[derive(Debug)]
pub(crate) struct L1TransactionObservation {
    direct_call: Option<DecodedPortalCall>,
    outcomes: Vec<OrderedL1Outcome>,
}

impl L1TransactionObservation {
    pub(crate) fn direct_call(&self) -> Option<&DecodedPortalCall> {
        self.direct_call.as_ref()
    }

    pub(crate) fn outcomes(&self) -> &[OrderedL1Outcome] {
        &self.outcomes
    }
}

/// Complete ephemeral observation of the exact Tempo block imported by L2.
#[derive(Debug)]
pub(crate) struct L1BlockObservation {
    block_number: u64,
    block_hash: B256,
    portal_address: Address,
    protocol_transactions: Vec<L1TransactionObservation>,
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
) -> Result<L1BlockObservation, ObservationError>
where
    P: Provider<TempoNetwork>,
{
    let block = authentication::acquire_block(provider, imported).await?;
    let receipts = authentication::acquire_receipts(provider, imported, &block).await?;
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
            direct_call,
            outcomes: transaction.outcomes,
        });
    }

    Ok(L1BlockObservation {
        block_number: imported.number(),
        block_hash: imported.hash(),
        portal_address: portal,
        protocol_transactions,
    })
}

/// Authenticate every imported Tempo block independently, preserving import order.
pub(crate) async fn observe_l1_range<P>(
    provider: &P,
    imported: &[ImportedTempoHeader],
    portal: Address,
) -> Result<Vec<L1BlockObservation>, ObservationError>
where
    P: Provider<TempoNetwork>,
{
    let mut observations = Vec::with_capacity(imported.len());
    for header in imported {
        observations.push(observe_l1(provider, header, portal).await?);
    }
    Ok(observations)
}
