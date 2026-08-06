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
        store.activate_finding(FindingKey::new(1, hash(0xc4), 0), record.clone(), parent),
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

    for write in 1..=2 {
        assert!(matches!(
            store.activate_finding_aborting_after(key, record.clone(), parent, write),
            Err(StoreError::InjectedWriteFailure)
        ));
        assert_eq!(store.active_alert().unwrap(), None);
        let tx = store.database().tx().unwrap();
        assert_eq!(
            tx.get::<CheckerFindings>(key).unwrap().unwrap().status(),
            FindingStatus::Orphaned
        );
        tx.commit().unwrap();
    }
    assert_eq!(
        store.activate_finding(key, record, parent).unwrap(),
        WriteOutcome::Applied
    );
    assert_eq!(store.active_alert().unwrap().unwrap().finding, key);
    let tx = store.database().tx().unwrap();
    assert_eq!(
        tx.get::<CheckerFindings>(key).unwrap().unwrap().status(),
        FindingStatus::Canonical
    );
    tx.commit().unwrap();
    store.check_consistency().unwrap();
}

#[test]
fn finding_anchor_uses_zone_parent_but_treats_imported_tempo_as_evidence() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let parent = initialization.verified_zone_tip;
    let key = FindingKey::new(parent.number + 1, hash(0xd1), 0);
    let record = FindingRecord::new(
        parent.hash,
        Some(tip(900, 0xd2)),
        FindingStatus::Canonical,
        FindingKind::TempoContinuity(tip(10, 0xd3), 900, hash(0xd4)),
    )
    .unwrap();

    assert_eq!(
        store.activate_finding(key, record, parent).unwrap(),
        WriteOutcome::Applied
    );
}

#[test]
fn finding_anchor_still_rejects_wrong_zone_key_parent_and_status() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let parent = initialization.verified_zone_tip;
    let imported = Some(tip(900, 0xe5));
    let kind = || FindingKind::MissingSupply(Address::repeat_byte(0xe4));

    let wrong_height =
        FindingRecord::new(parent.hash, imported, FindingStatus::Canonical, kind()).unwrap();
    assert!(matches!(
        store.activate_finding(FindingKey::new(2, hash(0xe3), 0), wrong_height, parent),
        Err(StoreError::FindingParent { .. })
    ));

    let wrong_parent =
        FindingRecord::new(hash(0xe2), imported, FindingStatus::Canonical, kind()).unwrap();
    assert!(matches!(
        store.activate_finding(FindingKey::new(1, hash(0xe3), 0), wrong_parent, parent),
        Err(StoreError::FindingParent { .. })
    ));

    let wrong_status =
        FindingRecord::new(parent.hash, imported, FindingStatus::Orphaned, kind()).unwrap();
    assert!(matches!(
        store.activate_finding(FindingKey::new(1, hash(0xe3), 0), wrong_status, parent),
        Err(StoreError::FindingStatus { .. })
    ));
}
