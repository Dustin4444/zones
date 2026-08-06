# zone-checker

A checker crate containing the live observe-only execution extension and the
non-wired in-memory evaluator used to validate the complete Goal 5 pipeline.

The approved architecture and release gate are in [`DESIGN.md`](DESIGN.md).
The checker-owned protocol vectors frozen by Goal 0 are inventoried in
[`MODEL_VECTORS.md`](MODEL_VECTORS.md).

## Current status

Goals 0 through 5 are implemented:

- Goal 0 defines checker-owned protocol constants, encodings, event types, and
  lifecycle vocabulary.
- Goal 1 establishes the ephemeral authenticated-observation boundary.
- Goals 2 through 4 implement the complete pure Portal, Zone-deposit,
  withdrawal, batch, processing, bounce-back, and refund lifecycle model.
- Goal 5 projects authenticated observations into that model, compares typed
  implementation outputs, reads exact post-Zone commitments and supply, checks
  post-L1/pre-Zone collateral, and commits passing candidates in memory.

This is still not deployable protocol coverage. The Goal 5 evaluator is an
explicit `InMemoryChecker`; it is deliberately not wired into the ExEx until
durable state and exact unwind exist. There is no persistence, restart/reorg
recovery, finding storage, or enforcement. The live Goal 1 diagnostic path
continues to acquire the six fixed commitments at the exact Zone block hash,
but has no authoritative model-owned token set and therefore requests no
supply slots. The in-memory Goal 5 path reads every enabled token supply.

`--checker.mode` remains `off` by default. `observe` is a diagnostic development
mode, not a shadow-release recommendation.

## Authenticated observation boundary

The observer keeps authenticated inputs separate from authenticated
implementation outcomes. Observations live only while one canonical block is
processed. Raw data is decoded into typed in-memory observations, but those
observations are never persisted.

| Data | Authentication or trust source | Missing or inconsistent data |
|---|---|---|
| Imported Tempo header, deposits, decryptions, enabled tokens | Canonical `advanceTempo` calldata in the first Zone system transaction; exact ABI and header-RLP round trips | Malformed authenticated data |
| Optional finalization count, block number, encrypted senders | Canonical `finalizeWithdrawalBatch` calldata in the unique final Zone system transaction | Invalid envelope or malformed authenticated data |
| Zone protocol outcomes and containing transaction hashes | Ordered successful notification-local receipts paired with the canonical recovered block | Missing or internally inconsistent notification block/receipt data is an acquisition failure; protocol events fail closed |
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

The explicit Goal 5 evaluator then compares `TempoAdvanced`,
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
length, Zone block number, envelope position, and successful receipt. The Goal
5 projection/model layer checks event grammar, pending-count, reveal-mode,
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
receipt-to-transaction metadata after receipt-root authentication. Goal 1 does
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
| `observe` | Authenticate and log ephemeral Goal 1 observations, including exact fixed-state acquisition. Do not run the Goal 5 model, persist, enforce, or claim complete coverage. |

Committed and reorged-in blocks are observed oldest-to-newest. Reverted and
reorged-out blocks are logged newest-to-oldest but are not re-observed after
they leave the canonical chain. A failed observation does not terminate the
Zone node; it permanently holds the ExEx pruning acknowledgement behind the
gap for the remainder of that process. Generic retry and durable recovery are
later goals.

## Validation

```sh
cargo test -p zone-checker
cargo clippy -p zone-checker --all-targets -- -D warnings
cargo fmt --check
```
