# Zone checker logical model: implementation specification

Status: design approved for implementation

Target: first useful shadow-deployed release

Implementation workflow: reviewable Codex `/goal` milestones

## 1. Executive recommendation

Replace the current collection of temporary facts and one-off invariants with one
checker-owned logical state machine. For every canonical non-genesis Zone block, the
checker must:

1. decode the `advanceTempo` input from the first Zone transaction;
2. authenticate the imported Tempo header and the Portal events in that block;
3. replay the imported Portal operations into a candidate logical state;
4. check Portal collateral at the post-Tempo/pre-Zone logical cut;
5. replay the Zone block in transaction and log order;
6. independently derive queue hashes, fees, identities, batches, and lifecycle changes;
7. compare those results with authenticated implementation outputs, fixed Zone state commitments,
   and exact Zone supply;
8. atomically persist the new model state, canonical progress, and unwind before-images;
9. acknowledge the ExEx notification only after the database commit.

The checker is a specification model, not a second EVM. It must model every bridge-value
lifecycle, but it does not independently decide whether arbitrary execution succeeded. For
example, it does not evaluate AES-GCM, Chaum-Pedersen proofs, TIP-403 policy execution, token
code, or callback code. It accepts the locally authenticated success/failure outcome as a
branch input, then independently verifies all logical consequences of that branch. This is
the smallest boundary that closes the bridge lifecycle without sharing the production state
transition implementation.

There is one release boundary: the complete system described here. The implementation goals
below are review boundaries only. A deposit-only model, a batch-only model, or a model without
both refund paths must not be shadow deployed or described as useful coverage.

### The resulting architecture

```text
canonical Zone block + receipts + exact post-state
                         │
                         │ first tx supplies the Tempo header
                         ▼
              authenticated observation adapter
                         │
        imported Tempo header + receipt-root-checked logs
        + selectively fetched direct Portal transaction inputs
                         │
                         ▼
            checker-owned pure logical transition
                         │
          ┌──────────────┴──────────────┐
          │ expected outputs            │ forward logical delta
          ▼                             ▼
 implementation output checks     exact Zone state/supply + collateral checks
          │                             │
          └──────────────┬──────────────┘
                         ▼
             one checker MDBX transaction
       model state + canonical tip + before-images + findings
                         │
                         ▼
                 ExEx FinishedHeight
```

## 2. Protocol facts and resolved assumptions

This design is based on the repository at the following dependency revisions:

- Tempo: `916141317f121d9b39e4f6d3c8bb7dad4c2c4af6`
- Reth: `fc3f7da91b345cd011767a3e71cde541ebfa02e0`

The implementation must re-audit these facts if either revision changes.

### 2.1 Block and import ordering

- Every non-genesis Zone block starts with exactly one `advanceTempo` system transaction,
  followed by user transactions, and optionally ends with the unique batch-finalization
  transaction (`specs/spec.md:742-750`).
- Imported Tempo blocks and post-genesis Zone blocks are consecutive and one-to-one; a Zone
  may catch up quickly but may not skip or reuse an imported block
  (`specs/spec.md:750-752`, `specs/spec.md:796-802`).
- The checker obtains the imported header from `advanceTempo` calldata. It must not use
  `TempoAdvanced` to select the header whose identity that event is supposed to confirm.
- The current Zone genesis helper stores the exact L1 anchor hash and number but does not
  prove that earlier Portal queues, batches, counters, fallbacks, or refunds were empty
  (`crates/node/src/genesis.rs:29-114`). Development provisioning deliberately anchors before
  `createZone` so the first Zone block imports the creation block
  (`crates/node/src/dev.rs:61-148`).
- Release one requires that nonzero anchored-genesis checkpoint. The protocol also describes a
  zero-checkpoint bootstrap in which the first import may select any finalized Tempo block that
  proves the Portal exists (`specs/spec.md:796-802`); supporting that style would require a
  different gap-replay rule and is explicitly out of scope.

### 2.2 Deposit processing is always prefix processing

The model has one deposit-import rule: an `advanceTempo` call may consume any contiguous
oldest-first prefix of the pending Portal deposit queue, including the empty prefix. Full
catch-up is the special case in which the remaining suffix is empty. There is no checker mode,
fork branch, or second implementation for full processing.

The current native Inbox still requires its computed processed hash to equal the Portal's
current queue hash (`crates/precompiles/src/inbox/mod.rs:134-147`), so current successful blocks
will consume the full pending queue. Partial processing is anticipated protocol behavior. The
checker model and fixed vectors must support it before production begins emitting partial
prefixes. A production protocol change may alter its own equality/proof checks; it must not
require a checker state migration or alternate transition.

The model needs the ordered open deposit records, the Portal's append-only queue commitment,
and the Zone's processed `(hash, deposit_number)` cursor. No additional mode state is needed.

The repository currently contains one deposit-rejection inconsistency that release one must resolve
conservatively. The written spec says there is no sequencer-supplied accept/reject decision
(`specs/spec.md:445-457`), while a later paragraph and the Solidity reference Inbox retain a
`QueuedDeposit.rejected` branch and `DepositRejected` event (`specs/spec.md:495-506`,
`specs/ref-impls/src/zone/ZoneInbox.sol:236-268`). The pinned native Inbox calldata omits that flag
and its execution has only processed-or-failed outcomes
(`crates/contracts/src/precompiles/zone_inbox.rs:38-42`,
`crates/precompiles/src/inbox/mod.rs:106-131`,
`crates/precompiles/src/inbox/mod.rs:194-231`). Release one follows the pinned native protocol:

- it does not synthesize or accept a sequencer-rejection outcome;
- `DepositRejected` from the native Inbox address is an unsupported protocol event and freezes the
  model rather than being treated as an authenticated branch selector;
- if rejection is added to the production calldata, the version-pinned adapter and expected-event
  vectors must first be updated to authenticate the rejection flag.

The later protocol change can reuse the existing failed-deposit ownership and accounting
transition. It requires no new durable owner or database migration, but it is not release-one
behavior merely because the stale Solidity reference implements it.

### 2.3 Withdrawal identities

An ordinary accepted user withdrawal has:

- the actual sender emitted by `WithdrawalRequested`;
- the hash of the containing Zone transaction;
- a monotonically assigned, nonzero, unique fallback nonce;
- a durable fallback owner keyed by nonce and holding token, amount, and originating withdrawal
  until successful delivery or a bounce-back consumes it.

The fee is:

```text
(50_000 + gas_limit) * tempo_gas_rate
```

and both principal and fee are burned on the Zone. The fee is not part of the Portal withdrawal
preimage (`specs/spec.md:598-621`). Same-block `TempoGasRateUpdated` and
`MaxWithdrawalsPerBlockUpdated` events take effect in transaction/log order.

The checker models the withdrawal cap with one ephemeral counted-withdrawal scalar per Zone block:

1. The scalar starts at zero for the block.
2. A config update does not reset it.
3. When the active limit is zero, a user withdrawal does not increment it.
4. When the active limit is nonzero, require `count < limit`, then increment.
5. Disabling and re-enabling the limit in one block therefore preserves the withdrawals counted
   while it was nonzero; withdrawals accepted while it was zero remain uncounted.
6. Inbox-generated failed-deposit withdrawals never count.

This is the observable logical behavior of the lazy production counter
(`crates/precompiles/src/outbox/mod.rs:122-145`,
`crates/precompiles/src/outbox/mod.rs:245-255`,
`crates/precompiles/src/outbox/mod.rs:357-369`, `specs/spec.md:604`). It needs no durable
per-block counter. Release one initializes `tempo_gas_rate`, `max_withdrawals_per_block`, and
Portal `bounceback_gas` to the literal deployment/genesis value zero, then replays every successful
update in order. Goal 0 must pin those initial values to the generated precompile initialization and
Portal deployment source (`specs/ref-impls/src/tempo/ZonePortal.sol:132-134` already makes the
Portal default explicit); observed live configuration must never become the baseline.

A failed ordinary deposit creates a special withdrawal with:

- `sender = address(0)`;
- internal `tx_hash = bytes32(0)`, not the containing `advanceTempo` transaction hash;
- `fallback_nonce = 0`, intentionally reusable;
- no user `FallbackOwner` entry;
- `gas_limit = 0` and empty callback/reveal data;
- an identity linked to the failed Portal deposit, not to nonce zero.

These values follow the specification and implementation
(`specs/spec.md:489-516`, `crates/precompiles/src/outbox/mod.rs:245-255`,
`crates/precompiles/src/outbox/mod.rs:406-495`). The checker must have a fixed vector for the
sender tag derived from zero sender and zero transaction hash.

### 2.4 Withdrawal batching and Portal processing

- Finalization folds all pending withdrawals newest-to-oldest around the literal empty sentinel
  so the oldest withdrawal is outermost and processed first (`specs/spec.md:623-635`).
- Each withdrawal belongs to exactly one finalized Zone batch. Each submitted non-empty batch
  belongs to exactly one logical Portal queue slot. Empty batches advance the batch index without
  consuming a Portal queue index (`specs/spec.md:637-649`).
- `processWithdrawals(withdrawals, remainingQueue)` proves and consumes a FIFO prefix of the
  current Portal slot. Partial processing leaves `remainingQueue` as the slot commitment;
  exhaustion restores the sentinel and advances the logical head.
- `processWithdrawals([], arbitraryRemainingQueue)` is an exact no-op. The production loops do
  not inspect `remainingQueue` when the array is empty
  (`specs/ref-impls/src/tempo/ZonePortal.sol:971-995`). The checker must not reject the call, read
  the queue head, or change any state in this case.

Release one requires `submitBatch` and every non-empty `processWithdrawals` that drives the model
to be a direct top-level transaction to the configured Portal. A successful nested call that
emits a model-relevant event is `UnsupportedNestedPortalCall`: it triggers a finding and freezes
the model. An empty nested `processWithdrawals` has no observable protocol effect and need not be
discovered.

### 2.5 Bounce-backs and refunds

- A failed user withdrawal creates one `WithdrawalBounceBack` Portal deposit containing token,
  amount, and fallback nonce (`specs/spec.md:671-695`). Its Zone import consumes the fallback
  owner and either mints or creates one Inbox refund credit.
- A failed ordinary deposit remains deposit-origin liability while its special withdrawal waits
  for batching and Portal processing. Portal processing either pays the refund or creates one
  Portal refund credit after deducting the computed bounce-back fee
  (`specs/spec.md:489-526`).
- Successful claims clear all credits represented by the claimed `(token, recipient)` aggregate.
  The checker retains per-origin credits so lifecycle identity is not lost merely because the
  implementation refund maps aggregate by recipient.
- The failed-deposit bounce-back fee is independently derived as
  `min(ceil(bounceback_gas * block_basefee / 10^12), withdrawal_amount)`. The active
  `bounceback_gas` at the exact `processWithdrawals` transaction and the authenticated imported
  Tempo header's base fee are both inputs. Same-block `BouncebackGasUpdated` ordering therefore
  matters (`specs/spec.md:509-516`).

### 2.6 Exact supply and collateral

