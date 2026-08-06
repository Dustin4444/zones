use std::num::NonZeroU64;

use alloy_primitives::{Address, B256, Bytes, U256, b256};

use super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    constants::AUTHENTICATED_WITHDRAWAL_SIZE,
    encoding::{DepositQueueMember, WithdrawalDataError},
    input::{
        AuthenticatedDepositOutcome, BatchFinalizationInput, ImportedTempoBlockInput,
        ZoneBlockContext, ZoneBlockInput, ZoneDepositPrefixInput, ZoneOperation,
    },
    ownership::{BatchId, PendingWithdrawal, WithdrawalId, WithdrawalOwner},
    state::ZoneLastBatch,
    transition::ModelError,
};

fn reveal_key(seed: u8) -> Bytes {
    let mut bytes = vec![seed; 33];
    bytes[0] = 0x02;
    Bytes::from(bytes)
}

fn encrypted_sender(seed: u8) -> Bytes {
    Bytes::from(vec![seed; AUTHENTICATED_WITHDRAWAL_SIZE])
}

fn finalization(
    declared_count: usize,
    block_number: u64,
    encrypted_senders: Vec<Bytes>,
) -> BatchFinalizationInput {
    BatchFinalizationInput::new(declared_count, block_number, encrypted_senders)
}

fn block(
    block_hash: B256,
    block_number: u64,
    advance: ZoneDepositPrefixInput,
    operations: Vec<ZoneOperation>,
    finalization: Option<BatchFinalizationInput>,
) -> ZoneBlockInput {
    ZoneBlockInput::new(
        ZoneBlockContext::new(block_hash, block_number),
        advance,
        operations,
        finalization,
    )
}

fn state_with_pending_users(reveals: Vec<Bytes>) -> crate::model::state::ModelState {
    let token = token(0x91);
    let mut state = created_state(token);
    state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::from(10_000),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        },
    );
    let operations = reveals
        .into_iter()
        .enumerate()
        .map(|(index, reveal)| {
            ZoneOperation::user_withdrawal_accepted(user_withdrawal(
                token,
                0x20 + u8::try_from(index).unwrap(),
                100 + u128::try_from(index).unwrap(),
                0,
                reveal,
            ))
        })
        .collect();
    commit_full_block(
        &mut state,
        &ImportedTempoBlockInput::new(10, alloy_primitives::U256::ZERO, Vec::new()),
        &block(
            B256::repeat_byte(0xa0),
            20,
            ZoneDepositPrefixInput::default(),
            operations,
            None,
        ),
    )
    .unwrap();
    state
}

