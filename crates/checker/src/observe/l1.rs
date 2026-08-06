//! Authenticated Tempo L1 observation adapter.
//!
//! The imported header is selected exclusively by `advanceTempo` calldata on
//! L2. This adapter authenticates the complete ordered receipt stream against
//! that header, then selectively acquires only Portal transaction bodies whose
//! calldata is needed by the model.

use alloy_primitives::{Address, B256};
use alloy_provider::Provider;
use tempo_alloy::TempoNetwork;

use crate::model::events::L1ProtocolEvent;

use super::{
    abi::{DecodedPortalCall, ImportedTempoHeader},
    error::{AcquisitionError, AcquisitionSource, ObservationError},
};

mod authentication;
mod calls;
mod events;

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

// Goal 1 freezes this read-only adapter API before the pure model consumes it.
#[allow(dead_code)]
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

// Goal 1 freezes this read-only adapter API before the pure model consumes it.
#[allow(dead_code)]
impl OrderedL1Outcome {
    pub(crate) fn position(&self) -> L1EventPosition {
        self.position
    }

    pub(crate) fn event(&self) -> &L1ProtocolEvent {
        &self.event
    }
}

/// Authenticated outcomes and any selectively acquired direct Portal input for
/// one transaction. `direct_call` is absent when no model rule needs calldata.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct L1TransactionObservation {
    transaction_index: usize,
    transaction_hash: B256,
    direct_call: Option<DecodedPortalCall>,
    outcomes: Vec<OrderedL1Outcome>,
}

// Goal 1 freezes transaction identity before the pure model consumes it.
#[allow(dead_code)]
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
    protocol_transactions: Vec<L1TransactionObservation>,
}

impl L1BlockObservation {
    pub(crate) fn block_number(&self) -> u64 {
        self.block_number
    }

    pub(crate) fn block_hash(&self) -> B256 {
        self.block_hash
    }

    pub(crate) fn protocol_transactions(&self) -> &[L1TransactionObservation] {
        &self.protocol_transactions
    }
}

/// Observe the exact L1 block selected by the authenticated `advanceTempo`
/// header.
///
/// Receipt-root and bloom authentication completes before any event can cause
/// a transaction-body fetch. The release-one design deliberately trusts the
/// configured archive RPC to bind each selectively fetched body to the
/// authenticated block; it does not fetch every body or recompute the
/// transaction root.
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
    let pending = events::ordered_transactions(portal, &block.transaction_hashes, &receipts)?;

    let mut protocol_transactions = Vec::with_capacity(pending.len());
    for transaction in pending {
        let direct_call = match transaction.required_call {
            Some(required) => Some(
                calls::acquire_direct_portal_call(
                    provider,
                    portal,
                    imported,
                    transaction.transaction_index,
                    transaction.transaction_hash,
                    required,
                )
                .await?,
            ),
            None => None,
        };
        protocol_transactions.push(L1TransactionObservation {
            transaction_index: transaction.transaction_index,
            transaction_hash: transaction.transaction_hash,
            direct_call,
            outcomes: transaction.outcomes,
        });
    }

    Ok(L1BlockObservation {
        block_number: imported.number(),
        block_hash: imported.hash(),
        protocol_transactions,
    })
}

fn ensure_acquisition_equal<T>(
    source: AcquisitionSource,
    field: impl core::fmt::Display,
    expected: T,
    actual: T,
) -> Result<(), ObservationError>
where
    T: PartialEq + core::fmt::Debug,
{
    if expected != actual {
        return Err(AcquisitionError::inconsistent(
            source,
            format!("{field} {expected:?}"),
            format!("{field} {actual:?}"),
        )
        .into());
    }
    Ok(())
}
