//! Zone block header type with RLP encoding and hash computation.

use alloc::vec::Vec;
use alloy_primitives::{Address, B256};
use alloy_rlp::Encodable as _;

/// Simplified zone block header for hash computation.
///
/// The zone block hash is `keccak256(rlp_encode(header))`.
#[derive(Debug, Clone)]
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
