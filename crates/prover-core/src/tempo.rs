use alloy_primitives::{B256, Bytes, keccak256};
use alloy_rlp::Decodable;
use tempo_primitives::TempoHeader;

use crate::{ProverError, PublicInputs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TempoBinding {
    pub block_number: u64,
    pub block_hash: B256,
    pub state_root: B256,
}

pub(crate) fn verify_tempo_ancestry(
    public: &PublicInputs,
    binding: TempoBinding,
    ancestry_headers: &[Bytes],
) -> Result<(), ProverError> {
    if binding.block_number != public.tempo_block_number {
        return Err(ProverError::TempoBlockNumberMismatch {
            expected: public.tempo_block_number,
            actual: binding.block_number,
        });
    }
    let expected_len = public
        .anchor_block_number
        .checked_sub(binding.block_number)
        .ok_or(ProverError::AnchorBeforeTempo)?;
    let actual_len =
        u64::try_from(ancestry_headers.len()).map_err(|_| ProverError::TempoAncestryTooLong)?;
    if actual_len != expected_len {
        return Err(ProverError::TempoAncestryLengthMismatch {
            expected: expected_len,
            actual: actual_len,
        });
    }

    let mut previous_hash = binding.block_hash;
    for (index, encoded) in ancestry_headers.iter().enumerate() {
        let header = decode_tempo_header(index, encoded)?;
        let offset = u64::try_from(index).map_err(|_| ProverError::TempoAncestryTooLong)?;
        let expected_number = binding
            .block_number
            .checked_add(offset)
            .and_then(|number| number.checked_add(1))
            .ok_or(ProverError::TempoAncestryBlockNumberOverflow { index })?;
        if header.inner.number != expected_number {
            return Err(ProverError::TempoAncestryBlockNumberMismatch {
                index,
                expected: expected_number,
                actual: header.inner.number,
            });
        }
        if header.inner.parent_hash != previous_hash {
            return Err(ProverError::TempoAncestryParentHashMismatch {
                index,
                expected: previous_hash,
                actual: header.inner.parent_hash,
            });
        }
        previous_hash = keccak256(encoded.as_ref());
    }

    if previous_hash != public.anchor_block_hash {
        return Err(ProverError::TempoAnchorHashMismatch {
            expected: public.anchor_block_hash,
            actual: previous_hash,
        });
    }

    Ok(())
}

fn decode_tempo_header(index: usize, encoded: &Bytes) -> Result<TempoHeader, ProverError> {
    let mut cursor = encoded.as_ref();
    let header = TempoHeader::decode(&mut cursor)
        .map_err(|_| ProverError::TempoAncestryHeaderInvalid { index })?;
    if !cursor.is_empty() {
        return Err(ProverError::TempoAncestryHeaderInvalid { index });
    }
    Ok(header)
}
