use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256, Bytes, U256};
use tempo_zone_contracts::ZonePortal;

use super::super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    encoding::{UserWithdrawalIdentity, UserWithdrawalRequest},
    ownership::{
        BatchBoundary, BatchId, BatchMembers, BatchOwner, DepositCursor, DepositId,
        FailedDepositPendingWithdrawal, FallbackId, FallbackOwner, FinalizedBatchState,
        FinalizedWithdrawal, PendingWithdrawal, PortalQueueId, RefundAccount, SubmittedBatchState,
        UserPendingWithdrawal, WithdrawalId, WithdrawalOwner,
    },
    state::{ModelState, ZoneLastBatch},
};

const FINAL_ZONE_HASH: B256 = B256::repeat_byte(0xab);
const FINAL_TEMPO_NUMBER: u64 = 90;
const FINAL_ZONE_HEIGHT: u64 = 12;

#[derive(Debug, Clone, Copy)]
enum SubmissionCase {
    Empty,
    Nonempty,
}

#[tokio::test]
async fn empty_and_nonempty_batch_submission_cross_call_projection_and_output_reconciliation() {
    for case in [SubmissionCase::Empty, SubmissionCase::Nonempty] {
        let imported = imported_header(0);
        let (mut model, finalized_withdrawals, collateral) = match case {
            SubmissionCase::Empty => (created_model(TokenAccounting::ZERO), Vec::new(), U256::ZERO),
            SubmissionCase::Nonempty => {
                let amount = 10_u128;
                let (model, finalized, _, _) =
                    user_processing_fixture(amount, 0x41, 0, Bytes::new());
                (model, vec![finalized], U256::from(amount))
            }
        };
        let withdrawals = finalized_withdrawals
            .iter()
            .map(|finalized| finalized.preimage().clone())
            .collect::<Vec<_>>();
        let wire_withdrawals = finalized_withdrawals
            .iter()
            .map(portal_withdrawal)
            .collect::<Vec<_>>();
        let queue_hash = independent_withdrawal_queue_hash(&wire_withdrawals);
        let members = BatchMembers::from_withdrawals(0, &withdrawals).unwrap();
        assert_eq!(members.withdrawal_queue_hash(), queue_hash);
        let batch = batch_id();
        model.seed_batch_for_test(
            batch,
            BatchOwner::Finalized(FinalizedBatchState::new(boundary(), members)),
        );
        model.set_last_batch_for_test(ZoneLastBatch::for_test(queue_hash, 1));

        let call = ZonePortal::submitBatchCall {
            tempoBlockNumber: FINAL_TEMPO_NUMBER,
            recentTempoBlockNumber: FINAL_TEMPO_NUMBER,
            blockTransition: ZonePortal::BlockTransition {
                prevBlockHash: B256::ZERO,
                nextBlockHash: FINAL_ZONE_HASH,
            },
            depositQueueTransition: ZonePortal::DepositQueueTransition {
                prevProcessedHash: B256::ZERO,
                nextProcessedHash: B256::ZERO,
                prevDepositNumber: 0,
                nextDepositNumber: 0,
            },
            withdrawalQueueHash: queue_hash,
            verifierConfig: Bytes::new(),
            proof: Bytes::new(),
            nextZoneHeight: U256::from(FINAL_ZONE_HEIGHT),
            signatures: Vec::new(),
        };
        let queue_index = match case {
            SubmissionCase::Empty => crate::model::constants::NO_WITHDRAWAL_QUEUE_INDEX,
            SubmissionCase::Nonempty => U256::ZERO,
        };
        let l1 = vec![l1_transaction(
            1,
            Some(direct_call(&call)),
            vec![portal_event(ZonePortal::BatchSubmitted {
                withdrawalBatchIndex: 1,
                withdrawalQueueIndex: queue_index,
                nextProcessedDepositQueueHash: B256::ZERO,
                nextBlockHash: FINAL_ZONE_HASH,
                withdrawalQueueHash: queue_hash,
                lastProcessedDepositNumber: 0,
            })],
        )];
        let l2 = zone_observation(
            &imported,
            Vec::new(),
            Vec::new(),
            advance_logs(&imported, Vec::new(), B256::ZERO, 0),
            Vec::new(),
            None,
        );
        let exact = ExactPostState::from_model(&model).with_supply(INITIAL_TOKEN, U256::ZERO);
        let checker = run_valid_block(model, &imported, l1, &l2, &[collateral], exact, false).await;

        match case {
            SubmissionCase::Empty => assert!(checker.model().batch(batch).is_none()),
            SubmissionCase::Nonempty => assert!(matches!(
                checker.model().batch(batch),
                Some(BatchOwner::Submitted(_))
            )),
        }
        let settlement = checker.model().portal().created().unwrap().settlement();
        assert_eq!(settlement.withdrawal_batch_index(), 1);
        assert_eq!(
            settlement.withdrawal_queue_tail(),
            U256::from(matches!(case, SubmissionCase::Nonempty) as u8)
        );
    }
}

