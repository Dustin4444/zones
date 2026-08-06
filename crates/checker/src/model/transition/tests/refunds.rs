use std::num::NonZeroU128;

use alloy_primitives::{Address, B256, U256};

use super::{super::ModelError, support::*};
use crate::model::{
    accounting::TokenAccounting,
    encoding::{DepositQueueMember, withdrawal_queue_hash},
    input::{
        AuthenticatedDepositOutcome, AuthenticatedWithdrawalOutcome, ImportedTempoBlockInput,
        ImportedTempoOperation, RefundClaimInput, WithdrawalProcessingInput,
        ZoneDepositPrefixInput, ZoneOperation,
    },
    output::{ExpectedImportedTempoOperation, ExpectedProcessedWithdrawal, ExpectedZoneOperation},
    ownership::{
        InboxRefundId, InboxRefundOwner, PortalRefundId, PortalRefundOwner, RefundAccount,
    },
};

fn portal_claim(
    block_number: u64,
    recipient: Address,
    token: Address,
    amount: u128,
) -> ImportedTempoBlockInput {
    ImportedTempoBlockInput::new(
        block_number,
        U256::ZERO,
        vec![ImportedTempoOperation::PortalRefundClaimed(
            RefundClaimInput::new(recipient, token, amount),
        )],
    )
}

#[test]
fn portal_claim_closes_every_matching_origin_and_only_that_prefix() {
    let token = token(0xa1);
    let recipient = Address::repeat_byte(0xb1);
    let other_recipient = Address::repeat_byte(0xb2);
    let first = portal_refund_id(token, recipient, 1);
    let second = portal_refund_id(token, recipient, 2);
    let unrelated = portal_refund_id(token, other_recipient, 3);
    let mut state = created_state(token);
    seed_portal_credit(&mut state, first, 11);
    seed_portal_credit(&mut state, second, 13);
    seed_portal_credit(&mut state, unrelated, 17);
    state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(41),
            withdrawal_liability: U256::ZERO,
        },
    );

    let expected = commit(
        &mut state,
        &portal_claim(1, recipient, token, 24),
        &empty_zone(),
    )
    .unwrap();

    let [ExpectedImportedTempoOperation::RefundClaimed(claim)] =
        expected.imported_tempo_block().operations()
    else {
        panic!("one Portal claim expectation required")
    };
    assert_eq!(claim.recipient(), recipient);
    assert_eq!(claim.token(), token);
    assert_eq!(claim.amount(), 24);
    assert!(state.portal_refund(first).is_none());
    assert!(state.portal_refund(second).is_none());
    assert_eq!(
        state.portal_refund(unrelated),
        Some(&PortalRefundOwner::Pending { amount: 17 })
    );
    assert_eq!(
        state.portal_refund_total(RefundAccount { token, recipient }),
        0
    );
    assert_eq!(
        state.portal_refund_total(RefundAccount {
            token,
            recipient: other_recipient,
        }),
        17
    );
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(17),
            withdrawal_liability: U256::ZERO,
        }
    );
}

