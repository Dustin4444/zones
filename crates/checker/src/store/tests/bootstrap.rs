use super::*;

#[test]
fn bootstrap_model_and_cursor_commit_or_abort_together() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::L1Replay);
    let token = Address::repeat_byte(0xb1);
    let next_tempo = tip(11, 0xb2);
    assert!(matches!(
        store.bootstrap_commit_from_mutations(
            BootstrapState::l1_replay(Some(tip(9, 0xb0))),
            initialization.imported_tempo_tip,
            next_tempo,
            Vec::new(),
        ),
        Err(StoreError::InvalidBootstrapProgress(_))
    ));
    assert!(matches!(
        store.apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0xb3,
            0xb4,
            Vec::new(),
        )),
        Err(StoreError::InvalidBootstrapProgress(_))
    ));
    let commit = store
        .bootstrap_commit_from_mutations(
            initialization.bootstrap,
            initialization.imported_tempo_tip,
            next_tempo,
            vec![ModelMutation::put(ModelKey::Token(token), token_value(9)).unwrap()],
        )
        .unwrap();
    // Model row, bootstrap cursor, and imported tip are one commit.
    for write in 1..=3 {
        assert!(matches!(
            store.apply_bootstrap_aborting_after(commit.clone(), write),
            Err(StoreError::InjectedWriteFailure)
        ));
        let unchanged = store.load_current().unwrap();
        assert_eq!(
            unchanged.imported_tempo_tip,
            initialization.imported_tempo_tip
        );
        assert_eq!(unchanged.bootstrap, initialization.bootstrap);
        assert!(!unchanged.model_rows.contains_key(&ModelKey::Token(token)));
    }

    assert_eq!(
        store.apply_bootstrap(commit).unwrap(),
        WriteOutcome::Applied
    );
    let applied = store.load_current().unwrap();
    assert_eq!(applied.imported_tempo_tip, next_tempo);
    assert_eq!(
        applied.model_rows.get(&ModelKey::Token(token)),
        Some(&token_value(9))
    );
    let transition = store
        .enter_zone_replay(applied.bootstrap, next_tempo)
        .unwrap();
    store.apply_bootstrap(transition).unwrap();
    assert!(
        store
            .bootstrap_commit_from_mutations(
                store.load_current().unwrap().bootstrap,
                next_tempo,
                tip(12, 0xb3),
                Vec::new(),
            )
            .is_err()
    );
}

#[test]
fn bootstrap_commit_rejects_a_settlement_beyond_its_next_tempo_tip() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::L1Replay);
    let parent = store.load_current().unwrap();
    let next_tempo = tip(11, 0xc1);
    let mutations = terminal_settlement_rows(12, 10, hash(0xc2))
        .into_iter()
        .map(|(key, value)| ModelMutation::put(key, value).unwrap())
        .collect();
    let commit = store
        .bootstrap_commit_from_mutations(
            initialization.bootstrap,
            initialization.imported_tempo_tip,
            next_tempo,
            mutations,
        )
        .unwrap();

    assert!(matches!(
        store.apply_bootstrap(commit),
        Err(StoreError::PortalSettlementBeyondImportedTempoTip {
            settlement_height: 12,
            imported_tip,
        }) if imported_tip == next_tempo
    ));
    assert_eq!(store.load_current().unwrap(), parent);
}
