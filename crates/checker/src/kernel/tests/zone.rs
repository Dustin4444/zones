//! Zone tests.

use super::*;

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
                        operations: Vec::new(),
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
fn user_delivery_preserves_same_token_callback_deposit_accounting() {
    let (mut state, withdrawal) = finalized_user_state_with_gas_limit(1);
    submit_first_batch(&mut state);
    let callback = deposit();
    state = apply_imported(
        &state,
        &ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::ZERO,
                    withdrawals: vec![withdrawal],
                    remaining_queue: B256::ZERO,
                    outcomes: vec![WithdrawalOutcome::UserDelivered {
                        operations: vec![PortalCallbackOperation::AppendDeposit(callback.clone())],
                    }],
                },
            )],
            ..ImportedFacts::default()
        },
    )
    .unwrap()
    .into_state();

    let StateValue::Token(token) = state.rows()[&StateKey::Token(callback.token)] else {
        unreachable!()
    };
    assert_eq!(token.accounting.supply, U256::from(60));
    assert_eq!(token.accounting.withdrawals, U256::ZERO);
    assert_eq!(token.accounting.deposits, U256::from(callback.amount));
    validate(&state).unwrap();
}

#[test]
fn user_delivery_applies_callback_refund_claim_before_completing_withdrawal() {
    let (mut state, withdrawal) = finalized_user_state_with_gas_limit(1);
    submit_first_batch(&mut state);
    let token = identity().initial_token;
    let claim = RefundClaim {
        token,
        recipient: withdrawal.to,
        amount: 7,
    };
    let mut rows = state.rows().clone();
    let StateValue::Token(mut accounting) = rows[&StateKey::Token(token)].clone() else {
        unreachable!()
    };
    accounting.accounting.deposits = U256::from(claim.amount);
    rows.insert(StateKey::Token(token), StateValue::Token(accounting));
    rows.insert(
        StateKey::PortalRefund(PortalRefundId {
            token,
            recipient: claim.recipient,
            deposit: DepositId::new(identity().portal, 1).unwrap(),
        }),
        StateValue::PortalRefund(RefundCredit {
            amount: claim.amount,
        }),
    );
    state = State::from_rows(rows).unwrap();

    state = apply_imported(
        &state,
        &ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::ZERO,
                    withdrawals: vec![withdrawal],
                    remaining_queue: B256::ZERO,
                    outcomes: vec![WithdrawalOutcome::UserDelivered {
                        operations: vec![PortalCallbackOperation::ClaimRefund(claim)],
                    }],
                },
            )],
            ..ImportedFacts::default()
        },
    )
    .unwrap()
    .into_state();

    assert!(
        !state
            .rows()
            .keys()
            .any(|key| matches!(key, StateKey::PortalRefund(_)))
    );
    validate(&state).unwrap();
}

#[test]
fn callback_claims_refund_created_by_an_earlier_withdrawal_member() {
    let (mut state, deposit) = created_with_deposit();
    let mut rows = state.rows().clone();
    let StateValue::Token(mut token) = rows[&StateKey::Token(deposit.token)].clone() else {
        unreachable!()
    };
    token.accounting.supply = U256::from(100);
    rows.insert(StateKey::Token(deposit.token), StateValue::Token(token));
    state = State::from_rows(rows).unwrap();
    let user = UserWithdrawal {
        gas_limit: 1,
        ..user_withdrawal(0x50, 40)
    };
    commit(
        &mut state,
        ImportedFacts::default(),
        ZoneFacts {
            block_hash: B256::repeat_byte(1),
            block_number: 1,
            deposits: vec![Deposit::Ordinary(deposit.clone())],
            outcomes: vec![DepositOutcome::Failed],
            operations: vec![ZoneOperation::AcceptWithdrawal(user)],
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
            let StateValue::Withdrawal(WithdrawalOwner::Finalized { ref data, .. }) = state.rows()
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
    let claim = RefundClaim {
        token: deposit.token,
        recipient: deposit.tempo_refund_recipient,
        amount: deposit.amount,
    };
    state = apply_imported(
        &state,
        &ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::ZERO,
                    withdrawals,
                    remaining_queue: B256::ZERO,
                    outcomes: vec![
                        WithdrawalOutcome::FailedDepositPending { collected_fee: 0 },
                        WithdrawalOutcome::UserDelivered {
                            operations: vec![PortalCallbackOperation::ClaimRefund(claim)],
                        },
                    ],
                },
            )],
            ..ImportedFacts::default()
        },
    )
    .unwrap()
    .into_state();

    assert!(
        !state
            .rows()
            .keys()
            .any(|key| matches!(key, StateKey::PortalRefund(_)))
    );
    validate(&state).unwrap();
}