Zone tokens are specified to have only Inbox mint authority and Outbox burn authority
(`specs/spec.md:303-312`, `specs/spec.md:1393-1400`). The checker starts each newly enabled Zone
token at an independently expected supply of zero. A future protocol with nonzero genesis supply
requires a new checker model version with an explicit fixed genesis rule; observed supply must
never silently become the baseline.

The exact TIP-20 total-supply storage slot is literal slot `8`. This is confirmed by the pinned
Tempo field layout (`tempo@9161413:crates/precompiles/src/tip20/mod.rs:74-103`) and its Solidity
layout fixture
(`tempo@9161413:crates/precompiles/tests/storage_tests/solidity/testdata/tip20.layout.json:78-84`).
Live checking reads this slot through the in-process node's `state_by_block_hash`; Reth defines
that provider as exact-hash state including the named block's changes
(`reth@fc3f7da:crates/storage/storage-api/src/state.rs:134-181`). It does not generate or persist
an MPT proof.

At the logical cut immediately after replaying the imported Tempo block and before applying the
Zone block, the checker queries each enabled TIP-20's `balanceOf(Portal)` at the exact imported
block hash and requires:

```text
Portal balance >= S + D + W
```

The configured L1 archive RPC is trusted for this exact-block balance call. Calls occur before an
MDBX write transaction opens. Passing values are not persisted.

### 2.7 Authenticated outcomes that are intentionally not predicted

The following successful-block outcomes are authenticated implementation outputs used to select
a model branch, not values the checker independently predicts:

- whether an encrypted deposit is processed or fails verification/mint;
- the private recipient of a successful deposit mint;
- whether a Portal token transfer or callback succeeds;
- whether a bounce-back mint succeeds or becomes an Inbox refund;
- whether a refund claim transaction succeeds (only successful receipts/events are modeled);
- arbitrary callback side effects, except for full-preimage Portal deposit events they emit.

For each branch, the checker still independently determines the required identity movement,
queue transition, refund credit, and `S/D/W` change and compares all exposed commitments. This
limitation is an explicit release-one non-claim, not an excuse to omit a lifecycle edge.

## 3. Essential versus incidental complexity

### 3.1 Essential components

| Component | Why correctness or operation forces it | What fails without it | Classification |
|---|---|---|---|
| One pure logical transition module | Expected results must not come from production helpers | Common-mode queue, identity, and fee bugs survive | Authoritative logic |
| One verified model tip | There must be exactly one parent for the next transition | Duplicate/reordered apply becomes ambiguous | Authoritative progress |
| Open lifecycle records | Every nonterminal identity and unit of value needs one owner | Duplicate, dropped, and stranded value cannot be detected | Authoritative state |
| Per-token `S/D/W` | Supply and Portal collateral need exact lifecycle totals | Aggregate solvency cannot be checked | Authoritative derived state |
| Canonical height-to-hash rows | Restart, duplicate notification, and unwind need fork identity | Height alone cannot distinguish a reorg | Authoritative progress/index |
| Exact per-block before-images | Ordinary and multi-block reorgs must unwind without replaying from genesis | Reorg requires a full rebuild | Unwind journal |
| Compact findings | Divergence must survive restart and remain diagnosable after a reorg | Alerts lose coordinates/evidence | Durable diagnostic evidence |
| Bootstrap phase/cursor | Portal history may predate Zone genesis and acquisition must resume | Crash restarts a potentially long bootstrap | Authoritative operational progress |
| Active alert record | The model freezes while acknowledgements continue | Restart might apply descendants of a divergent block | Authoritative status |
| Short-lived observations | External data must be acquired before writing | Pure transition has no authenticated inputs | Disposable data |

### 3.2 Components intentionally omitted

| Omitted component | Why it is incidental in release one |
|---|---|
| Generic invariant registry/trait | There is one state machine and one deterministic comparison path. A registry adds dispatch without an ownership boundary. |
| Second observed semantic state machine | Authenticated observations are inputs/outputs, not another mutable truth. |
| Raw evidence archive or normalized observation tables | L1 and L2 archive nodes are an explicit operating requirement. Re-fetching is simpler than duplicating chain history. |
| Passing supply/collateral observations | They are reproducible and have no recovery role. Only failures are durable. |
| Accepted forward-delta log | Current state plus exact before-images is sufficient. The chain archives reproduce forward deltas. |
| Periodic full model snapshots | Current model state already is the materialized snapshot. Historical snapshots duplicate it. |
| Custom WAL | One MDBX transaction is the crash boundary. |
| Static-file evidence | Pinned Reth's static-file system is for supported node segments; the checker has no custom segment integration and no raw evidence requirement. |
| ETL/bulk-import framework | Replay is already ordered and updates a small materialized model. Add bulk machinery only if measurements show bootstrap is write-bound. |
| In-place migrations | Any semantic or incompatible format change rebuilds a fresh DB from archives. |
| DUPSORT tables | Composite ordered keys cover refund-origin and changeset relationships without duplicate-key cursor semantics. |
| Per-check provider/factory abstractions | The ExEx already owns concrete local-state and L1-provider handles. Introduce a test seam only where an existing trait naturally supplies one. |
| Concurrent writers/retry machinery | The ExEx is the sole checker writer. Parent-tip mismatch is an invariant violation, not a transaction retry. |

### 3.3 Existing abstractions to use

Use pinned Reth APIs rather than libmdbx:

- `tables!` and `TableSet` define a checker table set
  (`reth@fc3f7da:crates/storage/db-api/src/tables/mod.rs:106-290`).
- `init_db_for::<_, CheckerTables>` creates a dedicated MDBX environment with those tables
  (`reth@fc3f7da:crates/storage/db/src/mdbx.rs:76-117`).
- `Database`, `DbTx`, `DbTxMut`, typed cursors, walkers, `put`, `append`, and `delete` provide all
  required operations (`reth@fc3f7da:crates/storage/db-api/src/database.rs:8-26`,
  `reth@fc3f7da:crates/storage/db-api/src/transaction.rs:20-79`).
- Checker values should follow Tempo's existing `reth_codecs::Compact` practice, while ordered
  composite keys use explicit fixed-width big-endian `Encode`/`Decode` implementations.
- Reth's numeric unwind helper illustrates descending cursor deletion, but model restoration uses
  checker-owned before-images because model keys are not block-number keyed
  (`reth@fc3f7da:crates/storage/db-api/src/unwind.rs:5-61`).
- `ExExNotification` exposes committed, reverted, and reorged chains, and
  `send_finished_height` is the acknowledgement API
  (`reth@fc3f7da:crates/exex/types/src/notification.rs:7-46`,
  `reth@fc3f7da:crates/exex/exex/src/context.rs:110-136`).

## 4. Trust boundary and source-of-truth audit

The integrated code must preserve separate concrete Rust types for `AuthenticatedInput`,
`AuthenticatedOutcome`, `ExpectedOutput`, and `Finding`. This makes it difficult to use an output
to choose the input it confirms. Goal 0 freezes the role table below; Goal 1 introduces the
authenticated input/outcome types in the adapters that can actually establish those claims, later
pure-model goals introduce expected-output types, and the reporting/persistence goals own the
concrete finding type. Do not use a generic crate-wide wrapper constructor that can relabel an
arbitrary value as authenticated.

| Transition/check | Authenticated model input | Implementation output checked or branch outcome | Independently derived expectation | Unavailable or deliberately trusted data |
|---|---|---|---|---|
| Tempo import | First Zone tx `advanceTempo` calldata; RLP Tempo header | `TempoAdvanced`; exact post-block `TempoState` hash/number | Header hash/number/parent continuity, one import | L1 finality is inherited from node configuration, not independently proved |
| L1 block logs | Header receipt root plus the full ordered receipt set | Portal events in successful receipts | Receipt root and logs bloom; event ordering and Portal address | RPC receipt-to-transaction metadata is trusted after root verification |
| Portal creation | Configured creation block hash; authenticated `ZoneFactory.ZoneCreated` | Portal/factory/zone identity and constructor events | Configured identity, initial token/config, first counters | Automatic creation-block discovery is provisioning, not checker core |
| Portal token/config | Full-preimage Portal events in receipt order | `TokenEnabled`, `BouncebackGasUpdated` | Uniqueness, exact order, active config at each later operation | Arbitrary Portal storage not proved |
| Portal deposit append | Full `DepositMade` or `WithdrawalBounceBack` event | Emitted new hash and deposit number | Checker-owned ABI preimage hash, monotonic number, `D`/`W` ownership | Deposit/callback execution validity is not re-executed |
| Zone deposit prefix | `advanceTempo.deposits`; oldest open Portal records | Per-item ordinary processed/failed or withdrawal-bounce-back processed/pending event; `TempoAdvanced`; exact post-block Inbox processed hash/number | Contiguous prefix identity and hash; exactly one type-correct outcome; required mint/refund owner | Crypto validity and private mint recipient proof are excluded; sequencer rejection is unsupported by the pinned native ABI |
| User withdrawal | Successful Zone receipt/log, containing tx hash, active config | `WithdrawalRequested` | Index, nonzero nonce, fee, sender tag, burn/accounting, one fallback owner | Nested arbitrary call execution is not re-executed; withdrawal-time fallback recipient is deliberately not acquired |
| Failed-deposit withdrawal | Failed ordinary deposit record | `WithdrawalRequested` emitted during `advanceTempo` | Zero sender/hash/nonce, no fallback map, all remaining fields, unique origin | Nothing is inferred from containing tx hash |
| Zone batch finalization | Final system-call calldata, including `encryptedSenders` | `BatchFinalized`; exact post-block Outbox queue hash/index | Batch index, members, sender tags, hash fold, block/deposit transition | Encrypted-sender cryptographic validity is excluded |
| Portal `submitBatch` | Selectively fetched direct tx calldata, trusted bound to imported block | `BatchSubmitted` event | Batch continuity, block/deposit transitions, queue slot/head/tail effect | Proof and threshold cryptography are not independently checked |
| Portal processing | Selectively fetched direct tx calldata and authenticated per-item outcomes | `WithdrawalProcessed`, bounce-back/refund events | FIFO preimages, partial suffix, empty-array no-op, one lifecycle transition | Arbitrary transfer/callback success is a branch outcome |
| Refund claim | Successful claim event | Claimed recipient/token/amount | Sum of per-origin open credits; close exactly those credits; supply/liability effect | Failed calls produce no transition |
| Zone supply | Literal slot 8 at exact Zone block hash | Actual post-block `U256` | Per-token `S` | In-process exact-state provider is trusted; no MPT proof |
| Portal collateral | Exact-block `balanceOf(Portal)` for each enabled token | Actual post-L1 balance | `S + D + W` at the pre-Zone cut | Configured L1 archive RPC is trusted; no storage proof |

An RPC timeout, missing response, block-hash mismatch, or receipt-root mismatch is an acquisition
failure until a valid authenticated observation exists. It must not be converted into zero/default
data or a protocol finding. Authenticated, well-formed data that violates a logical rule is a
finding.

The six fixed Zone commitment outputs and per-token supply use the same trusted in-process
exact-block-hash state provider. They are authenticated implementation outputs, not expected-value
sources, and the checker does not generate independent MPT proofs for them. A missing exact state
value is an acquisition failure, never a zero/default observation.

