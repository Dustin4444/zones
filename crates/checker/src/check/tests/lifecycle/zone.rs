use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256, Bytes, U256};
use tempo_zone_contracts::{IZoneInbox, IZoneOutbox};

use super::super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    encoding::{UserWithdrawalIdentity, UserWithdrawalRequest, WithdrawalBounceBackDeposit},
    ownership::{
        BatchId, BatchOwner, DepositId, DepositOwner, FailedDepositPendingWithdrawal, FallbackId,
        FallbackOwner, PendingWithdrawal, UserPendingWithdrawal, WithdrawalId, WithdrawalOwner,
    },
    state::{PortalDepositCursor, TokenPhase, TokenState, ZoneLastBatch},
};

#[tokio::test]
async fn ordered_zone_config_updates_drive_each_user_withdrawal_and_preserve_the_counter() {
    let imported = imported_header(0);
    let initial_supply = U256::from(1_000_000_u64);
    let model = created_model(TokenAccounting {
        supply: initial_supply,
        deposit_liability: U256::ZERO,
        withdrawal_liability: U256::ZERO,
    });
    let sender_one = Address::repeat_byte(0x51);
    let sender_two = Address::repeat_byte(0x52);
    let sender_three = Address::repeat_byte(0x53);
    let users = vec![
        ZoneUserTransaction {
            sender: sender_one,
            logs: vec![
                zone_log(
                    crate::model::constants::ZONE_OUTBOX_ADDRESS,
                    IZoneOutbox::TempoGasRateUpdated { tempoGasRate: 2 },
                ),
                zone_log(
                    crate::model::constants::ZONE_OUTBOX_ADDRESS,
                    IZoneOutbox::MaxWithdrawalsPerBlockUpdated {
                        maxWithdrawalsPerBlock: 1,
                    },
                ),
                withdrawal_event(0, sender_one, 10, 100_000, 1),
            ],
        },
        ZoneUserTransaction {
            sender: sender_two,
            logs: vec![
                zone_log(
                    crate::model::constants::ZONE_OUTBOX_ADDRESS,
                    IZoneOutbox::MaxWithdrawalsPerBlockUpdated {
                        maxWithdrawalsPerBlock: 0,
                    },
                ),
                withdrawal_event(1, sender_two, 20, 100_000, 2),
            ],
        },
        ZoneUserTransaction {
            sender: sender_three,
            logs: vec![
                zone_log(
                    crate::model::constants::ZONE_OUTBOX_ADDRESS,
                    IZoneOutbox::MaxWithdrawalsPerBlockUpdated {
                        maxWithdrawalsPerBlock: 2,
                    },
                ),
                zone_log(
                    crate::model::constants::ZONE_OUTBOX_ADDRESS,
                    IZoneOutbox::TempoGasRateUpdated { tempoGasRate: 3 },
                ),
                withdrawal_event(2, sender_three, 30, 150_000, 3),
                zone_log(
                    crate::model::constants::ZONE_OUTBOX_ADDRESS,
                    IZoneOutbox::TempoGasRateUpdated { tempoGasRate: 5 },
                ),
            ],
        },
    ];
    let l2 = zone_observation(
        &imported,
        Vec::new(),
        Vec::new(),
        advance_logs(&imported, Vec::new(), B256::ZERO, 0),
        users,
        None,
    );
    let expected_supply = U256::from(649_940_u64);
    let exact = ExactPostState::from_model(&model).with_supply(INITIAL_TOKEN, expected_supply);
    let checker = run_valid_block(
        model,
        &imported,
        Vec::new(),
        &l2,
        &[initial_supply],
        exact,
        false,
    )
    .await;

    let state = checker.model();
    assert_eq!(state.zone().config().tempo_gas_rate(), 5);
    assert_eq!(state.zone().config().max_withdrawals_per_block(), 2);
    assert_eq!(state.zone().next_withdrawal_index(), 3);
    assert_eq!(state.zone().last_fallback_nonce(), 3);
    assert_eq!(
        state.token(INITIAL_TOKEN).unwrap().accounting(),
        TokenAccounting {
            supply: expected_supply,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(60),
        }
    );
}

