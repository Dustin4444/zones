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
             │ commit before acknowledgement
             ▼
Reth FinishedHeight
```

Observation and expectation use separate construction paths. ABI bindings
decode wire data. The kernel uses checker-owned types rather than production
Inbox, Outbox, Portal, sequencer, or payload transition code.

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

Unavailable data is retried and may become a coverage gap. It is never replaced
with a default value. A contradiction in authenticated data becomes a finding.

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
the target path. The canonical journal is not pruned.

Each update commits its metadata, journal, checkpoint, finding, and reorg
changes in one MDBX transaction. Restart loads a checkpoint, replays its journal
suffix, and validates the complete reconstructed state.

## Canonical reorgs and findings

A journal entry stores the block, parent, imported Tempo cut, and state delta.
On a reorg, the checker reconstructs the common ancestor from a checkpoint and
journal, removes the old suffix in the same transaction, and applies the
replacement branch in order.

A semantic mismatch atomically records a finding without applying the expected
state delta. Descendants are recorded as `NotCheckedAncestorDivergence`, not as
verified blocks. A reorg that retains the finding's block keeps it active. A
reorg that removes the block clears the active finding in the same transaction;
the finding row remains as an audit record.

## Runtime state machine

The runtime has one current notification and one bounded queue:

- `Starting`: validate state and acquire catch-up work;
- `Healthy`: follow the canonical stream;
- `Retrying`: retain work while retrying acquisition;
- `Alerting`: retain a finding and skip descendants;
- `Disabled`: stop checking after a terminal failure.

Notifications containing several blocks are applied one block at a time, each
with its own commit. The runtime acknowledges a height only after state or a
coverage gap through that height commits. A crash after commit may resend the
acknowledgement. A crash before commit leaves the parent state unchanged.

Retry policy distinguishes immediate terminal, bounded retry, transient retry,
and authenticated divergence. When retries are exhausted, the unchecked suffix
is committed as a gap before acknowledgement. Verified and acknowledged tips
remain separate.

Stream loss triggers canonical catch-up reconstruction rather than trusting
the next fragment. A malformed or discontinuous `FinishedHeight` update records
an unchecked range rather than advancing the verified tip.

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
- **acknowledged with gap:** the unchecked range was recorded before
  acknowledgement;
- **ancestor divergence:** a finding stopped checks of descendant blocks.

## Operational limitations

There is no journal pruning policy; disk use grows with canonical history.
Rebuild requires local Zone history and the Tempo archive evidence used by the
builder. Schema changes require a new database path.