## 5. Minimal complete lifecycle and state machine

### 5.1 One-owner rule

At every verified tip, each nonterminal origin is represented by exactly one open record. A
transition atomically replaces that record's owner state in place, or deletes it and creates either
one new owner or a terminal outcome. Terminal records need not be retained forever: monotonic
counters, queue commitments, and the absence of an open owner prevent replay, while archive history
can reconstruct details. Before-images restore replaced or deleted owners during unwind.

### 5.2 Lifecycle closure audit

| Entity/value origin | Unique identity | Creation transition | Current owner representation | Permitted next transitions | Terminal outcomes |
|---|---|---|---|---|---|
| Ordinary Portal deposit | `(portal, deposit_number)` | Authenticated `DepositMade` | `PendingDeposit` | Consume as next Zone deposit | Zone mint; failed-deposit withdrawal path |
| Withdrawal bounce-back deposit | `(portal, deposit_number)` linked to user withdrawal | Failed Portal user delivery | `PendingDeposit::WithdrawalBounceBack` | Consume as next Zone deposit | Zone bounce-back mint; Inbox refund credit |
| Accepted user withdrawal | `(zone_id, withdrawal_index)` plus unique fallback nonce | Authenticated `WithdrawalRequested` | `Withdrawal::Pending(User)` and `FallbackOwner(token, amount, withdrawal_index)` | Finalize into next batch | Successful Portal delivery; bounce-back path |
| Failed-deposit withdrawal | `(zone_id, withdrawal_index)` linked to ordinary deposit | Zone ordinary-deposit failure | `Withdrawal::Pending(FailedDeposit)` | Finalize into next batch | Direct Portal refund; Portal refund credit |
| Finalized batch | `(zone_id, withdrawal_batch_index)` | Final Zone system transaction | `Batch` plus its contiguous `Withdrawal::Finalized` range | Submit once to Portal | Empty submitted batch; fully processed Portal slot |
| Submitted non-empty Portal slot | `(portal, logical_queue_index)` linked to batch | Direct successful `submitBatch` | Submitted `Batch` cursor and remaining `Withdrawal` rows | FIFO full/partial processing | Slot exhausted, head advanced |
| User fallback owner | `(zone_id, fallback_nonce)` | User withdrawal acceptance | `FallbackOwner(token, amount, withdrawal_index)` | Delete on successful Portal delivery, or consume by the linked bounce-back deposit | Successful Tempo delivery; minted to observed outcome recipient; moved to observed-recipient Inbox refund credit |
| Portal refund credit | `(token, recipient, failed_deposit_id)` | Failed direct failed-deposit refund | `PortalRefundCredit` | Successful aggregate claim | Tempo refund paid |
| Inbox refund credit | `(token, recipient, user_withdrawal_id)` | Failed Zone bounce-back mint | `InboxRefundCredit` | Successful aggregate claim | Zone refund minted |

No edge may simply delete value. A transition that cannot find exactly the expected owner is a
finding and freezes the model.

The fallback owner intentionally does not contain `zoneFallbackRecipient`. On bounce-back import,
the authenticated `WithdrawalBounceBackProcessed` or `WithdrawalBounceBackPending` event supplies
the outcome recipient; a pending event's recipient keys the `InboxRefundCredit`. The checker still
requires the nonce, token, amount, and originating withdrawal to match and consumes the owner once,
but does not claim that the observed outcome recipient equals the withdrawal-time
`zoneFallbackRecipient`.

### 5.3 Required transition sequence

#### Portal deposit to Zone mint or failed-deposit refund

```text
DepositMade
  -> PendingDeposit(ordinary)
  -> advanceTempo consumes the next prefix item
     -> DepositProcessed -> terminal Zone mint
     OR
     -> DepositFailed
        -> Withdrawal::Pending(failed deposit, zero identity fields)
        -> Withdrawal::Finalized in the next Batch range
        -> submitted Portal slot
        -> processWithdrawals
           -> DepositBounceBack -> terminal Tempo refund
           OR
           -> DepositBounceBackPending -> PortalRefundCredit
              -> Portal RefundClaimed -> terminal Tempo refund
```

#### User withdrawal to delivery or Zone bounce-back

```text
WithdrawalRequested
  -> Withdrawal::Pending(user) + FallbackOwner
  -> Withdrawal::Finalized in the next Batch range
  -> submitted Portal slot
  -> processWithdrawals
     -> WithdrawalProcessed(success=true)
        -> terminal Tempo delivery; delete FallbackOwner
     OR
     -> WithdrawalProcessed(success=false) + WithdrawalBounceBack
        -> PendingDeposit(withdrawal bounce-back); FallbackOwner remains
        -> advanceTempo consumes the next prefix item
           -> WithdrawalBounceBackProcessed
              -> terminal Zone mint; delete FallbackOwner
           OR
           -> WithdrawalBounceBackPending
              -> InboxRefundCredit; delete FallbackOwner
              -> Inbox RefundClaimed -> terminal Zone mint
```

The fallback owner may be deleted on successful Portal delivery because no later transition can
consume it. It remains present after Portal failure until the bounce-back deposit is imported.

The pinned native Inbox supports only `DepositProcessed` and `DepositFailed` for ordinary deposits.
If a future protocol version adds an authenticated rejection flag, its rejection event must require
the same failed-deposit owner/accounting transition: it may skip decryption/mint execution, but it
may not skip the deposit queue or required failed-deposit withdrawal. Section 2.2 defines the
release-one fail-closed behavior until such an input exists.

### 5.4 Batch state required for settlement checks

For each open batch, retain:

- batch index;
- first Zone parent hash and final Zone block hash;
- first and final processed-deposit `(hash, number)` cursors;
- final imported Tempo block number;
- final Zone height;
- expected withdrawal queue hash;
- immutable first withdrawal index and member count;
- next Portal-processing ordinal;
- optional submitted logical Portal queue index.

`Withdrawal(withdrawal_index)` is the stable identity key from acceptance until terminal Portal
processing. Finalization replaces its value in place, changing `Pending` to `Finalized` and adding
`encryptedSender` and the complete public preimage. Its batch assignment is represented once by the
immutable batch range. The member at ordinal `n` is `first_withdrawal_index + n`; no `BatchMember`
key is needed. Partial Portal processing deletes only the consumed `Withdrawal` rows and advances
the batch cursor, so updates and before-images remain proportional to the consumed prefix.

### 5.5 Token accounting

Each Portal-enabled token has exact unsigned `U256` aggregates:

- `S`: expected Zone token total supply;
- `D`: deposit-origin value still backed by the Portal but not represented by Zone supply or a
  terminal Tempo refund;
- `W`: user-withdrawal principal burned on the Zone but not delivered on Tempo or reminted on the
  Zone.

All arithmetic is checked. Underflow or overflow is a finding.

| Transition | `S` | `D` | `W` |
|---|---:|---:|---:|
| Token enabled | initialize to `0` | initialize to `0` | initialize to `0` |
| Ordinary `DepositMade(net_amount)` | — | `+ net_amount` | — |
| Ordinary deposit mint succeeds | `+ amount` | `- amount` | — |
| Ordinary deposit fails and special withdrawal is queued | — | — | — |
| Failed-deposit refund paid directly | — | `- original_amount` | — |
| Failed-deposit refund becomes pending | — | `- bounceback_fee` | — |
| Portal pending refund claimed | — | `- refund_amount` | — |
| User withdrawal accepted | `- (amount + fee)` | — | `+ amount` |
| User withdrawal delivered on Tempo | — | — | `- amount` |
| Failed delivery creates a Portal bounce-back deposit | — | — | — |
| Withdrawal bounce-back mint succeeds | `+ amount` | — | `- amount` |
| Withdrawal bounce-back becomes Inbox refund | — | — | — |
| Inbox pending refund claimed/minted | `+ amount` | — | `- amount` |

For a pending failed-deposit refund, verify
`refund_amount + bounceback_fee == original_amount`. The fee is no longer a user liability even
if the best-effort admin transfer itself failed; any retained value is Portal surplus. For a
failed user withdrawal, the amount must not enter `D`: it remains the same withdrawal-origin
liability in `W` while represented by the bounce-back deposit.

## 6. Pure model API and transition ordering

### 6.1 Module boundary

The model module must depend only on checker-owned protocol types, generic primitive hashing/ABI
facilities, and deterministic collections. It must not depend on the production Inbox, Outbox,
Portal, payload builder, sequencer, prover transition, or their queue/fee helper methods.

A suitable API shape is:

```rust,ignore
pub struct ModelState { /* authoritative materialized state */ }

pub struct ImportedBlockInput { /* authenticated L1 inputs and outcomes */ }
pub struct ZoneBlockInput { /* canonical L2 inputs and outcomes */ }

pub struct LogicalDelta {
    pub mutations: BTreeMap<ModelKey, Option<ModelValue>>,
    pub comparisons: Vec<Comparison>,
}

pub fn apply_imported_block(
    state: &ModelState,
    input: &ImportedBlockInput,
) -> Result<LogicalDelta, ModelError>;

pub fn apply_zone_block(
    state_after_l1: &ModelState,
    input: &ZoneBlockInput,
) -> Result<LogicalDelta, ModelError>;
```

The exact Rust names are not a compatibility promise. The required properties are:

- deterministic input plus parent state produces deterministic delta/comparisons;
- no provider, database, clock, environment, logging, or async access in the model;
- outcomes and expected outputs are distinct fields/types;
- the persistence adapter, not the model, captures exact database before-images;
- the candidate state is not made authoritative until all comparisons pass and MDBX commits.

An in-memory overlay over the verified state is preferable to cloning all open records per block.
Do not add a generic storage-provider trait solely for this overlay.

### 6.2 Conceptual order for each Zone block

1. Verify the Zone parent equals `verified_tip`.
2. Require/decode the first `advanceTempo` transaction and its successful receipt.
3. Decode the imported header and verify consecutive Tempo parent/number.
4. Fetch and authenticate the exact imported L1 header and receipt set.
5. Replay successful Portal operations in `(transaction_index, log_index)` order.
6. Form the post-L1/pre-Zone candidate cut.
7. Check each enabled token's Portal collateral against that cut.
8. Apply `advanceTempo`:
   - token enablements in supplied order;
   - the supplied contiguous deposit prefix in order;
   - exactly one authenticated outcome per consumed item;
   - expected processed hash/number and `TempoAdvanced` output.
9. Process the remaining successful Zone transaction logs in canonical order:
   - Inbox refund claims;
   - `TempoGasRateUpdated` and `MaxWithdrawalsPerBlockUpdated` immediately when emitted;
   - accepted user withdrawals using the active rate/config at that point.
10. If present, require finalization to be the unique final system transaction and derive its
    complete batch commitment.
11. At the exact post-Zone block hash, compare expected `TempoState` hash/number, Inbox processed
    deposit hash/number, and Outbox last-batch queue hash/index. These fixed commitment reads catch
    an event that reports the expected value while implementation state stores a different value.
12. Read and compare exact post-Zone supply for every enabled Zone token.
13. If every comparison passes, persist once and acknowledge after commit.