#[test]
fn mixed_user_failed_and_revealed_withdrawals_finalize_as_one_fixed_batch() {
    let token = token(0x92);
    let mut state = created_state(token);
    state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::from(1_000),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        },
    );

    commit_full_block(
        &mut state,
        &ImportedTempoBlockInput::new(40, alloy_primitives::U256::ZERO, Vec::new()),
        &block(
            B256::repeat_byte(0xa1),
            41,
            ZoneDepositPrefixInput::default(),
            vec![ZoneOperation::user_withdrawal_accepted(user_withdrawal(
                token,
                0x31,
                100,
                0,
                Bytes::new(),
            ))],
            None,
        ),
    )
    .unwrap();

    let failed_deposit = ordinary(token, 0x41, 9);
    let failed = DepositQueueMember::Ordinary(failed_deposit.clone());
    let imported = ImportedTempoBlockInput::new(
        77,
        alloy_primitives::U256::ZERO,
        ordinary_append_operations(std::slice::from_ref(&failed_deposit)),
    );
    let reveal = reveal_key(0x52);
    let cipher = encrypted_sender(0x63);
    let zone = block(
        B256::repeat_byte(0xb2),
        42,
        ZoneDepositPrefixInput::new(
            Vec::new(),
            vec![failed.clone()],
            vec![AuthenticatedDepositOutcome::OrdinaryFailed],
        ),
        vec![ZoneOperation::user_withdrawal_accepted(user_withdrawal(
            token, 0x51, 200, 0, reveal,
        ))],
        Some(finalization(
            3,
            42,
            vec![Bytes::new(), Bytes::new(), cipher.clone()],
        )),
    );
    let expected = commit_full_block(&mut state, &imported, &zone).unwrap();
    let event = expected.zone_block().finalized_batch().unwrap();
    let batch = finalized_batch(&state, event.batch());

    assert_eq!(event.batch().withdrawal_batch_index.get(), 1);
    assert_eq!(batch.members().first_withdrawal_index(), 0);
    assert_eq!(batch.members().member_count(), 3);
    assert_eq!(
        batch.members().withdrawal_queue_hash(),
        event.withdrawal_queue_hash()
    );
    // Independently cross-checked with Foundry `cast abi-encode` and `cast keccak`.
    assert_eq!(
        event.withdrawal_queue_hash(),
        b256!("e04ec2a6c6937744ff58c7c3a1f9664fa37dda4915b6159871d1ceff382eff04")
    );

    let boundary = batch.boundary();
    assert_eq!(boundary.first_zone_parent_hash, B256::ZERO);
    assert_eq!(boundary.final_zone_block_hash, B256::repeat_byte(0xb2));
    assert_eq!(boundary.first_processed_deposit.hash, B256::ZERO);
    assert_eq!(boundary.first_processed_deposit.number, 0);
    assert_eq!(
        boundary.final_processed_deposit.hash,
        failed.hash_after(B256::ZERO)
    );
    assert_eq!(boundary.final_processed_deposit.number, 1);
    assert_eq!(boundary.final_imported_tempo_block_number, 77);
    assert_eq!(boundary.final_zone_height, 42);

    for index in 0..3 {
        let id = WithdrawalId {
            zone_id: ZONE_ID,
            withdrawal_index: index,
        };
        assert!(matches!(
            state.withdrawal(id),
            Some(WithdrawalOwner::Finalized(_))
        ));
    }
    let Some(WithdrawalOwner::Finalized(revealed)) = state.withdrawal(WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index: 2,
    }) else {
        panic!("revealed user must be finalized")
    };
    assert_eq!(revealed.preimage().encrypted_sender(), &cipher);
    assert_eq!(
        state.zone().last_batch().withdrawal_queue_hash(),
        event.withdrawal_queue_hash()
    );
    assert_eq!(state.zone().last_batch().withdrawal_batch_index(), 1);
}

#[test]
fn empty_batches_advance_index_retain_ranges_and_update_the_exact_last_batch_pair() {
    let token = token(0x93);
    let mut state = created_state(token);
    let first_hash = B256::repeat_byte(0xc1);
    let first = commit_full_block(
        &mut state,
        &ImportedTempoBlockInput::new(500, alloy_primitives::U256::ZERO, Vec::new()),
        &block(
            first_hash,
            60,
            ZoneDepositPrefixInput::default(),
            Vec::new(),
            Some(finalization(0, 60, Vec::new())),
        ),
    )
    .unwrap();
    let first_event = first.zone_block().finalized_batch().unwrap();
    let first_batch = finalized_batch(&state, first_event.batch());
    assert_eq!(first_event.withdrawal_queue_hash(), B256::ZERO);
    assert_eq!(first_event.batch().withdrawal_batch_index.get(), 1);
    assert_eq!(first_batch.members().first_withdrawal_index(), 0);
    assert_eq!(first_batch.members().member_count(), 0);

    let second_hash = B256::repeat_byte(0xc2);
    let second = commit_full_block(
        &mut state,
        &ImportedTempoBlockInput::new(501, alloy_primitives::U256::ZERO, Vec::new()),
        &block(
            second_hash,
            61,
            ZoneDepositPrefixInput::default(),
            Vec::new(),
            Some(finalization(0, 61, Vec::new())),
        ),
    )
    .unwrap();
    let second_event = second.zone_block().finalized_batch().unwrap();
    let second_batch = finalized_batch(&state, second_event.batch());
    assert_eq!(second_event.withdrawal_queue_hash(), B256::ZERO);
    assert_eq!(second_event.batch().withdrawal_batch_index.get(), 2);
    assert_eq!(second_batch.members().first_withdrawal_index(), 0);
    assert_eq!(second_batch.members().member_count(), 0);
    assert_eq!(second_batch.boundary().first_zone_parent_hash, first_hash);
    assert_eq!(second_batch.boundary().final_zone_block_hash, second_hash);
    assert_eq!(
        state.zone().last_batch().withdrawal_queue_hash(),
        B256::ZERO
    );
    assert_eq!(state.zone().last_batch().withdrawal_batch_index(), 2);
    assert_eq!(
        state.zone().batch_start().first_zone_parent_hash(),
        second_hash
    );
}

