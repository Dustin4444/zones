use super::*;

#[test]
fn rejects_batch_owner_and_submitted_queue_incoherence() {
    let valid = submitted_batch_state();

    let mut missing_withdrawal = valid.clone();
    missing_withdrawal.withdrawals.clear();
    assert_eq!(
        validate_authoritative(&missing_withdrawal),
        Err(AuthoritativeStateError::BatchWithdrawalMissing {
            batch_index: 1,
            withdrawal_index: 1,
        })
    );

    let mut missing_queue_owner = valid.clone();
    missing_queue_owner.batches.clear();
    missing_queue_owner.withdrawals.clear();
    assert_eq!(
        validate_authoritative(&missing_queue_owner),
        Err(AuthoritativeStateError::SubmittedQueueOwnerCountMismatch {
            expected: U256::ONE,
            actual: 0,
        })
    );

    let mut bad_batch_index = valid;
    let owner = bad_batch_index
        .batches
        .remove(&BatchId {
            zone_id: ZONE_ID,
            withdrawal_batch_index: NonZeroU64::new(1).unwrap(),
        })
        .unwrap();
    bad_batch_index.batches.insert(
        BatchId {
            zone_id: ZONE_ID,
            withdrawal_batch_index: NonZeroU64::new(2).unwrap(),
        },
        owner,
    );
    assert_eq!(
        validate_authoritative(&bad_batch_index),
        Err(AuthoritativeStateError::BatchBeyondLastIndex {
            batch_index: 2,
            last_batch_index: 1,
        })
    );
}