#[derive(Debug, Clone, Copy)]
enum ProcessingCase {
    UserDelivered,
    UserBounced,
    FailedDepositPaid,
    FailedDepositPending,
}

#[tokio::test]
async fn every_portal_processing_disposition_is_checked_through_the_direct_call_grammar() {
    for case in [
        ProcessingCase::UserDelivered,
        ProcessingCase::UserBounced,
        ProcessingCase::FailedDepositPaid,
        ProcessingCase::FailedDepositPending,
    ] {
        let imported = imported_header(1_000_000_000_000);
        let amount = 100_u128;
        let (mut model, finalized, withdrawal, fallback) = match case {
            ProcessingCase::UserDelivered => {
                user_processing_fixture(amount, 0x45, 100, Bytes::from_static(b"callback"))
            }
            ProcessingCase::UserBounced => user_processing_fixture(amount, 0x41, 0, Bytes::new()),
            ProcessingCase::FailedDepositPaid | ProcessingCase::FailedDepositPending => {
                failed_processing_fixture(amount)
            }
        };
        let wire = portal_withdrawal(&finalized);
        seed_submitted_batch(&mut model, finalized, &wire);
        let call = ZonePortal::processWithdrawalsCall {
            withdrawals: vec![wire.clone()],
            remainingQueue: B256::ZERO,
        };
        let mut l1 = Vec::new();
        if matches!(
            case,
            ProcessingCase::FailedDepositPaid | ProcessingCase::FailedDepositPending
        ) {
            l1.push(l1_transaction(
                1,
                None,
                vec![portal_event(ZonePortal::BouncebackGasUpdated {
                    bouncebackGas: 7,
                })],
            ));
        }
        let processing_events = match case {
            ProcessingCase::UserDelivered => {
                let callback = ordinary(INITIAL_TOKEN, 0x61, 20);
                let queue_hash = independent_ordinary_queue_hash(&callback, B256::ZERO);
                vec![
                    portal_event(ZonePortal::DepositMade {
                        newCurrentDepositQueueHash: queue_hash,
                        sender: callback.sender,
                        token: callback.token,
                        netAmount: callback.amount,
                        fee: 0,
                        keyIndex: callback.keyIndex,
                        ephemeralPubkeyX: callback.encrypted.ephemeralPubkeyX,
                        ephemeralPubkeyYParity: callback.encrypted.ephemeralPubkeyYParity,
                        ciphertext: callback.encrypted.ciphertext,
                        nonce: callback.encrypted.nonce,
                        tag: callback.encrypted.tag,
                        tempoRefundRecipient: callback.tempoRefundRecipient,
                        depositNumber: 1,
                    }),
                    portal_event(ZonePortal::WithdrawalProcessed {
                        to: wire.to,
                        senderTag: wire.senderTag,
                        token: wire.token,
                        amount: wire.amount,
                        callbackSuccess: true,
                    }),
                ]
            }
            ProcessingCase::UserBounced => {
                let bounce = tempo_zone_contracts::IZoneInbox::WithdrawalBounceBackDeposit {
                    token: wire.token,
                    to: fallback_recipient(wire.fallbackNonce),
                    amount: wire.amount,
                };
                let queue_hash = independent_bounce_queue_hash(&bounce, B256::ZERO);
                vec![
                    portal_event(ZonePortal::WithdrawalBounceBack {
                        newCurrentDepositQueueHash: queue_hash,
                        fallbackNonce: wire.fallbackNonce,
                        token: wire.token,
                        amount: wire.amount,
                        depositNumber: 1,
                    }),
                    portal_event(ZonePortal::WithdrawalProcessed {
                        to: wire.to,
                        senderTag: wire.senderTag,
                        token: wire.token,
                        amount: wire.amount,
                        callbackSuccess: false,
                    }),
                ]
            }
            ProcessingCase::FailedDepositPaid => {
                vec![portal_event(ZonePortal::DepositBounceBack {
                    tempoRefundRecipient: wire.to,
                    token: wire.token,
                    amount: amount - 7,
                    bouncebackFee: 7,
                })]
            }
            ProcessingCase::FailedDepositPending => {
                vec![portal_event(ZonePortal::DepositBounceBackPending {
                    tempoRefundRecipient: wire.to,
                    token: wire.token,
                    amount: amount - 7,
                    bouncebackFee: 7,
                })]
            }
        };
        l1.push(l1_transaction(
            2,
            Some(direct_call(&call)),
            processing_events,
        ));
        if matches!(
            case,
            ProcessingCase::FailedDepositPaid | ProcessingCase::FailedDepositPending
        ) {
            l1.push(l1_transaction(
                3,
                None,
                vec![portal_event(ZonePortal::BouncebackGasUpdated {
                    bouncebackGas: 99,
                })],
            ));
        }
        let l2 = zone_observation(
            &imported,
            Vec::new(),
            Vec::new(),
            advance_logs(&imported, Vec::new(), B256::ZERO, 0),
            Vec::new(),
            None,
        );
        let collateral = match case {
            ProcessingCase::UserDelivered => U256::from(20),
            ProcessingCase::FailedDepositPaid => U256::ZERO,
            ProcessingCase::UserBounced => U256::from(amount),
            ProcessingCase::FailedDepositPending => U256::from(amount - 7),
        };
        let exact = ExactPostState::from_model(&model).with_supply(INITIAL_TOKEN, U256::ZERO);
        let checker = run_valid_block(model, &imported, l1, &l2, &[collateral], exact, false).await;
        let state = checker.model();
        assert!(state.withdrawal(withdrawal).is_none());
        assert!(state.batch(batch_id()).is_none());
        assert_eq!(
            state
                .portal()
                .created()
                .unwrap()
                .settlement()
                .withdrawal_queue_head(),
            U256::ONE
        );

        match case {
            ProcessingCase::UserDelivered => {
                assert!(state.fallback_owner(fallback.unwrap()).is_none());
                assert_eq!(
                    state
                        .token(INITIAL_TOKEN)
                        .unwrap()
                        .accounting()
                        .withdrawal_liability,
                    U256::ZERO
                );
                assert_eq!(
                    state
                        .token(INITIAL_TOKEN)
                        .unwrap()
                        .accounting()
                        .deposit_liability,
                    U256::from(20)
                );
                assert_eq!(state.pending_deposits().len(), 1);
            }
            ProcessingCase::UserBounced => {
                assert!(matches!(
                    state.fallback_owner(fallback.unwrap()),
                    Some(FallbackOwner::BounceBackQueued { .. })
                ));
                assert_eq!(state.pending_deposits().len(), 1);
            }
            ProcessingCase::FailedDepositPaid => {
                assert_eq!(
                    state
                        .token(INITIAL_TOKEN)
                        .unwrap()
                        .accounting()
                        .deposit_liability,
                    U256::ZERO
                );
                assert_eq!(
                    state.portal().created().unwrap().config().bounceback_gas(),
                    99
                );
            }
            ProcessingCase::FailedDepositPending => {
                assert_eq!(
                    state
                        .token(INITIAL_TOKEN)
                        .unwrap()
                        .accounting()
                        .deposit_liability,
                    U256::from(93)
                );
                assert_eq!(
                    state.portal_refund_total(RefundAccount {
                        token: INITIAL_TOKEN,
                        recipient: wire.to,
                    }),
                    93
                );
                assert_eq!(
                    state.portal().created().unwrap().config().bounceback_gas(),
                    99
                );
            }
        }
    }
}

