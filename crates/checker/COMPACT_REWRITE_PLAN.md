# Compact Zone checker rewrite plan

Status: implementation handoff

Target agent: Amp `medium`

Reviewed branch: `horsefacts/zone-checker`

Reviewed HEAD: `d9002b4bee897698b2252b45bdedc90d41d4ba3c`

Merge base: `55b0fcbed520570c5e1089dc5db97d05b5546571`

## Implementation progress

| Milestone | Status | Local commit | Validation and measurements | Remaining caveats |
|---|---|---|---|---|
| 0 — baseline and characterization | passed | `746e6ad4` | Branch `horsefacts/zone-checker`; starting HEAD `fa525d28cb38246cf53a635012fbcf90a4f9652c`; merge base confirmed; clean worktree; baseline LOC and guarantee inventories recorded below; `cargo +1.95.0 test -p zone-checker` (432 unit tests and 1 doc test passed) | The default Rust 1.94.1 toolchain is older than the workspace's required 1.95.0, so gates use the already-installed `+1.95.0` toolchain. |
| 1 — full L1 transaction authentication | passed | `18535206` | Full envelopes fetched in one exact-block response; local envelope hashes and `transactions_root` checked before receipt binding and Portal decode; selective by-hash fetch deleted. `cargo +1.95.0 fmt --check`; `cargo +1.95.0 test -p zone-checker` (433 unit tests and 1 doc test); `cargo +1.95.0 clippy -p zone-checker --all-targets --all-features -- -D warnings`. Checker production/test LOC: 34,179/28,193. | One metrics test was flaky under a concurrent gate run; its isolated rerun and the subsequent full serial gate passed. |
| 2 — compact kernel skeleton | passed | `f2d91909` | Independent four-dependency kernel; one typed row space, read-through overlay, sorted delta, compact facts/effects/findings, creation/config/token/ordinary-deposit transitions, literal commitment vector, and legacy semantic snapshot parity. `cargo +1.95.0 fmt --check`; `cargo +1.95.0 test -p zone-checker-kernel` (12 tests); `RUST_TEST_THREADS=1 cargo +1.95.0 test -p zone-checker` (434 unit tests and 1 doc test); `cargo +1.95.0 clippy -p zone-checker-kernel -p zone-checker --all-targets --all-features -- -D warnings`. Kernel production/test LOC: 1,018/393; migration-wide production/test LOC: 35,197/28,821. The skeleton's density supports the planned 6–7.5k complete-kernel range. | Legacy remains authoritative outside creation, configuration, token enablement, and ordinary-deposit append/processing. The pre-existing metrics recorder test requires serial execution because its derived metric handles are process-global. |
| 3 — complete lifecycle kernel | passed | this milestone commit; resolved in the next ledger update | Complete release-one ownership and lifecycle, exact native effect grammar, imported-context ownership, owner-graph/ring/S-D-W invariants, all six terminal differential traces, partial processing, empty batches, aggregate claims, and field mutations. Oracle blockers were applied. `cargo +1.95.0 fmt --check`; `cargo +1.95.0 test -p zone-checker-kernel` (20 tests); `RUST_TEST_THREADS=1 cargo +1.95.0 test -p zone-checker` (439 unit tests and 1 doc test); focused differential suite (6 tests); `cargo +1.95.0 clippy -p zone-checker-kernel -p zone-checker --all-targets --all-features -- -D warnings`. Kernel production/test LOC: 2,610/1,000. | Legacy remains the differential oracle through Milestone 5. Persistence-load validation and production observation reconciliation are Milestones 4–5 responsibilities. |
| 4 — checkpoint/journal store | active | pending | Four-table schema, bounded codecs, checkpoint/journal/reorg/findings implementation next | — |
| 5 — builder and compact runtime | pending | pending | — | — |
| 6 — cutover and deletion | pending | pending | — | — |

### Milestone 0 baseline record

LOC uses physical Rust lines under `crates/checker/src`; files below a `tests/` directory or named
`tests.rs` are counted as tests. The same script and classification will be used at cutover.

| Subsystem | Production LOC | Test LOC | Total LOC |
|---|---:|---:|---:|
| `check` | 1,936 | 2,490 | 4,426 |
| `model` | 13,418 | 9,392 | 22,810 |
| `observe` | 2,736 | 2,224 | 4,960 |
| `runtime` | 3,514 | 2,383 | 5,897 |
| `store` | 11,522 | 8,616 | 20,138 |
| roots and cross-cutting | 1,031 | 3,031 | 4,062 |
| **Total** | **34,157** | **28,136** | **62,293** |

Authenticated-field classification at baseline:

- **Consumed:** exact imported and Zone headers and chain coordinates; complete receipt identity,
  order, roots, and blooms; system-envelope identity and calldata; Portal and Zone lifecycle
  inputs; operation order and branch outcomes; exact-hash state and collateral reads.
- **Compared:** deposit numbers and queue commitments; batch, withdrawal, refund, processing,
  finalization, token-enable, and Tempo-advance outputs; exact fixed Zone state; enabled-token
  supplies; Portal collateral lower bound.