#[test]
fn callback_bounceback_gas_applies_to_later_withdrawal_members() {
    let mut state = funded_state();
    let user = UserWithdrawal {
        gas_limit: 1,
        ..user_withdrawal(0x50, 40)
    };
    commit(
        &mut state,
        ImportedFacts::default(),
        ZoneFacts {
            block_hash: B256::repeat_byte(1),
            block_number: 1,
            operations: vec![ZoneOperation::AcceptWithdrawal(user)],
            ..ZoneFacts::default()
        },
    );

    let failed = deposit();
    commit(
        &mut state,
        ImportedFacts {
            block_hash: B256::repeat_byte(2),
            block_number: 2,
            operations: vec![
                ImportedOperation::AppendDeposit(failed.clone()),
                ImportedOperation::AppendDeposit(failed.clone()),
            ],
        },
        ZoneFacts {
            block_hash: B256::repeat_byte(2),
            block_number: 2,
            deposits: vec![Deposit::Ordinary(failed.clone()), Deposit::Ordinary(failed)],
            outcomes: vec![DepositOutcome::Failed, DepositOutcome::Failed],
            finalization: Some(Finalization {
                block_number: 2,
                declared_count: 3,
                encrypted_senders: vec![Default::default(), Default::default(), Default::default()],
            }),
            ..ZoneFacts::default()
        },
    );
    submit_first_batch(&mut state);

    let withdrawals = (0..3)
        .map(|index| {
            let StateValue::Withdrawal(WithdrawalOwner::Finalized { ref data, .. }) = state.rows()
                [&StateKey::Withdrawal(WithdrawalId {
                    zone_id: ZONE_ID,
                    index,
                })]
            else {
                unreachable!()
            };
            data.clone()
        })
        .collect();
    let bounceback_gas = 10;
    state = apply_imported(
        &state,
        &ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::from(1_000_000_000_000u64),
                    withdrawals,
                    remaining_queue: B256::ZERO,
                    outcomes: vec![
                        WithdrawalOutcome::UserDelivered {
                            operations: vec![PortalCallbackOperation::UpdateBouncebackGas(
                                bounceback_gas,
                            )],
                        },
                        WithdrawalOutcome::FailedDepositPaid {
                            collected_fee: u128::from(bounceback_gas),
                        },
                        WithdrawalOutcome::FailedDepositPending {
                            collected_fee: u128::from(bounceback_gas),
                        },
                    ],
                },
            )],
            ..ImportedFacts::default()
        },
    )
    .unwrap()
    .into_state();

    assert!(matches!(
        state.rows()[&StateKey::Portal],
        StateValue::Portal(PortalState::Created {
            bounceback_gas: 10,
            ..
        })
    ));
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
    let suffix = crate::kernel::derivation::withdrawal_hash(&withdrawals[1], WITHDRAWAL_TERMINATOR);
    commit(
        &mut state,
        ImportedFacts {
            operations: vec![ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::ZERO,
                    withdrawals: vec![withdrawals[0].clone()],
                    remaining_queue: suffix,
                    outcomes: vec![WithdrawalOutcome::UserDelivered { operations: vec![] }],
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
                    outcomes: vec![WithdrawalOutcome::UserDelivered { operations: vec![] }],
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
                            outcomes: vec![WithdrawalOutcome::UserDelivered { operations: vec![] }],
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