#[tokio::test]
async fn failed_deposit_withdrawal_is_exempt_from_the_active_user_withdrawal_cap() {
    let imported = imported_header(0);
    let deposit = ordinary(INITIAL_TOKEN, 0x59, 50);
    let queue_hash = independent_ordinary_queue_hash(&deposit, B256::ZERO);
    let mut model = created_model(TokenAccounting {
        supply: U256::from(1_000),
        deposit_liability: U256::from(deposit.amount),
        withdrawal_liability: U256::ZERO,
    });
    model.seed_pending_deposit_for_test(
        DepositId {
            portal: portal(),
            deposit_number: NonZeroU64::new(1).unwrap(),
        },
        DepositOwner::PendingOrdinary {
            preimage: model_ordinary(&deposit),
        },
    );
    model.set_portal_deposit_cursor_for_test(PortalDepositCursor::new(queue_hash, 1));
    model.set_zone_config_for_test(0, 1);
    let sender = Address::repeat_byte(0x5a);
    let l2 = zone_observation(
        &imported,
        vec![queued_ordinary(&deposit)],
        Vec::new(),
        advance_logs(
            &imported,
            vec![
                zone_log(
                    crate::model::constants::ZONE_OUTBOX_ADDRESS,
                    IZoneOutbox::WithdrawalRequested {
                        withdrawalIndex: 0,
                        sender: Address::ZERO,
                        token: INITIAL_TOKEN,
                        to: deposit.tempoRefundRecipient,
                        amount: deposit.amount,
                        fee: 0,
                        memo: B256::ZERO,
                        gasLimit: 0,
                        fallbackNonce: 0,
                        data: Bytes::new(),
                        revealTo: Bytes::new(),
                    },
                ),
                zone_log(
                    crate::model::constants::ZONE_INBOX_ADDRESS,
                    IZoneInbox::DepositFailed {
                        depositHash: queue_hash,
                        sender: deposit.sender,
                        token: INITIAL_TOKEN,
                        amount: deposit.amount,
                    },
                ),
            ],
            queue_hash,
            1,
        ),
        vec![ZoneUserTransaction {
            sender,
            logs: vec![withdrawal_event(1, sender, 10, 0, 1)],
        }],
        None,
    );
    let exact = ExactPostState {
        tempo_hash: None,
        tempo_number: None,
        processed_hash: queue_hash,
        processed_number: 1,
        withdrawal_hash: B256::ZERO,
        withdrawal_batch_index: 0,
        supplies: vec![(INITIAL_TOKEN, U256::from(990))],
    };
    let checker = run_valid_block(
        model,
        &imported,
        Vec::new(),
        &l2,
        &[U256::from(1_050)],
        exact,
        false,
    )
    .await;

    assert_eq!(checker.model().zone().next_withdrawal_index(), 2);
    assert!(
        checker
            .model()
            .withdrawal(WithdrawalId {
                zone_id: ZONE_ID,
                withdrawal_index: 0,
            })
            .is_some()
    );
    assert!(
        checker
            .model()
            .withdrawal(WithdrawalId {
                zone_id: ZONE_ID,
                withdrawal_index: 1,
            })
            .is_some()
    );
}

