use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256, Bytes, U256};

use super::support::*;
use crate::model::{
    accounting::{AccountingError, Component, TokenAccounting},
    encoding::{DepositQueueMember, UserWithdrawalIdentity},
    input::{
        AuthenticatedDepositOutcome, ImportedTempoBlockInput, ZoneBlockContext, ZoneBlockInput,
        ZoneDepositPrefixInput, ZoneOperation,
    },
    output::{
        ExpectedDepositOutcome, ExpectedOutputs, ExpectedWithdrawalRequested, ExpectedZoneOperation,
    },
    ownership::{FallbackId, FallbackOwner, WithdrawalId, WithdrawalIdentity, WithdrawalOwner},
    state::ModelState,
    transition::ModelError,
};

fn withdrawal(token: Address, seed: u8, amount: u128) -> ZoneOperation {
    ZoneOperation::user_withdrawal_accepted(user_withdrawal(token, seed, amount, 0, Bytes::new()))
}

fn expected_user_withdrawals(expected: &ExpectedOutputs) -> Vec<&ExpectedWithdrawalRequested> {
    expected
        .zone_block()
        .operations()
        .iter()
        .map(|operation| match operation {
            ExpectedZoneOperation::WithdrawalRequested(withdrawal) => withdrawal.as_ref(),
            ExpectedZoneOperation::RefundClaimed(_) => {
                panic!("withdrawal-only fixture produced a refund expectation")
            }
        })
        .collect()
}

fn assert_block_rejected_atomically(
    state: &mut ModelState,
    block_number: u64,
    operations: Vec<ZoneOperation>,
    expected_error: ModelError,
) {
    let before = state.clone();
    assert_eq!(
        commit_block(state, block_number, operations, None),
        Err(expected_error)
    );
    assert_eq!(*state, before);
}

#[test]
fn ordered_gas_rate_updates_drive_each_fee_and_persist_the_last_rate() {
    let token = token(0x81);
    let mut state = funded_state(token, U256::from(1_000_000_u64));
    let first = user_withdrawal(token, 0x11, 10, 0, Bytes::new());
    let second = user_withdrawal(token, 0x21, 20, 10, Bytes::from(vec![0x02; 33]));

    let expected = commit_block(
        &mut state,
        1,
        vec![
            ZoneOperation::TempoGasRateUpdated(2),
            ZoneOperation::user_withdrawal_accepted(first),
            ZoneOperation::TempoGasRateUpdated(3),
            ZoneOperation::user_withdrawal_accepted(second),
            ZoneOperation::TempoGasRateUpdated(5),
        ],
        None,
    )
    .unwrap();

    let withdrawals = expected_user_withdrawals(&expected);
    let [first, second] = withdrawals.as_slice() else {
        panic!("two withdrawal expectations required")
    };
    assert_eq!(first.withdrawal().withdrawal_index, 0);
    assert_eq!(first.sender(), Address::repeat_byte(0x11));
    assert_eq!(first.token(), token);
    assert_eq!(first.to(), Address::repeat_byte(0x13));
    assert_eq!(first.amount(), 10);
    assert_eq!(first.fee(), 100_000);
    assert_eq!(first.memo(), B256::repeat_byte(0x14));
    assert_eq!(first.gas_limit(), 0);
    assert_eq!(first.fallback_nonce(), 1);
    assert_eq!(first.data(), &Bytes::from(vec![0x11; 2]));
    assert_eq!(first.reveal_to(), &Bytes::new());

    assert_eq!(second.withdrawal().withdrawal_index, 1);
    assert_eq!(second.fee(), 150_030);
    assert_eq!(second.gas_limit(), 10);
    assert_eq!(second.fallback_nonce(), 2);
    assert_eq!(second.reveal_to(), &Bytes::from(vec![0x02; 33]));
    assert_eq!(state.zone().config().tempo_gas_rate(), 5);
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::from(749_940_u64),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(30),
        }
    );
}

