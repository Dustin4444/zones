use alloy_primitives::{Address, B256, Bytes, U256, address, b256, fixed_bytes};

use std::{collections::BTreeMap, num::NonZeroU64};

use crate::kernel::{
    BatchId, BatchSubmission, Deposit, DepositId, DepositOutcome, Effect, Finalization,
    ImportedFacts, ImportedOperation, OrdinaryDeposit, PortalIdentity, PortalState, RefundClaim,
    State, StateKey, StateValue, TokenEnable, TokenPhase, UserWithdrawal, WithdrawalId,
    WithdrawalOutcome, WithdrawalProcessing, ZoneFacts, ZoneOperation,
    apply::TransitionError,
    apply_genesis_handoff, apply_imported, apply_zone,
    commitments::{WITHDRAWAL_SENTINEL, ordinary_deposit_hash, portal_address},
    facts::DepositPayload,
    invariants::{InvariantCode, validate},
    state::{
        BatchState, FallbackId, InboxRefundId, Overlay, PortalRefundId, TokenAccounting,
        TokenState, WithdrawalOwner,
    },
};

const ZONE_ID: u32 = 7;

fn token_state() -> TokenState {
    TokenState {
        phase: TokenPhase::PendingZoneEnable,
        accounting: TokenAccounting::default(),
    }
}

fn identity() -> PortalIdentity {
    PortalIdentity {
        zone_id: ZONE_ID,
        portal: portal_address(ZONE_ID),
        initial_token: address!("1111111111111111111111111111111111111111"),
    }
}

fn enable(token: Address) -> TokenEnable {
    TokenEnable {
        token,
        name: "Token".into(),
        symbol: "TOK".into(),
        currency: "USD".into(),
    }
}

fn deposit() -> OrdinaryDeposit {
    OrdinaryDeposit {
        token: identity().initial_token,
        sender: address!("2222222222222222222222222222222222222222"),
        amount: 1_000,
        tempo_refund_recipient: address!("3333333333333333333333333333333333333333"),
        key_index: U256::from(7),
        encrypted: DepositPayload {
            ephemeral_pubkey_x: b256!(
                "4444444444444444444444444444444444444444444444444444444444444444"
            ),
            ephemeral_pubkey_y_parity: 2,
            ciphertext: fixed_bytes!(
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f"
            ),
            nonce: fixed_bytes!("555555555555555555555555"),
            tag: fixed_bytes!("66666666666666666666666666666666"),
        },
    }
}

fn created_with_deposit() -> (State, OrdinaryDeposit) {
    let parent = State::awaiting(identity());
    let deposit = deposit();
    let imported = apply_imported(
        &parent,
        &ImportedFacts {
            operations: vec![
                ImportedOperation::Create {
                    identity: identity(),
                    initial_token: enable(identity().initial_token),
                },
                ImportedOperation::AppendDeposit(deposit.clone()),
            ],
            ..ImportedFacts::default()
        },
    )
    .unwrap();
    let mut state = imported.into_state();
    state
        .apply(&apply_genesis_handoff(&state).unwrap())
        .unwrap();
    (state, deposit)
}

#[test]
fn zone_rejects_a_deposit_prefix_without_mutating_parent() {
    let parent = funded_state();
    let first = deposit();
    let mut second = first.clone();
    second.amount += 1;
    let imported = apply_imported(
        &parent,
        &ImportedFacts {
            operations: vec![
                ImportedOperation::AppendDeposit(first.clone()),
                ImportedOperation::AppendDeposit(second),
            ],
            ..Default::default()
        },
    )
    .unwrap();
    let before = parent.clone();
    assert_eq!(
        apply_zone(
            imported,
            &ZoneFacts {
                deposits: vec![Deposit::Ordinary(first)],
                outcomes: vec![DepositOutcome::Minted],
                ..Default::default()
            }
        ),
        Err(TransitionError::CommitmentMismatch)
    );
    assert_eq!(parent, before);
}

#[test]
fn genesis_handoff_promotes_authenticated_pending_tokens_for_first_live_use() {
    let mut state = State::awaiting(identity());
    let imported = apply_imported(
        &state,
        &ImportedFacts {
            operations: vec![ImportedOperation::Create {
                identity: identity(),
                initial_token: enable(identity().initial_token),
            }],
            ..Default::default()
        },
    )
    .unwrap();
    state = imported.into_state();
    assert!(matches!(
        state.rows().get(&StateKey::Token(identity().initial_token)),
        Some(StateValue::Token(TokenState {
            phase: TokenPhase::PendingZoneEnable,
            ..
        }))
    ));
    state
        .apply(&apply_genesis_handoff(&state).unwrap())
        .unwrap();
    assert!(matches!(
        state.rows().get(&StateKey::Token(identity().initial_token)),
        Some(StateValue::Token(TokenState {
            phase: TokenPhase::ZoneEnabled,
            ..
        }))
    ));
    apply_zone(
        apply_imported(&state, &ImportedFacts::default()).unwrap(),
        &ZoneFacts {
            operations: vec![ZoneOperation::ClaimInboxRefund(RefundClaim {
                token: identity().initial_token,
                recipient: Address::repeat_byte(9),
                amount: 0,
            })],
            ..Default::default()
        },
    )
    .unwrap();
}

