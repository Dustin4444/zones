# X-Ray Report

> Tempo Zones (ref-impls) | 2,021 nSLOC in scope | `6b6c943b` (`main`) | Foundry | 29/06/26

---

## 1. Protocol Overview

**What it does:** A privacy "Zone" is a sequencer-run private chain anchored to Tempo L1; these Solidity reference contracts implement the bidirectional bridge — escrowing TIP-20s on Tempo, minting/burning them on the zone, and reconciling state via batch proofs.

- **Users**: depositors (Tempo → zone), withdrawers (zone → Tempo), cross-zone transferors.
- **Core flow**: deposit escrows on L1 → sequencer mints on zone; withdraw burns on zone → sequencer releases escrow on L1.
- **Key mechanism**: hash-chained deposit queue + fixed-size withdrawal ring buffer, reconciled by `submitBatch` proofs; mirrored Tempo header (`TempoState`) lets zone predeploys read L1 storage slots.
- **Token model**: `PrivateZoneToken` — a TIP-20 precompile variant with private balances/allowances, fixed transfer gas, and system-only mint (`ZoneInbox`) / burn (`ZoneOutbox`).
- **Admin model**: per-zone `admin` (token registry) and `sequencer` (batches, fees, keys) are separate; `sequencer` is the dominant trusted party.

For a visual overview, see the [architecture diagram](architecture.svg).

### Contracts in Scope

| Subsystem | Key Contracts | nSLOC | Role |
|-----------|--------------|------:|------|
| L1 Portal | `ZonePortal`, `ZoneMessenger`, `ZoneFactory`, `Verifier` | 791 | Escrow, deposits, withdrawal processing, batch verification, zone deployment |
| Zone predeploys | `ZoneInbox`, `ZoneOutbox`, `ZoneConfig`, `TempoState` | 933 | Deposit minting, withdrawal burning/batching, L1 state mirroring |
| Queues | `WithdrawalQueueLib`, `DepositQueueLib` | 79 | Ring buffer + hash-chain primitives |
| Crypto / token | `PrivateZoneToken`, `EncryptedDeposit`, `Secp256k1Lib`, `BlockHashHistory` | 130 | ECIES encrypted deposits, key validation, EIP-2935 lookups |
| Routing | `SwapAndDepositRouter` | 87 | Cross-zone transfer with optional StablecoinDEX swap |

(Excludes `IZone.sol`, 612 nSLOC of interfaces/structs.)

### Backwards-Compatibility Code

- `BlockHashHistory.BLOCKHASH_HISTORY` — legacy alias for `EIP2935`, retained for tests only; not used by production paths.

### How It Fits Together

The core trick: **Tempo L1 is the source of truth and the zone mirrors it** — zone predeploys never trust local config, they read L1 `ZonePortal` storage slots through the `TempoState` mirror, which is advanced one block at a time with strict parent-hash continuity.

### Deposit (Tempo → Zone)

```
User → ZonePortal.deposit()
  ├─ _validateDepositPolicy()         → TIP403_REGISTRY (whitelist/blacklist)
  ├─ _collectDepositFunds()           → escrow amount, pay fee to sequencer  *value enters here*
  └─ DepositQueueLib.enqueue()        → currentDepositQueueHash advanced, depositCount++
Sequencer → ZoneInbox.advanceTempo()
  ├─ TempoState.finalizeTempo(header) → mirror next Tempo block  *chain continuity enforced*
  └─ for each deposit: IZoneToken.mint()  *zone supply created here*
         └─ on failure → ZoneOutbox.enqueueDepositBounceBack()
```

### Withdrawal (Zone → Tempo)

```
User → ZoneOutbox.requestWithdrawal()
  ├─ transferFrom + burn (amount + fee)   *zone supply destroyed here*
  └─ store PendingWithdrawal
Sequencer → ZoneOutbox.finalizeWithdrawalBatch()  → withdrawalQueueHash (hash chain)
Sequencer → ZonePortal.submitBatch()
  └─ Verifier.verify(...)  *** STUB: always true ***  → WithdrawalQueue.enqueue()
Sequencer → ZonePortal.processWithdrawal()
  ├─ WithdrawalQueueLib.dequeue()     *hash-verified against committed batch*
  ├─ ZoneMessenger.relayMessage()     → IWithdrawalReceiver callback  *escrow released here*
  └─ on failure → _enqueueBounceBack() → re-enters deposit queue
```

### Cross-Zone Transfer

