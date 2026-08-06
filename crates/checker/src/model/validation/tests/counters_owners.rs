use super::*;

#[test]
fn accepts_empty_lifecycle_and_coherent_owner_graphs() {
    assert_eq!(
        validate_authoritative(&ModelState::awaiting_creation(PortalIdentity::new(
            portal(),
            ZONE_ID,
            token(),
        ))),
        Ok(())
    );
    assert_eq!(validate_authoritative(&created()), Ok(()));

    let mut portal_only = created();
    portal_only.tokens.insert(
        token(),
        crate::model::state::TokenState::for_test(
            crate::model::state::TokenPhase::PendingZoneEnable,
            TokenAccounting::ZERO,
        ),
    );
    portal_only.pending_deposits.insert(
        deposit_id(1),
        DepositOwner::PendingOrdinary {
            preimage: ordinary(token(), Address::repeat_byte(0x7a)),
        },
    );
    let cursor_hash = portal_only
        .pending_deposits
        .get(&deposit_id(1))
        .unwrap()
        .queue_member()
        .hash_after(B256::ZERO);
    set_portal_cursor(&mut portal_only, cursor_hash, 1);
    portal_only.set_token_accounting_for_test(
        token(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(9),
            withdrawal_liability: U256::ZERO,
        },
    );
    assert_eq!(validate_authoritative(&portal_only), Ok(()));

    assert_eq!(validate_authoritative(&valid_user_state()), Ok(()));
    assert_eq!(validate_authoritative(&valid_bounce_back_state()), Ok(()));
    assert_eq!(validate_authoritative(&submitted_batch_state()), Ok(()));
}

#[test]
fn rejects_awaiting_creation_records_and_portal_queue_counter_failures() {
    let mut awaiting =
        ModelState::awaiting_creation(PortalIdentity::new(portal(), ZONE_ID, token()));
    awaiting.tokens.insert(
        token(),
        crate::model::state::TokenState::for_test(
            crate::model::state::TokenPhase::ZoneEnabled,
            TokenAccounting::ZERO,
        ),
    );
    assert_eq!(
        validate_authoritative(&awaiting),
        Err(AuthoritativeStateError::AwaitingCreationHasLifecycleState)
    );

    let mut reversed = created();
    reversed.set_portal_withdrawal_queue_for_test(U256::ONE, U256::ZERO);
    assert_eq!(
        validate_authoritative(&reversed),
        Err(AuthoritativeStateError::PortalQueueCountersReversed {
            head: U256::ONE,
            tail: U256::ZERO,
        })
    );

    let mut oversized = created();
    oversized.set_portal_withdrawal_queue_for_test(U256::ZERO, U256::from(101));
    assert_eq!(
        validate_authoritative(&oversized),
        Err(AuthoritativeStateError::PortalQueueCapacityExceeded {
            length: U256::from(101),
            capacity: U256::from(100),
        })
    );
}

#[test]
fn rejects_cursor_and_monotonic_counter_incoherence() {
    let mut mismatched_cursor = created();
    set_portal_cursor(&mut mismatched_cursor, B256::repeat_byte(0x81), 1);
    mismatched_cursor.zone.processed_deposit_cursor =
        ZoneProcessedDepositCursor::new(B256::repeat_byte(0x82), 1);
    assert!(matches!(
        validate_authoritative(&mismatched_cursor),
        Err(AuthoritativeStateError::CursorCommitmentMismatch { .. })
    ));

    let mut fallback_counter = created();
    fallback_counter.zone.last_fallback_nonce = 1;
    assert_eq!(
        validate_authoritative(&fallback_counter),
        Err(AuthoritativeStateError::FallbackCounterBeyondWithdrawals {
            fallback_nonce: 1,
            next_withdrawal_index: 0,
        })
    );

    let mut batch_start = created();
    batch_start.zone.next_withdrawal_index = 1;
    batch_start.zone.last_batch = ZoneLastBatch::for_test(B256::ZERO, 1);
    batch_start.zone.batch_start.first_withdrawal_index = 2;
    assert_eq!(
        validate_authoritative(&batch_start),
        Err(AuthoritativeStateError::BatchStartBeyondNextWithdrawal {
            first_withdrawal_index: 2,
            next_withdrawal_index: 1,
        })
    );
}