#[test]
fn finalization_rejects_block_count_and_sender_cardinality_mismatches_atomically() {
    let state = state_with_pending_users(vec![Bytes::new()]);
    let before = state.clone();
    let cases = [
        (
            finalization(1, 999, vec![Bytes::new()]),
            ModelError::FinalizationBlockNumberMismatch {
                expected: 21,
                actual: 999,
            },
        ),
        (
            finalization(0, 21, Vec::new()),
            ModelError::FinalizationCountMismatch {
                expected: 1,
                actual: 0,
            },
        ),
        (
            finalization(2, 21, vec![Bytes::new(), Bytes::new()]),
            ModelError::FinalizationCountMismatch {
                expected: 1,
                actual: 2,
            },
        ),
        (
            finalization(1, 21, Vec::new()),
            ModelError::FinalizationSenderCountMismatch {
                declared: 1,
                actual: 0,
            },
        ),
        (
            finalization(1, 21, vec![Bytes::new(), Bytes::new()]),
            ModelError::FinalizationSenderCountMismatch {
                declared: 1,
                actual: 2,
            },
        ),
    ];

    for (input, expected_error) in cases {
        let result = apply_full_block(
            &state,
            &ImportedTempoBlockInput::new(11, alloy_primitives::U256::ZERO, Vec::new()),
            &block(
                B256::repeat_byte(0xa1),
                21,
                ZoneDepositPrefixInput::default(),
                Vec::new(),
                Some(input),
            ),
        );
        assert_eq!(result.err(), Some(expected_error));
        assert_eq!(state, before);
    }
}

#[test]
fn mixed_sender_shape_reorder_fails_but_the_positionally_correct_vector_finalizes() {
    let state = state_with_pending_users(vec![Bytes::new(), reveal_key(0x44)]);
    let cipher = encrypted_sender(0x55);
    let wrong = block(
        B256::repeat_byte(0xa2),
        22,
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        Some(finalization(2, 22, vec![cipher.clone(), Bytes::new()])),
    );
    assert_eq!(
        apply_full_block(
            &state,
            &ImportedTempoBlockInput::new(12, alloy_primitives::U256::ZERO, Vec::new()),
            &wrong,
        )
        .err(),
        Some(ModelError::WithdrawalData(
            WithdrawalDataError::InvalidEncryptedSenderLength {
                actual: AUTHENTICATED_WITHDRAWAL_SIZE,
                expected: 0,
            }
        ))
    );

    let correct = block(
        B256::repeat_byte(0xa2),
        22,
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        Some(finalization(2, 22, vec![Bytes::new(), cipher.clone()])),
    );
    let (next, _) = apply_full_block(
        &state,
        &ImportedTempoBlockInput::new(12, alloy_primitives::U256::ZERO, Vec::new()),
        &correct,
    )
    .unwrap();
    let Some(WithdrawalOwner::Finalized(second)) = next.withdrawal(WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index: 1,
    }) else {
        panic!("second withdrawal must be finalized")
    };
    assert_eq!(second.preimage().encrypted_sender(), &cipher);
}

