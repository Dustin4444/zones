# zone-checker

A durable, observe-only execution extension that independently reconstructs
Portal and Zone state and records authenticated divergences.

The approved architecture and release gate are in [`DESIGN.md`](DESIGN.md).
The checker-owned protocol vectors frozen by Goal 0 are inventoried in
[`MODEL_VECTORS.md`](MODEL_VECTORS.md).

## Current status

Goals 0 through 10 are implemented. Goals 0 through 5 define the authenticated
observation boundary, pure lifecycle model, typed output checks, exact Zone
state reads, and Portal collateral checks. Goals 6 through 8 add the dedicated
checker database, commit-before-acknowledge processing, crash recovery, exact
unwind, durable findings, and alert mode. Goal 9 adds authenticated archive
bootstrap, resumable L1 and Zone replay, normal restart, and fresh-path rebuild.

Goal 10 adds the closed-system lifecycle/recovery matrix, operator health and
diagnostic surfaces, and a reproducible real-node performance baseline. The
checker is alerting, not enforcing. `--checker.mode` remains `off` by default,
and `observe` never changes consensus or blocks core-node progress after an
authenticated finding.

## Operator configuration

Observe mode requires the Zone's exact identity and an archive-capable L1 RPC:

```sh
ZONE_JSON=generated/my-zone/zone.json

tempo-zone \
  --checker.mode observe \
  --checker.portal-creation-block-hash "$(jq -er '.portalCreationBlockHash' "$ZONE_JSON")" \
  --l1.rpc-url wss://<tempo-archive-rpc> \
  --l1.portal-address "$(jq -er '.portal' "$ZONE_JSON")" \
  --zone.id "$(jq -er '.zoneId' "$ZONE_JSON")"
```

The creation hash must be the nonzero Tempo block hash containing the
configured Portal's authenticated `ZoneFactory.ZoneCreated` event. `--zone.id`
must be the factory Zone ID whose derived chain ID matches the local genesis.
`just create-zone` writes the hash to `generated/<name>/zone.json`; the
`tempo-zone dev` command writes the same field to its datadir's `zone.json`.
The equivalent environment variables are `CHECKER_MODE`,
`CHECKER_PORTAL_CREATION_BLOCK_HASH`, `L1_RPC_URL`, `L1_PORTAL_ADDRESS`, and
`ZONE_ID`.

By default, the checker opens `<resolved node data directory>/checker`. This is
a dedicated MDBX database, separate from the node database. Override it with
`--checker.database-path PATH` or `CHECKER_DATABASE_PATH`; the override is valid
only in `observe` mode.

### Bootstrap and restart

A fresh database is initialized only after the checker validates the local
canonical Zone genesis and its nonzero Tempo checkpoint, then authenticates the
configured Portal creation block. It replays exact parent-linked Tempo history
to the genesis anchor, or keeps the Portal-not-yet-created phase when creation
follows that anchor. It replays Zone blocks from genesis through the canonical
head using the ordinary live transition. Each L1 cursor and each Zone block is
committed independently, so a crash resumes after the last durable block
without a gap or double apply.

A normal restart validates the database identity and version, loads its model,
tips, and active alert, reconciles the durable Zone tip with the local canonical
chain, and resumes. It does not replay from genesis. A canonical reorg unwinds
exact before-images and applies the replacement blocks through the same path.

### Archive requirements

Bootstrap and repair require the configured Tempo RPC to retain exact blocks,
complete receipts, selectively required transaction bodies, and historical
Portal balance state from creation onward. The local Zone node must retain
canonical block bodies and the historical execution state needed by Reth's
backfill job from genesis onward. Catch-up re-executes those blocks to produce
receipt sets; the checker authenticates each set against its Zone header before
reading exact post-block state for comparisons. Missing, pruned, or internally
inconsistent history is an explicit failure; the checker never substitutes a
default record or a remote L2 state source.

### Fresh-path rebuild

Model-version changes and database corruption are repaired by archive replay
into a new empty path. There are no in-place migrations.