#[tokio::test]
async fn nonempty_finalization_authenticates_the_calldata_and_exact_last_batch_commitment() {
    let imported = imported_header(0);
    let sender = Address::repeat_byte(0x61);
    let transaction_hash = B256::repeat_byte(0x62);
    let fallback_nonce = NonZeroU64::new(1).unwrap();
    let amount = 30_u128;
    let identity = UserWithdrawalIdentity::new(sender, transaction_hash, fallback_nonce).unwrap();
    let request = UserWithdrawalRequest::new(
        INITIAL_TOKEN,
        Address::repeat_byte(0x63),
        amount,
        B256::repeat_byte(0x64),
        0,
        Bytes::new(),
    )
    .unwrap();
    let pending = UserPendingWithdrawal::new(identity, request.clone(), Bytes::new()).unwrap();
    let finalized = pending.clone().finalize(Bytes::new()).unwrap();
    let portal_preimage = portal_withdrawal(&finalized);
    let queue_hash = independent_withdrawal_queue_hash(std::slice::from_ref(&portal_preimage));
    let withdrawal = WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index: 0,
    };
    let mut model = created_model(TokenAccounting {
        supply: U256::ZERO,
        deposit_liability: U256::ZERO,
        withdrawal_liability: U256::from(amount),
    });
    model.seed_withdrawal_for_test(
        withdrawal,
        WithdrawalOwner::Pending(PendingWithdrawal::User(pending)),
    );
    model.seed_fallback_owner_for_test(
        FallbackId {
            zone_id: ZONE_ID,
            fallback_nonce,
        },
        FallbackOwner::Held {
            withdrawal,
            token: INITIAL_TOKEN,
            amount: NonZeroU128::new(amount).unwrap(),
        },
    );
    model.set_next_withdrawal_index_for_test(1);
    model.set_last_fallback_nonce_for_test(1);

    let l2 = zone_observation(
        &imported,
        Vec::new(),
        Vec::new(),
        advance_logs(&imported, Vec::new(), B256::ZERO, 0),
        Vec::new(),
        Some(ZoneFinalization {
            encrypted_senders: vec![Bytes::new()],
            event: zone_log(
                crate::model::constants::ZONE_OUTBOX_ADDRESS,
                IZoneOutbox::BatchFinalized {
                    withdrawalQueueHash: queue_hash,
                    withdrawalBatchIndex: 1,
                },
            ),
        }),
    );
    let exact = ExactPostState {
        tempo_hash: None,
        tempo_number: None,
        processed_hash: B256::ZERO,
        processed_number: 0,
        withdrawal_hash: queue_hash,
        withdrawal_batch_index: 1,
        supplies: vec![(INITIAL_TOKEN, U256::ZERO)],
    };
    let checker = run_valid_block(
        model,
        &imported,
        Vec::new(),
        &l2,
        &[U256::from(amount)],
        exact,
        false,
    )
    .await;

    assert!(matches!(
        checker.model().withdrawal(withdrawal),
        Some(WithdrawalOwner::Finalized(_))
    ));
    assert!(
        checker
            .model()
            .batch(BatchId {
                zone_id: ZONE_ID,
                withdrawal_batch_index: NonZeroU64::new(1).unwrap(),
            })
            .is_some()
    );
    assert_eq!(
        checker.model().zone().last_batch(),
        ZoneLastBatch::for_test(queue_hash, 1)
    );
}

