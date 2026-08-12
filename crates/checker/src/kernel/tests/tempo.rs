//! Tempo tests.

use super::*;

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
