use super::*;
use crate::store::history::BlockWriteResult;

#[test]
fn raw_test_commits_reject_duplicate_keys() {
    let initialization = initialization(BootstrapPhase::ZoneReplay);
    let token = Address::repeat_byte(0x72);
    assert!(matches!(
        BlockCommit::from_mutations(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            tip(1, 0x73),
            tip(11, 0x74),
            vec![
                ModelMutation::put(ModelKey::Token(token), token_value(1)).unwrap(),
                ModelMutation::put(ModelKey::Token(token), token_value(2)).unwrap(),
            ],
        ),
        Err(StoreError::DuplicateMutation(found)) if found == ModelKey::Token(token)
    ));
}

#[test]
fn block_commit_rejects_a_settlement_incoherent_with_its_child_cut() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let parent = store.load_current().unwrap();
    let child_zone_hash = hash(0xa1);

    let beyond_tempo = terminal_settlement_rows(12, 1, child_zone_hash)
        .into_iter()
        .map(|(key, value)| ModelMutation::put(key, value).unwrap())
        .collect();
    assert!(matches!(
        store.apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0xa1,
            0xa2,
            beyond_tempo,
        )),
        Err(StoreError::PortalSettlementBeyondImportedTempoTip {
            settlement_height: 12,
            imported_tip,
        }) if imported_tip == tip(11, 0xa2)
    ));
    assert_eq!(store.load_current().unwrap(), parent);

    let wrong_child_hash = terminal_settlement_rows(11, 1, hash(0xaf))
        .into_iter()
        .map(|(key, value)| ModelMutation::put(key, value).unwrap())
        .collect();
    assert!(matches!(
        store.apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0xa1,
            0xa2,
            wrong_child_hash,
        )),
        Err(StoreError::PortalSettlementCanonicalConflict {
            height: 1,
            settlement_hash,
            canonical_hash,
        }) if settlement_hash == hash(0xaf) && canonical_hash == child_zone_hash
    ));
    assert_eq!(store.load_current().unwrap(), parent);

    let tx = store.database().tx().unwrap();
    assert_eq!(tx.entries::<CheckerCanonical>().unwrap(), 1);
    assert_eq!(tx.entries::<CheckerChangesets>().unwrap(), 0);
    tx.commit().unwrap();
}

#[test]
fn block_commit_is_atomic_journals_first_images_and_reconstructs_deletion() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let token = Address::repeat_byte(0x77);
    let zone0 = initialization.verified_zone_tip;
    let tempo0 = initialization.imported_tempo_tip;
    let zone_config = initialization.model.zone().config();
    let insert = block(
        zone0,
        tempo0,
        0x11,
        0x61,
        vec![
            ModelMutation::put(
                ModelKey::ZoneConfig,
                ModelValue::ZoneConfig {
                    tempo_gas_rate: zone_config.tempo_gas_rate(),
                    max_withdrawals_per_block: zone_config.max_withdrawals_per_block(),
                },
            )
            .unwrap(),
            ModelMutation::put(ModelKey::Token(token), token_value(1)).unwrap(),
        ],
    );

    // One block row, one before-image/model pair, canonical, bootstrap, and two tips.
    let parent_snapshot = store.load_current().unwrap();
    for write in 1..=7 {
        assert!(matches!(
            store.apply_block_aborting_after(insert.clone(), write),
            Err(StoreError::InjectedWriteFailure)
        ));
        let unchanged = store.load_current().unwrap();
        assert_eq!(unchanged, parent_snapshot);
        assert_eq!(unchanged.verified_zone_tip, zone0);
        assert_eq!(unchanged.imported_tempo_tip, tempo0);
        assert!(!unchanged.model_rows.contains_key(&ModelKey::Token(token)));
        let tx = store.database().tx().unwrap();
        assert_eq!(tx.entries::<CheckerCanonical>().unwrap(), 1);
        assert_eq!(tx.entries::<CheckerChangesets>().unwrap(), 0);
        tx.commit().unwrap();
    }

    let BlockWriteResult::Applied(metrics) = store.apply_block_measured(insert.clone()).unwrap()
    else {
        panic!("first insert must apply");
    };
    assert_eq!(
        metrics.model_rows,
        store.load_current().unwrap().model_rows.len()
    );
    assert!(metrics.changeset_bytes > 0);
    assert_eq!(
        store.apply_block(insert).unwrap(),
        WriteOutcome::AlreadyApplied
    );
    let zone1 = tip(1, 0x11);
    let tempo1 = tip(11, 0x61);
    let update = block(
        zone1,
        tempo1,
        0x12,
        0x62,
        vec![ModelMutation::put(ModelKey::Token(token), token_value(2)).unwrap()],
    );
    assert_eq!(store.apply_block(update).unwrap(), WriteOutcome::Applied);
    let zone2 = tip(2, 0x12);
    let tempo2 = tip(12, 0x62);
    let delete = block(
        zone2,
        tempo2,
        0x13,
        0x63,
        vec![ModelMutation::delete(ModelKey::Token(token))],
    );
    assert_eq!(store.apply_block(delete).unwrap(), WriteOutcome::Applied);

    let tx = store.database().tx().unwrap();
    assert!(matches!(
        tx.get::<CheckerChangesets>(ChangesetKey::new(1, zone1.hash, 1))
            .unwrap(),
        Some(BeforeImage::Model { key: ModelKey::Token(found), value: None }) if found == token
    ));
    assert!(matches!(
        tx.get::<CheckerChangesets>(ChangesetKey::new(2, zone2.hash, 1))
            .unwrap(),
        Some(BeforeImage::Model { value: Some(value), .. }) if *value == token_value(1)
    ));
    assert!(matches!(
        tx.get::<CheckerChangesets>(ChangesetKey::new(3, hash(0x13), 1))
            .unwrap(),
        Some(BeforeImage::Model { value: Some(value), .. }) if *value == token_value(2)
    ));
    tx.commit().unwrap();

    assert_eq!(
        store
            .reconstruct(2)
            .unwrap()
            .model_rows
            .get(&ModelKey::Token(token)),
        Some(&token_value(2))
    );
    assert_eq!(
        store
            .reconstruct(1)
            .unwrap()
            .model_rows
            .get(&ModelKey::Token(token)),
        Some(&token_value(1))
    );
    assert!(
        !store
            .reconstruct(0)
            .unwrap()
            .model_rows
            .contains_key(&ModelKey::Token(token))
    );
    store.check_consistency().unwrap();

    let expected = store.load_current().unwrap();
    assert_eq!(
        expected.bootstrap,
        BootstrapState::zone_replay(expected.imported_tempo_tip)
    );
    drop(store);

    let reopened = CheckerStore::open(directory.path(), initialization).unwrap();
    assert_eq!(reopened.load_current().unwrap(), expected);
    let enter_live = reopened
        .enter_live(expected.bootstrap, expected.imported_tempo_tip)
        .unwrap();
    assert_eq!(
        reopened.apply_bootstrap(enter_live).unwrap(),
        WriteOutcome::Applied
    );
    assert_eq!(
        reopened.load_current().unwrap().bootstrap,
        BootstrapState::live()
    );
}

