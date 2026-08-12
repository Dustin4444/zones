use std::{
    fs,
    time::{Duration, Instant},
};

use crate::kernel::{
    Datum, ExpectedState, FindingCategory, FindingLocation, ImportedFacts, PortalIdentity, State,
    TransitionCandidate, ZoneFacts, ZoneOperation, apply_imported, apply_zone,
};
use alloy_primitives::{Address, B256};
use tempfile::TempDir;

use crate::{
    failure::{Failure, FailureClass},
    notification::NotificationPlan,
    observe::{
        AcquisitionError, AcquisitionSource, AuthenticatedDataEvidence, AuthenticatedTransaction,
        DataSource, EnvelopeRule, ObservationError, PortalCallError, ProtocolChain,
        events::ProtocolEventError,
    },
    persistence::{
        BlockNumHash, ChainCut, Coverage, CoverageGapReason, Identity, Persistence,
        PersistenceError,
    },
    runtime::{
        AuthenticatedBlock, AuthenticatedOutputs, EnqueueAction, RetryBudget, Runtime,
        RuntimeAction, RuntimeState, StreamFailureAction,
    },
};

#[derive(Debug, Clone)]
struct FakeNotification {
    plan: NotificationPlan,
    blocks: Result<Vec<AuthenticatedBlock>, FailureClass>,
}

trait RuntimeTestExt {
    fn push(&mut self, notification: FakeNotification) -> Result<(), ()>;

    fn push_or_record_overflow(
        &mut self,
        store: &Persistence,
        notification: FakeNotification,
    ) -> Result<EnqueueAction, PersistenceError>;
}

impl RuntimeTestExt for Runtime<FakeNotification> {
    fn push(&mut self, notification: FakeNotification) -> Result<(), ()> {
        let plan = notification.plan.clone();
        self.push_planned(notification, plan).map_err(|_| ())
    }

    fn push_or_record_overflow(
        &mut self,
        store: &Persistence,
        notification: FakeNotification,
    ) -> Result<EnqueueAction, PersistenceError> {
        let plan = notification.plan.clone();
        self.push_planned_or_record_overflow(store, notification, plan)
    }
}

fn authenticate(
    notification: &FakeNotification,
    index: usize,
    _parent_state: &State,
) -> Result<AuthenticatedBlock, Failure> {
    notification
        .blocks
        .clone()
        .map(|blocks| blocks[index].clone())
        .map_err(failure)
}

fn failure(class: FailureClass) -> Failure {
    Failure {
        class,
        gap_reason: CoverageGapReason::ProviderUnavailable,
        message: "injected failure".into(),
        finding: None,
    }
}

