//! Direct Portal withdrawal-processing transition.

use std::num::NonZeroU128;

use alloy_primitives::U256;

use super::{
    ModelError, ModelTransition, WithdrawalOriginKind, WithdrawalProcessingOutcomeKind, portal,
    refunds, validated_portal_queue_len,
};
use crate::model::{
    accounting::{AccountingTransition, apply_token_accounting},
    encoding::{
        ProcessedWithdrawalQueue, WithdrawalBounceBackDeposit, process_empty_withdrawals,
        process_nonempty_withdrawal_prefix,
    },
    fees::bounce_back_fee,
    input::{AuthenticatedWithdrawalOutcome, WithdrawalProcessingInput},
    output::{
        ExpectedDepositRefund, ExpectedProcessedWithdrawal, ExpectedUserWithdrawalBounce,
        ExpectedUserWithdrawalDelivery, ExpectedWithdrawalBounceBackAppend,
        ExpectedWithdrawalProcessed, ExpectedWithdrawalProcessing,
    },
    ownership::{
        BatchId, BatchOwner, FallbackId, FallbackOwner, FinalizedWithdrawal, PortalRefundId,
        SubmittedBatchState, WithdrawalId, WithdrawalIdentity, WithdrawalOwner,
    },
    state::PortalLifecycle,
};

pub(super) fn apply(
    candidate: &mut ModelTransition<'_>,
    input: &WithdrawalProcessingInput,
    block_base_fee: U256,
) -> Result<ExpectedWithdrawalProcessing, ModelError> {
    if input.withdrawals().len() != input.outcomes().len() {
        return Err(ModelError::WithdrawalProcessingOutcomeCountMismatch {
            withdrawals: input.withdrawals().len(),
            outcomes: input.outcomes().len(),
        });
    }
    if input.withdrawals().is_empty() {
        let ProcessedWithdrawalQueue::Noop = process_empty_withdrawals(input.remaining_queue())
        else {
            unreachable!("empty process helper always returns Noop")
        };
        return Ok(ExpectedWithdrawalProcessing::default());
    }

    let head = acquire_queue_head(candidate)?;
    let ValidatedPrefix {
        next_ordinal,
        queue_result,
        withdrawals,
    } = validate_prefix(candidate, &head, input)?;
    let mut expected = Vec::with_capacity(withdrawals.len());
    for ((withdrawal, finalized), outcome) in withdrawals.into_iter().zip(input.outcomes()) {
        expected.push(apply_outcome(
            candidate,
            withdrawal,
            finalized,
            outcome,
            block_base_fee,
        )?);
    }
    finish_queue_progress(candidate, head, next_ordinal, queue_result)?;
    Ok(ExpectedWithdrawalProcessing::new(expected))
}

struct QueueHead {
    logical_index: U256,
    batch: BatchId,
    submitted: SubmittedBatchState,
}

struct ValidatedPrefix {
    next_ordinal: u64,
    queue_result: ProcessedWithdrawalQueue,
    withdrawals: Vec<(WithdrawalId, FinalizedWithdrawal)>,
}

fn acquire_queue_head(candidate: &ModelTransition<'_>) -> Result<QueueHead, ModelError> {
    let created = portal::require_created(candidate)?;
    let head = created.settlement.withdrawal_queue_head;
    let tail = created.settlement.withdrawal_queue_tail;
    let queue_len = validated_portal_queue_len(head, tail)?;
    if queue_len.is_zero() {
        return Err(ModelError::PortalWithdrawalQueueEmpty);
    }

    let (batch_id, owner) = candidate
        .first_batch()
        .ok_or(ModelError::PortalWithdrawalQueueHeadMissing)?;
    let BatchOwner::Submitted(submitted) = owner else {
        return Err(ModelError::PortalWithdrawalQueueHeadNotSubmitted);
    };
    validate_head(created.identity.portal(), head, &submitted)?;
    Ok(QueueHead {
        logical_index: head,
        batch: batch_id,
        submitted,
    })
}