fn commit(state: &mut State, imported: ImportedFacts, zone: ZoneFacts) -> Vec<Effect> {
    let candidate = apply_zone(apply_imported(state, &imported).unwrap(), &zone).unwrap();
    state.apply(&candidate.delta).unwrap();
    candidate.expected_effects
}

fn funded_state() -> State {
    let mut state = State::awaiting(identity());
    commit(
        &mut state,
        ImportedFacts {
            operations: vec![ImportedOperation::Create {
                identity: identity(),
                initial_token: enable(identity().initial_token),
            }],
            ..ImportedFacts::default()
        },
        ZoneFacts {
            enabled_tokens: vec![enable(identity().initial_token)],
            ..ZoneFacts::default()
        },
    );
    let mut rows = state.rows().clone();
    let StateValue::Token(mut token) = rows[&StateKey::Token(identity().initial_token)].clone()
    else {
        unreachable!()
    };
    token.accounting.supply = U256::from(100);
    rows.insert(
        StateKey::Token(identity().initial_token),
        StateValue::Token(token),
    );
    State::from_rows(rows).unwrap()
}

fn user_withdrawal(seed: u8, amount: u128) -> UserWithdrawal {
    UserWithdrawal {
        sender: Address::repeat_byte(seed),
        transaction_hash: B256::repeat_byte(seed.wrapping_add(1)),
        token: identity().initial_token,
        to: Address::repeat_byte(seed.wrapping_add(2)),
        amount,
        memo: B256::repeat_byte(seed.wrapping_add(3)),
        gas_limit: 0,
        callback_data: Default::default(),
        reveal_to: Default::default(),
    }
}

fn finalized_user_state() -> (State, crate::kernel::Withdrawal) {
    let mut state = funded_state();
    commit(
        &mut state,
        ImportedFacts::default(),
        ZoneFacts {
            block_hash: B256::repeat_byte(1),
            block_number: 1,
            operations: vec![ZoneOperation::AcceptWithdrawal(user_withdrawal(0x40, 40))],
            ..ZoneFacts::default()
        },
    );
    commit(
        &mut state,
        ImportedFacts {
            block_number: 2,
            ..ImportedFacts::default()
        },
        ZoneFacts {
            block_hash: B256::repeat_byte(2),
            block_number: 2,
            finalization: Some(Finalization {
                block_number: 2,
                declared_count: 1,
                encrypted_senders: vec![Default::default()],
            }),
            ..ZoneFacts::default()
        },
    );
    let id = WithdrawalId {
        zone_id: ZONE_ID,
        index: 0,
    };
    let StateValue::Withdrawal(WithdrawalOwner::Finalized { data, .. }) =
        state.rows()[&StateKey::Withdrawal(id)].clone()
    else {
        unreachable!()
    };
    (state, data)
}

fn submit_first_batch(state: &mut State) {
    submit_batch(state, NonZeroU64::MIN);
}

fn submit_batch(state: &mut State, index: NonZeroU64) {
    let id = BatchId {
        zone_id: ZONE_ID,
        index,
    };
    let StateValue::Batch(BatchState::Finalized {
        boundary,
        queue_hash,
        ..
    }) = state.rows()[&StateKey::Batch(id)].clone()
    else {
        unreachable!()
    };
    commit(
        state,
        ImportedFacts {
            operations: vec![ImportedOperation::SubmitBatch(BatchSubmission {
                tempo_block: boundary.tempo_block,
                previous_block: boundary.first_parent,
                next_block: boundary.final_block,
                previous_deposit: boundary.first_deposit,
                next_deposit: boundary.final_deposit,
                withdrawal_queue_hash: queue_hash,
                next_zone_height: U256::from(boundary.zone_height),
            })],
            ..ImportedFacts::default()
        },
        ZoneFacts::default(),
    );
}

#[test]
fn batch_invariants_allow_tempo_to_advance_faster_than_zone_height() {
    let mut state = funded_state();
    commit(
        &mut state,
        ImportedFacts {
            block_number: 1,
            ..ImportedFacts::default()
        },
        ZoneFacts {
            block_hash: B256::repeat_byte(1),
            block_number: 1,
            operations: vec![ZoneOperation::AcceptWithdrawal(user_withdrawal(0x40, 40))],
            ..ZoneFacts::default()
        },
    );
    commit(
        &mut state,
        ImportedFacts {
            block_number: 4,
            ..ImportedFacts::default()
        },
        ZoneFacts {
            block_hash: B256::repeat_byte(2),
            block_number: 2,
            finalization: Some(Finalization {
                block_number: 2,
                declared_count: 1,
                encrypted_senders: vec![Default::default()],
            }),
            ..ZoneFacts::default()
        },
    );
    commit(
        &mut state,
        ImportedFacts {
            block_number: 5,
            ..ImportedFacts::default()
        },
        ZoneFacts {
            block_hash: B256::repeat_byte(3),
            block_number: 3,
            operations: vec![ZoneOperation::AcceptWithdrawal(user_withdrawal(0x50, 20))],
            ..ZoneFacts::default()
        },
    );
    commit(
        &mut state,
        ImportedFacts {
            block_number: 8,
            ..ImportedFacts::default()
        },
        ZoneFacts {
            block_hash: B256::repeat_byte(4),
            block_number: 4,
            finalization: Some(Finalization {
                block_number: 4,
                declared_count: 1,
                encrypted_senders: vec![Default::default()],
            }),
            ..ZoneFacts::default()
        },
    );
    validate(&state).unwrap();

    submit_batch(&mut state, NonZeroU64::MIN);
    validate(&state).unwrap();
    submit_batch(&mut state, NonZeroU64::new(2).unwrap());
    validate(&state).unwrap();
}

