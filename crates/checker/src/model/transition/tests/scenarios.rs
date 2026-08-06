use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256, U256};

use super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    encoding::DepositQueueMember,
    input::{
        AuthenticatedDepositOutcome, AuthenticatedWithdrawalOutcome, ImportedTempoBlockInput,
        ZoneDepositPrefixInput,
    },
    output::{ExpectedDepositOutcome, ExpectedImportedTempoOperation, ExpectedProcessedWithdrawal},
    ownership::{FallbackId, FallbackOwner, InboxRefundId, InboxRefundOwner},
};

fn minted_outcomes(count: usize, seed: u8) -> Vec<AuthenticatedDepositOutcome> {
    (0..count)
        .map(|index| AuthenticatedDepositOutcome::OrdinaryMinted {
            recipient: Address::repeat_byte(seed.wrapping_add(index as u8)),
            memo: B256::repeat_byte(index as u8),
        })
        .collect()
}

#[test]
fn multiple_tempo_appends_and_zone_prefixes_reproduce_every_cursor() {
    let token = token(0x41);
    let mut state = created_state(token);
    let first_deposits = vec![ordinary(token, 0x51, 10), ordinary(token, 0x52, 20)];
    let second_deposits = vec![ordinary(token, 0x53, 30), ordinary(token, 0x54, 40)];
    let first_block = ordinary_members(&first_deposits);
    let second_block = ordinary_members(&second_deposits);

    let imported = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        ordinary_append_operations(&first_deposits),
    );
    let expected = commit(&mut state, &imported, &empty_zone()).unwrap();
    let [
        ExpectedImportedTempoOperation::DepositAppended(first_expected),
        ExpectedImportedTempoOperation::DepositAppended(second_expected),
    ] = expected.imported_tempo_block().operations()
    else {
        panic!("two ordered append expectations required")
    };
    let first_hash = first_block[0].hash_after(B256::ZERO);
    let second_hash = first_block[1].hash_after(first_hash);
    assert_eq!(first_expected.id().deposit_number.get(), 1);
    assert_eq!(first_expected.queue_hash(), first_hash);
    assert_eq!(second_expected.id().deposit_number.get(), 2);
    assert_eq!(second_expected.queue_hash(), second_hash);
    assert_eq!(
        state.portal().created().unwrap().deposit_cursor().number(),
        2
    );
    assert_eq!(state.zone().processed_deposit_cursor().number(), 0);

    let imported = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        ordinary_append_operations(&second_deposits),
    );
    let expected = commit(&mut state, &imported, &empty_zone()).unwrap();
    let [
        ExpectedImportedTempoOperation::DepositAppended(third_expected),
        ExpectedImportedTempoOperation::DepositAppended(fourth_expected),
    ] = expected.imported_tempo_block().operations()
    else {
        panic!("two ordered append expectations required")
    };
    let third_hash = second_block[0].hash_after(second_hash);
    let fourth_hash = second_block[1].hash_after(third_hash);
    assert_eq!(third_expected.id().deposit_number.get(), 3);
    assert_eq!(third_expected.queue_hash(), third_hash);
    assert_eq!(fourth_expected.id().deposit_number.get(), 4);
    assert_eq!(fourth_expected.queue_hash(), fourth_hash);
    assert_eq!(
        state.portal().created().unwrap().deposit_cursor().number(),
        4
    );

    let all = first_block
        .iter()
        .chain(&second_block)
        .cloned()
        .collect::<Vec<_>>();
    let first_prefix =
        ZoneDepositPrefixInput::new(vec![], all[..3].to_vec(), minted_outcomes(3, 0x60));
    let expected = commit(&mut state, &empty_import(), &first_prefix).unwrap();
    let first_cursor = expected.zone_deposit_prefix().processed_cursor();
    assert_eq!(first_cursor.number(), 3);
    assert_eq!(
        first_cursor.hash(),
        all[2].hash_after(all[1].hash_after(all[0].hash_after(B256::ZERO)))
    );
    let ExpectedDepositOutcome::OrdinaryMinted(first_mint) =
        &expected.zone_deposit_prefix().deposit_outcomes()[0]
    else {
        panic!("first ordinary deposit must produce a mint expectation")
    };
    let DepositQueueMember::Ordinary(first_preimage) = &all[0] else {
        unreachable!()
    };
    assert_eq!(first_mint.deposit_hash(), first_hash);
    assert_eq!(first_mint.sender(), first_preimage.sender());
    assert_eq!(first_mint.token(), token);
    assert_eq!(first_mint.amount(), 10);
    assert_eq!(state.pending_deposits().len(), 1);

    let final_prefix =
        ZoneDepositPrefixInput::new(vec![], all[3..].to_vec(), minted_outcomes(1, 0x70));
    commit(&mut state, &empty_import(), &final_prefix).unwrap();
    assert!(state.pending_deposits().is_empty());
    assert_eq!(
        state.zone().processed_deposit_cursor().hash(),
        state.portal().created().unwrap().deposit_cursor().hash()
    );
    let accounting = state.token(token).unwrap().accounting();
    assert_eq!(accounting.supply, U256::from(100));
    assert_eq!(accounting.deposit_liability, U256::ZERO);
}

