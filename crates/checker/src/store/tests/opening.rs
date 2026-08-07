use super::*;
use crate::store::model_state::assemble_model;

#[test]
fn fresh_initialization_derives_literal_genesis_and_awaiting_creation_state() {
    let identity = identity();
    let creation_parent = tip(4, 0x41);
    let l1 = Initialization::fresh(identity, FreshBootstrap::L1Replay { creation_parent });
    assert_eq!(l1.bootstrap, BootstrapState::l1_replay(None));
    assert_eq!(l1.imported_tempo_tip, creation_parent);
    assert_eq!(l1.verified_zone_tip, tip(0, 0x10));
    assert_eq!(
        l1.model,
        ModelState::awaiting_creation(identity.portal_identity())
    );

    let genesis_anchor = tip(9, 0x42);
    let zone = Initialization::fresh(identity, FreshBootstrap::ZoneReplay { genesis_anchor });
    assert_eq!(zone.bootstrap, BootstrapState::zone_replay(genesis_anchor));
    assert_eq!(zone.imported_tempo_tip, genesis_anchor);
    assert_eq!(zone.verified_zone_tip, l1.verified_zone_tip);
    assert_eq!(zone.model, l1.model);
}

#[test]
fn unstarted_l1_replay_requires_the_creation_parent_height() {
    let directory = TempDir::new().unwrap();
    let identity = identity();
    let wrong_parent = tip(3, 0x41);
    let initialization = Initialization::fresh(
        identity,
        FreshBootstrap::L1Replay {
            creation_parent: wrong_parent,
        },
    );

    assert!(matches!(
        CheckerStore::open(directory.path(), initialization),
        Err(StoreError::L1ReplayStartHeightMismatch { creation, actual })
            if creation == identity.portal_creation_block() && actual == wrong_parent
    ));
    assert!(!CheckerStore::path_in(directory.path()).exists());
}

#[test]
fn reopen_rejects_an_unstarted_l1_cursor_that_skips_creation() {
    let directory = TempDir::new().unwrap();
    let identity = identity();
    let initialization = Initialization::fresh(
        identity,
        FreshBootstrap::L1Replay {
            creation_parent: tip(4, 0x41),
        },
    );
    let store = CheckerStore::open(directory.path(), initialization).unwrap();
    let skipped_tip = tip(5, 0x50);
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerMeta>(
        MetaKey::ImportedTempoTip,
        MetaValue::ImportedTempoTip(skipped_tip),
    )
    .unwrap();
    tx.commit().unwrap();

    assert!(matches!(
        store.load_current(),
        Err(StoreError::L1ReplayStartHeightMismatch { creation, actual })
            if creation == identity.portal_creation_block() && actual == skipped_tip
    ));
}

#[test]
fn l1_replay_cursor_cannot_precede_the_authenticated_creation() {
    let directory = TempDir::new().unwrap();
    let mut initialization = initialization(BootstrapPhase::L1Replay);
    let cursor = tip(4, 0x4f);
    initialization.bootstrap = BootstrapState::l1_replay(Some(cursor));
    initialization.imported_tempo_tip = cursor;

    assert!(matches!(
        CheckerStore::open(directory.path(), initialization.clone()),
        Err(StoreError::L1ReplayCursorOutsideCreationHistory { creation, cursor: actual })
            if creation == initialization.identity.portal_creation_block() && actual == cursor
    ));
}

#[test]
fn portal_creation_phase_tracks_imported_progress_in_every_runtime_phase() {
    let identity = identity();
    let creation = identity.portal_creation_block();
    assert_creation_progress_mismatch(Initialization::new(
        identity,
        BootstrapState::l1_replay(Some(creation)),
        tip(0, 0x10),
        creation,
        ModelState::awaiting_creation(identity.portal_identity()),
    ));

    let mut created = initialization(BootstrapPhase::L1Replay);
    created.bootstrap = BootstrapState::l1_replay(None);
    created.imported_tempo_tip = tip(4, 0x4f);
    assert_creation_progress_mismatch(created);

    for bootstrap in [
        BootstrapState::zone_replay(tip(4, 0x4f)),
        BootstrapState::live(),
    ] {
        assert_creation_progress_mismatch(Initialization::new(
            identity,
            bootstrap,
            tip(0, 0x10),
            tip(4, 0x4f),
            ModelState::created_with_zone_token_for_test(
                identity.portal_identity(),
                TokenAccounting::ZERO,
            ),
        ));
    }
    for bootstrap in [
        BootstrapState::zone_replay(creation),
        BootstrapState::live(),
    ] {
        assert_creation_progress_mismatch(Initialization::new(
            identity,
            bootstrap,
            tip(0, 0x10),
            creation,
            ModelState::awaiting_creation(identity.portal_identity()),
        ));
    }

    assert_creation_progress_mismatch(Initialization::new(
        identity,
        BootstrapState::live(),
        tip(0, 0x10),
        tip(creation.number, 0x51),
        ModelState::created_with_zone_token_for_test(
            identity.portal_identity(),
            TokenAccounting::ZERO,
        ),
    ));
}

