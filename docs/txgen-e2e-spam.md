# Txgen Bridge and Policy E2E Spam

This flow drives four real workload classes through an Anvil-backed local Zone:

- L1 `ZonePortal.deposit` calls and L1-to-L2 ingestion;
- L2 `ZoneOutbox.requestWithdrawal` calls, batch settlement, and L1 processing;
- L1 TIP-403 policy creation, membership changes, compound-policy creation, and
  `ITIP20.changeTransferPolicyId` calls for every enabled token; and
- L2 TIP-20 transfers whose receipts and balances prove each inherited policy
  allows or rejects the expected traffic.

The runner discovers the portal and Zone RPC from the `zone.json` written by
`tempo-zone dev`. It uses the YAML specs and minimal ABIs in `scripts/txgen/`.

## Build

Build the Zone node and the sibling txgen checkout in release mode:

```bash
cargo build --release --bin tempo-zone
cargo build --release \
  --manifest-path ../txgen/Cargo.toml \
  -p txgen-tempo \
  -p bench-cli
```

By default, the runner expects `../txgen/target/release/txgen-tempo` and
`../txgen/target/release/bench`. Set `TXGEN_DIR`, `TXGEN_TEMPO_BIN`, or
`BENCH_BIN` to override those paths.

## Start the L1 and Zone

This path requires Foundry 1.8 or newer, or a nightly built July 11, 2026 or
later. Check `anvil --version` before starting; older Tempo-mode Anvil builds
do not return the canonical Tempo header representation required by the Zone
subscriber.

In terminal 1, start a fresh Tempo-mode Anvil:

```bash
anvil --network tempo --block-time 1
```

In terminal 2, start the Zone with the default dev key and default datadir:

```bash
cargo run --release --bin tempo-zone -- dev \
  --l1.rpc-url ws://127.0.0.1:8545 \
  -- --zone.batch-interval-blocks 10
```

The shortened batch interval makes the withdrawal E2E assertion complete
quickly. The standard batch interval also works; increase `SYNC_TIMEOUT` if it
is longer than the runner's default 180 seconds.

`tempo-zone dev` requires TIP-1091's ZoneFactory to exist at its fixed L1
address. The CI wrapper installs it into the fresh Anvil process internally.
The spam runner itself does no L1 provisioning: local `just txgen-e2e-spam`
only needs an already-running Zone and its `zone.json`.

That command starts one sequencer. The throughput measurements below are
single-sequencer results; follower replication is outside this runner's scope.

## Run the Complete Matrix

In terminal 3, run 20 transactions per bridge workload, policy mode, and
transfer outcome, capped at 10 transactions per second:

```bash
just txgen-e2e-spam all 20 10
```

The `all` action is intentionally ordered:

1. Submit L1 deposits and wait for their exact net amount to appear on L2.
2. Exercise every TIP-403 policy mode on every enabled token using real L2
   TIP-20 transfers.
3. Restore allow-all and submit L2 withdrawal requests, then wait for the L1
   portal queue to drain.
4. Run one measured allow-all TIP-20 transfer workload, verify every receipt,
   and wait for its ending Zone block to settle on L1.

The final workload uses `COUNT` and `TPS` unless
`TXGEN_THROUGHPUT_COUNT` or `TXGEN_THROUGHPUT_TPS` overrides them. This lets CI
keep the bridge and policy matrix small while measuring a larger transfer run
from the same `all` invocation.

The default policy matrix covers five behaviors:

| Mode | Policy setup | L2 transfer assertions |
| --- | --- | --- |
| `allow-all` | Built-in policy `1` | allowed target succeeds |
| `reject-all` | Built-in policy `0` | denied target reverts |
| `whitelist` | Fresh whitelist containing the sender and allowed target | allowed succeeds; non-member reverts |
| `blacklist` | Fresh blacklist containing the denied target | non-member succeeds; member reverts |
| `compound` | sender whitelist + recipient blacklist + mint allow-all | allowed recipient succeeds; blacklisted recipient reverts |

For each phase the runner submits `COUNT` policy assignments per enabled token,
confirms the L1 token exposes the assigned policy ID, and waits for the zone to
enforce it with zero-value `eth_call` transfer probes. These probes exercise the
same TIP-20 precompile path as the stress transactions; the zone token's local
`transferPolicyId()` storage is not the L1-derived cache used by enforcement.
The runner then checks the relevant TIP-403 authorization view and submits
`COUNT` L2 transfers per expected outcome. JSON bench reports must contain the
exact accepted-submission count. Because ordinary txgen templates do not wait
for receipts, the runner independently scans the reported block range and
requires exactly `COUNT` matching TIP-20 receipt statuses for each outcome.
Allowed-recipient balances must increase by exactly `COUNT *
TXGEN_TRANSFER_AMOUNT`; denied-recipient balances must not change.

## Run One Phase

Each workload can also run independently:

```bash
just txgen-e2e-spam policies 100 25
just txgen-e2e-spam deposits 100 25
just txgen-e2e-spam withdrawals 100 25
```

For a single throughput attempt, use the dedicated allow-all transfer action:

```bash
COUNT=5000 TPS=1000 scripts/txgen-e2e-spam.sh throughput
```

The attempt passes only if every submission is admitted, every matching receipt
succeeds, the exact recipient balance delta lands, and the portal's L1
`withdrawalBatchIndex` and `blockHash` prove settlement through or beyond the
last transfer block.

## CI Throughput Spam

The `txgen e2e spam` job in `.github/workflows/test.yml` invokes `all` once. It
runs deposits, policies, and withdrawals with two transactions per workload,
then one single-rate transfer attempt rather than a sweep: 5,000 TIP-20
transfers at a target of 1,000 TPS using eight senders. The CI Zone raises
`--txpool.max-account-slots` to 1,024 so the benchmark measures Zone execution
instead of stopping at the default 16 pending transactions per sender.