#[tokio::test]
async fn empty_process_withdrawals_is_an_end_to_end_noop_even_with_a_nonzero_suffix() {
    let imported = imported_header(0);
    let model = created_model(TokenAccounting::ZERO);
    let before = model.clone();
    let call = ZonePortal::processWithdrawalsCall {
        withdrawals: Vec::new(),
        remainingQueue: B256::repeat_byte(0xef),
    };
    let l1 = vec![l1_transaction(1, Some(direct_call(&call)), Vec::new())];
    let l2 = zone_observation(
        &imported,
        Vec::new(),
        Vec::new(),
        advance_logs(&imported, Vec::new(), B256::ZERO, 0),
        Vec::new(),
        None,
    );
    let exact = ExactPostState::from_model(&model).with_supply(INITIAL_TOKEN, U256::ZERO);
    let checker = run_valid_block(model, &imported, l1, &l2, &[U256::ZERO], exact, false).await;

    assert_eq!(checker.model(), &before);
}

#[tokio::test]
async fn partial_processing_keeps_the_exact_suffix_and_only_closes_the_consumed_owner() {
    let imported = imported_header(0);
    let (first, first_id, first_fallback) = finalized_user(0, 1, 10);
    let (second, second_id, second_fallback) = finalized_user(1, 2, 20);
    let first_wire = portal_withdrawal(&first);
    let second_wire = portal_withdrawal(&second);
    let remaining_queue = independent_withdrawal_queue_hash(std::slice::from_ref(&second_wire));
    let full_queue = independent_withdrawal_queue_hash(&[first_wire.clone(), second_wire]);
    let mut model = created_model(TokenAccounting {
        supply: U256::ZERO,
        deposit_liability: U256::ZERO,
        withdrawal_liability: U256::from(30),
    });
    for (id, finalized, fallback, amount) in [
        (first_id, first.clone(), first_fallback, 10_u128),
        (second_id, second.clone(), second_fallback, 20_u128),
    ] {
        model.seed_withdrawal_for_test(id, WithdrawalOwner::Finalized(finalized));
        model.seed_fallback_owner_for_test(
            fallback,
            FallbackOwner::Held {
                withdrawal: id,
                token: INITIAL_TOKEN,
                amount: NonZeroU128::new(amount).unwrap(),
            },
        );
    }
    let members =
        BatchMembers::from_withdrawals(0, &[first.preimage().clone(), second.preimage().clone()])
            .unwrap();
    assert_eq!(members.withdrawal_queue_hash(), full_queue);
    let submitted = SubmittedBatchState::new(
        FinalizedBatchState::new(boundary(), members),
        PortalQueueId::new(portal(), U256::ZERO).unwrap(),
    )
    .unwrap();
    model.seed_batch_for_test(batch_id(), BatchOwner::Submitted(submitted));
    model.set_last_batch_for_test(ZoneLastBatch::for_test(full_queue, 1));
    model.set_portal_withdrawal_queue_for_test(U256::ZERO, U256::ONE);

    let call = ZonePortal::processWithdrawalsCall {
        withdrawals: vec![first_wire.clone()],
        remainingQueue: remaining_queue,
    };
    let l1 = vec![l1_transaction(
        1,
        Some(direct_call(&call)),
        vec![portal_event(ZonePortal::WithdrawalProcessed {
            to: first_wire.to,
            senderTag: first_wire.senderTag,
            token: first_wire.token,
            amount: first_wire.amount,
            callbackSuccess: true,
        })],
    )];
    let l2 = zone_observation(
        &imported,
        Vec::new(),
        Vec::new(),
        advance_logs(&imported, Vec::new(), B256::ZERO, 0),
        Vec::new(),
        None,
    );
    let exact = ExactPostState::from_model(&model).with_supply(INITIAL_TOKEN, U256::ZERO);
    let checker = run_valid_block(model, &imported, l1, &l2, &[U256::from(20)], exact, false).await;

    let state = checker.model();
    assert!(state.withdrawal(first_id).is_none());
    assert!(state.fallback_owner(first_fallback).is_none());
    assert!(matches!(
        state.withdrawal(second_id),
        Some(WithdrawalOwner::Finalized(_))
    ));
    assert!(state.fallback_owner(second_fallback).is_some());
    let Some(BatchOwner::Submitted(batch)) = state.batch(batch_id()) else {
        panic!("partial processing must keep the submitted batch open")
    };
    assert_eq!(batch.next_processing_ordinal(), 1);
    assert_eq!(batch.remaining_queue_hash(), remaining_queue);
    assert_eq!(
        state
            .token(INITIAL_TOKEN)
            .unwrap()
            .accounting()
            .withdrawal_liability,
        U256::from(20)
    );
}