#[test]
fn rejects_pending_deposit_bounds_zero_refund_and_missing_tokens() {
    let mut zero_refund = created();
    set_portal_cursor(&mut zero_refund, B256::repeat_byte(0x91), 1);
    zero_refund.pending_deposits.insert(
        deposit_id(1),
        DepositOwner::PendingOrdinary {
            preimage: ordinary(token(), Address::ZERO),
        },
    );
    assert_eq!(
        validate_authoritative(&zero_refund),
        Err(AuthoritativeStateError::ZeroTempoRefundRecipient { deposit_number: 1 })
    );

    let mut beyond = created();
    set_portal_cursor(&mut beyond, B256::repeat_byte(0x92), 1);
    beyond.pending_deposits.insert(
        deposit_id(2),
        DepositOwner::PendingOrdinary {
            preimage: ordinary(token(), Address::repeat_byte(0x93)),
        },
    );
    assert_eq!(
        validate_authoritative(&beyond),
        Err(AuthoritativeStateError::DepositBeyondPortalCursor {
            deposit_number: 2,
            portal_deposit_number: 1,
        })
    );

    let missing = Address::repeat_byte(0x94);
    let mut missing_token = created();
    set_portal_cursor(&mut missing_token, B256::repeat_byte(0x95), 1);
    missing_token.pending_deposits.insert(
        deposit_id(1),
        DepositOwner::PendingOrdinary {
            preimage: ordinary(missing, Address::repeat_byte(0x96)),
        },
    );
    assert_eq!(
        validate_authoritative(&missing_token),
        Err(AuthoritativeStateError::MissingOwnerToken {
            owner: OwnerKind::PendingDeposit,
            token: missing,
        })
    );
}

#[test]
fn failed_deposit_withdrawals_require_the_admitted_nonzero_recipient() {
    let mut pending = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(5),
            withdrawal_liability: U256::ZERO,
        },
    );
    set_processed_cursor(&mut pending, B256::repeat_byte(0x97), 1);
    pending.zone.next_withdrawal_index = 1;
    pending.withdrawals.insert(
        withdrawal_id(0),
        WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(
            FailedDepositPendingWithdrawal::from_parts(deposit_id(1), token(), Address::ZERO, 5),
        )),
    );
    assert_eq!(
        validate_authoritative(&pending),
        Err(AuthoritativeStateError::ZeroPendingFailedDepositRecipient {
            withdrawal_index: 0,
        })
    );

    let mut finalized = submitted_batch_state();
    finalized.withdrawals.insert(
        withdrawal_id(1),
        WithdrawalOwner::Finalized(
            FailedDepositPendingWithdrawal::from_parts(deposit_id(2), token(), Address::ZERO, 4)
                .finalize(),
        ),
    );
    assert_eq!(
        validate_authoritative(&finalized),
        Err(
            AuthoritativeStateError::ZeroFinalizedFailedDepositRecipient {
                withdrawal_index: 1,
            }
        )
    );
}

#[test]
fn rejects_broken_fallback_links_and_owner_bounds() {
    let mut missing = valid_user_state();
    missing.fallback_owners.clear();
    assert_eq!(
        validate_authoritative(&missing),
        Err(AuthoritativeStateError::UserFallbackMissing {
            withdrawal_index: 0,
            fallback_nonce: 1,
        })
    );

    let mut mismatched = valid_user_state();
    mismatched.fallback_owners.insert(
        fallback_id(1),
        FallbackOwner::Held {
            withdrawal: withdrawal_id(0),
            token: token(),
            amount: NonZeroU128::new(6).unwrap(),
        },
    );
    assert_eq!(
        validate_authoritative(&mismatched),
        Err(AuthoritativeStateError::UserFallbackMismatch {
            withdrawal_index: 0,
            fallback_nonce: 1,
        })
    );

    let mut broken_bounce = valid_bounce_back_state();
    broken_bounce.fallback_owners.clear();
    assert_eq!(
        validate_authoritative(&broken_bounce),
        Err(AuthoritativeStateError::BounceBackFallbackMissing { deposit_number: 1 })
    );

    let mut withdrawal_bound = valid_user_state();
    withdrawal_bound.zone.next_withdrawal_index = 0;
    assert_eq!(
        validate_authoritative(&withdrawal_bound),
        Err(AuthoritativeStateError::FallbackCounterBeyondWithdrawals {
            fallback_nonce: 1,
            next_withdrawal_index: 0,
        })
    );
}

