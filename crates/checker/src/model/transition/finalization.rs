use std::num::NonZeroU64;

use super::{ModelError, ModelTransition};
use crate::model::{
    input::{BatchFinalizationInput, ZoneBlockContext},
    output::ExpectedBatchFinalized,
    ownership::{
        BatchBoundary, BatchId, BatchMembers, BatchOwner, DepositCursor, FinalizedBatchState,
        WithdrawalId, WithdrawalOwner,
    },
    state::{BatchStart, ZoneLastBatch},
};

pub(super) fn apply(
    candidate: &mut ModelTransition<'_>,
    context: ZoneBlockContext,
    tempo_block_number: u64,
    input: &BatchFinalizationInput,
) -> Result<ExpectedBatchFinalized, ModelError> {
    if input.block_number() != context.block_number() {
        return Err(ModelError::FinalizationBlockNumberMismatch {
            expected: context.block_number(),
            actual: input.block_number(),
        });
    }

    let mut zone = candidate.zone().clone();
    let first_withdrawal_index = zone.batch_start.first_withdrawal_index;
    let member_count = zone
        .next_withdrawal_index
        .checked_sub(first_withdrawal_index)
        .ok_or(ModelError::InvalidBatchWithdrawalRange {
            first: first_withdrawal_index,
            next: zone.next_withdrawal_index,
        })?;
    if u64::try_from(input.declared_count()).ok() != Some(member_count) {
        return Err(ModelError::FinalizationCountMismatch {
            expected: member_count,
            actual: input.declared_count(),
        });
    }
    if input.encrypted_senders().len() != input.declared_count() {
        return Err(ModelError::FinalizationSenderCountMismatch {
            declared: input.declared_count(),
            actual: input.encrypted_senders().len(),
        });
    }

    let zone_id = candidate.portal().zone_id();
    let mut withdrawals = Vec::with_capacity(input.declared_count());
    for (ordinal, encrypted_sender) in input.encrypted_senders().iter().enumerate() {
        let ordinal = u64::try_from(ordinal).expect("declared finalization count fits u64");
        let withdrawal_index = first_withdrawal_index
            .checked_add(ordinal)
            .expect("validated contiguous batch range fits u64");
        let id = WithdrawalId {
            zone_id,
            withdrawal_index,
        };
        let owner = candidate
            .withdrawal(id)
            .cloned()
            .ok_or(ModelError::WithdrawalOwnerMissing { withdrawal_index })?;
        let WithdrawalOwner::Pending(pending) = owner else {
            return Err(ModelError::WithdrawalAlreadyFinalized { withdrawal_index });
        };
        let finalized = pending.finalize(encrypted_sender.clone())?;
        withdrawals.push(finalized.preimage().clone());
        candidate.set_withdrawal(id, Some(WithdrawalOwner::Finalized(finalized)));
    }

    let members = BatchMembers::from_withdrawals(first_withdrawal_index, &withdrawals)?;
    let withdrawal_batch_index = zone
        .last_batch
        .withdrawal_batch_index
        .checked_add(1)
        .ok_or(ModelError::WithdrawalBatchIndexOverflow)?;
    let batch = BatchId {
        zone_id,
        withdrawal_batch_index: NonZeroU64::new(withdrawal_batch_index)
            .expect("checked increment from a u64 batch index is nonzero"),
    };
    if candidate.batch(batch).is_some() {
        return Err(ModelError::BatchOwnerCollision {
            withdrawal_batch_index,
        });
    }

    let start = zone.batch_start;
    let final_processed = zone.processed_deposit_cursor;
    let boundary = BatchBoundary {
        first_zone_parent_hash: start.first_zone_parent_hash,
        final_zone_block_hash: context.block_hash(),
        first_processed_deposit: DepositCursor {
            hash: start.first_processed_deposit.hash(),
            number: start.first_processed_deposit.number(),
        },
        final_processed_deposit: DepositCursor {
            hash: final_processed.hash(),
            number: final_processed.number(),
        },
        final_imported_tempo_block_number: tempo_block_number,
        final_zone_height: context.block_number(),
    };
    let withdrawal_queue_hash = members.withdrawal_queue_hash();
    candidate.set_batch(
        batch,
        Some(BatchOwner::Finalized(FinalizedBatchState::new(
            boundary, members,
        ))),
    );

    zone.last_batch = ZoneLastBatch {
        withdrawal_queue_hash,
        withdrawal_batch_index,
    };
    zone.batch_start = BatchStart {
        first_zone_parent_hash: context.block_hash(),
        first_processed_deposit: final_processed,
        first_withdrawal_index: zone.next_withdrawal_index,
    };
    candidate.set_zone(zone);

    Ok(ExpectedBatchFinalized::new(batch, withdrawal_queue_hash))
}
