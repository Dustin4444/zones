use super::*;

fn submitted_chain() -> ModelState {
    let mut state = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(9),
            withdrawal_liability: U256::ZERO,
        },
    );
    let final_deposit_hash = B256::repeat_byte(0xf0);
    set_processed_cursor(&mut state, final_deposit_hash, 3);
    state.zone.next_withdrawal_index = 3;

    let owners = [
        failed_withdrawal(1, 2),
        failed_withdrawal(2, 3),
        failed_withdrawal(3, 4),
    ];
    for (withdrawal_index, owner) in owners.iter().enumerate() {
        state.withdrawals.insert(
            withdrawal_id(u64::try_from(withdrawal_index).unwrap()),
            owner.clone(),
        );
    }

    let first_deposit = DepositCursor {
        hash: B256::repeat_byte(0xf1),
        number: 1,
    };
    let first_boundary = BatchBoundary {
        first_zone_parent_hash: B256::ZERO,
        final_zone_block_hash: B256::repeat_byte(0xf2),
        first_processed_deposit: DepositCursor::default(),
        final_processed_deposit: first_deposit,
        final_imported_tempo_block_number: 10,
        final_zone_height: 20,
    };
    let second_boundary = BatchBoundary {
        first_zone_parent_hash: first_boundary.final_zone_block_hash,
        final_zone_block_hash: B256::repeat_byte(0xf3),
        first_processed_deposit: first_deposit,
        final_processed_deposit: DepositCursor {
            hash: final_deposit_hash,
            number: 3,
        },
        final_imported_tempo_block_number: 11,
        final_zone_height: 21,
    };
    let first_members = batch_members(0, &owners[..1]);
    let second_members = batch_members(1, &owners[1..]);
    state.batches.insert(
        batch_id(1),
        BatchOwner::Submitted(
            SubmittedBatchState::new(
                FinalizedBatchState::new(first_boundary, first_members),
                PortalQueueId::new(portal(), U256::ZERO).unwrap(),
            )
            .unwrap(),
        ),
    );
    state.batches.insert(
        batch_id(2),
        BatchOwner::Submitted(
            SubmittedBatchState::new(
                FinalizedBatchState::new(second_boundary, second_members),
                PortalQueueId::new(portal(), U256::ONE).unwrap(),
            )
            .unwrap(),
        ),
    );
    state.zone.last_batch = ZoneLastBatch::for_test(second_members.withdrawal_queue_hash(), 2);
    state.zone.batch_start = BatchStart::new(
        second_boundary.final_zone_block_hash,
        ZoneProcessedDepositCursor::new(final_deposit_hash, 3),
        3,
    );
    set_settlement(&mut state, 2, second_boundary, U256::ZERO, U256::from(2));
    state
}

