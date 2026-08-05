# zone-checker

An observe-only L2 checker execution extension (ExEx) for the Tempo Zone node.

## Purpose

The checker is an Reth ExEx that runs in-process alongside the Zone node. It
receives canonical L2 block notifications from the node's execution pipeline,
verifies that required block data (receipts and exact post-state) is available
for each committed block, logs its observations, and acknowledges the processed
height back to Reth.

## Current milestone: observe-only

At this stage the checker:

- Receives canonical L2 commit, revert, and reorg notifications.
- Processes committed blocks oldest-to-newest and reverted blocks
  newest-to-oldest.
- Confirms that receipts and exact post-state are available for each committed
  or reorged-in block (queried by exact block hash).
- Logs each block's number, hash, and parent hash.
- Acknowledges the finished height only after the complete notification has
  been processed.

It does **not** yet:

- Fetch L1 facts or correlate L1/L2 state.
- Persist derived checker state (no MDBX, SQLite, or other storage).
- Evaluate solvency or accounting invariants.
- Block proposal, settlement, or any enforcement action.
- Emit metrics.

## Modes

| Mode | Behaviour |
|------|-----------|
| `off` | Default. The checker ExEx is not installed. The node runs without any checker overhead. |
| `observe` | The checker ExEx is installed. It logs observations and verifies data availability but does not enforce findings. |

Modes are selected via the Zone CLI argument `--checker.mode <off|observe>` or
the `CHECKER_MODE` environment variable.

## Intended architecture

```text
Tempo L1 blocks/events
         │
         ▼
Zone L1 subscriber + sequencer
         │ produces canonical L2 blocks
         ▼
Zone node / Reth
         │ ExEx notifications
         ▼
Zone checker
  1. extract L2 facts
  2. fetch exact L1 facts
  3. derive/check invariants
  4. commit checker state
  5. report findings
```

Only step 1 (observing L2 notifications) is implemented today. The remaining
steps are planned for later milestones.

## Staged direction

1. **Observe L2 notifications** (current) — receive canonical block
   notifications, verify receipt/state availability, log observations.
2. **Extract Zone facts** — parse deposits, withdrawals, and other L2 state
   transitions from the observed blocks.
3. **Fetch corresponding exact L1 facts** — retrieve the L1 block data that
   corresponds to each L2 notification for cross-layer verification.
4. **Persist derived state and evaluate invariants** — store checker-derived
   state and check solvency/accounting invariants against the extracted facts.
5. **Report findings** — surface invariant violations and checker status.
   Enforcement (blocking proposals or settlement) is considered only after
   reporting is proven reliable.

## Reorg handling and acknowledgement ordering

The checker processes reorg notifications by first rolling back the old fork
newest-to-oldest, then applying the new fork oldest-to-newest. Reverted and
reorged-out blocks are logged but not checked — their data is no longer
canonical.

The ExEx acknowledges a height to Reth (`send_finished_height`) only after the
entire notification has been processed. This prevents Reth from pruning or
advancing past a block the checker has not yet observed.