fn assert_creation_progress_mismatch(initialization: Initialization) {
    let directory = TempDir::new().unwrap();
    let creation = initialization.identity.portal_creation_block();
    let imported_tip = initialization.imported_tempo_tip;
    let portal_created = initialization.model.portal().created().is_some();
    assert!(matches!(
        CheckerStore::open(directory.path(), initialization),
        Err(StoreError::PortalCreationProgressMismatch {
            creation: actual_creation,
            imported_tip: actual_tip,
            portal_created: actual_created,
        }) if actual_creation == creation
            && actual_tip == imported_tip
            && actual_created == portal_created
    ));
}

#[test]
fn l1_replay_rejects_a_verified_zone_tip_beyond_exact_genesis() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::L1Replay);
    let corrupt_tip = tip(1, 0x11);
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerMeta>(
        MetaKey::VerifiedZoneTip,
        MetaValue::VerifiedZoneTip(corrupt_tip),
    )
    .unwrap();
    tx.put::<CheckerCanonical>(corrupt_tip.number, CanonicalHash::new(corrupt_tip.hash))
        .unwrap();
    tx.commit().unwrap();
    drop(store);

    assert!(matches!(
        CheckerStore::open_existing(directory.path(), initialization.identity),
        Err(StoreError::L1ReplayZoneTipMismatch { expected, actual })
            if expected == tip(0, 0x10) && actual == corrupt_tip
    ));
}

#[test]
fn explicit_fresh_and_existing_paths_never_nest_or_overwrite() {
    let directory = TempDir::new().unwrap();
    let initialization = initialization(BootstrapPhase::ZoneReplay);
    let primary_path = directory.path().join("primary-checker");
    let primary = CheckerStore::create_fresh_at(&primary_path, initialization.clone()).unwrap();
    let expected = primary.load_current().unwrap();
    assert_eq!(primary.path(), primary_path);
    drop(primary);

    assert_eq!(
        CheckerStore::inspect_identity_at(&primary_path).unwrap(),
        initialization.identity
    );
    assert_eq!(
        CheckerStore::inspect_existing_at(&primary_path, initialization.identity).unwrap(),
        expected
    );
    assert!(matches!(
        CheckerStore::create_fresh_at(&primary_path, initialization.clone()),
        Err(StoreError::NonEmptyFreshDatabase { path }) if path == primary_path
    ));

    let rebuild_path = directory.path().join("checker-v2");
    let rebuild = CheckerStore::create_fresh_at(&rebuild_path, initialization.clone()).unwrap();
    assert_eq!(rebuild.path(), rebuild_path);
    assert!(!rebuild_path.join("checker").exists());
    drop(rebuild);

    let primary = CheckerStore::open_existing_at(&primary_path, initialization.identity).unwrap();
    assert_eq!(primary.load_current().unwrap(), expected);
}

#[test]
fn existing_open_never_initializes_missing_or_empty_state() {
    let directory = TempDir::new().unwrap();
    let checker_path = directory.path().join("checker");
    assert!(matches!(
        CheckerStore::open_existing(directory.path(), identity()),
        Err(StoreError::EmptyExistingDatabase { path }) if path == checker_path
    ));
    assert!(!checker_path.exists());

    let mut invalid = initialization(BootstrapPhase::Live);
    invalid.verified_zone_tip = tip(1, 0x51);
    assert!(matches!(
        CheckerStore::open(directory.path(), invalid),
        Err(StoreError::InvalidInitialization(_))
    ));
    assert!(
        !checker_path.exists(),
        "invalid initialization must fail before MDBX creates the target path"
    );
    assert!(matches!(
        CheckerStore::open_existing(directory.path(), identity()),
        Err(StoreError::EmptyExistingDatabase { path }) if path == checker_path
    ));
}