1. Stop the Zone node and preserve the old checker directory.
2. Choose a nonexistent or empty path, such as a sibling `checker-v3`.
3. Start the same Zone configuration with the fresh path:

   ```sh
   ZONE_JSON=generated/my-zone/zone.json

   tempo-zone \
     --checker.mode observe \
     --checker.portal-creation-block-hash "$(jq -er '.portalCreationBlockHash' "$ZONE_JSON")" \
     --checker.database-path /var/lib/tempo-zone/checker-v3 \
     --l1.rpc-url wss://<tempo-archive-rpc> \
     --l1.portal-address "$(jq -er '.portal' "$ZONE_JSON")" \
     --zone.id "$(jq -er '.zoneId' "$ZONE_JSON")"
   ```

4. Wait for replay to reach the canonical Zone head and verify the configured
   identity and reported health before making the new path permanent. The
   `Durable checker started` log must show the expected `zone_id`,
   `zone_chain_id`, `portal`, `portal_creation_block_hash`, and `database`.
   At the configured Reth Prometheus endpoint, a caught-up, non-alerting checker
   reports exactly:

   ```text
   reth_tempo_zone_checker_runtime_healthy 1
   reth_tempo_zone_checker_runtime_active_alert 0
   ```

Startup refuses a zero creation hash, a zero Tempo checkpoint in Zone genesis,
nonzero Inbox or Outbox protocol progress in Zone genesis, a creation event or
database identity mismatch, unsupported database version, nonempty rebuild
path, broken exact-hash ancestry, receipt authentication failure, or
unavailable required archive history. Existing incompatible data is not
modified or deleted.

## Authenticated observation boundary

The observer keeps authenticated inputs separate from authenticated
implementation outcomes. Raw data is decoded into typed observations while one
canonical block is processed, but those observations are never persisted. The
database stores authoritative model state, exact unwind data, compact findings,
bootstrap progress, and verified tips.

| Data | Authentication or trust source | Missing or inconsistent data |
|---|---|---|
| Imported Tempo header, deposits, decryptions, enabled tokens | Canonical `advanceTempo` calldata in the first Zone system transaction; exact ABI and header-RLP round trips | Malformed authenticated data |
| Optional finalization count, block number, encrypted senders | Canonical `finalizeWithdrawalBatch` calldata in the unique final Zone system transaction | Invalid envelope or malformed authenticated data |
| Zone protocol outcomes and containing transaction hashes | Complete ordered notification or backfill receipts authenticated against the canonical Zone header's receipt root and logs bloom, then paired with the recovered block | Missing or internally inconsistent notification block/receipt data is an acquisition failure; protocol events fail closed |
| Six fixed Zone commitments and model-selected token supplies | In-process `state_by_block_hash` at the exact canonical Zone hash | Retryable acquisition failure; an absent account or unwritten slot has canonical EVM value zero after the exact block state is acquired |
| Ordered Tempo protocol outcomes | Complete receipt set authenticated against the receipt root and logs bloom in the imported header | Retryable acquisition failure or fail-closed protocol-event error |
| Direct `submitBatch` and non-empty `processWithdrawals` inputs | Selectively fetched transaction body, bound by hash/block/index metadata, with exactly one top-level call to the configured Portal | Retryable acquisition failure, malformed calldata, or `UnsupportedNestedPortalCall` |

Implementation events never choose an independently knowable input or
commitment. In particular, `TempoAdvanced` is compared with the imported
header derived from `advanceTempo`; it is not an L1 block anchor. Outcome-only
branches and private recipients come from their authenticated events, while
the model independently checks the required identity movement, queue update,
ownership, and `S/D/W` accounting.

### Zone block checks

For each non-genesis canonical Zone block, the observation layer:

1. Requires equal transaction, recovered-sender, and receipt cardinalities.
2. Requires the first transaction to be the successful system call to the
   Inbox and canonically decodes `advanceTempo`.
