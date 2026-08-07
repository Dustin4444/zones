mod persistence;

use crate::model::transition::lifecycle_recovery_scenario;

#[test]
fn every_open_owner_phase_survives_restart_rebuild_and_reorg() {
    persistence::assert_scenario_recovery(&lifecycle_recovery_scenario());
}
