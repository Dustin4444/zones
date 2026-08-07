//! Real local L1 and Zone integration coverage for the checker ExEx.

use std::time::Duration;

use crate::utils::{L1TestNode, ZoneTestLaunchConfig, ZoneTestNode};
use alloy::{eips::BlockNumHash, providers::Provider, rpc::types::BlockNumberOrTag};
use tempo_precompiles::PATH_USD_ADDRESS;
use tempo_zone_contracts::{TEMPO_STATE_ADDRESS, TempoState};
use zone_checker::{
    CheckerConfig,
    test_utils::{
        CheckerBootstrapPhase, CheckerSnapshot, CheckerTokenPhase, checker_with_progress,
        inspect_database as inspect_checker_database,
    },
};
use zone_node::dev::{ProvisionConfig, provision_zone};

/// Real L1 tests allow for node launch and archive catch-up.
const L1_TIMEOUT: Duration = Duration::from_secs(30);

async fn wait_for_zone_to_finalize_l1(zone: &ZoneTestNode, target: u64) -> eyre::Result<()> {
    let after = target
        .checked_sub(1)
        .ok_or_else(|| eyre::eyre!("L1 target must be past genesis"))?;
    zone.wait_for_l2_tempo_finalized(after, L1_TIMEOUT).await?;
    Ok(())
}

fn assert_live_snapshot(
    snapshot: &CheckerSnapshot,
    zone_tip: BlockNumHash,
    tempo_tip: BlockNumHash,
) {
    assert_eq!(snapshot.verified_zone_tip, zone_tip);
    assert_eq!(snapshot.imported_tempo_tip, tempo_tip);
    assert_eq!(snapshot.bootstrap, CheckerBootstrapPhase::Live);
    assert!(snapshot.portal_created, "Portal creation was not modeled");
    assert!(!snapshot.active_alert, "valid dev history raised an alert");
    assert_eq!(
        snapshot.tokens.get(&PATH_USD_ADDRESS),
        Some(&CheckerTokenPhase::ZoneEnabled),
        "the provisioned initial token did not reach the Zone-enabled model phase"
    );
}