#[test]
fn overlay_reads_writes_deletes_and_finishes_in_key_order() {
    let mut rows = State::awaiting(identity()).rows().clone();
    let token = identity().initial_token;
    rows.insert(StateKey::Token(token), StateValue::Token(token_state()));
    let parent = State::from_rows(rows).unwrap();

    let mut overlay = Overlay::new(&parent);
    overlay.set(StateKey::Token(token), None);
    overlay.set(
        StateKey::Portal,
        Some(StateValue::Portal(PortalState::Created {
            identity: identity(),
            bounceback_gas: 5,
            deposit: crate::kernel::state::Cursor::ZERO,
            settlement: crate::kernel::state::Settlement::ZERO,
        })),
    );
    assert!(overlay.get(&StateKey::Token(token)).is_none());
    let delta = overlay.finish();
    assert_eq!(delta.writes()[0].0, StateKey::Portal);
    assert_eq!(delta.writes()[1], (StateKey::Token(token), None));
}

#[test]
fn state_rejects_wrong_value_family() {
    let rows = BTreeMap::from([(StateKey::Portal, StateValue::Token(token_state()))]);
    assert_eq!(State::from_rows(rows).unwrap_err().key, StateKey::Portal);
}

#[test]
fn state_key_family_order_is_stable() {
    let deposit = DepositId {
        portal: identity().portal,
        number: NonZeroU64::MIN,
    };
    let withdrawal = WithdrawalId {
        zone_id: ZONE_ID,
        index: 0,
    };
    let keys = [
        StateKey::Portal,
        StateKey::Zone,
        StateKey::Token(Address::ZERO),
        StateKey::Deposit(deposit),
        StateKey::Withdrawal(withdrawal),
        StateKey::Batch(BatchId {
            zone_id: ZONE_ID,
            index: NonZeroU64::MIN,
        }),
        StateKey::Fallback(FallbackId {
            zone_id: ZONE_ID,
            nonce: NonZeroU64::MIN,
        }),
        StateKey::PortalRefund(PortalRefundId {
            token: Address::ZERO,
            recipient: Address::ZERO,
            deposit,
        }),
        StateKey::InboxRefund(InboxRefundId {
            token: Address::ZERO,
            recipient: Address::ZERO,
            withdrawal,
        }),
    ];
    assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn creation_checks_identity_address_initial_token_and_repetition() {
    let parent = State::awaiting(identity());
    let create = |identity, token| ImportedFacts {
        operations: vec![ImportedOperation::Create {
            identity,
            initial_token: enable(token),
        }],
        ..ImportedFacts::default()
    };
    let mut wrong_identity = identity();
    wrong_identity.zone_id += 1;
    assert_eq!(
        apply_imported(&parent, &create(wrong_identity, identity().initial_token)).unwrap_err(),
        TransitionError::PortalIdentityMismatch
    );
    let wrong_address_identity = PortalIdentity {
        portal: Address::repeat_byte(0xaa),
        ..identity()
    };
    let wrong_address_parent = State::awaiting(wrong_address_identity);
    assert_eq!(
        apply_imported(
            &wrong_address_parent,
            &create(wrong_address_identity, wrong_address_identity.initial_token)
        )
        .unwrap_err(),
        TransitionError::PortalAddressMismatch
    );
    assert_eq!(
        apply_imported(&parent, &create(identity(), Address::repeat_byte(0xbb))).unwrap_err(),
        TransitionError::InitialTokenMismatch
    );
    assert!(apply_imported(&parent, &create(identity(), identity().initial_token)).is_ok());
    assert_eq!(
        apply_imported(
            &parent,
            &ImportedFacts {
                operations: vec![
                    ImportedOperation::Create {
                        identity: identity(),
                        initial_token: enable(identity().initial_token),
                    },
                    ImportedOperation::Create {
                        identity: identity(),
                        initial_token: enable(identity().initial_token),
                    },
                ],
                ..ImportedFacts::default()
            }
        )
        .unwrap_err(),
        TransitionError::PortalAlreadyCreated
    );
}

#[test]
fn token_enablement_requires_creation_and_rejects_duplicates() {
    let parent = State::awaiting(identity());
    assert_eq!(
        apply_imported(
            &parent,
            &ImportedFacts {
                operations: vec![ImportedOperation::EnableToken(enable(
                    identity().initial_token
                ))],
                ..ImportedFacts::default()
            }
        )
        .unwrap_err(),
        TransitionError::PortalNotCreated
    );
    assert_eq!(
        apply_imported(
            &parent,
            &ImportedFacts {
                operations: vec![
                    ImportedOperation::Create {
                        identity: identity(),
                        initial_token: enable(identity().initial_token),
                    },
                    ImportedOperation::EnableToken(enable(identity().initial_token)),
                ],
                ..ImportedFacts::default()
            }
        )
        .unwrap_err(),
        TransitionError::TokenAlreadyEnabled(identity().initial_token)
    );
}

#[test]
fn zone_enablement_must_exactly_match_this_imported_block() {
    let parent = State::awaiting(identity());
    let imported = apply_imported(
        &parent,
        &ImportedFacts {
            operations: vec![ImportedOperation::Create {
                identity: identity(),
                initial_token: enable(identity().initial_token),
            }],
            ..ImportedFacts::default()
        },
    )
    .unwrap();
    assert_eq!(
        apply_zone(imported, &ZoneFacts::default()).unwrap_err(),
        TransitionError::TokenEnableMismatch
    );
}

#[test]
fn ordinary_deposit_commitment_matches_literal_vector() {
    assert_eq!(
        ordinary_deposit_hash(&deposit(), B256::ZERO),
        b256!("89982eeee3ca64954daa0322b331f17efd85a433564bfdb4938c0ab087663a5d")
    );
}

#[test]
fn sender_tag_matches_literal_vector_and_includes_fallback_nonce() {
    let sender = Address::repeat_byte(0x11);
    let transaction = B256::repeat_byte(0x22);
    assert_eq!(
        crate::kernel::commitments::sender_tag(sender, transaction, 0x0102_0304_0506_0708),
        b256!("09e5aae3d74dbb09f2046a3a15c5504ce844113049b83c2884ca41a43124acbf")
    );
    assert_ne!(
        crate::kernel::commitments::sender_tag(sender, transaction, 1),
        crate::kernel::commitments::sender_tag(sender, transaction, 2)
    );
    assert_eq!(
        crate::kernel::commitments::failed_deposit_sender_tag(),
        alloy_primitives::keccak256([0u8; 52])
    );
}

#[test]
fn ordinary_deposit_append_and_mint_close_accounting() {
    let (state, deposit) = created_with_deposit();
    let candidate = apply_zone(
        apply_imported(&state, &ImportedFacts::default()).unwrap(),
        &ZoneFacts {
            deposits: vec![Deposit::Ordinary(deposit.clone())],
            outcomes: vec![DepositOutcome::Minted],
            ..ZoneFacts::default()
        },
    )
    .unwrap();
    assert_eq!(candidate.expected_effects.len(), 1);
    assert!(matches!(
        candidate.expected_effects[0],
        Effect::DepositProcessed { amount: 1_000, .. }
    ));
    let mut after = state;
    after.apply(&candidate.delta).unwrap();
    let Some(StateValue::Token(token)) = after.rows().get(&StateKey::Token(deposit.token)) else {
        panic!("token row missing")
    };
    assert_eq!(token.accounting.supply, U256::from(1_000));
    assert_eq!(token.accounting.deposits, U256::ZERO);
    validate(&after).unwrap();
}

#[test]
fn failed_deposit_creates_refund_owner_and_rejects_prefix_mutation() {
    let (state, deposit) = created_with_deposit();
    let mut mutated = deposit.clone();
    mutated.amount += 1;
    let error = apply_zone(
        apply_imported(&state, &ImportedFacts::default()).unwrap(),
        &ZoneFacts {
            deposits: vec![Deposit::Ordinary(mutated)],
            outcomes: vec![DepositOutcome::Failed],
            ..ZoneFacts::default()
        },
    )
    .unwrap_err();
    assert_eq!(error, TransitionError::DepositPrefixMismatch);

    let candidate = apply_zone(
        apply_imported(&state, &ImportedFacts::default()).unwrap(),
        &ZoneFacts {
            deposits: vec![Deposit::Ordinary(deposit)],
            outcomes: vec![DepositOutcome::Failed],
            ..ZoneFacts::default()
        },
    )
    .unwrap();
    assert!(matches!(
        candidate.expected_effects[0],
        Effect::WithdrawalRequested { .. }
    ));
    assert!(matches!(
        candidate.expected_effects[1],
        Effect::DepositFailed { .. }
    ));
    assert!(
        candidate
            .delta
            .writes()
            .iter()
            .any(|(key, value)| matches!(key, StateKey::Withdrawal(_)) && value.is_some())
    );
}

#[test]
fn outcome_cardinality_is_exact_and_failures_do_not_mutate_parent() {
    let (state, deposit) = created_with_deposit();
    let before = state.clone();
    assert_eq!(
        apply_zone(
            apply_imported(&state, &ImportedFacts::default()).unwrap(),
            &ZoneFacts {
                deposits: vec![Deposit::Ordinary(deposit)],
                outcomes: vec![],
                ..ZoneFacts::default()
            }
        )
        .unwrap_err(),
        TransitionError::DepositOutcomeCountMismatch
    );
    assert_eq!(state, before);
}

#[test]
fn withdrawal_outcome_cardinality_is_distinct_and_exact() {
    let (mut state, withdrawal) = finalized_user_state();
    submit_first_batch(&mut state);
    let before = state.clone();
    let error = apply_imported(
        &state,
        &ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::ZERO,
                    withdrawals: vec![withdrawal],
                    remaining_queue: B256::ZERO,
                    outcomes: vec![],
                },
            )],
            ..ImportedFacts::default()
        },
    )
    .unwrap_err();
    assert_eq!(error, TransitionError::WithdrawalOutcomeCountMismatch);
    assert_eq!(state, before);
}