#[test]
fn production_failed_deposits_create_and_claim_one_portal_total() {
    let token = token(0xa9);
    let recipient = Address::repeat_byte(0xb9);
    let fixture = submitted_failed_deposits(
        token,
        vec![
            ordinary_with_refund_recipient(token, 0x91, 11, recipient),
            ordinary_with_refund_recipient(token, 0x92, 13, recipient),
        ],
    );
    let mut state = fixture.state;
    let batch = fixture.batch;
    let withdrawals = fixture.withdrawals;
    let origins = fixture.origins;
    let account = RefundAccount { token, recipient };

    let first_output = commit_imported(
        &mut state,
        52,
        U256::ZERO,
        vec![withdrawals_processed(
            vec![withdrawals[0].clone()],
            withdrawal_queue_hash(&withdrawals[1..]),
            vec![AuthenticatedWithdrawalOutcome::FailedDepositPending],
        )],
    )
    .unwrap();
    let [ExpectedImportedTempoOperation::WithdrawalsProcessed(first_processing)] =
        first_output.imported_tempo_block().operations()
    else {
        panic!("one first pending refund required")
    };
    let [ExpectedProcessedWithdrawal::FailedDepositPending(first)] = first_processing.members()
    else {
        panic!("first failed deposit must become pending")
    };
    assert_eq!(first.failed_deposit(), origins[0]);
    assert_eq!(first.recipient(), recipient);
    assert_eq!(first.token(), token);
    assert_eq!(first.amount(), 11);
    assert_eq!(first.bounceback_fee(), 0);
    assert_eq!(state.portal_refund_total(account), 11);

    let second_output = commit_imported(
        &mut state,
        53,
        U256::ZERO,
        vec![withdrawals_processed(
            vec![withdrawals[1].clone()],
            B256::ZERO,
            vec![AuthenticatedWithdrawalOutcome::FailedDepositPending],
        )],
    )
    .unwrap();
    let [ExpectedImportedTempoOperation::WithdrawalsProcessed(second_processing)] =
        second_output.imported_tempo_block().operations()
    else {
        panic!("one second pending refund required")
    };
    let [ExpectedProcessedWithdrawal::FailedDepositPending(second)] = second_processing.members()
    else {
        panic!("second failed deposit must become pending")
    };
    assert_eq!(second.failed_deposit(), origins[1]);
    assert_eq!(second.recipient(), recipient);
    assert_eq!(second.token(), token);
    assert_eq!(second.amount(), 13);
    assert_eq!(second.bounceback_fee(), 0);
    assert_eq!(state.portal_refund_total(account), 24);

    let claim_output = commit_imported(
        &mut state,
        54,
        U256::ZERO,
        vec![ImportedTempoOperation::PortalRefundClaimed(
            RefundClaimInput::new(recipient, token, 24),
        )],
    )
    .unwrap();
    let [ExpectedImportedTempoOperation::RefundClaimed(claim)] =
        claim_output.imported_tempo_block().operations()
    else {
        panic!("one aggregate claim required")
    };
    assert_eq!(claim.recipient(), recipient);
    assert_eq!(claim.token(), token);
    assert_eq!(claim.amount(), 24);
    assert_eq!(state.portal_refund_total(account), 0);
    assert!(state.portal_refunds().is_empty());
    assert!(state.batch(batch).is_none());
    assert!(state.withdrawals().is_empty());
    assert!(state.pending_deposits().is_empty());
    assert!(state.fallback_owners.is_empty());
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting::ZERO
    );
}

#[test]
fn inbox_claim_closes_every_matching_origin_and_only_that_prefix() {
    let token = token(0xa2);
    let recipient = Address::repeat_byte(0xc1);
    let other_recipient = Address::repeat_byte(0xc2);
    let first = inbox_refund_id(token, recipient, 10);
    let second = inbox_refund_id(token, recipient, 11);
    let unrelated = inbox_refund_id(token, other_recipient, 12);
    let mut state = created_state(token);
    seed_inbox_credit(&mut state, first, 7);
    seed_inbox_credit(&mut state, second, 9);
    seed_inbox_credit(&mut state, unrelated, 19);
    state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::from(5),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(35),
        },
    );

    let expected = commit_block(
        &mut state,
        1,
        vec![ZoneOperation::InboxRefundClaimed(RefundClaimInput::new(
            recipient, token, 16,
        ))],
        None,
    )
    .unwrap();

    let [ExpectedZoneOperation::RefundClaimed(claim)] = expected.zone_block().operations() else {
        panic!("one Inbox claim expectation required")
    };
    assert_eq!(claim.recipient(), recipient);
    assert_eq!(claim.token(), token);
    assert_eq!(claim.amount(), 16);
    assert!(state.inbox_refund(first).is_none());
    assert!(state.inbox_refund(second).is_none());
    assert_eq!(
        state.inbox_refund(unrelated),
        Some(&InboxRefundOwner::Pending {
            amount: NonZeroU128::new(19).unwrap(),
        })
    );
    assert_eq!(
        state.inbox_refund_total(RefundAccount { token, recipient }),
        0
    );
    assert_eq!(
        state.inbox_refund_total(RefundAccount {
            token,
            recipient: other_recipient,
        }),
        19
    );
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::from(21),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(19),
        }
    );
}

