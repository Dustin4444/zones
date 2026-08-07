//! Milestone-3 parity fixtures. The two sides receive separately constructed
//! values so a shared transition or expected-value constructor cannot make the
//! comparison pass vacuously.

use alloy_primitives::{Address, B256, Bytes, U256};
use std::num::NonZeroU64;
use zone_checker_kernel as compact;

use super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    encoding::{DepositQueueMember, OrdinaryDeposit, withdrawal_queue_hash},
    input::{
        AuthenticatedDepositOutcome, AuthenticatedWithdrawalOutcome, ImportedTempoBlockInput,
        ZoneDepositPrefixInput, ZoneOperation,
    },
    output::{ExpectedDepositOutcome, ExpectedImportedTempoOperation, ExpectedProcessedWithdrawal},
    state::{ModelState, TokenPhase},
};

fn compact_identity(token: Address) -> compact::PortalIdentity {
    compact::PortalIdentity {
        portal: portal(),
        zone_id: ZONE_ID,
        initial_token: token,
    }
}

fn compact_enable(token: Address, label: &str) -> compact::TokenEnable {
    compact::TokenEnable {
        token,
        name: format!("{label} Token"),
        symbol: label.into(),
        currency: "USD".into(),
    }
}

fn compact_deposit(value: &OrdinaryDeposit) -> compact::OrdinaryDeposit {
    compact::OrdinaryDeposit {
        token: value.token(),
        sender: value.sender(),
        amount: value.amount(),
        tempo_refund_recipient: value.tempo_refund_recipient(),
        key_index: value.key_index(),
        encrypted: compact::DepositPayload {
            ephemeral_pubkey_x: value.encrypted().ephemeral_pubkey_x(),
            ephemeral_pubkey_y_parity: value.encrypted().ephemeral_pubkey_y_parity(),
            ciphertext: value.encrypted().ciphertext(),
            nonce: value.encrypted().nonce(),
            tag: value.encrypted().tag(),
        },
    }
}

fn compact_token(state: &compact::State, token: Address) -> compact::TokenState {
    match state.rows().get(&compact::StateKey::Token(token)).unwrap() {
        compact::StateValue::Token(value) => *value,
        _ => unreachable!(),
    }
}

fn assert_snapshot_eq(old: &ModelState, new: &compact::State, token: Address) {
    let old_portal = old.portal().created().unwrap();
    let (new_portal, new_settlement, new_bounceback_gas) =
        match new.rows().get(&compact::StateKey::Portal).unwrap() {
            compact::StateValue::Portal(compact::PortalState::Created {
                identity,
                deposit,
                settlement,
                bounceback_gas,
            }) => ((*identity, *deposit), *settlement, *bounceback_gas),
            _ => panic!("compact Portal was not created"),
        };
    assert_eq!(old_portal.identity().portal(), new_portal.0.portal);
    assert_eq!(old_portal.identity().zone_id(), new_portal.0.zone_id);
    assert_eq!(old_portal.deposit_cursor().hash(), new_portal.1.hash);
    assert_eq!(old_portal.deposit_cursor().number(), new_portal.1.number);
    assert_eq!(old_portal.config().bounceback_gas(), new_bounceback_gas);
    let old_settlement = old_portal.settlement();
    assert_eq!(
        old_settlement.withdrawal_batch_index(),
        new_settlement.batch_index
    );
    assert_eq!(old_settlement.block_hash(), new_settlement.block_hash);
    assert_eq!(
        old_settlement.last_synced_tempo_block_number(),
        new_settlement.tempo_block
    );
    assert_eq!(
        old_settlement.last_submitted_deposit_cursor().hash,
        new_settlement.submitted_deposit.hash
    );
    assert_eq!(
        old_settlement.last_submitted_deposit_cursor().number,
        new_settlement.submitted_deposit.number
    );
    assert_eq!(old_settlement.zone_height(), new_settlement.zone_height);
    assert_eq!(
        old_settlement.withdrawal_queue_head(),
        new_settlement.queue_head
    );
    assert_eq!(
        old_settlement.withdrawal_queue_tail(),
        new_settlement.queue_tail
    );

    let old_token = old.token(token).unwrap();
    let new_token = compact_token(new, token);
    assert_eq!(
        old_token.phase() == TokenPhase::ZoneEnabled,
        new_token.phase == compact::TokenPhase::ZoneEnabled
    );
    assert_eq!(old_token.accounting().supply, new_token.accounting.supply);
    assert_eq!(
        old_token.accounting().deposit_liability,
        new_token.accounting.deposits
    );
    assert_eq!(
        old_token.accounting().withdrawal_liability,
        new_token.accounting.withdrawals
    );

    let compact::StateValue::Zone(new_zone) = &new.rows()[&compact::StateKey::Zone] else {
        unreachable!()
    };
    assert_eq!(
        old.zone().processed_deposit_cursor().hash(),
        new_zone.processed_deposit.hash
    );
    assert_eq!(
        old.zone().processed_deposit_cursor().number(),
        new_zone.processed_deposit.number
    );
    assert_eq!(
        old.zone().next_withdrawal_index(),
        new_zone.next_withdrawal_index
    );
    assert_eq!(
        old.zone().last_batch.withdrawal_queue_hash(),
        new_zone.withdrawal_queue_hash
    );
    assert_eq!(
        old.zone().last_batch.withdrawal_batch_index(),
        new_zone.withdrawal_batch_index
    );
    assert_eq!(old.zone().config.tempo_gas_rate(), new_zone.tempo_gas_rate);
    assert_eq!(
        old.zone().config.max_withdrawals_per_block(),
        new_zone.max_withdrawals_per_block
    );
    assert_eq!(
        old.zone().last_fallback_nonce(),
        new_zone.last_fallback_nonce
    );
    assert_eq!(
        old.pending_deposits().len(),
        new.rows()
            .keys()
            .filter(|key| matches!(key, compact::StateKey::Deposit(_)))
            .count()
    );
    assert_eq!(
        old.withdrawals().len(),
        new.rows()
            .keys()
            .filter(|key| matches!(key, compact::StateKey::Withdrawal(_)))
            .count()
    );
    assert_eq!(
        old.batches().len(),
        new.rows()
            .keys()
            .filter(|key| matches!(key, compact::StateKey::Batch(_)))
            .count()
    );
    assert_eq!(
        old.fallback_owners().len(),
        new.rows()
            .keys()
            .filter(|key| matches!(key, compact::StateKey::Fallback(_)))
            .count()
    );
}

