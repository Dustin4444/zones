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
            snapshot.state.as_ref().clone()
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
fn divergence_abort_leaves_finding_and_coverage_fully_old() {
    let (directory, store) = create();
    let (key, value) = finding(block(1, 0x41));
    store.inject_abort();
    assert!(matches!(
        store.record_divergence(&current(&store), key, value, key.zone),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(store.load().unwrap().meta.active_finding, None);
    drop(store);
    let (_, snapshot) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(snapshot.meta.observed_zone_tip, bootstrap().zone);
    assert_eq!(snapshot.meta.coverage, Coverage::Complete);
    assert_eq!(snapshot.meta.active_finding, None);
}

#[test]
fn final_metadata_validation_aborts_the_finding_write() {
    let (_directory, store) = create();
    let prior = current(&store);
    let finding_block = block(1, 0x11);
    let conflicting_endpoint = block(1, 0x21);
    let (key, value) = finding(finding_block);

    assert!(matches!(
        store.record_divergence(&prior, key, value, conflicting_endpoint),
        Err(PersistenceError::Invalid(_))
    ));
    assert_eq!(current(&store), prior);

    let tx = store.db.tx().unwrap();
    assert_eq!(tx.get::<Findings>(key).unwrap(), None);
    tx.commit().unwrap();
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
    assert_eq!(*store.load().unwrap().state, state());
}

#[test]
fn aborting_a_retention_checkpoint_leaves_history_unchanged() {
    let (directory, mut store) = create();
    store.set_retention_for_tests(2, 4);
    let mut parent = bootstrap().zone;
    for number in 1..=7 {
        parent = apply(&store, number, parent);
    }
    let prior = current(&store);
    store.inject_abort();
    assert!(matches!(
        store.apply(&prior, entry(8, parent), block(8, 0x18), Coverage::Complete,),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(current(&store), prior);
    drop(store);
    let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(reopened, prior);
}