#[tokio::test]
async fn one_finalized_batch_preserves_mixed_token_and_mixed_origin_ownership() {
    let imported = imported_header(0);
    let user_amount = NonZeroU128::new(10).unwrap();
    let failed_amount = 20_u128;
    let fallback_nonce = NonZeroU64::new(1).unwrap();
    let user_identity = UserWithdrawalIdentity::new(
        Address::repeat_byte(0x65),
        B256::repeat_byte(0x66),
        fallback_nonce,
    )
    .unwrap();
    let user_request = UserWithdrawalRequest::new(
        INITIAL_TOKEN,
        Address::repeat_byte(0x67),
        user_amount.get(),
        B256::repeat_byte(0x68),
        0,
        Bytes::new(),
    )
    .unwrap();
    let user_pending =
        UserPendingWithdrawal::new(user_identity, user_request, Bytes::new()).unwrap();
    let user_finalized = user_pending.clone().finalize(Bytes::new()).unwrap();
    let failed_deposit = DepositId {
        portal: portal(),
        deposit_number: NonZeroU64::new(1).unwrap(),
    };
    let failed_wire = ordinary(SECOND_TOKEN, 0x69, failed_amount);
    let failed_pending = FailedDepositPendingWithdrawal::from_failed_deposit(
        failed_deposit,
        model_ordinary(&failed_wire),
    );
    let failed_finalized = failed_pending.clone().finalize();
    let queue_hash = independent_withdrawal_queue_hash(&[
        portal_withdrawal(&user_finalized),
        portal_withdrawal(&failed_finalized),
    ]);

    let user_withdrawal = WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index: 0,
    };
    let failed_withdrawal = WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index: 1,
    };
    let mut model = created_model(TokenAccounting {
        supply: U256::ZERO,
        deposit_liability: U256::ZERO,
        withdrawal_liability: U256::from(user_amount.get()),
    });
    model.seed_token_for_test(
        SECOND_TOKEN,
        TokenState::for_test(
            TokenPhase::ZoneEnabled,
            TokenAccounting {
                supply: U256::ZERO,
                deposit_liability: U256::from(failed_amount),
                withdrawal_liability: U256::ZERO,
            },
        ),
    );
    model.seed_withdrawal_for_test(
        user_withdrawal,
        WithdrawalOwner::Pending(PendingWithdrawal::User(user_pending)),
    );
    model.seed_withdrawal_for_test(
        failed_withdrawal,
        WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(failed_pending)),
    );
    model.seed_fallback_owner_for_test(
        FallbackId {
            zone_id: ZONE_ID,
            fallback_nonce,
        },
        FallbackOwner::Held {
            withdrawal: user_withdrawal,
            token: INITIAL_TOKEN,
            amount: user_amount,
        },
    );
    model.set_next_withdrawal_index_for_test(2);
    model.set_last_fallback_nonce_for_test(1);

    let l2 = zone_observation(
        &imported,
        Vec::new(),
        Vec::new(),
        advance_logs(&imported, Vec::new(), B256::ZERO, 0),
        Vec::new(),
        Some(ZoneFinalization {
            encrypted_senders: vec![Bytes::new(), Bytes::new()],
            event: zone_log(
                crate::model::constants::ZONE_OUTBOX_ADDRESS,
                IZoneOutbox::BatchFinalized {
                    withdrawalQueueHash: queue_hash,
                    withdrawalBatchIndex: 1,
                },
            ),
        }),
    );
    let exact = ExactPostState {
        tempo_hash: None,
        tempo_number: None,
        processed_hash: B256::ZERO,
        processed_number: 0,
        withdrawal_hash: queue_hash,
        withdrawal_batch_index: 1,
        supplies: vec![(INITIAL_TOKEN, U256::ZERO), (SECOND_TOKEN, U256::ZERO)],
    };
    let checker = run_valid_block(
        model,
        &imported,
        Vec::new(),
        &l2,
        &[U256::from(user_amount.get()), U256::from(failed_amount)],
        exact,
        false,
    )
    .await;

    for withdrawal in [user_withdrawal, failed_withdrawal] {
        assert!(matches!(
            checker.model().withdrawal(withdrawal),
            Some(WithdrawalOwner::Finalized(_))
        ));
    }
    let Some(BatchOwner::Finalized(batch)) = checker.model().batch(BatchId {
        zone_id: ZONE_ID,
        withdrawal_batch_index: NonZeroU64::new(1).unwrap(),
    }) else {
        panic!("mixed batch must remain finalized")
    };
    assert_eq!(batch.members().member_count(), 2);
}

#[derive(Debug, Clone, Copy)]
enum BounceOutcome {
    Minted,
    Pending,
}