- **Explicitly unchecked:** successful batch proof/quorum payloads after authenticated execution;
  event coordinates as semantic values; ordinary `DepositProcessed.to` and `memo`; bounce-back
  `zone_fallback_recipient`; finalization encrypted-sender contents after grammar/count checks;
  non-enabled token supplies; surplus collateral; upper bytes of the packed Outbox batch slot.
- **Baseline classification gap to close:** `advanceTempo.decryptions` is authenticated and bounded
  but neither semantically consumed nor explicitly documented as unchecked. Compared-field
  mutation coverage is complete only for fixed state, supply, collateral, one imported queue hash,
  and one finalized hash; Milestone 3 must add the remaining one-field mutations.
- **Milestone 1 evidence hole:** the RPC transaction-hash vector binds receipts, but full envelopes
  are not authenticated against `transactions_root`; selected Portal bodies are fetched by hash.

Existing guarantee coverage at baseline:

| Guarantee family | Existing focused coverage | Known weak point carried forward |
|---|---|---|
| Complete lifecycle | Creation/config/token, ordinary and failed deposits, refunds, user withdrawals and fees, finalization, submission, delivery, pending refund, bounce-back, callback, partial/empty processing, ring reuse, aggregate accounting and all terminal owners in `model/transition/tests`, `check/tests/lifecycle`, and `tests/lifecycle_recovery.rs` | Ring reuse and empty-batch persistence/reorg are not isolated. |
| Authentication | Exact L1 header; receipt cardinality/coordinates/root/bloom; Portal topic and ABI policy; Zone system envelopes and receipt commitments in `observe/l1/tests.rs` and `observe/l2/tests.rs` | No complete L1 transaction-root/body proof; no authentication-failure/restart/retry cross-test. |
| Persistence/restart | Store opening, schema/codec corruption, history, lifecycle recovery, durable L1 cursor resume, and bootstrap restart tests | Accounting persistence is mostly whole-state rather than focused boundary tests. |
| Reorg and alert | One/multi/interrupted reorg; descendant-preserving, exact-finding-removing, and replacement-divergence alert reorgs in `tests/runtime` | Alert restart followed by alert-removing reorg is not one combined scenario. |
| Coverage and acknowledgement | Partial-notification durable-prefix retry, acquisition atomicity, prepared-candidate non-authority, commit-abort retry, commit-before-mirror restart, and no-ack acquisition failure in `tests/runtime/atomicity.rs` | Current architecture does not yet persist the compact runtime's explicit verified/acknowledged gap model. |
| Real node | Bootstrap/restart/advance and active-alert progress in `crates/node/tests/it/checker_e2e.rs` | No real-node lifecycle/reorg/crash breadth beyond those smoke tests. |

### Milestone 3 authenticated-field disposition

- **Consumed by compact semantics:** the authenticated imported block hash, number, base fee, and
  ordered Portal calls; complete creation/configuration/token/deposit/submission/processing/refund
  call fields; ordered Zone token enables, deposit preimages and dispositions, configuration and
  withdrawal operations, finalization block/count/sender vector, Zone block hash and number; exact
  state/supply/collateral values. Imported context is carried once from the authenticated boundary,
  never copied from Zone observations.
- **Compared independently:** every native event field represented by `ExpectedEffect`; imported and
  Zone fixed-state commitments; supply and collateral; complete queue preimages, suffixes, indices,
  counters, batch boundaries, fees, sender tags, refund aggregates, owner IDs, and S/D/W values.
  One-field mutation suites cover transaction/receipt/header identity and roots, deposit and
  withdrawal preimages, submission commitments, finalization structure, event grammar, exact state,
  supply, and collateral. Differential traces compare all six lifecycle terminals plus partial and
  empty batches.
- **Explicitly unchecked:** successful proof/quorum payloads; event coordinates except as finding
  locations; ordinary `DepositProcessed.to` and `memo`; bounce-back `zone_fallback_recipient`;
  opaque finalization encrypted-sender bytes after authenticated ordering/shape checks;
  `advanceTempo.decryptions`; non-enabled token supplies; surplus collateral; and upper bytes of the
  packed Outbox batch slot. These values must remain classified as unchecked in the production
  adapter and may not be reported as verified.

## 1. Executive direction

The checker is overengineered, but not primarily because its bridge lifecycle model is too
detailed. The excess is concentrated in persistence representations, durable finding mirrors,
historical diagnostics, adapter and DTO layers, runtime state machinery, and tests for those
abstractions.

The reviewed branch adds 66,279 lines across 250 files. The checker contains approximately:

| Area | Production Rust LOC |
|---|---:|
| `check` | 1,936 |
| `model` | 13,418 |
| `observe` | 2,736 |
| `runtime` | 3,514 |
| `store` | 11,522 |
| roots and cross-cutting code | remainder |
| **Total** | **approximately 33.5–33.9k** |

The growth history is instructive:

- initial observation and checking: approximately 2.9k lines;
- exact model persistence and history: approximately +16.5k;
- reorgs and durable findings: approximately +9.9k;
- archive bootstrap: approximately +8.2k.

