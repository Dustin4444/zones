# Zone checker design

The checker observes Zone and Tempo data, evaluates checker-owned bridge
transitions, compares the result with observed output, and stores its progress.
It does not call production bridge transitions or affect consensus.

## Data flow

```text
Zone block, receipts, and state
             │ advanceTempo names imported Tempo headers
             ▼
authenticated observation
  full envelopes ─ transactions_root
  full receipts  ─ receipts_root and bloom
             │ protocol facts and observed output
             ▼
kernel transition
             │ expected effects, state, and accounting
             ▼
state/supply/collateral comparisons
             │
             ▼
metadata + journal + checkpoint + finding transaction
             │ commit before watermark advance
             ▼
Reth FinishedHeight
```

Observation and expectation use separate construction paths. ABI bindings
decode wire data. The kernel uses checker-owned types rather than production
Inbox, Outbox, Portal, sequencer, or payload transition code.

## Module boundaries

The checker is organized around the authenticated-data flow rather than the
source chain alone:

- `observe` reads and strictly decodes Zone and Tempo data. Its `l1` and `l2`
  modules own chain-specific acquisition and observation; `abi` and `events`
  own shared wire decoding and event classification.
- `adapter` compares the authenticated observations across the two chains and
  constructs `AuthenticatedBlock` inputs. `adapter/tempo` covers imported
  Tempo operations; `adapter/zone` covers Zone outputs.
- `kernel` contains the checker-owned state model, deterministic transitions,
  derived effects, and internal consistency invariants. It has no provider or
  ExEx dependency.
- `runtime` turns the locally retained canonical history into durable,
  sequentially verified progress.
- `persistence` owns the MDBX schema, codecs, durable model, and atomic
  checkpoint, journal, finding, and reorg updates.
- `bootstrap` authenticates local Zone genesis and historical Tempo imports to
  construct the initial checkpoint.
- `exex` is the integration boundary: notifications wake recovery, canonical
  replay data comes from the local node, and Reth's watermark follows verified
  progress.

`failure` and `inspection` provide the shared failure policy and public
read-only inspection API.

## Authenticated observation

The first Zone system transaction supplies `advanceTempo` calldata and the
imported Tempo headers. An optional final system transaction supplies
finalization inputs. The checker validates transaction position, caller,
destination, success, ABI encoding, value bounds, and header RLP. It classifies
protocol logs by emitter and topic and rejects malformed or unknown protocol
events.

For each imported Tempo block, the checker fetches the complete ordered
transaction envelopes and receipt set. It reconstructs both roots and checks
them against the imported header. It also checks receipt count, order, block
and transaction coordinates, and the aggregate bloom. It decodes `submitBatch`
and non-empty `processWithdrawals` calls from those transactions.

Unavailable data pauses recovery and is retried. It is never replaced with a
default value. A contradiction in authenticated data becomes a finding.

## Kernel transitions

The checker's internal `kernel` module stores semantic state as validated
`StateKey` and `StateValue` rows. An overlay reads a parent state, stages
changes, validates key/value families, and emits a sorted `StateDelta`.
Transitions, checkpoints, journal replay, and reorg reconstruction use the same
representation.

Transitions cover:

- Portal creation, configuration, and token enablement;
- ordinary, callback, and bounce-back deposits;
- partial and complete deposit processing;
- failed deposits and refunds;
- withdrawals, fees, sender tags, and burns;
- finalization, batches, queue commitments, and ring ownership;
- delivery, Portal refunds, Inbox refunds, and claims;
- ownership and per-token `S/D/W` accounting.

Each operation derives expected identities, fees, commitments, effects, and
accounting independently from observed outcomes. Arithmetic is checked.

The imported Tempo transition is applied first. Portal collateral is compared
against `S + D + W` at that post-Tempo/pre-Zone cut. The Zone transition then
applies, after which fixed commitments and exact token supply are compared.

## Persistent representation

The checker uses a dedicated MDBX environment with four tables:

| Table | Content |
|---|---|
| `Meta` | Version, identity, tips, coverage, and active finding latch |
| `Checkpoints` | State snapshots |
| `Journal` | Block identity, parent continuity, and sorted state delta |
| `Findings` | Findings and chain coordinates |

Keys and values use bounded, versioned codecs. Unknown versions or tags,
trailing data, missing rows, conflicting entries, and invalid key/value
families invalidate the database. Schema changes require a new checkpoint.

The identity-bound bootstrap checkpoint is immutable. Checkpoint publication
uses a sibling staging directory and reopens the database before moving it to
the target path. The checker retains bootstrap, recovery, and active
checkpoints, plus at least 16,384 Zone blocks of journal history after the
recovery checkpoint. Older journal rows are pruned atomically when recovery
advances; normal checkpoint cadence retains at most one additional interval.

Each update commits its metadata, journal, checkpoint, finding, and reorg
changes in one MDBX transaction. Restart validates retained journal continuity,
then loads the active checkpoint and replays only its journal suffix.

## Canonical reorgs and findings

A journal entry stores the block, parent, imported Tempo cut, and state delta.
On a reorg within the retained history, the checker reconstructs the common
ancestor from the recovery or active checkpoint and journal, removes the old
suffix in the same transaction, and applies the replacement branch in order.
A reorg before the recovery checkpoint durably blocks checking without
advancing verified progress; it requires a rebuilt checker database.

A semantic mismatch atomically records a finding and the observed unchecked
suffix without applying the expected state delta. A reorg that retains the
finding's block keeps it active. A reorg that removes the block clears the
active finding in the same transaction; the finding row remains as an audit
record.

## Runtime recovery

The runtime recovers directly from local canonical history:

- `Complete` coverage means the verified and observed tips agree;
- `Recovering` coverage means retained canonical history remains to be checked;
- `Gap` coverage is paired with an active authenticated divergence;
- a blocked reason means durable verification cannot resume automatically.

Notifications only update the observed canonical head. The runtime acquires and
commits every missing block one at a time. A crash leaves the verified checkpoint
unchanged and recovery resumes from it.

Temporary acquisition failures retry with bounded backoff. Verified and observed
tips remain separate, so no unchecked suffix is recorded merely because Tempo is
temporarily unavailable.

Stream loss is harmless while local history remains available: the next wakeup,
or a restart, resumes direct canonical recovery. A malformed notification blocks
the checker rather than advancing verified progress.

## Checkpoint builder

The builder uses the same observation adapters and kernel transitions as the
runtime. It validates the local Zone genesis, chain IDs, Portal creation,
Tempo ancestry, initial token, zero genesis supply, and the genesis token
handoff.

Checkpoint construction uses the local Zone database and configured Tempo
endpoint. The checker does not import third-party state snapshots.

## Trust and coverage contract

The checker verifies imported transaction and receipt roots. It does not verify
Tempo finality or availability. The Tempo archive endpoint supplies hash-pinned
Portal balance reads, which are not checked against a storage trie.

The in-process node supplies the canonical Zone chain and hash-pinned state
reads. The checker does not verify those reads against a state trie. Missing
history is an acquisition failure.

The checker does not validate encrypted payload cryptography, arbitrary EVM or
callback behavior, private recipients, or a fallback recipient absent from the
observed data.

Coverage has three relevant states:

- **verified:** observation, transition, comparisons, and commit succeeded;
- **recovering:** the local canonical head is ahead of verified progress;
- **ancestor divergence:** a finding stopped checks of descendant blocks.

## Operational limitations

Journal retention bounds live checker state, but historical findings are kept
as audit records and may grow indefinitely. Rebuild requires local Zone history
and the Tempo archive evidence used by the builder. Schema changes require a
new database path.
