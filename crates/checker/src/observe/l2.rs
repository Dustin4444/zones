//! Canonical Zone block observation from one in-process Reth notification.

use alloy_consensus::{BlockHeader as _, Transaction as _, transaction::TxHashRef as _};
use alloy_primitives::{Address, B256};
use reth_primitives_traits::RecoveredBlock;
use tempo_primitives::{Block, TempoTxEnvelope};

use crate::model::{
    constants::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS},
    events::{Inbox, L2ProtocolEvent, Outbox, TempoState, classify_l2_protocol_event},
};

use super::{
    abi::{DecodedAdvanceTempo, DecodedFinalization, decode_advance_tempo, decode_finalization},
    error::{EnvelopeRule, MismatchValue, ObservationError, OutputField, ProtocolChain},
    state::ZonePostStateOutputs,
};

/// Canonical coordinates retained for every supported protocol log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct L2EventPosition {
    transaction_index: usize,
    receipt_log_index: usize,
    block_log_index: usize,
    transaction_hash: B256,
}

// Goal 1 freezes this read-only adapter API before the pure model consumes it.
#[allow(dead_code)]
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
}

/// One strictly decoded implementation outcome in canonical block order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OrderedL2Outcome {
    position: L2EventPosition,
    event: L2ProtocolEvent,
}

// Goal 1 freezes this read-only adapter API before the pure model consumes it.
#[allow(dead_code)]
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

/// Outputs authenticated by successful receipts and exact post-block state.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct L2AuthenticatedOutcomes {
    events: Vec<OrderedL2Outcome>,
    post_state: ZonePostStateOutputs,
}

impl L2AuthenticatedOutcomes {
    pub(crate) fn events(&self) -> &[OrderedL2Outcome] {
        &self.events
    }

    pub(crate) fn post_state(&self) -> &ZonePostStateOutputs {
        &self.post_state
    }
}

/// Complete ephemeral observation of one non-genesis Zone block.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct L2BlockObservation {
    block_number: u64,
    block_hash: B256,
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

    pub(crate) fn inputs(&self) -> &L2AuthenticatedInputs {
        &self.inputs
    }

    pub(crate) fn outcomes(&self) -> &L2AuthenticatedOutcomes {
        &self.outcomes
    }
}

/// Observe one canonical non-genesis Zone block.
///
/// Transactions, recovered senders, and receipts all come from the same Reth
/// notification. `load_post_state` is invoked with the exact block hash only
/// after deterministic envelope, calldata, and protocol-event validation.
pub(crate) fn observe_l2_block<R, S>(
    block: &RecoveredBlock<Block>,
    receipts: &[R],
    load_post_state: S,
) -> Result<L2BlockObservation, ObservationError>
where
    R: alloy_consensus::TxReceipt<Log = alloy_primitives::Log>,
    S: FnOnce(B256) -> Result<ZonePostStateOutputs, ObservationError>,
{
    let block_number = block.header().number();
    let block_hash = block.hash();
    if block_number == 0 {
        return Err(ObservationError::invalid_block_envelope(
            EnvelopeRule::NonGenesis,
        ));
    }

    let transactions = &block.body().transactions;
    let senders = block.senders();
    if transactions.len() != receipts.len() {
        return Err(ObservationError::invalid_block_envelope(
            EnvelopeRule::TransactionReceiptCardinality,
        ));
    }
    if transactions.len() != senders.len() {
        return Err(ObservationError::invalid_block_envelope(
            EnvelopeRule::TransactionSenderCardinality,
        ));
    }

    let first = transactions
        .first()
        .ok_or_else(|| ObservationError::invalid_block_envelope(EnvelopeRule::AdvancePresent))?;
    if !first.is_system_tx() || senders[0] != Address::ZERO {
        return Err(ObservationError::invalid_envelope(
            0,
            EnvelopeRule::AdvanceSystemCaller,
        ));
    }
    if first.to() != Some(ZONE_INBOX_ADDRESS) {
        return Err(ObservationError::invalid_envelope(
            0,
            EnvelopeRule::AdvanceDestination,
        ));
    }
    if !receipts[0].status() {
        return Err(ObservationError::invalid_envelope(
            0,
            EnvelopeRule::AdvanceSuccess,
        ));
    }
    let advance_tempo = decode_advance_tempo(first.input())?;

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
        let input = decode_finalization(transaction.input())?;
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

    let events = ordered_l2_events(transactions, receipts)?;
    reconcile_event_outputs(&advance_tempo, finalization.as_ref(), &events)?;
    let post_state = load_post_state(block_hash)?;
    reconcile_exact_state(block_hash, &advance_tempo, &post_state)?;

    Ok(L2BlockObservation {
        block_number,
        block_hash,
        inputs: L2AuthenticatedInputs {
            advance_transaction_hash: *first.tx_hash(),
            advance_tempo,
            finalization,
        },
        outcomes: L2AuthenticatedOutcomes { events, post_state },
    })
}

