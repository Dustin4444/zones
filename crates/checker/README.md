# Zone checker

`zone-checker` is an independent, observe-only checker for the complete Zone
bridge lifecycle. It authenticates canonical Zone and imported Tempo evidence,
applies that evidence to the pure `zone-checker-kernel` state machine, compares
the independently derived effects with implementation outcomes, and commits
verified progress to a dedicated MDBX database before acknowledging Reth.

The checker is not an EVM or precompile replay engine and never changes
consensus. A deterministic divergence becomes a durable, sticky alert;
descendants are acknowledged as not checked because of that ancestor rather
than reported as passing.

See [DESIGN.md](DESIGN.md) for the data flow and persistence contract and
[MODEL_VECTORS.md](MODEL_VECTORS.md) for the independent vector inventory.

## Operator setup

The initial checkpoint must be built locally from the Zone database and the
configured Tempo archive endpoint. A checkpoint is bound to the Zone genesis,
chain identities, Portal, factory creation evidence, and imported Tempo cut.
Third-party checkpoints are not trusted by default.

```sh
tempo-zone checker build-checkpoint \
  --checker.database-path /var/lib/tempo-zone/checker \
  --checker.portal-creation-block-hash 0x... \
  -- \
  --chain /etc/tempo-zone/genesis.json \
  --datadir /var/lib/tempo-zone \
  --l1.rpc-url wss://tempo-archive.example \
  --l1.portal-address 0x... \
  --zone.id 123

tempo-zone \
  --checker.mode observe \
  --checker.database-path /var/lib/tempo-zone/checker \
  --checker.portal-creation-block-hash 0x... \
  --l1.rpc-url wss://tempo-archive.example \
  --l1.portal-address 0x... \
  --zone.id 123
```

Use `tempo-zone checker build-checkpoint --help` for the exact CLI accepted by
the current binary. The default checker database is the node's resolved
checker directory; `--checker.database-path` selects an explicit dedicated
path. `off` remains the default checker mode.

Checkpoint publication uses a sibling staging directory and validates the
completed database before making it available. An existing incompatible
database is preserved: schema changes and corruption require a newly built
path, not an in-place migration.

## Evidence boundary

For every imported Tempo block the checker:

1. obtains the exact full transaction envelopes;
2. hashes each envelope locally and reconstructs `transactions_root`;
3. compares the root with the header authenticated by Zone `advanceTempo`;
4. authenticates the complete ordered receipt vector, receipt root, and bloom;
5. binds each receipt to the locally derived transaction hash and index; and
6. decodes required direct Portal calldata from those authenticated envelopes.

Zone system envelopes, canonical ABI and RLP, dynamic allocation bounds,
receipt cardinality, protocol event topics, fixed state commitments, token
supply, and Portal collateral are checked under the same fail-closed policies.
Unknown activity from a protocol emitter is never silently classified as
verified.

Expected values are constructed only by `zone-checker-kernel`. Generated ABI
types are observation/wire carriers and production protocol transition helpers
are outside the kernel's dependency fence.

## Lifecycle coverage

The kernel covers creation and configuration, token enablement, ordinary and
failed deposits, withdrawal requests and fees, finalization, queue folds,
submission, full and partial processing, successful delivery, pending refunds,
bounce-backs, callback deposits, aggregate claims, empty batches, and complete
owner closure. It preserves checker-owned IDs, sender tags, sentinels, queue
commitments, fees, ownership, refund origins, counters, storage layouts, and
per-token `S/D/W` accounting.

Collateral is checked at the imported Tempo cut before the Zone transition;
fixed Zone state and supply are checked after the Zone transition.

## Durability and runtime behavior

The dedicated database has four tables:

| Table | Purpose |
|---|---|
| `Meta` | Identity, schema, durable tips, coverage, and active alert |
| `Checkpoints` | Immutable bootstrap state and later complete state cuts |
| `Journal` | Ordered canonical per-block deltas and continuity data |
| `Findings` | Compact deterministic findings and canonical lineage |

The bootstrap checkpoint is immutable. The canonical journal is currently
retained without pruning; no unsupported reorg horizon is implied.

The runtime uses one current notification, one bounded FIFO, bounded retry
attempts/timers, and the states `Starting`, `Healthy`, `Retrying`, `Alerting`,
and `Disabled`. Each successful block is committed separately. Reth
`FinishedHeight` is sent only after all covered blocks have committed.

Verified, acknowledged, and gap progress are distinct durable facts. Before
fail-open acknowledgement, every unverified suffix is persisted as a gap. A
finding freezes semantic state at its verified parent and records exact alert
lineage. Restart reconstructs state from a checkpoint plus journal; canonical
reorgs restore the ancestor and apply the replacement branch. Removing the
alerting block orphans its finding and clears the latch atomically. Retaining it
keeps the alert active.

Malformed or unprovable `FinishedHeight` jumps are not treated as verified.
The runtime records the uncovered range and keeps checker state explicit rather
than silently applying an untrusted fragment.

## Failure policy

- **Immediate terminal:** local identity/schema violations and authenticated
  contradictions that cannot become valid by retrying.
- **Bounded retry:** unavailable notification/provider data with a finite
  acquisition budget.
- **Transient retry:** bounded stream/acquisition recovery where progress is
  retained and no default observation is fabricated.
- **Authenticated divergence:** atomically persist a finding and alert before
  acknowledging; descendants remain unverified.

Terminal fail-open behavior disables semantic verification but persists the
exact gap before acknowledgement. It does not report those blocks as passing.

## Trust assumptions and non-claims

The checker authenticates imported transaction and receipt commitments but
does not independently prove consensus finality or data availability. It
trusts the in-process Reth node for its canonical Zone chain and for
hash-pinned local state reads. Those local reads are bound to an exact block
hash but are not accompanied by independently verified state-trie proofs.

Likewise, the configured Tempo archive endpoint is required for historical
envelopes, receipts, and exact imported-block Portal balance reads. Collateral
reads are hash-pinned but do not carry an independently verified storage proof.
Loss of required history is an explicit availability failure, never a zero
value or passing result.

The checker does not independently validate encrypted payload cryptography,
proof systems, arbitrary callback/EVM behavior, private mint recipients, or
the withdrawal-time fallback recipient when that value is not authenticated
at the observation boundary. It checks all lifecycle, ownership, commitment,
and accounting consequences exposed within this boundary.

The removed checker architecture's diagnostic command, retained before-image
history, live bootstrap state machine, and metrics suite are not part of this
runtime. Operational diagnosis uses compact findings plus the named L1/L2
archive coordinates. No performance or production-SLO claim is made by this
document; the current real-node gate establishes functional checkpoint,
processing, persistence, and restart behavior.

## Validation

```sh
cargo +1.95.0 fmt --check
cargo +1.95.0 test -p zone-checker-kernel
RUST_TEST_THREADS=1 cargo +1.95.0 test -p zone-checker
cargo +1.95.0 clippy -p zone-checker-kernel -p zone-checker \
  --all-targets --all-features -- -D warnings
cargo +1.95.0 test -p zone-node --features cli,test-utils --test it checker
```
