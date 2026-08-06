use super::*;
use crate::store::model_state::assemble_model;

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
fn stable_version_row_reports_mismatch_before_decoding_incompatible_metadata() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let tx = store.database().tx_mut().unwrap();
    assert_eq!(MetaValue::Version(2).compress(), [0, 0, 0, 0, 2]);
    tx.put::<CheckerMeta>(MetaKey::Version, MetaValue::Version(2))
        .unwrap();

    let mut incompatible = tx
        .get::<CheckerMeta>(MetaKey::Contracts)
        .unwrap()
        .unwrap()
        .compress();
    incompatible[0] = 2;
    tx.put::<RawTable<CheckerMeta>>(
        RawKey::new(MetaKey::Contracts),
        RawValue::from_vec(incompatible),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(store);

    for _ in 0..2 {
        let error = CheckerStore::open(directory.path(), initialization.clone()).unwrap_err();
        let StoreError::VersionMismatch {
            path,
            expected: 1,
            actual: 2,
            rebuild_path,
        } = error
        else {
            panic!("unexpected version error: {error:?}");
        };
        assert_eq!(path, directory.path().join("checker"));
        assert_eq!(rebuild_path, directory.path().join("checker-v1"));
    }
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

    CheckerStore::open(directory.path(), correct).unwrap();
}

#[test]
fn replay_bootstraps_allow_pending_zone_tokens_with_only_deposit_liability() {
    for phase in [BootstrapPhase::L1Replay, BootstrapPhase::ZoneReplay] {
        let directory = TempDir::new().unwrap();
        let (initialization, token) = initialization_with_pending_token(phase);
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
}

#[test]
fn live_initialization_and_restart_reject_pending_zone_tokens() {
    let directory = TempDir::new().unwrap();
    let (live_initialization, token) = initialization_with_pending_token(BootstrapPhase::Live);
    assert!(matches!(
        CheckerStore::open(directory.path(), live_initialization),
        Err(StoreError::LiveModelHasPendingToken { token: found }) if found == token
    ));
    // Rejection happens before any initial rows commit.
    CheckerStore::open(directory.path(), initialization(BootstrapPhase::Live)).unwrap();

    let directory = TempDir::new().unwrap();
    let (initialization, token) = initialization_with_pending_token(BootstrapPhase::ZoneReplay);
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerMeta>(
        MetaKey::Bootstrap,
        MetaValue::Bootstrap(BootstrapState::live()),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(store);

    assert!(matches!(
        CheckerStore::open(directory.path(), initialization),
        Err(StoreError::LiveModelHasPendingToken { token: found }) if found == token
    ));
}

#[test]
fn enter_live_rejects_pending_zone_tokens_without_mutation() {
    let directory = TempDir::new().unwrap();
    let (initialization, token) = initialization_with_pending_token(BootstrapPhase::ZoneReplay);
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
    let commit = store
        .enter_live(initialization.bootstrap, initialization.imported_tempo_tip)
        .unwrap();

    assert!(matches!(
        store.apply_bootstrap(commit),
        Err(StoreError::LiveModelHasPendingToken { token: found }) if found == token
    ));
    let current = store.load_current().unwrap();
    assert_eq!(current.bootstrap, initialization.bootstrap);
    assert_eq!(
        current.model.token(token).unwrap().phase(),
        TokenPhase::PendingZoneEnable
    );
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
    let zone_replay_without_cursor = vec![1, 0x05, 0x01, 0x00];
    let mut live_with_cursor = vec![1, 0x05, 0x02, 0x01];
    live_with_cursor.extend_from_slice(&10_u64.to_be_bytes());
    live_with_cursor.extend_from_slice(hash(0x41).as_slice());

    for malformed in [zone_replay_without_cursor, live_with_cursor] {
        assert!(MetaValue::decompress(&malformed).is_err());
    }
}
