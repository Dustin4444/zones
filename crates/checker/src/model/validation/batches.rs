use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::U256;

use super::{AuthoritativeStateError, CursorKind, OwnerKind, cursor, require_cursor_prefix};
use crate::model::{
    encoding::withdrawal_queue_hash,
    ownership::{
        BatchBoundary, BatchId, BatchOwner, FinalizedBatchState, WithdrawalId, WithdrawalOwner,
    },
    state::{CreatedPortalState, ModelState},
};

use super::owners::require_zone;

pub(super) fn validate(
    state: &ModelState,
    portal: &CreatedPortalState,
) -> Result<(), AuthoritativeStateError> {
    let zone = state.zone();
    let last_batch_index = zone.last_batch().withdrawal_batch_index();
    let portal_batch_index = portal.settlement().withdrawal_batch_index();
    validate_terminal_boundary(state, portal, portal_batch_index, last_batch_index)?;
    let mut submitted_queues = BTreeMap::<U256, BatchId>::new();
    let mut owned_withdrawals = BTreeSet::<WithdrawalId>::new();
    let mut prior_range_end = None;
    let mut prior_submitted_boundary = None;
    let mut last_open_range = None;

    for (id, owner) in &state.batches {
        let batch_index = id.withdrawal_batch_index.get();
        require_zone(OwnerKind::Batch, portal.identity().zone_id(), id.zone_id)?;
        if batch_index > last_batch_index {
            return Err(AuthoritativeStateError::BatchBeyondLastIndex {
                batch_index,
                last_batch_index,
            });
        }
        let (batch, start_ordinal, submitted_queue_index) = match owner {
            BatchOwner::Finalized(batch) => {
                if batch_index <= portal_batch_index {
                    return Err(AuthoritativeStateError::BatchPhaseCounterMismatch {
                        batch_index,
                        portal_index: portal_batch_index,
                    });
                }
                (batch, 0, None)
            }
            BatchOwner::Submitted(submitted) => {
                if batch_index > portal_batch_index {
                    return Err(AuthoritativeStateError::BatchPhaseCounterMismatch {
                        batch_index,
                        portal_index: portal_batch_index,
                    });
                }
                let queue = submitted.portal_queue();
                if queue.portal() != portal.identity().portal() {
                    return Err(AuthoritativeStateError::SubmittedQueuePortalMismatch {
                        batch_index,
                        expected: portal.identity().portal(),
                        actual: queue.portal(),
                    });
                }
                let queue_index = queue.logical_queue_index();
                let settlement = portal.settlement();
                if queue_index < settlement.withdrawal_queue_head()
                    || queue_index >= settlement.withdrawal_queue_tail()
                {
                    return Err(AuthoritativeStateError::SubmittedQueueIndexOutOfRange {
                        batch_index,
                        queue_index,
                        head: settlement.withdrawal_queue_head(),
                        tail: settlement.withdrawal_queue_tail(),
                    });
                }
                if submitted_queues.insert(queue_index, *id).is_some() {
                    return Err(AuthoritativeStateError::DuplicateSubmittedQueueIndex {
                        queue_index,
                    });
                }
                (
                    submitted.batch(),
                    submitted.next_processing_ordinal(),
                    Some(queue_index),
                )
            }
        };

        let boundary = batch.boundary();
        validate_cursors(state, batch_index, boundary)?;
        if submitted_queue_index.is_some() {
            if let Some((previous_batch_index, previous_boundary)) = prior_submitted_boundary {
                validate_submitted_boundary(
                    previous_batch_index,
                    previous_boundary,
                    batch_index,
                    boundary,
                )?;
            }
            prior_submitted_boundary = Some((batch_index, boundary));
        }
        let members = batch.members();
        let range_end = members.withdrawal_range_end();
        let expected_start = prior_range_end.or((portal_batch_index == 0).then_some(0));
        if let Some(expected) = expected_start
            && members.first_withdrawal_index() != expected
        {
            return Err(AuthoritativeStateError::OpenBatchRangeDiscontinuity {
                batch_index,
                expected,
                actual: members.first_withdrawal_index(),
            });
        }
        prior_range_end = Some(range_end);
        last_open_range = Some((batch_index, range_end));
        if range_end > zone.next_withdrawal_index() {
            return Err(AuthoritativeStateError::BatchRangeBeyondNext {
                batch_index,
                range_end,
                next_withdrawal_index: zone.next_withdrawal_index(),
            });
        }

        if start_ordinal > members.member_count() {
            return Err(
                AuthoritativeStateError::BatchProcessingOrdinalBeyondMembers {
                    batch_index,
                    ordinal: start_ordinal,
                    member_count: members.member_count(),
                },
            );
        }
        if let Some(queue_index) = submitted_queue_index
            && queue_index != portal.settlement().withdrawal_queue_head()
            && start_ordinal != 0
        {
            return Err(
                AuthoritativeStateError::SubmittedBatchProcessedAheadOfHead {
                    batch_index,
                    queue_index,
                    head: portal.settlement().withdrawal_queue_head(),
                    ordinal: start_ordinal,
                },
            );
        }
        let first_remaining = members
            .first_withdrawal_index()
            .checked_add(start_ordinal)
            .ok_or(AuthoritativeStateError::BatchRangeOverflow { batch_index })?;
        let range_start = WithdrawalId {
            zone_id: id.zone_id,
            withdrawal_index: first_remaining,
        };
        let range_end_id = WithdrawalId {
            zone_id: id.zone_id,
            withdrawal_index: range_end,
        };
        let mut expected_withdrawal_index = first_remaining;
        let mut remaining = Vec::new();
        for (withdrawal, owner) in state.withdrawals().range(range_start..range_end_id) {
            if withdrawal.withdrawal_index != expected_withdrawal_index {
                return Err(AuthoritativeStateError::BatchWithdrawalMissing {
                    batch_index,
                    withdrawal_index: expected_withdrawal_index,
                });
            }
            let WithdrawalOwner::Finalized(finalized) = owner else {
                return Err(AuthoritativeStateError::BatchWithdrawalNotFinalized {
                    batch_index,
                    withdrawal_index: withdrawal.withdrawal_index,
                });
            };
            if !owned_withdrawals.insert(*withdrawal) {
                return Err(AuthoritativeStateError::DuplicateBatchWithdrawal {
                    withdrawal_index: withdrawal.withdrawal_index,
                });
            }
            remaining.push(finalized.preimage().clone());
            expected_withdrawal_index = expected_withdrawal_index
                .checked_add(1)
                .ok_or(AuthoritativeStateError::BatchRangeOverflow { batch_index })?;
        }
        if expected_withdrawal_index != range_end {
            return Err(AuthoritativeStateError::BatchWithdrawalMissing {
                batch_index,
                withdrawal_index: expected_withdrawal_index,
            });
        }
        let expected_hash = withdrawal_queue_hash(&remaining);
        let actual_hash = match owner {
            BatchOwner::Finalized(_) => members.withdrawal_queue_hash(),
            BatchOwner::Submitted(submitted) => submitted.remaining_queue_hash(),
        };
        if expected_hash != actual_hash {
            return Err(AuthoritativeStateError::BatchQueueCommitmentMismatch { batch_index });
        }

        if batch_index == last_batch_index {
            validate_last_accumulator(state, batch_index, batch)?;
        }
        if batch_index == portal_batch_index && matches!(owner, BatchOwner::Submitted(_)) {
            validate_latest_submission(portal, batch_index, batch)?;
        }
    }

    if let Some((batch_index, boundary)) = prior_submitted_boundary {
        validate_submitted_tail(portal, batch_index, boundary)?;
    }
    if let Some((batch_index, range_end)) = last_open_range {
        let batch_start_index = zone.batch_start().first_withdrawal_index();
        if range_end != batch_start_index {
            return Err(
                AuthoritativeStateError::OpenBatchTerminalRangeDiscontinuity {
                    batch_index,
                    range_end,
                    batch_start_index,
                },
            );
        }
    }

    validate_unsubmitted_suffix(state, portal, portal_batch_index, last_batch_index)?;
    for (id, owner) in state.withdrawals() {
        if matches!(owner, WithdrawalOwner::Finalized(_)) && !owned_withdrawals.contains(id) {
            return Err(AuthoritativeStateError::OrphanFinalizedWithdrawal {
                withdrawal_index: id.withdrawal_index,
            });
        }
    }
    validate_submitted_queue(portal, &submitted_queues)
}

