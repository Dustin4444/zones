use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use reth_db_api::table::{Decode, Encode};

use crate::store::{
    codec::{CodecError, Decoder, Encoder},
    schema::ModelKey,
};

use super::{
    super::{
        leaf::StoredProtocolChain,
        types::{ChainLocation, FindingStatus, FindingSummary, LocationKind},
    },
    invalid,
};

pub(super) fn encode_location(out: &mut Encoder, location: ChainLocation) {
    encode_chain(out, location.chain);
    match location.kind {
        LocationKind::Block => out.u8(0x00),
        LocationKind::Transaction(index, hash) => {
            out.u8(0x01);
            out.u64(index);
            out.hash(hash);
        }
        LocationKind::Log {
            transaction_index,
            transaction_hash,
            receipt_log_index,
            block_log_index,
        } => {
            out.u8(0x02);
            out.u64(transaction_index);
            out.hash(transaction_hash);
            out.u64(receipt_log_index);
            out.u64(block_log_index);
        }
    }
}

pub(super) fn decode_location(input: &mut Decoder<'_>) -> Result<ChainLocation, CodecError> {
    let chain = decode_chain(input)?;
    match input.u8("finding location")? {
        0x00 => Ok(ChainLocation::block(chain)),
        0x01 => Ok(ChainLocation::transaction(
            chain,
            input.u64("finding transaction index")?,
            input.hash("finding transaction hash")?,
        )),
        0x02 => Ok(ChainLocation::log(
            chain,
            input.u64("finding transaction index")?,
            input.hash("finding transaction hash")?,
            input.u64("finding receipt log index")?,
            input.u64("finding block log index")?,
        )),
        tag => Err(CodecError::UnknownTag {
            kind: "finding location",
            tag,
        }),
    }
}

pub(super) fn encode_status(out: &mut Encoder, status: FindingStatus) {
    out.u8(match status {
        FindingStatus::Canonical => 0x00,
        FindingStatus::Orphaned => 0x01,
    });
}

pub(super) fn decode_status(input: &mut Decoder<'_>) -> Result<FindingStatus, CodecError> {
    match input.u8("finding status")? {
        0x00 => Ok(FindingStatus::Canonical),
        0x01 => Ok(FindingStatus::Orphaned),
        tag => Err(CodecError::UnknownTag {
            kind: "finding status",
            tag,
        }),
    }
}

pub(super) fn encode_summary(out: &mut Encoder, value: FindingSummary) {
    out.u64(value.length);
    out.hash(value.hash);
}

pub(super) fn decode_summary(input: &mut Decoder<'_>) -> Result<FindingSummary, CodecError> {
    Ok(FindingSummary::new(
        input.u64("finding summary length")?,
        input.hash("finding summary hash")?,
    ))
}

pub(super) fn encode_optional_tip(out: &mut Encoder, tip: Option<BlockNumHash>) {
    match tip {
        None => out.u8(0x00),
        Some(tip) => {
            out.u8(0x01);
            encode_tip(out, tip);
        }
    }
}

pub(super) fn decode_optional_tip(
    input: &mut Decoder<'_>,
) -> Result<Option<BlockNumHash>, CodecError> {
    match input.u8("finding Tempo block presence")? {
        0x00 => Ok(None),
        0x01 => Ok(Some(decode_tip(input)?)),
        tag => Err(CodecError::UnknownTag {
            kind: "finding Tempo block presence",
            tag,
        }),
    }
}

pub(super) fn encode_optional_hash(out: &mut Encoder, hash: Option<B256>) {
    match hash {
        None => out.u8(0x00),
        Some(hash) => {
            out.u8(0x01);
            out.hash(hash);
        }
    }
}

pub(super) fn decode_optional_hash(input: &mut Decoder<'_>) -> Result<Option<B256>, CodecError> {
    match input.u8("finding topic presence")? {
        0x00 => Ok(None),
        0x01 => Ok(Some(input.hash("finding topic")?)),
        tag => Err(CodecError::UnknownTag {
            kind: "finding topic presence",
            tag,
        }),
    }
}

pub(super) fn encode_optional_model_key(out: &mut Encoder, key: Option<ModelKey>) {
    match key {
        None => out.u8(0x00),
        Some(key) => {
            out.u8(0x01);
            let encoded = key.encode();
            out.u8(u8::try_from(encoded.len())
                .expect("release-one model key length must fit one byte"));
            out.raw(&encoded);
        }
    }
}

pub(super) fn decode_optional_model_key(
    input: &mut Decoder<'_>,
) -> Result<Option<ModelKey>, CodecError> {
    match input.u8("finding model-key presence")? {
        0x00 => Ok(None),
        0x01 => {
            let len = usize::from(input.u8("finding model-key length")?);
            let encoded = input.raw("finding model key", len)?;
            ModelKey::decode(encoded)
                .map(Some)
                .map_err(|_| invalid("finding model key", "unknown or non-canonical key"))
        }
        tag => Err(CodecError::UnknownTag {
            kind: "finding model-key presence",
            tag,
        }),
    }
}

pub(super) fn encode_tip(out: &mut Encoder, tip: BlockNumHash) {
    out.u64(tip.number);
    out.hash(tip.hash);
}

pub(super) fn decode_tip(input: &mut Decoder<'_>) -> Result<BlockNumHash, CodecError> {
    Ok(BlockNumHash {
        number: input.u64("finding Tempo block number")?,
        hash: input.hash("finding Tempo block hash")?,
    })
}

fn encode_chain(out: &mut Encoder, chain: StoredProtocolChain) {
    out.u8(chain.wire_tag());
}

fn decode_chain(input: &mut Decoder<'_>) -> Result<StoredProtocolChain, CodecError> {
    let tag = input.u8("finding protocol chain")?;
    StoredProtocolChain::from_wire_tag(tag).ok_or(CodecError::UnknownTag {
        kind: "finding protocol chain",
        tag,
    })
}