fn compact_commit(
    state: &mut compact::State,
    imported: compact::ImportedFacts,
    zone: compact::ZoneFacts,
) {
    let candidate =
        compact::apply_zone(compact::apply_imported(state, &imported).unwrap(), &zone).unwrap();
    state.apply(&candidate.delta).unwrap();
}

fn compact_funded(token: Address, supply: u128) -> compact::State {
    let mut state = compact::State::awaiting(compact_identity(token));
    compact_commit(
        &mut state,
        compact::ImportedFacts {
            operations: vec![compact::ImportedOperation::Create {
                identity: compact_identity(token),
                initial_token: compact_enable(token, "INIT"),
            }],
            ..compact::ImportedFacts::default()
        },
        compact::ZoneFacts {
            enabled_tokens: vec![compact_enable(token, "INIT")],
            ..compact::ZoneFacts::default()
        },
    );
    let mut rows = state.rows().clone();
    let compact::StateValue::Token(mut token_state) = rows[&compact::StateKey::Token(token)] else {
        unreachable!()
    };
    token_state.accounting.supply = U256::from(supply);
    rows.insert(
        compact::StateKey::Token(token),
        compact::StateValue::Token(token_state),
    );
    compact::State::from_rows(rows).unwrap()
}

fn compact_finalize_and_submit(state: &mut compact::State, count: usize) {
    compact_commit(
        state,
        compact::ImportedFacts {
            block_number: 2,
            ..Default::default()
        },
        compact::ZoneFacts {
            block_hash: B256::repeat_byte(2),
            block_number: 2,
            finalization: Some(compact::Finalization {
                block_number: 2,
                declared_count: count,
                encrypted_senders: vec![Bytes::new(); count],
            }),
            ..Default::default()
        },
    );
    let id = compact::BatchId {
        zone_id: ZONE_ID,
        index: NonZeroU64::MIN,
    };
    let compact::StateValue::Batch(compact::BatchState::Finalized {
        boundary,
        queue_hash,
        ..
    }) = state.rows()[&compact::StateKey::Batch(id)].clone()
    else {
        panic!("compact batch was not finalized")
    };
    compact_commit(
        state,
        compact::ImportedFacts {
            operations: vec![compact::ImportedOperation::SubmitBatch(
                compact::BatchSubmission {
                    tempo_block: boundary.tempo_block,
                    previous_block: boundary.first_parent,
                    next_block: boundary.final_block,
                    previous_deposit: boundary.first_deposit,
                    next_deposit: boundary.final_deposit,
                    withdrawal_queue_hash: queue_hash,
                    next_zone_height: U256::from(boundary.zone_height),
                },
            )],
            ..Default::default()
        },
        Default::default(),
    );
}