/// A fresh checker database must authenticate the provisioner's exact Portal
/// creation block, catch up through the local Zone archive, and converge on the
/// same canonical head without gating Zone block production. Reinstalling the
/// ExEx with that populated database must then acknowledge and persist a new tip.
#[tokio::test(flavor = "multi_thread")]
async fn test_checker_bootstraps_and_restarts_from_durable_database() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    // Instant mining produces a finite authenticated archive during provisioning.
    // The Zone then catches up to a stable L1 head instead of racing a periodic miner.
    let l1 = L1TestNode::start_with(|config| config.dev.block_time = None).await?;
    let provisioned = provision_zone(ProvisionConfig {
        l1_rpc_url: l1.ws_url().to_string(),
        dev_key: l1.dev_signer(),
        factory: None,
        initial_token: PATH_USD_ADDRESS,
        is_access_open: true,
        is_gateway_enforced: false,
        zone_gateways: Vec::new(),
        allowed_accounts: Vec::new(),
        rpc_url: String::new(),
    })
    .await?;
    let creation_block = l1
        .provider()
        .get_block_by_hash(provisioned.portal_creation_block_hash)
        .await?
        .ok_or_else(|| eyre::eyre!("provisioned Portal creation block is missing"))?;
    assert!(
        creation_block.header.inner.number > provisioned.anchor_block_number,
        "the dev fixture must exercise Portal creation after the Zone genesis anchor"
    );

    let checker_directory = tempfile::tempdir()?;
    let checker_path = checker_directory.path().join("checker");
    assert!(!checker_path.exists(), "checker database must start fresh");
    let l1_target = l1.provider().get_block_number().await?;
    let checker_config = CheckerConfig {
        l1_rpc_url: l1.http_url().to_string(),
        portal_address: provisioned.portal,
        portal_creation_block_hash: provisioned.portal_creation_block_hash,
        zone_id: provisioned.zone_id,
        database_path: Some(checker_path.clone()),
    };
    let (checker, mut checker_progress) = checker_with_progress(checker_config.clone());
    let launch = ZoneTestLaunchConfig::new(
        l1.ws_url().to_string(),
        provisioned.portal,
        provisioned.chain_id,
    )
    .with_genesis(provisioned.genesis.clone())
    .with_sequencer_signer(l1.dev_signer())
    .with_checker(checker);
    let zone = ZoneTestNode::launch(launch).await?;

    wait_for_zone_to_finalize_l1(&zone, l1_target).await?;
    let canonical_number = zone.stop_engine().await?;
    assert!(
        canonical_number > 0,
        "the Zone must advance while the checker ExEx is enabled"
    );
    let canonical_block = zone
        .provider()
        .get_block_by_number(BlockNumberOrTag::Number(canonical_number))
        .await?
        .ok_or_else(|| eyre::eyre!("canonical Zone block {canonical_number} is missing"))?;
    let expected_tip = BlockNumHash::new(canonical_number, canonical_block.header.hash);
    let tempo_state = TempoState::new(TEMPO_STATE_ADDRESS, zone.provider());
    let expected_tempo_tip = BlockNumHash::new(
        tempo_state.tempoBlockNumber().call().await?,
        tempo_state.tempoBlockHash().call().await?,
    );

    checker_progress.wait_for(expected_tip, L1_TIMEOUT).await?;
    zone.crash();
    drop(zone);
    let snapshot = inspect_checker_database(&checker_path)?;
    assert_live_snapshot(&snapshot, expected_tip, expected_tempo_tip);

    // ZoneTestNode owns an ephemeral node database, so reconstruct the same
    // canonical archive from the same genesis while reusing the checker path.
    // Fresh checker creation rejects nonempty paths, making launch exercise reopen.
    let (restarted_checker, mut restarted_progress) = checker_with_progress(checker_config);
    let restarted_launch = ZoneTestLaunchConfig::new(
        l1.ws_url().to_string(),
        provisioned.portal,
        provisioned.chain_id,
    )
    .with_genesis(provisioned.genesis.clone())
    .with_sequencer_signer(l1.dev_signer())
    .with_checker(restarted_checker);
    let restarted_zone = ZoneTestNode::launch(restarted_launch).await?;
    wait_for_zone_to_finalize_l1(&restarted_zone, l1_target).await?;
    let recovered_block = restarted_zone
        .provider()
        .get_block_by_number(BlockNumberOrTag::Number(expected_tip.number))
        .await?
        .ok_or_else(|| eyre::eyre!("restarted Zone did not reconstruct its prior tip"))?;
    assert_eq!(
        recovered_block.header.hash, expected_tip.hash,
        "restarted Zone must reconstruct the checker database's canonical tip"
    );

    l1.fund_user(l1.user_signer().address(), 1).await?;
    let restarted_l1_target = l1.provider().get_block_number().await?;
    assert!(
        restarted_l1_target > l1_target,
        "restart progress fixture must advance the Tempo head"
    );
    wait_for_zone_to_finalize_l1(&restarted_zone, restarted_l1_target).await?;

    let restarted_canonical_number = restarted_zone.stop_engine().await?;
    let restarted_canonical_block = restarted_zone
        .provider()
        .get_block_by_number(BlockNumberOrTag::Number(restarted_canonical_number))
        .await?
        .ok_or_else(|| {
            eyre::eyre!("canonical Zone block {restarted_canonical_number} is missing")
        })?;
    let restarted_tip = BlockNumHash::new(
        restarted_canonical_number,
        restarted_canonical_block.header.hash,
    );
    assert!(
        restarted_tip.number > expected_tip.number,
        "restarted checker must advance past its previous durable Zone tip"
    );
    let restarted_tempo_state = TempoState::new(TEMPO_STATE_ADDRESS, restarted_zone.provider());
    let restarted_tempo_tip = BlockNumHash::new(
        restarted_tempo_state.tempoBlockNumber().call().await?,
        restarted_tempo_state.tempoBlockHash().call().await?,
    );
    restarted_progress
        .wait_for(restarted_tip, L1_TIMEOUT)
        .await?;
    restarted_zone.crash();
    drop(restarted_zone);
    let restarted_snapshot = inspect_checker_database(&checker_path)?;
    assert_live_snapshot(&restarted_snapshot, restarted_tip, restarted_tempo_tip);

    Ok(())
}
