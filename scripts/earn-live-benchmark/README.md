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
