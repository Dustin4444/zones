use super::*;

#[test]
fn every_unwind_write_fault_leaves_the_child_durable_after_reopen() {
    let (directory, initialization, mut store) = open_test_store(BootstrapPhase::Live);
    let parent = store.load_current().unwrap();
    let token = initialization.identity.portal_identity().initial_token();
    let (child_zone, _) = apply_token_child(
        &store,
        parent.verified_zone_tip,
        parent.imported_tempo_tip,
        0x84,
        0x94,
        token,
        7,
    );
    let child = store.load_current().unwrap();
    let child_history = durable_history(&store);

    // One model restore, two tips, one canonical row, and two changeset rows.
    for write in 1..=6 {
        assert!(matches!(
            store.unwind_tip_aborting_after(child_zone, write),
            Err(StoreError::InjectedWriteFailure)
        ));
        drop(store);
        store = CheckerStore::open_existing(directory.path(), initialization.identity).unwrap();
        assert_eq!(store.load_current().unwrap(), child);
        assert_eq!(durable_history(&store), child_history);
        assert_child_journal(&store, child_zone, 2);
    }

    store.unwind_tip(child_zone).unwrap();
    assert_eq!(store.load_current().unwrap(), parent);
}

#[test]
fn unwind_requires_the_exact_current_tip_and_rejects_active_alerts() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let parent = store.load_current().unwrap();
    let token = initialization.identity.portal_identity().initial_token();
    let (child_zone, child_tempo) = apply_token_child(
        &store,
        parent.verified_zone_tip,
        parent.imported_tempo_tip,
        0x85,
        0x95,
        token,
        7,
    );
    let child = store.load_current().unwrap();
    let wrong = BlockNumHash::new(child_zone.number, hash(0xff));
    assert!(matches!(
        store.unwind_tip(wrong),
        Err(StoreError::UnwindTipMismatch { expected, actual })
            if expected == wrong && actual == child_zone
    ));
    assert_eq!(store.load_current().unwrap(), child);

    let finding = FindingKey::new(child_zone.number + 1, hash(0x86), 0);
    let record = FindingRecord::new(
        child_zone.hash,
        Some(BlockNumHash::new(child_tempo.number + 1, hash(0x96))),
        FindingStatus::Canonical,
        FindingKind::MissingSupply(token),
    )
    .unwrap();
    store.activate_finding(finding, record, child_zone).unwrap();
    let alerted = store.load_current().unwrap();

    assert!(matches!(
        store.unwind_tip(child_zone),
        Err(StoreError::ActiveAlert(actual)) if actual == finding
    ));
    assert_eq!(store.load_current().unwrap(), alerted);
    assert_child_journal(&store, child_zone, 2);
}