#[tokio::test]
async fn withdrawal_bounceback_mint_and_pending_are_checked_as_distinct_zone_branches() {
    for outcome in [BounceOutcome::Minted, BounceOutcome::Pending] {
        let imported = imported_header(0);
        let amount = NonZeroU128::new(80).unwrap();
        let fallback_nonce = NonZeroU64::new(1).unwrap();
        let withdrawal = WithdrawalId {
            zone_id: ZONE_ID,
            withdrawal_index: 9,
        };
        let model_deposit = WithdrawalBounceBackDeposit::new(INITIAL_TOKEN, fallback_nonce, amount);
        let wire_deposit = IZoneInbox::WithdrawalBounceBackDeposit {
            token: INITIAL_TOKEN,
            to: model_deposit.recipient(),
            amount: amount.get(),
        };
        let queue_hash = independent_bounce_queue_hash(&wire_deposit, B256::ZERO);
        let deposit_id = DepositId {
            portal: portal(),
            deposit_number: NonZeroU64::new(1).unwrap(),
        };
        let fallback = FallbackId {
            zone_id: ZONE_ID,
            fallback_nonce,
        };
        let mut model = created_model(TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(amount.get()),
        });
        model.seed_pending_deposit_for_test(
            deposit_id,
            DepositOwner::PendingWithdrawalBounceBack {
                withdrawal,
                preimage: model_deposit,
            },
        );
        model.seed_fallback_owner_for_test(
            fallback,
            FallbackOwner::BounceBackQueued {
                withdrawal,
                token: INITIAL_TOKEN,
                amount,
                deposit: deposit_id,
            },
        );
        model.set_portal_deposit_cursor_for_test(PortalDepositCursor::new(queue_hash, 1));
        let recipient = Address::repeat_byte(match outcome {
            BounceOutcome::Minted => 0x71,
            BounceOutcome::Pending => 0x72,
        });
        let outcome_log = match outcome {
            BounceOutcome::Minted => zone_log(
                crate::model::constants::ZONE_INBOX_ADDRESS,
                IZoneInbox::WithdrawalBounceBackProcessed {
                    zoneFallbackRecipient: recipient,
                    token: INITIAL_TOKEN,
                    amount: amount.get(),
                },
            ),
            BounceOutcome::Pending => zone_log(
                crate::model::constants::ZONE_INBOX_ADDRESS,
                IZoneInbox::WithdrawalBounceBackPending {
                    zoneFallbackRecipient: recipient,
                    token: INITIAL_TOKEN,
                    amount: amount.get(),
                },
            ),
        };
        let l2 = zone_observation(
            &imported,
            vec![queued_bounce(&wire_deposit)],
            Vec::new(),
            advance_logs(&imported, vec![outcome_log], queue_hash, 1),
            Vec::new(),
            None,
        );
        let supply = match outcome {
            BounceOutcome::Minted => U256::from(amount.get()),
            BounceOutcome::Pending => U256::ZERO,
        };
        let exact = ExactPostState {
            tempo_hash: None,
            tempo_number: None,
            processed_hash: queue_hash,
            processed_number: 1,
            withdrawal_hash: B256::ZERO,
            withdrawal_batch_index: 0,
            supplies: vec![(INITIAL_TOKEN, supply)],
        };
        let checker = run_valid_block(
            model,
            &imported,
            Vec::new(),
            &l2,
            &[U256::from(amount.get())],
            exact,
            false,
        )
        .await;

        assert!(checker.model().fallback_owner(fallback).is_none());
        assert!(checker.model().pending_deposit(deposit_id).is_none());
        match outcome {
            BounceOutcome::Minted => assert_eq!(
                checker.model().token(INITIAL_TOKEN).unwrap().accounting(),
                TokenAccounting {
                    supply,
                    deposit_liability: U256::ZERO,
                    withdrawal_liability: U256::ZERO,
                }
            ),
            BounceOutcome::Pending => {
                assert_eq!(
                    checker
                        .model()
                        .inbox_refund_total(crate::model::ownership::RefundAccount {
                            token: INITIAL_TOKEN,
                            recipient,
                        }),
                    amount.get()
                );
            }
        }
    }
}

fn withdrawal_event(
    index: u64,
    sender: Address,
    amount: u128,
    fee: u128,
    fallback_nonce: u64,
) -> alloy_primitives::Log {
    zone_log(
        crate::model::constants::ZONE_OUTBOX_ADDRESS,
        IZoneOutbox::WithdrawalRequested {
            withdrawalIndex: index,
            sender,
            token: INITIAL_TOKEN,
            to: Address::repeat_byte(0x80 + index as u8),
            amount,
            fee,
            memo: B256::repeat_byte(0x90 + index as u8),
            gasLimit: 0,
            fallbackNonce: fallback_nonce,
            data: Bytes::new(),
            revealTo: Bytes::new(),
        },
    )
}
