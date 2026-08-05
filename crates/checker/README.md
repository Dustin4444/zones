# zone-checker

An observe-only L2 checker execution extension (ExEx) for the Tempo Zone node.

## Purpose

The checker is an Reth ExEx that runs in-process alongside the Zone node. It
receives canonical L2 block notifications from the node's execution pipeline,
reads receipts from the executed chain in each notification, decodes Zone
Inbox/Outbox bridge events, constructs typed per-block L2 facts, and then
independently fetches the exact Tempo/L1 block anchored by `TempoAdvanced`,
verifies its identity and receipt root, and decodes ZonePortal L1 events.

Both L2 and L1 facts are temporary: they are constructed while processing a
notification, used to produce log summaries, and then discarded. No
persistence exists yet.

## Current milestone: exact L1 fact extraction (Milestone 3)

The checker now:

- Receives canonical L2 commit, revert, and reorg notifications.
- Processes committed and reorged-in blocks oldest-to-newest; reverted and
  reorged-out blocks newest-to-oldest.
- Confirms that notification-local receipts and exact post-state are available
  for each committed or reorged-in block. Post-state is queried by exact hash.
- Extracts L2 bridge facts from canonical Zone Inbox/Outbox receipt logs.
- Reads the `TempoAdvanced` anchor (exact Tempo/L1 block hash and number).
- Fetches that exact L1 block by hash (never by latest/head), verifies its
  returned hash and number match the anchor, and fetches receipts by exact L1
  block hash.
- Validates transaction/receipt cardinality and recomputes the receipts root
  and logs bloom against the L1 header.
- Independently extracts ordered ZonePortal L1 facts from successful-receipt
  logs emitted by the configured Portal address.
- Logs one concise `"L2 bridge facts extracted"` and `"L1 Portal facts extracted"`
  summary per block.
- Acknowledges the finished height only after the complete notification has
  been processed.
- Logs extraction failures without terminating the Zone node. After a failure,
  it stops advancing its pruning watermark so a restart can replay the gap.

### L2 facts extracted

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

### L1 facts extracted

From `ZonePortal` on the exact Tempo/L1 block anchored by `TempoAdvanced`:

- `DepositMade` — token, net amount, fee, refund recipient, deposit number,
  deposit queue hash
- `TokenEnabled` — token address, name, symbol, currency
- `BatchSubmitted` — withdrawal batch index, queue index, queue hash, next
  block hash, last processed deposit number
- `WithdrawalProcessed` — recipient, sender tag, token, amount, callback success
- `WithdrawalBounceBack` — token, amount, fallback nonce, deposit number,
  deposit queue hash (distinct from `DepositMade`)
- `DepositBounceBack` — refund recipient, token, amount, bounce-back fee
- `DepositBounceBackPending` — refund recipient, token, amount, bounce-back fee
- `RefundClaimed` — recipient, token, amount

### Why bounce-backs are not ordinary deposits

Withdrawal bounce-backs recycle existing Portal backing that was already
escrowed on L1. They do not introduce new external backing the way a user
deposit does. Collapsing them into `DepositMade` would double-count backing in
later solvency accounting, so they are kept as a distinct typed category.

### Temporary facts

Both L2 and L1 facts exist only during block processing. They are constructed,
used to produce log summaries, and then discarded. No persistence exists yet.

## What is not implemented

- **Persistence** — no MDBX, SQLite, or other storage. Facts are discarded
  after logging.
- **Restart/rebuild state** — the checker re-derives facts from notifications
  on each run.
- **L1/L2 correlation or accounting** — L2 and L1 facts are extracted
  independently but not yet cross-checked or aggregated.
- **Invariant evaluation** — no solvency or accounting checks.
- **Per-user accounting** — no user balances or ledgers.
- **Enforcement** — the checker does not block proposal, settlement, or any
  node operation.
- **Metrics** — no metrics are emitted.

## Modes

| Mode | Behaviour |
|------|-----------|
| `off` | Default. The checker ExEx is not installed. The node runs without any checker overhead. |
| `observe` | The checker ExEx is installed. It logs observations, verifies data availability, extracts L2 and L1 facts, but does not enforce findings. |

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

Steps 1 and 2 are implemented (L2 fact extraction from receipts and exact L1
fact extraction from the anchored Tempo block). The remaining steps are
planned for later milestones.

## Staged direction

1. **Observe L2 notifications** (Milestone 1) — receive canonical block
   notifications, verify receipt/state availability, log observations.
2. **Extract Zone L2 facts** (Milestone 2) — decode Zone Inbox/Outbox events
   from canonical L2 receipts and construct typed per-block facts.
3. **Fetch exact Tempo L1 facts** (current, Milestone 3) — fetch the exact L1
   block anchored by `TempoAdvanced`, verify its identity and receipt root,
   and independently extract ZonePortal L1 facts.
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

## L1 connection

The checker establishes its own read-only Tempo L1 provider connection using
the node's existing `--l1.rpc-url` and `--l1.portal-address` CLI arguments.
The connection is established lazily on the first notification that needs it,
so a temporarily unavailable L1 RPC at startup does not prevent the ExEx from
running. L1 provider failures during notification processing are logged and
the pruning watermark is held back; the ExEx continues observing later
notifications.
