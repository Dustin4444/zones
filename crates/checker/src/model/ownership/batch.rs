//! Withdrawal-batch ownership phases and immutable range commitments.

use alloy_primitives::B256;

use super::PortalQueueId;
use crate::model::encoding::{Withdrawal, withdrawal_queue_hash};

/// One processed-deposit cursor captured at a batch boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DepositCursor {
    pub(crate) hash: B256,
    pub(crate) number: u64,
}

/// Immutable block/cursor boundary of one finalized batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatchBoundary {
    pub(crate) first_zone_parent_hash: B256,
    pub(crate) final_zone_block_hash: B256,
    pub(crate) first_processed_deposit: DepositCursor,
    pub(crate) final_processed_deposit: DepositCursor,
    pub(crate) final_imported_tempo_block_number: u64,
    pub(crate) final_zone_height: u64,
}

/// Exact member range and independently derived commitment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatchMembers {
    first_withdrawal_index: u64,
    member_count: u64,
    withdrawal_queue_hash: B256,
}

impl BatchMembers {
    pub(crate) fn from_withdrawals(
        first_withdrawal_index: u64,
        withdrawals: &[Withdrawal],
    ) -> Result<Self, BatchStateError> {
        let member_count =
            u64::try_from(withdrawals.len()).map_err(|_| BatchStateError::MemberCountOverflow {
                actual: withdrawals.len(),
            })?;
        if member_count == 0 {
            return Ok(Self {
                first_withdrawal_index,
                member_count,
                withdrawal_queue_hash: B256::ZERO,
            });
        }
        if first_withdrawal_index
            .checked_add(member_count - 1)
            .is_none()
        {
            return Err(BatchStateError::WithdrawalRangeOverflow {
                first_withdrawal_index,
                member_count,
            });
        }
        Ok(Self {
            first_withdrawal_index,
            member_count,
            withdrawal_queue_hash: withdrawal_queue_hash(withdrawals),
        })
    }

    pub(crate) const fn first_withdrawal_index(&self) -> u64 {
        self.first_withdrawal_index
    }

    pub(crate) const fn member_count(&self) -> u64 {
        self.member_count
    }

    pub(crate) const fn withdrawal_queue_hash(&self) -> B256 {
        self.withdrawal_queue_hash
    }

    /// Stable withdrawal identity at `ordinal`, if it belongs to this batch.
    pub(crate) const fn member_index(&self, ordinal: u64) -> Option<u64> {
        if ordinal >= self.member_count {
            return None;
        }
        self.first_withdrawal_index.checked_add(ordinal)
    }
}

/// Finalized but not yet submitted batch. It has no Portal queue or processing
/// cursor, so submitted-phase state cannot leak into this variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FinalizedBatchState {
    boundary: BatchBoundary,
    members: BatchMembers,
}

impl FinalizedBatchState {
    pub(crate) const fn new(boundary: BatchBoundary, members: BatchMembers) -> Self {
        Self { boundary, members }
    }

    pub(crate) const fn members(&self) -> BatchMembers {
        self.members
    }

    pub(crate) const fn boundary(&self) -> BatchBoundary {
        self.boundary
    }
}

/// Open submitted non-empty batch with a validated unconsumed member cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubmittedBatchState {
    batch: FinalizedBatchState,
    portal_queue: PortalQueueId,
    next_processing_ordinal: u64,
    remaining_queue_hash: B256,
}

impl SubmittedBatchState {
    /// Submit a non-empty finalized batch at its first member and full queue
    /// commitment. Partial progress is represented only by [`Self::advance_partial`].
    pub(crate) fn new(
        batch: FinalizedBatchState,
        portal_queue: PortalQueueId,
    ) -> Result<Self, BatchStateError> {
        if batch.members.member_count == 0 {
            return Err(BatchStateError::EmptyBatchCannotBeSubmitted);
        }
        let remaining_queue_hash = batch.members.withdrawal_queue_hash;
        Ok(Self {
            batch,
            portal_queue,
            next_processing_ordinal: 0,
            remaining_queue_hash,
        })
    }

    /// Advance to a later member while the batch remains open. Exhaustion is a
    /// terminal owner transition and cannot be encoded as submitted state.
    pub(crate) fn advance_partial(
        mut self,
        next_processing_ordinal: u64,
        remaining_queue_hash: B256,
    ) -> Result<Self, BatchStateError> {
        if next_processing_ordinal <= self.next_processing_ordinal {
            return Err(BatchStateError::ProcessingOrdinalDidNotAdvance {
                current: self.next_processing_ordinal,
                next: next_processing_ordinal,
            });
        }
        let member_count = self.batch.members.member_count;
        if next_processing_ordinal >= member_count {
            return Err(BatchStateError::ProcessingOrdinalOutOfRange {
                ordinal: next_processing_ordinal,
                member_count,
            });
        }
        self.next_processing_ordinal = next_processing_ordinal;
        self.remaining_queue_hash = remaining_queue_hash;
        Ok(self)
    }

    pub(crate) const fn batch(&self) -> &FinalizedBatchState {
        &self.batch
    }

    pub(crate) const fn next_processing_ordinal(&self) -> u64 {
        self.next_processing_ordinal
    }

    pub(crate) const fn remaining_queue_hash(&self) -> B256 {
        self.remaining_queue_hash
    }

    pub(crate) const fn portal_queue(&self) -> PortalQueueId {
        self.portal_queue
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BatchStateError {
    #[error("withdrawal member count {actual} does not fit u64")]
    MemberCountOverflow { actual: usize },
    #[error("an empty finalized batch cannot enter the submitted queue")]
    EmptyBatchCannotBeSubmitted,
    #[error("processing ordinal did not advance: current {current}, next {next}")]
    ProcessingOrdinalDidNotAdvance { current: u64, next: u64 },
    #[error("processing ordinal {ordinal} is outside member count {member_count}")]
    ProcessingOrdinalOutOfRange { ordinal: u64, member_count: u64 },
    #[error(
        "withdrawal range starting at {first_withdrawal_index} with {member_count} members overflows u64"
    )]
    WithdrawalRangeOverflow {
        first_withdrawal_index: u64,
        member_count: u64,
    },
}

/// Batch phase is encoded directly rather than by optional queue/cursor fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BatchOwner {
    Finalized(FinalizedBatchState),
    Submitted(SubmittedBatchState),
}
