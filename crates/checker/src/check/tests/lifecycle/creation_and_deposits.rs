use std::num::NonZeroU64;

use alloy_primitives::{Address, B256, Bytes, U256};
use tempo_zone_contracts::{IZoneInbox, IZoneOutbox, ZoneFactory, ZonePortal};

use super::super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    ownership::{BatchId, BatchOwner, DepositId, DepositOwner, WithdrawalId, WithdrawalOwner},
    state::{ModelState, PortalDepositCursor, TokenPhase},
};

#[tokio::test]
async fn creation_initial_enable_and_empty_finalization_cross_the_complete_pipeline() {
    let imported = imported_header(0);
    let enabled = IZoneInbox::EnabledToken {
        token: INITIAL_TOKEN,
        name: "Initial Token".into(),
        symbol: "INIT".into(),
        currency: "USD".into(),
    };
    let l1 = vec![l1_transaction(
        1,
        None,
        vec![
            portal_event(ZonePortal::TokenEnabled {
                token: INITIAL_TOKEN,
                name: enabled.name.clone(),
                symbol: enabled.symbol.clone(),
                currency: enabled.currency.clone(),
            }),
            factory_event(ZoneFactory::ZoneCreated {
                zoneId: ZONE_ID,
                portal: portal(),
                initialToken: INITIAL_TOKEN,
                accessMode: false,
                gatewayMode: false,
                admin: Address::repeat_byte(0xa1),
                sequencers: vec![Address::repeat_byte(0xa2)],
                threshold: 1,
                verifier: Address::repeat_byte(0xa3),
            }),
        ],
    )];
    let l2 = zone_observation(
        &imported,
        Vec::new(),
        vec![enabled.clone()],
        advance_logs(
            &imported,
            vec![zone_log(
                crate::model::constants::ZONE_INBOX_ADDRESS,
                IZoneInbox::TokenEnabled {
                    token: enabled.token,
                    name: enabled.name,
                    symbol: enabled.symbol,
                    currency: enabled.currency,
                },
            )],
            B256::ZERO,
            0,
        ),
        Vec::new(),
        Some(ZoneFinalization {
            encrypted_senders: Vec::new(),
            event: zone_log(
                crate::model::constants::ZONE_OUTBOX_ADDRESS,
                IZoneOutbox::BatchFinalized {
                    withdrawalQueueHash: B256::ZERO,
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
        withdrawal_hash: B256::ZERO,
        withdrawal_batch_index: 1,
        supplies: vec![(INITIAL_TOKEN, U256::ZERO)],
    };
    let checker = run_valid_block(
        ModelState::awaiting_creation(identity(INITIAL_TOKEN)),
        &imported,
        l1,
        &l2,
        &[U256::ZERO],
        exact,
        true,
    )
    .await;

    let model = checker.model();
    assert_eq!(
        model.portal().created().unwrap().identity(),
        identity(INITIAL_TOKEN)
    );
    assert_eq!(
        model.token(INITIAL_TOKEN).unwrap().phase(),
        TokenPhase::ZoneEnabled
    );
    let batch = BatchId {
        zone_id: ZONE_ID,
        withdrawal_batch_index: std::num::NonZeroU64::new(1).unwrap(),
    };
    assert!(matches!(model.batch(batch), Some(BatchOwner::Finalized(_))));
}

#[tokio::test]
async fn later_portal_and_zone_enablement_preserves_the_pending_to_enabled_phase_boundary() {
    let imported = imported_header(0);
    let model = created_model(TokenAccounting::ZERO);
    let enabled = IZoneInbox::EnabledToken {
        token: SECOND_TOKEN,
        name: "Second Token".into(),
        symbol: "SECOND".into(),
        currency: "USD".into(),
    };
    let l1 = vec![l1_transaction(
        1,
        None,
        vec![portal_event(ZonePortal::TokenEnabled {
            token: SECOND_TOKEN,
            name: enabled.name.clone(),
            symbol: enabled.symbol.clone(),
            currency: enabled.currency.clone(),
        })],
    )];
    let l2 = zone_observation(
        &imported,
        Vec::new(),
        vec![enabled.clone()],
        advance_logs(
            &imported,
            vec![zone_log(
                crate::model::constants::ZONE_INBOX_ADDRESS,
                IZoneInbox::TokenEnabled {
                    token: enabled.token,
                    name: enabled.name,
                    symbol: enabled.symbol,
                    currency: enabled.currency,
                },
            )],
            B256::ZERO,
            0,
        ),
        Vec::new(),
        None,
    );
    let exact = ExactPostState::from_model(&model)
        .with_supply(INITIAL_TOKEN, U256::ZERO)
        .with_supply(SECOND_TOKEN, U256::ZERO);
    let checker = run_valid_block(
        model,
        &imported,
        l1,
        &l2,
        &[U256::ZERO, U256::ZERO],
        exact,
        false,
    )
    .await;

    assert_eq!(
        checker.model().token(SECOND_TOKEN).unwrap().phase(),
        TokenPhase::ZoneEnabled
    );
}

#[derive(Debug, Clone, Copy)]
enum OrdinaryOutcome {
    Minted,
    Failed,
}

#[tokio::test]
async fn ordinary_mint_and_failed_deposit_withdrawal_are_end_to_end_branches() {
    for outcome in [OrdinaryOutcome::Minted, OrdinaryOutcome::Failed] {
        let imported = imported_header(0);
        let model = created_model(TokenAccounting::ZERO);
        let deposit = ordinary(INITIAL_TOKEN, 0x41, 700);
        let queue_hash = independent_ordinary_queue_hash(&deposit, B256::ZERO);
        let l1 = vec![l1_transaction(
            1,
            None,
            vec![portal_event(ZonePortal::DepositMade {
                newCurrentDepositQueueHash: queue_hash,
                sender: deposit.sender,
                token: deposit.token,
                netAmount: deposit.amount,
                fee: 0,
                keyIndex: deposit.keyIndex,
                ephemeralPubkeyX: deposit.encrypted.ephemeralPubkeyX,
                ephemeralPubkeyYParity: deposit.encrypted.ephemeralPubkeyYParity,
                ciphertext: deposit.encrypted.ciphertext.clone(),
                nonce: deposit.encrypted.nonce,
                tag: deposit.encrypted.tag,
                tempoRefundRecipient: deposit.tempoRefundRecipient,
                depositNumber: 1,
            })],
        )];

        let middle = match outcome {
            OrdinaryOutcome::Minted => vec![zone_log(
                crate::model::constants::ZONE_INBOX_ADDRESS,
                IZoneInbox::DepositProcessed {
                    depositHash: queue_hash,
                    sender: deposit.sender,
                    to: Address::repeat_byte(0x61),
                    token: deposit.token,
                    amount: deposit.amount,
                    memo: B256::repeat_byte(0x62),
                },
            )],
            OrdinaryOutcome::Failed => vec![
                zone_log(
                    crate::model::constants::ZONE_OUTBOX_ADDRESS,
                    IZoneOutbox::WithdrawalRequested {
                        withdrawalIndex: 0,
                        sender: Address::ZERO,
                        token: deposit.token,
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
                        token: deposit.token,
                        amount: deposit.amount,
                    },
                ),
            ],
        };
        let l2 = zone_observation(
            &imported,
            vec![queued_ordinary(&deposit)],
            Vec::new(),
            advance_logs(&imported, middle, queue_hash, 1),
            Vec::new(),
            None,
        );
        let expected_supply = match outcome {
            OrdinaryOutcome::Minted => U256::from(deposit.amount),
            OrdinaryOutcome::Failed => U256::ZERO,
        };
        let exact = ExactPostState {
            tempo_hash: None,
            tempo_number: None,
            processed_hash: queue_hash,
            processed_number: 1,
            withdrawal_hash: B256::ZERO,
            withdrawal_batch_index: 0,
            supplies: vec![(INITIAL_TOKEN, expected_supply)],
        };
        let checker = run_valid_block(
            model,
            &imported,
            l1,
            &l2,
            &[U256::from(deposit.amount)],
            exact,
            false,
        )
        .await;

        let accounting = checker.model().token(INITIAL_TOKEN).unwrap().accounting();
        match outcome {
            OrdinaryOutcome::Minted => assert_eq!(
                accounting,
                TokenAccounting {
                    supply: U256::from(700),
                    deposit_liability: U256::ZERO,
                    withdrawal_liability: U256::ZERO,
                }
            ),
            OrdinaryOutcome::Failed => {
                assert_eq!(
                    accounting,
                    TokenAccounting {
                        supply: U256::ZERO,
                        deposit_liability: U256::from(700),
                        withdrawal_liability: U256::ZERO,
                    }
                );
                assert!(matches!(
                    checker.model().withdrawal(WithdrawalId {
                        zone_id: ZONE_ID,
                        withdrawal_index: 0,
                    }),
                    Some(WithdrawalOwner::Pending(_))
                ));
            }
        }
    }
}

#[tokio::test]
async fn deposit_import_empty_partial_and_full_prefixes_preserve_the_exact_open_suffix() {
    let first = ordinary(INITIAL_TOKEN, 0x51, 10);
    let second = ordinary(INITIAL_TOKEN, 0x52, 20);
    let first_hash = independent_ordinary_queue_hash(&first, B256::ZERO);
    let second_hash = independent_ordinary_queue_hash(&second, first_hash);
    let deposits = [&first, &second];
    let hashes = [first_hash, second_hash];

    for prefix_len in 0..=deposits.len() {
        let imported = imported_header(0);
        let mut model = created_model(TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(30),
            withdrawal_liability: U256::ZERO,
        });
        for (number, deposit) in [(1, &first), (2, &second)] {
            model.seed_pending_deposit_for_test(
                DepositId {
                    portal: portal(),
                    deposit_number: NonZeroU64::new(number).unwrap(),
                },
                DepositOwner::PendingOrdinary {
                    preimage: model_ordinary(deposit),
                },
            );
        }
        model.set_portal_deposit_cursor_for_test(PortalDepositCursor::new(second_hash, 2));

        let processed_hash = prefix_len
            .checked_sub(1)
            .map_or(B256::ZERO, |index| hashes[index]);
        let processed_amount = deposits[..prefix_len]
            .iter()
            .map(|deposit| deposit.amount)
            .sum::<u128>();
        let queued = deposits[..prefix_len]
            .iter()
            .map(|deposit| queued_ordinary(deposit))
            .collect();
        let outcomes = deposits[..prefix_len]
            .iter()
            .enumerate()
            .map(|(index, deposit)| {
                zone_log(
                    crate::model::constants::ZONE_INBOX_ADDRESS,
                    IZoneInbox::DepositProcessed {
                        depositHash: hashes[index],
                        sender: deposit.sender,
                        to: Address::repeat_byte(0x71 + index as u8),
                        token: deposit.token,
                        amount: deposit.amount,
                        memo: B256::repeat_byte(0x73 + index as u8),
                    },
                )
            })
            .collect();
        let l2 = zone_observation(
            &imported,
            queued,
            Vec::new(),
            advance_logs(&imported, outcomes, processed_hash, prefix_len as u64),
            Vec::new(),
            None,
        );
        let exact = ExactPostState {
            tempo_hash: None,
            tempo_number: None,
            processed_hash,
            processed_number: prefix_len as u64,
            withdrawal_hash: B256::ZERO,
            withdrawal_batch_index: 0,
            supplies: vec![(INITIAL_TOKEN, U256::from(processed_amount))],
        };
        let checker = run_valid_block(
            model,
            &imported,
            Vec::new(),
            &l2,
            &[U256::from(30)],
            exact,
            false,
        )
        .await;

        for number in 1..=2 {
            let owner = checker.model().pending_deposit(DepositId {
                portal: portal(),
                deposit_number: NonZeroU64::new(number).unwrap(),
            });
            assert_eq!(owner.is_none(), number <= prefix_len as u64);
        }
        assert_eq!(
            checker.model().token(INITIAL_TOKEN).unwrap().accounting(),
            TokenAccounting {
                supply: U256::from(processed_amount),
                deposit_liability: U256::from(30 - processed_amount),
                withdrawal_liability: U256::ZERO,
            }
        );
    }
}
