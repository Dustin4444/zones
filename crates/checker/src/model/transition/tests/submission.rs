use alloy_primitives::{B256, U256};

use super::support::*;
use crate::model::{
    constants::{NO_WITHDRAWAL_QUEUE_INDEX, WITHDRAWAL_QUEUE_CAPACITY},
    input::{
        BatchBlockTransitionInput, BatchDepositTransitionInput, BatchSubmissionInput,
        ImportedTempoOperation,
    },
    output::{ExpectedBatchSubmission, ExpectedImportedTempoOperation, ExpectedOutputs},
    ownership::{BatchId, BatchOwner, DepositCursor, FinalizedBatchState},
    state::ModelState,
    transition::{BlockTransitionMismatch, DepositTransitionMismatch, ModelError},
};

#[derive(Debug, Clone, Copy)]
struct SubmissionParts {
    tempo_block_number: u64,
    previous_block_hash: B256,
    next_block_hash: B256,
    previous_deposit: DepositCursor,
    next_deposit: DepositCursor,
    withdrawal_queue_hash: B256,
    next_zone_height: U256,
}

impl SubmissionParts {
    fn exact(batch: &FinalizedBatchState) -> Self {
        let boundary = batch.boundary();
        Self {
            tempo_block_number: boundary.final_imported_tempo_block_number,
            previous_block_hash: boundary.first_zone_parent_hash,
            next_block_hash: boundary.final_zone_block_hash,
            previous_deposit: boundary.first_processed_deposit,
            next_deposit: boundary.final_processed_deposit,
            withdrawal_queue_hash: batch.members().withdrawal_queue_hash(),
            next_zone_height: U256::from(boundary.final_zone_height),
        }
    }

    fn input(self) -> BatchSubmissionInput {
        BatchSubmissionInput::new(
            self.tempo_block_number,
            BatchBlockTransitionInput::new(self.previous_block_hash, self.next_block_hash),
            BatchDepositTransitionInput::new(self.previous_deposit, self.next_deposit),
            self.withdrawal_queue_hash,
            self.next_zone_height,
        )
    }
}

fn only_batch_submission(output: &ExpectedOutputs) -> &ExpectedBatchSubmission {
    let [ExpectedImportedTempoOperation::BatchSubmitted(submission)] =
        output.imported_tempo_block().operations()
    else {
        panic!("one batch-submission output expected")
    };
    submission
}

fn block_mismatch(
    batch_index: u64,
    expected: SubmissionParts,
    actual: SubmissionParts,
) -> ModelError {
    ModelError::BatchBlockTransitionMismatch {
        withdrawal_batch_index: batch_index,
        details: Box::new(BlockTransitionMismatch {
            expected_previous: expected.previous_block_hash,
            actual_previous: actual.previous_block_hash,
            expected_next: expected.next_block_hash,
            actual_next: actual.next_block_hash,
        }),
    }
}

fn deposit_mismatch(
    batch_index: u64,
    expected: SubmissionParts,
    actual: SubmissionParts,
) -> ModelError {
    ModelError::BatchDepositTransitionMismatch {
        withdrawal_batch_index: batch_index,
        details: Box::new(DepositTransitionMismatch {
            expected_previous: expected.previous_deposit,
            actual_previous: actual.previous_deposit,
            expected_next: expected.next_deposit,
            actual_next: actual.next_deposit,
        }),
    }
}

fn assert_submission_rejections(
    state: &mut ModelState,
    cases: impl IntoIterator<Item = (SubmissionParts, ModelError)>,
) {
    for (parts, expected_error) in cases {
        let operation = ImportedTempoOperation::BatchSubmitted(Box::new(parts.input()));
        assert_eq!(
            reject_imported_atomically(state, U256::ZERO, vec![operation]),
            expected_error
        );
    }
}

#[test]
fn queue_progress_rejects_counter_underflow_and_over_capacity() {
    use super::super::validated_portal_queue_len;

    assert_eq!(
        validated_portal_queue_len(U256::ZERO, WITHDRAWAL_QUEUE_CAPACITY),
        Ok(WITHDRAWAL_QUEUE_CAPACITY)
    );
    for (head, tail) in [
        (U256::ONE, U256::ZERO),
        (U256::ZERO, WITHDRAWAL_QUEUE_CAPACITY + U256::ONE),
    ] {
        assert_eq!(
            validated_portal_queue_len(head, tail),
            Err(ModelError::InvalidPortalWithdrawalQueueProgress { head, tail })
        );
    }
}

