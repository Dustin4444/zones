use super::*;

#[test]
fn first_l1_bootstrap_commit_requires_the_exact_creation_block_and_transition() {
    let directory = TempDir::new().unwrap();
    let identity = identity();
    let creation_parent = tip(4, 0x4f);
    let initialization =
        Initialization::fresh(identity, FreshBootstrap::L1Replay { creation_parent });
    let update = ModelTransition::new(&initialization.model)
        .apply_imported_tempo_block(&imported_block(5, U256::ZERO, Vec::new()))
        .unwrap()
        .into_bootstrap_state_update();
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
    let wrong_creation = tip(5, 0x51);

    assert!(matches!(
        store.bootstrap_l1_commit(
            initialization.bootstrap,
            creation_parent,
            wrong_creation,
            &update,
        ),
        Err(StoreError::L1ReplayFirstBlockMismatch { expected, actual })
            if expected == identity.portal_creation_block() && actual == wrong_creation
    ));
    assert_eq!(
        store.load_current().unwrap().bootstrap,
        initialization.bootstrap
    );

    let parent = store.load_current().unwrap();
    let creation = identity.portal_creation_block();
    let incomplete = store
        .bootstrap_l1_commit(initialization.bootstrap, creation_parent, creation, &update)
        .unwrap();
    assert!(matches!(
        store.apply_bootstrap(incomplete),
        Err(StoreError::PortalCreationProgressMismatch {
            creation: actual_creation,
            imported_tip,
            portal_created: false,
        }) if actual_creation == creation && imported_tip == creation
    ));
    assert_eq!(
        store.load_current().unwrap(),
        parent,
        "an incomplete creation transition must abort model and cursor writes together"
    );
}

#[test]
fn typed_l1_bootstrap_update_and_cursor_commit_together() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::L1Replay);
    let token = initialization.identity.portal_identity().initial_token();
    let deposit = OrdinaryDeposit::new(
        token,
        Address::repeat_byte(0xa1),
        7,
        Address::repeat_byte(0xa2),
        U256::from(11),
        DepositPayload::new(
            hash(0xa3),
            CompressedYParity::Even,
            FixedBytes::repeat_byte(0xa4),
            FixedBytes::repeat_byte(0xa5),
            FixedBytes::repeat_byte(0xa6),
        ),
    );
    let imported = imported_block(
        11,
        U256::ZERO,
        vec![ImportedTempoOperation::OrdinaryDepositAppended(deposit)],
    );
    let update = ModelTransition::new(&initialization.model)
        .apply_imported_tempo_block(&imported)
        .unwrap()
        .into_bootstrap_state_update();
    let next_tempo = tip(11, 0xa7);
    let commit = store
        .bootstrap_l1_commit(
            initialization.bootstrap,
            initialization.imported_tempo_tip,
            next_tempo,
            &update,
        )
        .unwrap();

    assert_eq!(
        store.apply_bootstrap(commit.clone()).unwrap(),
        WriteOutcome::Applied
    );
    let current = store.load_current().unwrap();
    assert_eq!(current.imported_tempo_tip, next_tempo);
    assert_eq!(
        current.bootstrap,
        BootstrapState::l1_replay(Some(next_tempo))
    );
    assert!(
        current
            .model_rows
            .contains_key(&ModelKey::PendingDeposit(1))
    );
    assert_eq!(
        store.apply_bootstrap(commit).unwrap(),
        WriteOutcome::AlreadyApplied,
        "a resumed L1 cursor must not apply the same authenticated block twice"
    );
    assert_eq!(store.load_current().unwrap(), current);
}

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
            vec![ModelMutation::put(ModelKey::Token(token), pending_token_value()).unwrap()],
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
        Some(&pending_token_value())
    );
    let handoff = ZoneGenesisStateUpdate::from_portal_cut(&applied.model);
    let transition = store
        .enter_zone_replay(applied.bootstrap, next_tempo, &handoff)
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
fn zone_genesis_handoff_promotes_tokens_and_phase_atomically_and_idempotently() {
    let directory = TempDir::new().unwrap();
    let (initialization, token) = initialization_with_pending_token(BootstrapPhase::L1Replay);
    let store = CheckerStore::open(directory.path(), initialization).unwrap();
    let parent = store.load_current().unwrap();
    let pending = parent.model.token(token).unwrap().clone();
    let handoff = ZoneGenesisStateUpdate::from_portal_cut(&parent.model);
    let commit = store
        .enter_zone_replay(parent.bootstrap, parent.imported_tempo_tip, &handoff)
        .unwrap();

    // One token row and both bootstrap metadata rows form one transaction.
    for write in 1..=3 {
        assert!(matches!(
            store.apply_bootstrap_aborting_after(commit.clone(), write),
            Err(StoreError::InjectedWriteFailure)
        ));
        assert_eq!(store.load_current().unwrap(), parent);
    }

    assert_eq!(
        store.apply_bootstrap(commit.clone()).unwrap(),
        WriteOutcome::Applied
    );
    let applied = store.load_current().unwrap();
    assert_eq!(
        applied.bootstrap,
        BootstrapState::zone_replay(parent.imported_tempo_tip)
    );
    let enabled = applied.model.token(token).unwrap();
    assert_eq!(enabled.phase(), TokenPhase::ZoneEnabled);
    assert_eq!(enabled.accounting(), pending.accounting());
    assert_eq!(
        applied.model.pending_deposits(),
        parent.model.pending_deposits(),
        "genesis enablement must not rewrite deposit ownership"
    );

    assert_eq!(
        store.apply_bootstrap(commit).unwrap(),
        WriteOutcome::AlreadyApplied
    );
    assert_eq!(store.load_current().unwrap(), applied);
}

#[test]
fn zone_genesis_handoff_rejects_an_incomplete_update_atomically() {
    let directory = TempDir::new().unwrap();
    let (initialization, _extra_token) =
        initialization_with_pending_token(BootstrapPhase::L1Replay);
    let store = CheckerStore::open(directory.path(), initialization).unwrap();
    let parent = store.load_current().unwrap();
    let first_pending_token = parent.model.portal().identity().initial_token();
    let incomplete = ZoneGenesisStateUpdate::from_portal_cut(&ModelState::awaiting_creation(
        parent.model.portal().identity(),
    ));
    let commit = store
        .enter_zone_replay(parent.bootstrap, parent.imported_tempo_tip, &incomplete)
        .unwrap();

    assert!(matches!(
        store.apply_bootstrap(commit),
        Err(StoreError::BootstrapTokenPhaseMismatch {
            bootstrap: BootstrapState::ZoneReplay { .. },
            token: actual,
        }) if actual == first_pending_token
    ));
    assert_eq!(
        store.load_current().unwrap(),
        parent,
        "an incomplete genesis handoff must abort phase and model writes together"
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
