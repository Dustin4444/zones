# Zone stateless prover checkpoint TODO

This file records what the current checkpoint actually implements versus the
recovered stateless-prover goal. The checkpoint moves production proof paths onto
real witness-backed execution, but it should not be treated as the full goal
until the TODOs below are closed.

Recovered goal, in short:

- make `zone_prover_core::prove_zone_batch` the production Zone stateless prover;
- reuse battle-tested reth/revm/stateless-style execution and sparse-trie machinery;
- keep host/Nitro/native wrappers as proof packaging only;
- fail closed on incomplete witness data or commitment mismatches;
- avoid claiming e2e support until local witness generation can cover real dynamic batches.

## Checkpoint scope

- `prove_zone_batch` is no longer the empty-block placeholder. It constructs an `AlloyZoneBlockExecutor` and routes through `prove_zone_batch_with_executor`, which prepares a witness, executes prepared blocks through Alloy/revm, derives transaction roots, receipt roots, state roots, deposit queue state, Tempo commitment state, and withdrawal last-batch state from execution output.
- The old empty-block prover remains test-only behind `#[cfg(test)]` as `prove_empty_zone_batch`, which matches the goal's "test-only or removed from production paths" requirement.
- The execution plan converts `ZoneTempoImport::Advance` and `ZoneWithdrawalFinalization::Finalize` into system transactions, decodes user transactions, recovers signers, rejects creates/native value/EIP-7702, and currently restricts user calls to TIP-20 transfer selectors.
- `WitnessDatabase` is strict: accounts, storage, bytecode, and ancestor block hashes must be present in verified witness data. Missing reads fail closed instead of defaulting to zero.
- State-root calculation uses `reth_trie_sparse::SparseStateTrie` plus `reth_trie_common::HashedPostState`, so post-state root derivation moved toward the intended stateless/reth shape.
- Tempo L1 reads are served by `TempoWitnessProvider`, which verifies state proofs against bound Tempo roots and rejects unbound or duplicate reads.
- Nitro and native-signature paths call `prove_zone_batch` before signing proof material. The sequencer native path validates the prover output against the batch fields before submitting.
- `specs/spec.md` now documents `tempo_import`, `withdrawal_finalization`, required block env fields, the TIP-20-only user transaction restriction, and the execution-derived commitment flow.

## TODO status before calling the original goal complete

1. [ ] Replace the custom witness shape with an upstream-style execution witness adapter.
   Current code still starts from `ZoneAccountRead`, `ZoneStorageRead`, `proof_node_hashes`, and local proof conversion before revealing a sparse trie. The goal asked to reuse the `ExecutionWitness`/sparse-trie pattern as much as possible and avoid expanding hand-selected proof collection. Next step: introduce a Zone adapter around reth/alloy execution witness data, expose consumed Tempo reads as exact account/slot accesses, then keep custom formats only for Tempo L1 reads and Zone public inputs.

2. [ ] Finish local node witness generation for real dynamic batches.
   `LocalNodeProverWitnessSource` now derives the local pre-state read set for non-empty withdrawal finalization from canonical node state, and the integration sequencer helper uses the real local witness source instead of `UnavailableProverWitnessSource`. It still rejects dynamic `advanceTempo` deposits/decryptions/enabled tokens and user transactions because it cannot yet collect the needed proofs. It also emits empty `tempo_state_proofs`, so the node/sequencer path can only build local witnesses for static/header-only batches today, even though prover-core has production-path fixtures for deposits, encrypted deposits, transfers, and withdrawals.

   Settlement unblock checklist:

   - [ ] Replace `ensure_local_witness_source_coverage`'s dynamic-content rejections with real proof collection.
   - [ ] Extend local Zone pre-state proof collection beyond fixed system slots to every account, bytecode preimage, storage slot, and blockhash ancestor read by the decoded batch execution.
   - [ ] Collect final `ZonePortal.currentDepositQueueHash` storage proof material for batches that execute `advanceTempo`.
   - [ ] Collect Tempo/L1 state proofs for every `TempoStateReader` or L1 policy read bound to the imported Tempo roots instead of emitting empty `tempo_state_proofs`.
   - [ ] Support decoded `advanceTempo` calldata containing regular deposits, encrypted deposits with decryption data, and enabled tokens.
   - [ ] Support admitted user transactions with real sender/account/token-balance/policy witness reads.
   - [ ] Support `finalizeWithdrawalBatch(count > 0, encryptedSenders)` using the actual block-builder finalization transaction data through generated-witness replay. Partial evidence exists from the real signed L2 transaction regression and local pre-state read derivation, but not from L1 settlement.
   - [x] Expose enough local node provider state to integration sequencer helpers, or start the same production sequencer wiring in L1 e2e tests, so tests no longer use `UnavailableProverWitnessSource`. The L1 canary now reaches `LocalNodeProverWitnessSource` and fails at the dynamic `advanceTempo` proof guard instead of the unavailable-source guard.
   - [ ] Keep fail-closed behavior for any uncollected read, mismatched proof root, unsupported transaction class, or decoded block/pending batch mismatch.