The rewrite must preserve the complete independent semantic model while replacing the
representations around it. The production target is **16–20k LOC**, planning around **18k**.
The test target is **15–19k LOC**. A 15–16k production stretch target may be attempted only
after the compact state, effect, and journal prototypes prove that it does not delete semantic
or operational guarantees.

The implementation should replace the current architecture rather than incrementally grooming
it. Keep the current checker as a differential oracle until the compact checker proves semantic,
durability, restart, reorg, alert, and coverage parity.

## 2. Definition of success

The rewrite succeeds only if it preserves all of the following:

1. authenticated Zone and Tempo evidence before semantic use;
2. independent checker-owned queue, commitment, fee, sender-tag, ID, counter, ownership, batch,
   refund, and `S/D/W` derivation;
3. complete ordinary-deposit, failed-deposit, user-withdrawal, finalization, submission,
   processing, refund, bounce-back, callback-deposit, empty-batch, partial-processing, and ring
   lifecycle coverage;
4. separate expected and observed construction paths;
5. exact fixed state, token supply, and Portal collateral checks;
6. commit before mirror adoption and acknowledgement;
7. durable compact findings and a sticky active-alert latch;
8. restart from a validated authoritative state;
9. canonical reorg recovery and exact finding-orphan rules;
10. truthful distinction between verified and merely acknowledged history;
11. observe-only behavior that never rejects consensus;
12. production-only mutation tests proving semantic independence.

Operational latency, periodic checkpoint cost, instant reverse unwind, arbitrary historical
diagnostics, large typed error taxonomies, and automatic first-start convenience are not core
semantic guarantees. They may be traded for a smaller system if restart, reorg, alert, and
coverage correctness remain explicit.

## 3. Independence policy

Independence means independent derivation of claims, not a separate implementation of every
wire type, primitive, provider, and database utility.

For every checked claim `C`:

```text
expected(C) = checker-owned semantic derivation
actual(C)   = authenticated production output
```

Those derivation paths must remain separate. Neutral transport and representation may be
shared.

### 3.1 Mechanical kernel boundary

Create a small workspace crate:

```text
crates/checker-kernel
package name: zone-checker-kernel
```

This crate owns semantic state, commands, expected effects, transitions, commitments,
accounting, invariants, and compact finding data. It may depend only on neutral infrastructure,
such as:

- Alloy scalar and byte types;
- generic Keccak, RLP, and ABI primitives;
- `serde` for exact versioned state serialization;
- deterministic standard collections;
- `thiserror` if useful.

It must not depend on:

- `zone-evm`;
- `zone-precompiles`;
- `zone-l1`;
- `zone-payload`;
- `zone-sequencer`;
- `zone-node`;
- production queue, fee, state-layout, lifecycle, event-selection, genesis, or accounting helpers.

The existing `zone-checker` crate owns authenticated observation, generated wire bindings,
comparison against authenticated events, persistence, the checkpoint builder, ExEx integration,
coverage, and metrics.

### 3.2 Sharing matrix

| Concern | Policy | Guard |
|---|---|---|
| `Address`, `B256`, `U256`, `Bytes` | Share | Neutral data carriers |
| Generic Keccak, ABI, RLP, receipt trie and bloom algorithms | Share | Independent known-answer vectors |
| Generated ABI calls, events, and tuple structs | Share as wire carriers | Checker-owned selectors/topics, bounds, canonical round trips, and semantic interpretation |
| Generic `Observed<E>` wrapper and event coordinates | Share | Observation-only construction and chain provenance |
| RPC transports, authentication, retry primitives | Share | Provider responses remain hostile until exact binding |
| ExEx and MDBX infrastructure | Share | Must not choose semantic pass/fail |
| Persistence codec machinery | Share | Schema version, size limit, trailing rejection, rebuild policy |
| Queue folds, sentinels, field ordering | Checker-owned | External literal vectors and production-only mutations |
| Fee and bounce-back fee arithmetic | Checker-owned | Boundary, rounding, overflow, and cap vectors |
| Sender tags and fallback identities | Checker-owned | Literal preimage vectors |
| Batch membership, ordering, queue ownership | Checker-owned | Partial, empty, capacity, and wraparound traces |
| Ownership and `S/D/W` transitions | Checker-owned | Lifecycle and liability reconstruction tests |
| Storage slots and packing | Independently pinned | Never read through production layout helpers |
| Production genesis and current model snapshots | Never semantic inputs | Locally authenticate and replay |

### 3.3 Generated ABI types

Delete checker-local Solidity declarations where the generated contract binding is complete and
correct. Use generated types only as wire containers. Do not call production semantic methods
attached to those types, such as production withdrawal queue hashes or deposit folds.

The intended pattern is:

```rust
// Shared wire representation.
fn to_abi(&self) -> GeneratedWithdrawal { /* field mapping */ }

// Checker-owned expected-value derivation.
fn checker_hash_with_tail(&self, tail: B256) -> B256 {
    keccak256((self.to_abi(), tail).abi_encode_params())
}
```

