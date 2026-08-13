use std::{path::Path, time::Duration};

use crate::utils::{L1TestNode, ZoneTestLaunchConfig, ZoneTestNode, poll_until};
use alloy::providers::Provider;
use tempo_precompiles::PATH_USD_ADDRESS;
use zone_checker::{CheckerConfig, CheckerExEx, inspection::inspect_database};
use zone_node::dev::{ProvisionConfig, provision_zone};

const TIMEOUT: Duration = Duration::from_secs(30);

async fn wait_for_zone_to_finalize_l1(zone: &ZoneTestNode, target: u64) -> eyre::Result<()> {
    zone.wait_for_l2_tempo_finalized(target.saturating_sub(1), TIMEOUT)
        .await?;
    Ok(())
}

async fn wait_for_checker_to_catch_up(
    path: &Path,
    previous_verified_height: u64,
) -> eyre::Result<zone_checker::inspection::CheckerSnapshot> {
    let path = path.to_path_buf();
    poll_until(
        TIMEOUT,
        Duration::from_millis(100),
        "checker to verify all observed Zone blocks",
        move || {
            let path = path.clone();
            async move {
                let snapshot = match inspect_database(&path) {
                    Ok(snapshot) => snapshot,
                    // The checker owns the MDBX writer. A concurrent inspection can race its
                    // short write transaction, so retry the read on the next poll.
                    Err(error)
                        if error.chain().any(|cause| {
                            cause
                                .to_string()
                                .contains("Resource temporarily unavailable")
                        }) =>
                    {
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                };
                Ok(
                    (snapshot.verified_zone_tip.number > previous_verified_height
                        && snapshot.observed_zone_tip == snapshot.verified_zone_tip
                        && !snapshot.active_finding
                        && !snapshot.has_coverage_gap)
                        .then_some(snapshot),
                )
            }
        },
    )
    .await
}

async fn wait_for_checker_checkpoint(path: &Path) -> eyre::Result<()> {
    let path = path.to_path_buf();
    poll_until(
        TIMEOUT,
        Duration::from_millis(100),
        "checker to create its checkpoint",
        move || {
            let path = path.clone();
            async move { Ok(path.is_dir().then_some(())) }
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "checker progress is not externally observable while MDBX is writer-owned"]
async fn checker_restarts_and_advances_from_checkpoint() -> eyre::Result<()> {
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
        zone_id: provisioned.zone_id,
        database_path: path.clone(),
        acquisition_timeout: Duration::from_secs(30),
    };

    let builder = ZoneTestNode::launch(
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
    wait_for_checker_checkpoint(&path).await?;
    builder.crash();
    drop(builder);
    tokio::time::sleep(Duration::from_millis(250)).await;
    let initial = inspect_database(&path)?;

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
    zone.stop_engine().await?;
    let first = wait_for_checker_to_catch_up(&path, initial.verified_zone_tip.number).await?;
    zone.crash();
    drop(zone);
    assert_eq!(first.observed_zone_tip, first.verified_zone_tip);
    assert!(first.verified_zone_tip.number > initial.verified_zone_tip.number);
    assert!(!first.active_finding);
    assert!(!first.has_coverage_gap);

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
    restarted.stop_engine().await?;
    let snapshot = wait_for_checker_to_catch_up(&path, first.verified_zone_tip.number).await?;
    restarted.crash();
    drop(restarted);
    assert_eq!(snapshot.observed_zone_tip, snapshot.verified_zone_tip);
    assert!(snapshot.verified_zone_tip.number > first.verified_zone_tip.number);
    assert!(!snapshot.active_finding);
    assert!(!snapshot.has_coverage_gap);
    Ok(())
}
