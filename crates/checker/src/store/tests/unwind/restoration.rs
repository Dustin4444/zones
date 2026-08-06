use super::*;

#[test]
fn one_block_unwind_restores_the_exact_parent_cut_and_deletes_child_history() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let parent = store.load_current().unwrap();
    let token = initialization.identity.portal_identity().initial_token();
    let (child_zone, _) = apply_token_child(
        &store,
        parent.verified_zone_tip,
        parent.imported_tempo_tip,
        0x81,
        0x91,
        token,
        7,
    );
    assert_child_journal(&store, child_zone, 2);

    assert_eq!(
        store.unwind_tip(child_zone).unwrap(),
        ParentTips::new(parent.verified_zone_tip, parent.imported_tempo_tip)
    );
    assert_eq!(store.load_current().unwrap(), parent);

    let tx = store.database().tx().unwrap();
    assert!(
        tx.get::<CheckerCanonical>(child_zone.number)
            .unwrap()
            .is_none()
    );
    assert_eq!(tx.entries::<CheckerChangesets>().unwrap(), 0);
    tx.commit().unwrap();
    drop(store);

    let reopened = CheckerStore::open_existing(directory.path(), initialization.identity).unwrap();
    assert_eq!(reopened.load_current().unwrap(), parent);
    reopened.check_consistency().unwrap();
}

#[test]
fn metadata_only_block_unwinds_without_model_mutations() {
    let (_directory, _initialization, store) = open_test_store(BootstrapPhase::Live);
    let parent = store.load_current().unwrap();
    let child_zone = tip(parent.verified_zone_tip.number + 1, 0x82);
    let commit = block(
        parent.verified_zone_tip,
        parent.imported_tempo_tip,
        0x82,
        0x92,
        Vec::new(),
    );
    assert_eq!(store.apply_block(commit).unwrap(), WriteOutcome::Applied);
    assert_child_journal(&store, child_zone, 1);

    assert_eq!(
        store.unwind_tip(child_zone).unwrap(),
        ParentTips::new(parent.verified_zone_tip, parent.imported_tempo_tip)
    );
    assert_eq!(store.load_current().unwrap(), parent);
}

#[test]
fn deep_tip_unwind_restores_every_intermediate_parent() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let initial = store.load_current().unwrap();
    let token = initialization.identity.portal_identity().initial_token();
    let mut snapshots = vec![initial];
    let mut children = Vec::new();
    let mut zone_parent = snapshots[0].verified_zone_tip;
    let mut tempo_parent = snapshots[0].imported_tempo_tip;
    for (offset, (zone_byte, tempo_byte)) in
        [(0x82, 0x92), (0x83, 0x93), (0x84, 0x94), (0x85, 0x95)]
            .into_iter()
            .enumerate()
    {
        let (zone_child, tempo_child) = apply_token_child(
            &store,
            zone_parent,
            tempo_parent,
            zone_byte,
            tempo_byte,
            token,
            u64::try_from(offset + 1).unwrap(),
        );
        children.push(zone_child);
        snapshots.push(store.load_current().unwrap());
        zone_parent = zone_child;
        tempo_parent = tempo_child;
    }

    for index in (0..children.len()).rev() {
        let expected = &snapshots[index];
        assert_eq!(
            store.unwind_tip(children[index]).unwrap(),
            ParentTips::new(expected.verified_zone_tip, expected.imported_tempo_tip)
        );
        assert_eq!(&store.load_current().unwrap(), expected);
    }
}

#[test]
fn unwind_restores_insert_and_delete_before_images() {
    let (_directory, _initialization, store) = open_test_store(BootstrapPhase::Live);
    let initial = store.load_current().unwrap();
    let token = Address::repeat_byte(0xa1);
    let (zone1, tempo1) = apply_token_child(
        &store,
        initial.verified_zone_tip,
        initial.imported_tempo_tip,
        0x86,
        0x96,
        token,
        0,
    );
    let after_insert = store.load_current().unwrap();
    assert_eq!(
        after_insert.model_rows.get(&ModelKey::Token(token)),
        Some(&token_value(0))
    );

    let delete = block(
        zone1,
        tempo1,
        0x87,
        0x97,
        vec![ModelMutation::delete(ModelKey::Token(token))],
    );
    assert_eq!(store.apply_block(delete).unwrap(), WriteOutcome::Applied);
    let zone2 = tip(2, 0x87);
    let after_delete = store.load_current().unwrap();
    assert!(
        !after_delete
            .model_rows
            .contains_key(&ModelKey::Token(token))
    );

    let tx = store.database().tx().unwrap();
    assert!(matches!(
        tx.get::<CheckerChangesets>(ChangesetKey::new(zone1.number, zone1.hash, 1))
            .unwrap(),
        Some(BeforeImage::Model { key, value: None }) if key == ModelKey::Token(token)
    ));
    assert!(matches!(
        tx.get::<CheckerChangesets>(ChangesetKey::new(zone2.number, zone2.hash, 1))
            .unwrap(),
        Some(BeforeImage::Model { key, value: Some(value) })
            if key == ModelKey::Token(token) && *value == token_value(0)
    ));
    tx.commit().unwrap();

    store.unwind_tip(zone2).unwrap();
    assert_eq!(store.load_current().unwrap(), after_insert);
    store.unwind_tip(zone1).unwrap();
    assert_eq!(store.load_current().unwrap(), initial);
}
