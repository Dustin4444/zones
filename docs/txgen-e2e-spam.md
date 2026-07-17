# Zone E2E Properties and Txgen Spam

Correctness and load generation are intentionally separate:

- Rust `proptest` E2E tests own bridge, TIP-403, transfer, withdrawal, and
  bounceback invariants against real in-process Tempo L1 and Zone nodes.
- `scripts/txgen-e2e-spam.sh` is one randomized multi-sender L2 TIP-20 transfer
  primitive for measuring sustained submission and Zone execution throughput.

The spammer does not mutate policies or run a deposit/withdrawal sweep. Its
setup deposits only enough pathUSD to fund the generated senders.

## Build

Build the Zone node and the sibling txgen checkout:

```bash
cargo build --release --bin tempo-zone
cargo build --release \
  --manifest-path ../txgen/Cargo.toml \
  -p txgen-tempo \
  -p bench-cli
```

The defaults expect `../txgen/target/release/txgen-tempo` and
`../txgen/target/release/bench`. Override `TXGEN_DIR`, `TXGEN_TEMPO_BIN`, or
`BENCH_BIN` when the checkout lives elsewhere.

## Start a local Zone

Start a fresh Tempo-mode Anvil in terminal 1:

```bash
anvil --network tempo --block-time 1
```

Anvil must already expose TIP-1091's ZoneFactory at its protocol address.
The CI lifecycle script installs it into its disposable Anvil instance; the
standalone spammer deliberately does not modify the L1.

Start the Zone in terminal 2:

```bash
cargo run --release --bin tempo-zone -- dev \
  --l1.rpc-url ws://127.0.0.1:8545 \
  -- --zone.batch-interval-blocks 10 \
     --txpool.max-account-slots 1024
```

The larger per-account pool allows the default 1,000 TPS attempt to queue more
than the normal 16 pending transactions per sender. The YAML also selects a
random sender from eight mnemonic accounts and a random Tempo nonce lane.

This starts one sequencer. The result measures single-sequencer admission,
execution, and L1 batch submission; follower replication is not part of this
benchmark.

## Run the spammer

In terminal 3:

```bash
just txgen-e2e-spam
```

The default sends 5,000 transfers at a target of 1,000 TPS. A smaller example:

```bash
just txgen-e2e-spam 1000 250
```

The runner reads the portal, initial token, and Zone RPC from the `zone.json`
written by `tempo-zone dev`. On macOS that defaults to
`$TMPDIR/tempo-zone-dev/zone.json`; set `ZONE_DATADIR` or `ZONE_JSON` if needed.

Before the measured workload it funds every sender for the worst-case random
selection. The attempt passes when txgen admits every transaction, the dedicated
recipient receives the exact aggregate amount, and the portal submits an L1
batch at or beyond the workload's final Zone block. The final line reports:

```text
accepted=5000/5000 target_tps=1000 submission_tps=... zone_tps=... settled_l2_block=...
```

Useful controls:

| Variable | Default | Purpose |
| --- | --- | --- |
| `COUNT` | `5000` | Number of measured L2 transfers |
| `TPS` | `1000` | Submission target; `0` means unlimited |
| `TXGEN_TRANSFER_ACCOUNTS` | `8` | Random sender pool size |
| `TXGEN_NONCE_LANES` | `1000000` | Random parallel nonce-key range |
| `MAX_CONCURRENT` | `2000` | Bench in-flight request limit |
| `TXGEN_TRANSFER_AMOUNT` | `1000` | Amount per transfer |
| `TXGEN_DEPOSIT_AMOUNT` | `5000000000` | Setup deposit size used to fund senders |
| `DRAIN_TIMEOUT` | `240` | Seconds allowed for bench to drain |
| `SYNC_TIMEOUT` | `300` | Seconds allowed for balances and L1 settlement |
| `TXGEN_REPORT_DIR` | `$ZONE_DATADIR/txgen-reports` | JSON report directory |

The standalone primitive currently requires the transfer token and fee token to
be pathUSD and requires its active L1 transfer policy to be builtin allow-all
policy `1`. Policy behavior is tested in Rust instead of changed by the load
generator.

## Rust E2E properties

The model-based property generates amounts and action order while guaranteeing
coverage of reject-all, allow-all, whitelist, blacklist, and compound behavior
for both the initial and a dynamically enabled token. It checks deposits,
policy synchronization, L2 transfer receipts and balances, and L1 withdrawal
settlement.

Four additional proptests cover every existing Rust E2E bounceback branch:

- plaintext router callback/deposit failure;
- encrypted router callback/deposit failure;
- encrypted policy rejection refunded to the L1 depositor; and
- cross-zone encrypted policy rejection paid to an explicit bounceback recipient.

Run one generated case of the state machine with:

```bash
cargo test -p zone-node --test it \
  e2e_property::test_bridge_policy_transfer_state_machine_property -- --exact
```

Run the bounceback properties with the normal integration suite, or filter them:

```bash
cargo test -p zone-node --test it bounceback_property -- --test-threads=1
cargo test -p zone-node --test it \
  l1_e2e::test_cross_zone_encrypted_bounceback_recipient_property -- --exact
```

`ZONE_E2E_PROPERTY_CASES` increases complete generated cases. The default is one
because each case launches real nodes and the state machine already covers the
full behavior matrix. `ZONE_E2E_PROPERTY_SHRINK_ITERS` defaults to four; set it
to zero for a quick diagnostic run. Proptest prints the failing input and seed
for replay.

## CI

The normal Rust test job runs the properties. The separate `txgen L2 transfer
spam` job starts Anvil and one Zone sequencer, sends the default 5,000-transfer
1,000 TPS workload, uploads the report and node logs, and posts one concise PR
comment with accepted count, submission TPS, and observed Zone TPS.
