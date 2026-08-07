use alloy_primitives::{Address, B256, U256, address, b256, fixed_bytes};

use std::{collections::BTreeMap, num::NonZeroU64};

use crate::{
    BatchId, DepositId, DepositOutcome, ExpectedEffect, FallbackId, ImportedFacts,
    ImportedOperation, InboxRefundId, ModelError, OrdinaryDeposit, PortalIdentity, PortalRefundId,
    PortalState, State, StateKey, StateValue, TokenEnable, TokenPhase, WithdrawalId, ZoneFacts,
    apply_imported, apply_zone,
    commitments::{ordinary_deposit_hash, portal_address},
    facts::DepositPayload,
    invariants::{InvariantCode, validate},
    state::{Overlay, TokenAccounting, TokenState},
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
        },
    )
    .unwrap();
    let candidate = apply_zone(
        imported,
        &ZoneFacts {
            enabled_tokens: vec![enable(identity().initial_token)],
            ..ZoneFacts::default()
        },
    )
    .unwrap();
    let mut state = parent;
    state.apply(&candidate.delta).unwrap();
    (state, deposit)
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
            deposit: crate::state::Cursor::ZERO,
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
    let withdrawal = WithdrawalId { zone_id: ZONE_ID, index: 0 };
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
    };
    let mut wrong_identity = identity();
    wrong_identity.zone_id += 1;
    assert_eq!(
        apply_imported(&parent, &create(wrong_identity, identity().initial_token)).unwrap_err(),
        ModelError::PortalIdentityMismatch
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
        ModelError::PortalAddressMismatch
    );
    assert_eq!(
        apply_imported(&parent, &create(identity(), Address::repeat_byte(0xbb))).unwrap_err(),
        ModelError::InitialTokenMismatch
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
            }
        )
        .unwrap_err(),
        ModelError::PortalAlreadyCreated
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
            }
        )
        .unwrap_err(),
        ModelError::PortalNotCreated
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
            }
        )
        .unwrap_err(),
        ModelError::TokenAlreadyEnabled(identity().initial_token)
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
        },
    )
    .unwrap();
    assert_eq!(
        apply_zone(imported, &ZoneFacts::default()).unwrap_err(),
        ModelError::TokenEnableMismatch
    );
}

#[test]
fn ordinary_deposit_commitment_matches_independent_literal_vector() {
    assert_eq!(
        ordinary_deposit_hash(&deposit(), B256::ZERO),
        b256!("89982eeee3ca64954daa0322b331f17efd85a433564bfdb4938c0ab087663a5d")
    );
}

#[test]
fn ordinary_deposit_append_and_mint_close_accounting() {
    let (state, deposit) = created_with_deposit();
    let candidate = apply_zone(
        apply_imported(&state, &ImportedFacts::default()).unwrap(),
        &ZoneFacts {
            deposits: vec![deposit.clone()],
            outcomes: vec![DepositOutcome::Minted],
            ..ZoneFacts::default()
        },
    )
    .unwrap();
    assert_eq!(candidate.expected_effects.len(), 1);
    assert!(matches!(
        candidate.expected_effects[0],
        ExpectedEffect::DepositProcessed { amount: 1_000, .. }
    ));
    let mut after = state;
    after.apply(&candidate.delta).unwrap();
    let Some(StateValue::Token(token)) = after.rows().get(&StateKey::Token(deposit.token)) else {
        panic!("token row missing")
    };
    assert_eq!(token.accounting.supply, U256::from(1_000));
    assert_eq!(token.accounting.deposits, U256::ZERO);
    assert_eq!(
        candidate.expected_state.collateral_requirement,
        U256::from(1_000)
    );
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
            deposits: vec![mutated],
            outcomes: vec![DepositOutcome::Failed],
            ..ZoneFacts::default()
        },
    )
    .unwrap_err();
    assert_eq!(error, ModelError::DepositPrefixMismatch);

    let candidate = apply_zone(
        apply_imported(&state, &ImportedFacts::default()).unwrap(),
        &ZoneFacts {
            deposits: vec![deposit],
            outcomes: vec![DepositOutcome::Failed],
            ..ZoneFacts::default()
        },
    )
    .unwrap();
    assert!(matches!(
        candidate.expected_effects[0],
        ExpectedEffect::DepositFailed { .. }
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
                deposits: vec![deposit],
                outcomes: vec![],
                ..ZoneFacts::default()
            }
        )
        .unwrap_err(),
        ModelError::DepositOutcomeCountMismatch
    );
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
            }
        )
        .unwrap_err(),
        ModelError::ZeroRefundRecipient
    );
}
