# zone-checker

An observe-only execution extension for authenticating the Zone and Tempo data
that the checker model will consume.

The approved architecture and release gate are in [`DESIGN.md`](DESIGN.md).
The checker-owned protocol vectors frozen by Goal 0 are inventoried in
[`MODEL_VECTORS.md`](MODEL_VECTORS.md).

## Current status

Goals 0 and 1 are implemented:

- Goal 0 defines checker-owned protocol constants, encodings, event types, and
  lifecycle vocabulary.
- Goal 1 establishes the ephemeral authenticated-observation boundary.

This is not deployable protocol coverage. There is no mutable model,
persistence, restart/reorg recovery, lifecycle comparison, finding storage, or
enforcement. In particular, the Goal 1 exact-state API requires an explicit
checker-owned enabled-token set, but runtime orchestration has no model from
which to obtain that set yet. It therefore observes the six fixed commitments
with an empty supply set. Goal 5 connects the complete model-owned token set and
per-token supply comparisons.

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
| Zone protocol outcomes and containing transaction hashes | Ordered successful notification-local receipts paired with the canonical recovered block | Missing/inconsistent notification receipt sets are acquisition failures; per-block cardinality is an invalid envelope; protocol events fail closed |
| Six fixed Zone commitments and selected token supplies | In-process `state_by_block_hash` at the exact canonical Zone hash | Retryable acquisition failure; an unwritten slot of an existing account is canonical EVM zero |
| Ordered Tempo protocol outcomes | Complete receipt set authenticated against the receipt root and logs bloom in the imported header | Retryable acquisition failure or fail-closed protocol-event error |
| Direct `submitBatch` and non-empty `processWithdrawals` inputs | Selectively fetched transaction body, bound by hash/block/index metadata, with exactly one top-level call to the configured Portal | Retryable acquisition failure, malformed calldata, or `UnsupportedNestedPortalCall` |

Implementation events never choose the inputs they confirm. In particular,
`TempoAdvanced` is compared with the imported header derived from
`advanceTempo`; it is not an L1 block anchor.

### Zone block checks

For each non-genesis canonical Zone block, the observer:

1. Requires equal transaction, recovered-sender, and receipt cardinalities.
2. Requires the first transaction to be the successful system call to the
   Inbox and canonically decodes `advanceTempo`.
3. Allows at most one later system transaction, requires it to be the
   successful final transaction to the Outbox, and canonically decodes
   `finalizeWithdrawalBatch`.
4. Retains supported protocol logs in transaction/receipt order with the
   containing transaction hash.
5. Compares input-confirming fields in `TempoAdvanced`, `TempoBlockFinalized`,
   `TokenEnabled`, and optional `BatchFinalized` with authenticated calldata.
   It retains queue/cursor events and exact post-state independently; Goal 5
   supplies the model-owned expectations used to compare them.

Dynamic ABI counts, offsets, and byte lengths are checked before generated
decoders allocate. Calldata and Tempo-header RLP must re-encode byte-for-byte,
with no trailing data.

Goal 1 checks only the finalization relationships defined without mutable model
state: canonical shape, count versus sender-array length, sender length, Zone
block number, envelope position, successful receipt, event presence, and
containing transaction. Pending-count and reveal-mode relationships are model
rules introduced by later goals and are not guessed here.

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
- Invalid envelopes cover caller, destination, position, success, cardinality,
  and finalization block-number rules.
- Malformed authenticated data covers non-canonical or structurally unsafe ABI
  and RLP.
- Protocol-event errors fail closed for malformed known events, unknown topics
  from protocol emitters, and explicitly unsupported native events such as
  `DepositRejected`; they retain chain, transaction hash/index, and receipt and
  block log coordinates.
- Portal-call reconciliation errors cover nested or ambiguous calls,
  conflicting event-implied call families, calldata/event family mismatches,
  and eventful empty `processWithdrawals` bodies.
- Output mismatches report implementation outputs that disagree with an
  authenticated input or required envelope relationship. Goal 1 never treats
  one implementation output as the independent expectation for another.

## Runtime behavior

| Mode | Behavior |
|---|---|
| `off` | Default. The checker ExEx is not installed. |
| `observe` | Authenticate and log ephemeral Goal 1 observations. Do not persist, enforce, or claim complete coverage. |

Committed and reorged-in blocks are observed oldest-to-newest. Reverted and
reorged-out blocks are logged newest-to-oldest but are not re-observed after
they leave the canonical chain. A failed observation does not terminate the
Zone node; it permanently holds the ExEx pruning acknowledgement behind the
gap for the remainder of that process. Generic retry and durable recovery are
later goals.

## Validation

```sh
cargo test -p zone-checker
cargo fmt --check
```