#[test]
fn production_bouncebacks_create_and_claim_one_mixed_origin_inbox_total() {
    let token = token(0xa8);
    let recipient = Address::repeat_byte(0xc8);
    let account = RefundAccount { token, recipient };
    let mut state = funded_state(token, U256::from(30));
    let (batch, withdrawals) =
        prepare_submitted_users(&mut state, 1, &[(0x81, 12, 0), (0x82, 18, 0)]);
    let members = withdrawals
        .iter()
        .map(|(_, preimage)| {
            DepositQueueMember::WithdrawalBounceBack(bounce(
                preimage.token(),
                preimage.fallback_nonce(),
                preimage.amount(),
            ))
        })
        .collect::<Vec<_>>();
    let imported = ImportedTempoBlockInput::new(
        10_002,
        U256::ZERO,
        vec![withdrawals_processed(
            withdrawals
                .iter()
                .map(|(_, preimage)| preimage.clone())
                .collect(),
            B256::ZERO,
            vec![
                AuthenticatedWithdrawalOutcome::UserBounced,
                AuthenticatedWithdrawalOutcome::UserBounced,
            ],
        )],
    );
    let prefix = ZoneDepositPrefixInput::new(
        Vec::new(),
        members,
        vec![
            AuthenticatedDepositOutcome::WithdrawalBounceBackPending { recipient },
            AuthenticatedDepositOutcome::WithdrawalBounceBackPending { recipient },
        ],
    );

    commit(&mut state, &imported, &prefix).unwrap();

    let first_credit = InboxRefundId {
        token,
        recipient,
        user_withdrawal: withdrawals[0].0,
    };
    let second_credit = InboxRefundId {
        token,
        recipient,
        user_withdrawal: withdrawals[1].0,
    };
    assert_eq!(
        state.inbox_refund(first_credit),
        Some(&InboxRefundOwner::Pending {
            amount: NonZeroU128::new(12).unwrap(),
        })
    );
    assert_eq!(
        state.inbox_refund(second_credit),
        Some(&InboxRefundOwner::Pending {
            amount: NonZeroU128::new(18).unwrap(),
        })
    );
    assert_eq!(state.inbox_refund_total(account), 30);

    let output = commit_block(
        &mut state,
        2,
        vec![ZoneOperation::InboxRefundClaimed(RefundClaimInput::new(
            recipient, token, 30,
        ))],
        None,
    )
    .unwrap();

    let [ExpectedZoneOperation::RefundClaimed(claim)] = output.zone_block().operations() else {
        panic!("one aggregate Inbox claim expectation required")
    };
    assert_eq!(claim.recipient(), recipient);
    assert_eq!(claim.token(), token);
    assert_eq!(claim.amount(), 30);
    assert_eq!(state.inbox_refund_total(account), 0);
    assert!(state.inbox_refund(first_credit).is_none());
    assert!(state.inbox_refund(second_credit).is_none());
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::from(30),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        }
    );
    assert!(state.batch(batch).is_none());
    assert!(state.withdrawals().is_empty());
    assert!(state.fallback_owners.is_empty());
    assert!(state.pending_deposits().is_empty());
    assert!(state.inbox_refunds.is_empty());
}

#[test]
fn claims_are_isolated_between_portal_and_inbox_maps() {
    let token = token(0xa3);
    let recipient = Address::repeat_byte(0xd1);
    let portal_credit = portal_refund_id(token, recipient, 1);
    let inbox_credit = inbox_refund_id(token, recipient, 1);
    let mut state = created_state(token);
    seed_portal_credit(&mut state, portal_credit, 10);
    seed_inbox_credit(&mut state, inbox_credit, 20);
    state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(10),
            withdrawal_liability: U256::from(20),
        },
    );

    commit(
        &mut state,
        &portal_claim(1, recipient, token, 10),
        &empty_zone(),
    )
    .unwrap();
    assert!(state.portal_refund(portal_credit).is_none());
    assert!(state.inbox_refund(inbox_credit).is_some());
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(20),
        }
    );

    commit_block(
        &mut state,
        2,
        vec![ZoneOperation::InboxRefundClaimed(RefundClaimInput::new(
            recipient, token, 20,
        ))],
        None,
    )
    .unwrap();
    assert!(state.inbox_refund(inbox_credit).is_none());
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::from(20),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        }
    );
}

#[test]
fn zero_claims_without_credits_are_exact_noops_on_both_chains() {
    let initial_token = token(0xa4);
    let untracked_token = token(0xa5);
    let recipient = Address::repeat_byte(0xe1);
    let state = created_state(initial_token);
    let imported = portal_claim(1, recipient, untracked_token, 0);
    let zone = zone_block(
        1,
        vec![ZoneOperation::InboxRefundClaimed(RefundClaimInput::new(
            recipient,
            untracked_token,
            0,
        ))],
        None,
    );

    let (next, expected) = apply_full_block(&state, &imported, &zone).unwrap();

    assert_eq!(next, state);
    let [ExpectedImportedTempoOperation::RefundClaimed(portal_claim)] =
        expected.imported_tempo_block().operations()
    else {
        panic!("one Portal claim expectation required")
    };
    let [ExpectedZoneOperation::RefundClaimed(inbox_claim)] = expected.zone_block().operations()
    else {
        panic!("one Inbox claim expectation required")
    };
    assert_eq!(portal_claim.recipient(), recipient);
    assert_eq!(portal_claim.token(), untracked_token);
    assert_eq!(portal_claim.amount(), 0);
    assert_eq!(inbox_claim.recipient(), recipient);
    assert_eq!(inbox_claim.token(), untracked_token);
    assert_eq!(inbox_claim.amount(), 0);
}

