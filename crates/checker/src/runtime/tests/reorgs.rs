//! Reorgs tests.

use super::*;

#[test]
fn finding_descendants_stay_alerting_and_reorg_recovers_before_replacement() {
    let (_directory, store) = create();
    let state = store.load().unwrap().state;
    let mut bad = blocks(&state, 1, 1, 0x10);
    bad[0].outputs.state.tempo_block_hash = B256::ZERO;
    let mut runtime = runtime(&store, 2);
    runtime.push(notification(bad)).unwrap();
    runtime
        .poll(&store, identity(), authenticate, Instant::now())
        .unwrap();
    runtime
        .push(notification(blocks(&state, 2, 2, 0x10)))
        .unwrap();
    assert_eq!(
        runtime
            .poll(&store, identity(), authenticate, Instant::now())
            .unwrap(),
        RuntimeAction::Acknowledge(coordinate(3, 0x10))
    );
    assert_eq!(runtime.state(), RuntimeState::Alerting);
    assert!(
        matches!(store.load().unwrap().meta.coverage, Coverage::Gap { reason: CoverageGapReason::NotCheckedAncestorDivergence, acknowledged_through, .. } if acknowledged_through == coordinate(3, 0x10))
    );
    runtime.reorg(&store, anchor().zone).unwrap();
    assert_eq!(runtime.state(), RuntimeState::Starting);
    let mut replacement = blocks(&store.load().unwrap().state, 1, 1, 0x30);
    replacement[0].parent = anchor().zone;
    runtime.push(notification(replacement)).unwrap();
    runtime
        .poll(&store, identity(), authenticate, Instant::now())
        .unwrap();
    assert_eq!(runtime.state(), RuntimeState::Healthy);
}

#[test]
fn restart_catchup_containing_active_finding_never_authenticates() {
    let (_directory, store) = create();
    let state = store.load().unwrap().state;
    let mut bad = blocks(&state, 1, 1, 0x10);
    bad[0].outputs.state.tempo_block_hash = B256::ZERO;
    let mut first_runtime = runtime(&store, 2);
    first_runtime.push(notification(bad)).unwrap();
    first_runtime
        .poll(&store, identity(), authenticate, Instant::now())
        .unwrap();
    store
        .record_gap(
            &store.load().unwrap(),
            coordinate(1, 0x10),
            coordinate(3, 0x10),
            CoverageGapReason::NotCheckedAncestorDivergence,
        )
        .unwrap();

    let mut restarted = runtime(&store, 2);
    restarted
        .push(FakeNotification {
            plan: NotificationPlan::new(
                Vec::new(),
                vec![
                    coordinate(1, 0x10),
                    coordinate(2, 0x10),
                    coordinate(3, 0x10),
                ],
                anchor().zone,
            )
            .unwrap(),
            blocks: Err(FailureClass::ImmediateTerminal),
        })
        .unwrap();
    assert_eq!(
        restarted
            .poll(&store, identity(), authenticate, Instant::now())
            .unwrap(),
        RuntimeAction::Acknowledge(coordinate(3, 0x10))
    );
    assert_eq!(restarted.state(), RuntimeState::Alerting);
}

#[test]
fn active_finding_rejects_same_height_wrong_ancestor_hash() {
    let (_directory, store) = create();
    let state = store.load().unwrap().state;
    let mut bad = blocks(&state, 1, 1, 0x10);
    bad[0].outputs.state.tempo_block_hash = B256::ZERO;
    let mut runtime = runtime(&store, 2);
    runtime.push(notification(bad)).unwrap();
    runtime
        .poll(&store, identity(), authenticate, Instant::now())
        .unwrap();
    store
        .record_gap(
            &store.load().unwrap(),
            coordinate(1, 0x10),
            coordinate(3, 0x10),
            CoverageGapReason::NotCheckedAncestorDivergence,
        )
        .unwrap();
    runtime
        .push(FakeNotification {
            plan: NotificationPlan {
                reverted: vec![],
                ancestor: coordinate(3, 0x50),
                applied: vec![coordinate(4, 0x50)],
                acknowledge: coordinate(4, 0x50),
            },
            blocks: Ok(blocks(&state, 4, 1, 0x50)),
        })
        .unwrap();
    assert_eq!(
        runtime
            .poll(&store, identity(), authenticate, Instant::now())
            .unwrap(),
        RuntimeAction::Blocked
    );
    assert_eq!(
        store.load().unwrap().meta.acknowledged_zone_tip,
        coordinate(3, 0x10)
    );
}

#[test]
fn notification_plan_rejects_malformed_coordinates() {
    assert!(NotificationPlan::new(Vec::new(), Vec::new(), anchor().zone).is_err());
    assert!(
        NotificationPlan::new(
            Vec::new(),
            vec![coordinate(1, 0x10), coordinate(3, 0x10)],
            anchor().zone,
        )
        .is_err()
    );
}
