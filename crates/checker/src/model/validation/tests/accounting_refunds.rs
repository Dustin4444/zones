use super::*;

#[test]
fn refund_origin_sums_are_derived_and_checked_for_overflow() {
    let recipient = Address::repeat_byte(0xa1);
    let account = RefundAccount {
        token: token(),
        recipient,
    };
    let mut state = created();
    set_portal_cursor(&mut state, B256::repeat_byte(0xa2), 1);
    state.portal_refunds.insert(
        PortalRefundId {
            token: token(),
            recipient,
            failed_deposit: deposit_id(1),
        },
        PortalRefundOwner::Pending { amount: 7 },
    );
    assert_eq!(state.portal_refund_total(account), 7);

    let mut overflow = created();
    let processed_hash = B256::repeat_byte(0xa3);
    set_portal_cursor(&mut overflow, processed_hash, 2);
    overflow.zone.processed_deposit_cursor = ZoneProcessedDepositCursor::new(processed_hash, 2);
    for (origin, amount) in [(1, u128::MAX), (2, 1)] {
        overflow.portal_refunds.insert(
            PortalRefundId {
                token: token(),
                recipient,
                failed_deposit: deposit_id(origin),
            },
            PortalRefundOwner::Pending { amount },
        );
    }
    assert_eq!(
        validate_authoritative(&overflow),
        Err(AuthoritativeStateError::RefundAggregateOverflow {
            ledger: RefundLedger::Portal,
            token: token(),
            recipient,
        })
    );

    let mut inbox = created();
    inbox.zone.next_withdrawal_index = 1;
    inbox.inbox_refunds.insert(
        InboxRefundId {
            token: token(),
            recipient,
            user_withdrawal: withdrawal_id(0),
        },
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(3).unwrap(),
        },
    );
    assert_eq!(inbox.inbox_refund_total(account), 3);
}

#[test]
fn refund_origins_must_precede_their_authoritative_counters() {
    let recipient = Address::repeat_byte(0xb1);
    let mut portal_origin = created();
    let processed_hash = B256::repeat_byte(0xb2);
    set_portal_cursor(&mut portal_origin, processed_hash, 1);
    portal_origin.zone.processed_deposit_cursor =
        ZoneProcessedDepositCursor::new(processed_hash, 1);
    portal_origin.portal_refunds.insert(
        PortalRefundId {
            token: token(),
            recipient,
            failed_deposit: deposit_id(2),
        },
        PortalRefundOwner::Pending { amount: 1 },
    );
    assert_eq!(
        validate_authoritative(&portal_origin),
        Err(
            AuthoritativeStateError::DepositOriginBeyondProcessedCursor {
                deposit_number: 2,
                zone_processed_number: 1,
            }
        )
    );

    let mut inbox_origin = valid_user_state();
    inbox_origin.inbox_refunds.insert(
        InboxRefundId {
            token: token(),
            recipient,
            user_withdrawal: withdrawal_id(1),
        },
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(1).unwrap(),
        },
    );
    assert_eq!(
        validate_authoritative(&inbox_origin),
        Err(AuthoritativeStateError::WithdrawalOriginBeyondNext {
            withdrawal_index: 1,
            next_withdrawal_index: 1,
        })
    );
}

#[test]
fn refund_ledgers_require_the_admitted_nonzero_recipient() {
    let mut portal_credit = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(7),
            withdrawal_liability: U256::ZERO,
        },
    );
    set_processed_cursor(&mut portal_credit, B256::repeat_byte(0xb3), 1);
    portal_credit.portal_refunds.insert(
        PortalRefundId {
            token: token(),
            recipient: Address::ZERO,
            failed_deposit: deposit_id(1),
        },
        PortalRefundOwner::Pending { amount: 7 },
    );
    assert_eq!(
        validate_authoritative(&portal_credit),
        Err(AuthoritativeStateError::ZeroPortalRefundRecipient { deposit_number: 1 })
    );

    let mut inbox_credit = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(3),
        },
    );
    inbox_credit.zone.next_withdrawal_index = 1;
    set_terminal_batch(&mut inbox_credit, 1);
    inbox_credit.inbox_refunds.insert(
        InboxRefundId {
            token: token(),
            recipient: Address::ZERO,
            user_withdrawal: withdrawal_id(0),
        },
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(3).unwrap(),
        },
    );
    assert_eq!(
        validate_authoritative(&inbox_credit),
        Err(AuthoritativeStateError::ZeroInboxRefundRecipient {
            withdrawal_index: 0,
        })
    );
}