Retain independently pinned selectors, topics, indexed layouts, discriminators, length bounds,
canonical re-encoding, and trailing-byte rejection. Where production bindings omit a deployed
event, retain the checker-owned declaration until the neutral binding is corrected.

### 3.4 Observed events

Inside observation and comparison, prefer:

```rust
struct Observed<E> {
    position: EventPosition,
    event: E,
}
```

over one copied `Observed*` struct and accessor forest per generated event. `EventPosition` must
include chain provenance and authenticated transaction/log coordinates.

The expected side remains a separate checker-owned enum. Sharing the observed wire type does not
share expected-value derivation.

### 3.5 Constants

Semantic constants involved in expected calculations remain checker-owned literals or are backed
by independent literal assertions. Neutral wire declarations and allocation-only limits may be
shared when doing so cannot make production and checker derive the same wrong expected value.

Keep independent pinning for:

- withdrawal sentinel and fold direction;
- queue capacity and no-queue sentinel;
- fee scales and base gas;
- initial configuration used in expected state;
- fixed storage slots and packing;
- address derivation used as a checked identity.

## 4. Target architecture

```text
crates/checker-kernel/src/
  lib.rs
  facts.rs          authenticated semantic commands and branch facts
  effects.rs        checker-owned expected effects and exact-state expectations
  state.rs          logical rows, lifecycle values, overlay, and delta
  commitments.rs    checker-owned ABI preimages, hashes, folds, fees, constants
  apply.rs          imported and Zone transitions in protocol order
  invariants.rs     compact complete authoritative-state validation
  finding.rs        compact semantic failure data

crates/checker/src/
  lib.rs
  observe/
    mod.rs
    decode.rs
    zone.rs
    tempo.rs
    state.rs
  compare.rs        expected effects versus Observed<generated event>
  persistence/
    mod.rs
    schema.rs
    codec.rs
  service/
    mod.rs
    build.rs
    run.rs
    coverage.rs
  metrics.rs
```

The exact file split may change when cohesion warrants it. Do not recreate deep adapter,
projection, finding-codec, history, diagnostic, or runtime wrapper trees.

The essential flow is:

```text
authenticated observations
       │
       ├──────────────► Observed<wire event>
       ▼
ordered commands and branch facts
       │
       ▼
independent logical transition
       │
       ├──────────────► expected effects
       ▼
candidate StateDelta + exact-state expectations
       │
       ▼
explicit reconciliation
       │
       ├──────────────► compact finding + active alert
       ▼
atomic journal/head commit
```

## 5. Compact semantic state

Replace `ModelState`'s parallel maps, `LogicalDelta`'s parallel maps,
`LogicalMutationRef`, per-map overlay methods, `ModelStateParts`, store-side model projections,
and stored mirror types with one checker-semantic row space:

```rust
enum StateKey {
    Portal,
    Zone,
    Token(Address),
    Deposit(DepositId),
    Withdrawal(WithdrawalId),
    Batch(BatchId),
    Fallback(FallbackId),
    PortalRefund(PortalRefundId),
    InboxRefund(InboxRefundId),
}

enum StateValue {
    Portal(PortalState),
    Zone(ZoneState),
    Token(TokenState),
    Deposit(DepositState),
    Withdrawal(WithdrawalState),
    Batch(BatchState),
    Fallback(FallbackState),
    PortalRefund(PortalRefundCredit),
    InboxRefund(InboxRefundCredit),
}

struct State {
    rows: BTreeMap<StateKey, StateValue>,
    derived_refund_totals: RefundTotals,
}

struct Overlay<'a> {
    parent: &'a State,
    writes: BTreeMap<StateKey, Option<StateValue>>,
}

struct StateDelta {
    writes: Vec<(StateKey, Option<StateValue>)>,
}
```

These are semantic keys and values, not MDBX encoding types.

Requirements:

- retain distinct typed IDs;
- retain explicit owner and lifecycle enums;
- retain per-origin rows;
- expose only private typed transition accessors;
- reject key/value-family mismatch during decoding;
- lock `StateKey` ordering with tests because it affects deterministic deltas and range queries;
- rebuild derived refund totals from origin rows;
- implement one generic parent-plus-overlay range iterator;
- consume the overlay into a sorted, unique `StateDelta` used directly by persistence.

This should delete the current nine-map delta and most store-side model projection code without
making lifecycle state optional or untyped.

## 6. Facts, effects, and reconciliation

Use compact enums rather than one struct, constructor, getter set, stored mirror, and finding
wrapper per event.

```rust
enum Command {
    CreatePortal { /* authenticated input fields */ },
    UpdateConfig { /* authenticated input fields */ },
    EnableToken { /* authenticated input fields */ },
    AppendDeposit { /* authenticated input fields */ },
    SubmitBatch { /* authenticated input fields */ },
    ClaimRefund { /* authenticated input fields */ },
    RequestWithdrawal { /* authenticated input fields */ },
    FinalizeBatch { /* authenticated input fields */ },
    ProcessWithdrawals { /* authenticated input fields */ },
}

enum ExpectedEffect {
    DepositAppended { /* independently derived fields */ },
    BatchSubmitted { /* independently derived fields */ },
    WithdrawalProcessed { /* independently derived fields */ },
    RefundClaimed { /* independently derived fields */ },
    TokenEnabled { /* independently derived fields */ },
    DepositProcessed { /* independently derived fields */ },
    DepositFailed { /* independently derived fields */ },
    WithdrawalRequested { /* independently derived fields */ },
    BatchFinalized { /* independently derived fields */ },
}
```

