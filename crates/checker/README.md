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

See [DESIGN.md](DESIGN.md) for the data flow and persistence contract.

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

Use `tempo-zone checker build-checkpoint --help` for CLI details. Observe mode
requires `--checker.database-path`.
`--checker.acquisition-timeout-secs` bounds each authenticated block acquisition
attempt and defaults to 30 seconds. The checker is off by default.

Checkpoint publication uses a sibling staging directory and validates the
completed database before making it available. An existing incompatible
database is preserved: schema changes and corruption require a newly built
path.

## What it checks

For each imported Tempo block, the checker reconstructs the transaction root
from full envelopes and authenticates the complete receipt set, receipt root,
and bloom. It strictly decodes protocol calldata and events, then compares the
kernel's expected bridge effects with receipts and exact state reads.

The kernel covers portal creation, token enablement, deposits, withdrawals,
batches, bounce-backs, refunds, callbacks, ownership, commitments, and
per-token accounting. Collateral is checked after the Tempo import and before
the Zone transition. Zone commitments and token supply are checked after the
Zone transition.

## Durability and runtime behavior

The dedicated database has four tables:

| Table | Purpose |
|---|---|
| `Meta` | Identity, schema, durable tips, coverage, and active alert |
| `Checkpoints` | Immutable bootstrap state and later complete state cuts |
| `Journal` | Ordered canonical per-block deltas and continuity data |
| `Findings` | Deterministic findings and canonical lineage |

The bootstrap checkpoint is immutable. The canonical journal is retained
without pruning; no unsupported reorg horizon is implied.

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