fn validate_prefix(
    candidate: &ModelTransition<'_>,
    head: &QueueHead,
    input: &WithdrawalProcessingInput,
) -> Result<ValidatedPrefix, ModelError> {
    let members = head.submitted.batch().members();
    let start_ordinal = head.submitted.next_processing_ordinal();
    let remaining_members = members
        .member_count()
        .checked_sub(start_ordinal)
        .expect("submitted cursor is validated inside its batch range");
    let supplied_count = u64::try_from(input.withdrawals().len()).map_err(|_| {
        ModelError::WithdrawalProcessingLengthOverflow {
            actual: input.withdrawals().len(),
        }
    })?;
    if supplied_count > remaining_members {
        return Err(ModelError::WithdrawalProcessingBeyondBatch {
            remaining: remaining_members,
            actual: supplied_count,
        });
    }
    let next_ordinal = start_ordinal.checked_add(supplied_count).ok_or(
        ModelError::WithdrawalProcessingLengthOverflow {
            actual: input.withdrawals().len(),
        },
    )?;
    let queue_result = process_nonempty_withdrawal_prefix(
        head.submitted.remaining_queue_hash(),
        input.withdrawals(),
        input.remaining_queue(),
    )?;
    match queue_result {
        ProcessedWithdrawalQueue::Partial(_) if next_ordinal == members.member_count() => {
            return Err(ModelError::WithdrawalProcessingLeftSuffixAfterBatch);
        }
        ProcessedWithdrawalQueue::Exhausted if next_ordinal != members.member_count() => {
            return Err(ModelError::WithdrawalProcessingExhaustedEarly);
        }
        ProcessedWithdrawalQueue::Noop => {
            unreachable!("non-empty process helper cannot return Noop")
        }
        ProcessedWithdrawalQueue::Partial(_) | ProcessedWithdrawalQueue::Exhausted => {}
    }

    let withdrawals = acquire_exact_prefix(
        candidate,
        head.batch.zone_id,
        members,
        start_ordinal,
        input.withdrawals(),
    )?;
    Ok(ValidatedPrefix {
        next_ordinal,
        queue_result,
        withdrawals,
    })
}

fn finish_queue_progress(
    candidate: &mut ModelTransition<'_>,
    head: QueueHead,
    next_ordinal: u64,
    queue_result: ProcessedWithdrawalQueue,
) -> Result<(), ModelError> {
    match queue_result {
        ProcessedWithdrawalQueue::Partial(remaining_queue_hash) => {
            let submitted = head
                .submitted
                .advance_partial(next_ordinal, remaining_queue_hash)?;
            candidate.set_batch(head.batch, Some(BatchOwner::Submitted(submitted)));
        }
        ProcessedWithdrawalQueue::Exhausted => {
            candidate.set_batch(head.batch, None);
            let mut portal = portal::require_created(candidate)?.clone();
            if portal.settlement.withdrawal_queue_head != head.logical_index {
                return Err(ModelError::InvalidPortalWithdrawalQueueProgress {
                    head: portal.settlement.withdrawal_queue_head,
                    tail: portal.settlement.withdrawal_queue_tail,
                });
            }
            portal.settlement.withdrawal_queue_head = head
                .logical_index
                .checked_add(U256::ONE)
                .ok_or(ModelError::PortalWithdrawalQueueCounterOverflow)?;
            candidate.set_portal(PortalLifecycle::Created(Box::new(portal)));
        }
        ProcessedWithdrawalQueue::Noop => unreachable!("handled before queue acquisition"),
    }

    Ok(())
}

fn validate_head(
    portal: alloy_primitives::Address,
    head: U256,
    submitted: &SubmittedBatchState,
) -> Result<(), ModelError> {
    let queue = submitted.portal_queue();
    if queue.portal() != portal {
        return Err(ModelError::PortalWithdrawalQueuePortalMismatch {
            expected: portal,
            actual: queue.portal(),
        });
    }
    if queue.logical_queue_index() != head {
        return Err(ModelError::PortalWithdrawalQueueHeadMismatch {
            expected: head,
            actual: queue.logical_queue_index(),
        });
    }
    Ok(())
}

fn acquire_exact_prefix(
    candidate: &ModelTransition<'_>,
    zone_id: u32,
    members: crate::model::ownership::BatchMembers,
    start_ordinal: u64,
    supplied: &[crate::model::encoding::Withdrawal],
) -> Result<Vec<(WithdrawalId, FinalizedWithdrawal)>, ModelError> {
    let mut owners = Vec::with_capacity(supplied.len());
    for (offset, preimage) in supplied.iter().enumerate() {
        let offset =
            u64::try_from(offset).map_err(|_| ModelError::WithdrawalProcessingLengthOverflow {
                actual: supplied.len(),
            })?;
        let ordinal = start_ordinal.checked_add(offset).ok_or(
            ModelError::WithdrawalProcessingLengthOverflow {
                actual: supplied.len(),
            },
        )?;
        let withdrawal_index =
            members
                .member_index(ordinal)
                .ok_or(ModelError::WithdrawalProcessingBeyondBatch {
                    remaining: members.member_count() - start_ordinal,
                    actual: u64::try_from(supplied.len()).unwrap_or(u64::MAX),
                })?;
        let withdrawal = WithdrawalId {
            zone_id,
            withdrawal_index,
        };
        let owner = candidate
            .withdrawal(withdrawal)
            .cloned()
            .ok_or(ModelError::WithdrawalOwnerMissing { withdrawal_index })?;
        let WithdrawalOwner::Finalized(finalized) = owner else {
            return Err(ModelError::WithdrawalNotFinalizedForProcessing { withdrawal_index });
        };
        if finalized.preimage() != preimage {
            return Err(ModelError::WithdrawalProcessingPreimageMismatch { withdrawal_index });
        }
        owners.push((withdrawal, finalized));
    }
    Ok(owners)
}