Provider calls can be scheduled efficiently, but the logical cut and transition order must remain
as above. In particular, the collateral call observes post-L1 state while `S` is still the parent
Zone supply.

### 6.3 Portal operation ordering

The observation adapter must combine direct-call inputs with receipt logs into one ordered block
input. It must preserve callback-generated `DepositMade` events that occur during a
`processWithdrawals` transaction. For a non-empty direct `processWithdrawals`, calldata supplies
the withdrawal preimages; authenticated events supply each arbitrary-execution outcome. The model
must reconcile every calldata item to exactly one expected outcome sequence and reject missing,
extra, duplicated, or reordered events.

An empty withdrawal array returns before inspecting queue state. The model emits an empty delta
regardless of `remainingQueue`.

## 7. Authenticated observation and disposable evidence

### 7.1 L2 adapter

Use the canonical block, transactions, senders, and receipts supplied by the in-process Reth
notification. Do not ask RPC which L2 block is canonical. The adapter must:

- validate transaction/receipt cardinality;
- require exactly one successful `advanceTempo` system envelope in each non-genesis block, as the
  first transaction, with the protocol Zone system caller and `ZoneInbox` destination
  (`specs/spec.md:742-750`, `crates/precompiles/src/inbox/mod.rs:77-86`);
- require canonical `advanceTempo` ABI calldata with no trailing bytes, decode its imported Tempo
  header and deposit/token inputs, and require canonical header RLP by decode/re-encode equality;
- treat `TempoAdvanced` as output and compare every exposed field;
- retain containing transaction hash and event order for each accepted withdrawal;
- when finalization is present, require it to be the unique final transaction with the protocol
  Zone system caller and `ZoneOutbox` destination, require canonical ABI calldata with no trailing
  bytes, require `blockNumber` to equal the Zone block, and decode all `encryptedSenders` rather
  than trying to reconstruct those opaque bytes;
- require finalization `count` and `encryptedSenders.len()` to equal the modeled pending count, and
  require each encrypted sender to be empty when `revealTo` is absent or exactly the literal
  113-byte authenticated-withdrawal encoding when present (`specs/spec.md:625`,
  `specs/spec.md:709`, `crates/precompiles/src/outbox/mod.rs:287-321`);
- bound every dynamic array and byte string from authenticated calldata before allocation using
  calldata length, protocol count relationships, `MAX_CALLBACK_DATA_SIZE = 1_024`, and the
  authenticated-withdrawal size; malformed lengths must never cause an unbounded allocation;
- read the exact post-block `TempoState` hash/number, Inbox processed deposit hash/number, Outbox
  last-batch queue hash/index, and enabled-token total supplies as implementation outputs;
- reject authenticated protocol logs that cannot be reconciled with a supported transition.

Canonical envelope validation stops at fields required by the Zone protocol. Do not duplicate SPF
checks for incidental header, beneficiary, timestamp, gas, or arbitrary EVM execution fields.

### 7.2 Version-pinned protocol event classification

Event handling is a fixed, version-pinned classification by **emitting address and topic**, not a
generic event registry and not a name-only match. The pinned event surfaces are defined by the
Portal/factory interface and the native Zone ABIs
(`specs/ref-impls/src/interfaces/IZone.sol:504-605`,
`specs/ref-impls/src/interfaces/IZone.sol:652`,
`crates/contracts/src/precompiles/zone_factory.rs:40-51`,
`crates/contracts/src/precompiles/zone_inbox.rs:57-90`,
`crates/contracts/src/precompiles/outbox.rs:29-47`,
`crates/contracts/src/precompiles/tempo_state.rs:5-8`):

| Emitter | Model-driving or checked events | Known non-model-driving events |
|---|---|---|
| Configured Portal | `DepositMade`, `TokenEnabled`, `BatchSubmitted`, `WithdrawalProcessed`, `WithdrawalBounceBack`, `DepositBounceBack`, `DepositBounceBackPending`, `RefundClaimed`, `BouncebackGasUpdated` | `SequencerEncryptionKeyUpdated`, `ZoneGasRateUpdated`, `MaxTempoGasRateUpdated`, `AdminTransferStarted`, `AdminTransferred`, `RoleUpdated`, `EnforcementModesUpdated`, `SequencerSetUpdated`, `LeaderUpdated`, `DepositsPaused`, `DepositsResumed`, `RpcUrlUpdated` |
| `ZoneFactory` | The configured Portal's bootstrap `ZoneCreated` | `OwnershipTransferred`; unrelated or post-bootstrap `ZoneCreated` |
| `ZoneInbox` | `TempoAdvanced`, `DepositProcessed`, `DepositFailed`, `WithdrawalBounceBackProcessed`, `WithdrawalBounceBackPending`, `RefundClaimed`, `TokenEnabled` | None; `DepositRejected` is explicitly unsupported for the pinned native ABI |
| `ZoneOutbox` | `WithdrawalRequested`, `BatchFinalized`, `TempoGasRateUpdated`, `MaxWithdrawalsPerBlockUpdated` | None |
| `TempoState` | `TempoBlockFinalized` | None |

Every listed event is strictly ABI-decoded, with bounded dynamic fields, even when it is known
non-model-driving. Known non-model-driving Portal events are intentionally ignored because their
effects do not change release-one identities, commitments, lifecycle ownership, `S/D/W`, exact
Zone supply, or collateral; for example, a routine `LeaderUpdated` must not halt the checker. A
topic outside this table from one of these configured/fixed protocol addresses is
`UnsupportedProtocolEvent` and takes the finding/freeze path. This prevents a protocol upgrade from
silently escaping the model. Logs from other addresses, including TIP-20 transfer logs and callback
contracts, are outside this protocol-emitter allowlist and are ignored unless a supported Portal
transition explicitly consumes them.

### 7.3 L1 adapter

For the header supplied by `advanceTempo`:

1. compute its hash from its RLP bytes;
2. fetch that exact header/block by hash and require exact identity and number;
3. fetch the complete ordered receipt set;
4. require transaction/receipt cardinality metadata to be coherent;
5. recompute the receipt root and logs bloom against the imported header;
6. decode successful Portal/factory logs from authenticated receipts;
7. selectively fetch only transactions containing model-relevant Portal events whose calldata
   is required (`submitBatch` and non-empty `processWithdrawals`);
8. require their RPC-reported block hash/number/index and transaction target to match the direct
   Portal transaction expected by the receipt metadata;
9. canonically decode calldata with no trailing bytes and verify the transaction selector matches
   its events.

Release one deliberately trusts the configured L1 archive RPC to bind those selectively fetched
transactions to the authenticated block. It does not fetch all transaction bodies or recompute the
transaction root. Receipt-root verification remains mandatory because it authenticates the event
stream. This trust shortcut must be visible in health/config documentation.

### 7.4 Evidence lifetime

Headers, receipts, transaction bodies, decoded observations, passing comparisons, supply values,
and collateral values are disposable after the block commits. On failure, persist only a compact
finding with enough coordinates and digests to re-fetch full details from archive nodes:

- checker model/format version;
- finding code;
- Zone block number/hash and parent hash;
- imported Tempo block number/hash, when known;
- transaction hash/index and log index, when relevant;
- logical identity/model key;
- small typed expected and actual values, or their length plus hash when large;
- canonical/orphan status and timestamps only if an existing node clock convention is available.

This field list is the canonical durable `Finding` shape. Large expected or actual values are
represented by bounded length-and-hash summaries rather than copied evidence.

Do not persist raw calldata, receipts, normalized observations, or successful reads.

## 8. Persistent data model

### 8.1 Environment and schema

Use a dedicated checker MDBX environment under the node data directory, not extra tables in the
node's chain database. This isolates geometry, lifecycle, model-version rebuilds, and the sole
checker writer from the node's database schema. Open it through pinned Reth
`init_db_for::<_, CheckerTables>` with ordinary Reth database arguments. Do not call libmdbx.

Release one has five non-DUPSORT tables:

| Table | Key | Value | Role |
|---|---|---|---|
| `CheckerMeta` | `MetaKey` tagged byte | typed `MetaValue` | Identity, version, bootstrap phase/cursor, verified tip, active alert |
| `CheckerCanonical` | big-endian Zone `u64` height | canonical `B256` | Accepted canonical hash and idempotence/reorg index |
| `CheckerModelState` | ordered tagged `ModelKey` | `ModelValue` | Current authoritative open records, counters, commitments, configs, and `S/D/W` |
| `CheckerChangesets` | `(height_be, block_hash, ordinal_be)` | `BeforeImage` | Exact pre-block value or absence for each first-touched model key, plus block parent metadata |
| `CheckerFindings` | `(zone_height_be, zone_hash, ordinal_be)` | compact `Finding` | Durable active and orphaned diagnostics |

Fixed-width composite keys must encode numeric components big-endian so cursor order is block and
ordinal order. Values should use `reth_codecs::Compact` with golden byte tests. No codec may
silently accept trailing bytes, unknown tags, or an unknown model version.

### 8.2 Metadata

Required metadata is:

- format/model version (one version number is sufficient);
- Zone chain ID and genesis hash;
- L1 chain ID;
- configured ZoneFactory and Portal addresses;
- configured Portal creation block hash;
- bootstrap phase and exact L1 cursor while bootstrapping;
- verified Zone tip `(number, hash)` and imported Tempo tip `(number, hash)`;
- optional active alert pointing to one finding and the last verified parent.

Do not duplicate Reth's ExEx acknowledgement cursor in checker state. Reth owns operational
`FinishedHeight` progress. A crash can replay an acknowledgement or a descendant while active
alert state is set; both paths are idempotent. Add a checker-owned acknowledgement cursor only if
the pinned API is proven not to persist enough information for restart, and document that proof
before changing this design.

### 8.3 Model keys

The minimum key families are:

- `PortalConfig` and `ZoneConfig`;
- `PortalDepositCursor` and `ZoneProcessedDepositCursor`;
- `Token(token)` containing enabled-side flags and `S/D/W`;
- `PendingDeposit(deposit_number)`;
- `Withdrawal(withdrawal_index)` whose value is `Pending` or `Finalized`;
- `FallbackOwner(fallback_nonce)`;
- `Batch(batch_index)`;
- `PortalRefundCredit(token, recipient, failed_deposit_number)`;
- `InboxRefundCredit(token, recipient, withdrawal_index)`;
- current Zone batch-accumulator state;
- monotonic next/last indices required to reject reuse.

Refund claims use prefix scans over composite model keys to find all per-origin credits for the
claimed `(token, recipient)`. This is a natural ordered-key range and does not require DUPSORT.

Persist only open lifecycle records. When an identity becomes terminal, delete its open record in
the same transition that updates counters/accounting. Monotonic counters and queue cursors remain
as compact anti-replay state.

### 8.4 Before-images

For each successfully applied Zone block:

1. sort mutations by encoded model key;
2. for each key, read its committed pre-block value once;
3. append one changeset row containing `Some(old_value)` or `None`;
4. write/delete the new value;
5. store one block metadata row containing the prior verified/imported tips;
6. store canonical height/hash and advance metadata tips.

