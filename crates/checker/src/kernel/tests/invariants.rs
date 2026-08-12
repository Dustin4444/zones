//! Invariants tests.

use super::*;

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
fn invariant_validation_detects_pre_creation_rows() {
    let mut rows = State::awaiting(identity()).rows().clone();
    rows.insert(
        StateKey::Token(identity().initial_token),
        StateValue::Token(TokenState::pending()),
    );
    let state = State::from_rows(rows).unwrap();
    assert_eq!(
        validate(&state).unwrap_err().code,
        InvariantCode::PreCreationRows
    );
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
