//! Processing tests.

use super::*;
use crate::CheckerBlockedReason;

#[test]
fn apply_abort_never_acknowledges_and_keeps_parent_tip() {
    let (_directory, store) = create();
    let mut runtime = runtime(&store, 2);
    runtime
        .push(notification(blocks(
            &store.load().unwrap().state,
            1,
            1,
            0x10,
        )))
        .unwrap();
    store.inject_abort();
    assert!(matches!(
        runtime.poll(&store, identity(), authenticate, Instant::now()),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(store.load().unwrap().meta.verified_zone_tip, anchor().zone);
}

#[test]
fn terminal_failure_records_why_acknowledgement_stopped() {
    let (_directory, store) = create();
    let mut runtime = runtime(&store, 2);
    runtime
        .push(FakeNotification {
            plan: NotificationPlan::new(Vec::new(), vec![coordinate(1, 0x10)], anchor().zone)
                .unwrap(),
            blocks: Err(FailureClass::ImmediateTerminal),
        })
        .unwrap();

    assert_eq!(
        runtime
            .poll(&store, identity(), authenticate, Instant::now())
            .unwrap(),
        RuntimeAction::Blocked
    );
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.meta.acknowledged_zone_tip, anchor().zone);
    assert_eq!(
        snapshot.meta.blocked,
        Some(CheckerBlockedReason::InvalidAuthenticatedData)
    );
}

#[test]
fn committed_prefix_then_divergence_records_exact_second_gap_before_ack() {
    let (_directory, store) = create();
    let mut values = blocks(&store.load().unwrap().state, 1, 2, 0x10);
    values[1].outputs.state.tempo_block_number += 1;
    let second = values[1].zone;
    let mut runtime = runtime(&store, 2);
    runtime.push(notification(values)).unwrap();
    assert_eq!(
        runtime
            .poll(&store, identity(), authenticate, Instant::now())
            .unwrap(),
        RuntimeAction::None
    );
    assert_eq!(store.load().unwrap().meta.verified_zone_tip.number, 1);
    assert_eq!(
        runtime
            .poll(&store, identity(), authenticate, Instant::now())
            .unwrap(),
        RuntimeAction::Acknowledge(second)
    );
    assert_eq!(
        store.load().unwrap().meta.coverage,
        Coverage::Gap {
            first_unchecked: second,
            acknowledged_through: second,
            reason: CoverageGapReason::NotCheckedAncestorDivergence
        }
    );
}

#[test]
fn retry_spans_polls_accepts_queued_work_and_names_full_suffix() {
    let (_directory, store) = create();
    let coordinates = vec![coordinate(1, 0x10), coordinate(2, 0x10)];
    let failing = FakeNotification {
        plan: NotificationPlan::new(Vec::new(), coordinates.clone(), anchor().zone).unwrap(),
        blocks: Err(FailureClass::TransientRetry),
    };
    let mut runtime = runtime(&store, 2);
    runtime.push(failing).unwrap();
    let now = Instant::now();
    assert!(matches!(
        runtime.poll(&store, identity(), authenticate, now).unwrap(),
        RuntimeAction::RetryAt(_)
    ));
    runtime
        .push(notification(blocks(
            &store.load().unwrap().state,
            3,
            1,
            0x10,
        )))
        .unwrap();
    assert_eq!(
        runtime
            .poll(
                &store,
                identity(),
                authenticate,
                now + Duration::from_millis(25)
            )
            .unwrap(),
        RuntimeAction::AcknowledgeAndDisable(coordinate(3, 0x10))
    );
    assert_eq!(
        store.load().unwrap().meta.coverage,
        Coverage::Gap {
            first_unchecked: coordinates[0],
            acknowledged_through: coordinate(3, 0x10),
            reason: CoverageGapReason::ProviderUnavailable
        }
    );
}

#[test]
fn durable_gap_recovers_across_separate_notifications() {
    let (_directory, store) = create();
    let all = blocks(&store.load().unwrap().state, 1, 3, 0x10);
    store
        .record_gap(
            &store.load().unwrap(),
            all[0].zone,
            all[2].zone,
            CoverageGapReason::MissingReceipts,
        )
        .unwrap();
    let mut runtime = runtime(&store, 2);
    for block in &all {
        runtime.push(notification(vec![block.clone()])).unwrap();
    }
    for _ in 0..3 {
        assert!(matches!(
            runtime
                .poll(&store, identity(), authenticate, Instant::now())
                .unwrap(),
            RuntimeAction::Acknowledge(_)
        ));
    }
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.meta.coverage, Coverage::Complete);
    assert_eq!(snapshot.meta.verified_zone_tip, all[2].zone);
    assert_eq!(snapshot.meta.acknowledged_zone_tip, all[2].zone);
}

