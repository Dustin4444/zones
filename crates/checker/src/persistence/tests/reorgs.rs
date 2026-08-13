//! Reorgs tests.

use super::*;

#[test]
fn reorg_before_after_and_across_checkpoints_reconstructs_exact_metadata() {
    let (_directory, store) = create();
    let one = apply(&store, 1, bootstrap().zone);
    let snapshot = store.load().unwrap();
    store
        .checkpoint(
            identity(),
            ChainCut {
                zone: one,
                tempo: block(1, 0x21),
            },
            snapshot.state,
        )
        .unwrap();
    let two = apply(&store, 2, one);
    let three = apply(&store, 3, two);

    assert_eq!(
        store
            .reorg(&current(&store), two)
            .unwrap()
            .meta
            .verified_zone_tip,
        two
    );
    let replacement_three = apply(&store, 3, two);
    assert_eq!(replacement_three, three);
    assert_eq!(
        store
            .reorg(&current(&store), bootstrap().zone)
            .unwrap()
            .meta
            .active_checkpoint
            .height,
        0
    );
    assert_eq!(
        store.load().unwrap().meta.verified_zone_tip,
        bootstrap().zone
    );
}

#[test]
fn unverified_reorg_reanchors_gap_boundaries() {
    for (ancestor, expected_first) in [
        (block(2, 0x52), block(1, 0x41)),
        (block(1, 0x51), block(1, 0x51)),
    ] {
        let (directory, store) = create();
        store
            .record_gap(
                &current(&store),
                block(1, 0x41),
                block(3, 0x43),
                CoverageGapReason::ProviderUnavailable,
            )
            .unwrap();

        let snapshot = store.reorg(&current(&store), ancestor).unwrap();
        assert_eq!(snapshot.meta.acknowledged_zone_tip, ancestor);
        assert_eq!(
            snapshot.meta.coverage,
            Coverage::Gap {
                first_unchecked: expected_first,
                acknowledged_through: ancestor,
                reason: CoverageGapReason::ProviderUnavailable,
            }
        );

        drop(store);
        let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
        assert_eq!(reopened.meta.coverage, snapshot.meta.coverage);
    }
}

#[test]
fn alert_descendant_reorg_preserves_or_removes_the_latch_by_exact_height() {
    let (_directory, store) = create();
    let finding_block = block(1, 0x41);
    let (key, value) = finding(finding_block);
    store.record_finding(&current(&store), key, value).unwrap();
    store
        .record_gap(
            &current(&store),
            finding_block,
            block(3, 0x43),
            CoverageGapReason::NotCheckedAncestorDivergence,
        )
        .unwrap();
    assert_eq!(
        store
            .reorg(&current(&store), block(2, 0x42))
            .unwrap()
            .meta
            .active_finding,
        Some(key)
    );
    assert!(matches!(
        store.reorg(&current(&store), block(1, 0xff)),
        Err(PersistenceError::Invalid(_))
    ));
    assert_eq!(
        store
            .reorg(&current(&store), bootstrap().zone)
            .unwrap()
            .meta
            .active_finding,
        None
    );
}

#[test]
fn deep_reorg_retains_orphan_finding_as_structural_audit_record() {
    let (directory, store) = create();
    let one = apply(&store, 1, bootstrap().zone);
    let _two = apply(&store, 2, one);
    let finding_block = block(3, 0x43);
    let (key, value) = finding(finding_block);
    store.record_finding(&current(&store), key, value).unwrap();
    store
        .record_gap(
            &current(&store),
            finding_block,
            block(4, 0x44),
            CoverageGapReason::NotCheckedAncestorDivergence,
        )
        .unwrap();
    assert_eq!(
        store
            .reorg(&current(&store), bootstrap().zone)
            .unwrap()
            .meta
            .active_finding,
        None
    );
    drop(store);
    let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(reopened.meta.verified_zone_tip, bootstrap().zone);
    assert_eq!(reopened.meta.active_finding, None);
}

#[test]
fn retention_advances_the_recovery_floor_and_rejects_older_reorgs() {
    let (_directory, mut store) = create();
    store.set_retention_for_tests(2, 4);

    let mut parent = bootstrap().zone;
    for number in 1..=8 {
        parent = apply(&store, number, parent);
    }

    let snapshot = current(&store);
    assert_eq!(
        snapshot.meta.recovery_checkpoint,
        super::super::CheckpointId::from(block(4, 0x14))
    );
    assert_eq!(
        snapshot.meta.active_checkpoint,
        super::super::CheckpointId::from(block(8, 0x18))
    );

    let tx = store.db.tx().unwrap();
    let mut journal = tx.cursor_read::<Journal>().unwrap();
    assert_eq!(journal.first().unwrap().map(|(height, _)| height), Some(5));
    assert_eq!(journal.last().unwrap().map(|(height, _)| height), Some(8));
    let checkpoints = tx
        .cursor_read::<Checkpoints>()
        .unwrap()
        .walk(None)
        .unwrap()
        .count();
    assert_eq!(checkpoints, 3);
    tx.commit().unwrap();

    assert!(matches!(
        store.reorg(&snapshot, block(3, 0x13)),
        Err(PersistenceError::ReorgBeyondRetention { .. })
    ));
    assert_eq!(
        store
            .reorg(&snapshot, block(4, 0x14))
            .unwrap()
            .meta
            .verified_zone_tip,
        block(4, 0x14)
    );
    assert!(matches!(
        store.reorg(&snapshot, block(4, 0x44)),
        Err(PersistenceError::ReorgBeyondRetention { .. })
    ));
}

#[test]
fn retained_history_survives_restart_and_rejects_a_missing_middle_entry() {
    let (directory, mut store) = create();
    store.set_retention_for_tests(2, 4);
    let mut parent = bootstrap().zone;
    for number in 1..=8 {
        parent = apply(&store, number, parent);
    }
    let expected = current(&store);
    drop(store);

    let (store, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(reopened, expected);
    let tx = store.db.tx_mut().unwrap();
    let mut journal = tx.cursor_write::<Journal>().unwrap();
    journal.seek_exact(6).unwrap();
    journal.delete_current().unwrap();
    tx.commit().unwrap();
    assert!(matches!(store.load(), Err(PersistenceError::Invalid(_))));
}

#[test]
fn retained_reorg_installs_its_ancestor_as_the_active_checkpoint() {
    let (directory, mut store) = create();
    store.set_retention_for_tests(2, 4);
    let mut parent = bootstrap().zone;
    for number in 1..=8 {
        parent = apply(&store, number, parent);
    }

    let ancestor = block(6, 0x16);
    let snapshot = store.reorg(&current(&store), ancestor).unwrap();
    assert_eq!(
        snapshot.meta.active_checkpoint,
        super::super::CheckpointId::from(ancestor)
    );
    drop(store);

    let (_store, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(reopened, snapshot);
}