fn compact_withdrawal(state: &compact::State, index: u64) -> compact::Withdrawal {
    let compact::StateValue::Withdrawal(compact::WithdrawalOwner::Finalized { data, .. }) = &state
        .rows()[&compact::StateKey::Withdrawal(compact::WithdrawalId {
        zone_id: ZONE_ID,
        index,
    })] else {
        panic!("compact withdrawal was not finalized")
    };
    data.clone()
}

#[test]
fn creation_append_and_deposit_prefix_match_legacy_semantics() {
    let token = token(0x31);
    let deposits = [ordinary(token, 0x40, 10), ordinary(token, 0x41, 20)];

    let mut old = ModelState::awaiting_creation(identity(token));
    let old_creation = ImportedTempoBlockInput::new(0, U256::ZERO, vec![creation_operation(token)]);
    let old_initial_zone = ZoneDepositPrefixInput::new(vec![enable(token, "INIT")], vec![], vec![]);
    commit(&mut old, &old_creation, &old_initial_zone).unwrap();

    let mut new = compact::State::awaiting(compact_identity(token));
    let candidate = compact::apply_zone(
        compact::apply_imported(
            &new,
            &compact::ImportedFacts {
                operations: vec![compact::ImportedOperation::Create {
                    identity: compact_identity(token),
                    initial_token: compact_enable(token, "INIT"),
                }],
                ..compact::ImportedFacts::default()
            },
        )
        .unwrap(),
        &compact::ZoneFacts {
            enabled_tokens: vec![compact_enable(token, "INIT")],
            ..compact::ZoneFacts::default()
        },
    )
    .unwrap();
    new.apply(&candidate.delta).unwrap();
    assert_snapshot_eq(&old, &new, token);

    let old_imported =
        ImportedTempoBlockInput::new(1, U256::ZERO, ordinary_append_operations(&deposits));
    let old_expected = commit(&mut old, &old_imported, &empty_zone()).unwrap();
    let compact_deposits = deposits.iter().map(compact_deposit).collect::<Vec<_>>();
    let candidate = compact::apply_zone(
        compact::apply_imported(
            &new,
            &compact::ImportedFacts {
                operations: compact_deposits
                    .iter()
                    .cloned()
                    .map(compact::ImportedOperation::AppendDeposit)
                    .collect(),
                ..compact::ImportedFacts::default()
            },
        )
        .unwrap(),
        &compact::ZoneFacts::default(),
    )
    .unwrap();
    let old_appends = old_expected.imported_tempo_block().operations();
    let new_appends = &candidate.expected_effects;
    assert_eq!(old_appends.len(), new_appends.len());
    for (old, new) in old_appends.iter().zip(new_appends) {
        let ExpectedImportedTempoOperation::DepositAppended(old) = old else {
            panic!()
        };
        let compact::ExpectedEffect::DepositAppended { id, queue_hash } = new else {
            panic!()
        };
        assert_eq!(old.id().portal, id.portal);
        assert_eq!(old.id().deposit_number, id.number);
        assert_eq!(old.queue_hash(), *queue_hash);
    }
    new.apply(&candidate.delta).unwrap();
    assert_snapshot_eq(&old, &new, token);
    assert_eq!(
        old.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(30),
            withdrawal_liability: U256::ZERO,
        }
    );

    let members = deposits
        .iter()
        .cloned()
        .map(DepositQueueMember::Ordinary)
        .collect();
    let old_zone = ZoneDepositPrefixInput::new(
        vec![],
        members,
        vec![
            AuthenticatedDepositOutcome::OrdinaryMinted {
                recipient: Address::repeat_byte(0x50),
                memo: B256::ZERO,
            },
            AuthenticatedDepositOutcome::OrdinaryFailed,
        ],
    );
    let old_expected = commit(&mut old, &empty_import(), &old_zone).unwrap();
    let candidate = compact::apply_zone(
        compact::apply_imported(&new, &compact::ImportedFacts::default()).unwrap(),
        &compact::ZoneFacts {
            deposits: compact_deposits
                .into_iter()
                .map(compact::Deposit::Ordinary)
                .collect(),
            outcomes: vec![
                compact::DepositOutcome::Minted,
                compact::DepositOutcome::Failed,
            ],
            ..compact::ZoneFacts::default()
        },
    )
    .unwrap();
    assert!(matches!(
        old_expected.zone_deposit_prefix().deposit_outcomes(),
        [
            ExpectedDepositOutcome::OrdinaryMinted(_),
            ExpectedDepositOutcome::OrdinaryFailed(_)
        ]
    ));
    assert!(matches!(
        candidate.expected_effects.as_slice(),
        [
            compact::ExpectedEffect::DepositProcessed { .. },
            compact::ExpectedEffect::WithdrawalRequested { .. },
            compact::ExpectedEffect::DepositFailed { .. }
        ]
    ));
    assert_eq!(
        old_expected.zone_deposit_prefix().processed_cursor().hash(),
        candidate.expected_state.processed_deposit_hash
    );
    new.apply(&candidate.delta).unwrap();
    assert_snapshot_eq(&old, &new, token);
}