#[test]
fn historical_snapshots_match_fresh_lifecycle_replay_bytes() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let token = initialization.identity.portal_identity().initial_token();
    let deposit = OrdinaryDeposit::new(
        token,
        Address::repeat_byte(0xd1),
        17,
        Address::repeat_byte(0xd2),
        U256::from(3),
        DepositPayload::new(
            hash(0xd3),
            CompressedYParity::Even,
            FixedBytes::repeat_byte(0xd4),
            FixedBytes::repeat_byte(0xd5),
            FixedBytes::repeat_byte(0xd6),
        ),
    );
    let member = DepositQueueMember::Ordinary(deposit.clone());
    let transitions = [
        (
            imported_block(
                11,
                U256::ZERO,
                vec![ImportedTempoOperation::OrdinaryDepositAppended(deposit)],
            ),
            zone_block(hash(0xd7), 1, Default::default()),
        ),
        (
            imported_block(12, U256::ZERO, Vec::new()),
            zone_block(
                hash(0xd8),
                2,
                deposit_prefix(
                    Vec::new(),
                    vec![member],
                    vec![AuthenticatedDepositOutcome::OrdinaryMinted {
                        recipient: Address::repeat_byte(0xd9),
                        memo: hash(0xda),
                    }],
                ),
            ),
        ),
    ];
    let zone_tips = [initialization.verified_zone_tip, tip(1, 0xd7), tip(2, 0xd8)];
    let tempo_tips = [
        initialization.imported_tempo_tip,
        tip(11, 0xe1),
        tip(12, 0xe2),
    ];

    let mut persisted_parent = initialization.model.clone();
    for (index, (imported, zone)) in transitions.iter().enumerate() {
        let completed = ModelTransition::new(&persisted_parent)
            .apply_imported_tempo_block(imported)
            .unwrap()
            .apply_zone_block(zone)
            .unwrap();
        let update = completed.into_state_update();
        let commit = store
            .block_commit(
                zone_tips[index],
                tempo_tips[index],
                zone_tips[index + 1],
                tempo_tips[index + 1],
                &update,
            )
            .unwrap();
        update.apply_to_current_parent(&mut persisted_parent);
        assert_eq!(store.apply_block(commit).unwrap(), WriteOutcome::Applied);
    }

    let mut fresh_parent = initialization.model;
    let mut fresh_states = vec![fresh_parent.clone()];
    for (imported, zone) in &transitions {
        let update = ModelTransition::new(&fresh_parent)
            .apply_imported_tempo_block(imported)
            .unwrap()
            .apply_zone_block(zone)
            .unwrap()
            .into_state_update();
        update.apply_to_current_parent(&mut fresh_parent);
        fresh_states.push(fresh_parent.clone());
    }

    let pending_key = ModelKey::PendingDeposit(1);
    for target in 0..=2 {
        let historical = store.reconstruct(target).unwrap();
        let fresh_rows = flatten_model(&fresh_states[target as usize]).unwrap();
        assert_eq!(historical.verified_zone_tip, zone_tips[target as usize]);
        assert_eq!(historical.imported_tempo_tip, tempo_tips[target as usize]);
        assert_eq!(historical.model, fresh_states[target as usize]);
        assert_eq!(
            model_bytes(&historical.model_rows),
            model_bytes(&fresh_rows)
        );
    }
    assert!(
        store
            .reconstruct(1)
            .unwrap()
            .model_rows
            .contains_key(&pending_key)
    );
    assert!(
        !store
            .reconstruct(2)
            .unwrap()
            .model_rows
            .contains_key(&pending_key)
    );
}

