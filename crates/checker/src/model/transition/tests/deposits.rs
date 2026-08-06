use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256, Bytes, U256};

use super::support::*;
use crate::model::{
    accounting::{Component, TokenAccounting},
    encoding::{DepositQueueMember, sender_tag},
    input::{AuthenticatedDepositOutcome, ImportedTempoBlockInput, ZoneDepositPrefixInput},
    output::ExpectedDepositOutcome,
    ownership::{
        DepositId, FallbackId, InboxRefundId, InboxRefundOwner, PendingWithdrawal,
        WithdrawalIdentity, WithdrawalOwner,
    },
    transition::{DepositKind, DepositOutcomeKind, ModelError, ModelTransition},
};

#[test]
fn prefix_rejects_skip_reorder_replay_unknown_and_count_mismatch_atomically() {
    let token = token(0x31);
    let mut state = created_state(token);
    let members = vec![
        DepositQueueMember::Ordinary(ordinary(token, 0x41, 10)),
        DepositQueueMember::Ordinary(ordinary(token, 0x42, 20)),
    ];
    let imported = ImportedTempoBlockInput::new(0, append_operations(&members));
    commit(&mut state, &imported, &empty_zone()).unwrap();
    let minted = |seed| AuthenticatedDepositOutcome::OrdinaryMinted {
        recipient: Address::repeat_byte(seed),
        memo: B256::ZERO,
    };
    let unknown = DepositQueueMember::Ordinary(ordinary(token, 0x49, 99));
    let invalid_cases = [
        (
            ZoneDepositPrefixInput::new(vec![], vec![members[1].clone()], vec![minted(0x50)]),
            ModelError::DepositPrefixMismatch { number: 1 },
        ),
        (
            ZoneDepositPrefixInput::new(
                vec![],
                vec![members[1].clone(), members[0].clone()],
                vec![minted(0x51), minted(0x52)],
            ),
            ModelError::DepositPrefixMismatch { number: 1 },
        ),
        (
            ZoneDepositPrefixInput::new(vec![], vec![unknown], vec![minted(0x53)]),
            ModelError::DepositPrefixMismatch { number: 1 },
        ),
        (
            ZoneDepositPrefixInput::new(vec![], members.clone(), vec![]),
            ModelError::DepositOutcomeCountMismatch {
                deposits: 2,
                outcomes: 0,
            },
        ),
    ];
    for (input, expected_error) in invalid_cases {
        let before = state.clone();
        assert_eq!(
            ModelTransition::new(&state)
                .apply_imported_tempo_block(&empty_import())
                .unwrap()
                .apply_zone_block(&advance_only_block(&input))
                .err(),
            Some(expected_error)
        );
        assert_eq!(state, before);
        assert_eq!(
            state.token(token).unwrap().accounting(),
            TokenAccounting {
                supply: U256::ZERO,
                deposit_liability: U256::from(30),
                withdrawal_liability: U256::ZERO,
            }
        );
    }

    let first = ZoneDepositPrefixInput::new(vec![], vec![members[0].clone()], vec![minted(0x51)]);
    commit(&mut state, &empty_import(), &first).unwrap();
    let replay = ZoneDepositPrefixInput::new(vec![], vec![members[0].clone()], vec![minted(0x51)]);
    assert_eq!(
        ModelTransition::new(&state)
            .apply_imported_tempo_block(&empty_import())
            .unwrap()
            .apply_zone_block(&advance_only_block(&replay))
            .err(),
        Some(ModelError::DepositPrefixMismatch { number: 2 })
    );

    let second = ZoneDepositPrefixInput::new(vec![], vec![members[1].clone()], vec![minted(0x52)]);
    commit(&mut state, &empty_import(), &second).unwrap();
    assert_eq!(
        ModelTransition::new(&state)
            .apply_imported_tempo_block(&empty_import())
            .unwrap()
            .apply_zone_block(&advance_only_block(&replay))
            .err(),
        Some(ModelError::PendingDepositMissing { number: 3 })
    );
}

