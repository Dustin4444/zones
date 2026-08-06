//! Canonical Zone block observation from one in-process Reth notification.

use alloy_consensus::{BlockHeader as _, Transaction as _, transaction::TxHashRef as _};
use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256};
use reth_primitives_traits::RecoveredBlock;
use tempo_primitives::{Block, TempoTxEnvelope};

use crate::model::{
    constants::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS},
    events::{L2ProtocolEvent, classify_l2_protocol_event},
};

use super::{
    abi::{DecodedAdvanceTempo, DecodedFinalization, decode_advance_tempo, decode_finalization},
    error::{
        AcquisitionError, AcquisitionSource, AuthenticatedTransaction, EnvelopeRule,
        ObservationError, ProtocolChain,
    },
};

#[cfg(test)]
use super::abi::ImportedTempoHeader;

/// Canonical coordinates retained for every supported protocol log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct L2EventPosition {
    transaction_index: usize,
    receipt_log_index: usize,
    block_log_index: usize,
    transaction_hash: B256,
    transaction_sender: Address,
}

impl L2EventPosition {
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

    pub(crate) fn transaction_sender(&self) -> Address {
        self.transaction_sender
    }
}

/// One strictly decoded implementation outcome in canonical block order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OrderedL2Outcome {
    position: L2EventPosition,
    event: L2ProtocolEvent,
}

impl OrderedL2Outcome {
    pub(crate) fn position(&self) -> L2EventPosition {
        self.position
    }

    pub(crate) fn event(&self) -> &L2ProtocolEvent {
        &self.event
    }
}

/// Inputs authenticated by the canonical transaction envelopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct L2AuthenticatedInputs {
    advance_transaction_hash: B256,
    advance_tempo: DecodedAdvanceTempo,
    finalization: Option<FinalizationEnvelope>,
}

impl L2AuthenticatedInputs {
    pub(crate) fn advance_transaction_hash(&self) -> B256 {
        self.advance_transaction_hash
    }

    pub(crate) fn advance_tempo(&self) -> &DecodedAdvanceTempo {
        &self.advance_tempo
    }

    pub(crate) fn finalization(&self) -> Option<&FinalizationEnvelope> {
        self.finalization.as_ref()
    }
}

/// Final system-call input and its containing transaction identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FinalizationEnvelope {
    transaction_hash: B256,
    input: DecodedFinalization,
}

impl FinalizationEnvelope {
    pub(crate) fn transaction_hash(&self) -> B256 {
        self.transaction_hash
    }

    pub(crate) fn input(&self) -> &DecodedFinalization {
        &self.input
    }
}

/// Outputs authenticated by successful receipts.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct L2AuthenticatedOutcomes {
    events: Vec<OrderedL2Outcome>,
}

impl L2AuthenticatedOutcomes {
    pub(crate) fn events(&self) -> &[OrderedL2Outcome] {
        &self.events
    }
}

/// Complete ephemeral observation of one non-genesis Zone block.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct L2BlockObservation {
    block_number: u64,
    block_hash: B256,
    parent_hash: B256,
    inputs: L2AuthenticatedInputs,
    outcomes: L2AuthenticatedOutcomes,
}

impl L2BlockObservation {
    pub(crate) fn block_number(&self) -> u64 {
        self.block_number
    }

    pub(crate) fn block_hash(&self) -> B256 {
        self.block_hash
    }

    pub(crate) fn parent_hash(&self) -> B256 {
        self.parent_hash
    }

    pub(crate) fn inputs(&self) -> &L2AuthenticatedInputs {
        &self.inputs
    }

    pub(crate) fn outcomes(&self) -> &L2AuthenticatedOutcomes {
        &self.outcomes
    }

