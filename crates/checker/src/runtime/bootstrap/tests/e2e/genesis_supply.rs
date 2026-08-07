use std::path::Path;

use alloy_primitives::{Address, U256};
use tempfile::TempDir;

use super::super::super::{create_fresh, open_existing, resume_l1_replay};
use super::support::*;
use crate::{
    model::state::TokenPhase,
    runtime::{PersistentChecker, RuntimeError, bootstrap::error::BootstrapError},
    store::{db::CheckerStore, value::BootstrapState},
};

#[tokio::test]
async fn nonzero_genesis_supply_never_becomes_the_bootstrap_baseline() {
    let fixture = PreGenesisFixture::new();
    let actual_supply = U256::from(1);
    let zone_provider = zone_provider_with_genesis_supply(
        fixture.zone_genesis,
        fixture.anchor.tip(),
        fixture.initial_token,
        actual_supply,
    );

    let fresh_directory = TempDir::new().unwrap();
    let fresh_path = fresh_directory.path().join("fresh-nonzero-supply");
    let fresh_script = RpcScript::new();
    fresh_script.push_full_fresh_replay(&fixture);
    assert_nonzero_genesis_supply(
        create_fresh(
            &fixture.config(None),
            fixture.zone_chain_id,
            &fresh_path,
            &zone_provider,
            &fresh_script.provider(),
        )
        .await,
        fixture.initial_token,
        actual_supply,
    );
    fresh_script.assert_consumed();
    assert_l1_replay_without_promotion(&fresh_path, &fixture);

    let resumed_directory = TempDir::new().unwrap();
    let resumed_path = resumed_directory.path().join("resumed-nonzero-supply");
    persist_l1_prefix(&fixture, &resumed_path, L1ReplayCheckpoint::Anchor).await;
    let resume_script = RpcScript::new();
    let mut resumed = open_existing(
        &fixture.config(None),
        fixture.zone_chain_id,
        &resumed_path,
        fixture.identity(),
        &zone_provider,
    )
    .unwrap();
    let resumed = resume_l1_replay(
        &fixture.config(None),
        &zone_provider,
        &resume_script.provider(),
        &mut resumed,
    )
    .await
    .map(|()| resumed);
    assert_nonzero_genesis_supply(resumed, fixture.initial_token, actual_supply);
    resume_script.assert_consumed();
    assert_l1_replay_without_promotion(&resumed_path, &fixture);
}

#[tokio::test]
async fn creation_after_anchor_also_rejects_nonzero_genesis_supply() {
    let fixture = DevelopmentFixture::new();
    let actual_supply = U256::from(1);
    let zone_provider = zone_provider_with_genesis_supply(
        fixture.zone_genesis,
        fixture.anchor.tip(),
        INITIAL_TOKEN,
        actual_supply,
    );
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("development-nonzero-supply");
    let script = RpcScript::new();
    script.push_development_fresh(&fixture);

    assert_nonzero_genesis_supply(
        create_fresh(
            &fixture.config(),
            fixture.zone_chain_id,
            &path,
            &zone_provider,
            &script.provider(),
        )
        .await,
        INITIAL_TOKEN,
        actual_supply,
    );

    script.assert_consumed();
    assert!(
        !path.exists(),
        "invalid genesis supply must precede DB creation"
    );
}

fn assert_nonzero_genesis_supply(
    result: Result<PersistentChecker, RuntimeError>,
    expected_token: Address,
    expected_supply: U256,
) {
    assert!(matches!(
        result,
        Err(RuntimeError::Bootstrap(error))
            if matches!(
                error.as_ref(),
                BootstrapError::NonzeroZoneGenesisSupply { token, actual }
                    if *token == expected_token && *actual == expected_supply
            )
    ));
}

fn assert_l1_replay_without_promotion(path: &Path, fixture: &PreGenesisFixture) {
    let snapshot = CheckerStore::inspect_existing_at(path, fixture.identity()).unwrap();
    assert_eq!(
        snapshot.bootstrap,
        BootstrapState::l1_replay(Some(fixture.anchor.tip()))
    );
    assert_eq!(
        snapshot.model.token(fixture.initial_token).unwrap().phase(),
        TokenPhase::PendingZoneEnable
    );
}
