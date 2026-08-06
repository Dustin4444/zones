use crate::{
    model::constants::NO_WITHDRAWAL_QUEUE_INDEX,
    store::codec::{CodecError, Decoder, Encoder},
};

use super::{
    super::types::{BatchBoundaryValue, BatchMembersValue, BatchValue, FinalizedBatchValue},
    core::{decode_cursor, encode_cursor},
};

pub(super) fn encode_batch(out: &mut Encoder, value: BatchValue) {
    match value {
        BatchValue::Finalized(batch) => {
            out.u8(0x00);
            encode_finalized(out, batch);
        }
        BatchValue::Submitted {
            batch,
            portal,
            logical_queue_index,
            next_processing_ordinal,
            remaining_queue_hash,
        } => {
            out.u8(0x01);
            encode_finalized(out, batch);
            out.address(portal);
            out.u256(logical_queue_index);
            out.u64(next_processing_ordinal);
            out.hash(remaining_queue_hash);
        }
    }
}

pub(super) fn decode_batch(input: &mut Decoder<'_>) -> Result<BatchValue, CodecError> {
    match input.u8("batch phase")? {
        0x00 => Ok(BatchValue::Finalized(decode_finalized(input)?)),
        0x01 => {
            let batch = decode_finalized(input)?;
            if batch.members.member_count == 0 {
                return Err(CodecError::Invalid {
                    field: "submitted batch",
                    reason: "empty batch cannot be submitted",
                });
            }
            let portal = input.address("submitted batch Portal")?;
            let logical_queue_index = input.u256("submitted batch queue index")?;
            if logical_queue_index == NO_WITHDRAWAL_QUEUE_INDEX {
                return Err(CodecError::Invalid {
                    field: "submitted batch queue index",
                    reason: "empty-batch sentinel is not a queue index",
                });
            }
            let next_processing_ordinal = input.u64("submitted batch next ordinal")?;
            if next_processing_ordinal >= batch.members.member_count {
                return Err(CodecError::Invalid {
                    field: "submitted batch next ordinal",
                    reason: "ordinal is outside the member range",
                });
            }
            let remaining_queue_hash = input.hash("submitted batch remaining queue hash")?;
            if remaining_queue_hash.is_zero() {
                return Err(CodecError::Invalid {
                    field: "submitted batch remaining queue hash",
                    reason: "open batch must retain a queue commitment",
                });
            }
            Ok(BatchValue::Submitted {
                batch,
                portal,
                logical_queue_index,
                next_processing_ordinal,
                remaining_queue_hash,
            })
        }
        tag => Err(CodecError::UnknownTag {
            kind: "batch phase",
            tag,
        }),
    }
}

fn encode_finalized(out: &mut Encoder, value: FinalizedBatchValue) {
    let boundary = value.boundary;
    out.hash(boundary.first_zone_parent_hash);
    out.hash(boundary.final_zone_block_hash);
    encode_cursor(out, boundary.first_processed_deposit);
    encode_cursor(out, boundary.final_processed_deposit);
    out.u64(boundary.final_imported_tempo_block_number);
    out.u64(boundary.final_zone_height);

    let members = value.members;
    out.u64(members.first_withdrawal_index);
    out.u64(members.member_count);
    out.hash(members.withdrawal_queue_hash);
}

fn decode_finalized(input: &mut Decoder<'_>) -> Result<FinalizedBatchValue, CodecError> {
    let boundary = BatchBoundaryValue {
        first_zone_parent_hash: input.hash("batch first Zone parent hash")?,
        final_zone_block_hash: input.hash("batch final Zone block hash")?,
        first_processed_deposit: decode_cursor(input, "batch first deposit cursor")?,
        final_processed_deposit: decode_cursor(input, "batch final deposit cursor")?,
        final_imported_tempo_block_number: input.u64("batch final imported Tempo block")?,
        final_zone_height: input.u64("batch final Zone height")?,
    };
    let members = BatchMembersValue {
        first_withdrawal_index: input.u64("batch first withdrawal index")?,
        member_count: input.u64("batch member count")?,
        withdrawal_queue_hash: input.hash("batch withdrawal queue hash")?,
    };
    if members.member_count == 0 && !members.withdrawal_queue_hash.is_zero() {
        return Err(CodecError::Invalid {
            field: "batch withdrawal queue hash",
            reason: "empty batch cannot carry a queue commitment",
        });
    }
    if members.member_count > 0 && members.withdrawal_queue_hash.is_zero() {
        return Err(CodecError::Invalid {
            field: "batch withdrawal queue hash",
            reason: "non-empty batch must carry a queue commitment",
        });
    }
    if members
        .first_withdrawal_index
        .checked_add(members.member_count)
        .is_none()
    {
        return Err(CodecError::Invalid {
            field: "batch withdrawal range",
            reason: "range overflows u64",
        });
    }
    Ok(FinalizedBatchValue { boundary, members })
}