#[test]
fn existing_open_does_not_turn_a_junk_directory_into_an_mdbx_environment() {
    let directory = TempDir::new().unwrap();
    let checker_path = directory.path().join("checker");
    std::fs::create_dir(&checker_path).unwrap();
    let marker = checker_path.join("not-a-database");
    std::fs::write(&marker, b"leave me alone").unwrap();
    let before = std::fs::read_dir(&checker_path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();

    assert!(CheckerStore::open_existing(directory.path(), identity()).is_err());

    let after = std::fs::read_dir(&checker_path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(after, before);
    assert_eq!(std::fs::read(marker).unwrap(), b"leave me alone");
}

#[test]
fn existing_open_validates_identity_and_preserves_authenticated_creation_block() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let expected = store.load_current().unwrap();
    let expected_creation = initialization.identity.portal_creation_block();
    drop(store);

    let reopened = CheckerStore::open_existing(directory.path(), initialization.identity).unwrap();
    assert_eq!(reopened.load_current().unwrap(), expected);
    assert_eq!(
        reopened.portal_creation_block(),
        expected_creation,
        "runtime configuration must come from the validated durable identity"
    );
    drop(reopened);

    assert!(matches!(
        CheckerStore::open_existing(
            directory.path(),
            identity_with_portal(Address::repeat_byte(0x41)),
        ),
        Err(StoreError::IdentityMismatch {
            key: MetaKey::Contracts,
            ..
        })
    ));
    for wrong_creation in [tip(6, 0x50), tip(5, 0x51)] {
        assert!(matches!(
            CheckerStore::open_existing(
                directory.path(),
                identity_with_portal_and_creation(Address::repeat_byte(0x40), wrong_creation),
            ),
            Err(StoreError::IdentityMismatch {
                key: MetaKey::PortalCreationBlock,
                ..
            })
        ));
    }

    let reopened = CheckerStore::open_existing(directory.path(), initialization.identity).unwrap();
    assert_eq!(reopened.load_current().unwrap(), expected);
}

fn initialization_with_terminal_settlement(
    phase: BootstrapPhase,
    tempo_height: u64,
    zone_height: u64,
    zone_hash: B256,
) -> Initialization {
    let mut initialization = initialization(phase);
    let mut rows = flatten_model(&initialization.model).unwrap();
    rows.extend(terminal_settlement_rows(
        tempo_height,
        zone_height,
        zone_hash,
    ));
    initialization.model = assemble_model(initialization.identity.portal_identity(), rows).unwrap();
    initialization
}

#[test]
fn fresh_open_reopens_exactly_and_wrong_identity_never_writes() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let correct_initialization = initialization.clone();
    let expected = store.load_current().unwrap();
    let path = store.path().to_owned();
    drop(store);

    let reopened = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
    assert_eq!(reopened.path(), path);
    assert_eq!(reopened.load_current().unwrap(), expected);
    drop(reopened);

    let mut wrong = initialization;
    wrong.identity = identity_with_portal(Address::repeat_byte(0xff));
    assert!(matches!(
        CheckerStore::open(directory.path(), wrong),
        Err(StoreError::IdentityMismatch {
            key: MetaKey::Contracts,
            ..
        })
    ));

    let reopened = CheckerStore::open(directory.path(), correct_initialization).unwrap();
    assert_eq!(reopened.load_current().unwrap(), expected);
}

#[test]
fn settlement_tempo_progress_cannot_lead_the_imported_tip() {
    let directory = TempDir::new().unwrap();
    let invalid_initialization =
        initialization_with_terminal_settlement(BootstrapPhase::ZoneReplay, 11, 10, hash(0x91));
    let imported_tip = invalid_initialization.imported_tempo_tip;

    assert!(matches!(
        CheckerStore::open(directory.path(), invalid_initialization),
        Err(StoreError::PortalSettlementBeyondImportedTempoTip {
            settlement_height: 11,
            imported_tip: actual,
        }) if actual == imported_tip
    ));

    // The invalid initialization was rejected before its transaction committed.
    CheckerStore::open(directory.path(), initialization(BootstrapPhase::ZoneReplay)).unwrap();
}