#[test]
fn invariant_validation_detects_pre_creation_rows() {
    let mut rows = State::awaiting(identity()).rows().clone();
    rows.insert(
        StateKey::Token(identity().initial_token),
        StateValue::Token(token_state()),
    );
    let state = State::from_rows(rows).unwrap();
    assert_eq!(
        validate(&state).unwrap_err().code,
        InvariantCode::PreCreationRows
    );
}

#[test]
fn zero_refund_recipient_is_rejected() {
    let parent = State::awaiting(identity());
    let mut input = deposit();
    input.tempo_refund_recipient = Address::ZERO;
    assert_eq!(
        apply_imported(
            &parent,
            &ImportedFacts {
                operations: vec![
                    ImportedOperation::Create {
                        identity: identity(),
                        initial_token: enable(identity().initial_token),
                    },
                    ImportedOperation::AppendDeposit(input),
                ],
                ..ImportedFacts::default()
            }
        )
        .unwrap_err(),
        TransitionError::ZeroRefundRecipient
    );
}

#[test]
fn user_delivery_closes_batch_withdrawal_fallback_accounting() {
    let (mut state, withdrawal) = finalized_user_state();
    submit_first_batch(&mut state);
    commit(
        &mut state,
        ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::ZERO,
                    withdrawals: vec![withdrawal],
                    remaining_queue: B256::ZERO,
                    outcomes: vec![WithdrawalOutcome::UserDelivered {
                        callback_deposits: Vec::new(),
                    }],
                },
            )],
            ..ImportedFacts::default()
        },
        ZoneFacts::default(),
    );
    let token = match state.rows()[&StateKey::Token(identity().initial_token)] {
        StateValue::Token(token) => token,
        _ => unreachable!(),
    };
    assert_eq!(token.accounting.supply, U256::from(60));
    assert_eq!(token.accounting.withdrawals, U256::ZERO);
    assert!(!state.rows().keys().any(|key| matches!(
        key,
        StateKey::Withdrawal(_) | StateKey::Fallback(_) | StateKey::Batch(_)
    )));
    validate(&state).unwrap();
}