If a key changes multiple times within one block, its changeset still contains only the exact
pre-block value. This is both smaller and sufficient to restore the parent state.

Current `CheckerModelState` plus the complete retained canonical changeset chain must reconstruct
the exact model state and verified/imported tips at every canonical Zone end-of-block boundary from
genesis through the current verified tip. To reconstruct target height `h`, start from a read-only
snapshot of current state and apply changesets from the verified tip down through `h + 1`, validating
every canonical block hash and changeset sequence. Reconstruction must use an in-memory overlay or
separate scratch database; diagnostic reads must never unwind or mutate the authoritative checker
database. A missing row, hash conflict, duplicate ordinal, or undecodable before-image is explicit
corruption, not permission to return a partial state.

Changesets answer **what logical state changed** and permit exact historical state inspection. They
do not retain the authenticated calldata, receipt, or event that explains **why** it changed. A
diagnostic path combines the reconstructed before/after model values with the canonical block hash
and re-fetches raw evidence from the configured L1/L2 archives. Release one guarantees canonical
Zone block-boundary reconstruction only: pre-genesis L1 bootstrap intermediates, intra-block model
steps, and complete orphan-fork model states require archive replay and are not retained as
first-class historical state.

### 8.5 Category audit

- **Authoritative state:** `CheckerModelState`, verified/imported tips, bootstrap state, active
  alert, and canonical mapping.
- **Derived authoritative state:** `S/D/W` and queue commitments. They are checked derivatives but
  persisted because every next transition and cheap supply/collateral check needs them.
- **Unwind journal:** `CheckerChangesets`.
- **Durable diagnostic evidence:** `CheckerFindings`.
- **Disposable cache:** all fetched/decoded chain data and passing observations. Release one has no
  durable disposable-cache table.

## 9. Atomic apply, acknowledgement, and divergence behavior

### 9.1 Valid block

The sole writer follows this order:

1. Read the verified tip and required model state in a short read transaction; close it.
2. Fetch all L1 data and exact state/balance observations. No write transaction is open.
3. Authenticate/decode observations and compute the full candidate logical delta in memory.
4. Evaluate collateral, expected outputs, and exact Zone supply.
5. Open one MDBX write transaction.
6. Re-read and require unchanged parent tip and no active alert.
7. Capture first before-images, apply model mutations, store changesets/canonical row, and advance
   both tips.
8. Commit MDBX.
9. Apply the committed delta to any in-memory mirror.
10. Send `FinishedHeight` for the notification only after commit.

There must be no `.await`, provider call, state-provider construction, or network request between
steps 5 and 8.

### 9.2 Crash boundaries and duplicate replay

| Failure point | Durable state | Recovery behavior |
|---|---|---|
| Before MDBX commit | Parent remains authoritative | Reacquire and recompute the block |
| After commit, before in-memory update | DB is authoritative | Reload/patch memory from DB on restart |
| After commit, before `FinishedHeight` | Canonical row/tip already contain block | Verify same hash and acknowledge without reapplying |
| After acknowledgement | DB and Reth progress agree | Continue normally |
| Duplicate committed notification | Canonical row matches hash | No-op and acknowledge |
| Same height, different hash without revert | Canonical row conflicts | Treat as a notification/consistency error; do not overwrite |

### 9.3 Acquisition failure

Transport errors, unavailable archive data, RPC metadata conflicts, receipt-root mismatches, and
missing exact state are operational acquisition failures because no sound authenticated input is
available. The checker:

- commits no model state and no protocol finding;
- emits unhealthy status and an operational error metric;
- retries with bounded backoff;
- does not acknowledge past the gap while in normal mode;
- never substitutes defaults;
- eventually requires operator-provided archive access or resync if history is unavailable.

This may temporarily pin the ExEx pruning watermark, but it preserves the simple one-parent apply
path. Actual authenticated divergence follows the different behavior below.

### 9.4 Authenticated finding: alert-triggered mode

On the first deterministic mismatch, malformed authenticated protocol transition, unsupported
successful protocol behavior, or arithmetic/lifecycle violation:

1. stop evaluating at the first deterministic finding;
2. do not commit any candidate model mutation for that Zone block;
3. in one MDBX transaction, persist the compact finding and set `ActiveAlert` to that finding and
   the last verified parent;
4. emit critical logs, a sticky metric, and unhealthy checker status;
5. commit;
6. acknowledge the current and later ExEx notification tips without applying or checking their
   descendant blocks.

The model remains frozen at the last verified parent. Descendants are semantically
`NotCheckedAncestorDivergence`, not passing blocks. Do not create one finding per descendant.
The checker is alerting, not enforcing: it must not pin pruning or affect core node operation after
an actual divergence is authenticated.

The pinned release-one protocol exposes no authenticated protocol-version discriminator, so the
checker cannot soundly manufacture an `UnsupportedProtocolVersion` finding. Version-incompatible
successful behavior reaches the same alert path as `UnsupportedProtocolEvent`,
`UnsupportedNestedPortalCall`, or malformed authenticated data. Adding a real version field later
requires an explicit adapter rule and a new checker model version; unknown behavior must never be
guessed or skipped.

## 10. Bootstrap, backfill, restart, repair, and model upgrades

### 10.1 Required configuration and identity

Release one requires an operator-provided `portal_creation_block_hash`. Checker initialization
fetches that exact block, authenticates the matching `ZoneFactory.ZoneCreated` event, and requires
the configured factory, Portal, Zone ID/chain identity, and initial token to agree. Automatic
discovery belongs in provisioning tooling, not the checker state machine.

Initialization also reads the TempoState checkpoint from canonical Zone genesis and requires a
nonzero anchor hash. A zero checkpoint uses the protocol's arbitrary-first-import exception and is
`UnsupportedBootstrapStyle` in release one; the checker must fail before creating authoritative
model state.

The database identity tuple prevents accidental reuse for another Zone or Portal. Any mismatch
fails startup without modifying existing data.

### 10.2 Fresh bootstrap

1. Validate the local canonical Zone genesis hash, read its TempoState checkpoint, and require a
   nonzero exact Tempo anchor hash/number.
2. Authenticate the configured Portal creation block and its creation event.
3. Initialize a zero/empty checker model from literal protocol genesis rules, never from observed
   Portal counters or Zone supply.
4. Replay every canonical Tempo block from Portal creation through the Zone genesis anchor if the
   Portal already existed at that anchor. If creation is after the anchor (the current development
   pattern), retain a `PortalNotYetCreated` phase and require creation on the later consecutive
   imported path. Authenticate the replay range by exact hashes and parent-hash linkage between the
   configured creation hash and exact genesis/import anchors, and verify every block's receipt root
   using the same rule as live imported blocks. Never choose bootstrap history by an unverified
   `latest` or block-number-only response.
5. Persist the Portal model and exact L1 bootstrap cursor atomically after each replayed L1 block.
   Bootstrap commits need no Zone changeset because an incompatible/conflicting bootstrap is
   rebuilt rather than unwound.
6. Replay canonical Zone blocks from genesis forward. Each non-genesis block imports and applies
   the next Tempo block through the ordinary live transition and changeset path.
7. Catch up to the canonical Zone head, then switch to live ExEx notifications without changing
   transition logic.

The bootstrap cursor makes acquisition crash-resumable without storing raw evidence. A block is
either fully reflected with its cursor advanced in one transaction or replayed.

### 10.3 Normal restart

On restart:

- validate DB identity/version and table decoding;
- load `verified_tip`, imported Tempo tip, active alert, and current model state;
- compare the verified tip's hash with the local canonical chain;
- if canonical, resume from the Reth/ExEx head and handle duplicates idempotently;
- if not canonical, unwind to the common canonical ancestor before applying replacements;
- if an active alert exists, determine whether its Zone block is still canonical. Remain frozen if
  it is; orphan and clear it if it is not, then replay from the verified parent.

Normal restart must never replay from genesis.

### 10.4 Repair and detailed diagnostics

There is no raw checker evidence archive. Repair creates a fresh checker database and deterministically
replays the configured L1 and local L2 archive histories. Detailed finding diagnostics re-fetch the
blocks/receipts/transactions named by the compact finding.

If the local checker node no longer has historical exact Zone state, run repair on or resync it as
an L2 archive node. Do not silently replace the approved local `state_by_block_hash` supply trust
path with a different RPC path. Missing L1 or L2 archive history is a hard, explicit repair failure.

### 10.5 Upgrades

There is one model/format version. Any semantic change, key/value incompatibility, or expected-rule
change uses a new empty database and full archive replay. There are no in-place migrations,
compatibility readers, dual writes, or migration registry.

The implementation should fail with a command/actionable message that identifies the existing and
expected version and the new DB path/rebuild procedure. It must not delete or rewrite the old DB
automatically.

## 11. Reorg and changeset design

### 11.1 Ordinary and multi-block reorg

For a notification with an old and new chain:

1. determine the common ancestor using notification hashes and `CheckerCanonical`;
2. open one write transaction per unwound block, newest to oldest;
3. require the canonical row and changeset block hash to match;
4. restore every first-before-image (`Some` means put, `None` means delete);
5. restore prior verified/imported tips from the block metadata row;
6. delete that block's canonical and changeset rows;
7. mark findings anchored to the removed block orphaned; never delete them;
8. commit each unwind block;
9. apply replacement blocks oldest to newest through the ordinary acquisition/model/apply path.

Per-block unwind commits provide clear crash progress: startup reads the current verified tip and
continues descending or applying. No full-state snapshot is needed.

Release one retains all canonical changesets. A finality-based changeset pruning policy is deferred
until growth is measured and a protocol finality boundary is explicitly chosen.

### 11.2 Revert-only notification

Unwind the reverted blocks newest-to-oldest by the same path and acknowledge the resulting
canonical tip after commits. A revert is not merely logged; authoritative model state must move
backward.

### 11.3 Reorg involving a divergence

An alerting block has no applied model changeset. If it remains canonical, descendant replacement
does not clear the alert. If it is removed:

- mark its finding orphaned;
- clear `ActiveAlert` atomically;
- leave the model at its already verified parent (or unwind earlier verified blocks if the common
  ancestor is lower);
- acquire and apply the new branch in order.

If the replacement reproduces the divergence, create a new finding keyed by its new block hash.

### 11.4 Corruption or conflicting evidence

Missing changesets, a canonical hash/changeset conflict, undecodable values, impossible owner
counts, or a tip inconsistent with model metadata is checker database corruption. Do not attempt a
partial repair or infer a value from current chain state. Preserve the DB for diagnosis and require
a fresh archive rebuild.

## 12. Performance and resource growth

### 12.1 Per-block work

- L2 decoding: linear in the Zone block's transactions and logs.
- L1 authentication: linear in all receipts/logs in the single imported Tempo block because the
  receipt root is recomputed.
- L1 transaction fetching: only transactions containing model-relevant Portal events that require
  calldata.
- Deposit processing: linear in the consumed prefix.
- Withdrawal finalization/processing: linear in the finalized or processed members.
- Fixed Zone commitment reads: six local scalar reads at the exact post-block hash.
- Supply reads: one local slot read per enabled token.
- Collateral reads: one exact-block L1 call per enabled token.
- MDBX writes: one model mutation plus one before-image per distinct key touched, not per event.

