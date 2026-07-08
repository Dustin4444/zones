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
- keep e2e claims tied to generated witnesses built from real node/L1 state.

## Checkpoint scope

- `prove_zone_batch` is no longer the empty-block placeholder. It constructs an `AlloyZoneBlockExecutor` and routes through `prove_zone_batch_with_executor`, which prepares a witness, executes prepared blocks through Alloy/revm, derives transaction roots, receipt roots, state roots, deposit queue state, Tempo commitment state, and withdrawal last-batch state from execution output.
- The old empty-block prover remains test-only behind `#[cfg(test)]` as `prove_empty_zone_batch`, which matches the goal's "test-only or removed from production paths" requirement.
- The execution plan converts `ZoneTempoImport::Advance` and `ZoneWithdrawalFinalization::Finalize` into system transactions, decodes user transactions, recovers signers, rejects creates/native value/EIP-7702, and currently admits TIP-20 transfer/approval selectors plus `ZoneOutbox.requestWithdrawal`.
- `WitnessDatabase` is strict: accounts, storage, bytecode, and ancestor block hashes must be present in verified witness data. Missing reads fail closed instead of defaulting to zero.
- State-root calculation uses `reth_trie_sparse::SparseStateTrie` plus `reth_trie_common::HashedPostState`, so post-state root derivation moved toward the intended stateless/reth shape.
- Tempo L1 reads are served by `TempoWitnessProvider`, which verifies state proofs against bound Tempo roots and rejects unbound or duplicate reads.
- Nitro and native-signature paths call `prove_zone_batch` before signing proof material. The sequencer native path validates the prover output against the batch fields before submitting.
- `specs/spec.md` now documents `tempo_import`, `withdrawal_finalization`, required block env fields, the admitted user transaction scope, enabled-token fee accounting, and the execution-derived commitment flow.

## TODO status before calling the original goal complete

1. [ ] Replace the custom witness shape with an upstream-style execution witness adapter.
   Current code still starts from `ZoneAccountRead`, `ZoneStorageRead`, `proof_node_hashes`, and local proof conversion before revealing a sparse trie. The goal asked to reuse the `ExecutionWitness`/sparse-trie pattern as much as possible and avoid expanding hand-selected proof collection. Next step: introduce a Zone adapter around reth/alloy execution witness data, expose consumed Tempo reads as exact account/slot accesses, then keep custom formats only for Tempo L1 reads and Zone public inputs.

2. [x] Finish local node witness generation for real dynamic batches.
   `LocalNodeProverWitnessSource` now derives the local pre-state read set from canonical node state, executes the decoded batch once over a recording database, collects every Zone account/storage/code read observed by execution, records post-state proof targets needed by output derivation, and records Tempo/L1 reads when an `L1StateProvider` is attached. Generated witnesses are then replayed against canonical node headers and dynamic missing-read errors are closed over before submission. The integration sequencer helper uses this real local witness source instead of `UnavailableProverWitnessSource`.

   Settlement unblock checklist:

   - [x] Replace `ensure_local_witness_source_coverage`'s dynamic-content rejections with real proof collection.
   - [x] Extend local Zone pre-state proof collection beyond fixed system slots to account, bytecode preimage, storage slot, post-state proof target, and blockhash ancestor reads observed by decoded batch execution.
   - [x] Collect final `ZonePortal.currentDepositQueueHash` storage proof material for batches that execute `advanceTempo`.
   - [x] Collect Tempo/L1 state proofs for `TempoStateReader`, portal enabled-token, and builtin transfer-policy reads bound to the imported Tempo roots when the local source has an `L1StateProvider`.
   - [x] Support decoded `advanceTempo` calldata containing regular deposits, encrypted deposits with decryption data, and enabled tokens.
   - [x] Support admitted user transactions with real sender/account/token-balance/builtin-policy witness reads.
   - [x] Support `finalizeWithdrawalBatch(count > 0, encryptedSenders)` using the actual block-builder finalization transaction data through generated-witness replay.
   - [x] Expose enough local node provider state to integration sequencer helpers, or start the same production sequencer wiring in L1 e2e tests, so tests no longer use `UnavailableProverWitnessSource`. The L1 e2e path now reaches `LocalNodeProverWitnessSource`, builds a generated witness for dynamic blocks, and settles the batch.
   - [x] Keep fail-closed behavior for any uncollected read, mismatched proof root, unsupported transaction class, or decoded block/pending batch mismatch.

3. [x] Add an e2e generated-witness replay test.
   `zone-node` now has a local L1 + zone-node e2e (`l1_e2e::test_encrypted_deposit_and_withdrawal`) where the sequencer builds a generated witness from canonical node state, runs the production prover path, validates the output against the pending batch, submits `ZonePortal.submitBatch`, observes `BatchSubmitted`, processes the withdrawal on L1, and verifies the authenticated encrypted sender reveal.

   Required evidence:

   - [x] A local L1 + zone-node test where the sequencer builds a generated witness from canonical node state, runs `prove_zone_batch`, validates the output against the pending batch, submits `ZonePortal.submitBatch`, and observes a successful `BatchSubmitted` event.
   - [x] The same path covers at least one dynamic encrypted `advanceTempo` deposit batch, one admitted user transaction batch (`approve` + `requestWithdrawal`), and one non-empty withdrawal finalization batch.
   - [x] Withdrawal replay continues through `ZonePortal.processWithdrawal`, including the encrypted sender reveal case.

