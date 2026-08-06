# Local neobank benchmark UI

Run the benchmark UI with:

```sh
just live-bench
```

The command first builds and provisions one persistent local Tempo dev L1, private
Zone, and set of current Earn fixtures. It also prepares the reusable account
balances, approvals, and private-RPC authorization map. It opens
`http://127.0.0.1:4179` only after that setup is ready. Each **Go** button then runs
only the selected real `txgen-tempo` scenario and reports its p99 latency, gas, and
fees.

The four buttons are independent: onramp, Earn vault deposit, Earn vault redeem,
and offramp. They all reuse the running topology until `just live-bench` stops.
The transaction selector controls both the run size and concurrency: every selected
transaction starts immediately using a different pre-authorized account.

The pool defaults to 100 concurrent accounts and can be changed at startup with
`ITERATIVE_BENCH_ACCOUNT_CAPACITY`.

The first run can take several minutes while Rust binaries and the pinned txgen are
built and the persistent topology is provisioned. Runtime data and logs are written
under `target/iterative-bench/`; the UI does not dispatch GitHub Actions and has no
synthetic-results mode.

Required local tools are Rust/Cargo, Foundry (`forge` and `cast`), `git`, `gh`,
`jq`, `curl`, and `bc`. `gh` is only needed on the first run to clone the private
Earn fixture repository.