#[test]
fn observation_errors_map_to_runtime_policy_and_findings() {
    let transaction =
        AuthenticatedTransaction::new(ProtocolChain::ZoneL2, 3, B256::repeat_byte(0x31));
    let evidence = AuthenticatedDataEvidence::from_bytes(b"malformed authenticated bytes");
    let cases = [
        (
            "unavailable acquisition",
            AcquisitionError::unavailable(AcquisitionSource::L1Block, "offline").into(),
            FailureClass::TransientRetry,
            CoverageGapReason::ProviderUnavailable,
            None,
        ),
        (
            "missing acquisition",
            AcquisitionError::missing(AcquisitionSource::L1Receipts, "block").into(),
            FailureClass::BoundedRetry,
            CoverageGapReason::MissingTempoData,
            None,
        ),
        (
            "inconsistent acquisition",
            AcquisitionError::inconsistent(AcquisitionSource::L1Transaction, "expected", "actual")
                .into(),
            FailureClass::BoundedRetry,
            CoverageGapReason::MissingTempoData,
            None,
        ),
        (
            "malformed authenticated data",
            ObservationError::malformed(
                DataSource::AdvanceTempoCalldata,
                transaction,
                evidence,
                "bad encoding",
            ),
            FailureClass::AuthenticatedDivergence,
            CoverageGapReason::NotCheckedAncestorDivergence,
            Some((
                110,
                FindingLocation::Operation(3),
                Datum::Bytes {
                    length: evidence.length(),
                    digest: evidence.digest(),
                },
            )),
        ),
        (
            "invalid envelope",
            ObservationError::invalid_envelope(3, EnvelopeRule::AdvanceSystemCaller),
            FailureClass::AuthenticatedDivergence,
            CoverageGapReason::NotCheckedAncestorDivergence,
            Some((120, FindingLocation::Block, Datum::Code(120))),
        ),
        (
            "protocol event",
            ObservationError::protocol_event(
                ProtocolChain::TempoL1,
                3,
                1,
                2,
                B256::repeat_byte(0x32),
                ProtocolEventError::UnsupportedProtocolEvent {
                    emitter: Address::repeat_byte(0x33),
                    topic0: None,
                },
            ),
            FailureClass::AuthenticatedDivergence,
            CoverageGapReason::NotCheckedAncestorDivergence,
            Some((130, FindingLocation::Operation(3), Datum::Code(130))),
        ),
        (
            "portal call",
            PortalCallError::ConflictingFamilies {
                transaction_hash: B256::repeat_byte(0x34),
            }
            .into(),
            FailureClass::AuthenticatedDivergence,
            CoverageGapReason::NotCheckedAncestorDivergence,
            Some((140, FindingLocation::Block, Datum::Code(140))),
        ),
    ];

    for (name, error, class, gap_reason, expected_finding) in cases {
        let failure = Failure::from(error);
        assert_eq!(failure.class, class, "{name}");
        assert_eq!(failure.gap_reason, gap_reason, "{name}");
        assert!(!failure.message.is_empty(), "{name}");
        match (failure.finding.as_deref(), expected_finding) {
            (None, None) => {}
            (Some(finding), Some((code, location, actual))) => {
                assert_eq!(finding.category, FindingCategory::Observation, "{name}");
                assert_eq!(finding.code, code, "{name}");
                assert_eq!(finding.location.as_ref(), Some(&location), "{name}");
                assert_eq!(finding.expected, None, "{name}");
                assert_eq!(finding.actual.as_ref(), Some(&actual), "{name}");
            }
            (actual, expected) => panic!("{name}: finding mismatch: {actual:?} != {expected:?}"),
        }
    }
}

fn coordinate(number: u64, fork: u8) -> BlockNumHash {
    BlockNumHash {
        number,
        hash: B256::repeat_byte(fork.wrapping_add(number as u8)),
    }
}

fn identity() -> Identity {
    Identity {
        l1_chain_id: 1,
        zone_chain_id: 2,
        zone_id: 7,
        portal: Address::repeat_byte(0x70),
        creation_block: B256::repeat_byte(0xc0),
        creation_height: 0,
    }
}

fn portal() -> PortalIdentity {
    PortalIdentity {
        portal: identity().portal,
        zone_id: identity().zone_id,
        initial_token: Address::repeat_byte(0x11),
    }
}

fn anchor() -> ChainCut {
    ChainCut {
        zone: coordinate(0, 0x10),
        tempo: coordinate(0, 0x40),
    }
}

pub(crate) fn create() -> (TempDir, Persistence) {
    let directory = tempfile::tempdir().unwrap();
    let (store, _) = Persistence::create(
        directory.path(),
        identity(),
        anchor(),
        State::awaiting(portal()),
    )
    .unwrap();
    (directory, store)
}

/// Build observed outputs without the observation adapter to test the
/// runtime/kernel boundary.
fn outputs(candidate: &TransitionCandidate) -> AuthenticatedOutputs {
    AuthenticatedOutputs {
        effects: candidate.expected_effects.to_vec(),
        state: ExpectedState {
            tempo_block_hash: candidate.expected_state.tempo_block_hash,
            tempo_block_number: candidate.expected_state.tempo_block_number,
            processed_deposit_hash: candidate.expected_state.processed_deposit_hash,
            processed_deposit_number: candidate.expected_state.processed_deposit_number,
            withdrawal_queue_hash: candidate.expected_state.withdrawal_queue_hash,
            withdrawal_batch_index: candidate.expected_state.withdrawal_batch_index,
        },
        supplies: candidate
            .expected_accounting
            .iter()
            .map(|(token, value)| (*token, value.supply))
            .collect(),
    }
}