3. Allows at most one later system transaction, requires it to be the
   successful final transaction to the Outbox, and canonically decodes
   `finalizeWithdrawalBatch`.
4. Retains supported protocol logs in transaction/receipt order with the
   containing transaction hash.
5. Retains model-driving inputs and implementation outcomes as distinct typed
   values.

The checker then compares `TempoAdvanced`,
`TempoBlockFinalized`, `TokenEnabled`, deposit outcomes, Zone operations, and
optional `BatchFinalized` against model-owned expectations. It reads the six
fixed commitments and every enabled token's literal slot-8 supply after the
Zone block. Before applying the Zone transition, it checks one
exact-imported-block Portal balance per Portal-enabled token against the
checked `S + D + W` requirement.

Dynamic ABI counts, offsets, and byte lengths are checked before generated
decoders allocate. Calldata and Tempo-header RLP must re-encode byte-for-byte,
with no trailing data.

The observation layer checks only finalization relationships defined without
mutable model state: canonical shape, count versus sender-array length, sender
length, Zone block number, envelope position, and successful receipt. The
projection/model layer checks event grammar, pending-count, reveal-mode,
batch identity, and exact output commitments.

### Tempo block checks

The Tempo adapter takes only the imported header decoded above. It:

1. Fetches the exact block by the imported hash using hash-only transaction
   bodies.
2. Checks the RPC-reported hash, locally computed hash, number, and complete
   fetched header identity against the imported header.
3. Fetches every receipt for that exact hash and checks cardinality plus every
   receipt's block hash, block number, transaction index, and transaction hash.
4. Recomputes the receipt root and aggregate logs bloom against the imported
   header, never against values selected from the fetched response.
5. Classifies successful protocol logs in receipt-vector order. Known
   non-model events decode and are dropped; unknown or malformed events from a
   protocol emitter fail closed; unrelated emitters are ignored.
6. Fetches a transaction body only when an authenticated `BatchSubmitted` or
   withdrawal-processing outcome requires direct calldata.

An eventful nested or ambiguous Portal call is unsupported. A required direct
transaction must contain exactly one top-level Tempo call and that call must
target the configured Portal. An empty `processWithdrawals` has no protocol
outcome and creates no transaction-fetch requirement.

The configured L1 archive RPC remains an explicit trust boundary for
receipt-to-transaction metadata after receipt-root authentication. It does
not fetch every transaction body or recompute the transaction root.

### Operator trust and non-claims

The configured L1 archive RPC is trusted for availability, exact-block Portal
balance values, and the block/hash/index binding of selectively fetched
transaction bodies. The checker authenticates the imported header and complete
receipt vector against header commitments and locally hashes each selectively
fetched body, but it does not obtain a transaction-root proof or an independent
state-trie proof for collateral.

The in-process Zone node is trusted to expose the canonical L2 chain, complete
notification/backfill receipts, and exact historical state for canonical block
hashes. There is no remote L2 fallback. Release one also does not validate
AES-GCM, Chaum-Pedersen proofs, encrypted-sender validity, arbitrary token
transfers, callbacks, or TIP-403 policy; prove independent finality or data
availability; or enforce/stop consensus. It is an observe-only logical and
accounting divergence detector within those explicit boundaries.

In particular, it cannot verify a successful deposit mint's private recipient,
or that a bounce-back mint/refund recipient equals the withdrawal-time
`zoneFallbackRecipient`. It also does not prove arbitrary Portal storage beyond
authenticated event/call inputs and the exact-block balance value it queries.

## Failure classes

- Acquisition failures cover unavailable, absent, or internally inconsistent
  notification, RPC, transaction, receipt, or exact-state data. Remote failures
  can be retried; an inconsistent in-process notification is still an
  operational acquisition failure rather than a protocol finding. No
  acquisition failure becomes a zero/default observation.
- Invalid envelopes cover caller, destination, position, success, and
  finalization block-number rules.
- Malformed authenticated data covers non-canonical or structurally unsafe ABI
  and RLP.
