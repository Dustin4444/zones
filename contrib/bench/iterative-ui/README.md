# Iterative neobank benchmark

This local scenario runner dispatches the existing `zones-benchmark.yml` workflow,
follows the real GitHub Actions job, downloads its txgen report, and presents each
part of the neobank private Zone journey as latency, throughput, gas, and fee results.

```sh
just live-bench
```

The command opens `http://127.0.0.1:4179`. It requires an authenticated GitHub
CLI (`gh auth login`) and the current branch must already exist on `origin`, since
the remote workflow checks out that exact ref. Credentials stay in the local
server and are never sent to the browser.

For a presentation rehearsal that does not dispatch GitHub Actions:

```sh
ITERATIVE_BENCH_DEMO=1 just live-bench
```

The UI exposes four independent Go buttons: deposit from L1 into a private Zone,
deposit into Earn and return, redeem and return, and withdraw from the Zone to L1.
Setup needed to fund each standalone scenario is untimed, so the displayed numbers
only describe the operation selected on the card.