fn validate_unsubmitted_suffix(
    state: &ModelState,
    portal: &CreatedPortalState,
    portal_batch_index: u64,
    last_batch_index: u64,
) -> Result<(), AuthoritativeStateError> {
    if portal_batch_index == last_batch_index {
        return Ok(());
    }

    let settlement = portal.settlement();
    let mut prior_block = settlement.block_hash();
    let mut prior_deposit = settlement.last_submitted_deposit_cursor();
    let mut prior_zone_height = u64::try_from(settlement.zone_height())
        .expect("counter validation bounds the Portal Zone height");
    let mut prior_tempo_height = settlement.last_synced_tempo_block_number();
    let mut prior_batch_index = portal_batch_index;

    for (id, owner) in state.batches() {
        let batch_index = id.withdrawal_batch_index.get();
        if batch_index <= portal_batch_index {
            continue;
        }
        let expected_batch_index = prior_batch_index.checked_add(1).ok_or(
            AuthoritativeStateError::UnsubmittedBatchMissing {
                batch_index: prior_batch_index,
            },
        )?;
        if batch_index != expected_batch_index {
            return Err(AuthoritativeStateError::UnsubmittedBatchMissing {
                batch_index: expected_batch_index,
            });
        }
        let BatchOwner::Finalized(batch) = owner else {
            return Err(AuthoritativeStateError::UnsubmittedBatchAlreadySubmitted { batch_index });
        };
        let boundary = batch.boundary();
        if boundary.first_zone_parent_hash != prior_block {
            return Err(
                AuthoritativeStateError::UnsubmittedBatchBlockDiscontinuity { batch_index },
            );
        }
        if boundary.first_processed_deposit != prior_deposit {
            return Err(
                AuthoritativeStateError::UnsubmittedBatchDepositDiscontinuity { batch_index },
            );
        }
        let zone_advance = strict_advance(boundary.final_zone_height, prior_zone_height).ok_or(
            AuthoritativeStateError::UnsubmittedBatchZoneHeightNotIncreasing { batch_index },
        )?;
        let tempo_advance = strict_advance(
            boundary.final_imported_tempo_block_number,
            prior_tempo_height,
        )
        .ok_or(AuthoritativeStateError::UnsubmittedBatchTempoHeightNotIncreasing { batch_index })?;
        if prior_batch_index != 0 && zone_advance != tempo_advance {
            return Err(AuthoritativeStateError::BatchTipAdvanceMismatch {
                previous_batch_index: prior_batch_index,
                batch_index,
                zone_advance,
                tempo_advance,
            });
        }
        prior_block = boundary.final_zone_block_hash;
        prior_deposit = boundary.final_processed_deposit;
        prior_zone_height = boundary.final_zone_height;
        prior_tempo_height = boundary.final_imported_tempo_block_number;
        prior_batch_index = batch_index;
    }

    if prior_batch_index != last_batch_index {
        return Err(AuthoritativeStateError::UnsubmittedBatchMissing {
            batch_index: prior_batch_index
                .checked_add(1)
                .expect("a missing batch before the Zone counter fits u64"),
        });
    }

    Ok(())
}