```
requestWithdrawal(to = SwapAndDepositRouter, callbackData)
  → ZoneMessenger.relayMessage → SwapAndDepositRouter.onWithdrawalReceived()
       ├─ _swapIfNeeded() → StablecoinDEX.swapExactAmountIn()
       └─ IZonePortal(targetPortal).deposit()/depositEncrypted()  → re-deposits into another zone
```

---

## 2. Threat & Trust Model

### Protocol Threat Profile

> Protocol classified as: **Bridge** with **Liquid-Staking-style escrow** and **Privacy/Crypto** characteristics

Signals: lock/escrow-on-L1 + mint-on-zone + burn-on-withdraw, a batch/proof reconciliation with a sequencer, message nonces (`depositCount`, `withdrawalBatchIndex`), cross-domain hash chains, and ECIES encrypted payloads. This is fundamentally a bridge, so bridge adversaries dominate.

### Actors & Adversary Model

| Actor | Trust Level | Capabilities |
|-------|-------------|-------------|
| Sequencer | **Trusted (full)** | Submits batches behind a stub verifier → effectively can commit arbitrary deposit/withdrawal/block transitions (X-4). Sets gas rates, encryption keys, withdrawal caps, processes withdrawals. No timelock, no pause on its powers. |
| Admin | Bounded (token registry only) | `enableToken` (irreversible), `pauseDeposits`/`resumeDeposits`. Cannot block withdrawals (I-8). Instant, no timelock. |
| Pending sequencer | Bounded | Can only `acceptSequencer` to complete a handover the current sequencer initiated. |
| Zone user (depositor/withdrawer) | Untrusted | Permissionless deposit/withdraw/createZone; all inputs user-controlled. |
| Zone system contracts (`ZONE_INBOX`/`OUTBOX`/`CONFIG`, `address(0)`) | Trusted (enshrined) | Mint/burn, advance state, read L1 slots; identity enforced by the executor. |

### Adversary Ranking

1. **Malicious / compromised sequencer** — with the stub verifier, the sequencer is the single point of total failure for solvency and correctness.
2. **Bridge message/accounting attacker** — targets the deposit hash chain and withdrawal ring buffer for replay, skip, or double-credit.
3. **Storage-layout drift attacker (internal)** — the zone hand-codes L1 `ZonePortal` slot numbers; a layout change silently corrupts sequencer/key/token reads.
4. **Griefing attacker** — malformed encrypted payloads, bounce-back floods, withdrawal-cap bypass to bloat queues.
5. **Cross-zone composability attacker** — manipulates the StablecoinDEX swap or target-portal state in `SwapAndDepositRouter`.

See [entry-points.md](entry-points.md) for the full permissionless entry-point map.

### Trust Boundaries