    /// Exact Tempo block authenticated by this Zone block's `advanceTempo`
    /// envelope.
    pub(crate) fn imported_tempo(&self) -> BlockNumHash {
        let imported = self.inputs.advance_tempo().imported_header();
        BlockNumHash::new(imported.number(), imported.hash())
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        block_number: u64,
        block_hash: B256,
        parent_hash: B256,
        advance_transaction_hash: B256,
        imported_header: ImportedTempoHeader,
        events: Vec<L2ProtocolEvent>,
    ) -> Self {
        let advance_tempo = DecodedAdvanceTempo::empty_for_test(imported_header);
        let events = events
            .into_iter()
            .enumerate()
            .map(|(log_index, event)| OrderedL2Outcome {
                position: L2EventPosition {
                    transaction_index: 0,
                    receipt_log_index: log_index,
                    block_log_index: log_index,
                    transaction_hash: advance_transaction_hash,
                    transaction_sender: Address::ZERO,
                },
                event,
            })
            .collect();
        Self {
            block_number,
            block_hash,
            parent_hash,
            inputs: L2AuthenticatedInputs {
                advance_transaction_hash,
                advance_tempo,
                finalization: None,
            },
            outcomes: L2AuthenticatedOutcomes { events },
        }
    }
}

/// Observation failure plus the imported Tempo coordinate once `advanceTempo`
/// has been authenticated. Later Zone-envelope and event failures must retain
/// that coordinate for durable diagnostics.
#[derive(Debug)]
pub(crate) struct L2ObservationFailure {
    error: Box<ObservationError>,
    imported_tempo: Option<BlockNumHash>,
}

impl L2ObservationFailure {
    pub(crate) fn into_parts(self) -> (ObservationError, Option<BlockNumHash>) {
        (*self.error, self.imported_tempo)
    }

    fn with_imported_tempo(error: ObservationError, imported_tempo: BlockNumHash) -> Self {
        Self {
            error: Box::new(error),
            imported_tempo: Some(imported_tempo),
        }
    }
}

impl From<ObservationError> for L2ObservationFailure {
    fn from(error: ObservationError) -> Self {
        Self {
            error: Box::new(error),
            imported_tempo: None,
        }
    }
}

/// Observe one canonical non-genesis Zone block.
///
/// Transactions, recovered senders, and receipts all come from the same Reth
/// notification. This layer authenticates transaction envelopes and strictly
/// decodes protocol logs; the evaluator compares those independent inputs and
/// outputs against model expectations.
pub(crate) fn observe_l2_block<R>(
    block: &RecoveredBlock<Block>,
    receipts: &[R],
) -> Result<L2BlockObservation, ObservationError>
where
    R: alloy_consensus::TxReceipt<Log = alloy_primitives::Log>,
{
    observe_l2_block_with_context(block, receipts).map_err(|failure| failure.into_parts().0)
}

