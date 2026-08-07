use super::*;

#[test]
fn exact_next_live_child_returns_the_authoritative_parent_tips() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let child = tip(1, 0x71);

    let CanonicalBlock::Next {
        verified_zone_tip,
        imported_tempo_tip,
    } = store
        .preflight_block(child, initialization.verified_zone_tip.hash)
        .unwrap()
    else {
        panic!("next child was classified as retained canonical history");
    };
    assert_eq!(verified_zone_tip, initialization.verified_zone_tip);
    assert_eq!(imported_tempo_tip, initialization.imported_tempo_tip);
}

#[test]
fn retained_blocks_return_the_current_tip_without_regressing_acknowledgement() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let zone0 = initialization.verified_zone_tip;
    let tempo0 = initialization.imported_tempo_tip;
    store
        .apply_block(block(zone0, tempo0, 0x72, 0x82, Vec::new()))
        .unwrap();
    let zone1 = tip(1, 0x72);
    let tempo1 = tip(11, 0x82);
    store
        .apply_block(block(zone1, tempo1, 0x73, 0x83, Vec::new()))
        .unwrap();
    let current_tip = tip(2, 0x73);

    for retained in [zone0, zone1, current_tip] {
        assert_eq!(
            store
                .preflight_block(retained, B256::repeat_byte(0xff))
                .unwrap(),
            CanonicalBlock::AlreadyCanonical {
                verified_zone_tip: current_tip,
            }
        );
    }
}

#[test]
fn canonical_conflicts_gaps_and_wrong_next_parent_fail_explicitly() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::Live);
    let parent = initialization.verified_zone_tip;

    assert!(matches!(
        store.preflight_block(tip(0, 0x74), B256::ZERO),
        Err(StoreError::CanonicalConflict {
            height: 0,
            expected,
            actual,
        }) if expected == hash(0x74) && actual == parent.hash
    ));
    assert!(matches!(
        store.preflight_block(tip(2, 0x75), parent.hash),
        Err(StoreError::NonAdjacent {
            chain: "Zone",
            parent: found_parent,
            child,
        }) if found_parent == parent && child == tip(2, 0x75)
    ));
    assert!(matches!(
        store.preflight_block(tip(1, 0x76), hash(0x77)),
        Err(StoreError::CandidateParentConflict {
            child,
            expected,
            actual,
        }) if child == tip(1, 0x76) && expected == parent.hash && actual == hash(0x77)
    ));
}

#[test]
fn new_work_is_allowed_during_zone_replay_but_not_l1_replay_or_an_alert() {
    let (_directory, initialization, replay) = open_test_store(BootstrapPhase::ZoneReplay);
    let parent = initialization.verified_zone_tip;
    assert_eq!(
        replay.preflight_block(tip(1, 0x78), parent.hash).unwrap(),
        CanonicalBlock::Next {
            verified_zone_tip: parent,
            imported_tempo_tip: initialization.imported_tempo_tip,
        }
    );
    assert_eq!(
        replay.preflight_block(parent, B256::ZERO).unwrap(),
        CanonicalBlock::AlreadyCanonical {
            verified_zone_tip: parent,
        }
    );

    let (_directory, initialization, l1_replay) = open_test_store(BootstrapPhase::L1Replay);
    let parent = initialization.verified_zone_tip;
    assert!(matches!(
        l1_replay.preflight_block(tip(1, 0x79), parent.hash),
        Err(StoreError::InvalidBootstrapProgress(
            "ordinary block preflight is disabled during L1 replay"
        ))
    ));

    let (_directory, initialization, live) = open_test_store(BootstrapPhase::Live);
    let parent = initialization.verified_zone_tip;
    let key = FindingKey::new(1, hash(0x79), 0);
    let record = FindingRecord::new(
        parent.hash,
        Some(tip(11, 0x7a)),
        FindingStatus::Canonical,
        FindingKind::MissingSupply(Address::repeat_byte(0x7b)),
    )
    .unwrap();
    live.activate_finding(key, record, parent).unwrap();

    assert!(matches!(
        live.preflight_block(tip(1, 0x79), parent.hash),
        Err(StoreError::ActiveAlert(found)) if found == key
    ));
    assert_eq!(
        live.preflight_block(parent, B256::ZERO).unwrap(),
        CanonicalBlock::AlreadyCanonical {
            verified_zone_tip: parent,
        }
    );
}
