use alloy_eips::BlockNumHash;
use alloy_primitives::Address;
use tempfile::TempDir;

use crate::{
    model::{
        state::ModelState,
        transition::{LifecycleRecoveryScenario, RecoveryStep},
    },
    store::{
        db::{CheckerStore, Initialization},
        operations::WriteOutcome,
        value::{BootstrapState, StoreIdentity},
    },
};

pub(super) fn assert_scenario_recovery(scenario: &LifecycleRecoveryScenario) {
    let mut harness = RecoveryHarness::new(scenario);
    for step in &scenario.steps {
        harness.apply(step);
        if let Some(phase) = step.checkpoint {
            harness.assert_recovery(phase);
        }
    }
}

struct RecoveryHarness {
    directory: TempDir,
    initialization: Initialization,
    store: Option<CheckerStore>,
    state: ModelState,
    records: Vec<RecoveryStep>,
}

impl RecoveryHarness {
    fn new(scenario: &LifecycleRecoveryScenario) -> Self {
        let identity = StoreIdentity::new(
            7,
            scenario.initial_zone_tip.hash,
            scenario.portal_identity,
            4242,
            Address::repeat_byte(0xe5),
            scenario.initial_tempo_tip,
        );
        let initialization = Initialization::new(
            identity,
            BootstrapState::live(),
            scenario.initial_zone_tip,
            scenario.initial_tempo_tip,
            scenario.initial_state.clone(),
        );
        let directory = TempDir::new().unwrap();
        let store =
            CheckerStore::create_fresh_at(directory.path().join("checker"), initialization.clone())
                .unwrap();
        Self {
            directory,
            initialization,
            store: Some(store),
            state: scenario.initial_state.clone(),
            records: Vec::new(),
        }
    }

    fn apply(&mut self, record: &RecoveryStep) {
        let (parent_zone, parent_tempo) = self.records.last().map_or(
            (
                self.initialization.verified_zone_tip,
                self.initialization.imported_tempo_tip,
            ),
            |parent| (parent.zone_tip, parent.tempo_tip),
        );
        apply_record(
            self.store.as_ref().unwrap(),
            &mut self.state,
            parent_zone,
            parent_tempo,
            record,
        );
        assert_eq!(self.state, record.expected_state);
        self.records.push(record.clone());
    }

    fn assert_recovery(&mut self, phase: &str) {
        let expected = self.store.as_ref().unwrap().load_current().unwrap();

        drop(self.store.take().unwrap());
        let reopened = CheckerStore::open_existing_at(
            self.directory.path().join("checker"),
            self.initialization.identity,
        )
        .unwrap();
        assert_eq!(
            reopened.load_current().unwrap(),
            expected,
            "restart: {phase}"
        );
        self.store = Some(reopened);

        let rebuild_dir = TempDir::new().unwrap();
        let rebuilt = CheckerStore::create_fresh_at(
            rebuild_dir.path().join("checker"),
            self.initialization.clone(),
        )
        .unwrap();
        let mut rebuilt_state = self.initialization.model.clone();
        let mut zone_tip = self.initialization.verified_zone_tip;
        let mut tempo_tip = self.initialization.imported_tempo_tip;
        for record in &self.records {
            apply_record(&rebuilt, &mut rebuilt_state, zone_tip, tempo_tip, record);
            zone_tip = record.zone_tip;
            tempo_tip = record.tempo_tip;
        }
        assert_eq!(
            rebuilt.load_current().unwrap(),
            expected,
            "rebuild: {phase}"
        );

        let record = self.records.last().unwrap();
        let parent = self.records.iter().rev().nth(1);
        let parent_state =
            parent.map_or(&self.initialization.model, |parent| &parent.expected_state);
        let parent_tips = self
            .store
            .as_ref()
            .unwrap()
            .unwind_tip(record.zone_tip)
            .unwrap();
        let expected_parent_zone = parent.map_or(self.initialization.verified_zone_tip, |parent| {
            parent.zone_tip
        });
        let expected_parent_tempo = parent
            .map_or(self.initialization.imported_tempo_tip, |parent| {
                parent.tempo_tip
            });
        assert_eq!(parent_tips.zone, expected_parent_zone);
        assert_eq!(parent_tips.tempo, expected_parent_tempo);
        assert_eq!(
            &self.store.as_ref().unwrap().load_current().unwrap().model,
            parent_state,
            "unwind: {phase}"
        );

        let mut reapplied = parent_state.clone();
        apply_record(
            self.store.as_ref().unwrap(),
            &mut reapplied,
            parent_tips.zone,
            parent_tips.tempo,
            record,
        );
        assert_eq!(reapplied, record.expected_state, "reapply model: {phase}");
        assert_eq!(
            self.store.as_ref().unwrap().load_current().unwrap(),
            expected,
            "reapply: {phase}"
        );
        self.store.as_ref().unwrap().check_consistency().unwrap();
    }
}

fn apply_record(
    store: &CheckerStore,
    state: &mut ModelState,
    parent_zone: BlockNumHash,
    parent_tempo: BlockNumHash,
    record: &RecoveryStep,
) {
    let update = record.state_update(state);
    let commit = store
        .block_commit(
            parent_zone,
            parent_tempo,
            record.zone_tip,
            record.tempo_tip,
            &update,
        )
        .unwrap();
    assert_eq!(store.apply_block(commit).unwrap(), WriteOutcome::Applied);
    update.apply_to_current_parent(state);
    assert_eq!(&store.load_current().unwrap().model, state);
}