#[test]
fn withdrawal_cap_toggle_matrix_preserves_only_nonzero_mode_counts() {
    struct Case {
        name: &'static str,
        operations: Vec<ZoneOperation>,
        expected_count: usize,
        final_limit: u32,
        rejected_seed: u8,
    }

    let token = token(0x82);
    let cases = [
        Case {
            name: "zero is unlimited and does not count before enabling",
            operations: vec![
                ZoneOperation::MaxWithdrawalsPerBlockUpdated(0),
                withdrawal(token, 0x10, 1),
                withdrawal(token, 0x20, 1),
                ZoneOperation::MaxWithdrawalsPerBlockUpdated(1),
                withdrawal(token, 0x30, 1),
            ],
            expected_count: 3,
            final_limit: 1,
            rejected_seed: 0x40,
        },
        Case {
            name: "raising a nonzero cap does not reset the count",
            operations: vec![
                ZoneOperation::MaxWithdrawalsPerBlockUpdated(1),
                withdrawal(token, 0x11, 1),
                ZoneOperation::MaxWithdrawalsPerBlockUpdated(2),
                withdrawal(token, 0x21, 1),
            ],
            expected_count: 2,
            final_limit: 2,
            rejected_seed: 0x31,
        },
        Case {
            name: "nonzero to zero to nonzero preserves the earlier count",
            operations: vec![
                ZoneOperation::MaxWithdrawalsPerBlockUpdated(2),
                withdrawal(token, 0x12, 1),
                ZoneOperation::MaxWithdrawalsPerBlockUpdated(0),
                withdrawal(token, 0x22, 1),
                withdrawal(token, 0x32, 1),
                ZoneOperation::MaxWithdrawalsPerBlockUpdated(2),
                withdrawal(token, 0x42, 1),
            ],
            expected_count: 4,
            final_limit: 2,
            rejected_seed: 0x52,
        },
    ];

    for case in cases {
        let base = funded_state(token, U256::from(100));
        let mut accepted = base.clone();
        let expected = commit_block(&mut accepted, 1, case.operations.clone(), None)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        assert_eq!(
            expected_user_withdrawals(&expected).len(),
            case.expected_count,
            "{}",
            case.name
        );
        assert_eq!(
            accepted.zone().config().max_withdrawals_per_block(),
            case.final_limit,
            "{}",
            case.name
        );

        let mut rejected = base;
        let mut operations = case.operations;
        operations.push(withdrawal(token, case.rejected_seed, 1));
        assert_block_rejected_atomically(
            &mut rejected,
            1,
            operations,
            ModelError::WithdrawalBlockCapExceeded {
                limit: case.final_limit,
            },
        );
    }
}

#[test]
fn withdrawal_cap_counter_resets_at_the_next_zone_block() {
    let token = token(0x83);
    let mut state = funded_state(token, U256::from(10));
    let first = commit_block(
        &mut state,
        7,
        vec![
            ZoneOperation::MaxWithdrawalsPerBlockUpdated(1),
            withdrawal(token, 0x10, 1),
        ],
        None,
    )
    .unwrap();
    assert_eq!(expected_user_withdrawals(&first).len(), 1);

    let second = commit_block(&mut state, 8, vec![withdrawal(token, 0x20, 1)], None).unwrap();
    assert_eq!(expected_user_withdrawals(&second).len(), 1);
    assert_eq!(
        expected_user_withdrawals(&second)[0]
            .withdrawal()
            .withdrawal_index,
        1
    );
    assert_eq!(state.zone().next_withdrawal_index(), 2);
    assert_eq!(state.zone().last_fallback_nonce(), 2);
}

