//! Strict event grammar for direct Portal withdrawal processing.

use alloy_primitives::{Address, B256};
use tempo_zone_contracts::ZonePortal;

use crate::model::{
    encoding::{AuthenticatedWithdrawalPreimage, OrdinaryDeposit, Withdrawal},
    events::{L1ProtocolEvent, Portal, PortalModelEvent},
    input::{AuthenticatedWithdrawalOutcome, ImportedTempoOperation, WithdrawalProcessingInput},
};

use super::{
    ImportedProjectionError, ObservedDepositAppend, ObservedDepositRefund, ObservedEventPosition,
    ObservedImportedOutput, ObservedProcessedWithdrawal, ObservedUserWithdrawalBounce,
    ObservedUserWithdrawalDelivery, ObservedWithdrawalBounceBackAppend,
    ObservedWithdrawalProcessed, ObservedWithdrawalProcessing, PositionedEvent, error::event_kind,
    project_deposit_made,
};

mod cursor;

use cursor::WithdrawalEventCursor;

/// Model input and independently retained output for one calldata member.
struct WithdrawalMemberProjection {
    input: AuthenticatedWithdrawalOutcome,
    output: ObservedProcessedWithdrawal,
}

pub(super) fn project_withdrawal_processing(
    transaction_index: usize,
    transaction_hash: B256,
    call: &ZonePortal::processWithdrawalsCall,
    events: &[PositionedEvent<'_>],
) -> Result<(ImportedTempoOperation, ObservedImportedOutput), ImportedProjectionError> {
    let withdrawals = decode_withdrawals(transaction_index, call)?;
    let mut events = WithdrawalEventCursor::new(transaction_index, events);
    let mut inputs = Vec::with_capacity(withdrawals.len());
    let mut outputs = Vec::with_capacity(withdrawals.len());
    for member_index in 0..withdrawals.len() {
        let member = project_member(&mut events, transaction_index, member_index)?;
        inputs.push(member.input);
        outputs.push(member.output);
    }
    events.finish()?;

    let input = WithdrawalProcessingInput::new(withdrawals, call.remainingQueue, inputs);
    let output = ObservedWithdrawalProcessing {
        transaction_index,
        transaction_hash,
        members: outputs,
    };
    Ok((
        ImportedTempoOperation::WithdrawalsProcessed(Box::new(input)),
        ObservedImportedOutput::WithdrawalsProcessed(output),
    ))
}

fn decode_withdrawals(
    transaction_index: usize,
    call: &ZonePortal::processWithdrawalsCall,
) -> Result<Vec<Withdrawal>, ImportedProjectionError> {
    call.withdrawals
        .iter()
        .enumerate()
        .map(|(member_index, withdrawal)| {
            Withdrawal::from_authenticated_portal_preimage(AuthenticatedWithdrawalPreimage {
                token: withdrawal.token,
                sender_tag: withdrawal.senderTag,
                to: withdrawal.to,
                amount: withdrawal.amount,
                memo: withdrawal.memo,
                gas_limit: withdrawal.gasLimit,
                fallback_nonce: withdrawal.fallbackNonce,
                callback_data: withdrawal.callbackData.clone(),
                encrypted_sender: withdrawal.encryptedSender.clone(),
            })
            .map_err(
                |source| ImportedProjectionError::InvalidWithdrawalPreimage {
                    transaction_index,
                    member_index,
                    source,
                },
            )
        })
        .collect()
}

fn project_member<'slice, 'event>(
    events: &mut WithdrawalEventCursor<'slice, 'event>,
    transaction_index: usize,
    member_index: usize,
) -> Result<WithdrawalMemberProjection, ImportedProjectionError> {
    let first = events.next_required(member_index)?;
    match first.event {
        L1ProtocolEvent::Portal(PortalModelEvent::DepositBounceBack(refund)) => {
            Ok(WithdrawalMemberProjection {
                input: AuthenticatedWithdrawalOutcome::FailedDepositPaid,
                output: ObservedProcessedWithdrawal::FailedDepositPaid(observed_refund(
                    first.position,
                    refund.tempoRefundRecipient,
                    refund.token,
                    refund.amount,
                    refund.bouncebackFee,
                )),
            })
        }
        L1ProtocolEvent::Portal(PortalModelEvent::DepositBounceBackPending(refund)) => {
            Ok(WithdrawalMemberProjection {
                input: AuthenticatedWithdrawalOutcome::FailedDepositPending,
                output: ObservedProcessedWithdrawal::FailedDepositPending(observed_refund(
                    first.position,
                    refund.tempoRefundRecipient,
                    refund.token,
                    refund.amount,
                    refund.bouncebackFee,
                )),
            })
        }
        _ => project_user_member(events, transaction_index, member_index, first),
    }
}

fn project_user_member<'slice, 'event>(
    events: &mut WithdrawalEventCursor<'slice, 'event>,
    transaction_index: usize,
    member_index: usize,
    mut terminal: &'slice PositionedEvent<'event>,
) -> Result<WithdrawalMemberProjection, ImportedProjectionError> {
    let mut callback_inputs = Vec::<OrdinaryDeposit>::new();
    let mut callback_outputs = Vec::new();
    while let L1ProtocolEvent::Portal(PortalModelEvent::DepositMade(deposit)) = terminal.event {
        let (input, output) = project_deposit_made(terminal.position, deposit)?;
        callback_inputs.push(input);
        callback_outputs.push(output);
        terminal = events.next_required(member_index)?;
    }

    match terminal.event {
        L1ProtocolEvent::Portal(PortalModelEvent::WithdrawalProcessed(processed)) => {
            project_user_delivery(
                transaction_index,
                member_index,
                terminal.position,
                processed,
                callback_inputs,
                callback_outputs,
            )
        }
        L1ProtocolEvent::Portal(PortalModelEvent::WithdrawalBounceBack(append))
            if callback_inputs.is_empty() =>
        {
            project_user_bounce(
                events,
                transaction_index,
                member_index,
                terminal.position,
                append,
            )
        }
        _ => Err(unexpected_outcome(
            transaction_index,
            member_index,
            terminal,
        )),
    }
}

fn project_user_delivery(
    transaction_index: usize,
    member_index: usize,
    position: ObservedEventPosition,
    processed: &Portal::WithdrawalProcessed,
    callback_inputs: Vec<OrdinaryDeposit>,
    callback_outputs: Vec<ObservedDepositAppend>,
) -> Result<WithdrawalMemberProjection, ImportedProjectionError> {
    if !processed.callbackSuccess {
        return Err(ImportedProjectionError::WithdrawalCallbackSuccessMismatch {
            transaction_index,
            member_index,
            expected: true,
            actual: false,
        });
    }
    Ok(WithdrawalMemberProjection {
        input: AuthenticatedWithdrawalOutcome::user_delivered(callback_inputs),
        output: ObservedProcessedWithdrawal::UserDelivered(ObservedUserWithdrawalDelivery {
            callback_deposits: callback_outputs,
            processed: observed_withdrawal_processed(position, processed),
        }),
    })
}

fn project_user_bounce(
    events: &mut WithdrawalEventCursor<'_, '_>,
    transaction_index: usize,
    member_index: usize,
    position: ObservedEventPosition,
    append: &Portal::WithdrawalBounceBack,
) -> Result<WithdrawalMemberProjection, ImportedProjectionError> {
    let observed_append = ObservedWithdrawalBounceBackAppend {
        position,
        queue_hash: append.newCurrentDepositQueueHash,
        fallback_nonce: append.fallbackNonce,
        token: append.token,
        amount: append.amount,
        deposit_number: append.depositNumber,
    };
    let terminal = events.next_required(member_index)?;
    let L1ProtocolEvent::Portal(PortalModelEvent::WithdrawalProcessed(processed)) = terminal.event
    else {
        return Err(unexpected_outcome(
            transaction_index,
            member_index,
            terminal,
        ));
    };
    if processed.callbackSuccess {
        return Err(ImportedProjectionError::WithdrawalCallbackSuccessMismatch {
            transaction_index,
            member_index,
            expected: false,
            actual: true,
        });
    }
    Ok(WithdrawalMemberProjection {
        input: AuthenticatedWithdrawalOutcome::UserBounced,
        output: ObservedProcessedWithdrawal::UserBounced(ObservedUserWithdrawalBounce {
            append: observed_append,
            processed: observed_withdrawal_processed(terminal.position, processed),
        }),
    })
}

fn unexpected_outcome(
    transaction_index: usize,
    member_index: usize,
    event: &PositionedEvent<'_>,
) -> ImportedProjectionError {
    ImportedProjectionError::UnexpectedWithdrawalOutcome {
        transaction_index,
        member_index,
        event: event_kind(event.event),
    }
}

fn observed_refund(
    position: ObservedEventPosition,
    recipient: Address,
    token: Address,
    amount: u128,
    bounceback_fee: u128,
) -> ObservedDepositRefund {
    ObservedDepositRefund {
        position,
        recipient,
        token,
        amount,
        bounceback_fee,
    }
}

fn observed_withdrawal_processed(
    position: ObservedEventPosition,
    processed: &Portal::WithdrawalProcessed,
) -> ObservedWithdrawalProcessed {
    ObservedWithdrawalProcessed {
        position,
        to: processed.to,
        sender_tag: processed.senderTag,
        token: processed.token,
        amount: processed.amount,
        callback_success: processed.callbackSuccess,
    }
}
