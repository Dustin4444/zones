# Goal 0 model vectors

This is the review inventory for the checker-owned pure model in
`src/model/`. It covers Goal 0 of `DESIGN.md` only. None of these values are
derived with the production Portal, Inbox, Outbox, payload-builder, sequencer,
queue, transition, or fee helpers.

## Independent byte vectors

The primary byte vectors were generated outside the Rust implementation with
Foundry `cast` 1.7.1. `cast abi-encode` produced literal preimages and
`cast keccak` produced their commitments. The complete hex preimages are
checked into `src/model/test_vectors.rs`; the model tests compare the local ABI
encoders with those bytes before checking the hashes.

| Rule | Literal vector |
|---|---|
| First ordinary deposit after zero | `0x89982eeee3ca64954daa0322b331f17efd85a433564bfdb4938c0ab087663a5d` |
| Two-item ordinary-deposit prefix | `0x1c5c0c09978e9f50b319cbb91fe92fafa252fe6375cd0689ed1dd31ce7880fee` |
| Withdrawal bounce-back deposit after zero | `0x737e7e554cef04e8a45184e9162cde4450971ee8b09fbccb2658a22a58611808` |
| User sender tag (`sender || containing_tx_hash`) | `0x977ca7d7170498bf6675510cf2e40c11a6e5683f702bb46e206064af26a505a3` |
| Failed-deposit sender tag (`zero || zero`) | `0xa86d54e9aab41ae5e520ff0062ff1b4cbd0b2192bb01080a058bb170d84e6457` |
| Empty withdrawal batch | `bytes32(0)` |
| Three-member withdrawal queue | `0xea645b2419e3da96758eb0dbacc08e41d1c744b0cd8adb09405d6056e91f1753` |
| Suffix after first member | `0x9749a08b6e690a932830e1d29974cb9b7b1f7145cc006bbb2c42c426fe335c81` |
| Suffix after second member / final failed-deposit member | `0xac67cdf55db79608ba1e80bcf7ee9f623774def8252331e67102ad4dc683f910` |

Representative standalone commands are:

```sh
cast keccak 0x77777777777777777777777777777777777777778888888888888888888888888888888888888888888888888888888888888888
cast abi-encode 'f(uint8,(address,address,uint128),bytes32)' 0 '(0x1234567890123456789012345678901234567890,0x0000000000000000000000000000000000000007,500)' 0x0000000000000000000000000000000000000000000000000000000000000000
cast keccak 0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000001234567890123456789012345678901234567890000000000000000000000000000000000000000000000000000000000000000700000000000000000000000000000000000000000000000000000000000001f40000000000000000000000000000000000000000000000000000000000000000
```

The ordinary-deposit and withdrawal tuples are longer, so their complete
standalone results are retained as literal bytes rather than hidden behind a
test fixture generator.

## Rule-to-evidence inventory

| Protocol rule | Checker-owned evidence |
|---|---|
| Deposit discriminators, sentinel, no-queue index, base gas, `10^12`, zero deployment config, and TIP-20 slot 8 | `constants.rs` literal constants, pinned source comments, and constant/config tests |
| Ordinary and bounce-back deposit ABI preimages and append folds | `encoding.rs` plus the literal preimages in `test_vectors.rs`; single- and multi-item tests |
| Withdrawal member ABI, newest-to-oldest fold, empty/partial/full processing, and empty-call no-op | `encoding.rs`; literal member preimages and queue hashes, including an arbitrary ignored suffix |
| User and failed-deposit identities | `ownership.rs` and `encoding.rs`; one-based deposit/batch IDs, type-enforced nonzero user principal/fallback nonce/transaction hash, failed-deposit economics derived from the consumed ordinary preimage, literal zero public fields, and one-owner rejection fixture |
| Every open owner and section 5 lifecycle edge | `ownership.rs`; concrete owner snapshots across every section 5 lifecycle family, origin-specific pending withdrawals, reveal-mode/encrypted-sender coupling, no duplicated per-owner deposit commitment or per-withdrawal batch assignment, independently derived batch count/hash/range, and checked constructors rejecting overflowing ranges, invalid dynamic fields, empty submitted batches, sentinel queue IDs, and exhausted open cursors |
| Exact `S/D/W` effects | `accounting.rs`; one table row per DESIGN section 5.5 transition, record-presence enablement, duplicate-zero rejection, and checked overflow/underflow tests |
| Withdrawal and bounce-back fees | `fees.rs`; boundary, rounding, same-block order, zero, cap, and checked-intermediate overflow vectors |
| Exact state reads | `state_layout.rs`; literal predeploy/slot keys for six commitments, packed low-`uint64` decoding, and per-token slot 8 |
| Trust-boundary roles | DESIGN section 4 freezes the `AuthenticatedInput`, `AuthenticatedOutcome`, `ExpectedOutput`, and `Finding` roles and their rule mapping. Goal 0 has no adapter-authenticated observation type; Goal 1 must introduce concrete field-split types in the authenticating adapters instead of generic wrappers that can relabel arbitrary values |
| Version-pinned event surface | `events/mod.rs` and its per-emitter modules; separate L1/L2 classifiers, generated checker-owned interfaces, independent literal topics, strict canonical decode/re-encode, address-array count guards before allocation, bounded decoded dynamic fields, external-emitter exclusion, protocol-emitter fail-closed behavior, and valid encoded fixtures asserting each exact variant or known-event disposition |
| Native `DepositRejected` exclusion | `events/inbox.rs`; its independently pinned literal topic is recognized only to return `UnsupportedProtocolEvent` |

The literal event topics were independently pinned as the Keccak-256 hashes of
their exact Solidity signatures. Tests compare each literal with the generated
checker-owned interface and classify a valid encoded log at the intended
emitter for every allowed emitter/topic pair, asserting the exact decoded
variant or known-event disposition.

## Pinned-source confirmations

- `tempo_gas_rate` and `max_withdrawals_per_block` are zero-initialized by the
  generated native Outbox initialization path; Portal `bounceback_gas` has an
  explicit zero deployment default. The source paths are attached to
  `INITIAL_CONFIG`.
- The six exact-state layouts and TIP-20 slot 8 agree with the pinned native,
  reference-contract, and Tempo layout sources cited next to the constants.
- The pinned native Inbox has no rejection input or rejection execution path.
  `DepositRejected` remains an explicit unsupported event.
- The current production Rust Portal ABI mirror omits `DepositsPaused`,
  `DepositsResumed`, and `RpcUrlUpdated`, although the pinned Solidity event
  surface emits them. The checker declares those known non-model events
  locally, as required by DESIGN section 7.2, instead of inheriting the
  incomplete production binding.
- `RpcUrlUpdated(string)` has no pinned semantic maximum. Its decoder is
  therefore bounded to the canonical supplied log body and does not invent a
  protocol length limit.

No pinned-source check contradicted the approved Goal 0 contract.
