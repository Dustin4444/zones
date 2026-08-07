//! Manual real-node baseline for checker latency and durable growth metrics.

use std::time::{Duration, Instant};

use crate::utils::{L1TestNode, ZoneTestLaunchConfig, ZoneTestNode};
use alloy::{eips::BlockNumHash, providers::Provider, rpc::types::BlockNumberOrTag};
use metrics_exporter_prometheus::PrometheusBuilder;
use serde_json::json;
use tempo_precompiles::PATH_USD_ADDRESS;
use zone_checker::{CheckerConfig, test_utils::checker_with_paused_progress};
use zone_node::dev::{ProvisionConfig, provision_zone};

const BACKLOG_BLOCKS: u64 = 40;
const LIVE_BLOCKS: u64 = 40;
const PERF_TIMEOUT: Duration = Duration::from_secs(120);
const METRIC_PREFIX: &str = "reth_tempo_zone_checker";
const PERF_WITHDRAWAL_BATCH_INTERVAL_BLOCKS: u64 = BACKLOG_BLOCKS + LIVE_BLOCKS + 100;

fn metric_value(rendered: &str, sample: &str) -> eyre::Result<f64> {
    rendered
        .lines()
        .find_map(|line| {
            let (candidate, value) = line.split_once(' ')?;
            (candidate == sample).then(|| value.parse::<f64>())
        })
        .ok_or_else(|| eyre::eyre!("required Prometheus sample `{sample}` is missing"))?
        .map_err(Into::into)
}

fn quantile_value(rendered: &str, family: &str, quantile: &str) -> eyre::Result<f64> {
    metric_value(
        rendered,
        &format!("{METRIC_PREFIX}_{family}{{quantile=\"{quantile}\"}}"),
    )
}

fn require_positive(rendered: &str, sample: &str) -> eyre::Result<f64> {
    let value = metric_value(rendered, sample)?;
    eyre::ensure!(value > 0.0, "required sample `{sample}` was not positive");
    Ok(value)
}

fn require_positive_quantiles(rendered: &str, family: &str) -> eyre::Result<(f64, f64)> {
    let p50 = quantile_value(rendered, family, "0.5")?;
    let p95 = quantile_value(rendered, family, "0.95")?;
    eyre::ensure!(
        p50 > 0.0 && p95 > 0.0,
        "required `{family}` p50/p95 samples were not positive"
    );
    Ok((p50, p95))
}

