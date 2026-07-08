# Nitro Zone Prover

This repo includes a minimal AWS Nitro backend for the zone batch prover in `crates/prover`.
The backend calls the prover core in `crates/prover-core`; Nitro code is responsible only for transport, attestation, signing, and host-side proof packaging. The crate has a no-default compile check, but the current production sparse state-root path uses reth's std-gated sparse trie.

The enclave binary exposes one operation over vsock:

- receive a `BatchWitness`
- run `prove_zone_batch(witness)` inside the enclave
- sign only the resulting `BatchOutput`
- return `verifierConfig`, `proof`, and `BatchOutput` material for `ZonePortal.submitBatch`

The current `prove_zone_batch` implementation is the production state transition entry point. It validates witness-backed Zone state proofs, verifies Tempo L1 state proofs against bound Tempo roots, executes prepared Zone blocks through Alloy/revm, and derives block, transaction, receipt, state, deposit-queue, Tempo-binding, and withdrawal-batch commitments from execution output. Production fixtures cover header imports, regular deposits, encrypted deposits, TIP-20 transfer-family user transactions, and non-zero withdrawal finalization. Unsupported or incomplete witness data is rejected instead of defaulting to zero state or signing caller-supplied outputs.

Local node-generated witnesses are still narrower than the prover core. The local `LocalNodeProverWitnessSource` currently builds static/header-only witnesses, derives the needed pre-state reads for non-empty withdrawal finalization, and rejects dynamic deposit, enabled-token, and user-transaction batches until it can collect the corresponding Zone and Tempo proof material from the node. Integration sequencer tests now use this real local source rather than `UnavailableProverWitnessSource`; the current L1 settlement canary reaches the dynamic `advanceTempo` proof guard.

## Prover Core

`zone-prover-core` exposes:

```rust
pub fn prove_zone_batch(witness: BatchWitness) -> Result<BatchOutput, ProverError>
```

The crate builds with `--no-default-features` and does not use host IO, networking, files, wall-clock time, randomness, async runtimes, or Nitro-specific APIs. Native, ZKVM, and TEE backends should all call this same function and then wrap the returned `BatchOutput` in their backend-specific proof envelope.

## Build

Prerequisites on the Nitro parent instance:

- Docker
- `nitro-cli`
- `just`

Build the enclave image:

```bash
just nitro-prover-image
```

Build the EIF:

```bash
just nitro-prover-eif
```

`nitro-cli build-enclave` prints PCR measurements. Use the printed PCR0/PCR1/PCR2 hex values when registering the enclave and in the `NitroVerifierConfig` JSON that the host passes to `zone-prover-host`.

## Run

Start the enclave:

```bash
just nitro-prover-run
```

Ask the enclave for registration material and require it to match the EIF PCR pins:

```bash
just nitro-prover-register "$PCR0" "$PCR1" "$PCR2" 16 5005 build/zone-prover-registration.json
```

The equivalent host command is:

```bash
cargo run -p zone-prover --bin zone-prover-host -- register \
  --cid 16 \
  --port 5005 \
  --expected-pcr0 "$PCR0" \
  --expected-pcr1 "$PCR1" \
  --expected-pcr2 "$PCR2" \
  --out build/zone-prover-registration.json
```

Send a witness and verifier config:

```bash
just nitro-prover-prove 16 witness.json verifier-config.json build/zone-prover-proof.json
```

Submit the resulting host proof JSON to the portal:

```bash
export L1_RPC_URL="https://..."
export L1_PORTAL_ADDRESS="0x..."
export PRIVATE_KEY="0x..."
just nitro-prover-submit build/zone-prover-proof.json
```

Example verifier config:

```json
{
  "version": 1,
  "chainId": 421700001,
  "portalAddress": "0x0000000000000000000000000000000000001000",
  "verifierVersion": 1,
  "expectedSigner": "0x1111111111111111111111111111111111111111",
  "expectedPcr0": "0x...",
  "expectedPcr1": "0x...",
  "expectedPcr2": "0x..."
}
```

Nitro PCR values are SHA-384 measurements, so each PCR pin is 48 bytes. The host CLI and verifier config parse PCRs as fixed 48-byte values; malformed or short values fail before a request is sent or a registration report is accepted.

## Signing Key Handling

Ephemeral enclave signing keys are generated with libsecp256k1's `SecretKey::new` using an OS-backed CSPRNG. Imported keys are parsed as exactly 32 bytes and then validated by libsecp256k1, so short keys, zero keys, and out-of-range scalar values are rejected.

