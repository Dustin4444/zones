//! Checked, versioned codecs for checker table values.

use std::{fmt::Debug, num::TryFromIntError};

use alloy_primitives::{Address, B256, U256};

use super::SCHEMA_VERSION;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum CodecError {
    #[error("checker value is truncated while reading {field}")]
    Truncated { field: &'static str },
    #[error("checker value has {remaining} trailing bytes")]
    TrailingBytes { remaining: usize },
    #[error("unknown checker value version {actual}, expected {expected}")]
    UnknownVersion { actual: u8, expected: u8 },
    #[error("unknown {kind} tag {tag:#04x}")]
    UnknownTag { kind: &'static str, tag: u8 },
    #[error("invalid {field}: {reason}")]
    Invalid {
        field: &'static str,
        reason: &'static str,
    },
}

impl From<TryFromIntError> for CodecError {
    fn from(_: TryFromIntError) -> Self {
        Self::Invalid {
            field: "integer",
            reason: "value does not fit its encoded width",
        }
    }
}

pub(super) trait CheckedCompact: Sized + Debug + Send + Sync {
    fn encode_checked(&self, out: &mut Encoder);

    fn decode_checked(input: &mut Decoder<'_>) -> Result<Self, CodecError>;
}

pub(super) fn encoded<T: CheckedCompact>(value: &T) -> Vec<u8> {
    let mut encoder = Encoder::default();
    value.encode_checked(&mut encoder);
    encoder.finish()
}

pub(super) fn decode_exact<T: CheckedCompact>(bytes: &[u8]) -> Result<T, CodecError> {
    let mut decoder = Decoder::new(bytes);
    let value = T::decode_checked(&mut decoder)?;
    decoder.finish()?;
    Ok(value)
}

/// Prove that an in-memory value is admitted by the strict persisted decoder
/// and has one canonical byte representation before it enters a write
/// transaction.
pub(super) fn validate_canonical<T>(value: &T) -> Result<(), CodecError>
where
    T: CheckedCompact + PartialEq,
{
    let bytes = encoded(value);
    let decoded = decode_exact::<T>(&bytes)?;
    if &decoded == value {
        Ok(())
    } else {
        Err(CodecError::Invalid {
            field: "checker value",
            reason: "encoding is not canonical",
        })
    }
}

#[derive(Debug, Default)]
pub(super) struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    pub(super) fn version(&mut self) {
        self.u8(SCHEMA_VERSION);
    }

    pub(super) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(super) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(super) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(super) fn u128(&mut self, value: u128) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(super) fn address(&mut self, value: Address) {
        self.bytes.extend_from_slice(value.as_slice());
    }

    pub(super) fn hash(&mut self, value: B256) {
        self.bytes.extend_from_slice(value.as_slice());
    }

    pub(super) fn u256(&mut self, value: U256) {
        self.bytes.extend_from_slice(&value.to_be_bytes::<32>());
    }

    pub(super) fn bytes(&mut self, field: &'static str, value: &[u8]) {
        let len = u32::try_from(value.len()).unwrap_or_else(|_| {
            panic!(
                "{field} length {} exceeds release-one u32 bound",
                value.len()
            )
        });
        self.u32(len);
        self.bytes.extend_from_slice(value);
    }

    pub(super) fn raw(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    pub(super) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Decoder<'a> {
    remaining: &'a [u8],
}

impl<'a> Decoder<'a> {
    pub(super) const fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    pub(super) fn version(&mut self) -> Result<(), CodecError> {
        let actual = self.u8("format version")?;
        if actual != SCHEMA_VERSION {
            return Err(CodecError::UnknownVersion {
                actual,
                expected: SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    pub(super) fn u8(&mut self, field: &'static str) -> Result<u8, CodecError> {
        Ok(self.take::<1>(field)?[0])
    }

    pub(super) fn u32(&mut self, field: &'static str) -> Result<u32, CodecError> {
        Ok(u32::from_be_bytes(self.take(field)?))
    }

    pub(super) fn u64(&mut self, field: &'static str) -> Result<u64, CodecError> {
        Ok(u64::from_be_bytes(self.take(field)?))
    }

    pub(super) fn u128(&mut self, field: &'static str) -> Result<u128, CodecError> {
        Ok(u128::from_be_bytes(self.take(field)?))
    }

    pub(super) fn address(&mut self, field: &'static str) -> Result<Address, CodecError> {
        Ok(Address::from(self.take::<20>(field)?))
    }

    pub(super) fn hash(&mut self, field: &'static str) -> Result<B256, CodecError> {
        Ok(B256::from(self.take::<32>(field)?))
    }

    pub(super) fn u256(&mut self, field: &'static str) -> Result<U256, CodecError> {
        Ok(U256::from_be_bytes(self.take::<32>(field)?))
    }

    pub(super) fn bounded_bytes(
        &mut self,
        field: &'static str,
        maximum: usize,
    ) -> Result<Vec<u8>, CodecError> {
        let len = usize::try_from(self.u32(field)?)?;
        if len > maximum {
            return Err(CodecError::Invalid {
                field,
                reason: "encoded length exceeds the protocol bound",
            });
        }
        if self.remaining.len() < len {
            return Err(CodecError::Truncated { field });
        }
        let (value, rest) = self.remaining.split_at(len);
        self.remaining = rest;
        Ok(value.to_vec())
    }

    pub(super) fn raw(&mut self, field: &'static str, len: usize) -> Result<&'a [u8], CodecError> {
        if self.remaining.len() < len {
            return Err(CodecError::Truncated { field });
        }
        let (value, rest) = self.remaining.split_at(len);
        self.remaining = rest;
        Ok(value)
    }

    pub(super) const fn remaining_len(&self) -> usize {
        self.remaining.len()
    }

    pub(super) fn finish(self) -> Result<(), CodecError> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(CodecError::TrailingBytes {
                remaining: self.remaining.len(),
            })
        }
    }

    fn take<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], CodecError> {
        let bytes = self.raw(field, N)?;
        bytes
            .try_into()
            .map_err(|_| CodecError::Truncated { field })
    }
}

macro_rules! impl_value_codec {
    ($ty:ty) => {
        impl reth_codecs::Compress for $ty {
            type Compressed = Vec<u8>;

            fn compress_to_buf<B: bytes::BufMut + AsMut<[u8]>>(&self, buf: &mut B) {
                bytes::BufMut::put_slice(buf, &$crate::store::codec::encoded(self));
            }
        }

        impl reth_codecs::Decompress for $ty {
            fn decompress(value: &[u8]) -> Result<Self, reth_codecs::DecompressError> {
                $crate::store::codec::decode_exact(value).map_err(reth_codecs::DecompressError::new)
            }
        }

        impl serde::Serialize for $ty {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_bytes(&$crate::store::codec::encoded(self))
            }
        }
    };
}

pub(super) use impl_value_codec;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct Example(u64);

    impl CheckedCompact for Example {
        fn encode_checked(&self, out: &mut Encoder) {
            out.version();
            out.u64(self.0);
        }

        fn decode_checked(input: &mut Decoder<'_>) -> Result<Self, CodecError> {
            input.version()?;
            Ok(Self(input.u64("example")?))
        }
    }

    impl_value_codec!(Example);

    #[test]
    fn exact_decode_rejects_version_truncation_and_trailing_bytes() {
        assert_eq!(
            decode_exact::<Example>(&[SCHEMA_VERSION, 0, 0, 0, 0, 0, 0, 0, 7]),
            Ok(Example(7))
        );
        assert!(matches!(
            decode_exact::<Example>(&[SCHEMA_VERSION + 1, 0, 0, 0, 0, 0, 0, 0, 7]),
            Err(CodecError::UnknownVersion { .. })
        ));
        assert!(matches!(
            decode_exact::<Example>(&[SCHEMA_VERSION]),
            Err(CodecError::Truncated { .. })
        ));
        assert!(matches!(
            decode_exact::<Example>(&[SCHEMA_VERSION, 0, 0, 0, 0, 0, 0, 0, 7, 0]),
            Err(CodecError::TrailingBytes { remaining: 1 })
        ));
    }

    #[test]
    fn bounded_bytes_rejects_declared_length_before_copying() {
        let bytes = u32::MAX.to_be_bytes();
        let mut decoder = Decoder::new(&bytes);
        assert!(matches!(
            decoder.bounded_bytes("bounded", 1_024),
            Err(CodecError::Invalid {
                field: "bounded",
                ..
            })
        ));
    }
}
