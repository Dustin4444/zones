//! Opening `advanceTempo` transaction projection.

use tempo_zone_contracts::IZoneInbox;

use crate::{
    model::{
        encoding::DepositQueueMember,
        events::{Inbox, L2ProtocolEvent, TempoState},
        input::{AuthenticatedDepositOutcome, TokenEnable},
    },
    observe::DecodedAdvanceTempo,
};

use super::{
    super::{
        ObservedDepositOutcome, ObservedTempoAdvanced, ObservedTempoBlockFinalized,
        ObservedTokenEnabled, ZoneProjectionError, event_kind,
    },
    cursor::{ZoneEventCursor, observed_position},
    deposits::project_deposit_prefix,
};

pub(super) struct AdvanceProjection {
    pub(super) enabled_tokens: Vec<TokenEnable>,
    pub(super) deposits: Vec<DepositQueueMember>,
    pub(super) deposit_inputs: Vec<AuthenticatedDepositOutcome>,
    pub(super) tempo_block_finalized: ObservedTempoBlockFinalized,
    pub(super) token_enables: Vec<ObservedTokenEnabled>,
    pub(super) deposit_outcomes: Vec<ObservedDepositOutcome>,
    pub(super) tempo_advanced: ObservedTempoAdvanced,
}

struct TokenEnableProjection {
    inputs: Vec<TokenEnable>,
    outputs: Vec<ObservedTokenEnabled>,
}

pub(super) fn project_advance(
    events: &mut ZoneEventCursor<'_>,
    advance: &DecodedAdvanceTempo,
) -> Result<AdvanceProjection, ZoneProjectionError> {
    let tempo_block_finalized = project_tempo_block_finalized(events)?;
    let token_enables = project_token_enables(events, advance.enabled_tokens())?;
    let deposit_prefix = project_deposit_prefix(events, advance.deposits())?;
    let tempo_advanced = project_tempo_advanced(events)?;
    events.finish_advance()?;

    Ok(AdvanceProjection {
        enabled_tokens: token_enables.inputs,
        deposits: deposit_prefix.deposits,
        deposit_inputs: deposit_prefix.inputs,
        tempo_block_finalized,
        token_enables: token_enables.outputs,
        deposit_outcomes: deposit_prefix.outputs,
        tempo_advanced,
    })
}

fn project_tempo_block_finalized(
    events: &mut ZoneEventCursor<'_>,
) -> Result<ObservedTempoBlockFinalized, ZoneProjectionError> {
    let outcome = events.next_advance(ZoneProjectionError::MissingTempoBlockFinalized)?;
    let position = observed_position(outcome);
    match outcome.event() {
        L2ProtocolEvent::TempoState(TempoState::TempoStateEvents::TempoBlockFinalized(event)) => {
            Ok(ObservedTempoBlockFinalized {
                position,
                block_hash: event.blockHash,
                block_number: event.blockNumber,
                state_root: event.stateRoot,
            })
        }
        actual => Err(ZoneProjectionError::ReorderedTempoBlockFinalized {
            actual: event_kind(actual),
            position,
        }),
    }
}

fn project_token_enables(
    events: &mut ZoneEventCursor<'_>,
    enabled_tokens: &[IZoneInbox::EnabledToken],
) -> Result<TokenEnableProjection, ZoneProjectionError> {
    let mut inputs = Vec::with_capacity(enabled_tokens.len());
    let mut outputs = Vec::with_capacity(enabled_tokens.len());
    for (index, enabled) in enabled_tokens.iter().enumerate() {
        inputs.push(TokenEnable::new(
            enabled.token,
            enabled.name.clone(),
            enabled.symbol.clone(),
            enabled.currency.clone(),
        ));

        let outcome = events.next_advance(ZoneProjectionError::MissingTokenEnabled { index })?;
        let position = observed_position(outcome);
        match outcome.event() {
            L2ProtocolEvent::Inbox(Inbox::InboxEvents::TokenEnabled(event)) => {
                outputs.push(ObservedTokenEnabled {
                    position,
                    token: event.token,
                    name: event.name.clone(),
                    symbol: event.symbol.clone(),
                    currency: event.currency.clone(),
                });
            }
            actual => {
                return Err(ZoneProjectionError::ReorderedTokenEnabled {
                    index,
                    actual: event_kind(actual),
                    position,
                });
            }
        }
    }
    Ok(TokenEnableProjection { inputs, outputs })
}

fn project_tempo_advanced(
    events: &mut ZoneEventCursor<'_>,
) -> Result<ObservedTempoAdvanced, ZoneProjectionError> {
    let outcome = events.next_advance(ZoneProjectionError::MissingTempoAdvanced)?;
    let position = observed_position(outcome);
    match outcome.event() {
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::TempoAdvanced(event)) => {
            Ok(ObservedTempoAdvanced {
                position,
                tempo_block_hash: event.tempoBlockHash,
                tempo_block_number: event.tempoBlockNumber,
                deposits_processed: event.depositsProcessed,
                new_processed_deposit_queue_hash: event.newProcessedDepositQueueHash,
                last_processed_deposit_number: event.lastProcessedDepositNumber,
            })
        }
        actual => Err(ZoneProjectionError::ReorderedTempoAdvanced {
            actual: event_kind(actual),
            position,
        }),
    }
}