#[test]
fn gap_recovery_reanchors_replaced_first_and_endpoint_hashes() {
    for (case, first_replaced) in [("inside the gap", 1), ("at the gap start", 0)] {
        let (directory, store) = create();
        let state = store.load().unwrap().state;
        let original = blocks(&state, 1, 3, 0x10);
        store
            .record_gap(
                &store.load().unwrap(),
                original[0].zone,
                original[2].zone,
                CoverageGapReason::ProviderUnavailable,
            )
            .unwrap();

        let mut canonical = original.clone();
        for index in first_replaced..canonical.len() {
            canonical[index].zone = coordinate(index as u64 + 1, 0x20);
            if index > 0 {
                canonical[index].parent = canonical[index - 1].zone;
            }
        }
        let canonical_tip = canonical[2].zone;
        let mut runtime = runtime(&store, 2);
        runtime.push(notification(canonical)).unwrap();
        for _ in 0..3 {
            assert!(
                matches!(
                    runtime
                        .poll(&store, identity(), authenticate, Instant::now())
                        .unwrap(),
                    RuntimeAction::Acknowledge(_) | RuntimeAction::None
                ),
                "{case}"
            );
        }
        let snapshot = store.load().unwrap();
        assert_eq!(snapshot.meta.coverage, Coverage::Complete, "{case}");
        assert_eq!(snapshot.meta.verified_zone_tip, canonical_tip, "{case}");
        assert_eq!(snapshot.meta.acknowledged_zone_tip, canonical_tip, "{case}");
        drop(store);
        let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
        assert_eq!(reopened.meta.coverage, Coverage::Complete, "{case}");
        assert_eq!(reopened.meta.verified_zone_tip, canonical_tip, "{case}");
    }
}

#[test]
fn divergence_upgrades_an_existing_gap_atomically() {
    let (directory, store) = create();
    let mut values = blocks(&store.load().unwrap().state, 1, 3, 0x10);
    let first = values[0].zone;
    let through = values[2].zone;
    store
        .record_gap(
            &store.load().unwrap(),
            first,
            through,
            CoverageGapReason::ProviderUnavailable,
        )
        .unwrap();
    values[0].outputs.state.tempo_block_number += 1;
    let mut runtime = runtime(&store, 2);
    runtime.push(notification(values)).unwrap();
    assert_eq!(
        runtime
            .poll(&store, identity(), authenticate, Instant::now())
            .unwrap(),
        RuntimeAction::Acknowledge(through)
    );
    let snapshot = store.load().unwrap();
    assert!(snapshot.meta.active_finding.is_some());
    assert_eq!(
        snapshot.meta.coverage,
        Coverage::Gap {
            first_unchecked: first,
            acknowledged_through: through,
            reason: CoverageGapReason::NotCheckedAncestorDivergence,
        }
    );
    drop(store);
    let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert!(reopened.meta.active_finding.is_some());
    assert_eq!(reopened.meta.coverage, snapshot.meta.coverage);
}

#[test]
fn authentication_divergence_preserves_a_wider_gap_acknowledgement() {
    let (_directory, store) = create();
    let values = blocks(&store.load().unwrap().state, 1, 3, 0x10);
    let first = values[0].zone;
    let through = values[2].zone;
    store
        .record_gap(
            &store.load().unwrap(),
            first,
            through,
            CoverageGapReason::ProviderUnavailable,
        )
        .unwrap();
    let failure = Failure::from(ObservationError::invalid_envelope(
        0,
        EnvelopeRule::AdvanceSystemCaller,
    ));
    let mut runtime = runtime(&store, 2);
    runtime
        .push(FakeNotification {
            plan: NotificationPlan::new(Vec::new(), vec![first], anchor().zone).unwrap(),
            blocks: Ok(vec![values[0].clone()]),
        })
        .unwrap();
    runtime.push(notification(vec![values[1].clone()])).unwrap();
    let replacement_through = coordinate(3, 0x20);
    runtime
        .push(FakeNotification {
            plan: NotificationPlan::new(Vec::new(), vec![replacement_through], values[1].zone)
                .unwrap(),
            blocks: Ok(Vec::new()),
        })
        .unwrap();
    assert_eq!(
        runtime
            .poll(
                &store,
                identity(),
                |_notification, _index, _state| Err(failure.clone()),
                Instant::now(),
            )
            .unwrap(),
        RuntimeAction::Acknowledge(through)
    );
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.meta.acknowledged_zone_tip, through);
    assert_eq!(
        snapshot.meta.coverage,
        Coverage::Gap {
            first_unchecked: first,
            acknowledged_through: through,
            reason: CoverageGapReason::NotCheckedAncestorDivergence,
        }
    );
    assert_eq!(
        runtime
            .poll(&store, identity(), authenticate, Instant::now())
            .unwrap(),
        RuntimeAction::Acknowledge(through)
    );
    assert_eq!(
        runtime
            .poll(&store, identity(), authenticate, Instant::now())
            .unwrap(),
        RuntimeAction::Acknowledge(replacement_through)
    );
    assert_eq!(
        store.load().unwrap().meta.coverage,
        Coverage::Gap {
            first_unchecked: first,
            acknowledged_through: replacement_through,
            reason: CoverageGapReason::NotCheckedAncestorDivergence,
        }
    );
}

