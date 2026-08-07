# Zone checker design

## Purpose and boundary

The checker is a second, independently implemented specification of Zone
bridge semantics. It observes canonical data and reports divergence; it does
not execute production precompiles, replay arbitrary EVM behavior, or enforce
consensus.

```text
canonical Zone block and receipts
             │ advanceTempo authenticates imported header
             ▼
authenticated Zone and Tempo observation
  full envelopes ─ transactions_root
  full receipts  ─ receipts_root and bloom
             │ facts and implementation outcomes
             ▼
zone-checker-kernel pure transition
             │ expected effects and sorted StateDelta
             ▼
state/supply/collateral comparisons
             │
             ▼
checkpoint + journal + finding transaction
             │ commit before acknowledgement
             ▼
Reth FinishedHeight
```

Observation and expectation have deliberately separate construction paths.
ABI bindings decode authenticated wire data; they are not semantic model
helpers. The kernel depends on generic primitives and checker-owned protocol
types, not production Inbox, Outbox, Portal, sequencer, or payload transition
code.

## Authenticated observation

The first Zone system transaction supplies canonical `advanceTempo` calldata
and the imported Tempo header. The optional final Zone system transaction
supplies finalization inputs. Their caller, destination, location, success,
canonical ABI, bounded dynamic values, and canonical header RLP are checked.
Protocol-emitter logs are classified by address and topic. A malformed known
event or unknown protocol topic fails closed.

For the exact imported Tempo hash, the adapter fetches every full transaction
envelope and receipt. It locally hashes the envelopes, reconstructs the
transaction trie root, and compares it with the imported header. Receipt count,
ordering, block/index/hash metadata, trie root, and aggregate bloom must match;
receipt transaction hashes come from the local envelope order. Direct
`submitBatch` and non-empty `processWithdrawals` inputs are decoded from those
root-authenticated envelopes. There is no selective unauthenticated by-hash
body path.

An unavailable provider response is not evidence. Acquisition errors retry or
produce durable coverage gaps according to runtime policy; they never become a
default observation. Authenticated, well-formed semantic contradictions become
findings.

## Pure semantic kernel

`zone-checker-kernel` represents all authoritative semantic state with one
validated `StateKey` / `StateValue` vocabulary. A generic overlay reads a
parent `State`, stages typed changes, validates key/value families, and emits a
sorted `StateDelta`. This representation is shared by transitions,
persistence, checkpoints, journal replay, and reorg reconstruction; there are
no persistence-specific model mirrors.

Transitions cover:

- Portal creation, identity, configuration, and token enablement;
- ordered ordinary and callback/bounce-back deposits;
- empty, partial, and complete contiguous Zone deposit processing;
- failed-deposit withdrawal and refund ownership;
- user withdrawals, active fees, IDs, nonces, sender tags, and burns;
- finalization, withdrawal queue folds, and empty batches;
- batch submission, ring ownership, and queue slots;
- empty, partial, and complete Portal processing;
- successful delivery, pending Portal refunds, and aggregate claims;
- withdrawal bounce-back, Zone mint or Inbox refund, and aggregate claims;
- complete one-owner lifecycle closure and per-token `S/D/W` accounting.

Each operation derives expected identities, fees, commitments, effects, and
accounting independently, then compares them with observed outcomes. A valid
transition cannot silently drop an owner or value. Arithmetic is checked.

The imported Tempo transition is applied first. Portal collateral is compared
against `S + D + W` at that post-Tempo/pre-Zone cut. The Zone transition then
applies, after which fixed commitments and exact token supply are compared.

## Persistent representation

The checker owns a dedicated MDBX environment with exactly four tables:

| Table | Content |
|---|---|
| `Meta` | Version, identity, tips, coverage, and active finding latch |
| `Checkpoints` | Bounded complete state snapshots |
| `Journal` | Canonical block identity, parent continuity, sorted delta, coverage |
| `Findings` | Bounded findings and canonical/orphan lineage |

Keys and values use exact, bounded, versioned codecs. Unknown versions/tags,
trailing data, missing rows, duplicate positions, conflicting entries, and
invalid key/value families are corruption. An incompatible schema is opened
read-only only as needed to identify the mismatch, then routed to a fresh
checkpoint build; it is never modified as if compatible.

The identity-bound bootstrap checkpoint is immutable. Checkpoint publication
is staged and reopened for validation before becoming authoritative. The
canonical journal is intentionally unpruned because no accepted finality or
reorg-horizon contract exists.