#[test]
fn same_shape_encrypted_senders_remain_opaque_and_are_not_order_authenticated() {
    let state = state_with_pending_users(vec![reveal_key(0x41), reveal_key(0x42)]);
    let first = encrypted_sender(0x51);
    let second = encrypted_sender(0x52);
    let finalize = |senders| {
        block(
            B256::repeat_byte(0xa4),
            24,
            ZoneDepositPrefixInput::default(),
            Vec::new(),
            Some(finalization(2, 24, senders)),
        )
    };

    let (ordered, ordered_expected) = apply_full_block(
        &state,
        &ImportedTempoBlockInput::new(14, alloy_primitives::U256::ZERO, Vec::new()),
        &finalize(vec![first.clone(), second.clone()]),
    )
    .unwrap();
    let (swapped, swapped_expected) = apply_full_block(
        &state,
        &ImportedTempoBlockInput::new(14, alloy_primitives::U256::ZERO, Vec::new()),
        &finalize(vec![second.clone(), first.clone()]),
    )
    .unwrap();

    assert_ne!(
        ordered_expected
            .zone_block()
            .finalized_batch()
            .unwrap()
            .withdrawal_queue_hash(),
        swapped_expected
            .zone_block()
            .finalized_batch()
            .unwrap()
            .withdrawal_queue_hash(),
        "opaque same-shape inputs are accepted positionally and change the commitment"
    );
    for (materialized, expected) in [(&ordered, [&first, &second]), (&swapped, [&second, &first])] {
        for (withdrawal_index, encrypted_sender) in expected.into_iter().enumerate() {
            let Some(WithdrawalOwner::Finalized(owner)) = materialized.withdrawal(WithdrawalId {
                zone_id: ZONE_ID,
                withdrawal_index: u64::try_from(withdrawal_index).unwrap(),
            }) else {
                panic!("withdrawal {withdrawal_index} must be finalized")
            };
            assert_eq!(owner.preimage().encrypted_sender(), encrypted_sender);
        }
    }
}

#[test]
fn a_late_encrypted_sender_failure_discards_earlier_in_place_finalizations() {
    let state = state_with_pending_users(vec![Bytes::new(), reveal_key(0x45)]);
    let before = state.clone();
    let input = block(
        B256::repeat_byte(0xa3),
        23,
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        Some(finalization(2, 23, vec![Bytes::new(), Bytes::new()])),
    );
    assert_eq!(
        apply_full_block(
            &state,
            &ImportedTempoBlockInput::new(13, alloy_primitives::U256::ZERO, Vec::new()),
            &input,
        )
        .err(),
        Some(ModelError::WithdrawalData(
            WithdrawalDataError::InvalidEncryptedSenderLength {
                actual: 0,
                expected: AUTHENTICATED_WITHDRAWAL_SIZE,
            }
        ))
    );
    assert_eq!(state, before);
    for index in 0..2 {
        assert!(matches!(
            state.withdrawal(WithdrawalId {
                zone_id: ZONE_ID,
                withdrawal_index: index,
            }),
            Some(WithdrawalOwner::Pending(PendingWithdrawal::User(_)))
        ));
    }
    assert_eq!(state.zone().last_batch().withdrawal_batch_index(), 0);
}