#[test]
fn zero_portal_claim_closes_a_zero_valued_origin_without_an_aggregate_row() {
    let token = token(0xaa);
    let recipient = Address::repeat_byte(0xea);
    let credit = portal_refund_id(token, recipient, 1);
    let mut state = created_state(token);
    seed_portal_credit(&mut state, credit, 0);
    let account = RefundAccount { token, recipient };
    assert_eq!(state.portal_refund_total(account), 0);

    let output = commit(
        &mut state,
        &portal_claim(1, recipient, token, 0),
        &empty_zone(),
    )
    .unwrap();

    let [ExpectedImportedTempoOperation::RefundClaimed(claim)] =
        output.imported_tempo_block().operations()
    else {
        panic!("one zero Portal claim expected")
    };
    assert_eq!(claim.amount(), 0);
    assert!(state.portal_refund(credit).is_none());
    assert_eq!(state.portal_refund_total(account), 0);
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting::ZERO
    );
}

#[test]
fn nonzero_claims_without_credits_reject_atomically() {
    let token = token(0xa4);
    let recipient = Address::repeat_byte(0xe2);
    let base = created_state(token);

    let mut state = base.clone();
    assert_eq!(
        commit(
            &mut state,
            &portal_claim(1, recipient, token, 1),
            &empty_zone(),
        ),
        Err(ModelError::RefundClaimAmountMismatch {
            token,
            recipient,
            expected: 0,
            actual: 1,
        })
    );
    assert_eq!(state, base);

    let mut state = base.clone();
    assert_eq!(
        commit_block(
            &mut state,
            1,
            vec![ZoneOperation::InboxRefundClaimed(RefundClaimInput::new(
                recipient, token, 1,
            ))],
            None,
        ),
        Err(ModelError::RefundClaimAmountMismatch {
            token,
            recipient,
            expected: 0,
            actual: 1,
        })
    );
    assert_eq!(state, base);
}

#[test]
fn wrong_under_and_over_claim_amounts_reject_atomically() {
    let token = token(0xa5);
    let recipient = Address::repeat_byte(0xf1);
    let portal_credit = portal_refund_id(token, recipient, 1);
    let inbox_credit = inbox_refund_id(token, recipient, 1);
    let mut base = created_state(token);
    seed_portal_credit(&mut base, portal_credit, 10);
    seed_inbox_credit(&mut base, inbox_credit, 10);
    base.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(10),
            withdrawal_liability: U256::from(10),
        },
    );

    for actual in [9, 11] {
        let mut state = base.clone();
        let before = state.clone();
        let imported = ImportedTempoBlockInput::new(
            1,
            U256::ZERO,
            vec![
                ImportedTempoOperation::BouncebackGasUpdated(77),
                ImportedTempoOperation::PortalRefundClaimed(RefundClaimInput::new(
                    recipient, token, actual,
                )),
            ],
        );
        assert_eq!(
            commit(&mut state, &imported, &empty_zone()),
            Err(ModelError::RefundClaimAmountMismatch {
                token,
                recipient,
                expected: 10,
                actual,
            })
        );
        assert_eq!(
            state, before,
            "rejected Portal claim leaked a prefix mutation"
        );

        let mut state = base.clone();
        let before = state.clone();
        assert_eq!(
            commit_block(
                &mut state,
                1,
                vec![
                    ZoneOperation::TempoGasRateUpdated(88),
                    ZoneOperation::InboxRefundClaimed(RefundClaimInput::new(
                        recipient, token, actual,
                    )),
                ],
                None,
            ),
            Err(ModelError::RefundClaimAmountMismatch {
                token,
                recipient,
                expected: 10,
                actual,
            })
        );
        assert_eq!(
            state, before,
            "rejected Inbox claim leaked a prefix mutation"
        );
    }
}