#[test]
fn empty_partial_and_full_catch_up_are_one_algorithm_and_split_equivalent() {
    let token = token(0x42);
    let mut base = created_state(token);
    let deposits = vec![
        ordinary(token, 0x61, 10),
        ordinary(token, 0x62, 20),
        ordinary(token, 0x63, 30),
    ];
    let members = ordinary_members(&deposits);
    let imported = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        ordinary_append_operations(&deposits),
    );
    commit(&mut base, &imported, &empty_zone()).unwrap();

    let mut split = base.clone();
    let before_empty = split.clone();
    let expected = commit(&mut split, &empty_import(), &empty_zone()).unwrap();
    assert_eq!(expected.zone_deposit_prefix().deposits_processed(), 0);
    assert_eq!(split, before_empty);

    let first =
        ZoneDepositPrefixInput::new(vec![], members[..2].to_vec(), minted_outcomes(2, 0x71));
    commit(&mut split, &empty_import(), &first).unwrap();
    assert_eq!(split.pending_deposits().len(), 1);
    let final_prefix =
        ZoneDepositPrefixInput::new(vec![], members[2..].to_vec(), minted_outcomes(1, 0x73));
    commit(&mut split, &empty_import(), &final_prefix).unwrap();

    let mut one_shot = base;
    let full = ZoneDepositPrefixInput::new(vec![], members, minted_outcomes(3, 0x71));
    commit(&mut one_shot, &empty_import(), &full).unwrap();
    assert_eq!(split, one_shot);
    assert!(split.pending_deposits().is_empty());
}

#[test]
fn identical_consecutive_preimages_remain_distinct_numbered_queue_members() {
    let token = token(0x43);
    let mut state = created_state(token);
    let deposit = ordinary(token, 0x70, 5);
    let deposits = vec![deposit.clone(), deposit];
    let members = ordinary_members(&deposits);
    let imported = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        ordinary_append_operations(&deposits),
    );
    commit(&mut state, &imported, &empty_zone()).unwrap();
    assert_eq!(state.pending_deposits().len(), 2);
    let zone = ZoneDepositPrefixInput::new(vec![], members, minted_outcomes(2, 0x80));
    commit(&mut state, &empty_import(), &zone).unwrap();
    assert_eq!(state.zone().processed_deposit_cursor().number(), 2);
    assert!(state.pending_deposits().is_empty());
}

#[test]
fn same_candidate_ordinary_append_and_mint_reads_through_then_closes_the_owner() {
    let token = token(0x44);
    let mut state = created_state(token);
    let deposit = ordinary(token, 0x71, 55);
    let member = DepositQueueMember::Ordinary(deposit.clone());
    let imported = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        ordinary_append_operations(std::slice::from_ref(&deposit)),
    );
    let zone = ZoneDepositPrefixInput::new(
        vec![],
        vec![member.clone()],
        vec![AuthenticatedDepositOutcome::OrdinaryMinted {
            recipient: Address::repeat_byte(0x91),
            memo: B256::repeat_byte(0x92),
        }],
    );

    let expected = commit(&mut state, &imported, &zone).unwrap();
    let queue_hash = member.hash_after(B256::ZERO);
    let [ExpectedImportedTempoOperation::DepositAppended(append)] =
        expected.imported_tempo_block().operations()
    else {
        panic!("one append expectation required")
    };
    assert_eq!(append.queue_hash(), queue_hash);
    let [ExpectedDepositOutcome::OrdinaryMinted(mint)] =
        expected.zone_deposit_prefix().deposit_outcomes()
    else {
        panic!("one ordinary-mint expectation required")
    };
    let DepositQueueMember::Ordinary(preimage) = member else {
        unreachable!()
    };
    assert_eq!(mint.deposit_hash(), queue_hash);
    assert_eq!(mint.sender(), preimage.sender());
    assert_eq!(mint.token(), token);
    assert_eq!(mint.amount(), 55);
    assert!(state.pending_deposits().is_empty());
    assert_eq!(
        state.zone().processed_deposit_cursor(),
        crate::model::state::ZoneProcessedDepositCursor::new(queue_hash, 1)
    );
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::from(55),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        }
    );
}