fn strict_advance(current: u64, previous: u64) -> Option<u64> {
    current
        .checked_sub(previous)
        .filter(|advance| *advance != 0)
}

fn validate_submitted_boundary(
    previous_batch_index: u64,
    previous: BatchBoundary,
    batch_index: u64,
    current: BatchBoundary,
) -> Result<(), AuthoritativeStateError> {
    let batch_advance = batch_index
        .checked_sub(previous_batch_index)
        .expect("submitted batch rows are visited in increasing key order");
    let adjacent = batch_advance == 1;
    if adjacent && current.first_zone_parent_hash != previous.final_zone_block_hash {
        return Err(AuthoritativeStateError::SubmittedBatchBlockDiscontinuity {
            previous_batch_index,
            batch_index,
        });
    }

    let prior_deposit = previous.final_processed_deposit;
    let current_deposit = current.first_processed_deposit;
    let deposit_regressed = current_deposit.number < prior_deposit.number
        || (current_deposit.number == prior_deposit.number
            && current_deposit.hash != prior_deposit.hash);
    if deposit_regressed || (adjacent && current_deposit != prior_deposit) {
        return Err(
            AuthoritativeStateError::SubmittedBatchDepositDiscontinuity {
                previous_batch_index,
                batch_index,
            },
        );
    }
    validate_submitted_tip_advance(
        previous_batch_index,
        previous.final_zone_height,
        previous.final_imported_tempo_block_number,
        batch_index,
        current.final_zone_height,
        current.final_imported_tempo_block_number,
    )
}