fn apply_outcome(
    candidate: &mut ModelTransition<'_>,
    withdrawal: WithdrawalId,
    finalized: FinalizedWithdrawal,
    outcome: &AuthenticatedWithdrawalOutcome,
    block_base_fee: U256,
) -> Result<ExpectedProcessedWithdrawal, ModelError> {
    match finalized.identity() {
        WithdrawalIdentity::User(identity) => match outcome {
            AuthenticatedWithdrawalOutcome::UserDelivered { callback_deposits } => deliver_user(
                candidate,
                withdrawal,
                &finalized,
                identity,
                callback_deposits,
            ),
            AuthenticatedWithdrawalOutcome::UserBounced => {
                bounce_user(candidate, withdrawal, &finalized, identity)
            }
            _ => Err(outcome_mismatch(
                withdrawal,
                WithdrawalOriginKind::User,
                outcome,
            )),
        },
        WithdrawalIdentity::FailedDeposit { deposit } => match outcome {
            AuthenticatedWithdrawalOutcome::FailedDepositPaid => failed_deposit(
                candidate,
                withdrawal,
                &finalized,
                deposit,
                block_base_fee,
                FailedDepositDisposition::Paid,
            ),
            AuthenticatedWithdrawalOutcome::FailedDepositPending => failed_deposit(
                candidate,
                withdrawal,
                &finalized,
                deposit,
                block_base_fee,
                FailedDepositDisposition::Pending,
            ),
            _ => Err(outcome_mismatch(
                withdrawal,
                WithdrawalOriginKind::FailedDeposit,
                outcome,
            )),
        },
    }
}

fn deliver_user(
    candidate: &mut ModelTransition<'_>,
    withdrawal: WithdrawalId,
    finalized: &FinalizedWithdrawal,
    identity: crate::model::encoding::UserWithdrawalIdentity,
    callback_deposits: &[crate::model::encoding::OrdinaryDeposit],
) -> Result<ExpectedProcessedWithdrawal, ModelError> {
    let preimage = finalized.preimage();
    if preimage.gas_limit() == 0 && !callback_deposits.is_empty() {
        return Err(ModelError::CallbackDepositsWithoutCallback {
            withdrawal_index: withdrawal.withdrawal_index,
        });
    }
    let fallback = require_held_fallback(candidate, withdrawal, identity, preimage)?;
    let mut callback_appends = Vec::with_capacity(callback_deposits.len());
    for deposit in callback_deposits {
        callback_appends.push(portal::append_ordinary(candidate, deposit)?);
    }
    apply_token_accounting_transition(
        candidate,
        preimage.token(),
        AccountingTransition::UserWithdrawalDelivered {
            amount: U256::from(preimage.amount()),
        },
    )?;
    candidate.set_fallback_owner(fallback, None);
    candidate.set_withdrawal(withdrawal, None);
    Ok(ExpectedProcessedWithdrawal::UserDelivered(Box::new(
        ExpectedUserWithdrawalDelivery::new(
            callback_appends,
            ExpectedWithdrawalProcessed::delivered(withdrawal, preimage),
        ),
    )))
}