fn boundary() -> BatchBoundary {
    BatchBoundary {
        first_zone_parent_hash: B256::ZERO,
        final_zone_block_hash: FINAL_ZONE_HASH,
        first_processed_deposit: DepositCursor::default(),
        final_processed_deposit: DepositCursor::default(),
        final_imported_tempo_block_number: FINAL_TEMPO_NUMBER,
        final_zone_height: FINAL_ZONE_HEIGHT,
    }
}

fn batch_id() -> BatchId {
    BatchId {
        zone_id: ZONE_ID,
        withdrawal_batch_index: NonZeroU64::new(1).unwrap(),
    }
}

fn user_processing_fixture(
    amount: u128,
    seed: u8,
    gas_limit: u64,
    callback_data: Bytes,
) -> (
    ModelState,
    FinalizedWithdrawal,
    WithdrawalId,
    Option<FallbackId>,
) {
    let withdrawal = WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index: 0,
    };
    let fallback_nonce = NonZeroU64::new(1).unwrap();
    let identity = UserWithdrawalIdentity::new(
        Address::repeat_byte(seed),
        B256::repeat_byte(seed.wrapping_add(1)),
        fallback_nonce,
    )
    .unwrap();
    let request = UserWithdrawalRequest::new(
        INITIAL_TOKEN,
        Address::repeat_byte(seed.wrapping_add(2)),
        amount,
        B256::repeat_byte(seed.wrapping_add(3)),
        gas_limit,
        callback_data,
    )
    .unwrap();
    let finalized = UserPendingWithdrawal::new(identity, request, Bytes::new())
        .unwrap()
        .finalize(Bytes::new())
        .unwrap();
    let fallback = FallbackId {
        zone_id: ZONE_ID,
        fallback_nonce,
    };
    let mut model = created_model(TokenAccounting {
        supply: U256::ZERO,
        deposit_liability: U256::ZERO,
        withdrawal_liability: U256::from(amount),
    });
    model.seed_withdrawal_for_test(withdrawal, WithdrawalOwner::Finalized(finalized.clone()));
    model.seed_fallback_owner_for_test(
        fallback,
        FallbackOwner::Held {
            withdrawal,
            token: INITIAL_TOKEN,
            amount: NonZeroU128::new(amount).unwrap(),
        },
    );
    (model, finalized, withdrawal, Some(fallback))
}