#[test]
fn ordinary_and_bounce_back_outcomes_are_not_interchangeable() {
    let token = token(0x32);
    let mut ordinary_state = created_state(token);
    let ordinary = DepositQueueMember::Ordinary(ordinary(token, 0x61, 10));
    let imported =
        ImportedTempoBlockInput::new(0, append_operations(std::slice::from_ref(&ordinary)));
    commit(&mut ordinary_state, &imported, &empty_zone()).unwrap();
    let wrong = ZoneDepositPrefixInput::new(
        vec![],
        vec![ordinary],
        vec![AuthenticatedDepositOutcome::WithdrawalBounceBackMinted {
            recipient: Address::repeat_byte(0x60),
        }],
    );
    assert_eq!(
        ModelTransition::new(&ordinary_state)
            .apply_imported_tempo_block(&empty_import())
            .unwrap()
            .apply_zone_block(&advance_only_block(&wrong))
            .err(),
        Some(ModelError::DepositOutcomeKindMismatch {
            number: 1,
            expected: DepositKind::Ordinary,
            actual: DepositOutcomeKind::WithdrawalBounceBackMinted,
        })
    );

    let mut bounce_state = created_state(token);
    seed_fallback(&mut bounce_state, 4, 9, token, 10);
    bounce_state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(10),
        },
    );
    let bounce = DepositQueueMember::WithdrawalBounceBack(bounce(token, 9, 10));
    let imported =
        ImportedTempoBlockInput::new(0, append_operations(std::slice::from_ref(&bounce)));
    commit(&mut bounce_state, &imported, &empty_zone()).unwrap();
    let wrong = ZoneDepositPrefixInput::new(
        vec![],
        vec![bounce],
        vec![AuthenticatedDepositOutcome::OrdinaryFailed],
    );
    assert_eq!(
        ModelTransition::new(&bounce_state)
            .apply_imported_tempo_block(&empty_import())
            .unwrap()
            .apply_zone_block(&advance_only_block(&wrong))
            .err(),
        Some(ModelError::DepositOutcomeKindMismatch {
            number: 1,
            expected: DepositKind::WithdrawalBounceBack,
            actual: DepositOutcomeKind::OrdinaryFailed,
        })
    );
}

#[test]
fn failed_ordinary_deposit_creates_only_the_zero_rule_withdrawal_owner() {
    let token = token(0x33);
    let mut state = created_state(token);
    let ordinary = ordinary(token, 0x70, 500);
    let member = DepositQueueMember::Ordinary(ordinary.clone());
    let imported =
        ImportedTempoBlockInput::new(0, append_operations(std::slice::from_ref(&member)));
    commit(&mut state, &imported, &empty_zone()).unwrap();
    let zone = ZoneDepositPrefixInput::new(
        vec![],
        vec![member],
        vec![AuthenticatedDepositOutcome::OrdinaryFailed],
    );
    let expected = commit(&mut state, &empty_import(), &zone).unwrap();

    let expected = &expected.zone_deposit_prefix().deposit_outcomes()[0];
    let ExpectedDepositOutcome::OrdinaryFailed(expected) = expected else {
        panic!("wrong expected outcome")
    };
    assert_eq!(expected.first().withdrawal().withdrawal_index, 0);
    assert_eq!(expected.first().sender(), Address::ZERO);
    assert_eq!(expected.first().token(), token);
    assert_eq!(expected.first().to(), ordinary.tempo_refund_recipient());
    assert_eq!(expected.first().amount(), 500);
    assert_eq!(expected.first().fee(), 0);
    assert_eq!(expected.first().memo(), B256::ZERO);
    assert_eq!(expected.first().gas_limit(), 0);
    assert_eq!(expected.first().fallback_nonce(), 0);
    assert_eq!(expected.first().data(), &Bytes::new());
    assert_eq!(expected.first().reveal_to(), &Bytes::new());
    assert_eq!(
        expected.second().deposit_hash(),
        DepositQueueMember::Ordinary(ordinary.clone()).hash_after(B256::ZERO)
    );
    assert_eq!(expected.second().sender(), ordinary.sender());
    assert_eq!(expected.second().token(), token);
    assert_eq!(expected.second().amount(), 500);

    let withdrawal = expected.first().withdrawal();
    let WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(pending)) =
        state.withdrawal(withdrawal).unwrap()
    else {
        panic!("failed deposit must own one special pending withdrawal")
    };
    let finalized = pending.clone().finalize();
    assert_eq!(
        finalized.identity(),
        WithdrawalIdentity::FailedDeposit {
            deposit: DepositId {
                portal: portal(),
                deposit_number: NonZeroU64::new(1).unwrap(),
            }
        }
    );
    assert_eq!(
        finalized.preimage().sender_tag(),
        sender_tag(Address::ZERO, B256::ZERO)
    );
    assert_eq!(finalized.preimage().fallback_nonce(), 0);
    assert_eq!(finalized.preimage().gas_limit(), 0);
    assert_eq!(finalized.preimage().callback_data(), &Bytes::new());
    assert!(state.fallback_owners.is_empty());
    assert_eq!(state.token(token).unwrap().accounting().supply, U256::ZERO);
    assert_eq!(
        state.token(token).unwrap().accounting().deposit_liability,
        U256::from(500)
    );
}

