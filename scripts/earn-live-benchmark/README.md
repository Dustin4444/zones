# Earn live Zone benchmark

This runner measures the serial four-leg user path on `tempo-zone-unstable`:

1. **Payout:** encrypted stablecoin deposit from Tempo L1 into the Zone.
2. **Earn:** private stablecoin withdrawal, vault deposit, and encrypted EarnToken return.
3. **Redeem:** private EarnToken withdrawal, vault redemption, swap back to the input asset, and encrypted return.
4. **Offramp:** private input-asset withdrawal to the user's public Tempo account.

It discovers the newest successful `zone-txgen` workflow and refuses to start while that workflow is active because both use the same allowlisted devnet account.

```sh
cd scripts/earn-live-benchmark
pnpm install
pnpm benchmark
```

The default is ten serial $10 round trips. Override it with environment variables:

```sh
JOURNEYS=25 ASSET_AMOUNT=100000000 pnpm benchmark
```

The deterministic mnemonic is the public Anvil mnemonic used by the hourly devnet workflow. Never point this runner at a production network or fund that account with real assets.

The JSON reports protocol end-to-end latency from transaction submission through the terminal L1 or Zone RPC read, plus the user transaction and `processWithdrawals` receipt for each leg. Batch-submission/attestation transactions are operator transactions and must be joined from the Zone `BatchFinalized` and L1 `BatchSubmitted` events when calculating complete system cost.

The current live alternate asset is `AlphaUSD`, and the hourly Earn deploy uses `Tempo Earn Local Vault`, a zero-fee ERC-4626 fixture. Results therefore measure the live Zone/gateway boundary and not final DLUSD/metavault economics. The runner calls viem directly; it does not include a Privy-like HTTP/API boundary.

## 1,000-user Deel load runner

`load.ts` models one central Deel treasury paying 1,000 distinct Zone users. The treasury
submits every payout from `END_USER_PRIVATE_KEY`; users are deterministic mnemonic address
indices `1..1000`. By default this is the public dev mnemonic already used by the Zone fixture.
Set `EARN_LOAD_MNEMONIC` only when the deployed access policy expects a different deterministic
user set.

The load runner only accepts explicit 10, 100, or 1,000-user gates. Inspect the reproducible
schedule without credentials or network calls first:

```sh
cd scripts/earn-live-benchmark
EARN_LOAD_USERS=1000 pnpm load:plan > /tmp/earn-load-plan.json
```

For the live 1,000-user gate, export `END_USER_PRIVATE_KEY` through the normal shell environment
and pin the Earn deployment being measured:

```sh
EARN_LOAD_USERS=1000 \
EARN_LOAD_CONFIRM=tempo-zone-unstable:1000 \
EARN_GATEWAY=0xf2aB1d1A20ED4F9A8fF37606dbA1f9822Fe4027F \
EARN_TOKEN=0x20C00000000000000000000041bBF73004B56336 \
pnpm load
```

Do not put private keys on the command line or in generated artifacts. The runner reads
`END_USER_PRIVATE_KEY` directly; `EARN_LOAD_FUNDER_PRIVATE_KEY` is an optional explicit override.

All payouts are deterministically slotted inside `EARN_LOAD_PAYOUT_WINDOW_MS` (60 seconds by
default). Each user then waits seeded, independently varying intervals before Earn, Redeem, and
Offramp. The default concurrency limits are 16 L1 sends, 32 Zone sends, 16 Zone authorization
requests, and 32 accounting reads. Relevant overrides are:

```text
EARN_LOAD_SEED
EARN_LOAD_PAYOUT_WINDOW_MS
EARN_LOAD_EARN_THINK_MIN_MS / EARN_LOAD_EARN_THINK_MAX_MS
EARN_LOAD_REDEEM_THINK_MIN_MS / EARN_LOAD_REDEEM_THINK_MAX_MS
EARN_LOAD_OFFRAMP_THINK_MIN_MS / EARN_LOAD_OFFRAMP_THINK_MAX_MS
EARN_LOAD_L1_SEND_CONCURRENCY
EARN_LOAD_ZONE_SEND_CONCURRENCY
EARN_LOAD_ZONE_AUTH_CONCURRENCY
EARN_LOAD_RPC_READ_CONCURRENCY
EARN_LOAD_ACCOUNT_START_INDEX
EARN_LOAD_ASSET_AMOUNT
EARN_LOAD_USER_FEE_BUFFER
EARN_LOAD_L1_FEE_RESERVE
EARN_LOAD_TIMEOUT_MS
EARN_LOAD_MAX_FAILURES
```