- Protocol-event errors fail closed for malformed known events, unknown topics
  from protocol emitters, and explicitly unsupported native events such as
  `DepositRejected`; they retain chain, transaction hash/index, and receipt and
  block log coordinates.
- Portal-call reconciliation errors cover nested or ambiguous calls,
  conflicting event-implied call families, calldata/event family mismatches,
  and eventful empty `processWithdrawals` bodies.
- Dedicated typed findings report observation, continuity, model-transition,
  Portal/Zone output, fixed-state, supply, and collateral mismatches. There is
  no generic invariant or value registry.

## Runtime behavior

| Mode | Behavior |
|---|---|
| `off` | Default. The checker ExEx is not installed. |
| `observe` | Bootstrap or resume the durable model, authenticate canonical blocks, persist passing transitions and findings, and handle restart and reorg recovery. Never enforce protocol behavior. |

Committed and reorged-in blocks are checked oldest-to-newest. Acquisition
failures retain the current notification and retry without advancing durable
progress. A deterministic divergence is committed once and enters alert mode:
the model remains frozen at the last verified parent while the ExEx continues
acknowledging descendants. If a reorg removes the finding, its record is marked
orphaned and ordinary checking resumes from the verified parent.

### Health and critical logs

Reth prefixes checker families with `reth_` at its Prometheus endpoint. In
`off` mode the checker is not installed, so checker series are absent rather
than reporting healthy. In `observe` mode the two status gauges have these
exact meanings:

| Runtime state | `reth_tempo_zone_checker_runtime_healthy` | `reth_tempo_zone_checker_runtime_active_alert` |
|---|---:|---:|
| Initial startup before durable state is loaded | 0 | 0 |
| Archive replay, canonical catch-up, retry, or stream failure | 0 | Preserves the current alert state |
| Caught up and following the live head | 1 | 0 |
| Durable authenticated finding, including acknowledged descendants | 0 | 1 |
| Terminal checker failure in fail-open acknowledgement mode | 0 | Preserves the current alert state |

`active_alert` survives restart and remains set until a canonical reorg removes
the finding. `healthy` returns to 1 only after acquisition recovers and the
checker is again caught up and non-alerting. Alert on a missing series
separately from a zero value when `observe` is expected.

The `zone::checker` target emits stable operator messages:

| Level | Message | Meaning |
|---|---|---|
| INFO | `Durable checker started` | Identity, DB path, durable tip, and alert state were loaded and catch-up was installed. |
| INFO | `Checker archive replay reached live handoff` | Fresh or resumed Zone replay reached the canonical head. |
| WARN | `Checker acquisition unavailable; retaining notification` | No state was committed or acknowledged past the gap; the exact notification is being retried. |
| INFO | `Checker acquisition recovered` | A retained notification passed after one or more retries. |
| ERROR | `Checker entered durable alert mode after an authenticated finding` | The first deterministic divergence was committed and the model was frozen at its parent. |
| ERROR | `Checker disabled after a terminal failure; continuing fail-open acknowledgement` | Checker evaluation stopped, health is 0, and core-node progress continues. |

### Metrics

All checker metrics are fixed-cardinality: no token, address, block hash, or
finding labels are emitted. Duration families are Prometheus summaries. The
node's configured summary quantiles determine which `quantile` samples are
available.