#[test]
fn zone_replay_may_trail_settlement_but_live_load_consistency_and_reopen_reject_it() {
    let directory = TempDir::new().unwrap();
    let initialization =
        initialization_with_terminal_settlement(BootstrapPhase::ZoneReplay, 9, 10, hash(0x92));
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();

    let replay = store.load_current().unwrap();
    assert_eq!(replay.verified_zone_tip.number, 0);
    assert_eq!(
        replay
            .model
            .portal()
            .created()
            .unwrap()
            .settlement()
            .zone_height(),
        U256::from(10)
    );

    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerMeta>(
        MetaKey::Bootstrap,
        MetaValue::Bootstrap(BootstrapState::live()),
    )
    .unwrap();
    tx.commit().unwrap();

    for result in [store.load_current().map(|_| ()), store.check_consistency()] {
        assert!(matches!(
            result,
            Err(StoreError::LivePortalSettlementBeyondVerifiedZoneTip {
                settlement_height: 10,
                verified_tip,
            }) if verified_tip == initialization.verified_zone_tip
        ));
    }
    drop(store);

    assert!(matches!(
        CheckerStore::open(directory.path(), initialization),
        Err(StoreError::LivePortalSettlementBeyondVerifiedZoneTip {
            settlement_height: 10,
            verified_tip,
        }) if verified_tip.number == 0
    ));
}

#[test]
fn enter_live_rejects_a_settlement_ahead_of_zone_replay_without_mutation() {
    let directory = TempDir::new().unwrap();
    let initialization =
        initialization_with_terminal_settlement(BootstrapPhase::ZoneReplay, 9, 10, hash(0x92));
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
    let commit = store
        .enter_live(initialization.bootstrap, initialization.imported_tempo_tip)
        .unwrap();

    assert!(matches!(
        store.apply_bootstrap(commit),
        Err(StoreError::LivePortalSettlementBeyondVerifiedZoneTip {
            settlement_height: 10,
            verified_tip,
        }) if verified_tip == initialization.verified_zone_tip
    ));

    let current = store.load_current().unwrap();
    assert_eq!(current.bootstrap, initialization.bootstrap);
    assert_eq!(
        current.imported_tempo_tip,
        initialization.imported_tempo_tip
    );
}

#[test]
fn verified_settlement_height_must_anchor_to_its_canonical_hash() {
    let directory = TempDir::new().unwrap();
    let settlement_hash = hash(0x93);
    let initialization =
        initialization_with_terminal_settlement(BootstrapPhase::ZoneReplay, 9, 1, settlement_hash);
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();

    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerCanonical>(1, CanonicalHash::new(settlement_hash))
        .unwrap();
    tx.put::<CheckerMeta>(
        MetaKey::VerifiedZoneTip,
        MetaValue::VerifiedZoneTip(BlockNumHash::new(1, settlement_hash)),
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(store.load_current().unwrap().verified_zone_tip.number, 1);

    let canonical_hash = hash(0x94);
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerCanonical>(1, CanonicalHash::new(canonical_hash))
        .unwrap();
    tx.put::<CheckerMeta>(
        MetaKey::VerifiedZoneTip,
        MetaValue::VerifiedZoneTip(BlockNumHash::new(1, canonical_hash)),
    )
    .unwrap();
    tx.commit().unwrap();

    for result in [store.load_current().map(|_| ()), store.check_consistency()] {
        assert!(matches!(
            result,
            Err(StoreError::PortalSettlementCanonicalConflict {
                height: 1,
                settlement_hash: actual_settlement,
                canonical_hash: actual_canonical,
            }) if actual_settlement == settlement_hash && actual_canonical == canonical_hash
        ));
    }
    drop(store);

    assert!(matches!(
        CheckerStore::open(directory.path(), initialization),
        Err(StoreError::PortalSettlementCanonicalConflict {
            height: 1,
            settlement_hash: actual_settlement,
            canonical_hash: actual_canonical,
        }) if actual_settlement == settlement_hash && actual_canonical == canonical_hash
    ));
}

#[test]
fn enter_live_rejects_a_settlement_hash_conflict_without_mutating_metadata() {
    let directory = TempDir::new().unwrap();
    let settlement_hash = hash(0x93);
    let initialization =
        initialization_with_terminal_settlement(BootstrapPhase::ZoneReplay, 9, 1, settlement_hash);
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();

    store
        .apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0x93,
            0x95,
            Vec::new(),
        ))
        .unwrap();
    let first = store.load_current().unwrap();
    store
        .apply_block(block(
            first.verified_zone_tip,
            first.imported_tempo_tip,
            0x96,
            0x97,
            Vec::new(),
        ))
        .unwrap();
    let before = store.load_current().unwrap();

    let conflicting_hash = hash(0x94);
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerCanonical>(1, CanonicalHash::new(conflicting_hash))
        .unwrap();
    tx.commit().unwrap();

    let commit = store
        .enter_live(before.bootstrap, before.imported_tempo_tip)
        .unwrap();
    assert!(matches!(
        store.apply_bootstrap(commit),
        Err(StoreError::PortalSettlementCanonicalConflict {
            height: 1,
            settlement_hash: actual_settlement,
            canonical_hash: actual_canonical,
        }) if actual_settlement == settlement_hash && actual_canonical == conflicting_hash
    ));

    let tx = store.database().tx().unwrap();
    assert_eq!(
        tx.get::<CheckerMeta>(MetaKey::Bootstrap).unwrap(),
        Some(MetaValue::Bootstrap(before.bootstrap))
    );
    assert_eq!(
        tx.get::<CheckerMeta>(MetaKey::ImportedTempoTip).unwrap(),
        Some(MetaValue::ImportedTempoTip(before.imported_tempo_tip))
    );
    tx.commit().unwrap();
}

