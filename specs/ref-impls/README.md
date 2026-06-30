# Reference Implementations

This directory is reserved for Solidity reference specifications for behavior that is expected
to live in native Rust precompiles.

Currently, that is only:

- `src/token/PrivateZoneToken.sol` - reference behavior for privacy-zone TIP-20 token changes.

The deployable Solidity contracts live in `../../crates/contracts` under `l1`, `l2`,
`interfaces`, and `lib`. The Foundry tests, mocks, and fixtures live here under `test`.

Build deployable contract artifacts with:

```bash
forge build ../../crates/contracts src
```