The job verifies every transfer receipt, the exact recipient balance delta,
and settlement of the ending Zone block on L1. It uploads the txgen JSON
reports and both node logs, then writes one concise result to the workflow
summary and sticky PR comment: accepted transactions, verified TIP-20 TPS, and
total Zone TPS.

The `policies` action automatically sets allow-all and deposits enough of each
enabled token to cover its transfer matrix. It requires the txgen account to
hold enough of those tokens on L1. The withdrawal-only action still assumes the
selected bridge token already has enough L2 balance.

## Configuration

The most useful environment variables are:

| Variable | Default | Purpose |
| --- | --- | --- |
| `ZONE_DATADIR` | System temp directory + `/tempo-zone-dev` | Datadir containing the generated `zone.json`; this matches `tempo-zone dev` (`$TMPDIR/tempo-zone-dev` on macOS, `/tmp/tempo-zone-dev` on Linux) |
| `L1_HTTP_URL` | `http://127.0.0.1:8545` | L1 endpoint used by txgen, bench, and checks |
| `COUNT` | `20` | Transactions per bridge workload and per policy transfer outcome; policy assignments are also repeated this many times |
| `TPS` | `10` | Bench submission rate; `0` means unlimited |
| `TXGEN_THROUGHPUT_COUNT` | `COUNT` | Transfer count for the measured final workload in `all` |
| `TXGEN_THROUGHPUT_TPS` | `TPS` | Submission target for the measured final workload in `all` |
| `TXGEN_NONCE_LANES` | `100` | Number of reusable Tempo parallel nonce lanes |
| `TXGEN_DEPOSIT_AMOUNT` | `1000000` | Gross amount per L1 deposit, including the portal fee |
| `TXGEN_WITHDRAWAL_AMOUNT` | `1000` | Amount per L2 withdrawal request |
| `TXGEN_TRANSFER_AMOUNT` | `1000` | Amount per in-zone TIP-20 transfer |
| `TXGEN_TRANSFER_FEE_BUFFER` | `100000` | Conservative L2 balance reserve per attempted transfer when auto-funding policy tests |
| `TXGEN_TOKEN` | `initialToken` from `zone.json` | Token used for bridge traffic |
| `TXGEN_POLICY_MODES` | `all` | `all`, or a comma-separated subset of `allow-all,reject-all,whitelist,blacklist,compound` |
| `TXGEN_ANVIL_BOOTSTRAP_ADMIN` | `auto` | Grant the txgen account TIP-20 admin storage only when the endpoint identifies as Anvil; use `false` to require existing on-chain authority |
| `TXGEN_ALLOWED_RECIPIENT` | mnemonic account 1 | Target expected to pass the active policy |
| `TXGEN_DENIED_RECIPIENT` | mnemonic account 2 | Target expected to fail restrictive policies |
| `TXGEN_REPORT_DIR` | `$ZONE_DATADIR/txgen-reports` | JSON benchmark reports used for exact receipt-outcome checks |
| `SYNC_TIMEOUT` | `180` | Seconds allowed for L1/L2 ingestion and withdrawal processing |
| `DRAIN_TIMEOUT` | `120` | Seconds bench waits for each target txpool to drain |

For example:

```bash
TXGEN_NONCE_LANES=500 \
TXGEN_DEPOSIT_AMOUNT=2000000 \
TXGEN_POLICY_MODES=whitelist,blacklist,compound \
SYNC_TIMEOUT=300 \
just txgen-e2e-spam all 1000 100
```

The YAML signer pool is account 0 of the standard local-dev mnemonic. The
runner verifies that this address equals the generated Zone admin before it
sends anything. Txgen currently accepts mnemonic-derived signer pools, not an
arbitrary raw `DEV_KEY`; a custom `tempo-zone dev --dev.key` therefore needs a
matching `TXGEN_MNEMONIC` or a txgen signer-source extension.

The deposit approval uses a TIP-1009 expiring nonce. This avoids racing the
zone batch submitter, which intentionally shares the dev account's standard
nonce lane, and keeps repeated stress runs from reusing a persistent setup
nonce.

Policy spam requires the txgen account to hold `DEFAULT_ADMIN_ROLE` and enough
L1 balance on every enabled token. Tempo-mode Anvil currently initializes
pathUSD without granting its standard dev account that role, so the default
`TXGEN_ANVIL_BOOTSTRAP_ADMIN=auto` fills only the computed role-membership slot
through Anvil's development RPC and verifies it. This never runs against a
non-Anvil client. Set it to `false` to require existing authority even on
Anvil. The runner checks or bootstraps all token roles before creating a policy.
Policy modes are parsed into a canonical set before any RPC mutation; unknown,
empty, or duplicate modes fail early.

## What the Runner Verifies

Before sending, the runner checks both RPCs, the generated portal code, chain
IDs, the selected token, the mnemonic/admin match, and the complete enabled
token list. After sending, it requires:

- the expected post-fee deposit balance increase on L2;
- every requested policy assignment and authorization result to be visible
  through L2;
- exact success/revert receipt counts and exact allowed/denied target balance
  changes for in-zone TIP-20 transfers; and
- for the `throughput` action, an L1 portal batch whose canonical Zone block
  hash is at or beyond the last transfer block; and
- the L1 withdrawal queue head to advance by at least `COUNT` and equal its
  tail, proving the requested withdrawals were settled and processed.

Stop Anvil and `tempo-zone dev` with Ctrl-C in the terminals where they were
started. The runner does not start, stop, or delete either process or datadir.
