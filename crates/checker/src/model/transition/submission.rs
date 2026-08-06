//! Direct Portal batch-submission transition.

use std::num::NonZeroU64;

use alloy_primitives::U256;

use super::{
    BlockTransitionMismatch, DepositTransitionMismatch, ModelError, ModelTransition,
    portal::require_created, validated_portal_queue_len,
};
use crate::model::{
    constants::{NO_WITHDRAWAL_QUEUE_INDEX, WITHDRAWAL_QUEUE_CAPACITY},
    input::BatchSubmissionInput,
    output::ExpectedBatchSubmission,
    ownership::{BatchId, BatchOwner, PortalQueueId, SubmittedBatchState},
    state::PortalLifecycle,
};

pub(super) fn apply(
    candidate: &mut ModelTransition<'_>,
    input: &BatchSubmissionInput,
) -> Result<ExpectedBatchSubmission, ModelError> {
    let mut portal = require_created(candidate)?.clone();
    let mut settlement = portal.settlement;
    let withdrawal_batch_index = settlement
        .withdrawal_batch_index
        .checked_add(1)
        .ok_or(ModelError::PortalBatchIndexOverflow)?;
    let batch = BatchId {
        zone_id: portal.identity.zone_id(),
        withdrawal_batch_index: NonZeroU64::new(withdrawal_batch_index)
            .expect("checked increment from a u64 batch index is nonzero"),
    };
    let owner = candidate
        .batch(batch)
        .cloned()
        .ok_or(ModelError::BatchOwnerMissing {
            withdrawal_batch_index,
        })?;
    let BatchOwner::Finalized(finalized) = owner else {
        return Err(ModelError::BatchAlreadySubmitted {
            withdrawal_batch_index,
        });
    };
    let boundary = finalized.boundary();
    let members = finalized.members();

    validate_submission(
        input,
        withdrawal_batch_index,
        boundary,
        members.withdrawal_queue_hash(),
    )?;
    validate_portal_progress(
        withdrawal_batch_index,
        &settlement,
        boundary,
        portal.deposit_cursor.number(),
    )?;

    let head = settlement.withdrawal_queue_head;
    let tail = settlement.withdrawal_queue_tail;
    let queue_len = validated_portal_queue_len(head, tail)?;

    let withdrawal_queue_index = if members.member_count() == 0 {
        candidate.set_batch(batch, None);
        NO_WITHDRAWAL_QUEUE_INDEX
    } else {
        if queue_len == WITHDRAWAL_QUEUE_CAPACITY {
            return Err(ModelError::PortalWithdrawalQueueFull);
        }
        let next_tail = tail
            .checked_add(U256::ONE)
            .ok_or(ModelError::PortalWithdrawalQueueCounterOverflow)?;
        let queue = PortalQueueId::new(portal.identity.portal(), tail)?;
        let submitted = SubmittedBatchState::new(finalized, queue)?;
        settlement.withdrawal_queue_tail = next_tail;
        candidate.set_batch(batch, Some(BatchOwner::Submitted(submitted)));
        tail
    };

    settlement.withdrawal_batch_index = withdrawal_batch_index;
    settlement.block_hash = boundary.final_zone_block_hash;
    settlement.last_synced_tempo_block_number = boundary.final_imported_tempo_block_number;
    settlement.last_submitted_deposit_cursor = boundary.final_processed_deposit;
    settlement.zone_height = U256::from(boundary.final_zone_height);
    portal.settlement = settlement;
    candidate.set_portal(PortalLifecycle::Created(Box::new(portal)));

    Ok(ExpectedBatchSubmission::new(
        batch,
        withdrawal_queue_index,
        boundary.final_processed_deposit.hash,
        boundary.final_zone_block_hash,
        members.withdrawal_queue_hash(),
        boundary.final_processed_deposit.number,
    ))
}

fn validate_submission(
    input: &BatchSubmissionInput,
    withdrawal_batch_index: u64,
    boundary: crate::model::ownership::BatchBoundary,
    withdrawal_queue_hash: alloy_primitives::B256,
) -> Result<(), ModelError> {
    if input.tempo_block_number() != boundary.final_imported_tempo_block_number {
        return Err(ModelError::BatchTempoBlockMismatch {
            withdrawal_batch_index,
            expected: boundary.final_imported_tempo_block_number,
            actual: input.tempo_block_number(),
        });
    }
    let expected_height = U256::from(boundary.final_zone_height);
    if input.next_zone_height() != expected_height {
        return Err(ModelError::BatchZoneHeightMismatch {
            withdrawal_batch_index,
            expected: expected_height,
            actual: input.next_zone_height(),
        });
    }
    let block = input.block_transition();
    if block.previous() != boundary.first_zone_parent_hash
        || block.next() != boundary.final_zone_block_hash
    {
        return Err(ModelError::BatchBlockTransitionMismatch {
            withdrawal_batch_index,
            details: Box::new(BlockTransitionMismatch {
                expected_previous: boundary.first_zone_parent_hash,
                actual_previous: block.previous(),
                expected_next: boundary.final_zone_block_hash,
                actual_next: block.next(),
            }),
        });
    }
    let deposits = input.deposit_transition();
    if deposits.previous() != boundary.first_processed_deposit
        || deposits.next() != boundary.final_processed_deposit
    {
        return Err(ModelError::BatchDepositTransitionMismatch {
            withdrawal_batch_index,
            details: Box::new(DepositTransitionMismatch {
                expected_previous: boundary.first_processed_deposit,
                actual_previous: deposits.previous(),
                expected_next: boundary.final_processed_deposit,
                actual_next: deposits.next(),
            }),
        });
    }
    if input.withdrawal_queue_hash() != withdrawal_queue_hash {
        return Err(ModelError::BatchWithdrawalQueueHashMismatch {
            withdrawal_batch_index,
            expected: withdrawal_queue_hash,
            actual: input.withdrawal_queue_hash(),
        });
    }
    Ok(())
}

fn validate_portal_progress(
    withdrawal_batch_index: u64,
    settlement: &crate::model::state::PortalSettlementState,
    boundary: crate::model::ownership::BatchBoundary,
    portal_deposit_number: u64,
) -> Result<(), ModelError> {
    if settlement.block_hash != boundary.first_zone_parent_hash {
        return Err(ModelError::PortalBlockContinuityMismatch {
            withdrawal_batch_index,
            expected: settlement.block_hash,
            actual: boundary.first_zone_parent_hash,
        });
    }
    if settlement.last_submitted_deposit_cursor != boundary.first_processed_deposit {
        return Err(ModelError::PortalDepositContinuityMismatch {
            withdrawal_batch_index,
            expected: settlement.last_submitted_deposit_cursor,
            actual: boundary.first_processed_deposit,
        });
    }
    let next_zone_height = U256::from(boundary.final_zone_height);
    if next_zone_height <= settlement.zone_height {
        return Err(ModelError::PortalZoneHeightNotIncreasing {
            withdrawal_batch_index,
            previous: settlement.zone_height,
            next: next_zone_height,
        });
    }
    if boundary.final_processed_deposit.number > portal_deposit_number {
        return Err(ModelError::PortalDepositCursorBeyondQueue {
            withdrawal_batch_index,
            submitted: boundary.final_processed_deposit.number,
            deposited: portal_deposit_number,
        });
    }
    Ok(())
}