#[test]
fn divergence_and_gap_upgrade_abort_together() {
    let (_directory, store) = create();
    let mut values = blocks(&store.load().unwrap().state, 1, 2, 0x10);
    let first = values[0].zone;
    let through = values[1].zone;
    let original = Coverage::Gap {
        first_unchecked: first,
        acknowledged_through: through,
        reason: CoverageGapReason::ProviderUnavailable,
    };
    store
        .record_gap(
            &store.load().unwrap(),
            first,
            through,
            CoverageGapReason::ProviderUnavailable,
        )
        .unwrap();
    values[0].outputs.state.tempo_block_number += 1;
    let mut runtime = runtime(&store, 2);
    runtime.push(notification(values)).unwrap();
    store.inject_abort();
    assert!(matches!(
        runtime.poll(&store, identity(), authenticate, Instant::now()),
        Err(PersistenceError::InjectedAbort)
    ));
    let snapshot = store.load().unwrap();
    assert_eq!(snapshot.meta.active_finding, None);
    assert_eq!(snapshot.meta.coverage, original);
}

#[test]
fn overflow_and_stream_failure_are_durable_fail_open_apis() {
    let (directory, store) = create();
    let state = store.load().unwrap().state;
    let mut overflow_runtime = Runtime::new(
        store.load().unwrap(),
        1,
        RetryBudget::new(2, Duration::from_secs(1)),
    );
    overflow_runtime
        .push(notification(blocks(&state, 1, 1, 0x10)))
        .unwrap();
    overflow_runtime
        .push(notification(blocks(&state, 2, 1, 0x10)))
        .unwrap();
    let rejected = notification(blocks(&state, 3, 2, 0x10));
    assert_eq!(
        overflow_runtime
            .push_or_record_overflow(&store, rejected)
            .unwrap(),
        EnqueueAction::AcknowledgeAndDisable(coordinate(4, 0x10))
    );
    assert_eq!(
        store.load().unwrap().meta.coverage,
        Coverage::Gap {
            first_unchecked: coordinate(1, 0x10),
            acknowledged_through: coordinate(4, 0x10),
            reason: CoverageGapReason::ProviderUnavailable
        }
    );
    drop(store);
    let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert!(
        matches!(reopened.meta.coverage, Coverage::Gap { acknowledged_through, .. } if acknowledged_through == coordinate(4, 0x10))
    );

    let (stream_directory, store) = create();
    let mut runtime = runtime(&store, 2);
    let suffix = [
        coordinate(1, 0x10),
        coordinate(2, 0x10),
        coordinate(3, 0x10),
    ];
    assert_eq!(
        runtime.record_stream_failure(&store, &suffix).unwrap(),
        StreamFailureAction::GapRecorded(suffix[2])
    );
    assert_eq!(
        store.load().unwrap().meta.coverage,
        Coverage::Gap {
            first_unchecked: suffix[0],
            acknowledged_through: suffix[2],
            reason: CoverageGapReason::ProviderUnavailable
        }
    );
    drop(store);
    let (_, reopened) = Persistence::open(stream_directory.path(), identity()).unwrap();
    assert_eq!(
        reopened.meta.coverage,
        Coverage::Gap {
            first_unchecked: suffix[0],
            acknowledged_through: suffix[2],
            reason: CoverageGapReason::ProviderUnavailable
        }
    );
}