#[test]
fn multiple_failed_deposits_share_literal_nonce_zero_but_not_identity() {
    let token = token(0x34);
    let mut state = created_state(token);
    let members = vec![
        DepositQueueMember::Ordinary(ordinary(token, 0x71, 10)),
        DepositQueueMember::Ordinary(ordinary(token, 0x72, 20)),
    ];
    let imported = ImportedTempoBlockInput::new(0, append_operations(&members));
    commit(&mut state, &imported, &empty_zone()).unwrap();
    let zone = ZoneDepositPrefixInput::new(
        vec![],
        members,
        vec![
            AuthenticatedDepositOutcome::OrdinaryFailed,
            AuthenticatedDepositOutcome::OrdinaryFailed,
        ],
    );
    commit(&mut state, &empty_import(), &zone).unwrap();
    assert_eq!(state.zone().next_withdrawal_index(), 2);
    assert_eq!(state.withdrawals.len(), 2);
    assert!(state.fallback_owners.is_empty());
}

#[test]
fn failed_deposit_withdrawal_index_overflow_is_atomic() {
    let token = token(0x35);
    let mut state = created_state(token);
    let member = DepositQueueMember::Ordinary(ordinary(token, 0x73, 10));
    let imported =
        ImportedTempoBlockInput::new(0, append_operations(std::slice::from_ref(&member)));
    commit(&mut state, &imported, &empty_zone()).unwrap();
    state.set_next_withdrawal_index_for_test(u64::MAX);
    let before = state.clone();
    let zone = ZoneDepositPrefixInput::new(
        vec![],
        vec![member],
        vec![AuthenticatedDepositOutcome::OrdinaryFailed],
    );
    assert_eq!(
        ModelTransition::new(&state)
            .apply_imported_tempo_block(&empty_import())
            .unwrap()
            .apply_zone_block(&advance_only_block(&zone))
            .err(),
        Some(ModelError::WithdrawalIndexOverflow)
    );
    assert_eq!(state, before);
}

#[test]
fn bounce_back_mint_and_pending_paths_consume_fallbacks_and_preserve_origin() {
    let token = token(0x36);
    let mut state = created_state(token);
    let minted_withdrawal = seed_fallback(&mut state, 8, 11, token, 300);
    let pending_withdrawal = seed_fallback(&mut state, 9, 12, token, 400);
    state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(700),
        },
    );
    let members = vec![
        DepositQueueMember::WithdrawalBounceBack(bounce(token, 11, 300)),
        DepositQueueMember::WithdrawalBounceBack(bounce(token, 12, 400)),
    ];
    let imported = ImportedTempoBlockInput::new(0, append_operations(&members));
    commit(&mut state, &imported, &empty_zone()).unwrap();
    let minted_recipient = Address::repeat_byte(0x81);
    let pending_recipient = Address::repeat_byte(0x82);
    let zone = ZoneDepositPrefixInput::new(
        vec![],
        members,
        vec![
            AuthenticatedDepositOutcome::WithdrawalBounceBackMinted {
                recipient: minted_recipient,
            },
            AuthenticatedDepositOutcome::WithdrawalBounceBackPending {
                recipient: pending_recipient,
            },
        ],
    );
    let expected = commit(&mut state, &empty_import(), &zone).unwrap();
    let outputs = expected.zone_deposit_prefix().deposit_outcomes();
    let ExpectedDepositOutcome::WithdrawalBounceBackMinted(minted) = &outputs[0] else {
        panic!("wrong expected minted outcome")
    };
    assert_eq!(minted.token(), token);
    assert_eq!(minted.amount(), 300);
    let ExpectedDepositOutcome::WithdrawalBounceBackPending(pending) = &outputs[1] else {
        panic!("wrong expected pending outcome")
    };
    assert_eq!(pending.token(), token);
    assert_eq!(pending.amount(), 400);

    for nonce in [11, 12] {
        assert!(
            state
                .fallback_owner(FallbackId {
                    zone_id: ZONE_ID,
                    fallback_nonce: NonZeroU64::new(nonce).unwrap(),
                })
                .is_none()
        );
    }
    let refund = InboxRefundId {
        token,
        recipient: pending_recipient,
        user_withdrawal: pending_withdrawal,
    };
    assert_eq!(
        state.inbox_refund(refund),
        Some(&InboxRefundOwner::Pending {
            amount: NonZeroU128::new(400).unwrap(),
        })
    );
    assert!(
        state
            .inbox_refund(InboxRefundId {
                token,
                recipient: minted_recipient,
                user_withdrawal: minted_withdrawal,
            })
            .is_none()
    );
    let accounting = state.token(token).unwrap().accounting();
    assert_eq!(accounting.supply, U256::from(300));
    assert_eq!(accounting.withdrawal_liability, U256::from(400));
}

