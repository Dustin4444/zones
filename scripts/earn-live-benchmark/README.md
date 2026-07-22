# Earn live Zone benchmark

This runner measures the serial user path on `tempo-zone-unstable`:

1. Encrypted stablecoin deposit from Tempo L1 into the Zone.
2. Private Zone withdrawal to the latest hourly Earn gateway.
3. Vault deposit and encrypted EarnToken return to the Zone.

It discovers the newest successful `zone-txgen` workflow and refuses to start while that workflow is active because both use the same allowlisted devnet account.

```sh
cd scripts/earn-live-benchmark
pnpm install
pnpm benchmark
```

The default is ten serial $10 journeys. Override it with environment variables:

```sh
JOURNEYS=25 ASSET_AMOUNT=100000000 pnpm benchmark
```

The deterministic mnemonic is the public Anvil mnemonic used by the hourly devnet workflow. Never point this runner at a production network or fund that account with real assets.

The current live alternate asset is `AlphaUSD`, and the hourly Earn deploy uses `Tempo Earn Local Vault`, a zero-fee ERC-4626 fixture. Results therefore measure the live Zone/gateway boundary and not final DLUSD/metavault economics.