The enclave supports `--ephemeral-key` for local testing and `--key-file` or `ZONE_PROVER_PRIVATE_KEY_HEX` when a stable signer is needed. In-process string copies of private keys are wrapped in `Zeroizing`, and the signing key wrapper erases the libsecp256k1 secret key on drop using the crate's best-effort erase API. This is defense in depth, not a substitute for treating the Nitro enclave and any persistent key file as sensitive.

## Attestation Binding

The production enclave image is built with the `zone-prover/nsm-driver` feature and uses AWS's `aws-nitro-enclaves-nsm-api` crate to request attestation documents from `/dev/nsm`.

For proofs, the NSM request sets:

- `user_data` to the signed batch digest
- `nonce` to the host request nonce
- `public_key` to the enclave secp256k1 signing public key

For registration reports, the enclave extracts PCR0/PCR1/PCR2 from NSM-attested material, then sets `user_data` to a registration challenge digest that includes the signer, public key, request id, nonce, and those PCR pins.

The host uses the AWS-owned `nitro_attest` crate to verify the AWS Nitro root, certificate chain, certificate validity, and COSE signature before accepting a Nitro attestation document. After cryptographic document verification, the host checks nonce, user data, public key, and PCR bindings. PCR pins are 48-byte SHA-384 values and are rejected if they have any other length.

Keep registration pinned to operator-reviewed PCR0/PCR1/PCR2 measurements from the exact EIF build. Attestation proves the enclave image and request binding; it does not prove that an unreviewed measurement is safe.

## Local Testing

`ZonePortal.submitBatch` should not be tested against a placeholder verifier or
a Solidity MPT verifier. Local end-to-end tests use
`NativeSignatureVerifier`, which pins an approved signer per portal and verifies a
canonical ABI-encoded signature over the exact public inputs and output
commitments submitted to the portal. The Rust prover still verifies the witness
state proofs before producing those outputs; the Solidity verifier only checks
that the approved local proof backend signed the same values the portal will
apply.

Nitro attestation should be tested separately with captured COSE/X.509
attestation fixtures. Those fixtures exercise AWS Nitro certificate-chain
validation, COSE signature validation, nonce/user_data/public_key binding, and
PCR pinning without requiring live AWS Nitro hardware. A captured attestation is
only valid for the exact digest and request bytes embedded in the fixture, so it
does not replace the native verifier path for fresh local batch e2e tests.

### Captured Nitro Fixture Tests

Capture fixtures once from a real Nitro enclave, then run the verifier tests
offline on any machine. Use a non-production signer and fixed request IDs/nonces
so the fixture is reproducible and does not expose production key material.

1. Build and run the EIF on a Nitro parent instance.
2. Record the EIF PCR0/PCR1/PCR2 values printed by `nitro-cli build-enclave`.
3. Start the enclave with a test-only key source.
4. Run `zone-prover-host register` with fixed `--request-id`, fixed `--nonce`,
   and the expected PCR pins. Save the full `RegistrationReport` JSON.
5. Run `zone-prover-host prove` with a committed minimal witness, fixed
   `--request-id`, fixed `--nonce`, and a verifier config whose
   `expectedSigner` is the registration signer. Save the request/config and the
   returned proof envelope or `HostProof` JSON.
6. Commit only public fixture material: request JSON, proof/registration JSON,
   expected PCRs, public key/signature material, and the Unix timestamp used for
   certificate validation. Do not commit private keys or AWS instance metadata.

The Rust tests should:

- call `verify_attestation_doc_at(attestation_doc, fixture_time)` so certificate
  validation is deterministic
- call `validate_attestation_doc` with the fixture's exact nonce, user_data,
  public key, and PCR0/PCR1/PCR2
- call `verify_registration_report` for the captured registration report
- call `verify_proof_response` or `encode_host_proof` for the captured proof
  request/envelope pair
- mutate one field at a time and assert rejection: nonce, user_data/digest,
  public key, signer, PCR0/PCR1/PCR2, request ID, and signed batch output

This proves the Nitro attestation verifier and request binding logic without a
live enclave. It does not prove fresh local batches because NSM attestation signs
the exact request bytes; local e2e tests still need `NativeSignatureVerifier`.

Run the enclave server over TCP without attestation to exercise transport and fail-closed error handling:

```bash
cargo run -p zone-prover --bin zone-prover-enclave -- \
  --tcp-listen 127.0.0.1:5005 \
  --ephemeral-key \
  --attestation-provider none
```

Then use the host with `--tcp 127.0.0.1:5005` instead of `--cid`. Proof and registration requests intentionally fail unless the enclave has a valid NSM attestation provider or a valid Nitro attestation fixture.
