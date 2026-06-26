//! Zone batch prover backend wrappers and Nitro host/enclave protocol.
//!
//! The portable state transition entry point lives in `zone-prover-core`.
//! Backend-specific code in this crate serializes inputs, calls
//! [`prove_zone_batch`], and wraps the resulting [`BatchOutput`].

pub mod attestation;
pub mod crypto;
pub mod protocol;
pub mod server;
pub mod transport;
pub mod types;

#[cfg(test)]
pub(crate) mod test_utils;

pub use protocol::{
    HostProof, NATIVE_CONFIG_DOMAIN, NATIVE_DIGEST_DOMAIN, NITRO_SHA384_PCR_BYTES,
    NativeVerifierConfig, NitroPcr, NitroVerifierConfig, ProofEnvelope, ProofRequest,
    RegistrationReport, encode_host_proof, encode_native_host_proof, native_batch_digest,
    parse_b256_hex, parse_nitro_pcr_hex, verify_proof_response, verify_registration_report,
};
pub use server::{ProverServer, SignerConfig, SigningKeySource};
pub use types::{BatchOutput, BatchWitness, ProverError, prove_zone_batch};