fn ordered_l2_events<R>(
    transactions: &[TempoTxEnvelope],
    receipts: &[R],
) -> Result<Vec<OrderedL2Outcome>, ObservationError>
where
    R: alloy_consensus::TxReceipt<Log = alloy_primitives::Log>,
{
    let mut outcomes = Vec::new();
    let mut block_log_index = 0usize;
    for (transaction_index, (transaction, receipt)) in transactions.iter().zip(receipts).enumerate()
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

fn reconcile_event_outputs(
    advance: &DecodedAdvanceTempo,
    finalization: Option<&FinalizationEnvelope>,
    events: &[OrderedL2Outcome],
) -> Result<(), ObservationError> {
    let tempo_advanced = exactly_one(
        events.iter().filter_map(|outcome| match &outcome.event {
            L2ProtocolEvent::Inbox(Inbox::InboxEvents::TempoAdvanced(event)) => {
                Some((outcome.position, event))
            }
            _ => None,
        }),
        OutputField::TempoAdvancedCount,
    )?;
    if tempo_advanced.0.transaction_index != 0 {
        return Err(ObservationError::output_mismatch(
            OutputField::TempoAdvancedPosition,
            MismatchValue::TransactionIndex(0),
            MismatchValue::TransactionIndex(tempo_advanced.0.transaction_index),
        ));
    }
    compare(
        OutputField::TempoAdvancedHash,
        advance.imported_header().hash(),
        tempo_advanced.1.tempoBlockHash,
        MismatchValue::Hash,
    )?;
    compare(
        OutputField::TempoAdvancedNumber,
        advance.imported_header().number(),
        tempo_advanced.1.tempoBlockNumber,
        MismatchValue::Number,
    )?;
    compare(
        OutputField::TempoAdvancedDepositCount,
        alloy_primitives::U256::from(advance.deposits().len()),
        tempo_advanced.1.depositsProcessed,
        MismatchValue::Word,
    )?;
    let tempo_finalized = exactly_one(
        events.iter().filter_map(|outcome| match &outcome.event {
            L2ProtocolEvent::TempoState(TempoState::TempoStateEvents::TempoBlockFinalized(
                event,
            )) => Some((outcome.position, event)),
            _ => None,
        }),
        OutputField::TempoFinalizedCount,
    )?;
    if tempo_finalized.0.transaction_index != 0 {
        return Err(ObservationError::output_mismatch(
            OutputField::TempoFinalizedPosition,
            MismatchValue::TransactionIndex(0),
            MismatchValue::TransactionIndex(tempo_finalized.0.transaction_index),
        ));
    }
    compare(
        OutputField::TempoFinalizedHash,
        advance.imported_header().hash(),
        tempo_finalized.1.blockHash,
        MismatchValue::Hash,
    )?;
    compare(
        OutputField::TempoFinalizedNumber,
        advance.imported_header().number(),
        tempo_finalized.1.blockNumber,
        MismatchValue::Number,
    )?;
    compare(
        OutputField::TempoFinalizedStateRoot,
        advance.imported_header().header().state_root(),
        tempo_finalized.1.stateRoot,
        MismatchValue::Hash,
    )?;

    let token_outputs = events.iter().filter_map(|outcome| match &outcome.event {
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::TokenEnabled(event)) => {
            Some((outcome.position, event))
        }
        _ => None,
    });
    let token_outputs = token_outputs.collect::<Vec<_>>();
    if token_outputs.len() != advance.enabled_tokens().len() {
        return Err(ObservationError::output_mismatch(
            OutputField::TokenEnabledCount,
            MismatchValue::Count(advance.enabled_tokens().len()),
            MismatchValue::Count(token_outputs.len()),
        ));
    }
    for (index, (input, (position, output))) in advance
        .enabled_tokens()
        .iter()
        .zip(&token_outputs)
        .enumerate()
    {
        compare(
            OutputField::TokenEnabledPosition { index },
            0,
            position.transaction_index,
            MismatchValue::TransactionIndex,
        )?;
        compare(
            OutputField::TokenEnabledToken { index },
            input.token,
            output.token,
            MismatchValue::Address,
        )?;
        compare(
            OutputField::TokenEnabledName { index },
            input.name.as_str(),
            output.name.as_str(),
            |value| MismatchValue::Text(value.to_owned()),
        )?;
        compare(
            OutputField::TokenEnabledSymbol { index },
            input.symbol.as_str(),
            output.symbol.as_str(),
            |value| MismatchValue::Text(value.to_owned()),
        )?;
        compare(
            OutputField::TokenEnabledCurrency { index },
            input.currency.as_str(),
            output.currency.as_str(),
            |value| MismatchValue::Text(value.to_owned()),
        )?;
    }

    let batches = events
        .iter()
        .filter_map(|outcome| match &outcome.event {
            L2ProtocolEvent::Outbox(Outbox::OutboxEvents::BatchFinalized(event)) => {
                Some((outcome.position, event))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    match (finalization, batches.as_slice()) {
        (None, []) => {}
        (Some(finalization), [(position, _)]) => {
            if position.transaction_hash != finalization.transaction_hash {
                return Err(ObservationError::output_mismatch(
                    OutputField::BatchFinalizedTransaction,
                    MismatchValue::Hash(finalization.transaction_hash),
                    MismatchValue::Hash(position.transaction_hash),
                ));
            }
            // Queue/index are independently interpreted only after the model
            // exists in Goal 5. Goal 1 retains both authenticated outputs but
            // does not use either implementation value as the other's oracle.
        }
        (None, _) => {
            return Err(ObservationError::output_mismatch(
                OutputField::BatchFinalizedCount,
                MismatchValue::Count(0),
                MismatchValue::Count(batches.len()),
            ));
        }
        (Some(_), _) => {
            return Err(ObservationError::output_mismatch(
                OutputField::BatchFinalizedCount,
                MismatchValue::Count(1),
                MismatchValue::Count(batches.len()),
            ));
        }
    }
    Ok(())
}

fn reconcile_exact_state(
    zone_block_hash: B256,
    advance: &DecodedAdvanceTempo,
    state: &ZonePostStateOutputs,
) -> Result<(), ObservationError> {
    compare(
        OutputField::StateBlockBinding,
        zone_block_hash,
        state.block_hash(),
        MismatchValue::Hash,
    )?;
    compare(
        OutputField::ExactTempoHash,
        advance.imported_header().hash(),
        state.tempo_block_hash(),
        MismatchValue::Hash,
    )?;
    compare(
        OutputField::ExactTempoNumber,
        advance.imported_header().number(),
        state.tempo_block_number(),
        MismatchValue::Number,
    )
}

fn exactly_one<T>(
    mut values: impl Iterator<Item = T>,
    field: OutputField,
) -> Result<T, ObservationError> {
    let first = values.next();
    let count = usize::from(first.is_some()) + values.count();
    match (first, count) {
        (Some(first), 1) => Ok(first),
        (_, count) => Err(ObservationError::output_mismatch(
            field,
            MismatchValue::Count(1),
            MismatchValue::Count(count),
        )),
    }
}

fn compare<T>(
    field: OutputField,
    expected: T,
    actual: T,
    into_value: impl Fn(T) -> MismatchValue,
) -> Result<(), ObservationError>
where
    T: PartialEq,
{
    if expected != actual {
        return Err(ObservationError::output_mismatch(
            field,
            into_value(expected),
            into_value(actual),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