No network request holds an MDBX write lock. Model application and hash derivation should be small
relative to receipt fetching and exact-block RPC latency.

### 12.2 Durable growth

- Current model state grows with open deposits, withdrawals, batches, fallback owners, and refund
  origins, then shrinks as they terminate.
- Canonical rows grow by one fixed row per verified Zone block.
- Changesets grow with distinct model keys touched by canonical history.
- Findings grow only on actual mismatches and retain orphaned records.
- No chain payload is duplicated.

### 12.3 Deferred optimizations and their trigger measurements

| Optimization to defer | Measurement that justifies it |
|---|---|
| Batched/concurrent collateral RPC | Per-token exact-block calls dominate p95 checker lag or prevent head catch-up |
| Receipt/event cache | Reorg/restart refetch volume is a measured RPC bottleneck |
| Periodic model checkpoints beyond current state | Rebuild time from archives violates an agreed recovery SLO after ordered replay and DB batching are measured |
| ETL/ordered bulk append | Bootstrap profiling shows MDBX writes, not RPC/authentication/hashing, dominate wall time |
| Changeset pruning | Measured changeset bytes exceed the operational budget and an accepted finality/unwind boundary exists |
| Split model tables or DUPSORT | Prefix scans or mixed-value codecs are demonstrated by profiles to dominate transition time |
| Raw local evidence cache/static files | Archive access is operationally unreliable enough to change the approved availability assumption |

Do not implement these based on anticipated scale alone.

## 13. Codex `/goal` implementation sequence

### 13.1 How to run the goals

Codex Goals are durable, thread-scoped objectives whose completion should be audited against files,
tests, logs, and artifacts. Each milestone below has a verifiable stopping condition. Run one
milestone per `/goal` so its diff can be reviewed before the next starts. These milestones may land
separately, but Goals 0-9 are not independently deployable checker releases.

Recommended invocation:

```text
/goal Implement Goal N from crates/checker/DESIGN.md. Stay within that goal's scope. Continue until
every acceptance criterion is supported by concrete test or source evidence. Do not mark partial
protocol coverage as deployable, do not reuse production transition helpers for expected values,
and stop with an explicit blocker rather than guessing an unsupported protocol rule.
```

For an unattended end-to-end run after the document has been reviewed:

```text
/goal Implement crates/checker/DESIGN.md through the Goal 10 closed-system release gate. Treat each
goal as a review checkpoint, run its required validation before proceeding, preserve the approved
trust and persistence boundaries, and do not stop merely because a partial lifecycle model builds.
The goal is complete only when Goal 10's full acceptance matrix passes or a concrete external
blocker is documented with evidence.
```

### Goal 0 — Freeze model contract and independent vectors

**Goal.** Encode the checker-owned logical vocabulary, transition matrix, constants, and fixed
vectors before implementing stateful behavior.

**Why.** Independence is not reviewable if expected encodings first appear by calling production
helpers in integration code.

**Prerequisites.** This document and the pinned source revisions.

**Expected files/modules.** `crates/checker/src/model/` (or an equivalently cohesive module),
checker tests/fixtures, and updates to checker documentation. Avoid changing node integration.

**In scope.**

- Checker-owned IDs and open-owner enums for deposits, withdrawals, batches, fallback owners, and
  both refund kinds.
- Literal model constants, including empty sentinel, no-queue index, base gas, initial zero config,
  and total-supply slot 8.
- Checker-owned exact-state access layouts and fixed vectors for `TempoState` hash/number,
  `ZoneInbox` processed deposit hash/number, and `ZoneOutbox` last-batch queue hash/index.
- The literal `10^12` bounce-back base-fee scale, ceiling division, amount cap, and checked
  intermediate arithmetic.
- Checker-owned ABI preimage encoders and hashes for ordinary deposits, withdrawal bounce-back
  deposits, withdrawal queue members, sender tags, and queue folds.
- Exact `S/D/W` transition table and checked arithmetic.
- The section 4 source-of-truth roles and rule-to-role mapping as the contract for the concrete
  observation, model-output, and finding types introduced by their owning goals.
- The literal address-and-topic event classification from section 7.2, including strict event
  decoding, the pinned native rejection exclusion, and unknown-event fail-closed behavior.
- Fixed vectors generated independently from literal byte strings or a small standalone script
  checked into test data only if necessary.

**Out of scope.** Providers, ExEx changes, MDBX, production fact refactors, and live comparisons.

**Constraints.**

- Do not import production `DepositQueueLib`, `hash_with_tail`, fee calculators, payload builder,
  Inbox/Outbox transition helpers, or production constants on the expected side.
- Generic `keccak256` and generic ABI encoding are allowed. Define checker-owned tuples/structs.
- Production helpers may appear only in explicitly labeled differential tests after fixed vectors
  pass.

**Required tests/evidence.**

- Ordinary deposit queue append and multi-item prefix vectors.
- Empty, partial, and full withdrawal queue vectors.
- Empty `processWithdrawals` no-op with nonzero arbitrary suffix.
- Failed-deposit zero sender/zero tx hash/zero reusable nonce vector.
- User sender-tag vector with containing transaction hash.
- Fee boundary/overflow vectors.
- Bounce-back fee vectors covering base-fee rounding, same-block gas updates, zero fee, and the
  cap at the withdrawal amount.
- Literal zero initialization vectors for `tempo_gas_rate`, `max_withdrawals_per_block`, and
  `bounceback_gas` with pinned source citations.
- Same identity cannot have two owners in model test fixtures.
- Exact-state layouts, slot 8, and literal constant source comments cite pinned paths.

**Validation.** `cargo test -p zone-checker model` and `cargo fmt --check` (or narrower equivalent
until module test names exist).

**Acceptance gate.** Every expected commitment needed by later goals has an independent fixed
vector; no production transition helper generates an expected value; all lifecycle edges in
section 5 are representable. Leave a short inventory mapping vectors to protocol rules.

### Goal 1 — Establish the authenticated observation boundary

**Goal.** Replace event-selected anchors and unordered facts with ephemeral, ordered,
source-classified block observations.

**Why.** A correct model cannot repair circular or unauthenticated inputs later.

**Prerequisites.** Goal 0.

**Expected files/modules.** Existing `l1_facts.rs`, `l2_facts.rs`, and `lib.rs`, or renamed
`observe/l1.rs` and `observe/l2.rs`; focused adapter tests.

**In scope.**

- Validate the protocol-defined first `advanceTempo` and optional unique-final finalization system
  envelopes without expanding into incidental SPF header/execution checks.
- Canonically decode ABI calldata with no trailing bytes and canonical-RLP round trips; bound
  dynamic arrays and byte strings before allocation.
- Compare `TempoAdvanced` only as output.
- Exact imported-header identity, receipt-root, and logs-bloom verification.
- Ordered successful Portal logs.
- Selective transaction fetching and trusted binding checks for direct `submitBatch` and non-empty
  `processWithdrawals`.
- Nested-call detection, malformed authenticated-data classification, and retryable acquisition
  error classification.
- Ordered L2 transaction/log outcomes, containing transaction hashes, finalization calldata, and
  the fixed exact-hash Zone commitment/supply outputs from section 7.1.
- Concrete observation types that keep authenticated model inputs distinct from authenticated
  implementation outcomes, with construction owned by the authenticating adapters rather than a
  generic relabeling wrapper.

**Out of scope.** Mutable model state, persistence, lifecycle decisions, and generic retry loops.

**Constraints.** No normalized observation is persisted. Do not fetch all L1 transaction bodies or
recompute the transaction root. Do fetch all receipts required for the receipt root.

**Required tests/evidence.**

- A forged/mismatched `TempoAdvanced` cannot select a different L1 block.
- Wrong header hash/number, receipt root, logs bloom, cardinality, tx metadata, or Portal address is
  rejected in the correct error class.
- Reordered same-block config/operation logs retain exact order.
- A direct call decodes; a nested eventful call produces `UnsupportedNestedPortalCall`.
- Every known non-model-driving event decodes without changing the candidate model; a malformed
  known event and an unknown topic from a protocol emitter fail closed; arbitrary non-protocol logs
  do not.
- A `DepositRejected` log from the pinned native Inbox produces `UnsupportedProtocolEvent` rather
  than selecting the failed-deposit branch.
- Empty `processWithdrawals` has no required transaction fetch.
- Wrong system caller/destination/position, trailing ABI bytes, noncanonical RLP, finalization count
  or block number mismatch, and malformed dynamic lengths fail in the correct error class.
- Exact-state output acquisition never falls back to `latest` or a zero/default value.

**Validation.** `cargo test -p zone-checker` and `cargo fmt --check`.

**Acceptance gate.** Every later model input has a documented authentication/trust source, no
implementation output chooses its own input, and all missing data is explicit.

### Goal 2 — Implement the pure Portal and deposit-prefix model

**Goal.** Model Portal creation/config/token enablement, queue construction, and any contiguous
Zone import prefix using only Goal 0 primitives.

**Why.** Deposits and bounce-back deposits share one queue; this establishes their common identity
and ordering foundation.

**Prerequisites.** Goals 0-1.

**Expected files/modules.** Model state/transition modules and pure scenario tests. Bootstrap I/O is
deferred to Goal 9.

**In scope.**

- Portal identity and creation transition.
- Portal/Zone token enable order and zero initial supply.
- `BouncebackGasUpdated` ordered config.
- Ordinary `DepositMade` and withdrawal-bounce-back queue appends with expected deposit numbers and
  commitments.
- Pending ordered deposit records.
- Empty, partial, and full contiguous prefix consumption.
- Exactly one type-correct Zone outcome per consumed deposit.
- Successful ordinary mint, failed ordinary deposit owner transfer, successful withdrawal
  bounce-back mint, and Inbox refund owner transfer.

**Out of scope.** User withdrawal acceptance, batching, Portal withdrawal slots, DB, and ExEx apply.

**Constraints.** One prefix algorithm only. Do not encode current full-catch-up behavior as a mode
or special transition.

**Required tests/evidence.**

- Deposits appended over multiple Tempo blocks and consumed over multiple Zone prefixes.
- Prefix cannot skip, reorder, duplicate, or consume an unknown record.
- Full catch-up is the same transition with an empty suffix.
- Ordinary deposit and withdrawal bounce-back outcomes cannot be interchanged.
- A failed ordinary deposit creates a special withdrawal identity with all zero rules and no
  fallback owner.
- Same-block Portal config ordering survives candidate application.

**Validation.** `cargo test -p zone-checker model` and `cargo fmt --check`.

**Acceptance gate.** The pure model independently reproduces all deposit queue/cursor commitments
for the scenario matrix, and every consumed deposit is terminal or has exactly one next owner.
This goal is not deployable coverage.

### Goal 3 — Implement pure Zone withdrawals and finalization

**Goal.** Model accepted user withdrawals, config ordering, supply burns, fallback ownership, and
exact batch construction.

**Why.** Settlement cannot be checked from Portal events unless the checker first derives each
withdrawal preimage from Zone history.

