# Entry Point Map

> Tempo Zones (ref-impls) | 27 entry points | 6 permissionless | 18 role-gated | 3 admin-only

---

## Protocol Flow Paths

### Setup (Factory / Admin)

```
ZoneFactory.createZone()
  └─→ deploys ZoneMessenger + ZonePortal  (constructor enables initial token, grants messenger max approval)
        └─→ zone predeploys (ZoneConfig, ZoneInbox, ZoneOutbox, TempoState) read portal state

[admin]     → ZonePortal.enableToken() → ZonePortal.pauseDeposits() / resumeDeposits()
[sequencer] → ZonePortal.setSequencerEncryptionKey()   ◄── required before any depositEncrypted
```

### Deposit Flow (Tempo → Zone)

```
ZonePortal.deposit() / depositEncrypted()   ◄── token enabled + depositsActive, fee+bounceback covered
  └─→ funds escrowed, currentDepositQueueHash advanced, depositCount++
        └─→ [sequencer] ZoneInbox.advanceTempo()   ◄── after TempoState.finalizeTempo
              ├─→ IZoneToken.mint() (success) → recipient credited on zone
              └─→ ZoneOutbox.enqueueDepositBounceBack() (mint fails/rejected) → refund path
```

### Withdrawal Flow (Zone → Tempo)

```
ZoneOutbox.requestWithdrawal()   ◄── token enabled, gasLimit ≤ cap, per-block cap, approve amount+fee
  └─→ transferFrom + burn, pending withdrawal stored
        └─→ [sequencer] ZoneOutbox.finalizeWithdrawalBatch() → builds withdrawalQueueHash
              └─→ [sequencer] ZonePortal.submitBatch()   ◄── proof (stub) → enqueue into L1 ring buffer
                    └─→ [sequencer] ZonePortal.processWithdrawal()
                          ├─→ ITIP20.transfer() (gasLimit 0)
                          ├─→ ZoneMessenger.relayMessage() → IWithdrawalReceiver.onWithdrawalReceived() (callback)
                          └─→ _enqueueBounceBack() (failure) → bounces back to zone as a deposit
```

### Cross-Zone Transfer

```
requestWithdrawal(... SwapAndDepositRouter ...)
  → ZoneMessenger.relayMessage
  → SwapAndDepositRouter.onWithdrawalReceived()
  → StablecoinDEX.swapExactAmountIn (optional)
  → IZonePortal(target).deposit() / depositEncrypted()
```

### Refund Recovery

```
ZonePortal.claimRefund() (L1) / ZoneInbox.claimRefund() (zone)
  ◄── a prior bounce-back transfer/mint reverted and parked a refund
```

---

## Permissionless

### `ZonePortal.deposit()`

| Aspect | Detail |
|--------|--------|
| Visibility | external |
| Caller | Any user |
| Parameters | `_token` (user-controlled), `to` (user-controlled), `amount` (user-controlled), `memo` (user-controlled), `bouncebackRecipient` (user-controlled) |
| Call chain | `→ _validateDepositsActive → _validateDepositPolicy (TIP403_REGISTRY) → _collectDepositFunds (ITIP20.transferFrom/transfer) → DepositQueueLib.enqueue` |
| State modified | `currentDepositQueueHash`, `depositCount` |
| Value flow | Tokens: sender → portal (escrow); fee → sequencer |
| Reentrancy guard | no |

### `ZonePortal.depositEncrypted()`

| Aspect | Detail |
|--------|--------|
| Visibility | external |
| Caller | Any user |
| Parameters | `_token` (user-controlled), `amount` (user-controlled), `keyIndex` (user-controlled), `encrypted` (user-controlled), `bouncebackRecipient` (user-controlled) |
| Call chain | `→ _validateDepositsActive → TIP403_REGISTRY → Secp256k1Lib.isValidX → isEncryptionKeyValid → _collectDepositFunds → DepositQueueLib.enqueueEncrypted` |
| State modified | `currentDepositQueueHash`, `depositCount` |
| Value flow | Tokens: sender → portal (escrow); fee → sequencer |
| Reentrancy guard | no |

### `ZoneOutbox.requestWithdrawal()` (2 overloads)

| Aspect | Detail |
|--------|--------|
| Visibility | external |
| Caller | Any zone user |
| Parameters | `token`,`to`,`amount`,`memo`,`gasLimit`,`fallbackRecipient`,`data`,`revealTo` (all user-controlled) |
| Call chain | `→ config.isEnabledToken → _validateGasLimit → _validateRevealTo → ZONE_TX_CONTEXT.currentTxHash → IZoneToken.transferFrom → IZoneToken.burn` |
| State modified | `_pendingWithdrawals`, `nextWithdrawalIndex`, `_withdrawalsThisBlock` |
| Value flow | Tokens: sender → outbox → burned (`amount + fee`) |
| Reentrancy guard | no |

### `ZonePortal.claimRefund()` / `ZoneInbox.claimRefund()`

