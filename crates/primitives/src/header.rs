//! Zone block header type with RLP encoding and hash computation.

use alloc::vec::Vec;
use alloy_primitives::{Address, B256};
use alloy_rlp::{Decodable, Encodable as _};

/// Simplified zone block header for hash computation.
///
/// The zone block hash is `keccak256(rlp_encode(header))`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ZoneHeader {
    pub parent_hash: B256,
    pub beneficiary: Address,
    pub state_root: B256,
    pub transactions_root: B256,
    pub receipts_root: B256,
    pub number: u64,
    pub timestamp: u64,
    pub protocol_version: u64,
}

impl alloy_rlp::Encodable for ZoneHeader {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        alloy_rlp::Header {
            list: true,
            payload_length: self.fields_len(),
        }
        .encode(out);
        self.parent_hash.encode(out);
        self.beneficiary.encode(out);
        self.state_root.encode(out);
        self.transactions_root.encode(out);
        self.receipts_root.encode(out);
        self.number.encode(out);
        self.timestamp.encode(out);
        self.protocol_version.encode(out);
    }

    fn length(&self) -> usize {
        let header_len = alloy_rlp::Header {
            list: true,
            payload_length: self.fields_len(),
        }
        .length();
        checked_len_add(header_len, self.fields_len())
    }
}

impl alloy_rlp::Decodable for ZoneHeader {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = alloy_rlp::Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }

        let started_len = buf.len();
        let decoded = Self {
            parent_hash: Decodable::decode(buf)?,
            beneficiary: Decodable::decode(buf)?,
            state_root: Decodable::decode(buf)?,
            transactions_root: Decodable::decode(buf)?,
            receipts_root: Decodable::decode(buf)?,
            number: u64::decode(buf)?,
            timestamp: u64::decode(buf)?,
            protocol_version: u64::decode(buf)?,
        };

        let consumed = started_len
            .checked_sub(buf.len())
            .ok_or(alloy_rlp::Error::Custom(
                "zone header decode length underflow",
            ))?;
        if consumed != header.payload_length {
            return Err(alloy_rlp::Error::ListLengthMismatch {
                expected: header.payload_length,
                got: consumed,
            });
        }

        Ok(decoded)
    }
}

impl ZoneHeader {
    fn fields_len(&self) -> usize {
        let len = self.parent_hash.length();
        let len = checked_len_add(len, self.beneficiary.length());
        let len = checked_len_add(len, self.state_root.length());
        let len = checked_len_add(len, self.transactions_root.length());
        let len = checked_len_add(len, self.receipts_root.length());
        let len = checked_len_add(len, self.number.length());
        let len = checked_len_add(len, self.timestamp.length());
        checked_len_add(len, self.protocol_version.length())
    }

    /// Compute the block hash: `keccak256(rlp_encode(self))`.
    pub fn hash(&self) -> B256 {
        use alloy_rlp::Encodable;
        let mut buf = Vec::with_capacity(self.length());
        self.encode(&mut buf);
        alloy_primitives::keccak256(&buf)
    }
}

fn checked_len_add(left: usize, right: usize) -> usize {
    left.checked_add(right)
        .expect("zone header RLP length exceeds usize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use alloy_rlp::{Decodable, Encodable};

    fn header() -> ZoneHeader {
        ZoneHeader {
            parent_hash: B256::repeat_byte(0x01),
            beneficiary: address!("0x0000000000000000000000000000000000001000"),
            state_root: B256::repeat_byte(0x02),
            transactions_root: B256::repeat_byte(0x03),
            receipts_root: B256::repeat_byte(0x04),
            number: 7,
            timestamp: 42,
            protocol_version: 1,
        }
    }

    #[test]
    fn rlp_roundtrip_preserves_zone_header_hash() {
        let original = header();
        let mut encoded = Vec::new();
        original.encode(&mut encoded);

        let decoded = ZoneHeader::decode(&mut encoded.as_slice()).unwrap();

        assert_eq!(decoded, original);
        assert_eq!(decoded.hash(), original.hash());
    }

    #[test]
    fn rlp_decode_leaves_trailing_bytes_for_exact_callers() {
        let mut encoded = Vec::new();
        header().encode(&mut encoded);
        encoded.push(0);

        let mut cursor = encoded.as_slice();
        ZoneHeader::decode(&mut cursor).unwrap();

        assert!(!cursor.is_empty());
    }

    #[test]
    fn rlp_decode_rejects_non_list_header() {
        let mut cursor = [0x80].as_slice();

        assert!(ZoneHeader::decode(&mut cursor).is_err());
    }
}