Comparison receives checker-owned expected effects and authenticated `Observed<E>` wire events.
It explicitly matches every variant and field. A generic sequence helper may handle only
count/order/index mechanics; it must not generate semantic field comparison.

### 6.1 Field-level provenance

Classify each authenticated field, not each complete event, as exactly one of:

- **consumed**: a branch fact legitimately used as model input;
- **compared**: an implementation output checked against an independent prediction;
- **unchecked**: an explicit documented non-claim.

An event may contain fields in multiple classes. For example, a branch-success flag may be
consumed while its fee, queue index, counter, or commitment is compared.

Every compared field requires a mutation test proving that changing only that field produces a
finding. Never derive expected and observed fixture values from one shared semantic object.

### 6.2 Transition API and ordering

```rust
fn apply_imported(
    parent: &State,
    facts: &ImportedFacts,
) -> Result<ImportedCandidate, ModelError>;

fn apply_zone(
    imported: ImportedCandidate,
    facts: &ZoneFacts,
) -> Result<Candidate, ModelError>;

struct Candidate {
    delta: StateDelta,
    expected_effects: Vec<ExpectedEffect>,
    expected_state: ExpectedState,
}
```

The pipeline order remains explicit:

1. authenticate imported L1 operations;
2. apply imported transitions;
3. compare imported effects;
4. compare Portal collateral at the exact imported cut;
5. apply the Zone deposit prefix;
6. apply Zone operations in transaction/log order;
7. apply optional finalization only in its protocol position;
8. compare Zone effects;
9. compare exact fixed state and token supplies;
10. release `StateDelta` only after every comparison succeeds.

Do not introduce generic unordered event dispatch or a `Projection::apply` façade.

## 7. Compact authoritative validation

Preserve every invariant while replacing the bespoke error taxonomy with:

```rust
struct InvariantViolation {
    code: InvariantCode,
    location: Option<StateKey>,
    expected: Option<Datum>,
    actual: Option<Datum>,
}
```

The validator still checks:

- creation-state closure and configured identity;
- token phases;
- cursor ordering and equal-cursor hash agreement;
- deposit suffix continuity and commitment;
- withdrawal origins and counter bounds;
- fallback links;
- batch ranges, boundaries, queue identity, processing ordinal, and continuity;
- queue capacity and ring ownership;
- Portal and Inbox per-origin refund credits;
- refund aggregate overflow;
- owner-derived deposit and withdrawal liabilities;
- exact `S/D/W` closure;
- collateral arithmetic.

Run it:

- after checkpoint plus journal reconstruction on restart;
- after reconstruction of a reorg ancestor;
- after checkpoint-builder completion;
- after transitions in tests and debug builds.

Do not run a second complete validation state machine in every persistence projection layer.

## 8. Observation changes

### 8.1 Full L1 transaction-root authentication

Fix the current evidence hole before cutover:

1. fetch every full transaction envelope for the exact imported block;
2. locally hash every envelope;
3. reconstruct `transactions_root`;
4. compare it with the authenticated imported header;
5. bind receipts to that locally derived order;
6. decode needed Portal calldata from those authenticated envelopes;
7. delete the selective by-hash body-fetch path.

Keep:

- complete receipt vectors;
- cardinality and coordinate checks;
- receipt root and bloom recomputation;
- exact imported-header equality;
- bounded ABI allocation;
- canonical ABI and RLP round trips;
- fail-closed topic policy for protocol emitters;
- exact-hash state and collateral reads.

### 8.2 Shared receipt plumbing

A neutral pure helper that computes receipt root and bloom may be shared with production code.
The checker retains stricter cardinality, block identity, transaction index/hash, ordering, and
error policy. Do not reuse production event classifiers that skip unknown protocol topics.

### 8.3 Provider infrastructure

Reuse generic URL, TLS, authentication, reconnect, and transport retry construction where
practical. Do not reuse semantic caches, inherited values, latest-state fallback, event indexes,
or prepared model projections.

## 9. Persistence: checkpoints plus a canonical forward journal

Replace current sparse store mirrors, separate canonical table, row-level reverse before-images,
and arbitrary historical reconstruction with four logical tables:

1. `Meta`;
2. `Checkpoints`;
3. `Journal`;
4. `Findings`.

### 9.1 Meta

```rust
struct Meta {
    version: u32,
    identity: Identity,
    active_checkpoint: CheckpointId,
    verified_zone_tip: BlockNumHash,
    imported_tempo_tip: BlockNumHash,
    acknowledged_zone_tip: BlockNumHash,
    active_finding: Option<FindingKey>,
    coverage: Coverage,
}

enum Coverage {
    Complete,
    Gap {
        first_unchecked: BlockNumHash,
        acknowledged_through: BlockNumHash,
        reason: CoverageGapReason,
    },
}
```