#[test]
fn submission_reports_tail_counter_overflow_before_the_reserved_index() {
    let token = token(0xac);
    let mut state = funded_state(token, U256::from(1_000_000));
    let batch = finalize_initial_token_users(&mut state, 1, &[(0xc1, 10, 0)]);
    state.set_portal_withdrawal_queue_for_test(U256::MAX, U256::MAX);
    let submission = exact_submission(&state, batch);

    assert_eq!(
        reject_imported_atomically(
            &mut state,
            U256::ZERO,
            vec![ImportedTempoOperation::BatchSubmitted(
                Box::new(submission,)
            )],
        ),
        ModelError::PortalWithdrawalQueueCounterOverflow
    );
}

fn fill_portal_ring(state: &mut ModelState) {
    assert_eq!(WITHDRAWAL_QUEUE_CAPACITY, U256::from(100));
    for logical_index in 0_u64..100 {
        let batch = finalize_initial_token_users(
            state,
            logical_index + 1,
            &[(u8::try_from(logical_index).unwrap(), 1, 0)],
        );
        let output = submit_finalized_batch(state, batch);
        let event = only_batch_submission(&output);
        assert_eq!(event.withdrawal_queue_index(), U256::from(logical_index));
    }
    assert_queue_progress(state, U256::ZERO, WITHDRAWAL_QUEUE_CAPACITY);
}

fn assert_exact_submission_output(
    output: &ExpectedOutputs,
    batch: BatchId,
    exact: SubmissionParts,
) {
    let event = only_batch_submission(output);
    assert_eq!(event.batch(), batch);
    assert_eq!(event.withdrawal_queue_index(), U256::ZERO);
    assert_eq!(event.next_block_hash(), exact.next_block_hash);
    assert_eq!(
        event.next_processed_deposit_queue_hash(),
        exact.next_deposit.hash
    );
    assert_eq!(
        event.last_processed_deposit_number(),
        exact.next_deposit.number
    );
    assert_eq!(event.withdrawal_queue_hash(), exact.withdrawal_queue_hash);
}

#[test]
fn empty_batch_must_submit_before_the_following_nonempty_batch() {
    let token = token(0xa1);
    let mut state = funded_state(token, U256::from(1_000_000));
    let empty = finalize_initial_token_users(&mut state, 1, &[]);
    let nonempty = finalize_initial_token_users(&mut state, 2, &[(0x11, 10, 0)]);

    let out_of_order = exact_submission(&state, nonempty);
    let error = reject_imported_atomically(
        &mut state,
        U256::ZERO,
        vec![ImportedTempoOperation::BatchSubmitted(Box::new(
            out_of_order,
        ))],
    );
    assert!(matches!(
        error,
        ModelError::BatchTempoBlockMismatch {
            withdrawal_batch_index: 1,
            ..
        }
    ));

    let output = submit_finalized_batch(&mut state, empty);
    let event = only_batch_submission(&output);
    assert_eq!(event.batch(), empty);
    assert_eq!(event.withdrawal_queue_index(), NO_WITHDRAWAL_QUEUE_INDEX);
    assert!(state.batch(empty).is_none());
    let settlement = state.portal().created().unwrap().settlement();
    assert_eq!(settlement.withdrawal_batch_index(), 1);
    assert_eq!(settlement.withdrawal_queue_head(), U256::ZERO);
    assert_eq!(settlement.withdrawal_queue_tail(), U256::ZERO);

    let output = submit_finalized_batch(&mut state, nonempty);
    let event = only_batch_submission(&output);
    assert_eq!(event.batch(), nonempty);
    assert_eq!(event.withdrawal_queue_index(), U256::ZERO);
    let Some(BatchOwner::Submitted(submitted)) = state.batch(nonempty) else {
        panic!("non-empty batch must own a Portal queue slot")
    };
    assert_eq!(submitted.next_processing_ordinal(), 0);
    assert_eq!(
        submitted.remaining_queue_hash(),
        event.withdrawal_queue_hash()
    );
}

#[test]
fn duplicate_submission_targets_the_next_batch_and_fails_atomically() {
    let token = token(0xab);
    let mut state = funded_state(token, U256::from(1_000_000));
    let batch = finalize_initial_token_users(&mut state, 3, &[(0xb1, 10, 0)]);
    let submission = exact_submission(&state, batch);

    commit_imported(
        &mut state,
        4,
        U256::ZERO,
        vec![ImportedTempoOperation::BatchSubmitted(Box::new(
            submission.clone(),
        ))],
    )
    .unwrap();
    assert_eq!(
        reject_imported_atomically(
            &mut state,
            U256::ZERO,
            vec![ImportedTempoOperation::BatchSubmitted(Box::new(submission))],
        ),
        ModelError::BatchOwnerMissing {
            withdrawal_batch_index: 2,
        }
    );
}

