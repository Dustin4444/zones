//! Real local L1 and Zone integration coverage for checkpoint building and restart.

use std::time::Duration;

use crate::utils::{L1TestNode, ZoneTestLaunchConfig, ZoneTestNode};
use alloy::{eips::BlockNumHash, providers::Provider, rpc::types::BlockNumberOrTag};
use tempo_precompiles::PATH_USD_ADDRESS;
use zone_checker::{CheckerConfig, CheckerExEx, test_utils::inspect_database};
use zone_node::dev::{ProvisionConfig, provision_zone};

const TIMEOUT: Duration = Duration::from_secs(30);

async fn wait_for_zone_to_finalize_l1(zone: &ZoneTestNode, target: u64) -> eyre::Result<()> {
    zone.wait_for_l2_tempo_finalized(target.saturating_sub(1), TIMEOUT)
        .await?;
    Ok(())
}

async fn canonical_tip(zone: &ZoneTestNode) -> eyre::Result<BlockNumHash> {
    let number = zone.provider().get_block_number().await?;
    let block = zone
        .provider()
        .get_block_by_number(BlockNumberOrTag::Number(number))
        .await?
        .ok_or_else(|| eyre::eyre!("canonical Zone block {number} is missing"))?;
    Ok(BlockNumHash::new(number, block.header.hash))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_checker_builds_checkpoint_and_restarts_from_durable_journal() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
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

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("checker");
    let config = CheckerConfig {
        l1_rpc_url: l1.http_url().to_string(),
        portal_address: provisioned.portal,
        portal_creation_block_hash: provisioned.portal_creation_block_hash,
        zone_id: provisioned.zone_id,
        database_path: Some(path.clone()),
        acquisition_timeout: Duration::from_secs(30),
    };

    // Build the identity-bound bootstrap checkpoint from a local Zone provider.
    let builder = ZoneTestNode::launch(
        ZoneTestLaunchConfig::new(
            l1.ws_url().to_string(),
            provisioned.portal,
            provisioned.chain_id,
        )
        .with_genesis(provisioned.genesis.clone())
        .with_sequencer_signer(l1.dev_signer())
        .with_checker_checkpoint(config.clone()),
    )
    .await?;
    builder.crash();
    drop(builder);
    tokio::time::sleep(Duration::from_millis(250)).await;

    // The production ExEx consumes the checkpoint and durably catches up.
    let l1_target = l1.provider().get_block_number().await?;
    let zone = ZoneTestNode::launch(
        ZoneTestLaunchConfig::new(
            l1.ws_url().to_string(),
            provisioned.portal,
            provisioned.chain_id,
        )
        .with_genesis(provisioned.genesis.clone())
        .with_sequencer_signer(l1.dev_signer())
        .with_checker(CheckerExEx::new(config.clone())),
    )
    .await?;
    wait_for_zone_to_finalize_l1(&zone, l1_target).await?;
    let first_tip = canonical_tip(&zone).await?;
    zone.stop_engine().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    zone.crash();
    drop(zone);
    let first = inspect_database(&path)?;
    assert_eq!(first.acknowledged_zone_tip, first_tip);
    assert_eq!(first.verified_zone_tip, first_tip);
    assert!(!first.active_alert);
    assert!(!first.has_coverage_gap);

    // Reopen the same checker database, reconstruct the same Zone history,
    // then advance both chains and prove the durable journal moves forward.
    let restarted = ZoneTestNode::launch(
        ZoneTestLaunchConfig::new(
            l1.ws_url().to_string(),
            provisioned.portal,
            provisioned.chain_id,
        )
        .with_genesis(provisioned.genesis)
        .with_sequencer_signer(l1.dev_signer())
        .with_checker(CheckerExEx::new(config)),
    )
    .await?;
    wait_for_zone_to_finalize_l1(&restarted, l1_target).await?;
    l1.fund_user(l1.user_signer().address(), 1).await?;
    let next_l1_target = l1.provider().get_block_number().await?;
    wait_for_zone_to_finalize_l1(&restarted, next_l1_target).await?;
    let restarted_tip = canonical_tip(&restarted).await?;
    assert!(restarted_tip.number > first_tip.number);
    restarted.stop_engine().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    restarted.crash();
    drop(restarted);
    let snapshot = inspect_database(&path)?;
    assert_eq!(snapshot.acknowledged_zone_tip, restarted_tip);
    assert_eq!(snapshot.verified_zone_tip, restarted_tip);
    assert!(!snapshot.active_alert);
    assert!(!snapshot.has_coverage_gap);
    Ok(())
}