#[test]
fn same_candidate_bounce_append_outcomes_replace_each_owner_exactly_once() {
    for pending in [false, true] {
        let token = token(if pending { 0x46 } else { 0x45 });
        let amount = 65;
        let recipient = Address::repeat_byte(if pending { 0xa2 } else { 0xa1 });
        let mut state = funded_state(token, U256::from(amount));
        let (_, withdrawals) = prepare_submitted_users(&mut state, 1, &[(0x81, amount, 0)]);
        let (withdrawal, preimage) = &withdrawals[0];
        let fallback_id = FallbackId {
            zone_id: ZONE_ID,
            fallback_nonce: NonZeroU64::new(preimage.fallback_nonce()).unwrap(),
        };
        assert!(matches!(
            state.fallback_owner(fallback_id),
            Some(FallbackOwner::Held { .. })
        ));

        let member = DepositQueueMember::WithdrawalBounceBack(bounce(
            preimage.token(),
            preimage.fallback_nonce(),
            preimage.amount(),
        ));
        let imported = ImportedTempoBlockInput::new(
            2,
            alloy_primitives::U256::ZERO,
            vec![withdrawals_processed(
                vec![preimage.clone()],
                B256::ZERO,
                vec![AuthenticatedWithdrawalOutcome::UserBounced],
            )],
        );
        let outcome = if pending {
            AuthenticatedDepositOutcome::WithdrawalBounceBackPending { recipient }
        } else {
            AuthenticatedDepositOutcome::WithdrawalBounceBackMinted { recipient }
        };
        let zone = ZoneDepositPrefixInput::new(vec![], vec![member], vec![outcome]);
        let expected = commit(&mut state, &imported, &zone).unwrap();

        assert!(state.pending_deposits().is_empty());
        assert!(state.fallback_owner(fallback_id).is_none());
        let [ExpectedImportedTempoOperation::WithdrawalsProcessed(processed)] =
            expected.imported_tempo_block().operations()
        else {
            panic!("one production bounce processing expectation required")
        };
        let [ExpectedProcessedWithdrawal::UserBounced(bounced)] = processed.members() else {
            panic!("one bounced user expectation required")
        };
        assert_eq!(
            bounced.first().deposit().fallback_nonce().get(),
            preimage.fallback_nonce()
        );
        let accounting = state.token(token).unwrap().accounting();
        assert_eq!(accounting.deposit_liability, U256::ZERO);
        let [expected_outcome] = expected.zone_deposit_prefix().deposit_outcomes() else {
            panic!("one bounce-back expectation required")
        };
        if pending {
            let ExpectedDepositOutcome::WithdrawalBounceBackPending(output) = expected_outcome
            else {
                panic!("pending branch expectation required")
            };
            assert_eq!(output.token(), token);
            assert_eq!(output.amount(), amount);
            assert_eq!(accounting.supply, U256::ZERO);
            assert_eq!(accounting.withdrawal_liability, U256::from(amount));
            assert_eq!(
                state.inbox_refund(InboxRefundId {
                    token,
                    recipient,
                    user_withdrawal: *withdrawal,
                }),
                Some(&InboxRefundOwner::Pending {
                    amount: NonZeroU128::new(amount).unwrap(),
                })
            );
        } else {
            let ExpectedDepositOutcome::WithdrawalBounceBackMinted(output) = expected_outcome
            else {
                panic!("minted branch expectation required")
            };
            assert_eq!(output.token(), token);
            assert_eq!(output.amount(), amount);
            assert_eq!(accounting.supply, U256::from(amount));
            assert_eq!(accounting.withdrawal_liability, U256::ZERO);
            assert!(state.inbox_refunds.is_empty());
        }
    }
}