#[test]
fn submission_checks_every_committed_field_and_accepts_the_exact_boundary() {
    let token = token(0xa2);
    let mut state = funded_state(token, U256::from(1_000_000));
    let batch = finalize_initial_token_users(&mut state, 11, &[(0x21, 17, 0)]);
    let exact = SubmissionParts::exact(finalized_batch(&state, batch));
    let batch_index = batch.withdrawal_batch_index.get();

    let mut bad_tempo = exact;
    bad_tempo.tempo_block_number += 1;
    let mut bad_height = exact;
    bad_height.next_zone_height += U256::ONE;
    let mut bad_previous_block = exact;
    bad_previous_block.previous_block_hash = B256::repeat_byte(0x31);
    let mut bad_next_block = exact;
    bad_next_block.next_block_hash = B256::repeat_byte(0x32);
    let mut bad_previous_deposit = exact;
    bad_previous_deposit.previous_deposit.hash = B256::repeat_byte(0x33);
    let mut bad_next_deposit = exact;
    bad_next_deposit.next_deposit.number += 1;
    let mut bad_withdrawals = exact;
    bad_withdrawals.withdrawal_queue_hash = B256::repeat_byte(0x34);

    let cases = [
        (
            bad_tempo,
            ModelError::BatchTempoBlockMismatch {
                withdrawal_batch_index: batch_index,
                expected: exact.tempo_block_number,
                actual: bad_tempo.tempo_block_number,
            },
        ),
        (
            bad_height,
            ModelError::BatchZoneHeightMismatch {
                withdrawal_batch_index: batch_index,
                expected: exact.next_zone_height,
                actual: bad_height.next_zone_height,
            },
        ),
        (
            bad_previous_block,
            block_mismatch(batch_index, exact, bad_previous_block),
        ),
        (
            bad_next_block,
            block_mismatch(batch_index, exact, bad_next_block),
        ),
        (
            bad_previous_deposit,
            deposit_mismatch(batch_index, exact, bad_previous_deposit),
        ),
        (
            bad_next_deposit,
            deposit_mismatch(batch_index, exact, bad_next_deposit),
        ),
        (
            bad_withdrawals,
            ModelError::BatchWithdrawalQueueHashMismatch {
                withdrawal_batch_index: batch_index,
                expected: exact.withdrawal_queue_hash,
                actual: bad_withdrawals.withdrawal_queue_hash,
            },
        ),
    ];

    assert_submission_rejections(&mut state, cases);

    let output = submit_finalized_batch(&mut state, batch);
    assert_exact_submission_output(&output, batch, exact);
}

#[test]
fn full_ring_accepts_empty_then_reuses_capacity_without_aliasing_logical_indices() {
    let token = token(0xa9);
    let mut state = funded_state(token, U256::from(20_000_000));
    fill_portal_ring(&mut state);

    let empty = finalize_initial_token_users(&mut state, 101, &[]);
    let empty_output = submit_finalized_batch(&mut state, empty);
    assert_eq!(
        only_batch_submission(&empty_output).withdrawal_queue_index(),
        NO_WITHDRAWAL_QUEUE_INDEX
    );
    assert_queue_progress(&state, U256::ZERO, WITHDRAWAL_QUEUE_CAPACITY);

    let next = finalize_initial_token_users(&mut state, 102, &[(0xb1, 1, 0)]);
    let next_submission = exact_submission(&state, next);
    assert_eq!(
        reject_imported_atomically(
            &mut state,
            U256::ZERO,
            vec![ImportedTempoOperation::BatchSubmitted(Box::new(
                next_submission.clone(),
            ))],
        ),
        ModelError::PortalWithdrawalQueueFull
    );

    let first = finalized_preimage(&state, 0);
    commit_imported(
        &mut state,
        103,
        U256::ZERO,
        vec![withdrawals_processed(
            vec![first],
            B256::ZERO,
            user_delivered_outcomes(1),
        )],
    )
    .unwrap();
    assert_queue_progress(&state, U256::ONE, WITHDRAWAL_QUEUE_CAPACITY);

    let output = commit_imported(
        &mut state,
        104,
        U256::ZERO,
        vec![ImportedTempoOperation::BatchSubmitted(Box::new(
            next_submission,
        ))],
    )
    .unwrap();
    let event = only_batch_submission(&output);
    assert_eq!(event.withdrawal_queue_index(), U256::from(100));
    let Some(BatchOwner::Submitted(second_slot)) = state.batch(batch_id(2)) else {
        panic!("second batch must remain queued")
    };
    let Some(BatchOwner::Submitted(new_slot)) = state.batch(next) else {
        panic!("new batch must occupy the newly available logical slot")
    };
    assert_eq!(second_slot.portal_queue().logical_queue_index(), U256::ONE);
    assert_eq!(
        new_slot.portal_queue().logical_queue_index(),
        U256::from(100)
    );
    assert_ne!(second_slot.portal_queue(), new_slot.portal_queue());
    assert_queue_progress(&state, U256::ONE, U256::from(101));
}
