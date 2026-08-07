use std::{
    fs,
    time::{Duration, Instant},
};

use alloy_primitives::{Address, B256};
use tempfile::TempDir;
use zone_checker_kernel::{
    Candidate, ExpectedState, ImportedFacts, PortalIdentity, State, ZoneFacts, ZoneOperation,
    apply_imported, apply_zone,
};

use crate::{
    compact::{
        AuthenticatedBlock, AuthenticatedOutputs, BuildConfig, CompactRuntime, Failure,
        FailureClass, NotificationPlan, ObservationPipeline, ObservedEffect, PlannedNotification,
        RetryBudget, RuntimeAction, RuntimeState, build_checkpoint, compare_authenticated,
    },
    persistence::{
        BlockNumHash, ChainCut, Coverage, CoverageGapReason, Identity, Persistence,
        PersistenceError,
    },
};

#[derive(Debug, Clone)]
struct FakeNotification {
    plan: Option<NotificationPlan>,
    coordinates: Result<Vec<BlockNumHash>, ()>,
    blocks: Result<Vec<AuthenticatedBlock>, FailureClass>,
}

impl PlannedNotification for FakeNotification {
    fn plan(&self) -> Result<NotificationPlan, Failure> {
        if let Some(plan) = &self.plan {
            return plan.clone().validate();
        }
        let applied = self
            .coordinates
            .clone()
            .map_err(|()| failure(FailureClass::ImmediateTerminal))?;
        let first = applied
            .first()
            .copied()
            .ok_or_else(|| failure(FailureClass::ImmediateTerminal))?;
        let ancestor = BlockNumHash {
            number: first.number - 1,
            hash: coordinate(first.number - 1, 0x10).hash,
        };
        let acknowledge = *applied.last().unwrap();
        NotificationPlan {
            reverted: vec![],
            ancestor,
            applied,
            acknowledge,
        }
        .validate()
    }
}

#[derive(Default)]
struct FakePipeline;

