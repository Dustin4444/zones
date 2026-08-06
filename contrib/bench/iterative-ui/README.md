# Local neobank benchmark UI

Run the benchmark UI with:

```sh
just live-bench
```

The command opens `http://127.0.0.1:4179`. Each **Go** button performs an actual
local benchmark. It builds compatible release binaries, starts an isolated Tempo
dev L1 and private Zone, deploys the current Earn fixtures, runs the selected
`txgen-tempo` preset, and reports the resulting p99 latency, gas, and fees.

The four buttons are independent: onramp, Earn vault deposit, Earn vault redeem,
and offramp. Every click starts from a fresh local chain, so setup and prerequisite
funding are outside the measured scenario. The transaction input is the real txgen
journey count.

The first run can take several minutes while Rust binaries and the pinned txgen are
built. Later runs reuse those build artifacts. Runtime data and logs are written to
`target/iterative-bench/runs/`; the UI does not dispatch GitHub Actions and has no
synthetic-results mode.

Required local tools are Rust/Cargo, Foundry (`forge` and `cast`), `git`, `gh`,
`jq`, `curl`, and `bc`. `gh` is only needed on the first run to clone the private
Earn fixture repository.