#[test]
fn processing_integrates_fee_rounding_cap_config_order_and_same_block_claim() {
    let token = token(0xa7);
    let amounts = [10, 10, 3];
    let fixture = submitted_failed_deposit_batch(token, &amounts);
    let mut state = fixture.state;
    let batch = fixture.batch;
    let withdrawals = fixture.withdrawals;
    let origins = fixture.origins;
    let base_fee = U256::from(500_000_000_001_u64);
    let first_recipient = withdrawals[0].to();
    let second_recipient = withdrawals[1].to();
    let operations = vec![
        ImportedTempoOperation::BouncebackGasUpdated(1),
        ImportedTempoOperation::WithdrawalsProcessed(Box::new(WithdrawalProcessingInput::new(
            vec![withdrawals[0].clone()],
            withdrawal_queue_hash(&withdrawals[1..]),
            vec![AuthenticatedWithdrawalOutcome::FailedDepositPending],
        ))),
        ImportedTempoOperation::PortalRefundClaimed(RefundClaimInput::new(
            first_recipient,
            token,
            9,
        )),
        ImportedTempoOperation::BouncebackGasUpdated(3),
        ImportedTempoOperation::WithdrawalsProcessed(Box::new(WithdrawalProcessingInput::new(
            vec![withdrawals[1].clone()],
            withdrawal_queue_hash(&withdrawals[2..]),
            vec![AuthenticatedWithdrawalOutcome::FailedDepositPending],
        ))),
        ImportedTempoOperation::BouncebackGasUpdated(1_000),
        ImportedTempoOperation::WithdrawalsProcessed(Box::new(WithdrawalProcessingInput::new(
            vec![withdrawals[2].clone()],
            B256::ZERO,
            vec![AuthenticatedWithdrawalOutcome::FailedDepositPaid],
        ))),
        ImportedTempoOperation::BouncebackGasUpdated(7),
    ];
    let imported = ImportedTempoBlockInput::new(4, base_fee, operations);

    let expected = commit(&mut state, &imported, &empty_zone()).unwrap();

    let [
        ExpectedImportedTempoOperation::WithdrawalsProcessed(first_processing),
        ExpectedImportedTempoOperation::RefundClaimed(claim),
        ExpectedImportedTempoOperation::WithdrawalsProcessed(second_processing),
        ExpectedImportedTempoOperation::WithdrawalsProcessed(third_processing),
    ] = expected.imported_tempo_block().operations()
    else {
        panic!("process, claim, process, process expectations must preserve operation order")
    };
    let ExpectedProcessedWithdrawal::FailedDepositPending(first) = &first_processing.members()[0]
    else {
        panic!("first failed deposit must create a pending refund")
    };
    assert_eq!(first.failed_deposit(), origins[0]);
    assert_eq!(first.amount(), 9);
    assert_eq!(first.bounceback_fee(), 1);
    let ExpectedProcessedWithdrawal::FailedDepositPending(second) = &second_processing.members()[0]
    else {
        panic!("second failed deposit must create a pending refund")
    };
    assert_eq!(second.failed_deposit(), origins[1]);
    assert_eq!(second.amount(), 8);
    assert_eq!(second.bounceback_fee(), 2);
    let ExpectedProcessedWithdrawal::FailedDepositPaid(third) = &third_processing.members()[0]
    else {
        panic!("third failed deposit must be paid directly")
    };
    assert_eq!(third.failed_deposit(), origins[2]);
    assert_eq!(third.amount(), 0);
    assert_eq!(third.bounceback_fee(), 3);

    assert_eq!(claim.recipient(), first_recipient);
    assert_eq!(claim.amount(), 9);
    assert!(
        state
            .portal_refund(PortalRefundId {
                token,
                recipient: first_recipient,
                failed_deposit: origins[0],
            })
            .is_none()
    );
    assert_eq!(
        state.portal_refund(PortalRefundId {
            token,
            recipient: second_recipient,
            failed_deposit: origins[1],
        }),
        Some(&PortalRefundOwner::Pending { amount: 8 })
    );
    assert!(state.batch(batch).is_none());
    let portal = state.portal().created().unwrap();
    assert_eq!(portal.config().bounceback_gas(), 7);
    assert_eq!(portal.settlement().withdrawal_queue_head(), U256::ONE);
    assert_eq!(portal.settlement().withdrawal_queue_tail(), U256::ONE);
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(8),
            withdrawal_liability: U256::ZERO,
        }
    );
}
