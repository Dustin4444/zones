use alloy_primitives::{Address, B256, Bytes, FixedBytes, keccak256};
use alloy_sol_types::SolValue;
use serde::{Deserialize, Serialize};

use crate::{
    attestation::{ExpectedAttestation, validate_attestation_doc, verify_attestation_doc},
    crypto::{
        CryptoError, RecoverableSignatureBytes, address_from_uncompressed_public_key,
        recover_address,
    },
    types::{BatchOutput, BatchWitness, PublicInputs},
};

pub const PROTOCOL_VERSION: u16 = 1;
pub const DIGEST_DOMAIN: &str = "tempo.zone.prover.batch.v1";
pub const NATIVE_DIGEST_DOMAIN: &str = "tempo.zone.native.verifier.batch.v1";
pub const NATIVE_CONFIG_DOMAIN: &str = "tempo.zone.native.verifier.config.v1";
pub const REGISTRATION_DOMAIN: &str = "tempo.zone.prover.registration.v1";
pub const NITRO_SHA384_PCR_BYTES: usize = 48;
pub type NitroPcr = FixedBytes<NITRO_SHA384_PCR_BYTES>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NitroVerifierConfig {
    pub version: u16,
    pub chain_id: u64,
    pub portal_address: Address,
    pub verifier_version: u64,
    pub expected_signer: Address,
    pub expected_pcr0: NitroPcr,
    pub expected_pcr1: NitroPcr,
    pub expected_pcr2: NitroPcr,
}