#[test]
fn old_version_row_reports_rebuild_before_decoding_incompatible_finding() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let tx = store.database().tx_mut().unwrap();
    assert_eq!(MetaValue::Version(1).compress(), [0, 0, 0, 0, 1]);
    tx.put::<CheckerMeta>(MetaKey::Version, MetaValue::Version(1))
        .unwrap();

    let record = FindingRecord::new(
        hash(0x91),
        Some(tip(11, 0x92)),
        FindingStatus::Canonical,
        FindingKind::MissingSupply(Address::repeat_byte(0x93)),
    )
    .unwrap();
    let mut incompatible = record.compress();
    incompatible[0] = 1;
    assert!(FindingRecord::decompress(&incompatible).is_err());
    tx.put::<RawTable<CheckerFindings>>(
        RawKey::new(FindingKey::new(1, hash(0x94), 0)),
        RawValue::from_vec(incompatible),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(store);

    let checker_path = CheckerStore::path_in(directory.path());
    for error in [
        CheckerStore::inspect_identity_at(&checker_path).unwrap_err(),
        CheckerStore::inspect_existing_at(&checker_path, initialization.identity).unwrap_err(),
        CheckerStore::open_existing_at(&checker_path, initialization.identity).unwrap_err(),
        CheckerStore::open(directory.path(), initialization.clone()).unwrap_err(),
    ] {
        let StoreError::VersionMismatch {
            path,
            expected: 3,
            actual: 1,
            rebuild_path,
        } = error
        else {
            panic!("unexpected version error: {error:?}");
        };
        assert_eq!(path, directory.path().join("checker"));
        assert_eq!(rebuild_path, directory.path().join("checker-v3"));
    }

    let rebuild_path = directory.path().join("checker-v3");
    let rebuilt = CheckerStore::create_fresh_at(&rebuild_path, initialization.clone()).unwrap();
    assert_eq!(rebuilt.path(), rebuild_path);
    drop(rebuilt);
    assert!(matches!(
        CheckerStore::inspect_existing_at(&checker_path, initialization.identity),
        Err(StoreError::VersionMismatch {
            expected: 3,
            actual: 1,
            ..
        })
    ));
}

#[test]
fn old_version_at_a_custom_path_recommends_a_unique_sibling() {
    let directory = TempDir::new().unwrap();
    let initialization = initialization(BootstrapPhase::ZoneReplay);
    let path = directory.path().join("zone-a-shadow");
    let store = CheckerStore::create_fresh_at(&path, initialization).unwrap();
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerMeta>(MetaKey::Version, MetaValue::Version(1))
        .unwrap();
    tx.commit().unwrap();
    drop(store);

    let StoreError::VersionMismatch { rebuild_path, .. } =
        CheckerStore::inspect_identity_at(&path).unwrap_err()
    else {
        panic!("expected incompatible version");
    };
    assert_eq!(rebuild_path, directory.path().join("zone-a-shadow-v3"));
}

