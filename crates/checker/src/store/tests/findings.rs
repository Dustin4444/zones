use super::*;

#[test]
fn finding_activation_and_orphaning_are_atomic_and_conflict_safe() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let parent = initialization.verified_zone_tip;
    let key = FindingKey::new(1, hash(0xc1), 0);
    let record = FindingRecord::new(
        parent.hash,
        Some(tip(11, 0xc2)),
        FindingStatus::Canonical,
        FindingKind::MissingSupply(Address::repeat_byte(0xc3)),
    )
    .unwrap();
    for write in 1..=2 {
        assert!(matches!(
            store.activate_finding_aborting_after(key, record.clone(), parent, write),
            Err(StoreError::InjectedWriteFailure)
        ));
        assert_eq!(store.active_alert().unwrap(), None);
        let tx = store.database().tx().unwrap();
        assert!(tx.get::<CheckerFindings>(key).unwrap().is_none());
        tx.commit().unwrap();
    }

    assert_eq!(
        store.activate_finding(key, record.clone(), parent).unwrap(),
        WriteOutcome::Applied
    );
    assert_eq!(
        store.activate_finding(key, record.clone(), parent).unwrap(),
        WriteOutcome::AlreadyApplied
    );
    assert!(matches!(
        store.activate_finding(FindingKey::new(1, hash(0xc4), 0), record, parent),
        Err(StoreError::FindingConflict { .. })
    ));
    store.check_consistency().unwrap();

    for write in 1..=2 {
        assert!(matches!(
            store.orphan_finding_aborting_after(key, write),
            Err(StoreError::InjectedWriteFailure)
        ));
        assert_eq!(store.active_alert().unwrap().unwrap().finding, key);
        let tx = store.database().tx().unwrap();
        assert_eq!(
            tx.get::<CheckerFindings>(key).unwrap().unwrap().status(),
            FindingStatus::Canonical
        );
        tx.commit().unwrap();
    }
    assert_eq!(
        store.orphan_active_finding(key).unwrap(),
        WriteOutcome::Applied
    );
    assert_eq!(
        store.orphan_active_finding(key).unwrap(),
        WriteOutcome::AlreadyApplied
    );
    assert_eq!(store.active_alert().unwrap(), None);
    let tx = store.database().tx().unwrap();
    assert_eq!(
        tx.get::<CheckerFindings>(key).unwrap().unwrap().status(),
        FindingStatus::Orphaned
    );
    tx.commit().unwrap();
    store.check_consistency().unwrap();
}
