# Zone checker

`zone-checker` observes one Zone and its imported Tempo blocks. It authenticates
block data, evaluates bridge transitions, compares the resulting effects and
state, and stores progress in a dedicated MDBX database before acknowledging
Reth.

The checker does not affect consensus. When it finds a mismatch, it commits an
active finding. Descendants are acknowledged as not checked until a reorg
removes the finding's block.

See [DESIGN.md](DESIGN.md) for the data flow and persistence contract.

## Operator setup

Build the initial checkpoint from the local Zone database and a Tempo archive
endpoint. The checkpoint is bound to the Zone genesis, chain IDs, Portal,
creation block, and imported Tempo block.

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
requires `--checker.database-path` and
`--checker.portal-creation-block-hash`. The checker is off by default.
`--checker.acquisition-timeout-secs` defaults to 30 seconds.

Checkpoint publication uses a sibling staging directory and validates the
database before moving it to the requested path. An existing database is not
replaced. Build a new checkpoint at another path after a schema change or when
the existing database is invalid.

## What it checks

For each imported Tempo block, the checker fetches the complete ordered
transaction envelopes and receipt set. It reconstructs both roots, checks
receipt metadata and the aggregate bloom, and decodes protocol calldata and
events. It compares the kernel result with receipt events, Zone state, token
supply, and Portal collateral.

The kernel covers Portal creation, token enablement, deposits, withdrawals,
batches, bounce-backs, refunds, callbacks, commitments, ownership, and token
accounting. It checks collateral after the Tempo transition and before the Zone
transition. It checks Zone commitments and token supply afterward.

## Durability and runtime behavior

The dedicated database has four tables:

| Table | Purpose |
|---|---|
| `Meta` | Identity, schema, tips, coverage, and active finding |
| `Checkpoints` | Bootstrap state and later state cuts |
| `Journal` | Ordered canonical per-block deltas and continuity data |
| `Findings` | Findings and their chain coordinates |

The bootstrap checkpoint is immutable. The canonical journal is retained
without pruning.

The runtime keeps one current notification and a bounded queue. It commits each
block separately and sends Reth `FinishedHeight` only after all state, finding,
and coverage updates through that height have committed.

Verified, acknowledged, and unchecked progress are stored separately. Before
acknowledging an unchecked suffix, the checker records it as a coverage gap. A
finding leaves semantic state at the verified parent. Restart reconstructs
state from a checkpoint and journal. A reorg restores the common ancestor and
then applies the replacement branch. Removing the finding's block clears the
active finding atomically; retaining it keeps the finding active.

Malformed or discontinuous `FinishedHeight` updates are not treated as
verified. The checker records the unchecked range instead.

## Failure policy

- **Immediate terminal:** invalid local identity, schema, or notification data.
- **Bounded retry:** unavailable notification/provider data with a finite
  retry budget.
- **Transient retry:** interrupted stream or data acquisition.
- **Authenticated divergence:** record a finding before acknowledging the
  block; do not check descendants.

If retries are exhausted, the checker commits the unchecked suffix before
acknowledging it and stopping. Acknowledged blocks in that suffix are not
verified.

## Trust assumptions and non-claims

The checker verifies imported transaction and receipt commitments. It relies on
the in-process Reth node for the canonical Zone chain and hash-pinned Zone state
reads. It does not verify those reads against a state trie.

The Tempo archive endpoint supplies historical envelopes, receipts, and
hash-pinned Portal balance reads. The checker does not verify balance reads
against a storage trie. Missing history is an acquisition failure, not a zero
value or a successful check.

The checker does not validate encrypted payload cryptography, arbitrary EVM or
callback behavior, private mint recipients, or a fallback recipient that is
not present in the observed data.