#[test]
fn user_accept_finalize_submit_and_delivery_match_legacy_semantics() {
    let token = token(0xd7);
    let mut old = funded_state(token, U256::from(100));
    let mut new = compact_funded(token, 100);

    commit_block(
        &mut old,
        1,
        vec![ZoneOperation::user_withdrawal_accepted(user_withdrawal(
            token,
            0xdf,
            40,
            0,
            Bytes::new(),
        ))],
        None,
    )
    .unwrap();
    compact_commit(
        &mut new,
        compact::ImportedFacts::default(),
        compact::ZoneFacts {
            block_hash: B256::repeat_byte(1),
            block_number: 1,
            operations: vec![compact::ZoneOperation::AcceptWithdrawal(
                compact::UserWithdrawal {
                    sender: Address::repeat_byte(0xdf),
                    transaction_hash: B256::repeat_byte(0xe0),
                    token,
                    to: Address::repeat_byte(0xe1),
                    amount: 40,
                    memo: B256::repeat_byte(0xe2),
                    gas_limit: 0,
                    callback_data: Bytes::from(vec![0xdf]),
                    reveal_to: Bytes::new(),
                },
            )],
            ..compact::ZoneFacts::default()
        },
    );
    assert_snapshot_eq(&old, &new, token);

    commit_block(&mut old, 2, vec![], Some(empty_sender_finalization(2, 1))).unwrap();
    compact_commit(
        &mut new,
        compact::ImportedFacts {
            block_number: 2,
            ..compact::ImportedFacts::default()
        },
        compact::ZoneFacts {
            block_hash: B256::repeat_byte(2),
            block_number: 2,
            finalization: Some(compact::Finalization {
                block_number: 2,
                declared_count: 1,
                encrypted_senders: vec![Bytes::new()],
            }),
            ..compact::ZoneFacts::default()
        },
    );
    assert_snapshot_eq(&old, &new, token);

    submit_finalized_batch(&mut old, batch_id(1));
    let bid = compact::BatchId {
        zone_id: ZONE_ID,
        index: std::num::NonZeroU64::MIN,
    };
    let compact::StateValue::Batch(compact::BatchState::Finalized {
        boundary,
        queue_hash,
        ..
    }) = new.rows()[&compact::StateKey::Batch(bid)].clone()
    else {
        unreachable!()
    };
    compact_commit(
        &mut new,
        compact::ImportedFacts {
            operations: vec![compact::ImportedOperation::SubmitBatch(
                compact::BatchSubmission {
                    tempo_block: boundary.tempo_block,
                    previous_block: boundary.first_parent,
                    next_block: boundary.final_block,
                    previous_deposit: boundary.first_deposit,
                    next_deposit: boundary.final_deposit,
                    withdrawal_queue_hash: queue_hash,
                    next_zone_height: U256::from(boundary.zone_height),
                },
            )],
            ..compact::ImportedFacts::default()
        },
        compact::ZoneFacts::default(),
    );
    assert_snapshot_eq(&old, &new, token);

    let old_withdrawal = finalized_preimage(&old, 0);
    commit_imported(
        &mut old,
        20_003,
        U256::ZERO,
        vec![withdrawals_processed(
            vec![old_withdrawal.clone()],
            B256::ZERO,
            user_delivered_outcomes(1),
        )],
    )
    .unwrap();
    let compact::StateValue::Withdrawal(compact::WithdrawalOwner::Finalized { data, .. }) = new
        .rows()[&compact::StateKey::Withdrawal(compact::WithdrawalId {
        zone_id: ZONE_ID,
        index: 0,
    })]
        .clone()
    else {
        unreachable!()
    };
    assert_eq!(old_withdrawal.token(), data.token);
    assert_eq!(old_withdrawal.sender_tag(), data.sender_tag);
    assert_eq!(old_withdrawal.amount(), data.amount);
    compact_commit(
        &mut new,
        compact::ImportedFacts {
            operations: vec![compact::ImportedOperation::ProcessWithdrawals(
                compact::WithdrawalProcessing {
                    withdrawals: vec![data],
                    remaining_queue: B256::ZERO,
                    outcomes: vec![compact::WithdrawalOutcome::UserDelivered {
                        callback_deposits: vec![],
                    }],
                },
            )],
            ..compact::ImportedFacts::default()
        },
        compact::ZoneFacts::default(),
    );
    assert_snapshot_eq(&old, &new, token);
}