#[test]
fn user_bounce_pending_and_inbox_claim_close_complete_lifecycle() {
    let (mut state, withdrawal) = finalized_user_state();
    submit_first_batch(&mut state);
    let bounce = crate::kernel::BounceBackDeposit {
        token: identity().initial_token,
        fallback_nonce: NonZeroU64::MIN,
        amount: 40,
    };
    let recipient = Address::repeat_byte(0x88);
    commit(
        &mut state,
        ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::ZERO,
                    withdrawals: vec![withdrawal],
                    remaining_queue: B256::ZERO,
                    outcomes: vec![WithdrawalOutcome::UserBounced],
                },
            )],
            ..ImportedFacts::default()
        },
        ZoneFacts {
            deposits: vec![Deposit::BounceBack(bounce)],
            outcomes: vec![DepositOutcome::BounceBackPending { recipient }],
            ..ZoneFacts::default()
        },
    );
    assert!(
        state
            .rows()
            .keys()
            .any(|key| matches!(key, StateKey::InboxRefund(_)))
    );
    commit(
        &mut state,
        ImportedFacts::default(),
        ZoneFacts {
            operations: vec![ZoneOperation::ClaimInboxRefund(RefundClaim {
                token: identity().initial_token,
                recipient,
                amount: 40,
            })],
            ..ZoneFacts::default()
        },
    );
    let StateValue::Token(token) = state.rows()[&StateKey::Token(identity().initial_token)] else {
        unreachable!()
    };
    assert_eq!(token.accounting.supply, U256::from(100));
    assert_eq!(token.accounting.withdrawals, U256::ZERO);
    validate(&state).unwrap();
}

#[test]
fn empty_batch_submits_without_ring_capacity() {
    let mut state = funded_state();
    commit(
        &mut state,
        ImportedFacts::default(),
        ZoneFacts {
            block_hash: B256::repeat_byte(1),
            block_number: 1,
            finalization: Some(Finalization {
                block_number: 1,
                declared_count: 0,
                encrypted_senders: vec![],
            }),
            ..ZoneFacts::default()
        },
    );
    let effects = {
        let id = BatchId {
            zone_id: ZONE_ID,
            index: NonZeroU64::MIN,
        };
        let StateValue::Batch(BatchState::Finalized {
            boundary,
            queue_hash,
            ..
        }) = state.rows()[&StateKey::Batch(id)].clone()
        else {
            unreachable!()
        };
        commit(
            &mut state,
            ImportedFacts {
                operations: vec![ImportedOperation::SubmitBatch(BatchSubmission {
                    tempo_block: boundary.tempo_block,
                    previous_block: boundary.first_parent,
                    next_block: boundary.final_block,
                    previous_deposit: boundary.first_deposit,
                    next_deposit: boundary.final_deposit,
                    withdrawal_queue_hash: queue_hash,
                    next_zone_height: U256::from(boundary.zone_height),
                })],
                ..ImportedFacts::default()
            },
            ZoneFacts::default(),
        )
    };
    assert!(
        matches!(effects.last(), Some(Effect::BatchSubmitted { queue_index, .. }) if *queue_index == U256::MAX)
    );
    let StateValue::Portal(PortalState::Created { settlement, .. }) =
        &state.rows()[&StateKey::Portal]
    else {
        unreachable!()
    };
    assert_eq!(settlement.queue_head, settlement.queue_tail);
    assert!(
        !state
            .rows()
            .keys()
            .any(|key| matches!(key, StateKey::Batch(_)))
    );
    validate(&state).unwrap();
}

