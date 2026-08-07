use alloy_eips::BlockNumHash;
use reth_db_api::table::{Decode, Encode};

use crate::store::{
    codec::{
        CheckedCompact, CodecError, Decoder, Encoder, decode_exact, encoded, impl_value_codec,
    },
    schema::ModelKey,
};

use super::ModelValue;

const MAX_MODEL_VALUE_SIZE: usize = 4 * 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockBeforeImage {
    pub(crate) prior_verified_zone_tip: BlockNumHash,
    pub(crate) prior_imported_tempo_tip: BlockNumHash,
    pub(crate) mutation_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeforeImage {
    Block(BlockBeforeImage),
    Model {
        key: ModelKey,
        value: Option<Box<ModelValue>>,
    },
}

impl CheckedCompact for BeforeImage {
    fn encode_checked(&self, out: &mut Encoder) {
        out.version();
        match self {
            Self::Block(block) => {
                out.u8(0x00);
                encode_tip(out, block.prior_verified_zone_tip);
                encode_tip(out, block.prior_imported_tempo_tip);
                out.u32(block.mutation_count);
            }
            Self::Model { key, value } => {
                out.u8(0x01);
                let key_bytes = key.encode();
                out.u8(u8::try_from(key_bytes.len())
                    .expect("release-one model key length must fit one byte"));
                out.raw(&key_bytes);
                match value {
                    None => out.u8(0x00),
                    Some(value) => {
                        out.u8(0x01);
                        out.bytes("before-image model value", &encoded(value.as_ref()));
                    }
                }
            }
        }
    }

    fn decode_checked(input: &mut Decoder<'_>) -> Result<Self, CodecError> {
        input.version()?;
        match input.u8("before-image tag")? {
            0x00 => Ok(Self::Block(BlockBeforeImage {
                prior_verified_zone_tip: decode_tip(input, "prior verified Zone tip")?,
                prior_imported_tempo_tip: decode_tip(input, "prior imported Tempo tip")?,
                mutation_count: input.u32("changeset mutation count")?,
            })),
            0x01 => {
                let key_len = usize::from(input.u8("before-image model key length")?);
                let key_bytes = input.raw("before-image model key", key_len)?;
                let key = ModelKey::decode(key_bytes).map_err(|_| CodecError::Invalid {
                    field: "before-image model key",
                    reason: "key does not use a known exact encoding",
                })?;
                let value = match input.u8("before-image value presence")? {
                    0x00 => None,
                    0x01 => {
                        let bytes = input
                            .bounded_bytes("before-image model value", MAX_MODEL_VALUE_SIZE)?;
                        Some(Box::new(decode_exact::<ModelValue>(&bytes)?))
                    }
                    tag => {
                        return Err(CodecError::UnknownTag {
                            kind: "before-image value presence",
                            tag,
                        });
                    }
                };
                if value.as_ref().is_some_and(|value| !value.matches_key(key)) {
                    return Err(CodecError::Invalid {
                        field: "before-image model value",
                        reason: "value family does not match embedded key",
                    });
                }
                Ok(Self::Model { key, value })
            }
            tag => Err(CodecError::UnknownTag {
                kind: "before-image",
                tag,
            }),
        }
    }
}

impl_value_codec!(BeforeImage);

fn encode_tip(out: &mut Encoder, tip: BlockNumHash) {
    out.u64(tip.number);
    out.hash(tip.hash);
}

fn decode_tip(input: &mut Decoder<'_>, field: &'static str) -> Result<BlockNumHash, CodecError> {
    Ok(BlockNumHash {
        number: input.u64(field)?,
        hash: input.hash(field)?,
    })
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use reth_codecs::{Compress, Decompress};

    use super::*;
    use crate::store::{SCHEMA_VERSION, value::ModelValue};

    #[test]
    fn block_metadata_has_golden_bytes_and_round_trips() {
        let value = BeforeImage::Block(BlockBeforeImage {
            prior_verified_zone_tip: BlockNumHash::new(1, B256::repeat_byte(0x11)),
            prior_imported_tempo_tip: BlockNumHash::new(2, B256::repeat_byte(0x22)),
            mutation_count: 3,
        });
        let bytes = value.clone().compress();
        let mut golden = vec![SCHEMA_VERSION, 0];
        golden.extend_from_slice(&1_u64.to_be_bytes());
        golden.extend_from_slice(&[0x11; 32]);
        golden.extend_from_slice(&2_u64.to_be_bytes());
        golden.extend_from_slice(&[0x22; 32]);
        golden.extend_from_slice(&3_u32.to_be_bytes());

        assert_eq!(bytes, golden);
        assert_eq!(BeforeImage::decompress(&bytes).unwrap(), value);
    }

    #[test]
    fn absent_and_present_model_images_round_trip() {
        let absent = BeforeImage::Model {
            key: ModelKey::Token(alloy_primitives::Address::repeat_byte(0x33)),
            value: None,
        };
        let mut absent_golden = vec![SCHEMA_VERSION, 1, 21, 0x20];
        absent_golden.extend_from_slice(&[0x33; 20]);
        absent_golden.push(0);

        let present = BeforeImage::Model {
            key: ModelKey::ZoneNextWithdrawalIndex,
            value: Some(Box::new(ModelValue::ZoneNextWithdrawalIndex(7))),
        };
        let present_golden = vec![
            SCHEMA_VERSION,
            1,
            1,
            0x06,
            1,
            0,
            0,
            0,
            10,
            SCHEMA_VERSION,
            0x06,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            7,
        ];

        for (value, golden) in [(absent, absent_golden), (present, present_golden)] {
            let bytes = value.clone().compress();
            assert_eq!(bytes, golden);
            assert_eq!(BeforeImage::decompress(&bytes).unwrap(), value);
            for cut in 0..bytes.len() {
                assert!(BeforeImage::decompress(&bytes[..cut]).is_err());
            }
        }
    }

    #[test]
    fn before_image_rejects_unknown_tags_and_oversized_nested_values() {
        assert!(BeforeImage::decompress(&[SCHEMA_VERSION, 0xff]).is_err());
        assert!(BeforeImage::decompress(&[SCHEMA_VERSION, 1, 1, 0x06, 0xff]).is_err());

        let mut oversized = vec![SCHEMA_VERSION, 1, 1, 0x06, 1];
        oversized.extend_from_slice(&(MAX_MODEL_VALUE_SIZE as u32 + 1).to_be_bytes());
        assert!(BeforeImage::decompress(&oversized).is_err());

        let mismatched = BeforeImage::Model {
            key: ModelKey::ZoneNextWithdrawalIndex,
            value: Some(Box::new(ModelValue::ZoneNextWithdrawalIndex(7))),
        }
        .compress();
        let mut mismatched = mismatched;
        mismatched[3] = crate::store::schema::model_tag::ZONE_LAST_FALLBACK_NONCE;
        assert!(BeforeImage::decompress(&mismatched).is_err());
    }
}