| Exact exported family | Unit and semantics |
|---|---|
| `reth_tempo_zone_checker_live_block_duration_seconds` | End-to-end successful live-block time from durable preflight through MDBX commit, including exact L1 and local acquisition. Archive-backfill block samples are excluded. |
| `reth_tempo_zone_checker_catch_up_block_duration_seconds` | The same boundary for successful Reth archive-backfill blocks. |
| `reth_tempo_zone_checker_live_blocks_total`, `reth_tempo_zone_checker_catch_up_blocks_total` | Successful samples in the corresponding phase. Derive throughput from `rate`, not an in-process rate gauge. |
| `reth_tempo_zone_checker_receipt_fetch_duration_seconds` | Successful `eth_getBlockReceipts` RPC boundary for the exact imported Tempo hash; receipt authentication follows before the sample is accepted. |
| `reth_tempo_zone_checker_collateral_call_duration_seconds` | One exact-imported-block Portal balance call per Portal-enabled token. This is the per-token L1 read baseline. |
| `reth_tempo_zone_checker_exact_state_read_duration_seconds` | One aggregate local Zone exact-hash acquisition per block, including six fixed slots and all requested token-supply slots. It is not a per-token metric. |
| `reth_tempo_zone_checker_database_transaction_duration_seconds` | Successful checker MDBX write transaction from `tx_mut` through commit. No provider call occurs inside it. |
| `reth_tempo_zone_checker_changeset_bytes` | Canonically encoded changeset key plus before-image bytes for one applied block. |
| `reth_tempo_zone_checker_changeset_bytes_total` | Cumulative encoded changeset bytes, useful for rate and mean calculations. |
| `reth_tempo_zone_checker_model_rows` | Current physical rows in `CheckerModelState`, read from the committed MDBX table. |
| `reth_tempo_zone_checker_open_lifecycle_records` | Current nonterminal deposit, withdrawal, batch, fallback, Portal-refund, and Inbox-refund owners. |
| `reth_tempo_zone_checker_database_allocated_bytes` | Allocated bytes for regular files in the dedicated checker directory (`st_blocks * 512` on Unix; logical file length fallback on non-Unix). |
| `reth_tempo_zone_checker_observation_duration_seconds` | Full authenticated L2 and L1 observation, including L1 block, receipt, and selectively required transaction RPC acquisition. |
| `reth_tempo_zone_checker_transition_duration_seconds` | Model and comparison work with separately timed collateral and exact-state acquisition subtracted. |
| `reth_tempo_zone_checker_collateral_calls_total`, `reth_tempo_zone_checker_collateral_call_failures_total` | Per-token exact-block collateral attempts and failures. |
| `reth_tempo_zone_checker_exact_state_reads_total`, `reth_tempo_zone_checker_exact_state_read_failures_total`, `reth_tempo_zone_checker_supply_tokens_requested_total` | Aggregate exact-state attempts/failures and token slots requested. |
| `reth_tempo_zone_checker_latest_observed_zone_height`, `reth_tempo_zone_checker_latest_checked_zone_height`, `reth_tempo_zone_checker_model_lag_blocks` | Last attempted and last passing Zone heights plus their difference. Alert-mode descendants are acknowledgement-only and do not advance these gauges. |
| `reth_tempo_zone_checker_passed_blocks_total`, `reth_tempo_zone_checker_acquisition_failures_total`, `reth_tempo_zone_checker_findings_total` | Passing blocks, acquisition failures, and deterministic findings. |
| `reth_tempo_zone_checker_runtime_operational_retries_total`, `reth_tempo_zone_checker_runtime_operational_recoveries_total` | Retries include retained acquisition, stream-poll, and canonical-head attempts. Recoveries count only retained-notification acquisition gaps that later pass at the live head. |

Useful PromQL expressions are:

```promql
# Live p50 and p95 (when those summary quantiles are configured)
reth_tempo_zone_checker_live_block_duration_seconds{quantile="0.5"}
reth_tempo_zone_checker_live_block_duration_seconds{quantile="0.95"}

# Archive catch-up blocks per second
rate(reth_tempo_zone_checker_catch_up_blocks_total[5m])

# Per-token collateral-read and receipt-fetch p95
reth_tempo_zone_checker_collateral_call_duration_seconds{quantile="0.95"}
reth_tempo_zone_checker_receipt_fetch_duration_seconds{quantile="0.95"}

# Mean encoded changeset bytes per applied block over five minutes
rate(reth_tempo_zone_checker_changeset_bytes_total[5m])
/
(
  rate(reth_tempo_zone_checker_live_blocks_total[5m])
  + rate(reth_tempo_zone_checker_catch_up_blocks_total[5m])
)

# Page when observe mode is expected but unhealthy, absent, or alerting
(reth_tempo_zone_checker_runtime_healthy != 1)
or absent(reth_tempo_zone_checker_runtime_healthy)
or (reth_tempo_zone_checker_runtime_active_alert == 1)
```

