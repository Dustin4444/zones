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

## Open TODOs before calling the original goal complete

1. [ ] Replace the custom witness shape with an upstream-style execution witness adapter.
   Current code still starts from `ZoneAccountRead`, `ZoneStorageRead`, `proof_node_hashes`, and local proof conversion before revealing a sparse trie. The goal asked to reuse the `ExecutionWitness`/sparse-trie pattern as much as possible and avoid expanding hand-selected proof collection. Next step: introduce a Zone adapter around reth/alloy execution witness data, then keep custom formats only for Tempo L1 reads and Zone public inputs.

2. [ ] Finish local node witness generation for real dynamic batches.
   `LocalNodeProverWitnessSource` still rejects dynamic `advanceTempo` deposits/decryptions/enabled tokens, user transactions, and non-zero withdrawal finalization because it cannot yet collect the needed proofs. That means the node/sequencer path can only build local witnesses for static/header-only batches today, even though prover-core has unit fixtures for deposits, encrypted deposits, transfers, and withdrawals.

3. [ ] Add an e2e generated-witness replay test.
   The goal explicitly requires a local L1 + zone-node e2e where the generated witness replays to the exact node block hash and state root. Current tests appear concentrated in prover-core fixtures and sequencer/native proof plumbing; they do not prove that the node-generated witness covers a real L1-backed deposit/user-transfer/withdrawal batch.

4. [ ] Audit system transaction execution semantics against live Zone contracts.
   The implementation executes `advanceTempo` and `finalizeWithdrawalBatch` as system calls through the Zone EVM path, which is the right direction. Still verify gas/accounting/log/receipt behavior against the node's actual block builder for every system transaction shape, especially encrypted-deposit bounce-back and withdrawal sender reveal cases.

5. [ ] Complete negative coverage for all commitment inputs.
   Existing tests cover several mutations, but the original goal calls out deposit, tx, receipt, state proof, Tempo proof, and withdrawal count mutations. Confirm each category has a failing test that exercises the production `prove_zone_batch` path, not only the test-only empty prover.

6. [ ] Verify `no_std` continuously.
   `zone-prover-core` is structured as a `no_std`-oriented crate, but the recovered goal requires an explicit no-std compile check. Add a CI command or crate-specific check that builds `zone-prover-core` without default features and catches accidental `std` dependencies in the production prover path.

7. [ ] Tighten deposit queue proof binding.
   The current output validation checks any proved final `PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT` value against the execution-derived final deposit queue hash. Make this mandatory for batches with `advanceTempo` so a witness cannot omit the final portal queue proof and still pass a deposit-processing batch.

8. [ ] Review user transaction scope versus final Zone semantics.
   The current implementation intentionally admits only TIP-20 transfer-family calls. That matches the new spec text in this diff, but it is narrower than "execute all user transactions as Tempo transactions" in the original full-prover goal. Keep this restriction only if it is the intended product scope; otherwise expand and test the admission model.

9. [ ] Keep verifier policy/state as the enforcement layer.
   The native verifier flow now checks portal policy and signs a digest over `prove_zone_batch` output. Before production use, verify deployed verifier state pins approved signer/version policy and cannot be satisfied by request-controlled verifier config or stale prebuilt proof material.

## Suggested follow-up commits

- Land this checkpoint as "make native/Nitro proofs execute witness-backed Zone batches" without claiming full e2e dynamic witness coverage.
- Add local node witness generation for dynamic `advanceTempo`, user transactions, and withdrawal finalization.
- Add local L1 + zone-node e2e coverage proving generated witnesses replay to node roots.
- Replace hand-selected Zone proof collection with an upstream-style execution witness adapter.