#[test]
fn trailing_bytes_in_historical_values_fail_reopen_without_mutation() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let child = tip(1, 0x67);
    store
        .apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0x67,
            0x68,
            Vec::new(),
        ))
        .unwrap();

    let key = ChangesetKey::new(child.number, child.hash, 0);
    let tx = store.database().tx_mut().unwrap();
    let mut malformed = tx
        .get::<CheckerChangesets>(key)
        .unwrap()
        .unwrap()
        .compress();
    malformed.push(0xff);
    tx.put::<RawTable<CheckerChangesets>>(RawKey::new(key), RawValue::from_vec(malformed))
        .unwrap();
    tx.commit().unwrap();
    drop(store);

    for _ in 0..2 {
        assert!(matches!(
            CheckerStore::open(directory.path(), initialization.clone()),
            Err(StoreError::Database(_))
        ));
    }
}

#[test]
fn nonempty_partial_database_is_corruption_not_fresh_initialization() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let tx = store.database().tx_mut().unwrap();
    assert!(tx.delete::<CheckerMeta>(MetaKey::Bootstrap, None).unwrap());
    tx.commit().unwrap();
    drop(store);

    assert!(matches!(
        CheckerStore::open(directory.path(), initialization),
        Err(StoreError::MissingMetadata(MetaKey::Bootstrap))
    ));
}

#[test]
fn invalid_empty_initialization_aborts_without_leaving_partial_rows() {
    let directory = TempDir::new().unwrap();
    let correct = initialization(BootstrapPhase::ZoneReplay);

    let mut mismatched_cursor = correct.clone();
    mismatched_cursor.bootstrap = BootstrapState::zone_replay(tip(9, 0x59));
    assert!(matches!(
        CheckerStore::open(directory.path(), mismatched_cursor),
        Err(StoreError::InvalidInitialization(_))
    ));
    assert!(!CheckerStore::path_in(directory.path()).exists());

    CheckerStore::open(directory.path(), correct).unwrap();
}

#[test]
fn l1_replay_allows_pending_zone_tokens_with_only_deposit_liability() {
    let directory = TempDir::new().unwrap();
    let (initialization, token) = initialization_with_pending_token(BootstrapPhase::L1Replay);
    let expected_bootstrap = initialization.bootstrap;
    let store = CheckerStore::open(directory.path(), initialization).unwrap();
    let current = store.load_current().unwrap();

    assert_eq!(current.bootstrap, expected_bootstrap);
    let pending = current.model.token(token).unwrap();
    assert_eq!(pending.phase(), TokenPhase::PendingZoneEnable);
    assert_eq!(
        pending.accounting(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(9),
            withdrawal_liability: U256::ZERO,
        }
    );
}

#[test]
fn zone_replay_and_live_initialization_reject_pending_zone_tokens() {
    for phase in [BootstrapPhase::ZoneReplay, BootstrapPhase::Live] {
        let directory = TempDir::new().unwrap();
        let (initialization, token) = initialization_with_pending_token(phase);
        let expected_bootstrap = initialization.bootstrap;

        assert!(matches!(
            CheckerStore::open(directory.path(), initialization),
            Err(StoreError::BootstrapTokenPhaseMismatch {
                bootstrap,
                token: found,
            }) if bootstrap == expected_bootstrap && found == token
        ));
        assert!(!CheckerStore::path_in(directory.path()).exists());
    }
}

#[test]
fn l1_replay_initialization_rejects_zone_enabled_tokens() {
    let directory = TempDir::new().unwrap();
    let mut initialization = initialization(BootstrapPhase::L1Replay);
    let token = initialization.identity.portal_identity().initial_token();
    initialization
        .model
        .set_token_phase_for_test(token, TokenPhase::ZoneEnabled);

    assert!(matches!(
        CheckerStore::open(directory.path(), initialization),
        Err(StoreError::BootstrapTokenPhaseMismatch {
            bootstrap: BootstrapState::L1Replay { .. },
            token: found,
        }) if found == token
    ));
    assert!(!CheckerStore::path_in(directory.path()).exists());
}

