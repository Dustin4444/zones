use alloc::collections::BTreeMap;

use alloy_primitives::{B256, Bytes, keccak256};
use alloy_rlp::Decodable;
use zone_primitives::ZoneHeader;

use crate::ProverError;

/// EVM `BLOCKHASH` can access the 256 most recent ancestor blocks.
pub(crate) const BLOCKHASH_ANCESTOR_LIMIT: usize = 256;

/// Verify Zone ancestor headers and materialize the `BLOCKHASH` lookup map.
///
/// `prev_header` is the parent of the first block in the batch. Additional
/// ancestry headers are ordered newest-to-oldest, starting at
/// `prev_header.number - 1`.
pub(crate) fn verify_zone_ancestry_headers(
    prev_header: &ZoneHeader,
    ancestry_headers: &[Bytes],
) -> Result<BTreeMap<u64, B256>, ProverError> {
    let count = ancestry_headers.len().checked_add(1).ok_or(
        ProverError::ZoneAncestryHeaderLimitExceeded {
            count: usize::MAX,
            limit: BLOCKHASH_ANCESTOR_LIMIT,
        },
    )?;
    if count > BLOCKHASH_ANCESTOR_LIMIT {
        return Err(ProverError::ZoneAncestryHeaderLimitExceeded {
            count,
            limit: BLOCKHASH_ANCESTOR_LIMIT,
        });
    }

    let mut block_hashes = BTreeMap::new();
    block_hashes.insert(prev_header.number, prev_header.hash());

    let mut child_number = prev_header.number;
    let mut expected_parent_hash = prev_header.parent_hash;

    for (index, encoded) in ancestry_headers.iter().enumerate() {
        let header = decode_zone_header(index, encoded)?;
        let expected_number = child_number
            .checked_sub(1)
            .ok_or(ProverError::ZoneAncestryBlockNumberUnderflow { index })?;
        if header.number != expected_number {
            return Err(ProverError::ZoneAncestryBlockNumberMismatch {
                index,
                expected: expected_number,
                actual: header.number,
            });
        }

        let actual_hash = keccak256(encoded.as_ref());
        if actual_hash != expected_parent_hash {
            return Err(ProverError::ZoneAncestryParentHashMismatch {
                index,
                expected: expected_parent_hash,
                actual: actual_hash,
            });
        }

        block_hashes.insert(header.number, actual_hash);
        child_number = header.number;
        expected_parent_hash = header.parent_hash;
    }

    Ok(block_hashes)
}

fn decode_zone_header(index: usize, encoded: &Bytes) -> Result<ZoneHeader, ProverError> {
    let mut cursor = encoded.as_ref();
    let header = ZoneHeader::decode(&mut cursor)
        .map_err(|_| ProverError::ZoneAncestryHeaderInvalid { index })?;
    if !cursor.is_empty() {
        return Err(ProverError::ZoneAncestryHeaderInvalid { index });
    }
    Ok(header)
}