#[test]
fn disabled_runtime_extends_and_replaces_its_durable_gap_before_acknowledging() {
    let (_directory, store) = create();
    let state = store.load().unwrap().state;
    let mut runtime = Runtime::new(
        store.load().unwrap(),
        1,
        RetryBudget::new(2, Duration::from_secs(1)),
    );
    for number in 1..=2 {
        runtime
            .push(notification(blocks(&state, number, 1, 0x10)))
            .unwrap();
    }
    assert_eq!(
        runtime
            .push_or_record_overflow(&store, notification(blocks(&state, 3, 2, 0x10)))
            .unwrap(),
        EnqueueAction::AcknowledgeAndDisable(coordinate(4, 0x10))
    );
    assert_eq!(
        runtime.next_action(&store, Instant::now()).unwrap(),
        RuntimeAction::AwaitNotification
    );

    assert_eq!(
        runtime
            .push_or_record_overflow(&store, notification(blocks(&state, 5, 1, 0x10)))
            .unwrap(),
        EnqueueAction::AcknowledgeAndDisable(coordinate(5, 0x10))
    );
    assert!(matches!(
        store.load().unwrap().meta.coverage,
        Coverage::Gap { first_unchecked, acknowledged_through, .. }
            if first_unchecked == coordinate(1, 0x10) && acknowledged_through == coordinate(5, 0x10)
    ));

    let replacement = FakeNotification {
        plan: NotificationPlan::new(
            vec![
                coordinate(3, 0x10),
                coordinate(4, 0x10),
                coordinate(5, 0x10),
            ],
            vec![
                coordinate(3, 0x20),
                coordinate(4, 0x20),
                coordinate(5, 0x20),
            ],
            coordinate(2, 0x10),
        )
        .unwrap(),
        blocks: Ok(blocks(&state, 3, 3, 0x20)),
    };
    assert_eq!(
        runtime
            .push_or_record_overflow(&store, replacement)
            .unwrap(),
        EnqueueAction::AcknowledgeAndDisable(coordinate(5, 0x20))
    );
    assert!(matches!(
        store.load().unwrap().meta.coverage,
        Coverage::Gap { first_unchecked, acknowledged_through, .. }
            if first_unchecked == coordinate(1, 0x10) && acknowledged_through == coordinate(5, 0x20)
    ));

    let truncated_reorg = FakeNotification {
        plan: NotificationPlan::new(
            vec![coordinate(3, 0x20), coordinate(4, 0x20)],
            vec![
                coordinate(3, 0x30),
                coordinate(4, 0x30),
                coordinate(5, 0x30),
            ],
            coordinate(2, 0x10),
        )
        .unwrap(),
        blocks: Ok(blocks(&state, 3, 3, 0x30)),
    };
    assert_eq!(
        runtime
            .push_or_record_overflow(&store, truncated_reorg)
            .unwrap(),
        EnqueueAction::Blocked
    );
    let snapshot = store.load().unwrap();
    assert_eq!(
        snapshot.meta.blocked,
        Some(CheckerBlockedReason::InvalidNotificationSequence)
    );
    assert_eq!(snapshot.meta.acknowledged_zone_tip, coordinate(5, 0x20));
}

#[test]
fn disabled_runtime_resumes_after_a_reorg_removes_the_entire_gap() {
    let (_directory, store) = create();
    let state = store.load().unwrap().state;
    let mut runtime = Runtime::new(
        store.load().unwrap(),
        1,
        RetryBudget::new(2, Duration::from_secs(1)),
    );
    for number in 1..=2 {
        runtime
            .push(notification(blocks(&state, number, 1, 0x10)))
            .unwrap();
    }
    assert_eq!(
        runtime
            .push_or_record_overflow(&store, notification(blocks(&state, 3, 3, 0x10)))
            .unwrap(),
        EnqueueAction::AcknowledgeAndDisable(coordinate(5, 0x10))
    );

    let reverted = FakeNotification {
        plan: NotificationPlan::new(
            (1..=5).map(|number| coordinate(number, 0x10)).collect(),
            Vec::new(),
            anchor().zone,
        )
        .unwrap(),
        blocks: Ok(Vec::new()),
    };
    assert_eq!(
        runtime.push_or_record_overflow(&store, reverted).unwrap(),
        EnqueueAction::Acknowledge(anchor().zone)
    );
    assert_eq!(store.load().unwrap().meta.coverage, Coverage::Complete);

    let replacement = notification(blocks(&state, 1, 1, 0x10));
    assert_eq!(
        runtime
            .push_or_record_overflow(&store, replacement)
            .unwrap(),
        EnqueueAction::Queued
    );
    assert!(matches!(
        runtime
            .poll(&store, identity(), authenticate, Instant::now())
            .unwrap(),
        RuntimeAction::Acknowledge(_)
    ));
}
