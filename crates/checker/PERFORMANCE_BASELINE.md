# Checker performance status

The prior performance baseline measured an implementation and metrics surface
that were removed during the checker rewrite. Its numbers are not evidence for
the current architecture and have intentionally been retired.

The current real-node integration gate exercises local checkpoint construction,
authenticated processing, durable journal state, shutdown, and restart. It is
a functional correctness gate, not a throughput, latency, disk-growth, or
production SLO claim. Establish a new baseline against representative history,
hardware, and archive RPCs before making such claims.