#[test]
fn sparse_maximum_counters_reject_without_scanning_numeric_ranges() {
    let mut deposits = created();
    set_portal_cursor(&mut deposits, B256::repeat_byte(0xc1), u64::MAX);
    assert_eq!(
        validate_authoritative(&deposits),
        Err(AuthoritativeStateError::PendingDepositMissing { deposit_number: 1 })
    );

    let mut withdrawals = created();
    withdrawals.zone.next_withdrawal_index = u64::MAX;
    assert_eq!(
        validate_authoritative(&withdrawals),
        Err(AuthoritativeStateError::PendingWithdrawalMissing {
            withdrawal_index: 0,
        })
    );

    let mut batches = created();
    batches.zone.last_batch = ZoneLastBatch::for_test(B256::repeat_byte(0xc2), u64::MAX);
    assert_eq!(
        validate_authoritative(&batches),
        Err(AuthoritativeStateError::UnsubmittedBatchMissing { batch_index: 1 })
    );
}

#[test]
fn deposit_suffix_requires_every_owner_and_the_exact_commitment() {
    let mut missing = created();
    for deposit_number in [1, 3] {
        missing.pending_deposits.insert(
            deposit_id(deposit_number),
            DepositOwner::PendingOrdinary {
                preimage: ordinary(token(), Address::repeat_byte(0xc3)),
            },
        );
    }
    set_portal_cursor(&mut missing, B256::repeat_byte(0xc4), 3);
    missing.set_token_accounting_for_test(
        token(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(18),
            withdrawal_liability: U256::ZERO,
        },
    );
    assert_eq!(
        validate_authoritative(&missing),
        Err(AuthoritativeStateError::PendingDepositMissing { deposit_number: 2 })
    );

    let mut commitment = created();
    commitment.pending_deposits.insert(
        deposit_id(1),
        DepositOwner::PendingOrdinary {
            preimage: ordinary(token(), Address::repeat_byte(0xc5)),
        },
    );
    let actual = B256::repeat_byte(0xc6);
    set_portal_cursor(&mut commitment, actual, 1);
    commitment.set_token_accounting_for_test(
        token(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(9),
            withdrawal_liability: U256::ZERO,
        },
    );
    let expected = commitment
        .pending_deposit(deposit_id(1))
        .unwrap()
        .queue_member()
        .hash_after(B256::ZERO);
    assert_eq!(
        validate_authoritative(&commitment),
        Err(AuthoritativeStateError::PortalDepositCommitmentMismatch {
            deposit_number: 1,
            expected,
            actual,
        })
    );
}

#[test]
fn current_withdrawal_suffix_rejects_gaps_and_finalized_rows() {
    let mut missing = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(10),
        },
    );
    missing.zone.next_withdrawal_index = 3;
    missing.zone.last_fallback_nonce = 2;
    for (withdrawal_index, fallback_nonce) in [(0, 1), (2, 2)] {
        let (withdrawal, fallback) = pending_user_at(withdrawal_index, fallback_nonce, 5);
        missing
            .withdrawals
            .insert(withdrawal_id(withdrawal_index), withdrawal);
        missing
            .fallback_owners
            .insert(fallback_id(fallback_nonce), fallback);
    }
    assert_eq!(
        validate_authoritative(&missing),
        Err(AuthoritativeStateError::PendingWithdrawalMissing {
            withdrawal_index: 1,
        })
    );

    let mut finalized = valid_user_state();
    set_processed_cursor(&mut finalized, B256::repeat_byte(0xc7), 1);
    finalized
        .withdrawals
        .insert(withdrawal_id(0), failed_withdrawal(1, 4));
    assert_eq!(
        validate_authoritative(&finalized),
        Err(AuthoritativeStateError::CurrentWithdrawalAlreadyFinalized {
            withdrawal_index: 0,
        })
    );
}