#[test]
fn failed_deposit_paid_and_pending_claim_match_legacy_semantics() {
    for pending in [false, true] {
        let token = token(if pending { 0xd5 } else { 0xd6 });
        let deposit = ordinary(token, 0xde, 40);
        let recipient = deposit.tempo_refund_recipient();
        let mut old = created_state(token);
        let mut new = compact_funded(token, 0);

        commit(
            &mut old,
            &ImportedTempoBlockInput::new(
                1,
                U256::ZERO,
                vec![
                    crate::model::input::ImportedTempoOperation::OrdinaryDepositAppended(
                        deposit.clone(),
                    ),
                ],
            ),
            &ZoneDepositPrefixInput::new(
                vec![],
                vec![DepositQueueMember::Ordinary(deposit.clone())],
                vec![AuthenticatedDepositOutcome::OrdinaryFailed],
            ),
        )
        .unwrap();
        compact_commit(
            &mut new,
            compact::ImportedFacts {
                operations: vec![compact::ImportedOperation::AppendDeposit(compact_deposit(
                    &deposit,
                ))],
                ..Default::default()
            },
            compact::ZoneFacts {
                deposits: vec![compact::Deposit::Ordinary(compact_deposit(&deposit))],
                outcomes: vec![compact::DepositOutcome::Failed],
                ..Default::default()
            },
        );
        assert_snapshot_eq(&old, &new, token);
        let compact::StateValue::Withdrawal(compact::WithdrawalOwner::PendingFailedDeposit {
            deposit: owner_deposit,
            token: owner_token,
            recipient: owner_recipient,
            amount,
        }) = &new.rows()[&compact::StateKey::Withdrawal(compact::WithdrawalId {
            zone_id: ZONE_ID,
            index: 0,
        })]
        else {
            panic!("compact failed-deposit owner missing")
        };
        assert_eq!(owner_deposit.number.get(), 1);
        assert_eq!(
            (*owner_token, *owner_recipient, *amount),
            (token, recipient, 40)
        );

        commit_block(&mut old, 2, vec![], Some(empty_sender_finalization(2, 1))).unwrap();
        submit_finalized_batch(&mut old, batch_id(1));
        compact_finalize_and_submit(&mut new, 1);
        assert_snapshot_eq(&old, &new, token);

        let old_withdrawal = finalized_preimage(&old, 0);
        let new_withdrawal = compact_withdrawal(&new, 0);
        let old_output = commit_imported(
            &mut old,
            20_001,
            U256::ZERO,
            vec![withdrawals_processed(
                vec![old_withdrawal],
                B256::ZERO,
                vec![if pending {
                    crate::model::input::AuthenticatedWithdrawalOutcome::FailedDepositPending
                } else {
                    crate::model::input::AuthenticatedWithdrawalOutcome::FailedDepositPaid
                }],
            )],
        )
        .unwrap();
        let candidate = compact::apply_zone(
            compact::apply_imported(
                &new,
                &compact::ImportedFacts {
                    operations: vec![compact::ImportedOperation::ProcessWithdrawals(
                        compact::WithdrawalProcessing {
                            withdrawals: vec![new_withdrawal],
                            remaining_queue: B256::ZERO,
                            outcomes: vec![if pending {
                                compact::WithdrawalOutcome::FailedDepositPending
                            } else {
                                compact::WithdrawalOutcome::FailedDepositPaid
                            }],
                        },
                    )],
                    ..Default::default()
                },
            )
            .unwrap(),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(old_output.imported_tempo_block().operations().len(), 1);
        assert!(
            matches!(candidate.expected_effects.as_slice(), [compact::ExpectedEffect::FailedDepositRefunded {
            deposit, recipient: effect_recipient, token: effect_token, amount: 40, fee: 0, pending: effect_pending
        }] if deposit.number.get() == 1 && *effect_recipient == recipient && *effect_token == token && *effect_pending == pending)
        );
        new.apply(&candidate.delta).unwrap();
        assert_snapshot_eq(&old, &new, token);

        if pending {
            let refund_id = compact::PortalRefundId {
                token,
                recipient,
                deposit: compact::DepositId {
                    portal: portal(),
                    number: NonZeroU64::MIN,
                },
            };
            assert_eq!(
                new.rows()[&compact::StateKey::PortalRefund(refund_id)],
                compact::StateValue::PortalRefund(compact::RefundCredit { amount: 40 })
            );
            commit_imported(
                &mut old,
                20_002,
                U256::ZERO,
                vec![
                    crate::model::input::ImportedTempoOperation::PortalRefundClaimed(
                        crate::model::input::RefundClaimInput::new(recipient, token, 40),
                    ),
                ],
            )
            .unwrap();
            compact_commit(
                &mut new,
                compact::ImportedFacts {
                    operations: vec![compact::ImportedOperation::ClaimPortalRefund(
                        compact::RefundClaim {
                            token,
                            recipient,
                            amount: 40,
                        },
                    )],
                    ..Default::default()
                },
                Default::default(),
            );
            assert_snapshot_eq(&old, &new, token);
            assert!(
                !new.rows()
                    .contains_key(&compact::StateKey::PortalRefund(refund_id))
            );
        }
    }
}

#[test]
fn empty_batch_matches_legacy_semantics() {
    let token = token(0xee);
    let mut old = created_state(token);
    let mut new = compact_funded(token, 0);
    commit_block(&mut old, 2, vec![], Some(empty_sender_finalization(2, 0))).unwrap();
    compact_commit(
        &mut new,
        compact::ImportedFacts {
            block_number: 2,
            ..Default::default()
        },
        compact::ZoneFacts {
            block_hash: B256::repeat_byte(2),
            block_number: 2,
            finalization: Some(compact::Finalization {
                block_number: 2,
                declared_count: 0,
                encrypted_senders: vec![],
            }),
            ..Default::default()
        },
    );
    assert_snapshot_eq(&old, &new, token);
    let id = compact::BatchId {
        zone_id: ZONE_ID,
        index: NonZeroU64::MIN,
    };
    let compact::StateValue::Batch(compact::BatchState::Finalized {
        first_withdrawal,
        count,
        ..
    }) = new.rows()[&compact::StateKey::Batch(id)]
    else {
        panic!("empty compact batch missing")
    };
    assert_eq!((first_withdrawal, count), (0, 0));
}

fn submitted_users(token: Address, users: &[(u8, u128)]) -> (ModelState, compact::State) {
    let mut old = funded_state(token, U256::from(1_000));
    let mut new = compact_funded(token, 1_000);
    let old_operations = users
        .iter()
        .map(|&(seed, amount)| {
            ZoneOperation::user_withdrawal_accepted(user_withdrawal(
                token,
                seed,
                amount,
                0,
                Bytes::new(),
            ))
        })
        .collect();
    commit_block(
        &mut old,
        1,
        old_operations,
        Some(empty_sender_finalization(1, users.len())),
    )
    .unwrap();
    let operations = users
        .iter()
        .map(|&(seed, amount)| {
            compact::ZoneOperation::AcceptWithdrawal(compact::UserWithdrawal {
                sender: Address::repeat_byte(seed),
                transaction_hash: B256::repeat_byte(seed.wrapping_add(1)),
                token,
                to: Address::repeat_byte(seed.wrapping_add(2)),
                amount,
                memo: B256::repeat_byte(seed.wrapping_add(3)),
                gas_limit: 0,
                callback_data: Bytes::from(vec![seed; usize::from(seed % 3)]),
                reveal_to: Bytes::new(),
            })
        })
        .collect();
    compact_commit(
        &mut new,
        compact::ImportedFacts {
            block_number: 1,
            ..Default::default()
        },
        compact::ZoneFacts {
            block_hash: B256::repeat_byte(1),
            block_number: 1,
            operations,
            finalization: Some(compact::Finalization {
                block_number: 1,
                declared_count: users.len(),
                encrypted_senders: vec![Bytes::new(); users.len()],
            }),
            ..Default::default()
        },
    );
    submit_finalized_batch(&mut old, batch_id(1));
    compact_finalize_and_submit_existing(&mut new);
    assert_snapshot_eq(&old, &new, token);
    (old, new)
}

fn compact_finalize_and_submit_existing(state: &mut compact::State) {
    let id = compact::BatchId {
        zone_id: ZONE_ID,
        index: NonZeroU64::MIN,
    };
    let compact::StateValue::Batch(compact::BatchState::Finalized {
        boundary,
        queue_hash,
        ..
    }) = state.rows()[&compact::StateKey::Batch(id)].clone()
    else {
        panic!("compact batch was not finalized")
    };
    compact_commit(
        state,
        compact::ImportedFacts {
            operations: vec![compact::ImportedOperation::SubmitBatch(
                compact::BatchSubmission {
                    tempo_block: boundary.tempo_block,
                    previous_block: boundary.first_parent,
                    next_block: boundary.final_block,
                    previous_deposit: boundary.first_deposit,
                    next_deposit: boundary.final_deposit,
                    withdrawal_queue_hash: queue_hash,
                    next_zone_height: U256::from(boundary.zone_height),
                },
            )],
            ..Default::default()
        },
        Default::default(),
    );
}

#[test]
fn user_bounce_back_minted_and_pending_claim_traces_match_exactly() {
    for pending in [false, true] {
        let token = token(if pending { 0xb1 } else { 0xb0 });
        let recipient = Address::repeat_byte(if pending { 0xb3 } else { 0xb2 });
        let (mut old, mut new) = submitted_users(token, &[(0x51, 40)]);
        let old_withdrawal = finalized_preimage(&old, 0);
        let new_withdrawal = compact_withdrawal(&new, 0);

        let old_effects = commit_imported(
            &mut old,
            20,
            U256::ZERO,
            vec![withdrawals_processed(
                vec![old_withdrawal],
                B256::ZERO,
                vec![AuthenticatedWithdrawalOutcome::UserBounced],
            )],
        )
        .unwrap();
        let candidate = compact::apply_zone(
            compact::apply_imported(
                &new,
                &compact::ImportedFacts {
                    operations: vec![compact::ImportedOperation::ProcessWithdrawals(
                        compact::WithdrawalProcessing {
                            withdrawals: vec![new_withdrawal],
                            remaining_queue: B256::ZERO,
                            outcomes: vec![compact::WithdrawalOutcome::UserBounced],
                        },
                    )],
                    ..Default::default()
                },
            )
            .unwrap(),
            &Default::default(),
        )
        .unwrap();
        let [ExpectedImportedTempoOperation::WithdrawalsProcessed(processing)] =
            old_effects.imported_tempo_block().operations()
        else {
            panic!("legacy bounce processing effect missing")
        };
        let [ExpectedProcessedWithdrawal::UserBounced(bounced)] = processing.members() else {
            panic!("legacy user bounce effect missing")
        };
        let append = bounced.first();
        let processed = bounced.second();
        assert!(matches!(candidate.expected_effects.as_slice(), [
            compact::ExpectedEffect::BounceBackAppended { fallback_nonce: 1, token: effect_token, amount: 40, id, queue_hash },
            compact::ExpectedEffect::UserWithdrawalProcessed { id: wid, to, sender_tag, token: processed_token, amount: 40, callback_success: false }
        ] if *effect_token == token
            && id.number.get() == 1
            && id.portal == portal()
            && *queue_hash == append.append().queue_hash()
            && wid.index == 0
            && wid.zone_id == ZONE_ID
            && *to == processed.to()
            && *sender_tag == processed.sender_tag()
            && *processed_token == processed.token()));
        new.apply(&candidate.delta).unwrap();
        assert_snapshot_eq(&old, &new, token);

        let compact_bounce = compact::BounceBackDeposit {
            token,
            fallback_nonce: NonZeroU64::MIN,
            amount: 40,
        };
        let old_deposit_effects = commit(
            &mut old,
            &empty_import(),
            &ZoneDepositPrefixInput::new(
                vec![],
                vec![DepositQueueMember::WithdrawalBounceBack(bounce(
                    token, 1, 40,
                ))],
                vec![if pending {
                    AuthenticatedDepositOutcome::WithdrawalBounceBackPending { recipient }
                } else {
                    AuthenticatedDepositOutcome::WithdrawalBounceBackMinted { recipient }
                }],
            ),
        )
        .unwrap();
        let candidate = compact::apply_zone(
            compact::apply_imported(&new, &Default::default()).unwrap(),
            &compact::ZoneFacts {
                deposits: vec![compact::Deposit::BounceBack(compact_bounce)],
                outcomes: vec![if pending {
                    compact::DepositOutcome::BounceBackPending { recipient }
                } else {
                    compact::DepositOutcome::BounceBackMinted { recipient }
                }],
                ..Default::default()
            },
        )
        .unwrap();
        if pending {
            assert!(matches!(
                old_deposit_effects.zone_deposit_prefix().deposit_outcomes(),
                [ExpectedDepositOutcome::WithdrawalBounceBackPending(_)]
            ));
            assert!(matches!(candidate.expected_effects.as_slice(),
                [compact::ExpectedEffect::BounceBackPending { token: effect_token, amount: 40 }]
                    if *effect_token == token));
        } else {
            assert!(matches!(
                old_deposit_effects.zone_deposit_prefix().deposit_outcomes(),
                [ExpectedDepositOutcome::WithdrawalBounceBackMinted(_)]
            ));
            assert!(matches!(candidate.expected_effects.as_slice(),
                [compact::ExpectedEffect::BounceBackMinted { token: effect_token, amount: 40 }]
                    if *effect_token == token));
        }
        new.apply(&candidate.delta).unwrap();
        assert_snapshot_eq(&old, &new, token);

        if pending {
            let old_claim = commit_block(
                &mut old,
                2,
                vec![ZoneOperation::InboxRefundClaimed(
                    crate::model::input::RefundClaimInput::new(recipient, token, 40),
                )],
                None,
            )
            .unwrap();
            let candidate = compact::apply_zone(
                compact::apply_imported(&new, &Default::default()).unwrap(),
                &compact::ZoneFacts {
                    block_hash: B256::repeat_byte(2),
                    block_number: 2,
                    operations: vec![compact::ZoneOperation::ClaimInboxRefund(
                        compact::RefundClaim {
                            token,
                            recipient,
                            amount: 40,
                        },
                    )],
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(matches!(old_claim.zone_block().operations(), [_]));
            assert_eq!(
                candidate.expected_effects,
                vec![compact::ExpectedEffect::RefundClaimed {
                    token,
                    recipient,
                    amount: 40
                }]
            );
            new.apply(&candidate.delta).unwrap();
            assert_snapshot_eq(&old, &new, token);
        }
    }
}

#[test]
fn two_member_partial_processing_retains_exact_suffix_then_exhausts() {
    let token = token(0xb4);
    let (mut old, mut new) = submitted_users(token, &[(0x61, 30), (0x62, 50)]);
    let old_withdrawals = [finalized_preimage(&old, 0), finalized_preimage(&old, 1)];
    let new_withdrawals = [compact_withdrawal(&new, 0), compact_withdrawal(&new, 1)];
    let suffix = withdrawal_queue_hash(&old_withdrawals[1..]);
    assert_eq!(
        suffix,
        compact::withdrawal_queue_hash(&new_withdrawals[1..])
    );

    for index in 0..2 {
        let remaining = if index == 0 { suffix } else { B256::ZERO };
        commit_imported(
            &mut old,
            30 + index as u64,
            U256::ZERO,
            vec![withdrawals_processed(
                vec![old_withdrawals[index].clone()],
                remaining,
                user_delivered_outcomes(1),
            )],
        )
        .unwrap();
        let candidate = compact::apply_zone(
            compact::apply_imported(
                &new,
                &compact::ImportedFacts {
                    operations: vec![compact::ImportedOperation::ProcessWithdrawals(
                        compact::WithdrawalProcessing {
                            withdrawals: vec![new_withdrawals[index].clone()],
                            remaining_queue: remaining,
                            outcomes: vec![compact::WithdrawalOutcome::UserDelivered {
                                callback_deposits: vec![],
                            }],
                        },
                    )],
                    ..Default::default()
                },
            )
            .unwrap(),
            &Default::default(),
        )
        .unwrap();
        assert!(
            matches!(candidate.expected_effects.as_slice(), [compact::ExpectedEffect::UserWithdrawalProcessed { id, callback_success: true, .. }] if id.index == index as u64)
        );
        new.apply(&candidate.delta).unwrap();
        assert_snapshot_eq(&old, &new, token);
        if index == 0 {
            let id = compact::BatchId {
                zone_id: ZONE_ID,
                index: NonZeroU64::MIN,
            };
            let compact::StateValue::Batch(compact::BatchState::Submitted {
                queue_hash,
                next_ordinal,
                ..
            }) = new.rows()[&compact::StateKey::Batch(id)]
            else {
                panic!("partial batch missing")
            };
            assert_eq!((queue_hash, next_ordinal), (suffix, 1));
        }
    }
    assert!(
        !new.rows()
            .contains_key(&compact::StateKey::Batch(compact::BatchId {
                zone_id: ZONE_ID,
                index: NonZeroU64::MIN
            }))
    );
}
