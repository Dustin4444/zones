use revm_database::{State, StateBuilder, states::bundle_state::BundleRetention};

use crate::{ExecutionPostState, WitnessDatabase};

/// Mutable revm execution state layered over the strict witness database.
///
/// The underlying [`WitnessDatabase`] still fails closed on missing accounts,
/// storage, bytecode, or ancestor hashes. `State` adds revm's normal execution
/// cache and transition bundle so real block execution can commit writes and
/// later derive post-state commitments through reth's hashed post-state format.
pub type ZoneExecutionState = State<WitnessDatabase>;

pub fn zone_execution_state(db: WitnessDatabase) -> ZoneExecutionState {
    StateBuilder::new_with_database(db)
        .with_bundle_update()
        .build()
}

pub fn execution_post_state_from_state(mut state: ZoneExecutionState) -> ExecutionPostState {
    state.merge_transitions(BundleRetention::PlainState);
    let bundle = state.take_bundle();
    ExecutionPostState::from_bundle_state(&bundle)
}
