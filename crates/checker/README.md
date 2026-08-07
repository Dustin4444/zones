# zone-checker

A durable, observe-only execution extension that independently reconstructs
Portal and Zone state and records authenticated divergences.

The approved architecture and release gate are in [`DESIGN.md`](DESIGN.md).
The checker-owned protocol vectors frozen by Goal 0 are inventoried in
[`MODEL_VECTORS.md`](MODEL_VECTORS.md).

## Current status

Goals 0 through 9 are implemented. Goals 0 through 5 define the authenticated
observation boundary, pure lifecycle model, typed output checks, exact Zone
state reads, and Portal collateral checks. Goals 6 through 8 add the dedicated
checker database, commit-before-acknowledge processing, crash recovery, exact
unwind, durable findings, and alert mode. Goal 9 adds authenticated archive
bootstrap, resumable L1 and Zone replay, normal restart, and fresh-path rebuild.

The checker is alerting, not enforcing. `--checker.mode` remains `off` by
default, and `observe` never changes consensus or blocks core-node progress
after an authenticated finding. Goal 10's closed-system shadow-release and
performance acceptance gate is not complete.

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
   tempo_zone_checker_runtime_healthy 1
   tempo_zone_checker_runtime_active_alert 0
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

## Validation

```sh
cargo test -p zone-checker
cargo clippy -p zone-checker --all-targets -- -D warnings
cargo fmt --check
```