#[test]
fn missing_changeset_and_canonical_conflict_fail_reconstruction() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let token = Address::repeat_byte(0x71);
    let commit = block(
        initialization.verified_zone_tip,
        initialization.imported_tempo_tip,
        0x21,
        0x22,
        vec![ModelMutation::put(ModelKey::Token(token), token_value(1)).unwrap()],
    );
    store.apply_block(commit).unwrap();
    let tx = store.database().tx_mut().unwrap();
    assert!(
        tx.delete::<CheckerChangesets>(ChangesetKey::new(1, hash(0x21), 1), None)
            .unwrap()
    );
    tx.commit().unwrap();
    assert!(matches!(
        store.reconstruct(0),
        Err(StoreError::InvalidChangeset { .. })
    ));

    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    store
        .apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0x31,
            0x32,
            vec![ModelMutation::put(ModelKey::Token(token), token_value(1)).unwrap()],
        ))
        .unwrap();
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerCanonical>(1, CanonicalHash::new(hash(0xfe)))
        .unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        store.reconstruct(0),
        Err(StoreError::CanonicalConflict { height: 1, .. })
    ));
}

#[test]
fn surplus_changeset_row_is_rejected_by_history_diagnostics_after_restart() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let token = Address::repeat_byte(0x75);
    store
        .apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0x76,
            0x77,
            vec![ModelMutation::put(ModelKey::Token(token), token_value(1)).unwrap()],
        ))
        .unwrap();

    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerChangesets>(
        ChangesetKey::new(1, hash(0x76), 2),
        BeforeImage::Model {
            key: ModelKey::Token(Address::repeat_byte(0x78)),
            value: None,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    drop(store);

    // Normal restart validates decoding and the authoritative cut without
    // replaying from genesis. Explicit history diagnostics validate grouping.
    let reopened = CheckerStore::open(directory.path(), initialization).unwrap();
    assert!(matches!(
        reopened.reconstruct(0),
        Err(StoreError::InvalidChangeset {
            reason: "changeset has surplus mutation rows",
            ..
        })
    ));
    assert!(matches!(
        reopened.check_consistency(),
        Err(StoreError::InvalidChangeset {
            reason: "changeset has surplus mutation rows",
            ..
        })
    ));
}

#[test]
fn child_value_cannot_masquerade_as_its_own_before_image() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let token = Address::repeat_byte(0x79);
    let value = token_value(1);
    let commit = block(
        initialization.verified_zone_tip,
        initialization.imported_tempo_tip,
        0x7a,
        0x7b,
        vec![ModelMutation::put(ModelKey::Token(token), value.clone()).unwrap()],
    );
    store.apply_block(commit.clone()).unwrap();

    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerChangesets>(
        ChangesetKey::new(1, hash(0x7a), 1),
        BeforeImage::Model {
            key: ModelKey::Token(token),
            value: Some(Box::new(value)),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    for result in [store.reconstruct(0).map(|_| ()), store.check_consistency()] {
        assert!(matches!(
            result,
            Err(StoreError::InvalidChangeset {
                reason: "before-image equals the child model row",
                ..
            })
        ));
    }
    assert!(matches!(
        store.apply_block(commit),
        Err(StoreError::InvalidChangeset {
            reason: "duplicate replay before-image equals the child model row",
            ..
        })
    ));
}

#[test]
fn duplicate_replay_rejects_an_unordered_embedded_journal() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let first = Address::repeat_byte(0x81);
    let second = Address::repeat_byte(0x82);
    let commit = block(
        initialization.verified_zone_tip,
        initialization.imported_tempo_tip,
        0x83,
        0x84,
        vec![
            ModelMutation::put(ModelKey::Token(first), token_value(1)).unwrap(),
            ModelMutation::put(ModelKey::Token(second), token_value(2)).unwrap(),
        ],
    );
    store.apply_block(commit.clone()).unwrap();

    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerChangesets>(
        ChangesetKey::new(1, hash(0x83), 1),
        BeforeImage::Model {
            key: ModelKey::Token(second),
            value: None,
        },
    )
    .unwrap();
    tx.put::<CheckerChangesets>(
        ChangesetKey::new(1, hash(0x83), 2),
        BeforeImage::Model {
            key: ModelKey::Token(first),
            value: None,
        },
    )
    .unwrap();
    tx.commit().unwrap();

    assert!(matches!(
        store.apply_block(commit),
        Err(StoreError::InvalidChangeset { .. })
    ));
}

#[test]
fn reconstruction_rejects_a_corrupted_intermediate_model_boundary() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let token = Address::repeat_byte(0x91);
    let recipient = Address::repeat_byte(0x92);
    let refund_key = ModelKey::PortalRefundCredit {
        token,
        recipient,
        origin: 1,
    };
    store
        .apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0x93,
            0x94,
            vec![ModelMutation::put(ModelKey::Token(token), token_value(0)).unwrap()],
        ))
        .unwrap();
    store
        .apply_block(block(
            tip(1, 0x93),
            tip(11, 0x94),
            0x95,
            0x96,
            vec![
                ModelMutation::put(
                    ModelKey::PortalDepositCursor,
                    ModelValue::PortalDepositCursor(CursorValue {
                        hash: hash(0x95),
                        number: 1,
                    }),
                )
                .unwrap(),
                ModelMutation::put(
                    ModelKey::ZoneProcessedDepositCursor,
                    ModelValue::ZoneProcessedDepositCursor(CursorValue {
                        hash: hash(0x95),
                        number: 1,
                    }),
                )
                .unwrap(),
                ModelMutation::put(
                    ModelKey::Token(token),
                    token_value_with_liabilities(0, 9, 0),
                )
                .unwrap(),
                ModelMutation::put(refund_key, ModelValue::PortalRefundCredit(9)).unwrap(),
            ],
        ))
        .unwrap();
    store
        .apply_block(block(
            tip(2, 0x95),
            tip(12, 0x96),
            0x97,
            0x98,
            vec![
                ModelMutation::delete(ModelKey::Token(token)),
                ModelMutation::delete(refund_key),
            ],
        ))
        .unwrap();

    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerChangesets>(
        ChangesetKey::new(3, hash(0x97), 1),
        BeforeImage::Model {
            key: ModelKey::Token(token),
            value: None,
        },
    )
    .unwrap();
    tx.commit().unwrap();

    for result in [store.reconstruct(0).map(|_| ()), store.check_consistency()] {
        assert!(matches!(
            result,
            Err(StoreError::InvalidChangeset {
                reason: "before-image equals the child model row",
                ..
            })
        ));
    }
}