#[test]
fn sponsored_fee_burn_uses_u256_when_principal_plus_fee_exceeds_u128() {
    let token = token(0x84);
    let principal = u128::MAX;
    let fee = 50_000_u128;
    let total_burn = U256::from(principal) + U256::from(fee);
    let mut state = funded_state(token, total_burn + U256::from(7));

    // The unsponsored precompile path cannot form this u128 total. A sponsored
    // request burns the principal and fee separately, while the checker must
    // still account for their aggregate without narrowing it back to u128.
    let expected = commit_block(
        &mut state,
        1,
        vec![
            ZoneOperation::TempoGasRateUpdated(1),
            withdrawal(token, 0x40, principal),
        ],
        None,
    )
    .unwrap();

    assert_eq!(expected_user_withdrawals(&expected)[0].fee(), fee);
    assert!(total_burn > U256::from(u128::MAX));
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::from(7),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(principal),
        }
    );
}

#[test]
fn user_withdrawals_derive_unique_indices_nonces_and_recipient_free_fallbacks() {
    let token = token(0x85);
    let mut state = funded_state(token, U256::from(20));
    let expected = commit_block(
        &mut state,
        1,
        vec![withdrawal(token, 0x51, 3), withdrawal(token, 0x61, 4)],
        None,
    )
    .unwrap();

    let withdrawals = expected_user_withdrawals(&expected);
    let [first, second] = withdrawals.as_slice() else {
        panic!("two withdrawal expectations required")
    };
    assert_eq!(first.withdrawal().withdrawal_index, 0);
    assert_eq!(second.withdrawal().withdrawal_index, 1);
    assert_eq!(first.fallback_nonce(), 1);
    assert_eq!(second.fallback_nonce(), 2);
    assert_eq!(state.zone().next_withdrawal_index(), 2);
    assert_eq!(state.zone().last_fallback_nonce(), 2);
    assert_eq!(state.withdrawals().len(), 2);

    for (seed, withdrawal_index, fallback_nonce, amount) in [(0x51, 0, 1, 3), (0x61, 1, 2, 4)] {
        let withdrawal = WithdrawalId {
            zone_id: ZONE_ID,
            withdrawal_index,
        };
        let WithdrawalOwner::Pending(pending) = state.withdrawal(withdrawal).unwrap() else {
            panic!("withdrawal {withdrawal_index} must remain pending")
        };
        assert_eq!(
            pending.identity(),
            WithdrawalIdentity::User(
                UserWithdrawalIdentity::new(
                    Address::repeat_byte(seed),
                    B256::repeat_byte(seed + 1),
                    NonZeroU64::new(fallback_nonce).unwrap(),
                )
                .unwrap()
            )
        );

        let fallback = FallbackId {
            zone_id: ZONE_ID,
            fallback_nonce: NonZeroU64::new(fallback_nonce).unwrap(),
        };
        assert_eq!(
            state.fallback_owner(fallback),
            Some(&FallbackOwner::Held {
                withdrawal,
                token,
                amount: NonZeroU128::new(amount).unwrap(),
            })
        );
    }
}

#[test]
fn withdrawal_identity_overflows_collisions_and_accounting_errors_are_atomic() {
    let token = token(0x86);

    let mut nonce_overflow = funded_state(token, U256::from(10));
    nonce_overflow.set_last_fallback_nonce_for_test(u64::MAX);
    assert_block_rejected_atomically(
        &mut nonce_overflow,
        1,
        vec![withdrawal(token, 0x10, 1)],
        ModelError::FallbackNonceOverflow,
    );

    let mut index_overflow = funded_state(token, U256::from(10));
    index_overflow.set_next_withdrawal_index_for_test(u64::MAX);
    assert_block_rejected_atomically(
        &mut index_overflow,
        1,
        vec![withdrawal(token, 0x20, 1)],
        ModelError::WithdrawalIndexOverflow,
    );

    let mut withdrawal_collision = funded_state(token, U256::from(10));
    commit_block(
        &mut withdrawal_collision,
        1,
        vec![withdrawal(token, 0x30, 1)],
        None,
    )
    .unwrap();
    withdrawal_collision.set_next_withdrawal_index_for_test(0);
    assert_block_rejected_atomically(
        &mut withdrawal_collision,
        2,
        vec![withdrawal(token, 0x40, 1)],
        ModelError::WithdrawalOwnerCollision {
            withdrawal_index: 0,
        },
    );

    let mut fallback_collision = funded_state(token, U256::from(10));
    seed_fallback(&mut fallback_collision, 99, 1, token, 1);
    assert_block_rejected_atomically(
        &mut fallback_collision,
        1,
        vec![withdrawal(token, 0x50, 1)],
        ModelError::FallbackOwnerCollision { fallback_nonce: 1 },
    );

    let mut underfunded = funded_state(token, U256::ZERO);
    assert_block_rejected_atomically(
        &mut underfunded,
        1,
        vec![withdrawal(token, 0x60, 1)],
        ModelError::Accounting(AccountingError::Underflow(Component::Supply)),
    );
}