fn validate_submitted_tail(
    portal: &CreatedPortalState,
    previous_batch_index: u64,
    previous: BatchBoundary,
) -> Result<(), AuthoritativeStateError> {
    let settlement = portal.settlement();
    let batch_index = settlement.withdrawal_batch_index();
    if batch_index == previous_batch_index {
        return Ok(());
    }
    let previous_deposit = previous.final_processed_deposit;
    let current_deposit = settlement.last_submitted_deposit_cursor();
    if current_deposit.number < previous_deposit.number
        || (current_deposit.number == previous_deposit.number
            && current_deposit.hash != previous_deposit.hash)
    {
        return Err(
            AuthoritativeStateError::SubmittedBatchDepositDiscontinuity {
                previous_batch_index,
                batch_index,
            },
        );
    }
    validate_submitted_tip_advance(
        previous_batch_index,
        previous.final_zone_height,
        previous.final_imported_tempo_block_number,
        batch_index,
        u64::try_from(settlement.zone_height())
            .expect("counter validation bounds the Portal Zone height"),
        settlement.last_synced_tempo_block_number(),
    )
}

fn validate_submitted_tip_advance(
    previous_batch_index: u64,
    previous_zone_height: u64,
    previous_tempo_height: u64,
    batch_index: u64,
    current_zone_height: u64,
    current_tempo_height: u64,
) -> Result<(), AuthoritativeStateError> {
    let batch_advance = batch_index
        .checked_sub(previous_batch_index)
        .expect("submitted batch boundaries are visited in increasing order");
    let zone_advance = current_zone_height.saturating_sub(previous_zone_height);
    if zone_advance < batch_advance {
        return Err(
            AuthoritativeStateError::SubmittedBatchZoneHeightNotIncreasing {
                previous_batch_index,
                batch_index,
                previous_height: previous_zone_height,
                current_height: current_zone_height,
                minimum_advance: batch_advance,
            },
        );
    }
    let tempo_advance = current_tempo_height.saturating_sub(previous_tempo_height);
    if tempo_advance < batch_advance {
        return Err(
            AuthoritativeStateError::SubmittedBatchTempoHeightNotIncreasing {
                previous_batch_index,
                batch_index,
                previous_height: previous_tempo_height,
                current_height: current_tempo_height,
                minimum_advance: batch_advance,
            },
        );
    }
    if zone_advance != tempo_advance {
        return Err(AuthoritativeStateError::BatchTipAdvanceMismatch {
            previous_batch_index,
            batch_index,
            zone_advance,
            tempo_advance,
        });
    }
    Ok(())
}

fn validate_terminal_boundary(
    state: &ModelState,
    portal: &CreatedPortalState,
    portal_batch_index: u64,
    last_batch_index: u64,
) -> Result<(), AuthoritativeStateError> {
    if portal_batch_index != last_batch_index {
        return Ok(());
    }
    let settlement = portal.settlement();
    let batch_start = state.zone().batch_start();
    if settlement.block_hash() != batch_start.first_zone_parent_hash()
        || settlement.last_submitted_deposit_cursor().hash
            != batch_start.first_processed_deposit().hash()
        || settlement.last_submitted_deposit_cursor().number
            != batch_start.first_processed_deposit().number()
    {
        return Err(AuthoritativeStateError::PortalZoneAccumulatorMismatch);
    }
    Ok(())
}