#[test]
fn bounce_back_pending_rejects_duplicate_credit_and_mint_checks_accounting_bounds() {
    let token = token(0x37);
    let mut pending_state = created_state(token);
    let withdrawal = seed_fallback(&mut pending_state, 10, 13, token, 5);
    pending_state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(5),
        },
    );
    let member = DepositQueueMember::WithdrawalBounceBack(bounce(token, 13, 5));
    let imported =
        ImportedTempoBlockInput::new(0, append_operations(std::slice::from_ref(&member)));
    commit(&mut pending_state, &imported, &empty_zone()).unwrap();
    let recipient = Address::repeat_byte(0x91);
    pending_state.seed_inbox_refund_for_test(
        InboxRefundId {
            token,
            recipient,
            user_withdrawal: withdrawal,
        },
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(1).unwrap(),
        },
    );
    let zone = ZoneDepositPrefixInput::new(
        vec![],
        vec![member],
        vec![AuthenticatedDepositOutcome::WithdrawalBounceBackPending { recipient }],
    );
    assert_eq!(
        ModelTransition::new(&pending_state)
            .apply_imported_tempo_block(&empty_import())
            .unwrap()
            .apply_zone_block(&advance_only_block(&zone))
            .err(),
        Some(ModelError::InboxRefundCollision {
            withdrawal_index: withdrawal.withdrawal_index,
        })
    );

    let mut underflow = created_state(token);
    seed_fallback(&mut underflow, 11, 14, token, 5);
    underflow.set_token_accounting_for_test(token, TokenAccounting::ZERO);
    let member = DepositQueueMember::WithdrawalBounceBack(bounce(token, 14, 5));
    let imported =
        ImportedTempoBlockInput::new(0, append_operations(std::slice::from_ref(&member)));
    commit(&mut underflow, &imported, &empty_zone()).unwrap();
    let zone = ZoneDepositPrefixInput::new(
        vec![],
        vec![member],
        vec![AuthenticatedDepositOutcome::WithdrawalBounceBackMinted {
            recipient: Address::repeat_byte(0x92),
        }],
    );
    assert_eq!(
        ModelTransition::new(&underflow)
            .apply_imported_tempo_block(&empty_import())
            .unwrap()
            .apply_zone_block(&advance_only_block(&zone))
            .err(),
        Some(ModelError::Accounting(
            crate::model::accounting::AccountingError::Underflow(Component::WithdrawalLiability)
        ))
    );
}

#[test]
fn zero_bounce_back_recipient_rejects_both_branches_without_exposing_candidate_changes() {
    for pending in [false, true] {
        let token = token(if pending { 0x39 } else { 0x38 });
        let nonce = if pending { 16 } else { 15 };
        let withdrawal_index = if pending { 13 } else { 12 };
        let amount = 7;
        let mut state = created_state(token);
        let withdrawal = seed_fallback(&mut state, withdrawal_index, nonce, token, amount);
        state.set_token_accounting_for_test(
            token,
            TokenAccounting {
                supply: U256::ZERO,
                deposit_liability: U256::ZERO,
                withdrawal_liability: U256::from(amount),
            },
        );
        let before = state.clone();
        let member = DepositQueueMember::WithdrawalBounceBack(bounce(token, nonce, amount));
        let imported =
            ImportedTempoBlockInput::new(0, append_operations(std::slice::from_ref(&member)));
        let outcome = if pending {
            AuthenticatedDepositOutcome::WithdrawalBounceBackPending {
                recipient: Address::ZERO,
            }
        } else {
            AuthenticatedDepositOutcome::WithdrawalBounceBackMinted {
                recipient: Address::ZERO,
            }
        };
        let zone = ZoneDepositPrefixInput::new(vec![], vec![member], vec![outcome]);

        assert_eq!(
            ModelTransition::new(&state)
                .apply_imported_tempo_block(&imported)
                .unwrap()
                .apply_zone_block(&advance_only_block(&zone))
                .err(),
            Some(ModelError::ZeroBounceBackRecipient {
                withdrawal_index: withdrawal.withdrawal_index,
            })
        );
        assert_eq!(state, before);
    }
}