async fn wait_for_zone_to_finalize_l1(zone: &ZoneTestNode, target: u64) -> eyre::Result<()> {
    let prior = target
        .checked_sub(1)
        .ok_or_else(|| eyre::eyre!("L1 target must be past genesis"))?;
    zone.wait_for_l2_tempo_finalized(prior, PERF_TIMEOUT)
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

/// Run explicitly with the release-profile command documented in
/// `crates/checker/PERFORMANCE_BASELINE.md`. It configures the recorder used by
/// the in-process reth nodes, so keeping it ignored prevents interference with
/// parallel tests that may initialize the process-global recorder first.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "manual checker performance baseline"]
async fn test_checker_real_node_performance_baseline() -> eyre::Result<()> {
    eyre::ensure!(
        !cfg!(debug_assertions),
        "checker performance evidence requires a release-profile test binary"
    );
    reth_tracing::init_test_tracing();
    let metrics = reth_node_metrics::recorder::try_install_prometheus_recorder_with_builder(
        PrometheusBuilder::new().set_quantiles(&[0.5, 0.95])?,
    )?;

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

    for _ in 0..BACKLOG_BLOCKS {
        l1.fund_user(l1.user_signer().address(), 1).await?;
    }
    let backlog_target = l1.provider().get_block_number().await?;

    let checker_directory = tempfile::tempdir()?;
    let checker_path = checker_directory.path().join("checker");
    let checker_config = CheckerConfig {
        l1_rpc_url: l1.http_url().to_string(),
        portal_address: provisioned.portal,
        portal_creation_block_hash: provisioned.portal_creation_block_hash,
        zone_id: provisioned.zone_id,
        database_path: Some(checker_path),
    };
    let (checker, mut progress, start_checker) = checker_with_paused_progress(checker_config);
    let launch = ZoneTestLaunchConfig::new(
        l1.ws_url().to_string(),
        provisioned.portal,
        provisioned.chain_id,
    )
    .with_genesis(provisioned.genesis)
    .with_sequencer_signer(l1.dev_signer())
    .with_withdrawal_batch_interval(PERF_WITHDRAWAL_BATCH_INTERVAL_BLOCKS)
    .with_checker(checker);

    let zone = ZoneTestNode::launch(launch).await?;
    wait_for_zone_to_finalize_l1(&zone, backlog_target).await?;
    let caught_up_tip = canonical_tip(&zone).await?;
    let catch_up_started = Instant::now();
    start_checker
        .send(())
        .map_err(|()| eyre::eyre!("checker start gate closed before release"))?;
    progress.wait_for(caught_up_tip, PERF_TIMEOUT).await?;
    let catch_up_elapsed = catch_up_started.elapsed();

    let mut prior_tip = caught_up_tip;
    for _ in 0..LIVE_BLOCKS {
        l1.fund_user(l1.user_signer().address(), 1).await?;
        let l1_target = l1.provider().get_block_number().await?;
        wait_for_zone_to_finalize_l1(&zone, l1_target).await?;
        let tip = canonical_tip(&zone).await?;
        eyre::ensure!(tip.number > prior_tip.number, "live sample did not advance");
        progress.wait_for(tip, PERF_TIMEOUT).await?;
        prior_tip = tip;
    }

    let final_number = zone.stop_engine().await?;
    eyre::ensure!(
        final_number == prior_tip.number,
        "unacknowledged final block"
    );
    metrics.handle().run_upkeep();
    let rendered = metrics.handle().render();
    let catch_up_count =
        metric_value(&rendered, &format!("{METRIC_PREFIX}_catch_up_blocks_total"))?;
    let live_count = metric_value(&rendered, &format!("{METRIC_PREFIX}_live_blocks_total"))?;
    eyre::ensure!(
        catch_up_count == caught_up_tip.number as f64,
        "catch-up counter {catch_up_count} did not cover retained tip {}",
        caught_up_tip.number
    );
    eyre::ensure!(
        live_count == LIVE_BLOCKS as f64,
        "live counter {live_count} did not match requested {LIVE_BLOCKS}"
    );

    let (live_p50, live_p95) =
        require_positive_quantiles(&rendered, "live_block_duration_seconds")?;
    let (receipt_p50, receipt_p95) =
        require_positive_quantiles(&rendered, "receipt_fetch_duration_seconds")?;
    let (collateral_p50, collateral_p95) =
        require_positive_quantiles(&rendered, "collateral_call_duration_seconds")?;
    let (mdbx_p50, mdbx_p95) =
        require_positive_quantiles(&rendered, "database_transaction_duration_seconds")?;
    require_positive_quantiles(&rendered, "exact_state_read_duration_seconds")?;

    let applied_blocks = catch_up_count + live_count;
    for family in [
        "database_transaction_duration_seconds_count",
        "changeset_bytes_count",
    ] {
        let count = metric_value(&rendered, &format!("{METRIC_PREFIX}_{family}"))?;
        eyre::ensure!(
            count == applied_blocks,
            "`{family}` count {count} did not match {applied_blocks} applied blocks"
        );
    }
    for family in [
        "receipt_fetch_duration_seconds_count",
        "collateral_call_duration_seconds_count",
        "exact_state_read_duration_seconds_count",
    ] {
        let count = metric_value(&rendered, &format!("{METRIC_PREFIX}_{family}"))?;
        eyre::ensure!(
            count >= applied_blocks,
            "`{family}` count {count} did not cover {applied_blocks} applied blocks"
        );
    }
    let changeset_bytes =
        require_positive(&rendered, &format!("{METRIC_PREFIX}_changeset_bytes_sum"))?;
    let model_rows = require_positive(&rendered, &format!("{METRIC_PREFIX}_model_rows"))?;
    let open_records = metric_value(
        &rendered,
        &format!("{METRIC_PREFIX}_open_lifecycle_records"),
    )?;
    eyre::ensure!(
        open_records == 0.0,
        "isolated baseline unexpectedly retained {open_records} open lifecycle records"
    );
    eyre::ensure!(
        model_rows == 9.0,
        "isolated one-token baseline expected 9 physical model rows, got {model_rows}"
    );
    let database_allocated_bytes = require_positive(
        &rendered,
        &format!("{METRIC_PREFIX}_database_allocated_bytes"),
    )?;

    let checker_metrics = rendered
        .lines()
        .filter(|line| line.contains("tempo_zone_checker"))
        .collect::<Vec<_>>()
        .join("\n");
    let catch_up_blocks = catch_up_count as u64;
    println!(
        "CHECKER_BASELINE_META {}",
        serde_json::to_string(&json!({
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "release_profile": !cfg!(debug_assertions),
            "backlog_l1_blocks_requested": BACKLOG_BLOCKS,
            "catch_up_zone_blocks": catch_up_blocks,
            "catch_up_elapsed_seconds": catch_up_elapsed.as_secs_f64(),
            "catch_up_blocks_per_second": catch_up_blocks as f64 / catch_up_elapsed.as_secs_f64(),
            "live_blocks_requested": LIVE_BLOCKS,
            "live_block_p50_seconds": live_p50,
            "live_block_p95_seconds": live_p95,
            "receipt_fetch_p50_seconds": receipt_p50,
            "receipt_fetch_p95_seconds": receipt_p95,
            "per_token_collateral_read_p50_seconds": collateral_p50,
            "per_token_collateral_read_p95_seconds": collateral_p95,
            "mdbx_transaction_p50_seconds": mdbx_p50,
            "mdbx_transaction_p95_seconds": mdbx_p95,
            "changeset_bytes_per_block_mean": changeset_bytes / applied_blocks,
            "model_rows": model_rows,
            "open_lifecycle_records": open_records,
            "database_allocated_bytes": database_allocated_bytes,
        }))?
    );
    println!(
        "CHECKER_BASELINE_PROMETHEUS_BEGIN\n{checker_metrics}\nCHECKER_BASELINE_PROMETHEUS_END"
    );

    zone.crash();
    drop(zone);
    Ok(())
}