| Aspect | Detail |
|--------|--------|
| Visibility | external |
| Caller | Refund owner (`msg.sender` keyed) |
| Parameters | `token` (user-controlled) |
| Call chain (L1) | `→ ITIP20.transfer(msg.sender, amount)` |
| Call chain (zone) | `→ IZoneToken.mint(msg.sender, amount)` |
| State modified | `refunds[token][msg.sender] = 0` |
| Value flow | Tokens: portal/zone → caller |
| Reentrancy guard | no (effects-before-interaction: balance zeroed first) |

### `ZoneFactory.createZone()`

| Aspect | Detail |
|--------|--------|
| Visibility | external |
| Caller | Anyone (gas-gated, `gasleft() >= 15M`) |
| Parameters | `params` (user-controlled: token, admin, sequencer, verifier, genesis, rpcUrl) |
| Call chain | `→ ITIP20Factory.isTIP20 → new ZoneMessenger → new ZonePortal` |
| State modified | `_zones`, `_isZonePortal`, `_isZoneMessenger`, `_nextZoneId`, `_deploymentNonce` |
| Value flow | none |
| Reentrancy guard | no |

---

## Role-Gated

### Sequencer (L1 `ZonePortal`, via `onlySequencer`)

| Function | Parameters | State Modified |
|----------|-----------|----------------|
| `submitBatch()` | block/deposit transitions, `withdrawalQueueHash`, `proof` (sequencer-provided) | `withdrawalBatchIndex`, `blockHash`, `lastSyncedTempoBlockNumber`, `lastProcessedDepositNumber`, `_withdrawalQueue` |
| `processWithdrawal()` | `withdrawal`, `remainingQueue` (sequencer-provided) | `_withdrawalQueue`, `refunds`, `currentDepositQueueHash` (bounce-back), `depositCount` |
| `setZoneGasRate()` | `_zoneGasRate` | `zoneGasRate` |
| `setSequencerEncryptionKey()` | `x`,`yParity`,`popV/R/S` | `_encryptionKeys` |
| `setRpcUrl()` | `_rpcUrl` | `rpcUrl` |
| `transferSequencer()` | `newSequencer` | `pendingSequencer` |

### Pending Sequencer (`ZonePortal`)

| Function | Parameters | State Modified |
|----------|-----------|----------------|
| `acceptSequencer()` | none (`msg.sender == pendingSequencer`) | `sequencer`, `pendingSequencer` |

### Sequencer (zone predeploys, via `config.sequencer()` or system `address(0)`)

| Contract.Function | Parameters | State Modified |
|-------------------|-----------|----------------|
| `ZoneInbox.advanceTempo()` | `header`, `deposits`, `decryptions`, `enabledTokens` | `processedDepositQueueHash`, `processedDepositNumber`, `refunds`, TempoState fields, mints/bounce-backs |
| `ZoneOutbox.finalizeWithdrawalBatch()` | `count`, `blockNumber`, `encryptedSenders` | `_pendingWithdrawals(Head)`, `withdrawalBatchIndex`, `_lastBatch` |
| `ZoneOutbox.setTempoGasRate()` | `_tempoGasRate` | `tempoGasRate` |
| `ZoneOutbox.setMaxWithdrawalsPerBlock()` | `_maxWithdrawalsPerBlock` | `maxWithdrawalsPerBlock` |

### System-contract-gated

| Contract.Function | Restricted to | State Modified |
|-------------------|--------------|----------------|
| `TempoState.finalizeTempo()` | `ZONE_INBOX` | all TempoState header fields |
| `ZoneOutbox.enqueueDepositBounceBack()` | `ZONE_INBOX` | `_pendingWithdrawals`, `nextWithdrawalIndex` |
| `ZoneMessenger.relayMessage()` | `portal` (`onlyPortal`) | none (forwards tokens + callback) |
| `SwapAndDepositRouter.onWithdrawalReceived()` | registered zone messengers | none (swaps + deposits onward) |
| `PrivateZoneToken.mint()` | `ZONE_INBOX` | balances/supply (precompile) |
| `PrivateZoneToken.burn()` | `ZONE_OUTBOX` | balances/supply (precompile) |

---

## Admin-Only

| Contract | Function | Parameters | State Modified |
|----------|----------|------------|----------------|
| `ZonePortal` | `enableToken()` | `_token` | `_tokenConfigs`, `_enabledTokens`, messenger approval |
| `ZonePortal` | `pauseDeposits()` | `_token` | `_tokenConfigs[_token].depositsActive = false` |
| `ZonePortal` | `resumeDeposits()` | `_token` | `_tokenConfigs[_token].depositsActive = true` |

---

## Initialization

- `ZonePortal` constructor — enables initial token, grants messenger max approval, sets sequencer/admin/verifier/genesis (one-time, via `ZoneFactory.createZone`).
- `TempoState` constructor — decodes + stores the genesis Tempo header.
- `ZoneFactory` constructor — deploys the shared `Verifier` and marks it valid.