**Prerequisites.** Goals 0-2.

**In scope.**

- Ordered `TempoGasRateUpdated` and `MaxWithdrawalsPerBlockUpdated` transitions, including the
  exact zero-disabled, non-resetting within-block cap counter from section 2.3.
- User withdrawal index, sender, containing transaction hash, sender tag, fee, nonzero monotonic
  nonce, recipient-free fallback owner, and `S/W` changes.
- Failed-deposit special withdrawals in the same pending queue without nonce-map insertion.
- Finalization position/count/index checks.
- In-place `Withdrawal(index)` transition from `Pending` to `Finalized` using opaque
  `encryptedSenders` inputs.
- Empty and non-empty batch commitments and batch block/deposit transitions.
- Exactly-once membership in one immutable contiguous batch range.

**Out of scope.** Portal submission/processing, refund claims, persistence, and arbitrary encrypted
sender validity.

**Constraints.** The same pending withdrawal queue handles both origin kinds, but their identity
types remain distinct. A containing `advanceTempo` hash must never leak into a failed-deposit
sender tag.

**Required tests/evidence.**

- Multiple config changes and withdrawals in one block use the rate and cap active at each log.
- Cap vectors cover zero-disabled withdrawals, nonzero enforcement, nonzero -> zero -> nonzero
  toggles without counter reset, and the failed-deposit exemption.
- User and sponsored-fee burns both reduce total supply by principal plus fee.
- Nonzero nonce uniqueness, overflow, recipient-free fallback ownership, and duplicate owner
  failures.
- Mixed user/failed-deposit batch fixed vector.
- Empty batch advances batch index but has zero queue hash.
- Missing/extra `encryptedSenders`, shape-incompatible reordering, and wrong final transaction
  fail. Same-shape ciphertexts remain opaque and are not independently order-authenticated.

**Validation.** `cargo test -p zone-checker model` and `cargo fmt --check`.

**Acceptance gate.** Every accepted Zone withdrawal is burned and owned exactly once, and every
finalized withdrawal/range commitment is independently reproducible. This goal is not deployable
coverage.

### Goal 4 — Close Portal settlement and both refund paths

**Goal.** Model direct batch submission, FIFO full/partial Portal processing, all delivery outcomes,
and both refund maps/claims.

**Why.** This closes the value lifecycle logically; without it, deposit or withdrawal value can
leave the model through an untracked edge.

**Prerequisites.** Goals 0-3.

**In scope.**

- Match each direct `submitBatch` calldata/event to the next finalized batch.
- Validate block/deposit transitions, Zone height, batch index, empty/non-empty queue behavior,
  logical slot/head/tail/capacity, and exactly-once submission.
- Validate direct non-empty `processWithdrawals` preimages against the current FIFO slot.
- Full and partial slot processing and exact empty-array no-op.
- Successful user delivery; failed user delivery to exactly one Portal bounce-back deposit.
- Direct failed-deposit refund and pending Portal refund using active same-block bounce-back gas,
  authenticated imported-header base fee, ceiling division, and amount cap.
- Per-origin Portal and Inbox refund credits; aggregate claim amount and owner closure.
- Remove fallback owner on successful delivery or bounce-back import, at the correct edge.

**Out of scope.** Predicting arbitrary callback/transfer/mint success, MDBX, ExEx recovery, and
collateral RPC.

**Required tests/evidence.**

- Multiple finalized batches, including empty batches and the 100-slot boundary.
- Submission omission, duplication, reordering, and wrong transition failures.
- Withdrawal slot processed in several calls; bad suffix/preimage/order failures.
- `processWithdrawals([], random_nonzero)` leaves every field byte-for-byte unchanged.
- Same-block `BouncebackGasUpdated` before and after processing.
- Bounce-back fee derivation covers base-fee rounding and capping at the withdrawal amount.
- Direct refund, pending Portal refund then claim, Zone bounce-back mint, pending Inbox refund then
  claim.
- Bounce-back outcome recipients key their observed mint/refund branches without being compared to
  an unavailable withdrawal-time `zoneFallbackRecipient`.
- Mixed refund origins for one `(token, recipient)` aggregate all close on one claim.
- Nested eventful model-driving calls freeze as unsupported.

**Validation.** `cargo test -p zone-checker model` and `cargo fmt --check`.

**Acceptance gate.** Run the lifecycle closure table mechanically over scenario tests: every
created identity is terminal or has exactly one open owner and every unit is represented in the
correct `S/D/W` bucket. Persistence/recovery is still missing, so this is not a shadow release.

### Goal 5 — Add exact accounting and implementation-output adapters

**Goal.** Connect the complete pure model to authenticated observations and exact supply/collateral
checks without persistence.

**Why.** The model is useful only if it compares independent expectations at the correct chain
cuts.

**Prerequisites.** Goals 0-4.

**In scope.**

- Build imported/Zone model inputs from Goal 1 observations.
- Execute the post-L1/pre-Zone collateral cut.
- Exact-hash literal-slot-8 local supply reads after the Zone block.
- Exact-hash local reads of `TempoState` hash/number, Inbox processed deposit hash/number, and
  Outbox last-batch queue hash/index after the Zone block.
- One exact-block Portal `balanceOf` call per enabled token.
- Compare all exposed queue, cursor, identity, batch, and event outputs.
- Dedicated typed findings; remove the one-off invariant-registry direction rather than expanding
  it.
- Metrics for acquisition latency, transition latency, supply/collateral calls, model lag, and
  pass/failure counts sufficient to justify deferred optimizations.

**Out of scope.** Durable state, generic invariant framework, proof generation, and RPC batching
unless the simplest existing provider API already supplies it without a new abstraction.

**Required tests/evidence.**

- End-to-end in-memory scenarios for every lifecycle branch.
- Exact pre-Zone collateral cut catches a deficit without including same-block Zone mints/burns.
- Exact post-Zone supply catches an unauthorized mint/burn.
- Each fixed Zone commitment read catches a correct event paired with incorrect stored state; the
  expected value still comes from the logical model, never the observed event.
- Passing supply/collateral values leave no durable artifact.
- RPC/local-state failure is acquisition failure; authenticated mismatch is a finding.
- Checked `S + D + W` overflow/underflow behavior.

**Validation.** `cargo test -p zone-checker`, `cargo fmt --check`, and targeted clippy for the checker
crate if the repository toolchain supports it.

**Acceptance gate.** A complete in-memory replay detects injected queue, lifecycle, supply, and
collateral errors while passing all valid branches. No expected value is generated by a production
transition helper. This remains non-deployable because restart/reorg state is absent.

### Goal 6 — Implement minimal checker MDBX persistence

**Goal.** Materialize exactly the schema in section 8 using pinned Reth database APIs.

**Why.** Restart, efficient unwind, and crash boundaries require durable authoritative state, but
raw chain duplication does not.

**Prerequisites.** Goals 0-5.

**Expected files/modules.** A cohesive `store` module, table/key/value codecs, environment opening,
and storage tests; checker/node wiring only as needed to provide a data directory.

**In scope.**

- Dedicated checker MDBX directory and `CheckerTables`.
- Five tables and concrete typed codecs.
- DB identity/model-version validation.
- Current model load/write and prefix scans.
- Exact first-before-image capture and canonical/tip update in one transaction.
- Read-only reconstruction of model state and tips at an arbitrary retained canonical Zone height,
  without mutating the authoritative database.
- Compact findings and active-alert metadata primitives.
- Bootstrap cursor primitives.
- Database consistency checker for tip/canonical/model/changeset relationships.

**Out of scope.** Live ExEx acknowledgement changes, reorg orchestration, migrations, raw evidence,
custom WAL/static files/ETL.

**Constraints.** No direct libmdbx. No DUPSORT. No network/provider call in a store transaction.
One writer. Unknown tags/versions fail closed.

**Required tests/evidence.**

- Golden encoded bytes and round trips for every key/value family.
- Big-endian ordering across block/ordinal boundaries.
- Put/update/delete captures one exact pre-block value per key.
- Atomic commit/abort tests leave either parent or child state, never a mixture.
- Reconstructing several arbitrary historical targets is byte-identical to fresh archive replay at
  those targets, including a target before a later-terminal record was deleted.
- A missing/conflicting changeset or canonical hash fails reconstruction explicitly rather than
  returning partial state.
- Prefix refund scan and changeset reverse walk.
- Wrong DB identity/version and corrupted/trailing bytes fail without mutation.

**Validation.** `cargo test -p zone-checker store`, full checker tests, and `cargo fmt --check`.

**Acceptance gate.** A process can close/reopen the DB and recover exactly the verified model/tips;
the schema contains only approved state, before-images, and findings.

### Goal 7 — Make apply, restart, and acknowledgement atomic

**Goal.** Put the live ExEx on the acquire -> pure transition -> commit -> acknowledge path.

**Why.** The DB and `FinishedHeight` must have an unambiguous crash relationship.

**Prerequisites.** Goals 0-6.

**In scope.**

- Sole-writer run loop.
- Short model read, all async acquisition, pure candidate, exact checks, one write commit,
  post-commit acknowledgement.
- Canonical parent/tip recheck before writes.
- Duplicate notification and commit-before-ack idempotence.
- Bounded acquisition retry and unhealthy status without defaulting data.
- Startup load from current state rather than genesis.
- Fault injection at every boundary in section 9.2.

**Out of scope.** Full reorg orchestration and alert-descendant behavior (Goal 8), historical
bootstrap (Goal 9), and enforcement.

**Required tests/evidence.**

- Crash/abort before commit replays safely.
- Committed block with unsent acknowledgement is not applied twice.
- Duplicate notification no-ops only when hash matches.
- No test/provider hook observes a network call while a write transaction is open.
- Parent-tip conflict cannot overwrite state.
- Acquisition failure commits neither model nor finding and does not acknowledge the gap.

**Validation.** Full checker tests, relevant node integration tests, `cargo fmt --check`, and targeted
clippy.

**Acceptance gate.** Every simulated crash boundary converges to exactly one applied logical
transition and at-least-once/idempotent acknowledgement behavior.

### Goal 8 — Implement exact reorgs and alert-triggered mode

**Goal.** Restore old forks with before-images and keep the core node operational after actual
divergence while preserving a frozen verified model.

**Why.** Reorg and divergence are different state transitions and must not share ambiguous
best-effort behavior.

**Prerequisites.** Goals 0-7.

**In scope.**

- Commit, revert-only, and reorg notification orchestration.
- Descending exact unwind and ascending normal apply.
- Finding canonical/orphan status.
- First-divergence atomic finding + active alert.
- Critical logs, sticky metrics, unhealthy readiness/status.
- Continued ExEx acknowledgement in alert mode without descendant checking.
- Startup and reorg recovery when an alerting block remains or is removed.
- Version-incompatible event behavior and nested-call behavior, with no invented version signal.

**Out of scope.** Enforcement, auto-repair of a canonical divergence, and custom acknowledgement
cursors absent API evidence.

**Required tests/evidence.**

- One-block, multi-block, deep, and revert-only unwind restore exact parent bytes.
- Reorg applies old-out newest-first and new-in oldest-first.
- Divergent block commits no candidate state, persists one finding, freezes tip, and acknowledges
  descendants.