#[test]
fn partial_processing_keeps_exact_suffix_then_exhausts() {
    let mut state = funded_state();
    let mut rows = state.rows().clone();
    let StateValue::Token(mut token) = rows[&StateKey::Token(identity().initial_token)] else {
        unreachable!()
    };
    token.accounting.supply = U256::from(200);
    rows.insert(
        StateKey::Token(identity().initial_token),
        StateValue::Token(token),
    );
    state = State::from_rows(rows).unwrap();
    commit(
        &mut state,
        ImportedFacts::default(),
        ZoneFacts {
            block_hash: B256::repeat_byte(1),
            block_number: 1,
            operations: vec![
                ZoneOperation::AcceptWithdrawal(user_withdrawal(0x50, 40)),
                ZoneOperation::AcceptWithdrawal(user_withdrawal(0x60, 30)),
            ],
            finalization: Some(Finalization {
                block_number: 1,
                declared_count: 2,
                encrypted_senders: vec![Default::default(), Default::default()],
            }),
            ..ZoneFacts::default()
        },
    );
    submit_first_batch(&mut state);
    let withdrawals = (0..2)
        .map(|index| {
            let StateValue::Withdrawal(WithdrawalOwner::Finalized { data, .. }) = &state.rows()
                [&StateKey::Withdrawal(WithdrawalId {
                    zone_id: ZONE_ID,
                    index,
                })]
            else {
                unreachable!()
            };
            data.clone()
        })
        .collect::<Vec<_>>();
    let suffix = crate::kernel::commitments::withdrawal_hash(&withdrawals[1], WITHDRAWAL_SENTINEL);
    commit(
        &mut state,
        ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::ZERO,
                    withdrawals: vec![withdrawals[0].clone()],
                    remaining_queue: suffix,
                    outcomes: vec![WithdrawalOutcome::UserDelivered {
                        callback_deposits: vec![],
                    }],
                },
            )],
            ..ImportedFacts::default()
        },
        ZoneFacts::default(),
    );
    assert!(
        state
            .rows()
            .contains_key(&StateKey::Withdrawal(WithdrawalId {
                zone_id: ZONE_ID,
                index: 1
            }))
    );
    assert!(
        state
            .rows()
            .keys()
            .any(|key| matches!(key, StateKey::Batch(_)))
    );
    commit(
        &mut state,
        ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::ZERO,
                    withdrawals: vec![withdrawals[1].clone()],
                    remaining_queue: B256::ZERO,
                    outcomes: vec![WithdrawalOutcome::UserDelivered {
                        callback_deposits: vec![],
                    }],
                },
            )],
            ..ImportedFacts::default()
        },
        ZoneFacts::default(),
    );
    assert!(!state.rows().keys().any(|key| matches!(
        key,
        StateKey::Batch(_) | StateKey::Withdrawal(_) | StateKey::Fallback(_)
    )));
    validate(&state).unwrap();
}

#[test]
fn failed_deposit_pending_refund_claim_accounting() {
    let (mut state, deposit) = created_with_deposit();
    commit(
        &mut state,
        ImportedFacts::default(),
        ZoneFacts {
            deposits: vec![Deposit::Ordinary(deposit.clone())],
            outcomes: vec![DepositOutcome::Failed],
            block_hash: B256::repeat_byte(1),
            block_number: 1,
            finalization: Some(Finalization {
                block_number: 1,
                declared_count: 1,
                encrypted_senders: vec![Default::default()],
            }),
            ..ZoneFacts::default()
        },
    );
    submit_first_batch(&mut state);
    let wid = WithdrawalId {
        zone_id: ZONE_ID,
        index: 0,
    };
    let StateValue::Withdrawal(WithdrawalOwner::Finalized { data, .. }) =
        state.rows()[&StateKey::Withdrawal(wid)].clone()
    else {
        unreachable!()
    };
    commit(
        &mut state,
        ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::ZERO,
                    withdrawals: vec![data],
                    remaining_queue: B256::ZERO,
                    outcomes: vec![WithdrawalOutcome::FailedDepositPending { collected_fee: 0 }],
                },
            )],
            ..ImportedFacts::default()
        },
        ZoneFacts::default(),
    );
    assert!(
        state
            .rows()
            .keys()
            .any(|key| matches!(key, StateKey::PortalRefund(_)))
    );
    commit(
        &mut state,
        ImportedFacts {
            operations: vec![ImportedOperation::ClaimPortalRefund(RefundClaim {
                token: deposit.token,
                recipient: deposit.tempo_refund_recipient,
                amount: deposit.amount,
            })],
            ..ImportedFacts::default()
        },
        ZoneFacts::default(),
    );
    let StateValue::Token(token) = state.rows()[&StateKey::Token(deposit.token)] else {
        unreachable!()
    };
    assert_eq!(token.accounting.deposits, U256::ZERO);
    validate(&state).unwrap();
}