fn validate_cursors(
    state: &ModelState,
    batch_index: u64,
    boundary: BatchBoundary,
) -> Result<(), AuthoritativeStateError> {
    let first = cursor(
        CursorKind::BatchFirstProcessed { batch_index },
        boundary.first_processed_deposit.hash,
        boundary.first_processed_deposit.number,
    )?;
    let final_cursor = cursor(
        CursorKind::BatchFinalProcessed { batch_index },
        boundary.final_processed_deposit.hash,
        boundary.final_processed_deposit.number,
    )?;
    let zone = cursor(
        CursorKind::ZoneProcessedDeposit,
        state.zone().processed_deposit_cursor().hash(),
        state.zone().processed_deposit_cursor().number(),
    )?;
    require_cursor_prefix(first, final_cursor)?;
    require_cursor_prefix(final_cursor, zone)
}

fn validate_last_accumulator(
    state: &ModelState,
    batch_index: u64,
    batch: &FinalizedBatchState,
) -> Result<(), AuthoritativeStateError> {
    let boundary = batch.boundary();
    let start = state.zone().batch_start();
    if state.zone().last_batch().withdrawal_queue_hash() != batch.members().withdrawal_queue_hash()
        || start.first_zone_parent_hash() != boundary.final_zone_block_hash
        || start.first_processed_deposit().hash() != boundary.final_processed_deposit.hash
        || start.first_processed_deposit().number() != boundary.final_processed_deposit.number
        || start.first_withdrawal_index() != batch.members().withdrawal_range_end()
    {
        return Err(AuthoritativeStateError::LastBatchAccumulatorMismatch { batch_index });
    }
    Ok(())
}

fn validate_latest_submission(
    portal: &CreatedPortalState,
    batch_index: u64,
    batch: &FinalizedBatchState,
) -> Result<(), AuthoritativeStateError> {
    let boundary = batch.boundary();
    let settlement = portal.settlement();
    if settlement.block_hash() != boundary.final_zone_block_hash
        || settlement.last_synced_tempo_block_number() != boundary.final_imported_tempo_block_number
        || settlement.last_submitted_deposit_cursor() != boundary.final_processed_deposit
        || settlement.zone_height() != U256::from(boundary.final_zone_height)
    {
        return Err(AuthoritativeStateError::SubmittedBatchSettlementMismatch { batch_index });
    }
    Ok(())
}

fn validate_submitted_queue(
    portal: &CreatedPortalState,
    submitted: &BTreeMap<U256, BatchId>,
) -> Result<(), AuthoritativeStateError> {
    let settlement = portal.settlement();
    let expected = settlement.withdrawal_queue_tail() - settlement.withdrawal_queue_head();
    if expected != U256::from(u64::try_from(submitted.len()).expect("owner count fits u64")) {
        return Err(AuthoritativeStateError::SubmittedQueueOwnerCountMismatch {
            expected,
            actual: submitted.len(),
        });
    }
    let mut prior_batch = None;
    for (ordinal, (actual_index, batch)) in submitted.iter().enumerate() {
        let expected_index = settlement.withdrawal_queue_head()
            + U256::from(u64::try_from(ordinal).expect("bounded queue ordinal fits u64"));
        if *actual_index != expected_index {
            return Err(AuthoritativeStateError::SubmittedQueueIndexGap {
                expected: expected_index,
                actual: *actual_index,
            });
        }
        let batch_index = batch.withdrawal_batch_index.get();
        if prior_batch.is_some_and(|prior| batch_index <= prior) {
            return Err(AuthoritativeStateError::SubmittedQueueBatchOrder { batch_index });
        }
        prior_batch = Some(batch_index);
    }
    Ok(())
}