#[test]
fn unsubmitted_batches_are_exact_and_open_member_ranges_are_contiguous() {
    let mut missing = submitted_batch_state();
    missing.zone.last_batch = ZoneLastBatch::for_test(B256::repeat_byte(0xd0), 3);
    assert_eq!(
        validate_authoritative(&missing),
        Err(AuthoritativeStateError::UnsubmittedBatchMissing { batch_index: 2 })
    );

    let mut first_gap = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(7),
            withdrawal_liability: U256::ZERO,
        },
    );
    let processed = B256::repeat_byte(0xd1);
    set_processed_cursor(&mut first_gap, processed, 2);
    first_gap.zone.next_withdrawal_index = 7;
    let owners = [failed_withdrawal(1, 3), failed_withdrawal(2, 4)];
    first_gap
        .withdrawals
        .insert(withdrawal_id(5), owners[0].clone());
    first_gap
        .withdrawals
        .insert(withdrawal_id(6), owners[1].clone());
    let members = BatchMembers::from_withdrawals(
        5,
        &owners
            .iter()
            .map(finalized_preimage)
            .cloned()
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let boundary = BatchBoundary {
        first_zone_parent_hash: B256::ZERO,
        final_zone_block_hash: B256::repeat_byte(0xd2),
        first_processed_deposit: DepositCursor {
            hash: B256::ZERO,
            number: 0,
        },
        final_processed_deposit: DepositCursor {
            hash: processed,
            number: 2,
        },
        final_imported_tempo_block_number: 1,
        final_zone_height: 1,
    };
    first_gap.batches.insert(
        batch_id(1),
        BatchOwner::Finalized(FinalizedBatchState::new(boundary, members)),
    );
    first_gap.zone.last_batch = ZoneLastBatch::for_test(members.withdrawal_queue_hash(), 1);
    first_gap.zone.batch_start = BatchStart::new(
        boundary.final_zone_block_hash,
        ZoneProcessedDepositCursor::new(processed, 2),
        7,
    );
    assert_eq!(
        validate_authoritative(&first_gap),
        Err(AuthoritativeStateError::OpenBatchRangeDiscontinuity {
            batch_index: 1,
            expected: 0,
            actual: 5,
        })
    );

    let mut middle_gap = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(4),
            withdrawal_liability: U256::ZERO,
        },
    );
    let processed = B256::repeat_byte(0xd3);
    set_processed_cursor(&mut middle_gap, processed, 4);
    middle_gap.zone.next_withdrawal_index = 5;
    let rows = [
        (0, failed_withdrawal(1, 1)),
        (1, failed_withdrawal(2, 1)),
        (3, failed_withdrawal(3, 1)),
        (4, failed_withdrawal(4, 1)),
    ];
    for (index, owner) in &rows {
        middle_gap
            .withdrawals
            .insert(withdrawal_id(*index), owner.clone());
    }
    let first_members = BatchMembers::from_withdrawals(
        0,
        &rows[..2]
            .iter()
            .map(|(_, owner)| finalized_preimage(owner))
            .cloned()
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let second_members = BatchMembers::from_withdrawals(
        3,
        &rows[2..]
            .iter()
            .map(|(_, owner)| finalized_preimage(owner))
            .cloned()
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let first_boundary = BatchBoundary {
        first_zone_parent_hash: B256::ZERO,
        final_zone_block_hash: B256::repeat_byte(0xd4),
        first_processed_deposit: DepositCursor {
            hash: B256::ZERO,
            number: 0,
        },
        final_processed_deposit: DepositCursor {
            hash: B256::repeat_byte(0xd5),
            number: 2,
        },
        final_imported_tempo_block_number: 1,
        final_zone_height: 1,
    };
    let second_boundary = BatchBoundary {
        first_zone_parent_hash: first_boundary.final_zone_block_hash,
        final_zone_block_hash: B256::repeat_byte(0xd6),
        first_processed_deposit: first_boundary.final_processed_deposit,
        final_processed_deposit: DepositCursor {
            hash: processed,
            number: 4,
        },
        final_imported_tempo_block_number: 2,
        final_zone_height: 2,
    };
    middle_gap.batches.insert(
        batch_id(1),
        BatchOwner::Finalized(FinalizedBatchState::new(first_boundary, first_members)),
    );
    middle_gap.batches.insert(
        batch_id(2),
        BatchOwner::Finalized(FinalizedBatchState::new(second_boundary, second_members)),
    );
    middle_gap.zone.last_batch = ZoneLastBatch::for_test(second_members.withdrawal_queue_hash(), 2);
    middle_gap.zone.batch_start = BatchStart::new(
        second_boundary.final_zone_block_hash,
        ZoneProcessedDepositCursor::new(processed, 4),
        5,
    );
    assert_eq!(
        validate_authoritative(&middle_gap),
        Err(AuthoritativeStateError::OpenBatchRangeDiscontinuity {
            batch_index: 2,
            expected: 2,
            actual: 3,
        })
    );
}

#[test]
fn terminal_batch_deletion_still_links_portal_and_zone_anchors() {
    let mut state = created();
    state.zone.last_batch = ZoneLastBatch::for_test(B256::ZERO, 1);
    state.zone.batch_start =
        BatchStart::new(B256::repeat_byte(0xe1), ZoneProcessedDepositCursor::ZERO, 0);
    let PortalLifecycle::Created(portal) = &mut state.portal else {
        unreachable!()
    };
    portal.settlement = PortalSettlementState::new(
        1,
        B256::repeat_byte(0xe2),
        1,
        DepositCursor {
            hash: B256::ZERO,
            number: 0,
        },
        U256::ONE,
        U256::ZERO,
        U256::ZERO,
    );
    assert_eq!(
        validate_authoritative(&state),
        Err(AuthoritativeStateError::PortalZoneAccumulatorMismatch)
    );
}

#[test]
fn lifecycle_origins_cannot_have_two_primary_open_phases() {
    let mut deposit = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(6),
            withdrawal_liability: U256::ZERO,
        },
    );
    set_processed_cursor(&mut deposit, B256::repeat_byte(0xe3), 1);
    deposit.zone.next_withdrawal_index = 1;
    deposit.withdrawals.insert(
        withdrawal_id(0),
        WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(
            FailedDepositPendingWithdrawal::from_parts(
                deposit_id(1),
                token(),
                Address::repeat_byte(0xe4),
                5,
            ),
        )),
    );
    deposit.portal_refunds.insert(
        PortalRefundId {
            token: token(),
            recipient: Address::repeat_byte(0xe4),
            failed_deposit: deposit_id(1),
        },
        PortalRefundOwner::Pending { amount: 1 },
    );
    assert_eq!(
        validate_authoritative(&deposit),
        Err(AuthoritativeStateError::DuplicateDepositOriginOwner { deposit_number: 1 })
    );

    let mut withdrawal = valid_user_state();
    withdrawal.inbox_refunds.insert(
        InboxRefundId {
            token: token(),
            recipient: Address::repeat_byte(0xe5),
            user_withdrawal: withdrawal_id(0),
        },
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(1).unwrap(),
        },
    );
    assert_eq!(
        validate_authoritative(&withdrawal),
        Err(AuthoritativeStateError::DuplicateWithdrawalOriginOwner {
            withdrawal_index: 0,
        })
    );

    let mut queued = valid_bounce_back_state();
    queued.inbox_refunds.insert(
        InboxRefundId {
            token: token(),
            recipient: Address::repeat_byte(0xe6),
            user_withdrawal: withdrawal_id(0),
        },
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(1).unwrap(),
        },
    );
    assert_eq!(
        validate_authoritative(&queued),
        Err(AuthoritativeStateError::DuplicateWithdrawalOriginOwner {
            withdrawal_index: 0,
        })
    );
}