impl ObservationPipeline<FakeNotification> for FakePipeline {
    fn authenticate_at(
        &mut self,
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

    fn compare(&mut self, block: &AuthenticatedBlock, expected: &Candidate) -> Result<(), Failure> {
        compare_authenticated(block, expected)
    }
}

fn failure(class: FailureClass) -> Failure {
    Failure {
        class,
        gap_reason: CoverageGapReason::ProviderUnavailable,
        message: "injected failure".into(),
        finding: None,
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

fn create() -> (TempDir, Persistence) {
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

/// Assemble observed outputs field-by-field. This intentionally does not use
/// an observation adapter, so these tests exercise the runtime/kernel seam.
fn outputs(candidate: &Candidate) -> AuthenticatedOutputs {
    AuthenticatedOutputs {
        effects: candidate
            .expected_effects
            .iter()
            .map(ObservedEffect::from)
            .collect(),
        state: ExpectedState {
            tempo_block_hash: candidate.expected_state.tempo_block_hash,
            tempo_block_number: candidate.expected_state.tempo_block_number,
            processed_deposit_hash: candidate.expected_state.processed_deposit_hash,
            processed_deposit_number: candidate.expected_state.processed_deposit_number,
            withdrawal_queue_hash: candidate.expected_state.withdrawal_queue_hash,
            withdrawal_batch_index: candidate.expected_state.withdrawal_batch_index,
            collateral_requirement: candidate.expected_state.collateral_requirement,
        },
        supplies: candidate
            .expected_accounting
            .iter()
            .map(|(token, value)| (*token, value.supply))
            .collect(),
        collateral: candidate
            .expected_accounting
            .iter()
            .filter_map(|(token, value)| value.collateral().map(|amount| (*token, amount)))
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
    FakeNotification {
        plan: None,
        coordinates: Ok(blocks.iter().map(|block| block.zone).collect()),
        blocks: Ok(blocks),
    }
}

fn runtime(attempts: u32) -> CompactRuntime<FakeNotification> {
    CompactRuntime::new(2, RetryBudget::new(attempts, Duration::from_secs(60)))
}

#[test]
fn apply_abort_never_acknowledges_and_keeps_parent_tip() {
    let (_directory, store) = create();
    let mut runtime = runtime(2);
    runtime
        .push(notification(blocks(
            &store.load(identity()).unwrap().state,
            1,
            1,
            0x10,
        )))
        .unwrap();
    store.inject_abort();
    assert!(matches!(
        runtime.poll(&store, identity(), &mut FakePipeline, Instant::now()),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(
        store.load(identity()).unwrap().meta.verified_zone_tip,
        anchor().zone
    );
}

#[test]
fn committed_prefix_then_divergence_records_exact_second_gap_before_ack() {
    let (_directory, store) = create();
    let mut values = blocks(&store.load(identity()).unwrap().state, 1, 2, 0x10);
    values[1].outputs.state.tempo_block_number += 1;
    let second = values[1].zone;
    let mut runtime = runtime(2);
    runtime.push(notification(values)).unwrap();
    assert_eq!(
        runtime
            .poll(&store, identity(), &mut FakePipeline, Instant::now())
            .unwrap(),
        RuntimeAction::None
    );
    assert_eq!(
        store
            .load(identity())
            .unwrap()
            .meta
            .verified_zone_tip
            .number,
        1
    );
    assert_eq!(
        runtime
            .poll(&store, identity(), &mut FakePipeline, Instant::now())
            .unwrap(),
        RuntimeAction::Acknowledge(second)
    );
    assert_eq!(
        store.load(identity()).unwrap().meta.coverage,
        Coverage::Gap {
            first_unchecked: second,
            acknowledged_through: second,
            reason: CoverageGapReason::NotCheckedAncestorDivergence
        }
    );
}

#[test]
fn retry_spans_polls_accepts_fifo_and_exhaustion_names_full_suffix() {
    let (_directory, store) = create();
    let coordinates = vec![coordinate(1, 0x10), coordinate(2, 0x10)];
    let failing = FakeNotification {
        plan: None,
        coordinates: Ok(coordinates.clone()),
        blocks: Err(FailureClass::TransientRetry),
    };
    let mut runtime = runtime(2);
    runtime.push(failing).unwrap();
    let now = Instant::now();
    assert!(matches!(
        runtime
            .poll(&store, identity(), &mut FakePipeline, now)
            .unwrap(),
        RuntimeAction::RetryAt(_)
    ));
    runtime
        .push(notification(blocks(
            &store.load(identity()).unwrap().state,
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
                &mut FakePipeline,
                now + Duration::from_millis(25)
            )
            .unwrap(),
        RuntimeAction::AcknowledgeAndTerminate(coordinate(3, 0x10))
    );
    assert_eq!(
        store.load(identity()).unwrap().meta.coverage,
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
    let all = blocks(&store.load(identity()).unwrap().state, 1, 3, 0x10);
    store
        .record_gap(
            identity(),
            all[0].zone,
            all[2].zone,
            CoverageGapReason::MissingReceipts,
        )
        .unwrap();
    let mut runtime = runtime(2);
    for block in &all {
        runtime.push(notification(vec![block.clone()])).unwrap();
    }
    for _ in 0..3 {
        assert!(matches!(
            runtime
                .poll(&store, identity(), &mut FakePipeline, Instant::now())
                .unwrap(),
            RuntimeAction::Acknowledge(_)
        ));
    }
    let snapshot = store.load(identity()).unwrap();
    assert_eq!(snapshot.meta.coverage, Coverage::Complete);
    assert_eq!(snapshot.meta.verified_zone_tip, all[2].zone);
    assert_eq!(snapshot.meta.acknowledged_zone_tip, all[2].zone);
}

#[test]
fn overflow_and_stream_failure_are_durable_fail_open_apis() {
    let (directory, store) = create();
    let state = store.load(identity()).unwrap().state;
    let mut overflow_runtime = CompactRuntime::new(1, RetryBudget::new(2, Duration::from_secs(1)));
    overflow_runtime
        .push(notification(blocks(&state, 1, 1, 0x10)))
        .unwrap();
    overflow_runtime
        .push(notification(blocks(&state, 2, 1, 0x10)))
        .unwrap();
    let rejected = notification(blocks(&state, 3, 2, 0x10));
    assert_eq!(
        overflow_runtime
            .push_or_record_overflow(&store, identity(), rejected)
            .unwrap(),
        RuntimeAction::AcknowledgeAndTerminate(coordinate(4, 0x10))
    );
    assert_eq!(
        store.load(identity()).unwrap().meta.coverage,
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
    let mut runtime = runtime(2);
    let suffix = [
        coordinate(1, 0x10),
        coordinate(2, 0x10),
        coordinate(3, 0x10),
    ];
    assert_eq!(
        runtime
            .record_stream_failure(&store, identity(), &suffix)
            .unwrap(),
        RuntimeAction::AcknowledgeAndTerminate(suffix[2])
    );
    assert_eq!(
        store.load(identity()).unwrap().meta.coverage,
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
    let state = store.load(identity()).unwrap().state;
    let mut bad = blocks(&state, 1, 1, 0x10);
    bad[0].outputs.state.tempo_block_hash = B256::ZERO;
    let mut runtime = runtime(2);
    runtime.push(notification(bad)).unwrap();
    runtime
        .poll(&store, identity(), &mut FakePipeline, Instant::now())
        .unwrap();
    runtime
        .push(notification(blocks(&state, 2, 2, 0x10)))
        .unwrap();
    assert_eq!(
        runtime
            .poll(&store, identity(), &mut FakePipeline, Instant::now())
            .unwrap(),
        RuntimeAction::Acknowledge(coordinate(3, 0x10))
    );
    assert_eq!(runtime.state(), RuntimeState::Alerting);
    assert!(
        matches!(store.load(identity()).unwrap().meta.coverage, Coverage::Gap { reason: CoverageGapReason::NotCheckedAncestorDivergence, acknowledged_through, .. } if acknowledged_through == coordinate(3, 0x10))
    );
    runtime.reorg(&store, identity(), anchor().zone).unwrap();
    assert_eq!(runtime.state(), RuntimeState::Starting);
    let mut replacement = blocks(&store.load(identity()).unwrap().state, 1, 1, 0x30);
    replacement[0].parent = anchor().zone;
    runtime.push(notification(replacement)).unwrap();
    runtime
        .poll(&store, identity(), &mut FakePipeline, Instant::now())
        .unwrap();
    assert_eq!(runtime.state(), RuntimeState::Healthy);
}

#[test]
fn restart_catchup_containing_active_finding_never_authenticates() {
    let (_directory, store) = create();
    let state = store.load(identity()).unwrap().state;
    let mut bad = blocks(&state, 1, 1, 0x10);
    bad[0].outputs.state.tempo_block_hash = B256::ZERO;
    let mut first_runtime = runtime(2);
    first_runtime.push(notification(bad)).unwrap();
    first_runtime
        .poll(&store, identity(), &mut FakePipeline, Instant::now())
        .unwrap();
    store
        .record_gap(
            identity(),
            coordinate(1, 0x10),
            coordinate(3, 0x10),
            CoverageGapReason::NotCheckedAncestorDivergence,
        )
        .unwrap();

    let mut restarted = runtime(2);
    restarted
        .push(FakeNotification {
            plan: None,
            coordinates: Ok(vec![
                coordinate(1, 0x10),
                coordinate(2, 0x10),
                coordinate(3, 0x10),
            ]),
            blocks: Err(FailureClass::ImmediateTerminal),
        })
        .unwrap();
    assert_eq!(
        restarted
            .poll(&store, identity(), &mut FakePipeline, Instant::now())
            .unwrap(),
        RuntimeAction::Acknowledge(coordinate(3, 0x10))
    );
    assert_eq!(restarted.state(), RuntimeState::Alerting);
}

#[test]
fn active_finding_rejects_same_height_wrong_ancestor_hash() {
    let (_directory, store) = create();
    let state = store.load(identity()).unwrap().state;
    let mut bad = blocks(&state, 1, 1, 0x10);
    bad[0].outputs.state.tempo_block_hash = B256::ZERO;
    let mut runtime = runtime(2);
    runtime.push(notification(bad)).unwrap();
    runtime
        .poll(&store, identity(), &mut FakePipeline, Instant::now())
        .unwrap();
    store
        .record_gap(
            identity(),
            coordinate(1, 0x10),
            coordinate(3, 0x10),
            CoverageGapReason::NotCheckedAncestorDivergence,
        )
        .unwrap();
    runtime
        .push(FakeNotification {
            plan: Some(NotificationPlan {
                reverted: vec![],
                ancestor: coordinate(3, 0x50),
                applied: vec![coordinate(4, 0x50)],
                acknowledge: coordinate(4, 0x50),
            }),
            coordinates: Ok(vec![coordinate(4, 0x50)]),
            blocks: Ok(blocks(&state, 4, 1, 0x50)),
        })
        .unwrap();
    assert_eq!(
        runtime
            .poll(&store, identity(), &mut FakePipeline, Instant::now())
            .unwrap(),
        RuntimeAction::Terminal
    );
    assert_eq!(
        store.load(identity()).unwrap().meta.acknowledged_zone_tip,
        coordinate(3, 0x10)
    );
}

#[test]
fn malformed_coordinates_are_terminal_without_ack() {
    let (_directory, store) = create();
    for coordinates in [
        Err(()),
        Ok(vec![]),
        Ok(vec![coordinate(1, 0x10), coordinate(3, 0x10)]),
    ] {
        let mut runtime = runtime(2);
        runtime
            .push(FakeNotification {
                plan: None,
                coordinates,
                blocks: Ok(vec![]),
            })
            .unwrap();
        assert_eq!(
            runtime
                .poll(&store, identity(), &mut FakePipeline, Instant::now())
                .unwrap(),
            RuntimeAction::Terminal
        );
        assert_eq!(
            store.load(identity()).unwrap().meta.acknowledged_zone_tip,
            anchor().zone
        );
    }
}

#[test]
fn builder_refuses_unrelated_nonempty_path_and_replay_reopens_identically() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("unrelated"), b"occupied").unwrap();
    let config = || BuildConfig {
        path: directory.path(),
        l1_chain_id: 1,
        zone_chain_id: 2,
        creation_block: identity().creation_block,
        creation_height: identity().creation_height,
        portal_identity: portal(),
        anchor: anchor(),
    };
    assert!(matches!(
        build_checkpoint(config(), &[] as &[FakeNotification], &mut FakePipeline),
        Err(PersistenceError::Invalid(_))
    ));

    let directory = tempfile::tempdir().unwrap();
    let initial = State::awaiting(portal());
    let history = [notification(blocks(&initial, 1, 2, 0x10))];
    let snapshot = build_checkpoint(
        BuildConfig {
            path: directory.path(),
            l1_chain_id: 1,
            zone_chain_id: 2,
            creation_block: identity().creation_block,
            creation_height: identity().creation_height,
            portal_identity: portal(),
            anchor: anchor(),
        },
        &history,
        &mut FakePipeline,
    )
    .unwrap();
    let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(snapshot, reopened);
}

#[test]
fn builder_unwinds_reorg_before_authenticating_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let initial = State::awaiting(portal());
    let old = blocks(&initial, 1, 1, 0x10);
    let mut replacement = blocks(&initial, 1, 1, 0x30);
    replacement[0].parent = anchor().zone;
    let history = [
        notification(old.clone()),
        FakeNotification {
            plan: Some(NotificationPlan {
                reverted: vec![old[0].zone],
                ancestor: anchor().zone,
                applied: vec![replacement[0].zone],
                acknowledge: replacement[0].zone,
            }),
            coordinates: Ok(vec![replacement[0].zone]),
            blocks: Ok(replacement.clone()),
        },
    ];
    let snapshot = build_checkpoint(
        BuildConfig {
            path: directory.path(),
            l1_chain_id: 1,
            zone_chain_id: 2,
            creation_block: identity().creation_block,
            creation_height: identity().creation_height,
            portal_identity: portal(),
            anchor: anchor(),
        },
        &history,
        &mut FakePipeline,
    )
    .unwrap();
    assert_eq!(snapshot.meta.verified_zone_tip, replacement[0].zone);
    assert_ne!(snapshot.meta.verified_zone_tip, old[0].zone);
}

#[test]
fn failed_builder_removes_target_and_sibling_staging() {
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("checker");
    let initial = State::awaiting(portal());
    let mut bad = blocks(&initial, 1, 1, 0x10);
    bad[0].parent = coordinate(99, 0xee);

    assert!(
        build_checkpoint(
            BuildConfig {
                path: &target,
                l1_chain_id: 1,
                zone_chain_id: 2,
                creation_block: identity().creation_block,
                creation_height: identity().creation_height,
                portal_identity: portal(),
                anchor: anchor(),
            },
            &[notification(bad)],
            &mut FakePipeline,
        )
        .is_err()
    );
    assert!(!target.exists());
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
}