The schema version must be independently decodable before any other value. Version mismatch
reports actual, expected, and rebuild path without opening writable state.

### 9.2 Checkpoints and journal

```rust
struct Checkpoint {
    cut: ChainCut,
    state: State,
}

struct JournalEntry {
    zone: BlockNumHash,
    parent: BlockNumHash,
    imported_tempo: BlockNumHash,
    delta: StateDelta,
}
```

The journal is local. Ordinary restart and reorg do not depend on archive RPC availability.

### 9.3 Findings

Use one compact versioned finding record containing:

- Zone block number/hash and parent;
- optional imported Tempo coordinate;
- stable coarse category/code;
- event/state location and operation index;
- small expected and actual values;
- length plus digest for large values;
- bounded evidence summary.

Delete:

- the 33-variant durable `FindingKind` mirror;
- runtime-to-`Stored*` projections;
- per-leaf wire tags;
- the semantic canonical-byte tree used only for hashing;
- mutable canonical/orphan status rows;
- most per-variant codec golden and malformed-byte tests.

`Meta.active_finding` is the active latch. Unreferenced retained findings are orphan evidence.

### 9.4 Codec

Use a generated, bounded, deterministic exact codec rather than hand-writing every value family.
It must provide:

- schema versioning;
- maximum decoded size;
- trailing-byte rejection;
- unknown-tag/version rejection;
- deterministic `BTreeMap` ordering;
- exact key/value-family validation;
- representative round-trip and corruption tests.

Schema changes rebuild from authenticated history. There is no in-place migration requirement.

### 9.5 Apply

For each accepted block:

1. authenticate, transition, and compare before opening a writer;
2. atomically append `JournalEntry` and advance `Meta`;
3. commit;
4. adopt `StateDelta` in memory;
5. only then acknowledge.

### 9.6 Checkpoint

At a fixed initial interval:

1. serialize the exact current `State`;
2. atomically write the checkpoint and update `Meta.active_checkpoint`;
3. preserve the immutable bootstrap checkpoint;
4. leave the previous checkpoint usable until the transaction commits.

Initially retain the canonical journal without pruning. This preserves unbounded reorg recovery
equivalent to the current unbounded undo history while avoiding a premature horizon decision.
Checkpoint pruning and a fixed reorg horizon are separate future work.

### 9.7 Restart

1. decode and validate version and identity;
2. load the active complete checkpoint;
3. replay the unbroken local canonical journal to `Meta.verified_zone_tip`;
4. reject missing, duplicate, parent-mismatched, or hash-conflicting entries;
5. run the complete invariant pass;
6. require the active finding row when named;
7. adopt the reconstructed state.

### 9.8 Reorg

1. select the latest retained checkpoint at or before the common ancestor, with the immutable
   bootstrap checkpoint as fallback;
2. replay the local canonical journal to the ancestor;
3. validate the reconstructed state;
4. atomically truncate journal entries above the ancestor and move `Meta` to the ancestor;
5. update the active finding in the same transaction;
6. apply the replacement branch through the ordinary path.

Finding rules:

- reverting only descendants above the finding preserves the alert;
- removing the exact finding hash orphans the finding and clears the latch;
- conflicting evidence at the finding height is an error, not permission to clear;
- a replacement branch containing the finding remains alerting.

Crash during checkpoint creation must leave the previous checkpoint usable. Crash during reorg
must leave either the complete old branch or the complete ancestor state, never a mixture.

### 9.9 Sparse-row fallback

The checkpoint/journal design is the primary plan because it removes more representation and
recovery code. If its prototype fails explicit checkpoint-size, restart-latency, or reorg-latency
acceptance criteria, fall back to a simplified sparse store using the same semantic `StateKey`,
`StateValue`, and `StateDelta`. The fallback must still delete the second model type system,
compact findings, arbitrary retained-height reconstruction, and diagnostic CLI. Do not fall back
merely because the current before-image design already exists.

## 10. Deterministic local checkpoint builder

Move creation, ancestry, genesis proof, and initial replay out of the live ExEx state machine into
a deterministic local command using the same observation and kernel transition pipeline:

```text
zone-node checker build-checkpoint ...
```

The exact spelling should follow existing CLI conventions.

The builder must:

1. authenticate configured Portal creation;
2. prove exact creation/anchor ancestry;
3. validate genesis identity and supply;
4. replay imported and Zone history through the ordinary compact kernel;
5. run full invariant validation;
6. write an identity-bound checkpoint atomically;
7. refuse incompatible identity or unrelated nonempty state;
8. never trust a third-party checkpoint without local replay.

It may use the same checkpoint/journal format for resumability. The live ExEx refuses to start in
observe mode without a complete compatible local checkpoint. This changes startup convenience,
not trust or semantic coverage.

A post-genesis-anchor-only bootstrap is an optional product scope cut, not a transparent
refactor. Do not adopt it without an explicit inventory proving every supported deployment fits
that topology.

## 11. Runtime and coverage

Replace startup, driver, operational, retry, terminal, status, and head-probe machinery with:

- one bounded FIFO;
- one current notification;
- attempt and wall-clock retry budgets;
- `Starting | Healthy | Retrying | Alerting | Disabled`;
- a sticky active finding;
- durable coverage state.

Failure classes:

1. `ImmediateTerminal`: immutable local/config/notification inconsistency;
2. `BoundedRetry`: missing or inconsistent exact remote evidence;
3. `TransientRetry`: transport/provider unavailability;
4. `AuthenticatedDivergence`: persist finding and enter alert mode.

While retrying, continue draining into the bounded FIFO. When retry or FIFO capacity is exhausted,
persist the exact gap before fail-open acknowledgement. Never wait forever while no longer
draining Reth.

For a multi-block notification, commit successful prefix blocks individually. If a later block
fails, persist the exact skipped suffix before acknowledging beyond it.

On stream error, reconstruct catch-up from the last durable ready tip. Do not blindly repoll an
uncertain stream.

Alert descendants are acknowledged but not checked. Report them as
`NotCheckedAncestorDivergence`, not passing blocks.

Essential metrics:

- runtime state;
- verified height;
- acknowledged height;
- first unchecked height;
- finding height;
- unchecked block/notification count;
- retry count and exhaustion;
- gap reason.

Delete or defer generic retained-key diagnostics, allocation/model-row metrics, stale
live/catch-up classification, and the current ignored performance characterization until real
thresholds exist.

## 12. Differential migration strategy

Keep the existing checker as an oracle until cutover. Compare semantic state, effects, findings,
and coverage rather than private structs or persistence bytes.

Port lifecycle slices in this order:

1. creation, configuration, and token enablement;
2. ordinary deposits and contiguous processing;
3. failed deposits and refund ownership;
4. user withdrawal requests and fee derivation;
5. finalization and queue commitment;
6. submission and ring ownership;
7. delivery, pending refund, and bounce-back;
8. callback deposits;
9. partial processing and empty batches;
10. aggregate claims and complete closure.

For each slice, feed identical authenticated facts to old and compact implementations and require
equality of:

- expected effects;
- counters and cursors;
- queue commitments;
- owner identities and phases;
- token accounting and `S/D/W`;
- collateral requirements;
- semantic state;
- finding category and coordinates after one-field mutations.

Do not share transition or commitment calculations between old and compact paths during parity
testing unless external literal vectors independently cover that calculation.

## 13. Milestones for an Amp `medium` agent

This is a multi-milestone program. Keep the branch compiling and green at each boundary. Do not
delete the old implementation before Milestone 6.

### Milestone 0: baseline and characterization

- confirm branch, worktree, and applicable guidance;
- record source and test LOC by subsystem;
- run the focused checker test suite;
- inventory every authenticated field as consumed, compared, or unchecked;
- map existing tests to lifecycle and durability guarantees.

Gate: baseline and any pre-existing failures reported before edits.

### Milestone 1: full L1 transaction authentication

- fetch complete imported transaction envelopes;
- locally hash them and verify `transactions_root`;
- bind receipts to local hashes/order;
- decode Portal calls from authenticated envelopes;
- remove selective by-hash transaction fetch;
- add fabricated hash-list/body/index/order tests.

Gate:

```sh
cargo fmt --check
cargo test -p zone-checker
cargo clippy -p zone-checker --all-targets --all-features -- -D warnings
```

### Milestone 2: compact kernel skeleton

- add `zone-checker-kernel` with the safe dependency set;
- implement logical rows, overlay, delta, compact effects, and compact invariant errors;
- port creation, token, and deposit transitions;
- add semantic snapshots and differential tests.

Gate:

- old and compact paths agree for all creation/token/deposit fixtures and mutations;
- no prohibited kernel dependencies;
- measured trajectory supports a complete 6–7.5k LOC semantic kernel.

### Milestone 3: complete lifecycle kernel

- port the remaining lifecycle slices in protocol order;
- complete invariants and explicit reconciliation;
- add field-level mutation coverage.

Gate:

- all old/new semantic traces agree;
- every compared field has a mutation test;
- production-only semantic mutations fail in the compact checker;
- expected and observed constructors remain disjoint.

### Milestone 4: checkpoint/journal store

- implement four-table schema and exact codec;
- implement apply, checkpoint, restart, reorg, compact findings, coverage, and active-alert latch;
- add fault injection and corruption tests.

Gate:

- restart from checkpoint plus journal;
- reorg before, after, and across checkpoint boundaries;
- active finding preserved and orphaned correctly;
- transaction aborts leave one complete state;
- incompatible/corrupt state is rejected;
- persistence production code is approximately 2.5–3.5k LOC.

### Milestone 5: builder and compact runtime

- implement local checkpoint builder and node CLI;
- implement the one-loop runtime and retry taxonomy;
- integrate coverage, gap, alert, catch-up, and acknowledgement rules.

Gate:

- commit before ack;
- bounded retry cannot stall notification draining;
- exact partial-notification gap;
- startup/stream failure gap;
- alert descendants explicitly unchecked;
- restart can replay a recoverable gap;
- real-node smoke passes.

