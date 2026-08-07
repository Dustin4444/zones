use super::*;

#[test]
fn missing_changeset_row_aborts_without_partial_repair() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let parent = store.load_current().unwrap();
    let token = initialization.identity.portal_identity().initial_token();
    let (child_zone, _) = apply_token_child(
        &store,
        parent.verified_zone_tip,
        parent.imported_tempo_tip,
        0x87,
        0x97,
        token,
        7,
    );
    let child = store.load_current().unwrap();
    let mutation_key = ChangesetKey::new(child_zone.number, child_zone.hash, 1);
    let tx = store.database().tx_mut().unwrap();
    assert!(tx.delete::<CheckerChangesets>(mutation_key, None).unwrap());
    tx.commit().unwrap();
    let corrupted_history = durable_history(&store);

    assert!(matches!(
        store.unwind_tip(child_zone),
        Err(StoreError::InvalidChangeset { height, hash: actual, .. })
            if height == child_zone.number && actual == child_zone.hash
    ));
    assert_eq!(store.load_current().unwrap(), child);
    assert_eq!(durable_history(&store), corrupted_history);
}

#[test]
fn incoherent_restored_parent_is_validated_before_any_write() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let parent = store.load_current().unwrap();
    let token = initialization.identity.portal_identity().initial_token();
    let (child_zone, _) = apply_token_child(
        &store,
        parent.verified_zone_tip,
        parent.imported_tempo_tip,
        0x88,
        0x98,
        token,
        7,
    );
    let child = store.load_current().unwrap();
    let mutation_key = ChangesetKey::new(child_zone.number, child_zone.hash, 1);
    let corrupt_parent = BeforeImage::Model {
        key: ModelKey::Token(token),
        value: Some(Box::new(ModelValue::Token(TokenValue {
            phase: StoredTokenPhase::PendingZoneEnable,
            supply: U256::ONE,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        }))),
    };
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerChangesets>(mutation_key, corrupt_parent)
        .unwrap();
    tx.commit().unwrap();
    let corrupted_history = durable_history(&store);

    assert!(store.unwind_tip(child_zone).is_err());
    assert_eq!(store.load_current().unwrap(), child);
    assert_eq!(durable_history(&store), corrupted_history);
}

#[test]
fn surplus_changeset_row_aborts_without_partial_repair() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let parent = store.load_current().unwrap();
    let token = initialization.identity.portal_identity().initial_token();
    let (child_zone, _) = apply_token_child(
        &store,
        parent.verified_zone_tip,
        parent.imported_tempo_tip,
        0x89,
        0x99,
        token,
        7,
    );
    let child = store.load_current().unwrap();
    let surplus_key = ChangesetKey::new(child_zone.number, child_zone.hash, 2);
    let surplus = BeforeImage::Model {
        key: ModelKey::ZoneLastFallbackNonce,
        value: Some(Box::new(ModelValue::ZoneLastFallbackNonce(0))),
    };
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerChangesets>(surplus_key, surplus).unwrap();
    tx.commit().unwrap();
    let corrupted_history = durable_history(&store);

    assert!(matches!(
        store.unwind_tip(child_zone),
        Err(StoreError::InvalidChangeset { height, hash: actual, reason })
            if height == child_zone.number
                && actual == child_zone.hash
                && reason == "changeset has surplus mutation rows"
    ));
    assert_eq!(store.load_current().unwrap(), child);
    assert_eq!(durable_history(&store), corrupted_history);
}

#[test]
fn conflicting_changeset_hash_aborts_without_partial_repair() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let parent = store.load_current().unwrap();
    let token = initialization.identity.portal_identity().initial_token();
    let (child_zone, _) = apply_token_child(
        &store,
        parent.verified_zone_tip,
        parent.imported_tempo_tip,
        0x8a,
        0x9a,
        token,
        7,
    );
    let child = store.load_current().unwrap();
    let conflict_key = ChangesetKey::new(child_zone.number, hash(0x01), 0);
    let conflict = BeforeImage::Block(BlockBeforeImage {
        prior_verified_zone_tip: parent.verified_zone_tip,
        prior_imported_tempo_tip: parent.imported_tempo_tip,
        mutation_count: 0,
    });
    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerChangesets>(conflict_key, conflict).unwrap();
    tx.commit().unwrap();
    let corrupted_history = durable_history(&store);

    assert!(matches!(
        store.unwind_tip(child_zone),
        Err(StoreError::InvalidChangeset { height, hash: actual, reason })
            if height == child_zone.number
                && actual == child_zone.hash
                && reason == "changeset height contains a conflicting block hash"
    ));
    assert_eq!(store.load_current().unwrap(), child);
    assert_eq!(durable_history(&store), corrupted_history);
}