#[test]
fn reconstruction_rejects_a_tip_incoherent_but_internally_valid_boundary() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let zone_one = tip(1, 0xb1);
    let tempo_one = tip(11, 0xb2);
    let terminal = terminal_settlement_rows(11, 1, zone_one.hash)
        .into_iter()
        .map(|(key, value)| ModelMutation::put(key, value).unwrap())
        .collect();
    store
        .apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0xb1,
            0xb2,
            terminal,
        ))
        .unwrap();

    let initial_rows = flatten_model(&initialization.model).unwrap();
    let reset = [ModelKey::PortalSettlement, ModelKey::ZoneBatchAccumulator]
        .into_iter()
        .map(|key| ModelMutation::put(key, initial_rows[&key].clone()).unwrap())
        .collect();
    store
        .apply_block(block(zone_one, tempo_one, 0xb3, 0xb4, reset))
        .unwrap();

    // Preserve a fully valid terminal model boundary while making its Portal
    // settlement claim one Tempo block newer than the restored imported tip.
    let [(settlement_key, corrupt_settlement), _] = terminal_settlement_rows(12, 1, zone_one.hash);
    assert_eq!(settlement_key, ModelKey::PortalSettlement);
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerChangesets>(
        ChangesetKey::new(2, hash(0xb3), 1),
        BeforeImage::Model {
            key: settlement_key,
            value: Some(Box::new(corrupt_settlement)),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    for result in [store.reconstruct(1).map(|_| ()), store.check_consistency()] {
        assert!(matches!(
            result,
            Err(StoreError::PortalSettlementBeyondImportedTempoTip {
                settlement_height: 12,
                imported_tip,
            }) if imported_tip == tempo_one
        ));
    }
}