impl NitroVerifierConfig {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(self.version));
        }
        Ok(())
    }

    pub fn digest(&self) -> B256 {
        keccak256(
            (
                keccak256(b"tempo.zone.nitro.verifier.config.v1"),
                self.version,
                self.chain_id,
                self.portal_address,
                self.verifier_version,
                self.expected_signer,
                pcr_hash(&self.expected_pcr0),
                pcr_hash(&self.expected_pcr1),
                pcr_hash(&self.expected_pcr2),
            )
                .abi_encode_params(),
        )
    }

    pub fn to_verifier_config_bytes(&self) -> Result<Bytes, ProtocolError> {
        Ok(Bytes::from(serde_json::to_vec(self)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeVerifierConfig {
    pub version: u16,
    pub chain_id: u64,
    pub portal_address: Address,
    pub verifier_version: u64,
}

impl NativeVerifierConfig {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(self.version));
        }
        if self.verifier_version == 0 {
            return Err(ProtocolError::InvalidNativeVerifierVersion);
        }
        Ok(())
    }

    pub fn digest(&self) -> B256 {
        keccak256(
            (
                keccak256(NATIVE_CONFIG_DOMAIN.as_bytes()),
                self.version,
                self.chain_id,
                self.portal_address,
                self.verifier_version,
            )
                .abi_encode_params(),
        )
    }

    pub fn to_verifier_config_bytes(&self) -> Result<Bytes, ProtocolError> {
        self.validate()?;
        Ok(Bytes::from(
            (
                self.version,
                self.chain_id,
                self.portal_address,
                self.verifier_version,
            )
                .abi_encode_params(),
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofRequest {
    pub version: u16,
    pub request_id: B256,
    pub nonce: B256,
    pub verifier_config: NitroVerifierConfig,
    pub witness: BatchWitness,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofEnvelope {
    pub version: u16,
    pub request_id: B256,
    pub nonce: B256,
    pub digest: B256,
    pub signer: Address,
    pub public_key_uncompressed: Bytes,
    pub signature: RecoverableSignatureBytes,
    pub output: BatchOutput,
    /// Raw COSE_Sign1 Nitro attestation document. Host-side verification checks
    /// the AWS Nitro root, certificate chain, COSE signature, request bindings,
    /// public key, and PCR pins.
    pub attestation_doc: Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationReport {
    pub version: u16,
    pub request_id: B256,
    pub nonce: B256,
    pub signer: Address,
    pub public_key_uncompressed: Bytes,
    pub expected_pcr0: NitroPcr,
    pub expected_pcr1: NitroPcr,
    pub expected_pcr2: NitroPcr,
    pub attestation_doc: Bytes,
    pub digest: B256,
    pub signature: RecoverableSignatureBytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostProof {
    pub tempo_block_number: u64,
    /// `0` for direct EIP-2935 lookup, otherwise the recent Tempo block passed
    /// as `recentTempoBlockNumber` to `ZonePortal.submitBatch`.
    pub recent_tempo_block_number: u64,
    pub verifier_config: Bytes,
    pub proof: Bytes,
    pub output: BatchOutput,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("proof response request_id does not match request")]
    RequestIdMismatch,
    #[error("proof response nonce does not match request")]
    NonceMismatch,
    #[error("proof digest mismatch")]
    DigestMismatch,
    #[error("registration digest mismatch")]
    RegistrationDigestMismatch,
    #[error("proof signer {actual} does not match expected {expected}")]
    SignerMismatch { expected: Address, actual: Address },
    #[error("invalid native verifier version")]
    InvalidNativeVerifierVersion,
    #[error("native proof output {field} does not match public inputs")]
    NativeOutputMismatch { field: &'static str },
    #[error("invalid native proof recovery id {0}; expected 0/1 or 27/28")]
    InvalidNativeRecoveryId(u8),
    #[error("attestation error: {0}")]
    Attestation(#[from] crate::attestation::AttestationError),
    #[error("signature recovery failed: {0}")]
    SignatureRecovery(#[from] crate::crypto::CryptoError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn batch_digest(request: &ProofRequest, output: &BatchOutput) -> B256 {
    let public = &request.witness.public_inputs;
    let config = &request.verifier_config;
    keccak256(
        (
            keccak256(DIGEST_DOMAIN.as_bytes()),
            request.request_id,
            request.nonce,
            config.chain_id,
            config.portal_address,
            config.verifier_version,
            config.digest(),
            public.sequencer,
            public.prev_block_hash,
            public.tempo_block_number,
            public.anchor_block_number,
            public.anchor_block_hash,
            public.expected_withdrawal_batch_index,
            output.block_transition.prevBlockHash,
            output.block_transition.nextBlockHash,
            output.deposit_queue_transition.prevProcessedHash,
            output.deposit_queue_transition.nextProcessedHash,
            output.deposit_queue_transition.prevDepositNumber,
            output.deposit_queue_transition.nextDepositNumber,
            output.withdrawal_queue_hash,
            output.last_batch_commitment.withdrawal_queue_hash,
            output.last_batch_commitment.withdrawal_batch_index,
            output.digest(),
        )
            .abi_encode_params(),
    )
}

pub fn native_batch_digest(
    config: &NativeVerifierConfig,
    signer: Address,
    public: &PublicInputs,
    output: &BatchOutput,
) -> Result<B256, ProtocolError> {
    config.validate()?;
    validate_native_output(public, output)?;
    let output_digest = native_output_digest(public, output);
    Ok(keccak256(
        (
            keccak256(NATIVE_DIGEST_DOMAIN.as_bytes()),
            config.digest(),
            signer,
            public.tempo_block_number,
            public.anchor_block_number,
            public.anchor_block_hash,
            public.sequencer,
            output_digest,
        )
            .abi_encode_params(),
    ))
}

fn native_output_digest(public: &PublicInputs, output: &BatchOutput) -> B256 {
    keccak256(
        (
            output.block_transition.prevBlockHash,
            output.block_transition.nextBlockHash,
            output.deposit_queue_transition.prevProcessedHash,
            output.deposit_queue_transition.nextProcessedHash,
            output.deposit_queue_transition.prevDepositNumber,
            output.deposit_queue_transition.nextDepositNumber,
            output.withdrawal_queue_hash,
            output.withdrawal_queue_hash,
            public.expected_withdrawal_batch_index,
        )
            .abi_encode_params(),
    )
}

fn validate_native_output(
    public: &PublicInputs,
    output: &BatchOutput,
) -> Result<(), ProtocolError> {
    if output.block_transition.prevBlockHash != public.prev_block_hash {
        return Err(ProtocolError::NativeOutputMismatch {
            field: "prevBlockHash",
        });
    }
    if output.last_batch_commitment.withdrawal_queue_hash != output.withdrawal_queue_hash {
        return Err(ProtocolError::NativeOutputMismatch {
            field: "withdrawalQueueHash",
        });
    }
    if output.last_batch_commitment.withdrawal_batch_index != public.expected_withdrawal_batch_index
    {
        return Err(ProtocolError::NativeOutputMismatch {
            field: "withdrawalBatchIndex",
        });
    }
    Ok(())
}

pub fn registration_digest(report: &RegistrationReport) -> B256 {
    registration_digest_inner(report, keccak256(report.attestation_doc.as_ref()))
}

pub fn registration_challenge_digest(report: &RegistrationReport) -> B256 {
    registration_digest_inner(report, B256::ZERO)
}

fn registration_digest_inner(report: &RegistrationReport, attestation_doc_hash: B256) -> B256 {
    keccak256(
        (
            keccak256(REGISTRATION_DOMAIN.as_bytes()),
            report.version,
            report.request_id,
            report.nonce,
            report.signer,
            keccak256(report.public_key_uncompressed.as_ref()),
            pcr_hash(&report.expected_pcr0),
            pcr_hash(&report.expected_pcr1),
            pcr_hash(&report.expected_pcr2),
            attestation_doc_hash,
        )
            .abi_encode_params(),
    )
}

fn pcr_hash(pcr: &NitroPcr) -> B256 {
    keccak256::<&[u8]>(pcr.as_ref())
}

pub fn verify_proof_response(
    request: &ProofRequest,
    envelope: &ProofEnvelope,
) -> Result<(), ProtocolError> {
    if request.version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(request.version));
    }
    request.verifier_config.validate()?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(envelope.version));
    }
    if envelope.request_id != request.request_id {
        return Err(ProtocolError::RequestIdMismatch);
    }
    if envelope.nonce != request.nonce {
        return Err(ProtocolError::NonceMismatch);
    }
    let expected_digest = batch_digest(request, &envelope.output);
    if envelope.digest != expected_digest {
        return Err(ProtocolError::DigestMismatch);
    }

    let recovered = recover_address(envelope.digest, &envelope.signature)?;
    if recovered != envelope.signer {
        return Err(ProtocolError::SignerMismatch {
            expected: envelope.signer,
            actual: recovered,
        });
    }
    let public_key_signer =
        address_from_uncompressed_public_key(envelope.public_key_uncompressed.as_ref())?;
    if public_key_signer != envelope.signer {
        return Err(ProtocolError::SignerMismatch {
            expected: envelope.signer,
            actual: public_key_signer,
        });
    }
    if recovered != request.verifier_config.expected_signer {
        return Err(ProtocolError::SignerMismatch {
            expected: request.verifier_config.expected_signer,
            actual: recovered,
        });
    }
    let decoded = verify_attestation_doc(envelope.attestation_doc.as_ref())?;
    validate_attestation_doc(
        &decoded,
        ExpectedAttestation {
            user_data: envelope.digest.as_slice(),
            nonce: request.nonce.as_slice(),
            public_key: envelope.public_key_uncompressed.as_ref(),
            expected_pcr0: request.verifier_config.expected_pcr0.as_ref(),
            expected_pcr1: request.verifier_config.expected_pcr1.as_ref(),
            expected_pcr2: request.verifier_config.expected_pcr2.as_ref(),
        },
    )?;

    Ok(())
}

pub fn verify_registration_report(report: &RegistrationReport) -> Result<(), ProtocolError> {
    if report.version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(report.version));
    }
    let expected_digest = registration_digest(report);
    if report.digest != expected_digest {
        return Err(ProtocolError::RegistrationDigestMismatch);
    }

    let recovered = recover_address(report.digest, &report.signature)?;
    if recovered != report.signer {
        return Err(ProtocolError::SignerMismatch {
            expected: report.signer,
            actual: recovered,
        });
    }
    let public_key_signer =
        address_from_uncompressed_public_key(report.public_key_uncompressed.as_ref())?;
    if public_key_signer != report.signer {
        return Err(ProtocolError::SignerMismatch {
            expected: report.signer,
            actual: public_key_signer,
        });
    }

    let decoded = verify_attestation_doc(report.attestation_doc.as_ref())?;
    let challenge_digest = registration_challenge_digest(report);
    validate_attestation_doc(
        &decoded,
        ExpectedAttestation {
            user_data: challenge_digest.as_slice(),
            nonce: report.nonce.as_slice(),
            public_key: report.public_key_uncompressed.as_ref(),
            expected_pcr0: report.expected_pcr0.as_ref(),
            expected_pcr1: report.expected_pcr1.as_ref(),
            expected_pcr2: report.expected_pcr2.as_ref(),
        },
    )?;

    Ok(())
}

pub fn encode_host_proof(
    request: &ProofRequest,
    envelope: &ProofEnvelope,
) -> Result<HostProof, ProtocolError> {
    verify_proof_response(request, envelope)?;
    let public = &request.witness.public_inputs;
    Ok(HostProof {
        tempo_block_number: public.tempo_block_number,
        recent_tempo_block_number: if public.anchor_block_number == public.tempo_block_number {
            0
        } else {
            public.anchor_block_number
        },
        verifier_config: request.verifier_config.to_verifier_config_bytes()?,
        proof: Bytes::from(serde_json::to_vec(envelope)?),
        output: envelope.output.clone(),
    })
}

pub fn encode_native_host_proof(
    config: &NativeVerifierConfig,
    signer: Address,
    public: &PublicInputs,
    signature: &RecoverableSignatureBytes,
    output: BatchOutput,
) -> Result<HostProof, ProtocolError> {
    let digest = native_batch_digest(config, signer, public, &output)?;
    let recovered = recover_address(digest, signature)?;
    if recovered != signer {
        return Err(ProtocolError::SignerMismatch {
            expected: signer,
            actual: recovered,
        });
    }

    Ok(HostProof {
        tempo_block_number: public.tempo_block_number,
        recent_tempo_block_number: if public.anchor_block_number == public.tempo_block_number {
            0
        } else {
            public.anchor_block_number
        },
        verifier_config: config.to_verifier_config_bytes()?,
        proof: Bytes::from((digest, ethereum_signature_bytes(signature)?).abi_encode()),
        output,
    })
}

fn ethereum_signature_bytes(signature: &RecoverableSignatureBytes) -> Result<Bytes, ProtocolError> {
    let bytes = signature.as_bytes();
    if bytes.len() != 65 {
        return Err(ProtocolError::SignatureRecovery(
            CryptoError::InvalidSignatureLength(bytes.len()),
        ));
    }
    let mut ethereum_signature = [0u8; 65];
    ethereum_signature[..64].copy_from_slice(&bytes[..64]);
    ethereum_signature[64] = match bytes[64] {
        0 => 27,
        1 => 28,
        27 | 28 => bytes[64],
        other => return Err(ProtocolError::InvalidNativeRecoveryId(other)),
    };
    Ok(Bytes::copy_from_slice(&ethereum_signature))
}

pub fn parse_b256_hex(input: &str) -> Result<B256, alloy_primitives::hex::FromHexError> {
    input.parse()
}

pub fn parse_nitro_pcr_hex(input: &str) -> Result<NitroPcr, alloy_primitives::hex::FromHexError> {
    input.parse()
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, address};

    use super::*;
    use crate::{crypto::EnclaveSigningKey, types::prove_zone_batch};

    fn request_for(key: &EnclaveSigningKey) -> ProofRequest {
        ProofRequest {
            version: PROTOCOL_VERSION,
            request_id: B256::repeat_byte(0x44),
            nonce: B256::repeat_byte(0x55),
            verifier_config: NitroVerifierConfig {
                version: PROTOCOL_VERSION,
                chain_id: 421_700_001,
                portal_address: address!("0x0000000000000000000000000000000000001000"),
                verifier_version: 1,
                expected_signer: key.address(),
                expected_pcr0: NitroPcr::repeat_byte(0xa0),
                expected_pcr1: NitroPcr::repeat_byte(0xa1),
                expected_pcr2: NitroPcr::repeat_byte(0xa2),
            },
            witness: crate::test_utils::minimal_batch_witness(),
        }
    }

    #[test]
    fn digest_changes_when_required_public_output_changes() {
        let key = EnclaveSigningKey::from_secret_bytes([9u8; 32]).unwrap();
        let request = request_for(&key);
        let mut output = prove_zone_batch(request.witness.clone()).unwrap();
        let base = batch_digest(&request, &output);
        output.block_transition.nextBlockHash = B256::repeat_byte(0xee);
        assert_ne!(base, batch_digest(&request, &output));
    }

    #[test]
    fn native_digest_changes_when_required_public_output_changes() {
        let key = EnclaveSigningKey::from_secret_bytes([9u8; 32]).unwrap();
        let request = request_for(&key);
        let config = native_config();
        let mut output = prove_zone_batch(request.witness.clone()).unwrap();
        let public = &request.witness.public_inputs;
        let base = native_batch_digest(&config, key.address(), public, &output).unwrap();
        output.block_transition.nextBlockHash = B256::repeat_byte(0xee);

        assert_ne!(
            base,
            native_batch_digest(&config, key.address(), public, &output).unwrap()
        );
    }

    #[test]
    fn native_host_proof_encodes_abi_material_for_portal_verifier() {
        let key = EnclaveSigningKey::from_secret_bytes([9u8; 32]).unwrap();
        let request = request_for(&key);
        let public = &request.witness.public_inputs;
        let output = prove_zone_batch(request.witness.clone()).unwrap();
        let config = native_config();
        let digest = native_batch_digest(&config, key.address(), public, &output).unwrap();
        let signature = key.sign_digest(digest);

        let host_proof =
            encode_native_host_proof(&config, key.address(), public, &signature, output.clone())
                .unwrap();

        assert_eq!(
            host_proof.verifier_config,
            config.to_verifier_config_bytes().unwrap()
        );
        assert_eq!(host_proof.tempo_block_number, public.tempo_block_number);
        assert_eq!(host_proof.recent_tempo_block_number, 0);
        assert_eq!(
            host_proof.proof,
            Bytes::from((digest, ethereum_signature_bytes(&signature).unwrap()).abi_encode())
        );
        assert_ne!(
            host_proof.proof,
            Bytes::from(
                (digest, ethereum_signature_bytes(&signature).unwrap()).abi_encode_params()
            )
        );
        assert!(matches!(host_proof.proof.as_ref().get(192), Some(27 | 28)));
        assert_eq!(host_proof.output.digest(), output.digest());
    }

    #[test]
    fn native_host_proof_rejects_output_not_visible_to_portal() {
        let key = EnclaveSigningKey::from_secret_bytes([9u8; 32]).unwrap();
        let request = request_for(&key);
        let public = &request.witness.public_inputs;
        let mut output = prove_zone_batch(request.witness.clone()).unwrap();
        let config = native_config();
        output.last_batch_commitment.withdrawal_batch_index = output
            .last_batch_commitment
            .withdrawal_batch_index
            .checked_add(1)
            .unwrap();

        assert!(matches!(
            native_batch_digest(&config, key.address(), public, &output),
            Err(ProtocolError::NativeOutputMismatch {
                field: "withdrawalBatchIndex"
            })
        ));
    }

    #[test]
    fn rejects_signed_envelope_without_valid_attestation_doc() {
        let key = EnclaveSigningKey::from_secret_bytes([9u8; 32]).unwrap();
        let request = request_for(&key);
        let output = prove_zone_batch(request.witness.clone()).unwrap();
        let digest = batch_digest(&request, &output);
        let envelope = ProofEnvelope {
            version: PROTOCOL_VERSION,
            request_id: request.request_id,
            nonce: request.nonce,
            digest,
            signer: key.address(),
            public_key_uncompressed: key.public_key_uncompressed(),
            signature: key.sign_digest(digest),
            output,
            attestation_doc: Bytes::new(),
        };

        assert!(matches!(
            verify_proof_response(&request, &envelope),
            Err(ProtocolError::Attestation(_))
        ));
    }

    #[test]
    fn rejects_forged_output_even_when_digest_was_signed() {
        let key = EnclaveSigningKey::from_secret_bytes([9u8; 32]).unwrap();
        let request = request_for(&key);
        let output = prove_zone_batch(request.witness.clone()).unwrap();
        let digest = batch_digest(&request, &output);
        let mut envelope = ProofEnvelope {
            version: PROTOCOL_VERSION,
            request_id: request.request_id,
            nonce: request.nonce,
            digest,
            signer: key.address(),
            public_key_uncompressed: key.public_key_uncompressed(),
            signature: key.sign_digest(digest),
            output,
            attestation_doc: Bytes::new(),
        };
        envelope.output.block_transition.nextBlockHash = B256::repeat_byte(0xef);

        assert!(matches!(
            verify_proof_response(&request, &envelope),
            Err(ProtocolError::DigestMismatch)
        ));
    }

    #[test]
    fn invalid_pcr_length_is_rejected_during_config_parse() {
        let key = EnclaveSigningKey::from_secret_bytes([9u8; 32]).unwrap();
        let mut config = serde_json::to_value(request_for(&key).verifier_config).unwrap();
        config["expectedPcr0"] = serde_json::Value::String(format!("0x{}", "a0".repeat(32)));

        assert!(serde_json::from_value::<NitroVerifierConfig>(config).is_err());
    }

    fn native_config() -> NativeVerifierConfig {
        NativeVerifierConfig {
            version: PROTOCOL_VERSION,
            chain_id: 421_700_001,
            portal_address: address!("0x0000000000000000000000000000000000001000"),
            verifier_version: 1,
        }
    }
}