- **`submitBatch` proof boundary** &nbsp;[[X-4](invariants.md#x-4), [E-2](invariants.md#e-2)] — the intended trust boundary (`Verifier.verify`) is a stub returning `true` (`Verifier.sol:31`); today there is *no* cryptographic boundary, only the sequencer key.
- **L1 ↔ zone storage-slot boundary** &nbsp;[[X-1](invariants.md#x-1), [X-2](invariants.md#x-2)] — `ZoneConfig`/`ZoneInbox` read `ZonePortal` slots by hardcoded constant; protection depends on layout discipline, not the compiler.
- **Admin vs sequencer separation** — clean: admin cannot touch funds flow or withdrawals (`ZonePortal.sol:260-271`), only deposit pausing.
- **Mint/burn authority** &nbsp;[[X-5](invariants.md#x-5)] — precompile-enforced `ZONE_INBOX`-only mint / `ZONE_OUTBOX`-only burn.

### Key Attack Surfaces

- **Stub verifier gates all batch state** &nbsp;[[X-4](invariants.md#x-4), [I-17](invariants.md#i-17), [I-18](invariants.md#i-18), [I-19](invariants.md#i-19)] — `ZonePortal.sol:844-863` writes committed state on `verify()==true`. Worth confirming the production verifier is wired before mainnet and that every field it must bind (deposit number, withdrawal hash, block transition) is actually constrained.

- **Deposit contiguity check is a no-op** &nbsp;[[I-18](invariants.md#i-18)] — `ZoneInbox.sol:338-343` compares `currentHash != tempoCurrentHash` then does nothing. Worth tracing how partial-processing contiguity is meant to be enforced once a real proof exists.

- **L1 deposit-number trust** &nbsp;[[I-17](invariants.md#i-17)] — `lastProcessedDepositNumber = depositQueueTransition.nextDepositNumber` (`ZonePortal.sol:863`) with no `<= depositCount` check. Worth confirming the proof binds `nextDepositNumber` to the on-chain counter.

- **Hand-coded L1 storage slots** &nbsp;[[X-1](invariants.md#x-1), [X-2](invariants.md#x-2)] — `ZoneConfig`/`ZoneInbox` reconstruct `ZonePortal` array/mapping slots manually. Worth checking a storage-layout test pins `PORTAL_*_SLOT` constants to the actual layout.

- **Withdrawal callback re-entrancy / griefing** — `processWithdrawal` makes external `transfer`/`relayMessage` calls inside `try/catch` after `dequeue` (`ZonePortal.sol:658-714`); no `nonReentrant`. Worth tracing whether a callback can re-enter `processWithdrawal`/`claimRefund` while queue state is mid-update.

- **`maxWithdrawalsPerBlock` is best-effort** &nbsp;[[I-21](invariants.md#i-21)] — counter resets per block and `enqueueDepositBounceBack` bypasses the cap. Worth confirming `_pendingWithdrawals` growth is otherwise bounded.

- **Cross-zone swap slippage / target validation** — `SwapAndDepositRouter.onWithdrawalReceived` swaps via `StablecoinDEX` with caller-supplied `minAmountOut` and re-deposits. Worth checking target-portal/token validation (`_validateTarget`) cannot be steered to an attacker portal.

### Protocol-Type Concerns

**As a Bridge:**
- Replay/double-spend defense rests on the deposit hash chain + ring-buffer hash verification (I-13) — solid *given* a real proof, vacuous without one.
- 1:1 escrow↔supply backing (I-19/E-2) is unverifiable on a single chain; needs the proof + monitoring.

**As a Privacy system:**
- Encrypted-deposit griefing is mitigated at the L1 boundary (point/length validation, G-10/G-11) so invalid ciphertexts can't stall the zone.

### Temporal Risk Profile

**Deployment & Initialization:**
- `createZone` is permissionless and trusts caller-supplied `sequencer`/`admin`/`genesis`; a zone is only as honest as its creator's parameters. `Verifier` must point at a real implementation (currently stub).

**Market Stress:**
- Withdrawal ring buffer is fixed at 100 batches (`WITHDRAWAL_QUEUE_CAPACITY`); if `processWithdrawal` lags behind `submitBatch`, `enqueue` reverts with `WithdrawalQueueFull` — worth confirming the sequencer cadence keeps `tail - head < 100`.

### Composability & Dependency Risks

> **StablecoinDEX** — via `SwapAndDepositRouter._swapIfNeeded`
> - Assumes: `swapExactAmountIn` returns ≥ `minAmountOut`
> - Validates: `minAmountOut` (caller-supplied), target portal + token registration
> - Mutability: external Tempo system contract
> - On failure: whole callback reverts → withdrawal bounces back to source zone

> **TIP403_REGISTRY** — via `ZonePortal._validateDepositPolicy`
> - Assumes: authoritative recipient/mint authorization
> - Validates: `isAuthorizedRecipient` + `isAuthorizedMintRecipient` for `to` and `bouncebackRecipient`
> - Mutability: Tempo precompile, mirrored from L1
> - On failure: deposit reverts (`PolicyForbids`)

> **EIP-2935 / TempoStateReader precompiles** — via `BlockHashHistory.getBlockHash`, `TempoState.readTempoStorageSlot`
> - Assumes: correct historical block hash / L1 storage at `tempoBlockNumber`
> - Validates: zero-hash rejection (G-14); reader restricted to system contracts
> - On failure: `submitBatch`/reads revert

**Token Assumptions**: assumes TIP-20 `transferFrom`/`transfer` revert on failure (used without boolean checks in `_collectDepositFunds`, `ZonePortal.sol:485`); standard for TIP-20 but worth confirming for every enabled token.

---

## 3. Invariants

> ### 📋 Full invariant map: **[invariants.md](invariants.md)**
>
> - **24 Enforced Guards** (`G-1`…`G-24`) — per-call preconditions
> - **21 Single-Contract Invariants** (`I-1`…`I-21`) — Conservation, Bound, Ratio, StateMachine, Temporal
> - **5 Cross-Contract Invariants** (`X-1`…`X-5`) — L1↔zone slot reads, mint/burn authority, verifier gate
> - **2 Economic Invariants** (`E-1`,`E-2`) — fee conservation, bridge solvency
>
> The **12 On-chain=No** blocks are the high-signal ones — almost all trace to the stub `Verifier` (X-4): I-17, I-18, I-19, X-1, X-2, X-4, I-21, E-2.

---

## 4. Documentation Quality

| Aspect | Status | Notes |
|--------|--------|-------|
| README | Present | Repo root `README.md`; full spec at `specs/spec.md`, `specs/ref-impls/` |
| NatSpec | ~621 `@` tags | Thorough across all contracts |
| Spec/Whitepaper | Present | `specs/spec.md` + zone spec referenced in README |
| Inline Comments | Thorough | Extensive rationale comments, esp. on fees, keys, queues |

No `@invariant` NatSpec tags found; invariants here are structurally inferred (per code).

---

## 5. Test Analysis

| Metric | Value | Source |
|--------|-------|--------|
| Test files | 21 | File scan |
| Test functions | 359 | File scan |
| Line coverage | Unavailable — not run (Tempo hardfork toolchain) | Coverage tool |
| Branch coverage | Unavailable — not run | Coverage tool |

### Test Depth

| Category | Count | Contracts Covered |
|----------|-------|-------------------|
| Unit | ~331 | broad (per-contract `.t.sol` for every core contract) |
| Stateless Fuzz | 17 | `testFuzz_*` across portal, outbox, inbox, queues, crypto |
| Stateful Fuzz (Foundry invariant) | 0 | none |
| Formal Verification (Halmos/symbolic) | 11 | `check_*` in `ZoneSymbolic.t.sol` |

Mutation testing artifacts (`cache/mutation/*.survived`) exist — surviving mutants in `IZone`, `ZoneConfig`, `ZoneInbox`, `ZoneOutbox`, `ZonePortal`, `WithdrawalQueueLib`, `TempoState` warrant review.

### Gaps

- **No Foundry stateful invariant tests** despite the config block (`invariant = {runs, depth}` in `foundry.toml`) — the bridge solvency (E-2) and queue conservation (I-12/I-13) invariants are exactly what stateful fuzzing should target.
- Surviving mutants indicate weak spots in queue/portal logic — prioritize killing those in `ZonePortal`/`ZoneOutbox`.

---

## 6. Developer & Git History

> Repo shape: normal_dev — active history; the git-security script mislabeled it `squashed_import` because it scanned `src/` while sources live at `specs/ref-impls/src/` (path-prefix mismatch). Hotspot/recent-commit data below is accurate.

### File Hotspots

| File | Modifications | Note |
|------|-------------:|------|
| `ZonePortal.sol` | 10 | Highest churn — also top attack surface |
| `IZone.sol` | 9 | Shared interface/struct surface |
| `ZoneOutbox.sol` | 6 | Withdrawal batching |
| `ZoneInbox.sol` | 5 | Deposit processing |
| `ZoneFactory.sol` | 3 | Zone creation |

### Security-Relevant Commits (recent)

| Subject | Signal |
|---------|--------|
| `fix: Handle fee transfer failure during processWithdrawal (#529)` | fee-transfer try/catch on withdrawals |
| `Fix: Add checks specified in the spec for withdrawals (#523)` | withdrawal validation hardening |
| `fix: validate L1 receipt roots (#521)` | header/state validation |
| `fix: deposit hashing and Tempo RLP validation divergences (#503)` | deposit hash-chain correctness |
| `feat: implement deposit bouncebacks (#486)` | bounce-back machinery |

### Security Observations

- **Single-author concentration** — `0xKitsune` authored ~4,730/5,000 source lines; bus-factor and review-ergonomics risk.
- **No merge commits** (0 of 125) — review process not visible in git.
- **Highest churn = highest risk** — `ZonePortal.sol` is #1 in both modifications and attack-surface priority.
- **Recent burst of withdrawal/deposit fixes** (#503/#521/#523/#529) clusters around the exact hash-chain and fee paths flagged in I-17/I-18/E-1.

### Cross-Reference Synthesis

- **`ZonePortal.sol` is #1 in churn AND attack surface** → highest-leverage review: `submitBatch`, `processWithdrawal`, `_collectDepositFunds`.
- **Stub `Verifier` + no stateful invariant tests** → the properties most likely to break (I-17/I-18/I-19/E-2) are the least tested; prioritize a real verifier and invariant fuzzing together.

---

## X-Ray Verdict

**FRAGILE** — comprehensive unit tests and excellent docs, but the central trust boundary (the proof verifier) is an explicit stub and the highest-value invariants have no stateful-fuzz coverage.

**Structural facts:**
1. 2,021 in-scope nSLOC across 5 subsystems; 15 contracts/libraries.
2. `Verifier.verify` is a stub returning `true` — all 12 On-chain=No invariants trace to it.
3. 359 test functions across 21 files (17 stateless fuzz, 11 symbolic); 0 Foundry stateful invariant tests despite an invariant config.
4. Single developer wrote ~94% of source lines; 0 merge commits.
5. `ZonePortal.sol` leads both churn (10 mods) and attack-surface priority.