### Milestone 6: cutover and deletion

- switch public configuration and launch wiring;
- delete old model adapters, output wrappers, reconciliation trees, store mirrors, finding forest,
  reverse history, diagnostics, runtime wrappers, and live bootstrap FSM;
- remove temporary `v2`, `compact`, or `legacy` names;
- update `README.md`, `DESIGN.md`, and `MODEL_VECTORS.md` to match actual guarantees;
- remove claims not backed by real-node evidence.

Gate:

- no old semantic implementation remains;
- complete checker and node integration suites pass;
- production LOC is at most 20k, targeting approximately 18k;
- tests are at most 19k unless extra lines provide independent mutation or crash coverage;
- documentation distinguishes verified, gapped, and ancestor-divergent history.

## 14. Required test inventory

### Semantic

- every lifecycle branch;
- empty and nonempty batches;
- partial processing;
- callback deposits;
- ring capacity and logical-index reuse;
- refund aggregate overflow;
- queue sentinel and fold direction;
- fee rounding, cap, boundary, and overflow;
- complete `S/D/W` closure.

### Authentication

- wrong system transaction position, sender, or destination;
- duplicate/missing finalization;
- malformed ABI/RLP, offsets, discriminators, and trailing bytes;
- unknown protocol-emitter topic;
- receipt count, block, index, hash, order, root, and bloom;
- fabricated L1 transaction hash list or body;
- stale/latest instead of exact hash.

### Persistence and runtime

- commit before acknowledgement;
- abort before and after journal/head/checkpoint writes;
- duplicate replay;
- restart at every durable boundary;
- one-block, multi-block, and interrupted reorg;
- reorg across checkpoint;
- active finding above, at, and below reorg boundary;
- partial notification suffix;
- retry and FIFO exhaustion;
- durable gap before fail-open;
- malformed notification watermark policy.

### Independence

Mutate production-only queue folds, sentinel, sender tag, fees, slots, packing, processing suffix,
refund origin, lifecycle ordering, and state/event consistency. The unchanged compact checker must
find each mutation.

## 15. Test consolidation policy

Keep:

- external literal commitment and ABI vectors;
- focused transition branch tests;
- one long lifecycle/recovery scenario;
- malformed observation mutations;
- state corruption mutations;
- representative transaction fault tests;
- commit-before-ack restart;
- ordinary and interrupted reorg;
- alert-preserving and alert-removing reorg;
- both real-node smoke tests.

Delete or consolidate:

- duplicate lifecycle matrices;
- repeated owner inventories;
- checker-level scenarios duplicating transition tests;
- durable finding semantic/golden layers;
- repeated open/schema/model codec fixture trees;
- per-physical-write fault loops once transaction-class atomicity is covered;
- metric spelling tests;
- tests that prove only an abstraction being deleted.

Fixture values may be shared. Expected and observed derivation code may not.

## 16. Validation commands

Use the narrowest relevant command during development, then broaden at each gate:

```sh
cargo fmt --check
cargo test -p zone-checker-kernel
cargo test -p zone-checker
cargo clippy -p zone-checker-kernel -p zone-checker --all-targets --all-features -- -D warnings
cargo test -p zone-node --features cli,test-utils --test it checker
```

Equivalent focused `cargo nextest` commands are acceptable when available. The ignored
performance characterization is not a release gate until it has release-profile p95,
throughput, and bytes-per-block thresholds over lifecycle-heavy history.

## 17. LOC budget

| Area | Target production LOC |
|---|---:|
| Observation, decode, exact state, full transaction-root authentication | 3.0–3.5k |
| Facts, expected effects, observed comparison | 1.5–2.2k |
| State, commitments, fees, transitions, invariants | 6.0–7.5k |
| Checkpoint, journal, finding persistence | 2.5–3.5k |
| Builder, runtime, reorg, coverage, alerts | 2.4–3.2k |
| Root, status, essential metrics | 0.4–0.7k |
| **Total** | **16–20k** |

The design budget is 18k. A result above 20k requires architectural review before cutover. Do not
force the number by deleting independent derivations, mutation tests, crash tests, or truthful
coverage.

## 18. Agent reporting contract

After every milestone, report:

1. outcome and architecture decisions;
2. files changed;
3. production and test LOC by subsystem;
4. exact differential evidence;
5. commands and tests run;
6. failures and unverified assumptions;
7. whether the milestone gate passed;
8. the smallest next milestone.

Compilation is not parity. Unit tests are not real-node evidence. Do not delete the old checker
until semantic, persistence, runtime, and active-alert parity all pass.

## 19. Recommended execution cadence

Use one Amp `medium` thread across the program, but give it explicit continuation boundaries:

1. Milestones 0–1;
2. compact kernel skeleton and deposit parity;
3. remaining lifecycle parity;
4. checkpoint/journal persistence;
5. builder and runtime;
6. cutover, deletion, documentation, and full validation.

If time or context runs short, stop at a clean compiling milestone and return a precise handoff.
Do not begin a destructive cutover in a thread that cannot also complete the associated parity and
recovery gates.