The circuit breaker defaults to 1% failed journeys. The runner funds each user with a 1 PATHUSD
Zone fee reserve by default, even while the Zone reports a zero effective gas price, because live
transaction admission still checks the request's maximum gas liability. Override
`EARN_LOAD_USER_FEE_BUFFER` only after verifying the live admission policy.

Each run writes an append-only `events.ndjson` stream plus `manifest.json`, `schedule.json`,
`journeys.ndjson`, `latency.csv`, `cost-ledger.csv`, and `summary.json` under
`artifacts/<run-id>/`. Latency begins at the mock-API intent boundary and ends at the terminal RPC
read. The cost ledger attributes all observable gas regardless of payer:

- the treasury payout receipt and Zone deposit-ingestion system receipt;
- the user Zone withdrawal request;
- the full `processWithdrawals` receipt, including callback and `depositEncrypted`;
- the returned encrypted deposit's Zone ingestion receipt; and
- every covering `submitBatch`/proof-submission receipt.

Shared `processWithdrawals` receipts are divided by their `WithdrawalProcessed` event count.
System deposit receipts are divided by `TempoAdvanced.depositsProcessed`. Batch receipts are
divided by all inputs in the state transition (external Zone transactions plus processed
deposits), so a sequencer-paid receipt is counted once and attributed without double-counting.
For every receipt, the ledger reports both `gasUsed * effectiveGasPrice` (the 18-decimal formula)
and the actual rounded fee-token debit. On Tempo the latter is decoded from the PATHUSD `Transfer`
to the FeeManager predeploy (`0xfeec…`), and it is the primary actual-cost total in `summary.json`.
If a nonzero-formula receipt has no safely identifiable FeeManager transfer, its actual charge is
left `null` instead of guessed and the summary's receipt-coverage count exposes the gap. Raw gas,
gas price, allocation numerator/denominator, PATHUSD charges, transaction hashes, and block numbers
remain in the ledger for audit.

## Prepare 1,000 distinct users

The live EarnToken uses a TIP-403 compound policy whose recipient and mint-recipient component is a
mutable whitelist. [`policy-setup.ts`](./policy-setup.ts) expands the newest successful hourly
deployment in place for deterministic mnemonic users 1 through 1,000. It discovers and validates
the token, gateway, compound policy, simple policy administrator, and Zone mirror before writing.
It then sends at most 32 `modifyPolicyWhitelist` calls in each Tempo typed transaction, waits for the
Zone to anchor the final L1 receipt, and verifies both recipient checks for every user on L1 and the
Zone. Its report contains public user addresses and transaction receipts, never the mnemonic or
administrator key.

Do not run policy setup while the hourly workflow can rotate the Earn deployment. Record the prior
CronWorkflow state, suspend it, and wait for its active workflow to finish:

```sh
prior_suspend=$(kubectl -n argo-workflows get cronworkflow zone-txgen \
  -o jsonpath='{.spec.suspend}')
prior_suspend=${prior_suspend:-false}
kubectl -n argo-workflows patch cronworkflow zone-txgen --type merge \
  -p '{"spec":{"suspend":true}}'
```

[`argo-policy-setup.yaml`](./argo-policy-setup.yaml) consumes the existing
`zone-unstable-admin/private_key` Kubernetes secret by reference and writes the complete report as
an Argo artifact. The checked-in expected token, gateway, and policy ID make the workflow fail closed
if another deployment has replaced the reviewed target. Submit the manifest only after its
`zones-revision` is available on GitHub. Restore the exact prior CronWorkflow `suspend` value after
policy setup and the benchmark finish:

```sh
kubectl -n argo-workflows patch cronworkflow zone-txgen --type merge \
  -p "{\"spec\":{\"suspend\":${prior_suspend}}}"
```
