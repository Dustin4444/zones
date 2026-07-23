# Tempo Earn Zone boundary fixtures

These contracts are copied from `tempoxyz/earn` commit
`5d21954ce16ff6f7536a58fffcc47c0a917c502c` for the node integration tests.
They live under `test/fixtures` so Foundry compiles their deployment artifacts without treating the
vendored contracts as production coverage targets.

The production contracts are unchanged except that `VaultAdapter` and `FeeMath` import the local
minimal `Math` library. `TestERC1967Proxy` is a test-only deployment helper for the copied,
initializer-based `VaultAdapter` and Bridge controller.

The benchmark also copies Bridge's `DirectSwapV2`, TIP-20 controller and handler, auth registry,
and Earn's `BridgeStableSwapAdapter` from the same revision. The full-journey and swapped-lifecycle
presets route DLUSD/pathUSD conversions through that stack; StablecoinDEX is not their swap path.