#[test]
fn multi_block_batch_preserves_its_start_and_uses_the_imported_tempo_number_once() {
    let token = token(0x94);
    let mut state = created_state(token);
    let anchor_hash = B256::repeat_byte(0xd0);
    commit_full_block(
        &mut state,
        &ImportedTempoBlockInput::new(400, alloy_primitives::U256::ZERO, Vec::new()),
        &block(
            anchor_hash,
            70,
            ZoneDepositPrefixInput::default(),
            Vec::new(),
            Some(finalization(0, 70, Vec::new())),
        ),
    )
    .unwrap();

    let first_deposit = ordinary(token, 0x61, 10);
    let first = DepositQueueMember::Ordinary(first_deposit.clone());
    commit_full_block(
        &mut state,
        &ImportedTempoBlockInput::new(
            401,
            alloy_primitives::U256::ZERO,
            ordinary_append_operations(std::slice::from_ref(&first_deposit)),
        ),
        &block(
            B256::repeat_byte(0xd1),
            71,
            ZoneDepositPrefixInput::new(
                Vec::new(),
                vec![first.clone()],
                vec![AuthenticatedDepositOutcome::OrdinaryMinted {
                    recipient: Address::repeat_byte(0x71),
                    memo: B256::ZERO,
                }],
            ),
            Vec::new(),
            None,
        ),
    )
    .unwrap();
    assert_eq!(
        state.zone().batch_start().first_zone_parent_hash(),
        anchor_hash
    );
    assert_eq!(
        state
            .zone()
            .batch_start()
            .first_processed_deposit()
            .number(),
        0
    );

    let second_deposit = ordinary(token, 0x62, 20);
    let second = DepositQueueMember::Ordinary(second_deposit.clone());
    let second_cursor = second.hash_after(first.hash_after(B256::ZERO));
    let expected = commit_full_block(
        &mut state,
        &ImportedTempoBlockInput::new(
            402,
            alloy_primitives::U256::ZERO,
            ordinary_append_operations(std::slice::from_ref(&second_deposit)),
        ),
        &block(
            B256::repeat_byte(0xd2),
            72,
            ZoneDepositPrefixInput::new(
                Vec::new(),
                vec![second],
                vec![AuthenticatedDepositOutcome::OrdinaryMinted {
                    recipient: Address::repeat_byte(0x72),
                    memo: B256::ZERO,
                }],
            ),
            Vec::new(),
            Some(finalization(0, 72, Vec::new())),
        ),
    )
    .unwrap();
    let event = expected.zone_block().finalized_batch().unwrap();
    let batch = finalized_batch(&state, event.batch());
    let boundary = batch.boundary();
    assert_eq!(boundary.first_zone_parent_hash, anchor_hash);
    assert_eq!(boundary.final_zone_block_hash, B256::repeat_byte(0xd2));
    assert_eq!(boundary.first_processed_deposit.hash, B256::ZERO);
    assert_eq!(boundary.first_processed_deposit.number, 0);
    assert_eq!(boundary.final_processed_deposit.hash, second_cursor);
    assert_eq!(boundary.final_processed_deposit.number, 2);
    assert_eq!(boundary.final_imported_tempo_block_number, 402);
    assert_ne!(
        boundary.final_imported_tempo_block_number,
        boundary.final_zone_height
    );
    assert_eq!(boundary.final_zone_height, 72);
}

#[test]
fn later_batches_leave_prior_finalized_rows_and_ranges_untouched_exactly_once() {
    let mut state = state_with_pending_users(vec![Bytes::new()]);
    commit_full_block(
        &mut state,
        &ImportedTempoBlockInput::new(20, alloy_primitives::U256::ZERO, Vec::new()),
        &block(
            B256::repeat_byte(0xe1),
            30,
            ZoneDepositPrefixInput::default(),
            Vec::new(),
            Some(finalization(1, 30, vec![Bytes::new()])),
        ),
    )
    .unwrap();
    let first_id = WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index: 0,
    };
    let first_owner = state.withdrawal(first_id).unwrap().clone();
    let first_batch_id = BatchId {
        zone_id: ZONE_ID,
        withdrawal_batch_index: NonZeroU64::new(1).unwrap(),
    };
    let first_batch = state.batch(first_batch_id).unwrap().clone();

    let token = token(0x91);
    commit_full_block(
        &mut state,
        &ImportedTempoBlockInput::new(21, alloy_primitives::U256::ZERO, Vec::new()),
        &block(
            B256::repeat_byte(0xe2),
            31,
            ZoneDepositPrefixInput::default(),
            vec![ZoneOperation::user_withdrawal_accepted(user_withdrawal(
                token,
                0x70,
                200,
                0,
                Bytes::new(),
            ))],
            Some(finalization(1, 31, vec![Bytes::new()])),
        ),
    )
    .unwrap();

    assert_eq!(state.withdrawal(first_id), Some(&first_owner));
    assert_eq!(state.batch(first_batch_id), Some(&first_batch));
    assert!(matches!(
        state.withdrawal(WithdrawalId {
            zone_id: ZONE_ID,
            withdrawal_index: 1,
        }),
        Some(WithdrawalOwner::Finalized(_))
    ));
    let second_batch_id = BatchId {
        zone_id: ZONE_ID,
        withdrawal_batch_index: NonZeroU64::new(2).unwrap(),
    };
    let second_batch = finalized_batch(&state, second_batch_id);
    assert_eq!(second_batch.members().first_withdrawal_index(), 1);
    assert_eq!(second_batch.members().member_count(), 1);

    let before_replay = state.clone();
    let replay = block(
        B256::repeat_byte(0xe3),
        32,
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        Some(finalization(1, 32, vec![Bytes::new()])),
    );
    assert_eq!(
        apply_full_block(
            &state,
            &ImportedTempoBlockInput::new(22, alloy_primitives::U256::ZERO, Vec::new()),
            &replay,
        )
        .err(),
        Some(ModelError::FinalizationCountMismatch {
            expected: 0,
            actual: 1,
        })
    );
    assert_eq!(state, before_replay);
}