fn bounce_user(
    candidate: &mut ModelTransition<'_>,
    withdrawal: WithdrawalId,
    finalized: &FinalizedWithdrawal,
    identity: crate::model::encoding::UserWithdrawalIdentity,
) -> Result<ExpectedProcessedWithdrawal, ModelError> {
    let preimage = finalized.preimage();
    require_held_fallback(candidate, withdrawal, identity, preimage)?;
    let deposit = WithdrawalBounceBackDeposit::new(
        preimage.token(),
        identity.fallback_nonce(),
        NonZeroU128::new(preimage.amount())
            .expect("user withdrawal admission requires a nonzero principal"),
    );
    let append = portal::append_withdrawal_bounce_back(candidate, deposit)?;
    apply_token_accounting_transition(
        candidate,
        preimage.token(),
        AccountingTransition::WithdrawalDeliveryFailed,
    )?;
    candidate.set_withdrawal(withdrawal, None);
    Ok(ExpectedProcessedWithdrawal::UserBounced(Box::new(
        ExpectedUserWithdrawalBounce::new(
            ExpectedWithdrawalBounceBackAppend::new(deposit, append),
            ExpectedWithdrawalProcessed::bounced(withdrawal, preimage),
        ),
    )))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedDepositDisposition {
    Paid,
    Pending,
}

fn failed_deposit(
    candidate: &mut ModelTransition<'_>,
    withdrawal: WithdrawalId,
    finalized: &FinalizedWithdrawal,
    failed_deposit: crate::model::ownership::DepositId,
    block_base_fee: U256,
    disposition: FailedDepositDisposition,
) -> Result<ExpectedProcessedWithdrawal, ModelError> {
    let preimage = finalized.preimage();
    let bounceback_gas = portal::require_created(candidate)?.config.bounceback_gas;
    let fee = bounce_back_fee(bounceback_gas, block_base_fee, preimage.amount())?;
    let refund = preimage
        .amount()
        .checked_sub(fee)
        .expect("bounce-back fee helper caps at the withdrawal amount");
    let expected =
        ExpectedDepositRefund::new(failed_deposit, preimage.to(), preimage.token(), refund, fee);
    let expected = match disposition {
        FailedDepositDisposition::Paid => {
            apply_token_accounting_transition(
                candidate,
                preimage.token(),
                AccountingTransition::FailedDepositRefundPaid {
                    original_amount: U256::from(preimage.amount()),
                },
            )?;
            ExpectedProcessedWithdrawal::FailedDepositPaid(expected)
        }
        FailedDepositDisposition::Pending => {
            apply_token_accounting_transition(
                candidate,
                preimage.token(),
                AccountingTransition::FailedDepositRefundPending {
                    bounceback_fee: U256::from(fee),
                },
            )?;
            refunds::create_portal_credit(
                candidate,
                PortalRefundId {
                    token: preimage.token(),
                    recipient: preimage.to(),
                    failed_deposit,
                },
                refund,
            )?;
            ExpectedProcessedWithdrawal::FailedDepositPending(expected)
        }
    };
    candidate.set_withdrawal(withdrawal, None);
    Ok(expected)
}

fn require_held_fallback(
    candidate: &ModelTransition<'_>,
    withdrawal: WithdrawalId,
    identity: crate::model::encoding::UserWithdrawalIdentity,
    preimage: &crate::model::encoding::Withdrawal,
) -> Result<FallbackId, ModelError> {
    let fallback = FallbackId {
        zone_id: withdrawal.zone_id,
        fallback_nonce: identity.fallback_nonce(),
    };
    let owner = candidate
        .fallback_owner(fallback)
        .ok_or(ModelError::FallbackOwnerMissing {
            fallback_nonce: identity.fallback_nonce().get(),
        })?;
    match owner {
        FallbackOwner::Held {
            withdrawal: owned_withdrawal,
            token,
            amount,
        } if *owned_withdrawal == withdrawal
            && *token == preimage.token()
            && amount.get() == preimage.amount() =>
        {
            Ok(fallback)
        }
        FallbackOwner::Held { .. } | FallbackOwner::BounceBackQueued { .. } => {
            Err(ModelError::FallbackOwnerMismatch {
                fallback_nonce: identity.fallback_nonce().get(),
            })
        }
    }
}

fn apply_token_accounting_transition(
    candidate: &mut ModelTransition<'_>,
    token: alloy_primitives::Address,
    transition: AccountingTransition,
) -> Result<(), ModelError> {
    let mut state = candidate
        .token(token)
        .cloned()
        .ok_or(ModelError::TokenNotPortalEnabled { token })?;
    state.accounting = apply_token_accounting(Some(state.accounting), transition)?;
    candidate.set_token(token, state);
    Ok(())
}

fn outcome_mismatch(
    withdrawal: WithdrawalId,
    expected: WithdrawalOriginKind,
    outcome: &AuthenticatedWithdrawalOutcome,
) -> ModelError {
    ModelError::WithdrawalProcessingOutcomeMismatch {
        withdrawal_index: withdrawal.withdrawal_index,
        expected,
        actual: match outcome {
            AuthenticatedWithdrawalOutcome::UserDelivered { .. } => {
                WithdrawalProcessingOutcomeKind::UserDelivered
            }
            AuthenticatedWithdrawalOutcome::UserBounced => {
                WithdrawalProcessingOutcomeKind::UserBounced
            }
            AuthenticatedWithdrawalOutcome::FailedDepositPaid => {
                WithdrawalProcessingOutcomeKind::FailedDepositPaid
            }
            AuthenticatedWithdrawalOutcome::FailedDepositPending => {
                WithdrawalProcessingOutcomeKind::FailedDepositPending
            }
        },
    }
}
