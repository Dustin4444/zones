use std::{collections::BTreeMap, fs, path::PathBuf};

use alloy_primitives::Bytes;
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct AttestationRequest<'a> {
    pub user_data: &'a [u8],
    pub nonce: &'a [u8],
    pub public_key: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAttestationDoc {
    pub user_data: Option<Bytes>,
    pub nonce: Option<Bytes>,
    pub public_key: Option<Bytes>,
    pub pcrs: BTreeMap<usize, Bytes>,
}

pub trait AttestationProvider: Send + Sync {
    fn attest(&self, request: AttestationRequest<'_>) -> Result<Bytes, AttestationError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoAttestationProvider;

impl AttestationProvider for NoAttestationProvider {
    fn attest(&self, _request: AttestationRequest<'_>) -> Result<Bytes, AttestationError> {
        Err(AttestationError::NsmUnavailable(
            "attestation provider is disabled".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct FileAttestationProvider {
    path: PathBuf,
}

impl FileAttestationProvider {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl AttestationProvider for FileAttestationProvider {
    fn attest(&self, _request: AttestationRequest<'_>) -> Result<Bytes, AttestationError> {
        Ok(Bytes::from(fs::read(&self.path)?))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NsmAttestationProvider;

impl AttestationProvider for NsmAttestationProvider {
    fn attest(&self, request: AttestationRequest<'_>) -> Result<Bytes, AttestationError> {
        attest_with_nsm(request)
    }
}

#[cfg(all(feature = "nsm-driver", target_os = "linux"))]
fn attest_with_nsm(request: AttestationRequest<'_>) -> Result<Bytes, AttestationError> {
    use aws_nitro_enclaves_nsm_api::{
        api::{Request, Response},
        driver::{nsm_exit, nsm_init, nsm_process_request},
    };

    let fd = nsm_init();
    if fd < 0 {
        return Err(AttestationError::NsmUnavailable(
            "failed to open /dev/nsm".to_string(),
        ));
    }

    let response = nsm_process_request(
        fd,
        Request::Attestation {
            user_data: Some(request.user_data.to_vec().into()),
            nonce: Some(request.nonce.to_vec().into()),
            public_key: Some(request.public_key.to_vec().into()),
        },
    );
    nsm_exit(fd);

    match response {
        Response::Attestation { document } => Ok(Bytes::from(document)),
        Response::Error(code) => Err(AttestationError::NsmRejected(format!("{code:?}"))),
        other => Err(AttestationError::NsmRejected(format!(
            "unexpected NSM response {other:?}"
        ))),
    }
}

#[cfg(not(all(feature = "nsm-driver", target_os = "linux")))]
fn attest_with_nsm(_request: AttestationRequest<'_>) -> Result<Bytes, AttestationError> {
    Err(AttestationError::NsmUnavailable(
        "built without the nsm-driver feature on Linux".to_string(),
    ))
}

#[derive(Debug, Clone)]
pub struct ExpectedAttestation<'a> {
    pub user_data: &'a [u8],
    pub nonce: &'a [u8],
    pub public_key: &'a [u8],
    pub expected_pcr0: &'a [u8],
    pub expected_pcr1: &'a [u8],
    pub expected_pcr2: &'a [u8],
}

#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("NSM attestation is unavailable: {0}")]
    NsmUnavailable(String),
    #[error("NSM attestation request failed: {0}")]
    NsmRejected(String),
    #[error("Nitro attestation document verification failed: {0}")]
    Verification(String),
    #[error("attestation document missing {0}")]
    MissingField(&'static str),
    #[error("attestation {0} mismatch")]
    FieldMismatch(&'static str),
    #[error("attestation document missing PCR{0}")]
    MissingPcr(usize),
    #[error("attestation PCR{index} has invalid length {actual}; expected {expected} bytes")]
    InvalidPcrLength {
        index: usize,
        actual: usize,
        expected: usize,
    },
    #[error("attestation PCR{index} mismatch")]
    PcrMismatch { index: usize },
}

pub fn verify_attestation_doc(document: &[u8]) -> Result<VerifiedAttestationDoc, AttestationError> {
    verify_attestation_doc_at(document, OffsetDateTime::now_utc())
}

pub fn verify_attestation_doc_at(
    document: &[u8],
    verification_time: OffsetDateTime,
) -> Result<VerifiedAttestationDoc, AttestationError> {
    let parsed = nitro_attest::UnparsedAttestationDoc::from(document)
        .parse_and_verify(verification_time)
        .map_err(|err| AttestationError::Verification(err.to_string()))?;

    Ok(VerifiedAttestationDoc {
        user_data: parsed.user_data.map(|value| Bytes::from(value.into_vec())),
        nonce: parsed.nonce.map(|value| Bytes::from(value.into_vec())),
        public_key: parsed.public_key.map(|value| Bytes::from(value.into_vec())),
        pcrs: parsed
            .pcrs
            .into_iter()
            .map(|(index, digest)| (usize::from(index), Bytes::from(digest.value)))
            .collect(),
    })
}

pub fn validate_attestation_doc(
    doc: &VerifiedAttestationDoc,
    expected: ExpectedAttestation<'_>,
) -> Result<(), AttestationError> {
    assert_attestation_field(
        "user_data",
        doc.user_data.as_ref().map(|value| value.as_ref()),
        expected.user_data,
    )?;
    assert_attestation_field(
        "nonce",
        doc.nonce.as_ref().map(|value| value.as_ref()),
        expected.nonce,
    )?;
    assert_attestation_field(
        "public_key",
        doc.public_key.as_ref().map(|value| value.as_ref()),
        expected.public_key,
    )?;
    assert_pcr(doc, 0, expected.expected_pcr0)?;
    assert_pcr(doc, 1, expected.expected_pcr1)?;
    assert_pcr(doc, 2, expected.expected_pcr2)?;
    Ok(())
}

fn assert_attestation_field(
    name: &'static str,
    actual: Option<&[u8]>,
    expected: &[u8],
) -> Result<(), AttestationError> {
    let actual = actual.ok_or(AttestationError::MissingField(name))?;
    if actual != expected {
        return Err(AttestationError::FieldMismatch(name));
    }
    Ok(())
}

fn assert_pcr(
    doc: &VerifiedAttestationDoc,
    index: usize,
    expected: &[u8],
) -> Result<(), AttestationError> {
    let actual = doc
        .pcrs
        .get(&index)
        .ok_or(AttestationError::MissingPcr(index))?;
    if actual.as_ref() != expected {
        return Err(AttestationError::PcrMismatch { index });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_expected_attestation_fields_and_pcrs() {
        let mut pcrs = BTreeMap::new();
        pcrs.insert(0, Bytes::from(vec![0xa0; 48]));
        pcrs.insert(1, Bytes::from(vec![0xa1; 48]));
        pcrs.insert(2, Bytes::from(vec![0xa2; 48]));
        let doc = VerifiedAttestationDoc {
            user_data: Some(Bytes::from(vec![4, 5, 6])),
            nonce: Some(Bytes::from(vec![7, 8, 9])),
            public_key: Some(Bytes::from(vec![10, 11, 12])),
            pcrs,
        };

        validate_attestation_doc(
            &doc,
            ExpectedAttestation {
                user_data: &[4, 5, 6],
                nonce: &[7, 8, 9],
                public_key: &[10, 11, 12],
                expected_pcr0: &[0xa0; 48],
                expected_pcr1: &[0xa1; 48],
                expected_pcr2: &[0xa2; 48],
            },
        )
        .unwrap();
    }
}