#[test]
fn failed_deposit_uses_the_shared_index_but_not_the_user_cap_or_nonce() {
    let token = token(0x87);
    let ordinary = ordinary(token, 0x71, 9);
    let member = DepositQueueMember::Ordinary(ordinary.clone());
    let mut state = created_state(token);
    let imported = ImportedTempoBlockInput::new(
        1,
        alloy_primitives::U256::ZERO,
        ordinary_append_operations(std::slice::from_ref(&ordinary)),
    );
    commit(&mut state, &imported, &empty_zone()).unwrap();
    state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::from(10),
            deposit_liability: U256::from(9),
            withdrawal_liability: U256::ZERO,
        },
    );
    let before_zone_block = state.clone();

    let advance = ZoneDepositPrefixInput::new(
        vec![],
        vec![member],
        vec![AuthenticatedDepositOutcome::OrdinaryFailed],
    );
    let accepted_input = ZoneBlockInput::new(
        ZoneBlockContext::new(B256::repeat_byte(2), 2),
        advance.clone(),
        vec![
            ZoneOperation::MaxWithdrawalsPerBlockUpdated(1),
            withdrawal(token, 0x72, 3),
        ],
        None,
    );
    let expected = commit_full_block(
        &mut state,
        &ImportedTempoBlockInput::new(2, alloy_primitives::U256::ZERO, Vec::new()),
        &accepted_input,
    )
    .unwrap();

    let [ExpectedDepositOutcome::OrdinaryFailed(failed)] =
        expected.zone_deposit_prefix().deposit_outcomes()
    else {
        panic!("one failed-deposit expectation required")
    };
    let withdrawals = expected_user_withdrawals(&expected);
    let [user] = withdrawals.as_slice() else {
        panic!("one user-withdrawal expectation required")
    };
    assert_eq!(failed.first().withdrawal().withdrawal_index, 0);
    assert_eq!(failed.first().fallback_nonce(), 0);
    assert_eq!(user.withdrawal().withdrawal_index, 1);
    assert_eq!(user.fallback_nonce(), 1);
    assert_eq!(state.zone().next_withdrawal_index(), 2);
    assert_eq!(state.zone().last_fallback_nonce(), 1);
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::from(7),
            deposit_liability: U256::from(9),
            withdrawal_liability: U256::from(3),
        }
    );

    let rejected_input = ZoneBlockInput::new(
        ZoneBlockContext::new(B256::repeat_byte(2), 2),
        advance,
        vec![
            ZoneOperation::MaxWithdrawalsPerBlockUpdated(1),
            withdrawal(token, 0x72, 3),
            withdrawal(token, 0x82, 1),
        ],
        None,
    );
    let mut rejected = before_zone_block.clone();
    assert_eq!(
        commit_full_block(
            &mut rejected,
            &ImportedTempoBlockInput::new(2, alloy_primitives::U256::ZERO, Vec::new()),
            &rejected_input,
        ),
        Err(ModelError::WithdrawalBlockCapExceeded { limit: 1 })
    );
    assert_eq!(rejected, before_zone_block);
}