### Diagnosis and recovery

The offline diagnostic reconstructs one typed model key immediately before and
after a retained canonical Zone height and prints the canonical block and
changeset coordinates needed to locate archive evidence. Stop the node first,
or run it against a consistent copy of the checker directory:

```sh
tempo-zone checker diagnose \
  --database-path /var/lib/tempo-zone/checker \
  --zone-height 12345 \
  --key token:0x0000000000000000000000000000000000000001
```

Run `tempo-zone checker diagnose --help` for the live CLI contract. Key
selectors are the singleton names `portal-config`, `zone-config`,
`portal-deposit-cursor`, `zone-processed-deposit-cursor`, `portal-settlement`,
`zone-batch-accumulator`, `zone-next-withdrawal-index`, and
`zone-last-fallback-nonce`; or one of:

```text
token:<address>
pending-deposit:<number>
withdrawal:<index>
fallback-owner:<nonce>
batch:<index>
portal-refund-credit:<token>:<recipient>:<origin>
inbox-refund-credit:<token>:<recipient>:<origin>
```

`withdrawal:<index>` and the Inbox-refund origin admit zero. Pending-deposit
numbers, fallback nonces, batch indexes, and Portal-refund origins are nonzero.
The report prints decoded and exact encoded before/after values; an unchanged
key is explicit and has no changeset ordinal. `--zone-height` must select a
retained, non-genesis canonical height; height 0 fails because it has no parent
boundary.

Use the failure class, not an in-place DB edit, to choose recovery:

| Condition | Operator action |
|---|---|
| Retryable archive/RPC acquisition gap | Restore exact historical access or RPC availability. Leave the DB in place; the retained item retries without a default observation. |
| Authenticated finding | Preserve the DB and inspect the reported key/block plus L1/L2 archive evidence. Descendants remain unchecked. A finding-removing canonical reorg is orphaned automatically. |
| Terminal checker failure | Core-node progress is fail-open, but checker health stays 0. Correct the configuration/software/archive fault and restart before treating the node as shadow-covered. |
| Unsupported model version or corruption | Follow the fresh-path rebuild procedure above. Never migrate, truncate, or delete the old DB in place. |

The measured release-profile methodology and raw accepted samples are recorded
in [`PERFORMANCE_BASELINE.md`](PERFORMANCE_BASELINE.md).

## Validation

```sh
cargo +1.95.0 test -p zone-checker
cargo +1.95.0 clippy -p zone-checker --all-targets -- -D warnings
cargo +1.95.0 test -p zone-checker --all-targets --features test-utils
cargo +1.95.0 clippy -p zone-checker --all-targets --features test-utils -- -D warnings
cargo fmt --check
cargo +1.95.0 test -p zone-node --features cli,test-utils --lib cli::
cargo +1.95.0 test -p zone-node --features cli,test-utils --test it \
  checker_e2e -- --nocapture --test-threads=1
cargo +1.95.0 test -p zone-node --features cli,test-utils --test it \
  test_zone_advances_with_real_l1 -- --nocapture --test-threads=1
cargo +1.95.0 test --release -p zone-node --features cli,test-utils --test it \
  test_checker_real_node_performance_baseline -- --ignored --nocapture --test-threads=1
```

Named lifecycle and recovery evidence includes
`release_one_lifecycle_matrix_reaches_all_six_terminals_without_lost_owners`,
`every_open_owner_phase_survives_restart_rebuild_and_reorg`,
`test_checker_bootstraps_and_restarts_from_durable_database`,
`test_checker_alert_does_not_stall_zone_progress`,
the tests in `check/tests/lifecycle/{creation_and_deposits,zone,settlement,refunds,fixed_state}.rs`,
and the runtime `atomicity`, `loop_retry`, `replay`, `startup`, and `alert` suites.
