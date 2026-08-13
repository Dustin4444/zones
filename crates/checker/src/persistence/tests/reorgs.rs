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