/// Observe one Zone block while retaining how far authenticated envelope
/// decoding progressed if it fails.
pub(crate) fn observe_l2_block_with_context<R>(
    block: &RecoveredBlock<Block>,
    receipts: &[R],
) -> Result<L2BlockObservation, L2ObservationFailure>
where
    R: alloy_consensus::TxReceipt<Log = alloy_primitives::Log>,
{
    let block_number = block.header().number();
    let block_hash = block.hash();
    let parent_hash = block.header().parent_hash();
    if block_number == 0 {
        return Err(ObservationError::invalid_block_envelope(EnvelopeRule::NonGenesis).into());
    }

    let transactions = &block.body().transactions;
    let senders = block.senders();
    if transactions.len() != receipts.len() {
        return Err(ObservationError::from(AcquisitionError::inconsistent(
            AcquisitionSource::ZoneNotificationReceipts,
            transactions.len(),
            receipts.len(),
        ))
        .into());
    }
    if transactions.len() != senders.len() {
        return Err(ObservationError::from(AcquisitionError::inconsistent(
            AcquisitionSource::ZoneNotificationBlock,
            transactions.len(),
            senders.len(),
        ))
        .into());
    }

    let first = transactions
        .first()
        .ok_or_else(|| ObservationError::invalid_block_envelope(EnvelopeRule::AdvancePresent))?;
    if !first.is_system_tx() || senders[0] != Address::ZERO {
        return Err(
            ObservationError::invalid_envelope(0, EnvelopeRule::AdvanceSystemCaller).into(),
        );
    }
    if first.to() != Some(ZONE_INBOX_ADDRESS) {
        return Err(ObservationError::invalid_envelope(0, EnvelopeRule::AdvanceDestination).into());
    }
    if !receipts[0].status() {
        return Err(ObservationError::invalid_envelope(0, EnvelopeRule::AdvanceSuccess).into());
    }
    let advance_coordinate =
        AuthenticatedTransaction::new(ProtocolChain::ZoneL2, 0, *first.tx_hash());
    let advance_tempo = decode_advance_tempo(first.input(), advance_coordinate)?;
    let imported_header = advance_tempo.imported_header();
    let imported_tempo = BlockNumHash::new(imported_header.number(), imported_header.hash());

    let finish = || -> Result<L2BlockObservation, ObservationError> {
        let mut finalization = None;
        for (index, ((transaction, sender), receipt)) in transactions
            .iter()
            .zip(senders)
            .zip(receipts)
            .enumerate()
            .skip(1)
        {
            if !transaction.is_system_tx() && *sender != Address::ZERO {
                continue;
            }
            if !transaction.is_system_tx() || *sender != Address::ZERO {
                return Err(ObservationError::invalid_envelope(
                    index,
                    EnvelopeRule::SystemIdentity,
                ));
            }
            if index + 1 != transactions.len() {
                return Err(ObservationError::invalid_envelope(
                    index,
                    EnvelopeRule::FinalizationPosition,
                ));
            }
            if transaction.to() != Some(ZONE_OUTBOX_ADDRESS) {
                return Err(ObservationError::invalid_envelope(
                    index,
                    EnvelopeRule::FinalizationDestination,
                ));
            }
            if !receipt.status() {
                return Err(ObservationError::invalid_envelope(
                    index,
                    EnvelopeRule::FinalizationSuccess,
                ));
            }
            let coordinate =
                AuthenticatedTransaction::new(ProtocolChain::ZoneL2, index, *transaction.tx_hash());
            let input = decode_finalization(transaction.input(), coordinate)?;
            if input.block_number() != block_number {
                return Err(ObservationError::invalid_envelope(
                    index,
                    EnvelopeRule::FinalizationBlockNumber,
                ));
            }
            finalization = Some(FinalizationEnvelope {
                transaction_hash: *transaction.tx_hash(),
                input,
            });
        }

        let events = ordered_l2_events(transactions, senders, receipts)?;

        Ok(L2BlockObservation {
            block_number,
            block_hash,
            parent_hash,
            inputs: L2AuthenticatedInputs {
                advance_transaction_hash: *first.tx_hash(),
                advance_tempo,
                finalization,
            },
            outcomes: L2AuthenticatedOutcomes { events },
        })
    };
    finish().map_err(|error| L2ObservationFailure::with_imported_tempo(error, imported_tempo))
}

fn ordered_l2_events<R>(
    transactions: &[TempoTxEnvelope],
    senders: &[Address],
    receipts: &[R],
) -> Result<Vec<OrderedL2Outcome>, ObservationError>
where
    R: alloy_consensus::TxReceipt<Log = alloy_primitives::Log>,
{
    let mut outcomes = Vec::new();
    let mut block_log_index = 0usize;
    for (transaction_index, ((transaction, sender), receipt)) in
        transactions.iter().zip(senders).zip(receipts).enumerate()
    {
        if !receipt.status() {
            continue;
        }
        for (receipt_log_index, log) in receipt.logs().iter().enumerate() {
            let position = L2EventPosition {
                transaction_index,
                receipt_log_index,
                block_log_index,
                transaction_hash: *transaction.tx_hash(),
                transaction_sender: *sender,
            };
            block_log_index += 1;
            if let Some(event) = classify_l2_protocol_event(log).map_err(|error| {
                ObservationError::protocol_event(
                    ProtocolChain::ZoneL2,
                    transaction_index,
                    receipt_log_index,
                    position.block_log_index,
                    *transaction.tx_hash(),
                    error,
                )
            })? {
                outcomes.push(OrderedL2Outcome { position, event });
            }
        }
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests;
