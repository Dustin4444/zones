use std::str::FromStr;

use alloy_primitives::{Address, B256, Bytes, FixedBytes, keccak256};
use secp256k1::{
    Message, PublicKey, Secp256k1, SecretKey,
    ecdsa::{RecoverableSignature, RecoveryId},
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverableSignatureBytes(pub Bytes);

impl RecoverableSignatureBytes {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_ref()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid secp256k1 private key")]
    InvalidSecretKey,
    #[error("invalid signature length {0}; expected 65 bytes")]
    InvalidSignatureLength(usize),
    #[error("invalid uncompressed secp256k1 public key length {0}; expected 65 bytes")]
    InvalidPublicKeyLength(usize),
    #[error("invalid recovery id {0}")]
    InvalidRecoveryId(u8),
    #[error("secp256k1 error: {0}")]
    Secp256k1(#[from] secp256k1::Error),
}

pub struct EnclaveSigningKey {
    secret_key: SecretKey,
}

impl EnclaveSigningKey {
    pub fn from_secret_bytes(bytes: [u8; 32]) -> Result<Self, CryptoError> {
        let secret_key =
            SecretKey::from_byte_array(&bytes).map_err(|_| CryptoError::InvalidSecretKey)?;
        Ok(Self { secret_key })
    }

    pub fn from_hex(input: &str) -> Result<Self, CryptoError> {
        let bytes = FixedBytes::<32>::from_str(input).map_err(|_| CryptoError::InvalidSecretKey)?;
        let secret_key = SecretKey::from_byte_array(bytes.as_ref())
            .map_err(|_| CryptoError::InvalidSecretKey)?;
        Ok(Self { secret_key })
    }

    pub fn generate() -> Self {
        Self {
            secret_key: SecretKey::new(&mut secp256k1::rand::rngs::OsRng),
        }
    }

    pub fn secret_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(alloy_primitives::hex::encode(
            self.secret_key.secret_bytes(),
        ))
    }

    pub fn public_key_uncompressed(&self) -> Bytes {
        let secp = Secp256k1::new();
        Bytes::copy_from_slice(&self.secret_key.public_key(&secp).serialize_uncompressed())
    }

    pub fn address(&self) -> Address {
        let secp = Secp256k1::new();
        public_key_to_address(&self.secret_key.public_key(&secp))
    }

    pub fn sign_digest(&self, digest: B256) -> RecoverableSignatureBytes {
        let secp = Secp256k1::new();
        let message = Message::from_digest(digest.0);
        let signature = secp.sign_ecdsa_recoverable(&message, &self.secret_key);
        let (recovery_id, compact) = signature.serialize_compact();

        let mut bytes = [0u8; 65];
        bytes[..64].copy_from_slice(&compact);
        bytes[64] =
            u8::try_from(i32::from(recovery_id)).expect("secp256k1 recovery id is always in 0..=3");
        RecoverableSignatureBytes(Bytes::copy_from_slice(&bytes))
    }
}

impl Drop for EnclaveSigningKey {
    fn drop(&mut self) {
        self.secret_key.non_secure_erase();
    }
}

pub fn recover_address(
    digest: B256,
    signature: &RecoverableSignatureBytes,
) -> Result<Address, CryptoError> {
    let bytes = signature.as_bytes();
    if bytes.len() != 65 {
        return Err(CryptoError::InvalidSignatureLength(bytes.len()));
    }

    let recovery_id = match bytes[64] {
        0..=3 => bytes[64],
        27..=30 => bytes[64]
            .checked_sub(27)
            .ok_or(CryptoError::InvalidRecoveryId(bytes[64]))?,
        other => return Err(CryptoError::InvalidRecoveryId(other)),
    };
    let recovery_id = RecoveryId::try_from(i32::from(recovery_id))
        .map_err(|_| CryptoError::InvalidRecoveryId(bytes[64]))?;
    let signature = RecoverableSignature::from_compact(&bytes[..64], recovery_id)?;
    let message = Message::from_digest(digest.0);
    let public_key = Secp256k1::new().recover_ecdsa(&message, &signature)?;
    Ok(public_key_to_address(&public_key))
}

pub fn public_key_to_address(public_key: &PublicKey) -> Address {
    let uncompressed = public_key.serialize_uncompressed();
    let hash = keccak256(&uncompressed[1..]);
    Address::from_slice(&hash[12..])
}

pub fn address_from_uncompressed_public_key(bytes: &[u8]) -> Result<Address, CryptoError> {
    if bytes.len() != 65 {
        return Err(CryptoError::InvalidPublicKeyLength(bytes.len()));
    }
    let public_key = PublicKey::from_slice(bytes)?;
    Ok(public_key_to_address(&public_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_recovers_signer() {
        let key = EnclaveSigningKey::from_secret_bytes([7u8; 32]).unwrap();
        let digest = keccak256(b"zone-prover-test");
        let signature = key.sign_digest(digest);
        let recovered = recover_address(digest, &signature).unwrap();
        assert_eq!(recovered, key.address());
        assert_eq!(
            address_from_uncompressed_public_key(&key.public_key_uncompressed()).unwrap(),
            key.address()
        );
    }
}
