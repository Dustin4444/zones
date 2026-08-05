# zone-checker

An observe-only L2 checker execution extension (ExEx) for the Tempo Zone node.

## Purpose

The checker is an Reth ExEx that runs in-process alongside the Zone node. It
receives canonical L2 block notifications from the node's execution pipeline,
reads receipts from the executed chain in each notification, decodes Zone Inbox/Outbox bridge events,
constructs typed per-block L2 facts, logs a concise fact summary, and
acknowledges the processed height back to Reth.

## Current milestone: L2 fact extraction (Milestone 2)

The checker now:

- Receives canonical L2 commit, revert, and reorg notifications.
- Processes committed and reorged-in blocks oldest-to-newest; reverted and
  reorged-out blocks newest-to-oldest.
- Confirms that notification-local receipts and exact post-state are available
  for each committed or reorged-in block. Post-state is queried by exact hash.
- Extracts L2 bridge facts from canonical Zone Inbox/Outbox receipt logs.
- Logs one concise `"L2 bridge facts extracted"` summary per block.
- Acknowledges the finished height only after the complete notification has
  been processed.
- Logs extraction failures without terminating the Zone node. After a failure,
  it stops advancing its pruning watermark so a restart can replay the gap.

### Facts extracted

From `ZoneInbox.TempoAdvanced` (required block anchor, exactly one per
non-genesis block):

- Tempo/L1 block hash and number
- Deposits processed count
- Processed deposit queue hash
- Last processed deposit number

From the Zone Inbox:

- `DepositProcessed` / `DepositFailed` — deposit hash, token, amount, disposition
- `WithdrawalBounceBackProcessed` / `WithdrawalBounceBackPending` — token,
  amount, disposition (kept distinct from ordinary deposits)
- `RefundClaimed` — token, amount
- `TokenEnabled` — token address

From the Zone Outbox:

- `WithdrawalRequested` — withdrawal index, token, principal amount, fee
  (preserved separately)
- `BatchFinalized` — withdrawal queue hash, batch index (at most one per block)

### Why bounce-backs are not ordinary deposits

Withdrawal bounce-backs recycle existing Portal backing that was already
escrowed on L1. They do not introduce new external backing the way a user
deposit does. Collapsing them into `DepositProcessed` would double-count
backing in later solvency accounting, so they are kept as a distinct typed
category.

### Temporary facts

Facts exist only during block processing. They are constructed, used to
produce a log summary, and then discarded. No persistence exists yet.

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
| `observe` | The checker ExEx is installed. It logs observations, verifies data availability, and extracts L2 bridge facts but does not enforce findings. |

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

Steps 1 is implemented (L2 fact extraction from receipts). The remaining steps
are planned for later milestones.

## Staged direction

1. **Observe L2 notifications** (Milestone 1) — receive canonical block
   notifications, verify receipt/state availability, log observations.
2. **Extract Zone facts** (current, Milestone 2) — decode Zone Inbox/Outbox
   events from canonical L2 receipts and construct typed per-block facts.
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
reorged-out blocks are logged but not fact-checked — their receipts are no
longer canonical. Reorged-in blocks use the same extraction path as ordinary
committed blocks.

The ExEx acknowledges a height to Reth (`send_finished_height`) only after the
entire notification has been processed. This prevents Reth from pruning or
advancing past a block the checker has not yet observed.
