//! Ordinary Zone operations and optional finalization projection.

use alloy_primitives::B256;

use crate::model::{
    encoding::UserWithdrawalRequest,
    events::{Inbox, L2ProtocolEvent, Outbox},
    input::{RefundClaimInput, UserWithdrawalInput, ZoneOperation},
};

use super::{
    super::{
        ObservedBatchFinalized, ObservedRefundClaimed, ObservedZoneOperation, ZoneProjectionError,
        event_kind,
    },
    cursor::{ZoneEventCursor, observed_position},
    withdrawal::observed_withdrawal,
};

pub(super) struct ZoneOperationsProjection {
    pub(super) inputs: Vec<ZoneOperation>,
    pub(super) outputs: Vec<ObservedZoneOperation>,
}

pub(super) fn project_zone_operations(
    events: &mut ZoneEventCursor<'_>,
    finalization_hash: Option<B256>,
) -> Result<ZoneOperationsProjection, ZoneProjectionError> {
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    while let Some(outcome) = events.next_before_finalization(finalization_hash) {
        let position = observed_position(outcome);
        match outcome.event() {
            L2ProtocolEvent::Outbox(Outbox::OutboxEvents::TempoGasRateUpdated(event)) => {
                inputs.push(ZoneOperation::TempoGasRateUpdated(event.tempoGasRate));
            }
            L2ProtocolEvent::Outbox(Outbox::OutboxEvents::MaxWithdrawalsPerBlockUpdated(event)) => {
                inputs.push(ZoneOperation::MaxWithdrawalsPerBlockUpdated(
                    event.maxWithdrawalsPerBlock,
                ));
            }
            L2ProtocolEvent::Outbox(Outbox::OutboxEvents::WithdrawalRequested(event)) => {
                let request = UserWithdrawalRequest::new(
                    event.token,
                    event.to,
                    event.amount,
                    event.memo,
                    event.gasLimit,
                    event.data.clone(),
                )
                .map_err(|source| {
                    ZoneProjectionError::InvalidWithdrawalRequest {
                        transaction_index: position.transaction_index,
                        source,
                    }
                })?;
                inputs.push(ZoneOperation::user_withdrawal_accepted(
                    UserWithdrawalInput::new(
                        outcome.position().transaction_sender(),
                        position.transaction_hash,
                        request,
                        event.revealTo.clone(),
                    ),
                ));
                outputs.push(ObservedZoneOperation::WithdrawalRequested(
                    observed_withdrawal(position, event),
                ));
            }
            L2ProtocolEvent::Inbox(Inbox::InboxEvents::RefundClaimed(event)) => {
                inputs.push(ZoneOperation::InboxRefundClaimed(RefundClaimInput::new(
                    event.recipient,
                    event.token,
                    event.amount,
                )));
                outputs.push(ObservedZoneOperation::RefundClaimed(
                    ObservedRefundClaimed {
                        position,
                        recipient: event.recipient,
                        token: event.token,
                        amount: event.amount,
                    },
                ));
            }
            L2ProtocolEvent::Outbox(Outbox::OutboxEvents::BatchFinalized(_)) => {
                return Err(match finalization_hash {
                    Some(expected) => {
                        ZoneProjectionError::BatchFinalizedWrongTransaction { expected, position }
                    }
                    None => ZoneProjectionError::BatchFinalizedWithoutEnvelope { position },
                });
            }
            actual => {
                return Err(ZoneProjectionError::UnexpectedPostAdvanceEvent {
                    actual: event_kind(actual),
                    position,
                });
            }
        }
    }
    Ok(ZoneOperationsProjection { inputs, outputs })
}

pub(super) fn project_batch_finalization(
    events: &mut ZoneEventCursor<'_>,
    finalization_hash: Option<B256>,
) -> Result<Option<ObservedBatchFinalized>, ZoneProjectionError> {
    let Some(finalization_hash) = finalization_hash else {
        debug_assert!(events.is_empty(), "post-advance stage consumes all events");
        return Ok(None);
    };

    let outcome = events
        .next()
        .ok_or(ZoneProjectionError::MissingBatchFinalized {
            transaction_hash: finalization_hash,
        })?;
    let position = observed_position(outcome);
    let observed = match outcome.event() {
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::BatchFinalized(event)) => {
            ObservedBatchFinalized {
                position,
                withdrawal_queue_hash: event.withdrawalQueueHash,
                withdrawal_batch_index: event.withdrawalBatchIndex,
            }
        }
        actual => {
            return Err(ZoneProjectionError::ReorderedBatchFinalized {
                actual: event_kind(actual),
                position,
            });
        }
    };
    if let Some(extra) = events.next() {
        return Err(ZoneProjectionError::ExtraFinalizationEvent {
            actual: event_kind(extra.event()),
            position: observed_position(extra),
        });
    }
    Ok(Some(observed))
}