4. [ ] Audit system transaction execution semantics against live Zone contracts.
   The implementation executes `advanceTempo` and `finalizeWithdrawalBatch` as system calls through the Zone EVM path, which is the right direction. `zone-payload` now has a cross-crate regression (`prover_system_transactions_match_payload_builder_for_dynamic_shapes`) proving the payload builder and prover execution plan build byte-identical system transactions for mixed regular/encrypted `advanceTempo` data and encrypted withdrawal sender reveal finalization. `zone-node` also has a real signed L2 transaction regression (`test_withdrawal_reveal_to_finalization_uses_real_l2_transactions`) that submits `requestWithdrawal(..., revealTo)`, decodes the actual block-builder `finalizeWithdrawalBatch` system transaction, and decrypts its `encryptedSenders[0]` back to the request sender and tx hash. The local L1 e2e path now reaches full generated-witness settlement and `processWithdrawal` for the encrypted sender reveal case. Still verify gas/accounting/log/receipt behavior for the remaining system transaction shapes, especially encrypted-deposit bounce-back and failed-withdrawal bounce-back/refund paths.

5. [x] Complete negative coverage for all commitment inputs.
   Production-path tests now cover deposit queue mutation (`production_prover_rejects_mutated_regular_deposit_queue_hash`), user transaction mutation (`production_prover_rejects_mutated_user_transaction_bytes`), withdrawal sender/count mismatch (`production_prover_rejects_withdrawal_sender_count_mismatch`), Zone state proof mutation (`production_prover_rejects_mutated_zone_state_proof`), and Tempo proof mutation (`production_prover_rejects_mutated_tempo_state_proof`). Receipt commitments are derived from execution output and covered by output-layer negative tests (`mutated_execution_receipt_changes_public_batch_commitment`, `alloy_execution_output_rejects_receipt_count_mismatch`) rather than a direct malformed-witness `prove_zone_batch` fixture, because receipts are not an input witness collection.

6. [ ] Make production prover-core fully `no_std`.
   CI now has a `prover-core-no-std` job that runs `cargo check -p zone-prover-core --no-default-features`, and `test-success` depends on it. That catches accidental `std` use in the no-default build, but the production sparse state-root/replay path currently depends on reth's `SparseStateTrie`, which is exported only with reth's `std` feature. The no-default build therefore uses fail-closed witness/state-root fallbacks; a real no-std production guest still needs a no-std sparse-trie backend or an upstream reth export.

7. [x] Tighten deposit queue proof binding.
   `batch_output_from_execution` now requires a proved final `PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT` value for batches containing `advanceTempo` and returns `MissingExecutionDepositQueueHashProof` when the final queue proof is omitted. The production Alloy/revm path also rejects an omitted portal queue proof during `advanceTempo` execution. The regression tests are `production_prover_rejects_missing_regular_deposit_queue_hash_proof` and `execution_output_rejects_advance_tempo_without_final_deposit_queue_proof`.

8. [x] Review user transaction scope versus final Zone semantics.
   The implemented and documented product scope is TIP-20 transfer-family calls, TIP-20 `approve`, and `ZoneOutbox.requestWithdrawal`. `execution_plan.rs` admits those selectors and rejects non-admitted system selectors, direct TIP-403 policy proxy calls, native value, creates, and EIP-7702 authorizations; prover-core production tests also reject native-value transfers and missing L1 policy witness data. Expanding beyond this scope would be a spec change, not an implementation gap in the current spec.

9. [x] Keep verifier policy/state as the enforcement layer.
   The native sequencer path reads the portal's verifier address, checks `NativeSignatureVerifier.policies(portal)` for the expected signer and verifier version, signs the digest over the fresh `prove_zone_batch` output, and preflights `verify` before submit. The Solidity verifier keys policy by `msg.sender`/portal, requires matching chain id, protocol version, portal, enabled signer, and verifier version, so request-controlled verifier config or stale proof material cannot satisfy the policy path.

10. [x] Implement full witness-backed TIP-403 policy evaluation.
    Witness-backed policy providers now resolve the TIP-20 `transferPolicyId`, `TIP403Registry.policy_id_counter`, `policy_records`, `policy_set`, and compound sub-policy storage directly from Tempo state proofs bound to the imported Tempo roots. Builtin reject-all/allow-all, whitelist, blacklist, and compound sender/recipient/mint-recipient checks all use the same fail-closed L1 read path during prover execution and local witness read collection. Unit coverage in `zone-prover-core execution_policy` exercises builtin, missing-proof, whitelist, blacklist, and compound policy behavior.

11. [x] Align Zone fee-token semantics with the Zone fee manager direction.
    Zones use `ZoneFeeManager`: any USD TIP-20 token enabled on the L1 portal can pay fees, FeeAMM liquidity routing is disabled, and validator fees are credited directly in the user's fee token. pathUSD remains initialized in genesis as the default/reserved TIP-20 so Tempo fee-token storage expectations are valid at boot, but it is not the only runtime fee token.

12. [ ] Implement the target network-upgrade verifier rotation semantics.
    The spec's Network Upgrades section is still explicitly target design: the current `ZonePortal` keeps `verifier` immutable, while the target design needs active/fork verifier slots, fork activation cutoffs, and protocol-version rotation. Until that is implemented, the prover checkpoint can be complete for the current immutable-verifier contract set but not for every future-upgrade requirement in the spec.

## Suggested follow-up commits

- Land the current checkpoint with the remaining gaps scoped to upstream witness-shape cleanup, no-std production sparse trie support, network-upgrade verifier rotation, and the remaining live-contract semantics audit.
- Replace hand-selected Zone proof collection with an upstream-style execution witness adapter.
- Finish the remaining live-contract gas/accounting/log/receipt audit cases.
- Implement the target fork verifier/protocol-version rotation path, or split it into a later network-upgrade milestone if immutable-verifier portals remain the current product scope.