fn batch_members(first_withdrawal_index: u64, owners: &[WithdrawalOwner]) -> BatchMembers {
    BatchMembers::from_withdrawals(
        first_withdrawal_index,
        &owners
            .iter()
            .map(finalized_preimage)
            .cloned()
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

fn second_batch(state: &ModelState) -> (SubmittedBatchState, BatchBoundary, BatchMembers) {
    let BatchOwner::Submitted(submitted) = state.batches.get(&batch_id(2)).unwrap() else {
        unreachable!()
    };
    (
        submitted.clone(),
        submitted.batch().boundary(),
        submitted.batch().members(),
    )
}

fn replace_second_batch(state: &mut ModelState, boundary: BatchBoundary, members: BatchMembers) {
    state.batches.insert(
        batch_id(2),
        BatchOwner::Submitted(
            SubmittedBatchState::new(
                FinalizedBatchState::new(boundary, members),
                PortalQueueId::new(portal(), U256::ONE).unwrap(),
            )
            .unwrap(),
        ),
    );
}

fn set_settlement(
    state: &mut ModelState,
    batch_index: u64,
    boundary: BatchBoundary,
    head: U256,
    tail: U256,
) {
    let PortalLifecycle::Created(portal) = &mut state.portal else {
        unreachable!()
    };
    portal.settlement = PortalSettlementState::new(
        batch_index,
        boundary.final_zone_block_hash,
        boundary.final_imported_tempo_block_number,
        boundary.final_processed_deposit,
        U256::from(boundary.final_zone_height),
        head,
        tail,
    );
}

fn append_empty_finalized_batch(state: &mut ModelState, batch_index: u64, boundary: BatchBoundary) {
    let members =
        BatchMembers::from_withdrawals(state.zone.batch_start.first_withdrawal_index(), &[])
            .unwrap();
    state.batches.insert(
        batch_id(batch_index),
        BatchOwner::Finalized(FinalizedBatchState::new(boundary, members)),
    );
    state.zone.last_batch = ZoneLastBatch::for_test(B256::ZERO, batch_index);
    state.zone.batch_start = BatchStart::new(
        boundary.final_zone_block_hash,
        ZoneProcessedDepositCursor::new(
            boundary.final_processed_deposit.hash,
            boundary.final_processed_deposit.number,
        ),
        members.withdrawal_range_end(),
    );
}

#[test]
fn submitted_batches_form_one_monotonic_boundary_chain() {
    let state = submitted_chain();
    assert_eq!(validate_authoritative(&state), Ok(()));

    let mut block = state.clone();
    let (_, mut boundary, members) = second_batch(&block);
    boundary.first_zone_parent_hash = B256::repeat_byte(0xa1);
    replace_second_batch(&mut block, boundary, members);
    assert_eq!(
        validate_authoritative(&block),
        Err(AuthoritativeStateError::SubmittedBatchBlockDiscontinuity {
            previous_batch_index: 1,
            batch_index: 2,
        })
    );

    let mut deposit = state.clone();
    let (_, mut boundary, members) = second_batch(&deposit);
    boundary.first_processed_deposit.hash = B256::repeat_byte(0xa2);
    replace_second_batch(&mut deposit, boundary, members);
    assert_eq!(
        validate_authoritative(&deposit),
        Err(
            AuthoritativeStateError::SubmittedBatchDepositDiscontinuity {
                previous_batch_index: 1,
                batch_index: 2,
            }
        )
    );

    let mut zone_height = state.clone();
    let (_, mut boundary, members) = second_batch(&zone_height);
    boundary.final_zone_height = 20;
    replace_second_batch(&mut zone_height, boundary, members);
    set_settlement(&mut zone_height, 2, boundary, U256::ZERO, U256::from(2));
    assert_eq!(
        validate_authoritative(&zone_height),
        Err(
            AuthoritativeStateError::SubmittedBatchZoneHeightNotIncreasing {
                previous_batch_index: 1,
                batch_index: 2,
                previous_height: 20,
                current_height: 20,
                minimum_advance: 1,
            }
        )
    );

    let mut tempo_height = state.clone();
    let (_, mut boundary, members) = second_batch(&tempo_height);
    boundary.final_imported_tempo_block_number = 10;
    replace_second_batch(&mut tempo_height, boundary, members);
    set_settlement(&mut tempo_height, 2, boundary, U256::ZERO, U256::from(2));
    assert_eq!(
        validate_authoritative(&tempo_height),
        Err(
            AuthoritativeStateError::SubmittedBatchTempoHeightNotIncreasing {
                previous_batch_index: 1,
                batch_index: 2,
                previous_height: 10,
                current_height: 10,
                minimum_advance: 1,
            }
        )
    );

    let mut member_range = state;
    let (_, boundary, _) = second_batch(&member_range);
    let owners = [
        member_range
            .withdrawals
            .get(&withdrawal_id(1))
            .unwrap()
            .clone(),
        member_range
            .withdrawals
            .get(&withdrawal_id(2))
            .unwrap()
            .clone(),
    ];
    let moved_members = batch_members(0, &owners);
    replace_second_batch(&mut member_range, boundary, moved_members);
    assert_eq!(
        validate_authoritative(&member_range),
        Err(AuthoritativeStateError::OpenBatchRangeDiscontinuity {
            batch_index: 2,
            expected: 1,
            actual: 0,
        })
    );
}

fn submitted_chain_with_empty_middle_batch() -> ModelState {
    let mut state = submitted_chain();
    let (submitted, mut boundary, _) = second_batch(&state);
    state.batches.remove(&batch_id(2));
    boundary.first_zone_parent_hash = B256::repeat_byte(0xa5);
    boundary.final_imported_tempo_block_number = 12;
    boundary.final_zone_height = 22;
    state.batches.insert(
        batch_id(3),
        BatchOwner::Submitted(
            SubmittedBatchState::new(
                FinalizedBatchState::new(boundary, submitted.batch().members()),
                submitted.portal_queue(),
            )
            .unwrap(),
        ),
    );
    state.zone.last_batch =
        ZoneLastBatch::for_test(submitted.batch().members().withdrawal_queue_hash(), 3);
    set_settlement(&mut state, 3, boundary, U256::ZERO, U256::from(2));
    state
}

#[test]
fn submitted_batch_index_gaps_consume_distinct_zone_and_tempo_heights() {
    let state = submitted_chain_with_empty_middle_batch();
    assert_eq!(validate_authoritative(&state), Ok(()));

    let mut zone_height = state.clone();
    let BatchOwner::Submitted(submitted) = zone_height.batches.get(&batch_id(3)).unwrap() else {
        unreachable!()
    };
    let mut boundary = submitted.batch().boundary();
    let members = submitted.batch().members();
    let queue = submitted.portal_queue();
    boundary.final_zone_height = 21;
    zone_height.batches.insert(
        batch_id(3),
        BatchOwner::Submitted(
            SubmittedBatchState::new(FinalizedBatchState::new(boundary, members), queue).unwrap(),
        ),
    );
    set_settlement(&mut zone_height, 3, boundary, U256::ZERO, U256::from(2));
    assert_eq!(
        validate_authoritative(&zone_height),
        Err(
            AuthoritativeStateError::SubmittedBatchZoneHeightNotIncreasing {
                previous_batch_index: 1,
                batch_index: 3,
                previous_height: 20,
                current_height: 21,
                minimum_advance: 2,
            }
        )
    );

    let mut tempo_height = state;
    let BatchOwner::Submitted(submitted) = tempo_height.batches.get(&batch_id(3)).unwrap() else {
        unreachable!()
    };
    let mut boundary = submitted.batch().boundary();
    let members = submitted.batch().members();
    let queue = submitted.portal_queue();
    boundary.final_imported_tempo_block_number = 11;
    tempo_height.batches.insert(
        batch_id(3),
        BatchOwner::Submitted(
            SubmittedBatchState::new(FinalizedBatchState::new(boundary, members), queue).unwrap(),
        ),
    );
    set_settlement(&mut tempo_height, 3, boundary, U256::ZERO, U256::from(2));
    assert_eq!(
        validate_authoritative(&tempo_height),
        Err(
            AuthoritativeStateError::SubmittedBatchTempoHeightNotIncreasing {
                previous_batch_index: 1,
                batch_index: 3,
                previous_height: 10,
                current_height: 11,
                minimum_advance: 2,
            }
        )
    );

    let mut mismatched_advance = submitted_chain_with_empty_middle_batch();
    let BatchOwner::Submitted(submitted) = mismatched_advance.batches.get(&batch_id(3)).unwrap()
    else {
        unreachable!()
    };
    let mut boundary = submitted.batch().boundary();
    let members = submitted.batch().members();
    let queue = submitted.portal_queue();
    boundary.final_zone_height = 23;
    mismatched_advance.batches.insert(
        batch_id(3),
        BatchOwner::Submitted(
            SubmittedBatchState::new(FinalizedBatchState::new(boundary, members), queue).unwrap(),
        ),
    );
    set_settlement(
        &mut mismatched_advance,
        3,
        boundary,
        U256::ZERO,
        U256::from(2),
    );
    assert_eq!(
        validate_authoritative(&mismatched_advance),
        Err(AuthoritativeStateError::BatchTipAdvanceMismatch {
            previous_batch_index: 1,
            batch_index: 3,
            zone_advance: 3,
            tempo_advance: 2,
        })
    );
}

#[test]
fn unsubmitted_batches_strictly_advance_the_imported_tempo_height() {
    let mut state = submitted_chain();
    let prior_boundary = second_batch(&state).1;
    let boundary = BatchBoundary {
        first_zone_parent_hash: prior_boundary.final_zone_block_hash,
        final_zone_block_hash: B256::repeat_byte(0xa3),
        first_processed_deposit: prior_boundary.final_processed_deposit,
        final_processed_deposit: prior_boundary.final_processed_deposit,
        final_imported_tempo_block_number: prior_boundary.final_imported_tempo_block_number,
        final_zone_height: prior_boundary.final_zone_height + 1,
    };
    append_empty_finalized_batch(&mut state, 3, boundary);
    assert_eq!(
        validate_authoritative(&state),
        Err(AuthoritativeStateError::UnsubmittedBatchTempoHeightNotIncreasing { batch_index: 3 })
    );
}

#[test]
fn unsubmitted_boundaries_preserve_one_to_one_zone_and_tempo_progress() {
    let mut first = submitted_chain();
    let prior = second_batch(&first).1;
    let boundary = BatchBoundary {
        first_zone_parent_hash: prior.final_zone_block_hash,
        final_zone_block_hash: B256::repeat_byte(0xa6),
        first_processed_deposit: prior.final_processed_deposit,
        final_processed_deposit: prior.final_processed_deposit,
        final_imported_tempo_block_number: prior.final_imported_tempo_block_number + 1,
        final_zone_height: prior.final_zone_height + 2,
    };
    append_empty_finalized_batch(&mut first, 3, boundary);
    assert_eq!(
        validate_authoritative(&first),
        Err(AuthoritativeStateError::BatchTipAdvanceMismatch {
            previous_batch_index: 2,
            batch_index: 3,
            zone_advance: 2,
            tempo_advance: 1,
        })
    );

    let mut later = submitted_chain();
    let submitted = second_batch(&later).1;
    let third = BatchBoundary {
        first_zone_parent_hash: submitted.final_zone_block_hash,
        final_zone_block_hash: B256::repeat_byte(0xa7),
        first_processed_deposit: submitted.final_processed_deposit,
        final_processed_deposit: submitted.final_processed_deposit,
        final_imported_tempo_block_number: submitted.final_imported_tempo_block_number + 1,
        final_zone_height: submitted.final_zone_height + 1,
    };
    append_empty_finalized_batch(&mut later, 3, third);
    let fourth = BatchBoundary {
        first_zone_parent_hash: third.final_zone_block_hash,
        final_zone_block_hash: B256::repeat_byte(0xa8),
        first_processed_deposit: third.final_processed_deposit,
        final_processed_deposit: third.final_processed_deposit,
        final_imported_tempo_block_number: third.final_imported_tempo_block_number + 1,
        final_zone_height: third.final_zone_height + 2,
    };
    append_empty_finalized_batch(&mut later, 4, fourth);
    assert_eq!(
        validate_authoritative(&later),
        Err(AuthoritativeStateError::BatchTipAdvanceMismatch {
            previous_batch_index: 3,
            batch_index: 4,
            zone_advance: 2,
            tempo_advance: 1,
        })
    );
}

#[test]
fn first_batch_tip_offsets_are_not_compared_to_synthetic_zero_settlement() {
    let mut state = created();
    let boundary = BatchBoundary {
        first_zone_parent_hash: B256::ZERO,
        final_zone_block_hash: B256::repeat_byte(0xa9),
        first_processed_deposit: DepositCursor::default(),
        final_processed_deposit: DepositCursor::default(),
        final_imported_tempo_block_number: 3,
        final_zone_height: 10,
    };
    append_empty_finalized_batch(&mut state, 1, boundary);
    assert_eq!(validate_authoritative(&state), Ok(()));
}

#[test]
fn only_the_submitted_queue_head_may_be_partially_processed() {
    let mut state = submitted_chain();
    let (submitted, _, _) = second_batch(&state);
    let remaining_owner = state.withdrawals.get(&withdrawal_id(2)).unwrap().clone();
    let remaining_hash = withdrawal_queue_hash(&[finalized_preimage(&remaining_owner).clone()]);
    let processed_ahead = submitted.advance_partial(1, remaining_hash).unwrap();
    state
        .batches
        .insert(batch_id(2), BatchOwner::Submitted(processed_ahead));
    state.withdrawals.remove(&withdrawal_id(1));
    state.set_token_accounting_for_test(
        token(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(6),
            withdrawal_liability: U256::ZERO,
        },
    );
    assert_eq!(
        validate_authoritative(&state),
        Err(
            AuthoritativeStateError::SubmittedBatchProcessedAheadOfHead {
                batch_index: 2,
                queue_index: U256::ONE,
                head: U256::ZERO,
                ordinal: 1,
            }
        )
    );
}

#[test]
fn a_later_nonempty_batch_cannot_disappear_while_the_head_remains_open() {
    let mut state = submitted_chain();
    state.batches.remove(&batch_id(2));
    state.withdrawals.remove(&withdrawal_id(1));
    state.withdrawals.remove(&withdrawal_id(2));
    state.set_portal_withdrawal_queue_for_test(U256::ZERO, U256::ONE);
    state.set_token_accounting_for_test(
        token(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(2),
            withdrawal_liability: U256::ZERO,
        },
    );
    assert_eq!(
        validate_authoritative(&state),
        Err(
            AuthoritativeStateError::OpenBatchTerminalRangeDiscontinuity {
                batch_index: 1,
                range_end: 1,
                batch_start_index: 3,
            }
        )
    );
}

#[test]
fn submitted_terminal_empty_batches_may_leave_no_owner_row() {
    let mut state = submitted_chain();
    let empty_boundary = BatchBoundary {
        first_zone_parent_hash: state.zone.batch_start.first_zone_parent_hash(),
        final_zone_block_hash: B256::repeat_byte(0xa4),
        first_processed_deposit: DepositCursor {
            hash: state.zone.processed_deposit_cursor.hash(),
            number: state.zone.processed_deposit_cursor.number(),
        },
        final_processed_deposit: DepositCursor {
            hash: state.zone.processed_deposit_cursor.hash(),
            number: state.zone.processed_deposit_cursor.number(),
        },
        final_imported_tempo_block_number: 12,
        final_zone_height: 22,
    };
    state.zone.last_batch = ZoneLastBatch::for_test(B256::ZERO, 3);
    state.zone.batch_start = BatchStart::new(
        empty_boundary.final_zone_block_hash,
        state.zone.processed_deposit_cursor,
        3,
    );
    set_settlement(&mut state, 3, empty_boundary, U256::ZERO, U256::from(2));
    assert_eq!(validate_authoritative(&state), Ok(()));

    let mut insufficient = state.clone();
    let mut insufficient_boundary = empty_boundary;
    insufficient_boundary.final_zone_height = 21;
    set_settlement(
        &mut insufficient,
        3,
        insufficient_boundary,
        U256::ZERO,
        U256::from(2),
    );
    assert_eq!(
        validate_authoritative(&insufficient),
        Err(
            AuthoritativeStateError::SubmittedBatchZoneHeightNotIncreasing {
                previous_batch_index: 2,
                batch_index: 3,
                previous_height: 21,
                current_height: 21,
                minimum_advance: 1,
            }
        )
    );

    let mut mismatched = state.clone();
    let mut mismatched_boundary = empty_boundary;
    mismatched_boundary.final_zone_height = 23;
    set_settlement(
        &mut mismatched,
        3,
        mismatched_boundary,
        U256::ZERO,
        U256::from(2),
    );
    assert_eq!(
        validate_authoritative(&mismatched),
        Err(AuthoritativeStateError::BatchTipAdvanceMismatch {
            previous_batch_index: 2,
            batch_index: 3,
            zone_advance: 2,
            tempo_advance: 1,
        })
    );

    let mut deposit_regression = state;
    let regressed_deposit = DepositCursor::default();
    let mut regressed_boundary = empty_boundary;
    regressed_boundary.final_processed_deposit = regressed_deposit;
    deposit_regression.zone.batch_start = BatchStart::new(
        empty_boundary.final_zone_block_hash,
        ZoneProcessedDepositCursor::ZERO,
        3,
    );
    set_settlement(
        &mut deposit_regression,
        3,
        regressed_boundary,
        U256::ZERO,
        U256::from(2),
    );
    assert_eq!(
        validate_authoritative(&deposit_regression),
        Err(
            AuthoritativeStateError::SubmittedBatchDepositDiscontinuity {
                previous_batch_index: 2,
                batch_index: 3,
            }
        )
    );
}