fn failed_processing_fixture(
    amount: u128,
) -> (
    ModelState,
    FinalizedWithdrawal,
    WithdrawalId,
    Option<FallbackId>,
) {
    let withdrawal = WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index: 0,
    };
    let deposit = DepositId {
        portal: portal(),
        deposit_number: NonZeroU64::new(1).unwrap(),
    };
    let ordinary = model_ordinary(&ordinary(INITIAL_TOKEN, 0x51, amount));
    let finalized = PendingWithdrawal::FailedDeposit(
        FailedDepositPendingWithdrawal::from_failed_deposit(deposit, ordinary),
    )
    .finalize(Bytes::new())
    .unwrap();
    let mut model = created_model(TokenAccounting {
        supply: U256::ZERO,
        deposit_liability: U256::from(amount),
        withdrawal_liability: U256::ZERO,
    });
    model.seed_withdrawal_for_test(withdrawal, WithdrawalOwner::Finalized(finalized.clone()));
    (model, finalized, withdrawal, None)
}

fn finalized_user(
    withdrawal_index: u64,
    fallback_nonce: u64,
    amount: u128,
) -> (FinalizedWithdrawal, WithdrawalId, FallbackId) {
    let fallback_nonce = NonZeroU64::new(fallback_nonce).unwrap();
    let identity = UserWithdrawalIdentity::new(
        Address::repeat_byte(0x70 + withdrawal_index as u8),
        B256::repeat_byte(0x80 + withdrawal_index as u8),
        fallback_nonce,
    )
    .unwrap();
    let request = UserWithdrawalRequest::new(
        INITIAL_TOKEN,
        Address::repeat_byte(0x90 + withdrawal_index as u8),
        amount,
        B256::repeat_byte(0xa0 + withdrawal_index as u8),
        0,
        Bytes::new(),
    )
    .unwrap();
    let finalized = UserPendingWithdrawal::new(identity, request, Bytes::new())
        .unwrap()
        .finalize(Bytes::new())
        .unwrap();
    let withdrawal = WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index,
    };
    let fallback = FallbackId {
        zone_id: ZONE_ID,
        fallback_nonce,
    };
    (finalized, withdrawal, fallback)
}

fn seed_submitted_batch(
    model: &mut ModelState,
    finalized: FinalizedWithdrawal,
    wire: &ZonePortal::Withdrawal,
) {
    let members = BatchMembers::from_withdrawals(0, &[finalized.preimage().clone()]).unwrap();
    assert_eq!(
        members.withdrawal_queue_hash(),
        independent_withdrawal_queue_hash(std::slice::from_ref(wire))
    );
    let finalized_batch = FinalizedBatchState::new(boundary(), members);
    let submitted = SubmittedBatchState::new(
        finalized_batch,
        PortalQueueId::new(portal(), U256::ZERO).unwrap(),
    )
    .unwrap();
    model.seed_batch_for_test(batch_id(), BatchOwner::Submitted(submitted));
    model.set_last_batch_for_test(ZoneLastBatch::for_test(
        independent_withdrawal_queue_hash(std::slice::from_ref(wire)),
        1,
    ));
    model.set_portal_withdrawal_queue_for_test(U256::ZERO, U256::ONE);
}
