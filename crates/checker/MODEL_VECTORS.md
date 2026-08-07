# Kernel vectors and mutation evidence

The primary semantic authority is the independent test material in
`crates/checker-kernel/src/tests.rs`. Expected values are constructed with
kernel-owned encoders and literal constants. Production Portal, Inbox, Outbox,
payload, queue, and fee transition helpers are not dependencies of the kernel.

The vector set covers:

- ordinary and bounce-back deposit preimages and queue folds;
- empty, partial, and complete deposit prefixes;
- user sender tags and failed-deposit zero sender/hash/nonce identity;
- withdrawal fees, bounce-back fees, rounding, caps, and overflow boundaries;
- empty and multi-member withdrawal queues and partial suffixes;
- token enablement and zero initial `S/D/W`;
- all owner transfers through successful delivery, both pending refund paths,
  bounce-back, callback deposits, and aggregate claims;
- empty batches, batch submission, ring ownership, and partial processing;
- fixed storage layouts, queue sentinels, IDs, counters, and supply accounting.

Observation mutation tests live with `crates/checker/src/observe/` and mutate
authenticated fields independently. They cover imported header identity,
transaction root, full-envelope order/count/hash, receipt order/count/index and
hash binding, receipt root, bloom, canonical ABI/RLP, system-envelope shape,
known and unknown topics, exact state, supply, and collateral. In particular,
valid fake Portal calldata that is not committed by `transactions_root` cannot
drive the kernel.

Archived and randomized differential traces compared the complete kernel with
the prior checker before that implementation was deleted. They compared
expected effects, state, owners, IDs/counters, cursors/commitments, fees,
`S/D/W`, collateral requirements, and finding category/coordinates. These
gates passed before cutover; they are historical migration evidence, not a
second production semantic path.

Persistence and runtime tests independently mutate codecs/journal order and
inject transaction failures. They cover checkpoint replay, restart, duplicate
and conflicting journal rows, reorgs across checkpoints, active-finding
lineage, coverage gaps, and commit-before-acknowledgement.
