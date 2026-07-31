# Zone E2E Test Harness

End-to-end test infrastructure for the Tempo Zone node, supporting both fast
synthetic injection and real in-process L1 integration.

## Architecture

The harness provides two independent testing paths:

```
┌─────────────────────────────┐     ┌──────────────────────────────┐
│  Injection Path (e2e.rs)    │     │  Real L1 Path (l1_e2e.rs)    │
│                             │     │                              │
│  L1Fixture builds synthetic │     │  L1TestNode (Tempo dev mode) │
│  TempoHeaders + Deposits    │     │  produces real blocks @500ms │
│           │                 │     │           │                  │
│           ▼                 │     │           ▼                  │
│  DepositQueue.enqueue()     │     │  L1Subscriber (WS + HTTP)    │
│  + seed_l1_cache()          │     │  parses DepositMade logs     │
│                             │     │           │                  │
└───────────┬─────────────────┘     └───────────┬──────────────────┘
            │                                   │
            └───────────────┬───────────────────┘
                            ▼
                    ┌───────────────┐
                    │ DepositQueue  │
                    └───────┬───────┘
                            ▼
                    ┌───────────────┐
                    │  ZoneEngine   │  (pops L1 blocks, builds L2 blocks)
                    └───────┬───────┘
                            ▼
              ┌─────────────────────────┐
              │  Zone L2 Predeploys     │
              │  TempoState  (0x1c..00) │  slot 0=blockHash, slot 7=packed fields
              │  ZoneInbox   (0x1c..01) │  advanceTempo → mint pathUSD
              │  ZoneOutbox  (0x1c..02) │  finalizeWithdrawalBatch
              │  StateReader (0x1c..04) │  reads L1 storage via cache
              └─────────────────────────┘
```

The shared harness lives in `utils/`, one module per concern: `node`
(`ZoneTestNode` + the mock L1 RPC), `l1` (`L1TestNode`, TIP-403 seeding,
genesis builders), `fixture` (`L1Fixture` + queue injection), `accounts`
(`ZoneAccount`, withdrawal args), `p2p` (multi-sequencer cluster), and
`private_rpc` (auth tokens, RPC test contexts). Everything is re-exported
through `utils::`.

### Injection Path (`e2e.rs`)

Uses `L1Fixture` to manually construct `TempoHeader` and `Deposit` objects,
push them into the `DepositQueue`, and seed the `L1StateCache` for
`TempoState` storage reads. Fast (~1s per test) and deterministic.

```rust
let (zone, mut fixture) = start_local_zone_with_fixture(10).await?;
let deposit = make_deposit(PATH_USD_ADDRESS, sender, recipient, amount);
fixture.inject_deposits(zone.deposit_queue(), vec![deposit]);
// poll for balance change...
```

**L1Fixture internals:**
- Chains `parent_hash = keccak256(rlp(prev_header))` to match `TempoState` verification
- Monotonic block numbers starting from 1, timestamps from 1,000,000
- `seed_l1_cache()` populates portal storage slots (sequencer membership and deposit queue hash=3)
  so `TempoState` storage reads succeed without a real L1

**Multi-zone support:** Use `next_block()` + `enqueue()` to broadcast the same
`FixtureBlock` to multiple zone deposit queues:

```rust
let b1 = fixture.next_block();
fixture.enqueue(&b1, zone1.deposit_queue(), vec![deposit_for_zone1]);
fixture.enqueue(&b1, zone2.deposit_queue(), vec![]);
```

### Real L1 Path (`l1_e2e.rs`)

`start_l1_and_zone()` starts an in-process Tempo L1 dev node, deploys a zone
portal on it, and connects a zone node whose `L1Subscriber` receives real
blocks over WebSocket.

**Genesis patching in `start_from_l1()`:**

The zone's `TempoState` genesis must be anchored to the L1's current state.
`start_from_l1()` fetches the L1's latest header and patches the bundled genesis
template (`crates/node/assets/zone-dev-genesis.json`, via `zone_node::genesis`):

1. **Slot 0** (`tempoBlockHash`): Set to `keccak256(rlp(l1_header))`
2. **Slot 7** (packed `uint64` fields): Low 64 bits set to `l1_header.number`
   - Layout: `(tempoBlockNumber:u64, tempoGasLimit:u64, tempoGasUsed:u64, tempoTimestamp:u64)`
   - Only `tempoBlockNumber` is currently patched; other fields retain genesis defaults

## Test Modules

| Module | Covers |
|--------|--------|
| `e2e.rs` | Injection-based deposits, withdrawal batching, P2P leader/follower, engine lifecycle |
| `l1_e2e.rs` | Real-L1 deposits/withdrawals, cross-zone routing, encrypted deposits, TIP-403 policy bounces |
| `earn_zone_e2e.rs` | Earn vault deposit/redeem matrix through the zone (needs the private `earn` artifacts) |
| `restart_e2e.rs` | Sequencer restart resilience (batch submission and withdrawals resume from portal state) |
| `handoff_e2e.rs` | Multi-sequencer leadership handoff, forwarded transactions, live propagation |
| `stepping_e2e.rs` | Out-of-EIP-2935-window batch submission via ancestry anchors |
| `tip403_policy.rs` | Zone TIP-403 proxy precompile against seeded raw L1 policy state |
| `tip403_transfers.rs` | TIP-20 transfer/withdrawal flows under receive policies |
| `enable_token.rs` | `TokenEnabled` pipelines (injected events and the live real-L1 path) |
| `private_rpc.rs` | Auth-token parsing and method classification (pure unit tests) |
| `private_rpc_e2e.rs` | Private RPC server: auth, privacy scoping, method tiers, WS subscriptions |
| `demo_*.rs` | Narrated end-to-end flows: shield-and-send, multi-asset, cross-zone |
| `deposit.rs` | Real-testnet deposit round trip (`#[ignore]`d; needs `L1_PORTAL_ADDRESS`) |
| `precompiles.rs` | Precompiles disabled on zones |

Real-L1 modules require `forge build --root specs/ref-impls` first.

## Key Types

- **`ZoneTestNode`** — In-process zone L2 node with RPC endpoint. Fields are
  private; use `http_url()`, `deposit_queue()`, `l1_state_cache()` getters.
  Constructed via the `start_*` wrappers or `launch(ZoneNodeParams)`.
- **`L1TestNode`** — In-process Tempo L1 dev node. Fields are private; use
  `http_url()` and `ws_url()` getters. Constructed via `start()`; most tests
  use `start_l1_and_zone()` for the full preamble.
- **`ZoneAccount`** — A funded account with providers on both layers; wraps
  deposits (plain, token, encrypted, raw) and withdrawals.
- **`L1Fixture`** — Synthetic L1 block builder maintaining hash chain continuity.
- **`FixtureBlock`** — Clonable L1 block for multi-zone broadcast.
- **`poll_until`** — Generic async condition poller with timeout.

## Known Issues / Improvements

- **Slot 7 partial patch:** `start_from_l1()` only patches `tempoBlockNumber` in
  the packed slot 7. Should also patch `tempoGasLimit`, `tempoGasUsed`, and
  `tempoTimestamp` from the anchor header for full consistency.
- **Event assertions:** Some tests query events from block 0 and assume ordering.
  Filter by sender/recipient/amount for robustness.
