use std::{fs, io, path::PathBuf, sync::Arc};

use alloy_primitives::{B256, Bytes};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use zeroize::Zeroizing;

use crate::{
    attestation::{
        AttestationProvider, AttestationRequest, FileAttestationProvider, NoAttestationProvider,
        NsmAttestationProvider, VerifiedAttestationDoc, verify_attestation_doc,
    },
    crypto::EnclaveSigningKey,
    protocol::{
        NITRO_SHA384_PCR_BYTES, NitroPcr, PROTOCOL_VERSION, ProofEnvelope, ProofRequest,
        RegistrationReport, batch_digest, registration_challenge_digest_fields,
        registration_digest_fields,
    },
    transport::{
        DEFAULT_MAX_REQUEST_BYTES, bind_tcp, bind_vsock, read_json_frame, write_json_frame,
    },
    types::prove_zone_batch,
};

pub enum SigningKeySource {
    Ephemeral,
    Hex(Zeroizing<String>),
    File(PathBuf),
}

#[derive(Debug)]
pub enum AttestationProviderConfig {
    Nsm,
    File(PathBuf),
    None,
}

pub struct SignerConfig {
    pub key_source: SigningKeySource,
    pub attestation_provider: AttestationProviderConfig,
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("crypto error: {0}")]
    Crypto(#[from] crate::crypto::CryptoError),
    #[error("prover error: {0}")]
    Prover(#[from] crate::types::ProverError),
    #[error("attestation error: {0}")]
    Attestation(#[from] crate::attestation::AttestationError),
    #[error("unexpected request type")]
    UnexpectedRequestType,
}

pub struct ProverServer {
    signing_key: EnclaveSigningKey,
    attestation_provider: Arc<dyn AttestationProvider>,
}

impl ProverServer {
    pub fn from_config(config: SignerConfig) -> Result<Self, ServerError> {
        let signing_key = load_signing_key(config.key_source)?;
        let attestation_provider = attestation_provider(config.attestation_provider);
        Ok(Self {
            signing_key,
            attestation_provider,
        })
    }

    pub fn signer(&self) -> alloy_primitives::Address {
        self.signing_key.address()
    }

    pub fn handle(&self, request: EnclaveRequest) -> Result<EnclaveResponse, ServerError> {
        match request {
            EnclaveRequest::Prove(request) => self
                .prove(*request)
                .map(|proof| EnclaveResponse::Proof(Box::new(proof))),
            EnclaveRequest::Register {
                version,
                request_id,
                nonce,
            } => self
                .registration_report(version, request_id, nonce)
                .map(|report| EnclaveResponse::Registration(Box::new(report))),
        }
    }

    pub fn registration_report(
        &self,
        version: u16,
        request_id: B256,
        nonce: B256,
    ) -> Result<RegistrationReport, ServerError> {
        let public_key_uncompressed = self.signing_key.public_key_uncompressed();
        let attested_pcrs = self.attested_pcrs(nonce, public_key_uncompressed.as_ref())?;
        let challenge_digest = registration_challenge_digest_fields(
            version,
            request_id,
            nonce,
            self.signer(),
            public_key_uncompressed.as_ref(),
            &attested_pcrs.pcr0,
            &attested_pcrs.pcr1,
            &attested_pcrs.pcr2,
        );
        let attestation_doc =
            self.attest(challenge_digest, nonce, public_key_uncompressed.as_ref())?;
        let digest = registration_digest_fields(
            version,
            request_id,
            nonce,
            self.signer(),
            public_key_uncompressed.as_ref(),
            &attested_pcrs.pcr0,
            &attested_pcrs.pcr1,
            &attested_pcrs.pcr2,
            alloy_primitives::keccak256(attestation_doc.as_ref()),
        );
        Ok(RegistrationReport {
            version,
            request_id,
            nonce,
            signer: self.signer(),
            public_key_uncompressed,
            expected_pcr0: attested_pcrs.pcr0,
            expected_pcr1: attested_pcrs.pcr1,
            expected_pcr2: attested_pcrs.pcr2,
            attestation_doc,
            digest,
            signature: self.signing_key.sign_digest(digest),
        })
    }

    pub fn prove(&self, request: ProofRequest) -> Result<ProofEnvelope, ServerError> {
        let output = prove_zone_batch(request.witness.clone())?;
        let digest = batch_digest(&request, &output);
        let public_key_uncompressed = self.signing_key.public_key_uncompressed();
        Ok(ProofEnvelope {
            version: PROTOCOL_VERSION,
            request_id: request.request_id,
            nonce: request.nonce,
            digest,
            signer: self.signer(),
            public_key_uncompressed: public_key_uncompressed.clone(),
            signature: self.signing_key.sign_digest(digest),
            output,
            attestation_doc: self.attest(
                digest,
                request.nonce,
                public_key_uncompressed.as_ref(),
            )?,
        })
    }

    pub async fn serve_connection<S>(&self, mut stream: S) -> Result<(), ServerError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let request: EnclaveRequest =
            read_json_frame(&mut stream, DEFAULT_MAX_REQUEST_BYTES).await?;
        let response = match self.handle(request) {
            Ok(response) => response,
            Err(err) => EnclaveResponse::Error {
                message: err.to_string(),
            },
        };
        write_json_frame(&mut stream, &response).await?;
        Ok(())
    }

    pub async fn serve_tcp(&self, addr: &str) -> Result<(), ServerError> {
        let listener = bind_tcp(addr).await?;
        loop {
            let (stream, _) = listener.accept().await?;
            self.serve_connection(stream).await?;
        }
    }

    pub async fn serve_vsock(&self, port: u32) -> Result<(), ServerError> {
        let listener = bind_vsock(port)?;
        loop {
            let (stream, _) = listener.accept().await?;
            self.serve_connection(stream).await?;
        }
    }

    fn attest(
        &self,
        user_data: B256,
        nonce: B256,
        public_key: &[u8],
    ) -> Result<Bytes, ServerError> {
        Ok(self.attestation_provider.attest(AttestationRequest {
            user_data: user_data.as_slice(),
            nonce: nonce.as_slice(),
            public_key,
        })?)
    }

    fn attested_pcrs(&self, nonce: B256, public_key: &[u8]) -> Result<AttestedPcrs, ServerError> {
        let document = self.attest(B256::ZERO, nonce, public_key)?;
        let decoded = verify_attestation_doc(document.as_ref())?;
        Ok(AttestedPcrs {
            pcr0: attested_pcr(&decoded, 0)?,
            pcr1: attested_pcr(&decoded, 1)?,
            pcr2: attested_pcr(&decoded, 2)?,
        })
    }
}

struct AttestedPcrs {
    pcr0: NitroPcr,
    pcr1: NitroPcr,
    pcr2: NitroPcr,
}

fn attested_pcr(doc: &VerifiedAttestationDoc, index: usize) -> Result<NitroPcr, ServerError> {
    let pcr = doc
        .pcrs
        .get(&index)
        .ok_or(crate::attestation::AttestationError::MissingPcr(index))?;
    if pcr.len() != NITRO_SHA384_PCR_BYTES {
        return Err(crate::attestation::AttestationError::InvalidPcrLength {
            index,
            actual: pcr.len(),
            expected: NITRO_SHA384_PCR_BYTES,
        }
        .into());
    }
    Ok(NitroPcr::from_slice(pcr.as_ref()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EnclaveRequest {
    Prove(Box<ProofRequest>),
    Register {
        version: u16,
        request_id: B256,
        nonce: B256,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EnclaveResponse {
    Proof(Box<ProofEnvelope>),
    Registration(Box<RegistrationReport>),
    Error { message: String },
}

fn attestation_provider(config: AttestationProviderConfig) -> Arc<dyn AttestationProvider> {
    match config {
        AttestationProviderConfig::Nsm => Arc::new(NsmAttestationProvider),
        AttestationProviderConfig::File(path) => Arc::new(FileAttestationProvider::new(path)),
        AttestationProviderConfig::None => Arc::new(NoAttestationProvider),
    }
}

fn load_signing_key(source: SigningKeySource) -> Result<EnclaveSigningKey, ServerError> {
    match source {
        SigningKeySource::Ephemeral => Ok(EnclaveSigningKey::generate()),
        SigningKeySource::Hex(hex) => Ok(EnclaveSigningKey::from_hex(hex.as_ref())?),
        SigningKeySource::File(path) => {
            if path.exists() {
                let contents = Zeroizing::new(fs::read_to_string(&path)?);
                return Ok(EnclaveSigningKey::from_hex(contents.trim())?);
            }

            let key = EnclaveSigningKey::generate();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let key_hex = key.secret_hex();
            write_key_file(&path, key_hex.as_ref())?;
            Ok(key)
        }
    }
}

fn write_key_file(path: &PathBuf, key_hex: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::{fs::OpenOptions, io::Write as _, os::unix::fs::OpenOptionsExt};

        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(key_hex.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        fs::write(path, format!("{key_hex}\n"))
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, address};

    use super::*;
    use crate::protocol::{NitroVerifierConfig, PROTOCOL_VERSION};

    fn proof_request(signer: alloy_primitives::Address) -> ProofRequest {
        ProofRequest {
            version: PROTOCOL_VERSION,
            request_id: B256::repeat_byte(0x88),
            nonce: B256::repeat_byte(0x77),
            verifier_config: NitroVerifierConfig {
                version: PROTOCOL_VERSION,
                chain_id: 421_700_001,
                portal_address: address!("0x0000000000000000000000000000000000001000"),
                verifier_version: 1,
                expected_signer: signer,
                expected_pcr0: NitroPcr::repeat_byte(0xa0),
                expected_pcr1: NitroPcr::repeat_byte(0xa1),
                expected_pcr2: NitroPcr::repeat_byte(0xa2),
            },
            witness: crate::test_utils::minimal_batch_witness(),
        }
    }

    fn test_server() -> ProverServer {
        ProverServer::from_config(SignerConfig {
            key_source: SigningKeySource::Hex(Zeroizing::new(
                "0909090909090909090909090909090909090909090909090909090909090909".to_string(),
            )),
            attestation_provider: AttestationProviderConfig::None,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn server_connection_runs_prover_then_fails_when_attestation_disabled() {
        let server = test_server();
        let request = proof_request(server.signer());
        let wire_request = EnclaveRequest::Prove(Box::new(request.clone()));
        let (mut client_stream, server_stream) = tokio::io::duplex(64 * 1024);

        let server_task = async {
            server.serve_connection(server_stream).await.unwrap();
        };
        let client_task = async {
            crate::transport::write_json_frame(&mut client_stream, &wire_request)
                .await
                .unwrap();
            let response: EnclaveResponse =
                crate::transport::read_json_frame(&mut client_stream, 64 * 1024)
                    .await
                    .unwrap();
            let EnclaveResponse::Error { message } = response else {
                panic!("expected error response");
            };
            assert!(message.contains("attestation provider is disabled"));
        };

        tokio::join!(server_task, client_task);
    }

    #[test]
    fn registration_report_fails_when_attestation_disabled() {
        let server = test_server();
        let err = server
            .registration_report(
                PROTOCOL_VERSION,
                B256::repeat_byte(0x88),
                B256::repeat_byte(0x77),
            )
            .unwrap_err();
        assert!(err.to_string().contains("attestation provider is disabled"));
    }
}