- Restart in alert mode remains frozen and does not refetch/check descendants.
- Reorg removing the first divergent block orphans the finding, clears alert, and applies the
  replacement from the verified parent.
- Reorg below the frozen parent unwinds verified state first.
- Missing/conflicting changeset fails as corruption; no partial repair.

**Validation.** Full checker/node integration tests, `cargo fmt --check`, and targeted clippy.

**Acceptance gate.** All failure/recovery cases in sections 9 and 11 have executable tests, and an
actual finding never affects core-node progress after alert mode commits.

### Goal 9 — Add archive bootstrap, resumable backfill, and rebuild

**Goal.** Produce the same current model from configured archive histories as live processing,
starting from authenticated Portal creation.

**Why.** Genesis is not proof of empty Portal state, and model upgrades/repair cannot depend on
checker raw evidence.

**Prerequisites.** Goals 0-8.

**In scope.**

- Required `portal_creation_block_hash` configuration and CLI wiring.
- Exact factory creation authentication and database identity.
- Portal creation-to-genesis-anchor replay, including the creation-after-anchor development case.
- Ordered Zone genesis-to-head replay through the same transition used live.
- Per-L1-block bootstrap commits/cursor and per-Zone-block ordinary commits.
- Catch-up to live notifications without a gap/double apply.
- Fresh-path rebuild command/runbook for model-version or corruption recovery.
- Explicit failures for missing/pruned L1 or local L2 archive history.

**Out of scope.** Automatic discovery, in-place migration, remote L2 supply fallback, raw evidence
download, and speculative bulk import.

**Required tests/evidence.**

- Portal with deposits/config before Zone genesis reconstructs correctly.
- Current dev creation-after-anchor flow reconstructs correctly.
- Bootstrap rejects a zero TempoState genesis checkpoint as `UnsupportedBootstrapStyle`.
- Tampered or non-parent-linked creation-to-anchor history and wrong receipt roots fail
  authentication.
- Crash at every bootstrap cursor resumes at the next unapplied L1 block.
- Live-built and archive-rebuilt DBs have identical authoritative model/tip bytes (changeset
  histories may be compared where deterministic).
- Pruned/missing history fails explicitly and never creates default records.
- Unknown model version preserves old DB and instructs a fresh path.

**Validation.** Full checker tests plus an end-to-end local L1/L2 archive fixture or existing dev
stack test, `cargo fmt --check`, and targeted clippy.

**Acceptance gate.** A fresh DB reaches the same verified head as uninterrupted live processing,
normal restart performs no genesis replay, and every crash resumes from durable progress.

### Goal 10 — Closed-system shadow-release gate

**Goal.** Demonstrate that the integrated checker is operationally safe and covers the complete
release-one lifecycle, then update operator documentation to enable shadow deployment.

**Why.** No prior goal alone satisfies the closed-system release boundary.

**Prerequisites.** Goals 0-9.

**In scope.**

- One end-to-end matrix covering every lifecycle row and both terminal alternatives.
- Injected common-mode errors in queue ordering, IDs, zero/nonzero nonce semantics, batch
  membership, partial suffixes, fees, withdrawal caps, refunds, stored Zone commitments, supply,
  and collateral.
- Bootstrap, restart, duplicate replay, acquisition failure/recovery, every crash boundary,
  revert/reorg, divergence descendants, divergence-removing reorg, and fresh rebuild.
- Checker mode/config/docs, health, critical logs, metrics, DB path/size, archive requirements,
  trust shortcuts, and recovery runbook.
- A read-only diagnostic command or equivalent operator path that reconstructs model state at a
  retained canonical height and shows a selected key's before/after changes with source block
  coordinates for archive evidence lookup.
- Baseline performance measurements: live p50/p95 block latency, receipt-fetch time, per-token read
  time, MDBX transaction time, catch-up rate, model/open-record size, and changeset bytes/block.
- Remove or rewrite obsolete README claims about temporary facts/staged invariant coverage.

**Out of scope.** Enforcement and all release-one non-goals in section 14.

**Required validation.**

- `cargo test -p zone-checker`.
- Relevant `zone-node` checker CLI/integration tests.
- `cargo fmt --check`.
- Targeted clippy for changed crates with repository-standard flags.
- The repository's smallest existing end-to-end Zone dev test that exercises L1 imports.
- A documented benchmark/catch-up run; no unmeasured optimization is required to pass.

**Acceptance gate.** The checker may be shadow deployed only when:

1. every lifecycle identity reaches a verified terminal or one durable open owner;
2. every `S/D/W` transition, fixed Zone commitment, supply comparison, and collateral comparison
   is covered;
3. the complete failure/recovery audit passes;
4. no production transition helper generates expected outputs;
5. alert mode demonstrably does not affect core-node progress;
6. archive and trust assumptions are explicit in operator docs;
7. no earlier partial milestone is represented as equivalent coverage.

## 14. Release boundary and explicit non-claims

Release one claims to detect logical/specification divergence in:

- Portal deposit queue identities, order, and commitments;
- contiguous-prefix Zone imports;
- exactly-once deposit outcomes;
- ordinary versus failed-deposit withdrawal identity;
- accepted user withdrawal fee, burn, per-block cap, nonce, sender tag, and fallback ownership;
- exactly-once batch membership/submission;
- Portal FIFO and partial processing;
- delivery-to-bounce-back lifecycle movement;
- both pending refund maps and claims;
- exact `TempoState`, Inbox processed-deposit, and Outbox last-batch Zone state commitments;
- per-token expected Zone supply and exact actual supply;
- Portal collateral lower bound;
- restart, replay, crash, ordinary/deep reorg, and divergence-removing reorg behavior.

Release one does **not** claim to prove:

- arbitrary Portal storage integrity beyond authenticated events/call inputs and exact balance;
- the L1 transaction root or proof-bind selectively fetched Portal calldata;
- Chaum-Pedersen, AES-GCM, or encrypted-sender validity;
- the private recipient of a successful deposit mint;
- that a bounce-back mint/refund recipient equals the withdrawal-time `zoneFallbackRecipient`;
  recipient misdirection with otherwise correct nonce, token, amount, lifecycle, and accounting is
  therefore not detected;
- correctness of arbitrary EVM transfer/callback/policy outcomes;
- proof-system or threshold-certificate cryptography already enforced by Portal execution;
- independent consensus finality, liveness, censorship resistance, or data availability;
- zero-checkpoint genesis and its arbitrary-first-Tempo-import bootstrap exception;
- arbitrary EVM re-execution;
- enforcement or prevention of an invalid canonical block.

The checker trusts:

- the in-process core node for canonical Zone blocks/receipts and exact-hash Zone state;
- the configured L1 archive RPC for selectively fetched transaction binding, exact-block Portal
  balance calls, and data availability;
- archive availability for repair, upgrades, and detailed diagnostics.

## 15. Test strategy

### 15.1 Independent unit vectors

Fixed byte-level vectors are the primary expected-value authority. Include queue folds, ABI
preimages, sender tags, special zero identities, fees, sentinels, storage keys, and codecs.
Production differential tests are secondary and must fail if the independent vector is removed.

### 15.2 Pure state-machine scenarios

Use table-driven scenarios that assert before owner, transition, after owner, terminal/open status,
and exact `S/D/W`. Include invalid duplicate, missing, reordered, wrong-kind, overflow, and
underflow transitions.

### 15.3 Adapter/authentication tests

Use synthetic headers/receipts/transactions to mutate one trust-boundary field at a time. Verify
the distinction between retryable acquisition failures, authenticated findings, and unsupported
behavior.

### 15.4 Persistence/fault tests

Inject failures before commit, after commit, before acknowledgement, during multi-block unwind,
and during bootstrap cursor updates. Reopen the DB after each injection and assert exact bytes/tips,
not only high-level counts.

### 15.5 End-to-end lifecycle matrix

At minimum, cover:

- ordinary deposit -> mint;
- ordinary deposit -> failed-deposit withdrawal -> direct Tempo refund;
- ordinary deposit -> failed-deposit withdrawal -> Portal refund -> claim;
- user withdrawal -> successful delivery;
- user withdrawal -> bounce-back -> Zone mint;
- user withdrawal -> bounce-back -> Inbox refund -> claim;
- mixed tokens and mixed origin kinds in one batch;
- deposit import empty/partial/full prefixes;
- Portal withdrawal empty/partial/full processing;
- same-block config-before and config-after cases;
- withdrawal-cap zero/nonzero toggles and failed-deposit exemption;
- correct events paired with incorrect stored TempoState, Inbox, or Outbox commitments;
- unauthorized mint/burn supply injection and Portal collateral deficit;
- restart/rebuild/reorg at each open-owner phase.

### 15.6 Property tests

Property tests are valuable for queue folds and transition sequences after fixed vectors exist:

- folding append records then consuming any prefix yields the expected intermediate commitment;
- splitting Portal processing into arbitrary non-empty prefixes yields the same final state as one
  full call;
- apply then unwind restores byte-identical model state;
- reconstructing any retained canonical target from the current state and reverse changesets is
  byte-identical to direct replay through that target;
- aggregate `S/D/W` equals the sum of open/terminal scenario ownership after every generated valid
  transition;
- no valid transition leaves an origin with zero or multiple owners.

Do not use property tests as a substitute for independent literal vectors.

## 16. Resolved decisions and implementation-time confirmations

There are no open architecture decisions for release one. The following are locked:

1. Deposit processing is one contiguous-prefix rule; full catch-up is an empty-suffix case.
2. Model-driving Portal settlement calls are direct; selective calldata binding trusts L1 RPC.
3. Bootstrap requires configured Portal creation block hash and authenticated `ZoneCreated`.
4. Zone supply uses local exact-hash literal slot 8 without MPT proofs.
5. Persistence is current state, canonical progress, before-images, and compact findings only.
6. Actual divergence freezes the model but continues ExEx acknowledgements.
7. Exact-block Portal collateral is checked at the post-L1/pre-Zone cut.
8. Any model/format change rebuilds a fresh DB; there are no in-place migrations.
9. Event handling uses a literal version-pinned allowlist; unknown protocol-emitter events fail
   closed, while the pinned native Inbox does not support sequencer-rejected deposits.

Goals 0, 1, and 6 must confirm a few pinned-source mechanics before coding through them, but these
are source checks rather than product choices:

- pin the generated initialization source proving literal zero initial `tempo_gas_rate` and
  `max_withdrawals_per_block`, alongside Portal's explicit zero `bounceback_gas` default;
- record literal exact-state access vectors for `TempoState` hash/number, Inbox processed deposit
  hash/number, and Outbox last-batch queue hash/index;
- confirm the node data-directory path passed to the ExEx and the dedicated checker DB geometry;
- confirm Reth's persisted ExEx head behavior with a commit-before-ack restart test;
- re-audit ABI/storage vectors whenever pinned Tempo or Reth revisions change.

If a confirmation contradicts this design, stop the active `/goal` with source evidence and request
an explicit design revision. Do not introduce a silent fallback, duplicate semantic mode, generic
framework, or production-helper dependency to work around it.