fn blocks(state: &State, first: u64, count: usize, fork: u8) -> Vec<AuthenticatedBlock> {
    let mut state = state.clone();
    let mut parent = coordinate(first - 1, fork);
    (first..first + count as u64)
        .map(|number| {
            let imported = ImportedFacts {
                block_hash: coordinate(number, 0x40).hash,
                block_number: number,
                ..Default::default()
            };
            let zone_facts = ZoneFacts {
                operations: vec![ZoneOperation::UpdateTempoGasRate(number as u128)],
                ..Default::default()
            };
            let candidate =
                apply_zone(apply_imported(&state, &imported).unwrap(), &zone_facts).unwrap();
            let block = AuthenticatedBlock {
                zone: coordinate(number, fork),
                parent,
                tempo: coordinate(number, 0x40),
                tempo_parent: coordinate(number - 1, 0x40),
                imported,
                zone_facts,
                outputs: outputs(&candidate),
            };
            state.apply(&candidate.delta).unwrap();
            parent = block.zone;
            block
        })
        .collect()
}

fn notification(blocks: Vec<AuthenticatedBlock>) -> FakeNotification {
    let applied: Vec<_> = blocks.iter().map(|block| block.zone).collect();
    let first = *applied.first().expect("fixture must contain a block");
    FakeNotification {
        plan: NotificationPlan::new(
            Vec::new(),
            applied,
            BlockNumHash {
                number: first.number - 1,
                hash: coordinate(first.number - 1, 0x10).hash,
            },
        )
        .expect("fixture plan is contiguous"),
        blocks: Ok(blocks),
    }
}

fn runtime(store: &Persistence, attempts: u32) -> Runtime<FakeNotification> {
    Runtime::new(
        store.load().unwrap(),
        2,
        RetryBudget::new(attempts, Duration::from_secs(60)),
    )
}

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
        RuntimeAction::Terminal
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

#[test]
fn production_publication_rejects_existing_target_and_reopens() {
    let parent = tempfile::tempdir().unwrap();
    let occupied = parent.path().join("occupied");
    fs::create_dir(&occupied).unwrap();
    assert!(
        Persistence::create_atomic(&occupied, identity(), anchor(), State::awaiting(portal()),)
            .is_err()
    );

    let target = parent.path().join("checkpoint");
    let snapshot =
        Persistence::create_atomic(&target, identity(), anchor(), State::awaiting(portal()))
            .unwrap();
    let (_, reopened) = Persistence::open(&target, identity()).unwrap();
    assert_eq!(snapshot, reopened);
}

#[test]
fn failed_production_publication_removes_staging_directory() {
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("checkpoint");
    let mut wrong_identity = identity();
    wrong_identity.zone_id += 1;

    assert!(
        Persistence::create_atomic(&target, wrong_identity, anchor(), State::awaiting(portal()),)
            .is_err()
    );
    assert!(!target.exists());
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
}

#[test]
fn bounded_queue_keeps_one_current_and_preserves_order() {
    let (_directory, store) = create();
    let mut runtime = Runtime::new(
        store.load().unwrap(),
        2,
        RetryBudget::new(1, Duration::ZERO),
    );
    assert_eq!(runtime.state(), RuntimeState::Starting);
    let plan = NotificationPlan {
        reverted: vec![],
        ancestor: BlockNumHash {
            number: 0,
            hash: B256::ZERO,
        },
        applied: vec![BlockNumHash {
            number: 1,
            hash: B256::repeat_byte(1),
        }],
        acknowledge: BlockNumHash {
            number: 1,
            hash: B256::repeat_byte(1),
        },
    };
    assert!(runtime.enqueue(1, plan.clone()).is_ok());
    assert!(runtime.enqueue(2, plan.clone()).is_ok());
    assert!(runtime.enqueue(3, plan.clone()).is_ok());
    assert_eq!(runtime.enqueue(4, plan), Err(4));
    runtime.advance();
    assert_eq!(
        runtime.current().map(|(notification, _)| notification),
        Some(&2)
    );
    runtime.advance();
    assert_eq!(
        runtime.current().map(|(notification, _)| notification),
        Some(&3)
    );
}
#[test]
fn attempt_and_elapsed_budgets_are_both_terminal() {
    let attempts = RetryBudget::new(2, Duration::from_secs(60));
    let now = Instant::now();
    assert!(!attempts.exhausted(1, now, now));
    assert!(attempts.exhausted(2, now, now));
    let elapsed = RetryBudget::new(99, Duration::ZERO);
    assert!(elapsed.exhausted(0, now, now));
}