#[test]
fn enter_live_rejects_pending_zone_tokens_without_mutation() {
    let directory = TempDir::new().unwrap();
    let initialization = initialization(BootstrapPhase::ZoneReplay);
    let token = initialization.identity.portal_identity().initial_token();
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
    let pending = pending_token_value();
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerModelState>(ModelKey::Token(token), pending.clone())
        .unwrap();
    tx.commit().unwrap();
    let commit = store
        .enter_live(initialization.bootstrap, initialization.imported_tempo_tip)
        .unwrap();

    assert!(matches!(
        store.apply_bootstrap(commit),
        Err(StoreError::BootstrapTokenPhaseMismatch {
            bootstrap: BootstrapState::ZoneReplay { .. },
            token: found,
        }) if found == token
    ));
    let tx = store.database().tx().unwrap();
    assert_eq!(
        tx.get::<CheckerMeta>(MetaKey::Bootstrap).unwrap(),
        Some(MetaValue::Bootstrap(initialization.bootstrap))
    );
    assert_eq!(
        tx.get::<CheckerModelState>(ModelKey::Token(token)).unwrap(),
        Some(pending)
    );
    tx.commit().unwrap();
}

#[test]
fn live_transition_and_restart_allow_a_portal_created_after_the_current_head() {
    let directory = TempDir::new().unwrap();
    let anchor = tip(10, 0x60);
    let identity = identity_with_portal_and_creation(Address::repeat_byte(0x40), tip(11, 0x50));
    let initialization = Initialization::fresh(
        identity,
        FreshBootstrap::ZoneReplay {
            genesis_anchor: anchor,
        },
    );
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
    let commit = store
        .enter_live(initialization.bootstrap, initialization.imported_tempo_tip)
        .unwrap();

    assert_eq!(
        store.apply_bootstrap(commit).unwrap(),
        WriteOutcome::Applied
    );
    let expected = store.load_current().unwrap();
    assert_eq!(expected.bootstrap, BootstrapState::live());
    assert!(expected.model.portal().created().is_none());
    drop(store);

    let restarted = CheckerStore::open_existing(directory.path(), identity).unwrap();
    assert_eq!(restarted.load_current().unwrap(), expected);
}

#[test]
fn malformed_batch_range_fails_reopen_without_panicking() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let malformed = ModelValue::Batch(BatchValue::Finalized(FinalizedBatchValue {
        boundary: BatchBoundaryValue {
            first_zone_parent_hash: hash(0x31),
            final_zone_block_hash: hash(0x32),
            first_processed_deposit: CursorValue {
                hash: B256::ZERO,
                number: 0,
            },
            final_processed_deposit: CursorValue {
                hash: B256::ZERO,
                number: 0,
            },
            final_imported_tempo_block_number: 10,
            final_zone_height: 1,
        },
        members: BatchMembersValue {
            first_withdrawal_index: u64::MAX,
            member_count: 1,
            withdrawal_queue_hash: hash(0x33),
        },
    }));
    let tx = store.database().tx_mut().unwrap();
    tx.put::<RawTable<CheckerModelState>>(
        RawKey::new(ModelKey::Batch(1)),
        RawValue::from_vec(malformed.compress()),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(store);

    let reopened = catch_unwind(AssertUnwindSafe(|| {
        CheckerStore::open(directory.path(), initialization)
    }));
    assert!(reopened.is_ok(), "malformed batch must not panic on reopen");
    assert!(matches!(reopened.unwrap(), Err(StoreError::Database(_))));
}

#[test]
fn bootstrap_codec_rejects_phase_cursor_combinations_the_sum_type_cannot_represent() {
    let zone_replay_without_cursor = vec![crate::store::SCHEMA_VERSION, 0x05, 0x01, 0x00];
    let mut live_with_cursor = vec![crate::store::SCHEMA_VERSION, 0x05, 0x02, 0x01];
    live_with_cursor.extend_from_slice(&10_u64.to_be_bytes());
    live_with_cursor.extend_from_slice(hash(0x41).as_slice());

    for malformed in [zone_replay_without_cursor, live_with_cursor] {
        assert!(MetaValue::decompress(&malformed).is_err());
    }
}
