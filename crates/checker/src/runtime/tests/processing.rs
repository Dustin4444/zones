//! Processing tests.

use super::*;

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
        RuntimeAction::AcknowledgeAndTerminate(coordinate(3, 0x10))
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
        EnqueueAction::AcknowledgeAndTerminate(coordinate(4, 0x10))
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