#[test]
fn liabilities_are_rebuilt_from_owners_and_pending_tokens_remain_replay_only() {
    let mut withdrawal_mismatch = valid_user_state();
    withdrawal_mismatch.set_token_accounting_for_test(
        token(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(4),
        },
    );
    assert_eq!(
        validate_authoritative(&withdrawal_mismatch),
        Err(AuthoritativeStateError::TokenLiabilityMismatch {
            token: token(),
            kind: LiabilityKind::Withdrawal,
            expected: U256::from(5),
            actual: U256::from(4),
        })
    );

    let mut missing_initial = created();
    missing_initial.tokens.remove(&token());
    assert_eq!(
        validate_authoritative(&missing_initial),
        Err(AuthoritativeStateError::MissingInitialToken { token: token() })
    );

    let pending = Address::repeat_byte(0xe7);
    let mut pending_zone_state = created();
    pending_zone_state.tokens.insert(
        pending,
        crate::model::state::TokenState::for_test(
            crate::model::state::TokenPhase::PendingZoneEnable,
            TokenAccounting {
                supply: U256::ONE,
                deposit_liability: U256::ZERO,
                withdrawal_liability: U256::ZERO,
            },
        ),
    );
    assert_eq!(
        validate_authoritative(&pending_zone_state),
        Err(AuthoritativeStateError::PendingZoneTokenHasZoneState {
            token: pending,
            supply: U256::ONE,
            withdrawal_liability: U256::ZERO,
        })
    );

    let mut collateral_overflow = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::MAX,
            deposit_liability: U256::ONE,
            withdrawal_liability: U256::ZERO,
        },
    );
    assert_eq!(
        validate_authoritative(&collateral_overflow),
        Err(AuthoritativeStateError::TokenCollateralOverflow { token: token() })
    );

    collateral_overflow.set_token_accounting_for_test(token(), TokenAccounting::ZERO);
    let replay_token = Address::repeat_byte(0xe8);
    collateral_overflow.tokens.insert(
        replay_token,
        crate::model::state::TokenState::for_test(
            crate::model::state::TokenPhase::PendingZoneEnable,
            TokenAccounting {
                supply: U256::ZERO,
                deposit_liability: U256::from(9),
                withdrawal_liability: U256::ZERO,
            },
        ),
    );
    collateral_overflow.pending_deposits.insert(
        deposit_id(1),
        DepositOwner::PendingOrdinary {
            preimage: ordinary(replay_token, Address::repeat_byte(0xe9)),
        },
    );
    let cursor = collateral_overflow
        .pending_deposit(deposit_id(1))
        .unwrap()
        .queue_member()
        .hash_after(B256::ZERO);
    set_portal_cursor(&mut collateral_overflow, cursor, 1);
    assert_eq!(validate_authoritative(&collateral_overflow), Ok(()));
}

#[test]
fn each_refund_ledger_is_a_valid_liability_witness_including_zero_credit() {
    let recipient = Address::repeat_byte(0xea);
    let mut portal_credit = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(7),
            withdrawal_liability: U256::ZERO,
        },
    );
    set_processed_cursor(&mut portal_credit, B256::repeat_byte(0xeb), 1);
    portal_credit.portal_refunds.insert(
        PortalRefundId {
            token: token(),
            recipient,
            failed_deposit: deposit_id(1),
        },
        PortalRefundOwner::Pending { amount: 7 },
    );
    assert_eq!(validate_authoritative(&portal_credit), Ok(()));

    let mut zero_credit = created();
    set_processed_cursor(&mut zero_credit, B256::repeat_byte(0xec), 1);
    zero_credit.portal_refunds.insert(
        PortalRefundId {
            token: token(),
            recipient,
            failed_deposit: deposit_id(1),
        },
        PortalRefundOwner::Pending { amount: 0 },
    );
    assert_eq!(validate_authoritative(&zero_credit), Ok(()));

    let mut inbox_credit = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(3),
        },
    );
    inbox_credit.zone.next_withdrawal_index = 1;
    set_terminal_batch(&mut inbox_credit, 1);
    inbox_credit.inbox_refunds.insert(
        InboxRefundId {
            token: token(),
            recipient,
            user_withdrawal: withdrawal_id(0),
        },
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(3).unwrap(),
        },
    );
    assert_eq!(validate_authoritative(&inbox_credit), Ok(()));
}