#[test]
fn every_ordinary_deposit_field_is_prefix_authenticated() {
    let (state, value) = created_with_deposit();
    let mut mutations = Vec::new();
    macro_rules! mutated {
        ($body:expr) => {{
            let mut changed = value.clone();
            $body(&mut changed);
            mutations.push(changed);
        }};
    }
    mutated!(|v: &mut OrdinaryDeposit| v.token = Address::repeat_byte(0xa1));
    mutated!(|v: &mut OrdinaryDeposit| v.sender = Address::repeat_byte(0xa2));
    mutated!(|v: &mut OrdinaryDeposit| v.amount += 1);
    mutated!(|v: &mut OrdinaryDeposit| v.tempo_refund_recipient = Address::repeat_byte(0xa3));
    mutated!(|v: &mut OrdinaryDeposit| v.key_index += U256::ONE);
    mutated!(|v: &mut OrdinaryDeposit| v.encrypted.ephemeral_pubkey_x = B256::repeat_byte(0xa4));
    mutated!(|v: &mut OrdinaryDeposit| v.encrypted.ephemeral_pubkey_y_parity ^= 1);
    mutated!(|v: &mut OrdinaryDeposit| v.encrypted.ciphertext[0] ^= 1);
    mutated!(|v: &mut OrdinaryDeposit| v.encrypted.nonce[0] ^= 1);
    mutated!(|v: &mut OrdinaryDeposit| v.encrypted.tag[0] ^= 1);
    for changed in mutations {
        assert_eq!(
            apply_zone(
                apply_imported(&state, &ImportedFacts::default()).unwrap(),
                &ZoneFacts {
                    deposits: vec![Deposit::Ordinary(changed)],
                    outcomes: vec![DepositOutcome::Minted],
                    ..ZoneFacts::default()
                },
            )
            .unwrap_err(),
            TransitionError::DepositPrefixMismatch
        );
    }
}

#[test]
fn every_submission_commitment_field_is_compared() {
    let (state, _) = finalized_user_state();
    let id = BatchId {
        zone_id: ZONE_ID,
        index: NonZeroU64::MIN,
    };
    let StateValue::Batch(BatchState::Finalized {
        boundary,
        queue_hash,
        ..
    }) = state.rows()[&StateKey::Batch(id)].clone()
    else {
        unreachable!()
    };
    let exact = BatchSubmission {
        tempo_block: boundary.tempo_block,
        previous_block: boundary.first_parent,
        next_block: boundary.final_block,
        previous_deposit: boundary.first_deposit,
        next_deposit: boundary.final_deposit,
        withdrawal_queue_hash: queue_hash,
        next_zone_height: U256::from(boundary.zone_height),
    };
    let mut mutations = Vec::new();
    macro_rules! changed {
        ($field:ident, $value:expr) => {{
            let mut input = exact.clone();
            input.$field = $value;
            mutations.push(input);
        }};
    }
    changed!(tempo_block, exact.tempo_block + 1);
    changed!(previous_block, B256::repeat_byte(0x11));
    changed!(next_block, B256::repeat_byte(0x12));
    changed!(
        previous_deposit,
        crate::kernel::Cursor {
            hash: B256::repeat_byte(0x13),
            number: 0
        }
    );
    changed!(
        next_deposit,
        crate::kernel::Cursor {
            hash: B256::repeat_byte(0x14),
            number: 0
        }
    );
    changed!(withdrawal_queue_hash, B256::repeat_byte(0x15));
    changed!(withdrawal_queue_hash, WITHDRAWAL_SENTINEL);
    changed!(next_zone_height, exact.next_zone_height + U256::ONE);
    for input in mutations {
        assert_eq!(
            apply_imported(
                &state,
                &ImportedFacts {
                    operations: vec![ImportedOperation::SubmitBatch(input)],
                    ..ImportedFacts::default()
                },
            )
            .unwrap_err(),
            TransitionError::CommitmentMismatch
        );
    }
}

