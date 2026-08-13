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

mod policy;
mod queue;

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
    match class {
        FailureClass::ImmediateTerminal => Failure::terminal("injected failure"),
        FailureClass::TransientRetry => Failure::transient("injected failure"),
        FailureClass::BoundedRetry | FailureClass::AuthenticatedDivergence => {
            unreachable!("unsupported injected failure class")
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

mod processing;
mod publication;
mod reorgs;
