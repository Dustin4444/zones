//! Transactions tests.

use super::*;

#[test]
fn transaction_abort_leaves_apply_checkpoint_and_reorg_fully_old() {
    let (directory, store) = create();
    store.inject_abort();
    assert!(matches!(
        store.apply(
            &current(&store),
            entry(1, bootstrap().zone),
            block(1, 0x11),
            Coverage::Complete
        ),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(
        store.load().unwrap().meta.verified_zone_tip,
        bootstrap().zone
    );

    let one = apply(&store, 1, bootstrap().zone);
    let snapshot = store.load().unwrap();
    store.inject_abort();
    assert!(matches!(
        store.checkpoint(
            identity(),
            ChainCut {
                zone: one,
                tempo: block(1, 0x21)
            },
            snapshot.state
        ),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(store.load().unwrap().meta.active_checkpoint.height, 0);

    let two = apply(&store, 2, one);
    store.inject_abort();
    assert!(matches!(
        store.reorg(&current(&store), one),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(store.load().unwrap().meta.verified_zone_tip, two);
    drop(store);
    let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(reopened.meta.verified_zone_tip, two);
}

#[test]
fn finding_and_gap_abort_leave_latches_and_acknowledgement_fully_old() {
    let (directory, store) = create();
    let (key, value) = finding(block(1, 0x41));
    store.inject_abort();
    assert!(matches!(
        store.record_finding(&current(&store), key, value),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(store.load().unwrap().meta.active_finding, None);

    store.inject_abort();
    assert!(matches!(
        store.record_gap(
            &current(&store),
            block(1, 0x41),
            block(3, 0x43),
            CoverageGapReason::MissingTempoData,
        ),
        Err(PersistenceError::InjectedAbort)
    ));
    drop(store);
    let (_, snapshot) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(snapshot.meta.acknowledged_zone_tip, bootstrap().zone);
    assert_eq!(snapshot.meta.coverage, Coverage::Complete);
    assert_eq!(snapshot.meta.active_finding, None);
}

#[test]
fn checkpoint_ids_are_immutable_including_the_bootstrap_checkpoint() {
    let (_directory, store) = create();
    let mut conflicting = state();
    let mut zone = conflicting.zone().expect("fixture has Zone state").clone();
    zone.tempo_gas_rate = 1;
    conflicting
        .apply(&crate::kernel::StateDelta::from_sorted_writes(vec![(
            crate::kernel::StateKey::Zone,
            Some(crate::kernel::StateValue::Zone(zone)),
        )]))
        .unwrap();
    assert!(matches!(
        store.checkpoint(identity(), bootstrap(), conflicting),
        Err(PersistenceError::Invalid(_))
    ));
    assert_eq!(store.load().unwrap().state, state());
}
