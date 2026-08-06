//! Authenticated Tempo observation projection.

use alloy_consensus::BlockHeader as _;
use alloy_primitives::{Address, FixedBytes, U256};

use crate::{
    model::{
        encoding::{CompressedYParity, DepositPayload, OrdinaryDeposit},
        events::{L1ProtocolEvent, Portal, PortalModelEvent},
        input::{
            BatchBlockTransitionInput, BatchDepositTransitionInput, BatchSubmissionInput,
            ImportedTempoBlockInput, ImportedTempoOperation, PortalCreationInput, RefundClaimInput,
            TokenEnable,
        },
        ownership::DepositCursor,
        state::PortalIdentity,
    },
    observe::{ImportedTempoHeader, L1BlockObservation},
};

mod actual;
mod error;
mod processing;

#[cfg(test)]
mod tests;

pub(crate) use actual::{
    ImportedProjection, ObservedDepositAppend, ObservedDepositRefund, ObservedEventPosition,
    ObservedImportedOutput, ObservedProcessedWithdrawal, ObservedRefundClaim,
    ObservedSubmittedBatch, ObservedUserWithdrawalBounce, ObservedUserWithdrawalDelivery,
    ObservedWithdrawalBounceBackAppend, ObservedWithdrawalProcessed, ObservedWithdrawalProcessing,
};
pub(crate) use error::ImportedProjectionError;

use error::event_kind;
use processing::project_withdrawal_processing;

struct PositionedEvent<'a> {
    position: ObservedEventPosition,
    event: &'a L1ProtocolEvent,
}