Every block apply transaction records continuity, semantic delta, tips,
coverage, and any finding atomically. A checkpoint transaction writes a
complete validated cut without exposing a partial snapshot. Restart loads a
checkpoint and replays its exact journal suffix. Fault tests require an
interrupted write to expose either the old state or the complete new state,
never a mixture.

## Canonical reorgs and findings

A journal entry names the exact canonical hash, parent, prior imported cut, and
state delta needed to reconstruct state. Reorg handling finds the common
ancestor, reconstructs that exact cut from a checkpoint plus journal, truncates
the old canonical suffix atomically, and applies replacement blocks in order.
Reorgs before, after, and across checkpoints use the same representation.

A deterministic semantic divergence commits a compact finding and the active
alert latch without committing its candidate semantic delta. The latch names
the exact finding lineage and verified parent. Descendants are persisted as
`NotCheckedAncestorDivergence`; they are never passing blocks. If a reorg
retains the alerting block, the latch remains. If it removes the block, the
finding is orphaned and the latch is cleared in the same canonical update.

## Runtime state machine

The runtime has one current notification and one bounded FIFO. It does not
spawn overlapping semantic writers. Its externally meaningful states are:

- `Starting`: validating identity/checkpoint and acquiring catch-up work;
- `Healthy`: following the canonical stream with no uncovered suffix;
- `Retrying`: retaining work while bounded acquisition attempts run;
- `Alerting`: sticky authenticated finding; descendants are not checked;
- `Disabled`: terminal checker failure with truthful durable gaps.

Notifications containing several blocks are applied one block at a time, each
with its own commit. The runtime acknowledges a height only after all durable
effects or gaps through that height commit. A crash between commit and
acknowledgement replays idempotently. A crash before commit leaves the parent
authoritative.

Retry policy distinguishes immediate terminal, bounded retry, transient retry,
and authenticated divergence. Once bounded fail-open behavior is exhausted,
the exact unverified suffix is committed as a gap before acknowledging beyond
it. Verified, acknowledged, and gap ranges therefore remain distinguishable
after restart.

Stream loss triggers canonical catch-up reconstruction rather than trusting
the next fragment. A malformed or discontinuous `FinishedHeight` signal is
handled conservatively: absent proof of Reth jump semantics, the checker does
not silently jump and call the intervening range verified.

## Deterministic checkpoint builder

The builder uses the same authenticated adapters and pure kernel transitions as
the runtime. It validates local Zone genesis, configured chain IDs, Portal and
factory creation evidence, ancestry, initial token, zero genesis supply, and
the explicit genesis token handoff. Reorgs observed while building cause
canonical reconstruction, not publication of a mixed-fork checkpoint.

Checkpoint construction is local because its identity and trust sources are
local. Importing a third-party state snapshot without independently replaying
its evidence would weaken the checker boundary and is unsupported by default.

## Trust and coverage contract

The checker proves transaction and receipt commitment membership for imported
Tempo evidence. It does not prove independent Tempo finality or availability.
The configured archive endpoint is trusted for availability and hash-pinned
Portal balance reads; those reads do not include checker-verified state proofs.

The in-process node is trusted for the canonical Zone chain and exact-hash
state reads. Fixed commitment and supply reads are pinned to a block hash but
are not independently proven against a state trie by the checker. Missing
historical data is a visible acquisition failure.

Cryptographic validity of encrypted payloads/proofs, arbitrary EVM and callback
behavior, private recipients, and unavailable withdrawal-time fallback
recipients are outside the semantic claim. The checker does verify the exposed
identity, ordering, ownership, queue, accounting, supply, and collateral
consequences of every authenticated branch.

Coverage is truthful by construction:

- **verified** means authenticated observation, kernel transition, all required
  comparisons, and durable commit succeeded;
- **acknowledged with gap** means runtime progress continued after durably
  recording the exact unchecked range;
- **ancestor divergence** means checking stopped at a durable canonical
  finding and descendants were not evaluated.

## Operational limitations

There is no journal pruning policy yet; disk use grows with canonical history.
Rebuild requires the local Zone history and configured Tempo archive evidence
needed by the builder. Schema changes require a fresh database path. There is
no offline semantic-key diagnostic command or metrics/performance contract in
the cut-over implementation; findings contain compact coordinates for archive
investigation. Current real-node evidence is a functional checkpoint,
processing, persistence, and restart test, not a production performance claim.