3. [ ] Add an e2e generated-witness replay test.
   The goal explicitly requires a local L1 + zone-node e2e where the generated witness replays to the exact node block hash and state root. Current tests are concentrated in prover-core fixtures and sequencer/native proof plumbing. The integration helper now reaches the local node-backed witness source, but the canary still stops at the dynamic `advanceTempo` proof-collection guard, so it does not prove that a node-generated witness covers a real L1-backed deposit/user-transfer/withdrawal batch.

   Required evidence:

   - [ ] A local L1 + zone-node test where the sequencer builds a generated witness from canonical node state, runs `prove_zone_batch`, validates the output against the pending batch, submits `ZonePortal.submitBatch`, and observes a successful `BatchSubmitted` event.
   - [ ] The same path must cover at least one dynamic `advanceTempo` deposit batch, one admitted user transaction batch, and one non-empty withdrawal finalization batch.
   - [ ] Withdrawal replay must continue through `ZonePortal.processWithdrawal`, including the encrypted sender reveal case.

4. [ ] Audit system transaction execution semantics against live Zone contracts.
   The implementation executes `advanceTempo` and `finalizeWithdrawalBatch` as system calls through the Zone EVM path, which is the right direction. `zone-payload` now has a cross-crate regression (`prover_system_transactions_match_payload_builder_for_dynamic_shapes`) proving the payload builder and prover execution plan build byte-identical system transactions for mixed regular/encrypted `advanceTempo` data and encrypted withdrawal sender reveal finalization. `zone-node` also has a real signed L2 transaction regression (`test_withdrawal_reveal_to_finalization_uses_real_l2_transactions`) that submits `requestWithdrawal(..., revealTo)`, decodes the actual block-builder `finalizeWithdrawalBatch` system transaction, and decrypts its `encryptedSenders[0]` back to the request sender and tx hash. Still verify gas/accounting/log/receipt behavior for the remaining system transaction shapes, especially encrypted-deposit bounce-back and the full L1 `processWithdrawal` reveal path once local generated-witness wiring lets L1 e2e settlement run.

5. [x] Complete negative coverage for all commitment inputs.
   Production-path tests now cover deposit queue mutation (`production_prover_rejects_mutated_regular_deposit_queue_hash`), user transaction mutation (`production_prover_rejects_mutated_user_transaction_bytes`), withdrawal sender/count mismatch (`production_prover_rejects_withdrawal_sender_count_mismatch`), Zone state proof mutation (`production_prover_rejects_mutated_zone_state_proof`), and Tempo proof mutation (`production_prover_rejects_mutated_tempo_state_proof`). Receipt commitments are derived from execution output and covered by output-layer negative tests (`mutated_execution_receipt_changes_public_batch_commitment`, `alloy_execution_output_rejects_receipt_count_mismatch`) rather than a direct malformed-witness `prove_zone_batch` fixture, because receipts are not an input witness collection.

6. [ ] Make production prover-core fully `no_std`.
   CI now has a `prover-core-no-std` job that runs `cargo check -p zone-prover-core --no-default-features`, and `test-success` depends on it. That catches accidental `std` use in the no-default build, but the production sparse state-root/replay path currently depends on reth's `SparseStateTrie`, which is exported only with reth's `std` feature. The no-default build therefore uses fail-closed witness/state-root fallbacks; a real no-std production guest still needs a no-std sparse-trie backend or an upstream reth export.

7. [x] Tighten deposit queue proof binding.
   `batch_output_from_execution` now requires a proved final `PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT` value for batches containing `advanceTempo` and returns `MissingExecutionDepositQueueHashProof` when the final queue proof is omitted. The production Alloy/revm path also rejects an omitted portal queue proof during `advanceTempo` execution. The regression tests are `production_prover_rejects_missing_regular_deposit_queue_hash_proof` and `execution_output_rejects_advance_tempo_without_final_deposit_queue_proof`.

8. [x] Review user transaction scope versus final Zone semantics.
   The implemented and documented product scope is TIP-20 transfer-family user calls only. `execution_plan.rs` admits the transfer selectors and rejects system selectors, approve, TIP-403 policy proxy calls, native value, creates, and EIP-7702 authorizations; prover-core production tests also reject native-value transfers and missing L1 policy witness data. Expanding beyond this scope would be a spec change, not an implementation gap in the current spec.

9. [x] Keep verifier policy/state as the enforcement layer.
   The native sequencer path reads the portal's verifier address, checks `NativeSignatureVerifier.policies(portal)` for the expected signer and verifier version, signs the digest over the fresh `prove_zone_batch` output, and preflights `verify` before submit. The Solidity verifier keys policy by `msg.sender`/portal, requires matching chain id, protocol version, portal, enabled signer, and verifier version, so request-controlled verifier config or stale proof material cannot satisfy the policy path.

## Suggested follow-up commits

- Land this checkpoint as "make native/Nitro proofs execute witness-backed Zone batches" without claiming full e2e dynamic witness coverage.
- Add local node witness generation for dynamic `advanceTempo`, user transactions, and withdrawal finalization.
- Add local L1 + zone-node e2e coverage proving generated witnesses replay to node roots.
- Replace hand-selected Zone proof collection with an upstream-style execution witness adapter.
