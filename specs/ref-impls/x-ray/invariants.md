# Invariant Map

> Tempo Zones (ref-impls) | 24 guards | 21 inferred | 12 not enforced on-chain

> ⚠️ **Trust-model caveat that colours every "On-chain: No" below.** `Verifier.verify()` in [Verifier.sol:13-31](../src/zone/Verifier.sol#L13) is a stub that **always returns `true`**. Every batch-level property (deposit-processing correctness, withdrawal-hash correctness, block transitions, escrow solvency) is therefore enforced *only* by a trusted sequencer in this reference implementation, not by a real proof. Treat the sequencer as fully trusted until a real verifier ships.

---

## 1. Enforced Guards (Reference)

Per-call preconditions. Heading IDs (`G-N`) are anchor targets from x-ray.md.

#### G-1
`if (msg.sender != sequencer) revert NotSequencer();` · `ZonePortal.sol:161` · Restricts batch submission, withdrawal processing, gas-rate and key updates to the single trusted L1 sequencer — the root of the whole trust model.

#### G-2
`if (msg.sender != admin) revert NotAdmin();` · `ZonePortal.sol:166` · Gates the token registry (enable/pause/resume); admin and sequencer are deliberately separated powers.

#### G-3
`if (msg.sender != pendingSequencer) revert NotPendingSequencer();` · `ZonePortal.sol:183` · Enforces two-step sequencer handover so a mistyped address cannot brick block production.

#### G-4
`if (_zoneGasRate > MAX_GAS_FEE_RATE) revert GasFeeRateTooHigh();` · `ZonePortal.sol:196` · Bounds the L1 deposit-fee rate so `FIXED_DEPOSIT_GAS * zoneGasRate` cannot overflow `uint128`. Lifted to I-1.

#### G-5
`if (_tokenConfigs[_token].enabled) revert TokenAlreadyEnabled();` · `ZonePortal.sol:251` · Keeps token enablement a one-shot latch; re-enabling would re-grant approval and duplicate the `_enabledTokens` entry. Lifted to I-7.

#### G-6
`if (!ITIP20Factory(...).isTIP20(_token)) revert TokenNotEnabled();` · `ZonePortal.sol:252` · Only genuine TIP-20s can be escrowed, so deposit/withdraw token semantics hold.

#### G-7
`if (!cfg.enabled) revert TokenNotEnabled(); if (!cfg.depositsActive) revert DepositsNotActive();` · `ZonePortal.sol:448-449` · Deposits require an enabled + unpaused token; withdrawals deliberately omit the `depositsActive` check (non-custodial guarantee). Anchors I-8.

#### G-8
`if (bouncebackRecipient == address(0)) revert InvalidBouncebackRecipient();` · `ZonePortal.sol:517` · Every deposit must name a refund address so a failed zone-side credit can always bounce back.

#### G-9
`if (amount < fee + bouncebackFee) revert DepositTooSmall();` · `ZonePortal.sol:481` · Guarantees the deposit can pay both the sequencer fee and a reserved bounce-back fee; underpins fee conservation E-1.

#### G-10
`if (recovered == address(0) || recovered != expected) revert InvalidProofOfPossession();` · `ZonePortal.sol:339` · Proof-of-possession stops the sequencer registering an encryption key it cannot decrypt with (liveness protection).

#### G-11
`if (encrypted.ciphertext.length != ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE) revert InvalidCiphertextLength(...);` · `ZonePortal.sol:591` · Fixes ciphertext size to block DoS via oversized AES-GCM work on the zone side.

#### G-12
`if (blockTransition.prevBlockHash != blockHash) revert InvalidProof();` · `ZonePortal.sol:807` · Batch must chain onto the last committed zone block hash — L1 ordering guard. Anchors I-15.

#### G-13
`if (tempoBlockNumber < genesisTempoBlockNumber) revert InvalidTempoBlockNumber();` · `ZonePortal.sol:812` · Batches cannot anchor to a Tempo block older than the zone's genesis.

#### G-14
`if (anchorBlockHash == bytes32(0)) revert InvalidTempoBlockNumber();` · `ZonePortal.sol:841` · Rejects EIP-2935 misses (block outside the 8192 window) so a batch cannot anchor to an unknown hash.

#### G-15
`if (!valid) revert InvalidProof();` · `ZonePortal.sol:857` · The single gate that *should* enforce all batch correctness — currently defanged by the stub verifier (see top caveat). Anchors X-4 / E-2.

#### G-16
`if (msg.sender != address(0) && msg.sender != config.sequencer()) revert OnlySequencer();` · `ZoneOutbox.sol:397` · Zone-side sequencer gate; `address(0)` is the system caller for enshrined system transactions.

#### G-17
`if (msg.sender != ZONE_INBOX) revert OnlyZoneInbox();` · `ZoneOutbox.sol:331` · Only the inbox may enqueue a failed-deposit bounce-back, so bounce-backs cannot be forged by users.

#### G-18
`if (blockNumber != uint64(block.number)) revert InvalidBlockNumber();` · `ZoneOutbox.sol:398` · Batch finalization is pinned to the current block, preventing replay/stale finalization.

#### G-19
`if (encryptedSenders.length != count) revert InvalidEncryptedSenderCount(...);` · `ZoneOutbox.sol:406` · One sender ciphertext per finalized withdrawal — keeps the reveal array aligned with the batch.

#### G-20
`if (gasLimit > MAX_WITHDRAWAL_GAS_LIMIT) revert GasLimitTooHigh();` · `ZoneOutbox.sol:486` · Caps callback gas at request time so over-cap withdrawals never enter the L2 queue. Lifted to I-3.

#### G-21
`if (!zoneToken.transferFrom(msg.sender, address(this), totalBurn)) revert TransferFailed();` · `ZoneOutbox.sol:279` · Withdrawal must collect `amount + fee` before the burn, so burned supply is always backed by collected user funds.

#### G-22
`if (msg.sender != ZONE_INBOX) revert OnlyZoneInbox();` · `TempoState.sol:90` · Only the inbox can advance the mirrored Tempo header, the single writer of zone↔Tempo state.

#### G-23
`if (tempoParentHash != prevBlockHash) revert InvalidParentHash(); if (tempoBlockNumber != prevBlockNumber + 1) revert InvalidBlockNumber();` · `TempoState.sol:100-101` · Strict parent-hash + height+1 continuity for the mirrored Tempo chain. Anchors I-14.

#### G-24
`require(portal == predictedPortal, "Portal address mismatch - nonce tracking error");` · `ZoneFactory.sol:105` · Guarantees the messenger was built with the real portal address (CREATE-nonce prediction), or deployment reverts.

---

## 2. Inferred Invariants (Single-Contract)

Categories: `Conservation` · `Bound` · `Ratio` · `StateMachine` · `Temporal`.

---

#### I-1

`Bound` · On-chain: **Yes**

> `zoneGasRate ∈ [0, MAX_GAS_FEE_RATE]` (1e18) at all times.

**Derivation** — guard-lift of G-4 (`ZonePortal.sol:196`). Write sites of `zoneGasRate`: only `setZoneGasRate` (`ZonePortal.sol:197`) and zero-init; the single mutating writer enforces the bound.

**If violated** — `calculateDepositFee` (`FIXED_DEPOSIT_GAS * zoneGasRate`) could overflow `uint128`, breaking deposit pricing.

---

#### I-2

`Bound` · On-chain: **Yes**

> `tempoGasRate ∈ [0, MAX_GAS_FEE_RATE]` (1e18) at all times.

**Derivation** — guard-lift of `if (_tempoGasRate > MAX_GAS_FEE_RATE)` (`ZoneOutbox.sol:131`). Sole mutating write site `setTempoGasRate` (`ZoneOutbox.sol:132`).

**If violated** — `(WITHDRAWAL_BASE_GAS + gasLimit) * tempoGasRate` could overflow `uint128`, breaking withdrawal fee math.

---

#### I-3

`Bound` · On-chain: **Yes**

> Every stored pending withdrawal has `gasLimit <= MAX_WITHDRAWAL_GAS_LIMIT`.

**Derivation** — guard-lift of G-20 (`ZoneOutbox.sol:486`). Write sites that push to `_pendingWithdrawals`: `_requestWithdrawal` (validates via `_validateGasLimit`, `ZoneOutbox.sol:248`) and `enqueueDepositBounceBack` (hardcodes `gasLimit: 0`, `ZoneOutbox.sol:342`). Both satisfy the bound.

**If violated** — a withdrawal whose callback cannot fit an L1 block would enter the queue; L1 `processWithdrawal` defensively re-checks and bounces (`ZonePortal.sol:676`).

---

#### I-4

`Bound` · On-chain: **Yes**

> Stored callback data length `<= MAX_CALLBACK_DATA_SIZE` (1024) for every pending withdrawal.

**Derivation** — guard-lift of `if (data.length > MAX_CALLBACK_DATA_SIZE) revert CallbackDataTooLarge();` (`ZoneOutbox.sol:251`). Only `_requestWithdrawal` writes user `callbackData`; the bounce-back path writes `""`.

**If violated** — unbounded storage/hash cost during batch finalization (DoS).

---

#### I-5

`Temporal` · On-chain: **Yes**

> `withdrawalBatchIndex` (both L1 `ZonePortal` and zone `ZoneOutbox`) is strictly monotonically increasing.

**Derivation** — `withdrawalBatchIndex++` in `submitBatch` (`ZonePortal.sol:860`) and `withdrawalBatchIndex += 1` in `_finalizeWithdrawalBatch` (`ZoneOutbox.sol:455`); no other writers, no decrement path.

**If violated** — batch replay or gap; the two indices are meant to advance in lock-step across domains.

---

#### I-6

`Temporal` · On-chain: **Yes**

> `depositCount` and `nextWithdrawalIndex` are strictly increasing 1-indexed / 0-indexed counters; never reset.

**Derivation** — `++depositCount` (`ZonePortal.sol:496`, `:781`) and `nextWithdrawalIndex++` (`ZoneOutbox.sol:305`, `:349`); only-increment, no other writers.

**If violated** — duplicate deposit/withdrawal numbering would break user tracking and proof contiguity.

---

#### I-7

`StateMachine` · On-chain: **Yes**

> `TokenConfig.enabled` is a one-shot latch: once `true` it never returns to `false`; `_enabledTokens` is append-only.

**Derivation** — edge: `require(!_tokenConfigs[_token].enabled)` (G-5) → `_tokenConfigs[_token] = TokenConfig({enabled:true,...})` at `ZonePortal.sol:275`; no code path sets `enabled = false` or pops `_enabledTokens`. (`depositsActive` is separately togglable and is NOT part of this latch.)

**If violated** — a token could be un-enabled while escrow/zone supply exists, breaking the non-custodial guarantee.

---

#### I-8

`StateMachine` · On-chain: **Yes**

> Withdrawals are never blocked for an enabled token, even while deposits are paused.

**Derivation** — `pauseDeposits` only sets `depositsActive = false` (`ZonePortal.sol:262`); the withdrawal path (`processWithdrawal`) and zone `requestWithdrawal` check only `enabled`/`isEnabledToken`, never `depositsActive` (G-7 omits it on the withdrawal side).

**If violated** — admin could trap user funds in escrow (custodial), contradicting the stated guarantee.

---

#### I-9

`Ratio` · On-chain: **Yes**

> `depositFee == FIXED_DEPOSIT_GAS * zoneGasRate`.

**Derivation** — `calculateDepositFee` (`ZonePortal.sol:434`); `FIXED_DEPOSIT_GAS` is a `constant`.

**If violated** — fee accounting in `_collectDepositFunds` (and the bounce-back reserve check) would desync.

---

#### I-10

`Ratio` · On-chain: **Yes**

> `bouncebackFee == ceil(FIXED_BOUNCEBACK_GAS * block.basefee / 1e12)`.

**Derivation** — `calculateBouncebackFee` (`ZonePortal.sol:440-443`), rounding up via `(gasFee + SCALE - 1) / SCALE`.

**If violated** — bounce-back refunds could underpay Tempo gas (round-down) and stall.

---

#### I-11

`Ratio` · On-chain: **Yes**

> `withdrawalFee == (WITHDRAWAL_BASE_GAS + gasLimit) * tempoGasRate`.

**Derivation** — `_calculateWithdrawalFee` (`ZoneOutbox.sol:495`); the fee is snapshotted into the pending withdrawal at request time (`ZoneOutbox.sol:271`).

**If violated** — fee charged differs from fee paid to the sequencer on L1, breaking E-1.

---

#### I-12

`Conservation` · On-chain: **Yes**

> The L1 withdrawal ring buffer holds exactly the slots in `[head, tail)`; `length = tail - head <= WITHDRAWAL_QUEUE_CAPACITY` (100).

**Derivation** — Δ-pair: `enqueue` checks `tail - head >= CAPACITY` then `tail = tail + 1` (`WithdrawalQueueLib.sol:55-61`); `dequeue` advances `head` only when a slot is exhausted (`:95`). Monotonic `head`/`tail`, modular slot indexing.

**If violated** — batch overwrite or processing of a non-existent batch.

---

#### I-13

`Conservation` · On-chain: **Yes**

> A withdrawal can only be dequeued from L1 if it hashes to the head slot: `keccak256(abi.encode(withdrawal, expectedRemaining)) == slots[head % CAP]`.

**Derivation** — guard in `WithdrawalQueueLib.dequeue` (`:89`). `processWithdrawal` cannot pay out a withdrawal whose contents were not committed in the batch hash chain.

**If violated** — sequencer could pay an arbitrary recipient/amount not in the proven batch. (Note: the *contents* of the slot still originate from the stub-verified `submitBatch`, so end-to-end correctness is X-4 / On-chain No.)

---

#### I-14

`Temporal` · On-chain: **Yes**

> The mirrored Tempo chain advances by exactly one block with matching parent hash: `tempoBlockNumber == prev + 1 ∧ tempoParentHash == prevBlockHash`.

**Derivation** — temporal/edge G-23 (`TempoState.sol:100-101`), checked after `_decodeAndStoreHeader`.

**If violated** — the zone's view of Tempo could skip or fork, corrupting every `readTempoStorageSlot` (sequencer, keys, token config, deposit queue hash).

---

#### I-15

`Temporal` · On-chain: **Yes**

> Each submitted batch chains onto the previous: `blockTransition.prevBlockHash == blockHash` before `blockHash = blockTransition.nextBlockHash`.

**Derivation** — G-12 (`ZonePortal.sol:807`) then write at `:861`.

**If violated** — two divergent batch histories could be committed on L1.

---

#### I-16

`Temporal` · On-chain: **Yes**

> An old encryption key is accepted for new deposits only while `block.number < nextKey.activationBlock + ENCRYPTION_KEY_GRACE_PERIOD`; the latest key never expires.

**Derivation** — temporal predicate in `isEncryptionKeyValid` (`ZonePortal.sol:414-422`); enforced in `depositEncrypted` via G-/key-validity check (`:598`).

**If violated** — deposits could be encrypted to a rotated-out key the sequencer no longer accepts, forcing bounce-backs.

---

#### I-17

`Conservation` · On-chain: **No**

> `lastProcessedDepositNumber <= depositCount` on L1, and the zone's `processedDepositNumber` mirrors it.

**Derivation** — `depositCount` increments locally (I-6), but `lastProcessedDepositNumber = depositQueueTransition.nextDepositNumber` is taken from sequencer-supplied, stub-verified calldata in `submitBatch` (`ZonePortal.sol:863`). No on-chain check that `nextDepositNumber <= depositCount`.

**If violated** — L1 could mark more deposits processed than were ever enqueued; nothing on-chain rejects it while the verifier is a stub.

---

#### I-18

`Conservation` · On-chain: **No**

> The zone's `processedDepositQueueHash` is a contiguous ancestor of Tempo's `currentDepositQueueHash` (no skipped/duplicated deposits).

**Derivation** — `advanceTempo` computes `currentHash` over supplied `deposits`, reads `tempoCurrentHash`, but the mismatch branch (`ZonePortal`→`ZoneInbox.sol:338-343`) is an **empty `if`** — partial processing is explicitly allowed and contiguity is delegated to the (stub) proof.

**If violated** — the zone could mint for deposits that were never enqueued on L1, or skip enqueued ones. This is the highest-signal gap.

---

#### I-19

`Conservation` · On-chain: **No**

> Per token, escrowed L1 balance in `ZonePortal` ≥ outstanding zone-token supply minted by `ZoneInbox`.

**Derivation** — mint on deposit (`ZoneInbox.sol:233`, `:317`), burn on withdraw (`ZoneOutbox.sol:285`), release from escrow on L1 (`ZoneMessenger.relayMessage` / `processWithdrawal`). The mint↔escrow and burn↔release pairs span two domains; nothing on a single chain checks the sum.

**If violated** — zone tokens become under-collateralized (insolvent bridge). Relies entirely on the trusted sequencer + (stub) proof.

---

#### I-20

`StateMachine` · On-chain: **Yes**

> Sequencer transfer is two-step: `pendingSequencer` is set by the current sequencer, consumed exactly once by `acceptSequencer`, then reset to `address(0)`.

**Derivation** — edge: `transferSequencer` sets `pendingSequencer` (`ZonePortal.sol:177`) → `acceptSequencer` requires `msg.sender == pendingSequencer` (G-3) → `sequencer = pendingSequencer; pendingSequencer = address(0)` (`:185-186`).

**If violated** — an unintended address could seize block production.

---

#### I-21

`Bound` · On-chain: **No**

> At most `maxWithdrawalsPerBlock` `requestWithdrawal` calls succeed per zone block (when the cap is non-zero).

**Derivation** — guard-lift of `if (_withdrawalsThisBlock >= maxWithdrawalsPerBlock) revert` (`ZoneOutbox.sol:263`). On-chain=**No** because the counter resets on `block.number != _currentBlockNumber` (`:259`); `enqueueDepositBounceBack` (a second writer of `_pendingWithdrawals`) bypasses the cap entirely, so the property is "best-effort rate-limit," not a hard bound on queue growth.

**If violated** — bounce-back floods or cap=0 allow unbounded `_pendingWithdrawals` growth within a block.

---

## 3. Inferred Invariants (Cross-Contract)

---

#### X-1

On-chain: **No**

> `ZoneConfig.sequencer()` and the zone-side sequencer checks return the same address that `ZonePortal.sequencer` holds on L1.

**Caller side** — `ZoneConfig.sol:52-55` reads `tempoState.readTempoStorageSlot(tempoPortal, PORTAL_SEQUENCER_SLOT)`; `ZoneOutbox`/`ZoneInbox` gate on `config.sequencer()`.

**Callee side** — `ZonePortal.sequencer` is written at `ZonePortal.sol:185` (and constructor). The L1 storage *slot* of `sequencer` is hardcoded as `PORTAL_SEQUENCER_SLOT` in `IZone.sol`; nothing links the constant to the actual layout at compile time.

**If violated** — a storage-layout drift in `ZonePortal` silently points the zone at the wrong sequencer (or a wrong slot), breaking all zone-side auth.

---

#### X-2

On-chain: **No**

> `ZoneInbox._readEncryptionKey(keyIndex)` and `ZoneConfig.sequencerEncryptionKey()` read the correct `(x, yParity)` for the portal's `_encryptionKeys[keyIndex]`.

**Caller side** — `ZoneInbox.sol:162-171` and `ZoneConfig.sol:73-92` compute `keccak256(abi.encode(PORTAL_ENCRYPTION_KEYS_SLOT)) + keyIndex*2` and read parity from the low byte of the meta slot.

**Callee side** — `ZonePortal._encryptionKeys` (declared slot 7 per the comment at `ZonePortal.sol:108-109`) packs `yParity + activationBlock`. The two-slots-per-entry layout is replicated by hand in two readers.

**If violated** — wrong key → Chaum-Pedersen verification uses a wrong public key → all encrypted deposits fail/bounce, or (worse) a layout change mis-reads a valid-looking key.

---

#### X-3

On-chain: **Yes**

> `ZoneMessenger.relayMessage` can move escrowed tokens from the portal only because the portal granted it `type(uint256).max` approval when the token was enabled.

**Caller side** — `ZoneMessenger.sol:69` `ITIP20(token).transferFrom(portal, target, amount)`.

**Callee side** — `ZonePortal._enableTokenInternal` `ITIP20(_token).approve(messenger, type(uint256).max)` (`ZonePortal.sol:279`), and `relayMessage` is `onlyPortal`.

**If violated** — withdrawal callbacks could not deliver funds; or, if approval were granted to a non-messenger, escrow could be drained.

---

#### X-4

On-chain: **No**

> The contents committed by `submitBatch` (deposit transition, withdrawal-queue hash, block transition) faithfully reflect what actually happened on the zone.

**Caller side** — `ZonePortal.submitBatch` calls `IVerifier(verifier).verify(...)` (`ZonePortal.sol:844`) and, on `true`, writes `blockHash`, `lastProcessedDepositNumber`, and enqueues `withdrawalQueueHash`.

**Callee side** — `Verifier.verify` (`Verifier.sol:13-31`) is a **stub returning `true`** for all inputs.

**If violated** — a malicious/buggy sequencer can commit arbitrary state transitions; this is the parent cause of I-17, I-18, I-19, E-2.

---

#### X-5

On-chain: **Yes**

> Only `ZoneInbox` mints and only `ZoneOutbox` burns the zone token; `ZoneInbox._enqueueDepositBounceBack` can only push to the outbox via the inbox-gated entry.

**Caller side** — `ZoneInbox` calls `IZoneToken(token).mint(...)`; `ZoneOutbox` calls `zoneToken.burn(...)`.

**Callee side** — `PrivateZoneToken.mint` requires `msg.sender == ZONE_INBOX` (`:167`); `burn` requires `msg.sender == ZONE_OUTBOX` (`:183`); `ZoneOutbox.enqueueDepositBounceBack` requires `msg.sender == ZONE_INBOX` (G-17).

**If violated** — arbitrary mint/burn of zone tokens. (Enforcement lives in the precompile; `PrivateZoneToken` documents it.)

---

## 4. Economic Invariants

---

#### E-1

On-chain: **Yes** (per-call), composes into **No** end-to-end

> Deposit/withdrawal fees collected on one domain equal the fees the sequencer is paid: deposit fee → sequencer on L1 (`ZonePortal`), withdrawal fee burned with the amount on the zone and released to the sequencer on L1.

**Follows from** — I-9 + I-11 + G-9 + G-21. Each leg is locally exact; the cross-domain settlement of the burned fee depends on X-4.

**If violated** — sequencer over/under-charges; if X-4 is abused, the burned fee need not match what is released on L1.

---

#### E-2

On-chain: **No**

> Bridge solvency: total escrowed per token on L1 always covers all withdrawable zone balances.

**Follows from** — I-19 + I-17 + I-18 (all On-chain No) gated by X-4 (stub verifier).

**If violated** — insolvent bridge; users cannot withdraw the full minted supply. This is the protocol's top economic risk and is currently a pure trust assumption on the sequencer.
