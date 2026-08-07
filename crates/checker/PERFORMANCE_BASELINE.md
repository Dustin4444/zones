# Checker performance baseline

This is the measured Goal 10 release-profile baseline for the observe-only
checker. It is evidence for this checkout and host, not an SLO or a claim about
other hardware, RPCs, datasets, or production load.

## Run identity

- Date: 2026-08-07 (`America/New_York`).
- Source: base commit `a49bf42dd3231cdc7481239332ee6a02d9183ed2` plus the
  uncommitted Goal 10 candidate changes documented by this file.
- Host: MacBook Pro `Mac16,6`, Apple M4 Max, 36 GB memory, `aarch64`.
- OS: macOS 26.6 (build `25G72`), Darwin 25.6.0.
- Toolchain: `rustc 1.95.0 (59807616e 2026-04-14)`, LLVM 22.1.2,
  `aarch64-apple-darwin`.
- Profile: Cargo `release`; the test fails when `debug_assertions` is enabled.

Exact successful command:

```sh
cargo +1.95.0 test --release -p zone-node --features cli,test-utils --test it \
  test_checker_real_node_performance_baseline -- --ignored --nocapture --test-threads=1
```

Result: `1 passed; 0 failed`, 145 filtered out, test body completed in
84.03 seconds.

## Method

The ignored integration test uses an in-process native Tempo L1 and a real Zone
node. It provisions a fresh Portal/Zone, creates 40 additional L1 blocks, and
lets the checker initialize its durable genesis cut before pausing its
operational notification loop. The Zone reaches canonical height 42 while the
checker is paused. Releasing the checker produces 42 archive catch-up samples.

The test then creates 40 L1 blocks one at a time, waiting for both the Zone and
the checker acknowledgement after each, producing exactly 40 live samples.
Withdrawal batch finalization is set beyond the 82-block run so lifecycle
growth does not add noise: the expected final cut is nine physical model rows
and zero open lifecycle records.

The Prometheus recorder is configured with p50 and p95 summary quantiles. The
test fails unless the release profile, phase counts, required quantiles,
receipt/collateral/exact-state coverage, MDBX and changeset sample counts, and
model/open/DB gauges are all present and internally consistent.

Catch-up throughput below is the catch-up counter delta divided by wall-clock
time from release to acknowledgement of height 42. Production monitoring
should derive the rolling rate with
`rate(reth_tempo_zone_checker_catch_up_blocks_total[5m])`; there is no
reset-sensitive in-process throughput gauge.

## Results

| Measurement | Result |
|---|---:|
| Catch-up blocks | 42 |
| Catch-up elapsed | 0.261133834 s |
| Catch-up throughput | 160.8370672 blocks/s |
| Catch-up block p50 / p95 | 0.642400 ms / 0.913799 ms |
| Live blocks | 40 |
| Live block p50 / p95 | 2.079766 ms / 2.666736 ms |
| Receipt-fetch p50 / p95 | 0.158636 ms / 0.467131 ms |
| Per-token exact-block collateral read p50 / p95 | 0.262857 ms / 0.584882 ms |
| Aggregate local exact-state read p50 / p95 | 0.035044 ms / 0.057249 ms |
| MDBX write transaction p50 / p95 | 0.206564 ms / 0.478287 ms |
| Encoded changeset bytes | 10,876 bytes across 82 blocks |
| Mean encoded changeset bytes/block | 132.634146 bytes |
| Final physical model rows / open records | 9 / 0 |
| Checker directory allocated bytes | 1,269,760 bytes |

The DB value is the sum of allocated regular-file blocks in the ephemeral
checker directory used by the test (`st_blocks * 512` on this Unix host). It
therefore does not mistake MDBX's sparse logical map length for occupied disk
space. Non-Unix builds fall back to logical regular-file lengths. The ephemeral
path changes on every test run; the normal path and override semantics are
documented in [`README.md`](README.md).

## Raw accepted output

The test emitted this structured record:

```json
{"arch":"aarch64","backlog_l1_blocks_requested":40,"catch_up_blocks_per_second":160.83706717223015,"catch_up_elapsed_seconds":0.261133834,"catch_up_zone_blocks":42,"changeset_bytes_per_block_mean":132.6341463414634,"database_allocated_bytes":1269760.0,"live_block_p50_seconds":0.0020797657213373916,"live_block_p95_seconds":0.0026667360041389638,"live_blocks_requested":40,"mdbx_transaction_p50_seconds":0.0002065640481163989,"mdbx_transaction_p95_seconds":0.000478286788002901,"model_rows":9.0,"open_lifecycle_records":0.0,"os":"macos","per_token_collateral_read_p50_seconds":0.0002628570965777004,"per_token_collateral_read_p95_seconds":0.0005848822401915151,"receipt_fetch_p50_seconds":0.00015863572305816942,"receipt_fetch_p95_seconds":0.00046713137143955817,"release_profile":true}
```

Essential raw Prometheus samples from the same recorder snapshot:

```text
reth_tempo_zone_checker_catch_up_blocks_total 42
reth_tempo_zone_checker_live_blocks_total 40
reth_tempo_zone_checker_passed_blocks_total 82
reth_tempo_zone_checker_collateral_calls_total 83
reth_tempo_zone_checker_collateral_call_failures_total 0
reth_tempo_zone_checker_exact_state_reads_total 82
reth_tempo_zone_checker_exact_state_read_failures_total 0
reth_tempo_zone_checker_supply_tokens_requested_total 82
reth_tempo_zone_checker_findings_total 0
reth_tempo_zone_checker_acquisition_failures_total 0
reth_tempo_zone_checker_runtime_healthy 1
reth_tempo_zone_checker_runtime_active_alert 0
reth_tempo_zone_checker_model_rows 9
reth_tempo_zone_checker_open_lifecycle_records 0
reth_tempo_zone_checker_database_allocated_bytes 1269760
reth_tempo_zone_checker_catch_up_block_duration_seconds{quantile="0.5"} 0.0006423995926097846
reth_tempo_zone_checker_catch_up_block_duration_seconds{quantile="0.95"} 0.0009137989039055705
reth_tempo_zone_checker_catch_up_block_duration_seconds_count 42
reth_tempo_zone_checker_live_block_duration_seconds{quantile="0.5"} 0.0020797657213373916
reth_tempo_zone_checker_live_block_duration_seconds{quantile="0.95"} 0.0026667360041389638
reth_tempo_zone_checker_live_block_duration_seconds_count 40
reth_tempo_zone_checker_receipt_fetch_duration_seconds{quantile="0.5"} 0.00015863572305816942
reth_tempo_zone_checker_receipt_fetch_duration_seconds{quantile="0.95"} 0.00046713137143955817
reth_tempo_zone_checker_receipt_fetch_duration_seconds_count 83
reth_tempo_zone_checker_collateral_call_duration_seconds{quantile="0.5"} 0.0002628570965777004
reth_tempo_zone_checker_collateral_call_duration_seconds{quantile="0.95"} 0.0005848822401915151
reth_tempo_zone_checker_collateral_call_duration_seconds_count 83
reth_tempo_zone_checker_exact_state_read_duration_seconds{quantile="0.5"} 0.00003504421390796101
reth_tempo_zone_checker_exact_state_read_duration_seconds{quantile="0.95"} 0.000057249019772150795
reth_tempo_zone_checker_exact_state_read_duration_seconds_count 82
reth_tempo_zone_checker_database_transaction_duration_seconds{quantile="0.5"} 0.0002065640481163989
reth_tempo_zone_checker_database_transaction_duration_seconds{quantile="0.95"} 0.000478286788002901
reth_tempo_zone_checker_database_transaction_duration_seconds_count 82
reth_tempo_zone_checker_changeset_bytes{quantile="0.5"} 129.99552297711733
reth_tempo_zone_checker_changeset_bytes{quantile="0.95"} 129.99552297711733
reth_tempo_zone_checker_changeset_bytes_sum 10876
reth_tempo_zone_checker_changeset_bytes_count 82
```

The extra receipt-fetch and collateral samples are the authenticated Portal
creation block; exact Zone state, block latency, MDBX, and changeset families
begin with the 82 non-genesis Zone blocks.