/// Project the exact authenticated L1 block selected by `imported`.
pub(crate) fn project_imported(
    observation: &L1BlockObservation,
    imported: &ImportedTempoHeader,
) -> Result<ImportedProjection, ImportedProjectionError> {
    if observation.block_hash() != imported.hash() {
        return Err(ImportedProjectionError::BlockHashMismatch {
            expected: imported.hash(),
            actual: observation.block_hash(),
        });
    }
    if observation.block_number() != imported.number() {
        return Err(ImportedProjectionError::BlockNumberMismatch {
            expected: imported.number(),
            actual: observation.block_number(),
        });
    }
    let base_fee = imported
        .header()
        .base_fee_per_gas()
        .ok_or(ImportedProjectionError::MissingBaseFee)?;

    let mut operations = Vec::new();
    let mut outputs = Vec::new();
    let mut previous_transaction_index = None;
    for transaction in observation.protocol_transactions() {
        if let Some(previous) = previous_transaction_index
            && transaction.transaction_index() <= previous
        {
            return Err(ImportedProjectionError::TransactionOrderMismatch {
                previous,
                next: transaction.transaction_index(),
            });
        }
        previous_transaction_index = Some(transaction.transaction_index());

        let events = transaction
            .outcomes()
            .iter()
            .map(|outcome| {
                let position = outcome.position();
                let position = ObservedEventPosition {
                    transaction_index: position.transaction_index(),
                    receipt_log_index: position.receipt_log_index(),
                    block_log_index: position.block_log_index(),
                    transaction_hash: position.transaction_hash(),
                };
                if position.transaction_index != transaction.transaction_index()
                    || position.transaction_hash != transaction.transaction_hash()
                {
                    return Err(ImportedProjectionError::OutcomeCoordinateMismatch {
                        transaction_index: transaction.transaction_index(),
                        transaction_hash: transaction.transaction_hash(),
                        event_transaction_index: position.transaction_index,
                        event_transaction_hash: position.transaction_hash,
                    });
                }
                Ok(PositionedEvent {
                    position,
                    event: outcome.event(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        if let Some(call) = transaction.direct_call() {
            if let Some(call) = call.as_submit_batch() {
                let (operation, output) =
                    project_submit_batch(transaction.transaction_index(), call, &events)?;
                operations.push(operation);
                outputs.push(output);
            } else if let Some(call) = call.as_process_withdrawals() {
                let (operation, output) = project_withdrawal_processing(
                    transaction.transaction_index(),
                    transaction.transaction_hash(),
                    call,
                    &events,
                )?;
                operations.push(operation);
                outputs.push(output);
            } else {
                unreachable!("DecodedPortalCall has exactly two variants")
            }
            continue;
        }

        if events
            .iter()
            .any(|event| matches!(event.event, L1ProtocolEvent::FactoryZoneCreated(_)))
        {
            let operation = project_creation(transaction.transaction_index(), &events)?;
            operations.push(operation);
            continue;
        }

        for event in events {
            project_plain_event(
                transaction.transaction_index(),
                event,
                &mut operations,
                &mut outputs,
            )?;
        }
    }

    Ok(ImportedProjection {
        input: ImportedTempoBlockInput::new(imported.number(), U256::from(base_fee), operations),
        outputs,
    })
}

fn project_creation(
    transaction_index: usize,
    events: &[PositionedEvent<'_>],
) -> Result<ImportedTempoOperation, ImportedProjectionError> {
    let [
        PositionedEvent {
            event: L1ProtocolEvent::Portal(PortalModelEvent::TokenEnabled(enable)),
            ..
        },
        PositionedEvent {
            event: L1ProtocolEvent::FactoryZoneCreated(created),
            ..
        },
    ] = events
    else {
        return Err(ImportedProjectionError::InvalidCreationGrammar { transaction_index });
    };

    Ok(ImportedTempoOperation::Create(PortalCreationInput::new(
        PortalIdentity::new(created.portal, created.zoneId, created.initialToken),
        token_enable(enable.token, &enable.name, &enable.symbol, &enable.currency),
    )))
}

fn project_plain_event(
    transaction_index: usize,
    event: PositionedEvent<'_>,
    operations: &mut Vec<ImportedTempoOperation>,
    outputs: &mut Vec<ObservedImportedOutput>,
) -> Result<(), ImportedProjectionError> {
    match event.event {
        L1ProtocolEvent::Portal(PortalModelEvent::TokenEnabled(enable)) => {
            operations.push(ImportedTempoOperation::TokenEnabled(token_enable(
                enable.token,
                &enable.name,
                &enable.symbol,
                &enable.currency,
            )));
        }
        L1ProtocolEvent::Portal(PortalModelEvent::BouncebackGasUpdated(update)) => {
            operations.push(ImportedTempoOperation::BouncebackGasUpdated(
                update.bouncebackGas,
            ));
        }
        L1ProtocolEvent::Portal(PortalModelEvent::DepositMade(deposit)) => {
            let (input, output) = project_deposit_made(event.position, deposit)?;
            operations.push(ImportedTempoOperation::OrdinaryDepositAppended(input));
            outputs.push(ObservedImportedOutput::DepositAppended(output));
        }
        L1ProtocolEvent::Portal(PortalModelEvent::RefundClaimed(claim)) => {
            operations.push(ImportedTempoOperation::PortalRefundClaimed(
                RefundClaimInput::new(claim.recipient, claim.token, claim.amount),
            ));
            outputs.push(ObservedImportedOutput::RefundClaimed(ObservedRefundClaim {
                position: event.position,
                recipient: claim.recipient,
                token: claim.token,
                amount: claim.amount,
            }));
        }
        L1ProtocolEvent::Portal(
            PortalModelEvent::BatchSubmitted(_)
            | PortalModelEvent::WithdrawalProcessed(_)
            | PortalModelEvent::WithdrawalBounceBack(_)
            | PortalModelEvent::DepositBounceBack(_)
            | PortalModelEvent::DepositBounceBackPending(_),
        ) => {
            return Err(ImportedProjectionError::DirectCallRequired {
                transaction_index,
                event: event_kind(event.event),
            });
        }
        L1ProtocolEvent::FactoryZoneCreated(_) | L1ProtocolEvent::KnownNonModel => {
            return Err(ImportedProjectionError::UnexpectedEvent {
                transaction_index,
                event: event_kind(event.event),
            });
        }
    }
    Ok(())
}

fn project_submit_batch(
    transaction_index: usize,
    call: &tempo_zone_contracts::ZonePortal::submitBatchCall,
    events: &[PositionedEvent<'_>],
) -> Result<(ImportedTempoOperation, ObservedImportedOutput), ImportedProjectionError> {
    let [
        PositionedEvent {
            position,
            event: L1ProtocolEvent::Portal(PortalModelEvent::BatchSubmitted(event)),
        },
    ] = events
    else {
        return Err(ImportedProjectionError::InvalidSubmitBatchGrammar { transaction_index });
    };

    let input = BatchSubmissionInput::new(
        call.tempoBlockNumber,
        BatchBlockTransitionInput::new(
            call.blockTransition.prevBlockHash,
            call.blockTransition.nextBlockHash,
        ),
        BatchDepositTransitionInput::new(
            DepositCursor {
                hash: call.depositQueueTransition.prevProcessedHash,
                number: call.depositQueueTransition.prevDepositNumber,
            },
            DepositCursor {
                hash: call.depositQueueTransition.nextProcessedHash,
                number: call.depositQueueTransition.nextDepositNumber,
            },
        ),
        call.withdrawalQueueHash,
        call.nextZoneHeight,
    );
    let output = ObservedSubmittedBatch {
        position: *position,
        withdrawal_batch_index: event.withdrawalBatchIndex,
        withdrawal_queue_index: event.withdrawalQueueIndex,
        next_processed_deposit_queue_hash: event.nextProcessedDepositQueueHash,
        next_block_hash: event.nextBlockHash,
        withdrawal_queue_hash: event.withdrawalQueueHash,
        last_processed_deposit_number: event.lastProcessedDepositNumber,
    };
    Ok((
        ImportedTempoOperation::BatchSubmitted(Box::new(input)),
        ObservedImportedOutput::BatchSubmitted(output),
    ))
}

fn project_deposit_made(
    position: ObservedEventPosition,
    deposit: &Portal::DepositMade,
) -> Result<(OrdinaryDeposit, ObservedDepositAppend), ImportedProjectionError> {
    let parity = match deposit.ephemeralPubkeyYParity {
        0x02 => CompressedYParity::Even,
        0x03 => CompressedYParity::Odd,
        actual => {
            return Err(ImportedProjectionError::InvalidDepositKeyParity {
                block_log_index: position.block_log_index,
                actual,
            });
        }
    };
    let ciphertext_bytes: [u8; crate::model::constants::ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE] =
        deposit.ciphertext.as_ref().try_into().map_err(|_| {
            ImportedProjectionError::InvalidDepositCiphertextLength {
                block_log_index: position.block_log_index,
                actual: deposit.ciphertext.len(),
                expected: crate::model::constants::ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE,
            }
        })?;
    let input = OrdinaryDeposit::new(
        deposit.token,
        deposit.sender,
        deposit.netAmount,
        deposit.tempoRefundRecipient,
        deposit.keyIndex,
        DepositPayload::new(
            deposit.ephemeralPubkeyX,
            parity,
            FixedBytes::from(ciphertext_bytes),
            deposit.nonce,
            deposit.tag,
        ),
    );
    Ok((
        input,
        ObservedDepositAppend {
            position,
            queue_hash: deposit.newCurrentDepositQueueHash,
            deposit_number: deposit.depositNumber,
        },
    ))
}

fn token_enable(token: Address, name: &str, symbol: &str, currency: &str) -> TokenEnable {
    TokenEnable::new(token, name, symbol, currency)
}