#[test]
fn every_withdrawal_preimage_field_is_prefix_authenticated() {
    let (mut state, withdrawal) = finalized_user_state();
    submit_first_batch(&mut state);
    let mut mutations = Vec::new();
    macro_rules! changed {
        ($body:expr) => {{
            let mut value = withdrawal.clone();
            $body(&mut value);
            mutations.push(value);
        }};
    }
    changed!(|v: &mut crate::kernel::Withdrawal| v.token = Address::repeat_byte(0xb1));
    changed!(|v: &mut crate::kernel::Withdrawal| v.sender_tag = B256::repeat_byte(0xb2));
    changed!(|v: &mut crate::kernel::Withdrawal| v.to = Address::repeat_byte(0xb3));
    changed!(|v: &mut crate::kernel::Withdrawal| v.amount += 1);
    changed!(|v: &mut crate::kernel::Withdrawal| v.memo = B256::repeat_byte(0xb4));
    changed!(|v: &mut crate::kernel::Withdrawal| v.gas_limit += 1);
    changed!(|v: &mut crate::kernel::Withdrawal| v.fallback_nonce += 1);
    changed!(|v: &mut crate::kernel::Withdrawal| v.callback_data = Bytes::from_static(b"x"));
    changed!(|v: &mut crate::kernel::Withdrawal| v.encrypted_sender = Bytes::from_static(b"x"));
    for value in mutations {
        assert_eq!(
            apply_imported(
                &state,
                &ImportedFacts {
                    operations: vec![ImportedOperation::ProcessWithdrawals(
                        WithdrawalProcessing {
                            base_fee: U256::ZERO,
                            withdrawals: vec![value],
                            remaining_queue: B256::ZERO,
                            outcomes: vec![WithdrawalOutcome::UserDelivered {
                                callback_deposits: vec![]
                            }],
                        }
                    )],
                    ..ImportedFacts::default()
                },
            )
            .unwrap_err(),
            TransitionError::CommitmentMismatch
        );
    }
}

#[test]
fn batch_invariants_reject_state_relation_mutations() {
    type StateMutation = Box<dyn Fn(&mut BTreeMap<StateKey, StateValue>)>;

    let (finalized, _) = finalized_user_state();
    let batch_key = StateKey::Batch(BatchId {
        zone_id: ZONE_ID,
        index: NonZeroU64::MIN,
    });
    let mutations: Vec<StateMutation> = vec![
        // Phase/counter and complete unsubmitted suffix.
        Box::new(|rows| {
            let StateValue::Portal(PortalState::Created { settlement, .. }) =
                rows.get_mut(&StateKey::Portal).unwrap()
            else {
                unreachable!()
            };
            settlement.batch_index = 1;
        }),
        // Cursor well-formedness and prefix bounds.
        Box::new(move |rows| {
            let StateValue::Batch(BatchState::Finalized { boundary, .. }) =
                rows.get_mut(&batch_key).unwrap()
            else {
                unreachable!()
            };
            boundary.first_deposit.hash = B256::repeat_byte(0x91);
        }),
        // Open ranges terminate at the Zone accumulator.
        Box::new(move |rows| {
            let StateValue::Batch(BatchState::Finalized {
                first_withdrawal, ..
            }) = rows.get_mut(&batch_key).unwrap()
            else {
                unreachable!()
            };
            *first_withdrawal = 1;
        }),
        // Exact remaining members and queue commitment.
        Box::new(move |rows| {
            let StateValue::Batch(BatchState::Finalized { queue_hash, .. }) =
                rows.get_mut(&batch_key).unwrap()
            else {
                unreachable!()
            };
            *queue_hash = B256::repeat_byte(0x92);
        }),
        // Unsubmitted boundary chain and strict/equal tip advances.
        Box::new(move |rows| {
            let StateValue::Batch(BatchState::Finalized { boundary, .. }) =
                rows.get_mut(&batch_key).unwrap()
            else {
                unreachable!()
            };
            boundary.first_parent = B256::repeat_byte(0x93);
        }),
        // Last-batch/Zone accumulator binding.
        Box::new(|rows| {
            let StateValue::Zone(zone) = rows.get_mut(&StateKey::Zone).unwrap() else {
                unreachable!()
            };
            zone.withdrawal_queue_hash = B256::repeat_byte(0x94);
        }),
    ];
    for mutate in mutations {
        let mut rows = finalized.rows().clone();
        mutate(&mut rows);
        let changed = State::from_rows(rows).unwrap();
        assert_eq!(validate(&changed).unwrap_err().code, InvariantCode::Batch);
    }

    let mut submitted = finalized;
    submit_first_batch(&mut submitted);
    let submitted_mutations: Vec<(InvariantCode, StateMutation)> = vec![
        // Submitted ordinal bound/head-only processing.
        (
            InvariantCode::Batch,
            Box::new(move |rows| {
                let StateValue::Batch(BatchState::Submitted {
                    count,
                    next_ordinal,
                    ..
                }) = rows.get_mut(&batch_key).unwrap()
                else {
                    unreachable!()
                };
                *next_ordinal = *count + 1;
            }),
        ),
        // Exact queue slots.
        (
            InvariantCode::Ring,
            Box::new(move |rows| {
                let StateValue::Batch(BatchState::Submitted {
                    logical_queue_index,
                    ..
                }) = rows.get_mut(&batch_key).unwrap()
                else {
                    unreachable!()
                };
                *logical_queue_index += U256::ONE;
            }),
        ),
        // Latest submission/settlement binding (and direct equal-counter binding).
        (
            InvariantCode::Batch,
            Box::new(|rows| {
                let StateValue::Portal(PortalState::Created { settlement, .. }) =
                    rows.get_mut(&StateKey::Portal).unwrap()
                else {
                    unreachable!()
                };
                settlement.block_hash = B256::repeat_byte(0x95);
            }),
        ),
    ];
    for (code, mutate) in submitted_mutations {
        let mut rows = submitted.rows().clone();
        mutate(&mut rows);
        let changed = State::from_rows(rows).unwrap();
        assert_eq!(validate(&changed).unwrap_err().code, code);
    }
}