#[test]
fn batch_index_overflow_invalid_range_and_owner_collision_fail_atomically() {
    let token = token(0x95);

    let mut overflow = created_state(token);
    overflow.set_last_batch_for_test(ZoneLastBatch {
        withdrawal_queue_hash: B256::repeat_byte(0xf0),
        withdrawal_batch_index: u64::MAX,
    });
    let before = overflow.clone();
    assert_eq!(
        apply_full_block(
            &overflow,
            &ImportedTempoBlockInput::new(30, alloy_primitives::U256::ZERO, Vec::new()),
            &block(
                B256::repeat_byte(0xf1),
                80,
                ZoneDepositPrefixInput::default(),
                Vec::new(),
                Some(finalization(0, 80, Vec::new())),
            ),
        )
        .err(),
        Some(ModelError::WithdrawalBatchIndexOverflow)
    );
    assert_eq!(overflow, before);

    let mut invalid_range = created_state(token);
    invalid_range.zone.batch_start.first_withdrawal_index = 1;
    let before = invalid_range.clone();
    assert_eq!(
        apply_full_block(
            &invalid_range,
            &ImportedTempoBlockInput::new(31, alloy_primitives::U256::ZERO, Vec::new()),
            &block(
                B256::repeat_byte(0xf2),
                81,
                ZoneDepositPrefixInput::default(),
                Vec::new(),
                Some(finalization(0, 81, Vec::new())),
            ),
        )
        .err(),
        Some(ModelError::InvalidBatchWithdrawalRange { first: 1, next: 0 })
    );
    assert_eq!(invalid_range, before);

    let mut collision = created_state(token);
    commit_full_block(
        &mut collision,
        &ImportedTempoBlockInput::new(32, alloy_primitives::U256::ZERO, Vec::new()),
        &block(
            B256::repeat_byte(0xf3),
            82,
            ZoneDepositPrefixInput::default(),
            Vec::new(),
            Some(finalization(0, 82, Vec::new())),
        ),
    )
    .unwrap();
    collision.set_last_batch_for_test(ZoneLastBatch::ZERO);
    let before = collision.clone();
    assert_eq!(
        apply_full_block(
            &collision,
            &ImportedTempoBlockInput::new(33, alloy_primitives::U256::ZERO, Vec::new()),
            &block(
                B256::repeat_byte(0xf4),
                83,
                ZoneDepositPrefixInput::default(),
                Vec::new(),
                Some(finalization(0, 83, Vec::new())),
            ),
        )
        .err(),
        Some(ModelError::BatchOwnerCollision {
            withdrawal_batch_index: 1,
        })
    );
    assert_eq!(collision, before);
}
