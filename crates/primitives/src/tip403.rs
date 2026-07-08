//! TIP-403 registry storage helpers shared by the node and prover.
//!
//! These helpers mirror the Tempo `TIP403Registry` precompile storage layout
//! without depending on the Tempo precompile crate. They are intentionally small
//! and no_std-compatible so witness-backed execution can derive exactly the L1
//! slots it must prove.

use alloy_primitives::{Address, B256, U256, keccak256};

/// Builtin TIP-403 policy that always rejects.
pub const REJECT_ALL_POLICY_ID: u64 = 0;

/// Builtin TIP-403 policy that always allows.
pub const ALLOW_ALL_POLICY_ID: u64 = 1;

/// Raw `ITIP403Registry.PolicyType.WHITELIST`.
pub const POLICY_TYPE_WHITELIST: u8 = 0;

/// Raw `ITIP403Registry.PolicyType.BLACKLIST`.
pub const POLICY_TYPE_BLACKLIST: u8 = 1;

/// Raw `ITIP403Registry.PolicyType.COMPOUND`.
pub const POLICY_TYPE_COMPOUND: u8 = 2;

/// `TIP403Registry.policy_id_counter` slot.
pub const POLICY_ID_COUNTER_SLOT: U256 = U256::from_limbs([0, 0, 0, 0]);

/// `TIP403Registry.policy_records` mapping base slot.
pub const POLICY_RECORDS_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);

/// `TIP403Registry.policy_set` mapping base slot.
pub const POLICY_SET_SLOT: U256 = U256::from_limbs([2, 0, 0, 0]);

const POLICY_DATA_TYPE_OFFSET: usize = 0;
const POLICY_DATA_TYPE_BYTES: usize = 1;
const POLICY_DATA_ADMIN_OFFSET: usize = 1;
const POLICY_DATA_ADMIN_BYTES: usize = 20;

const COMPOUND_SENDER_OFFSET: usize = 0;
const COMPOUND_RECIPIENT_OFFSET: usize = 8;
const COMPOUND_MINT_RECIPIENT_OFFSET: usize = 16;
const COMPOUND_POLICY_ID_BYTES: usize = 8;

/// Decoded `TIP403Registry.PolicyData`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tip403PolicyData {
    pub policy_type: u8,
    pub admin: Address,
}

/// Decoded `TIP403Registry.CompoundPolicyData`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tip403CompoundPolicyData {
    pub sender_policy_id: u64,
    pub recipient_policy_id: u64,
    pub mint_recipient_policy_id: u64,
}

/// Compute the storage slot for `policy_records[policy_id].base`.
pub fn policy_record_base_slot(policy_id: u64) -> U256 {
    mapping_slot_u64(policy_id, POLICY_RECORDS_SLOT)
}

/// Compute the storage slot for `policy_records[policy_id].compound`.
pub fn policy_record_compound_slot(policy_id: u64) -> U256 {
    policy_record_base_slot(policy_id) + U256::ONE
}

/// Compute the storage slot for `policy_set[policy_id][user]`.
pub fn policy_set_account_slot(policy_id: u64, user: Address) -> U256 {
    let policy_slot = mapping_slot_u64(policy_id, POLICY_SET_SLOT);
    mapping_slot_address(user, policy_slot)
}

/// Decode `TIP403Registry.PolicyData` from a packed storage word.
pub fn decode_policy_data(word: U256) -> Tip403PolicyData {
    Tip403PolicyData {
        policy_type: extract_u8(word, POLICY_DATA_TYPE_OFFSET, POLICY_DATA_TYPE_BYTES),
        admin: extract_address(word, POLICY_DATA_ADMIN_OFFSET, POLICY_DATA_ADMIN_BYTES),
    }
}

/// Decode `TIP403Registry.CompoundPolicyData` from a packed storage word.
pub fn decode_compound_policy_data(word: U256) -> Tip403CompoundPolicyData {
    Tip403CompoundPolicyData {
        sender_policy_id: extract_u64(word, COMPOUND_SENDER_OFFSET, COMPOUND_POLICY_ID_BYTES),
        recipient_policy_id: extract_u64(word, COMPOUND_RECIPIENT_OFFSET, COMPOUND_POLICY_ID_BYTES),
        mint_recipient_policy_id: extract_u64(
            word,
            COMPOUND_MINT_RECIPIENT_OFFSET,
            COMPOUND_POLICY_ID_BYTES,
        ),
    }
}

/// Encode the raw `PolicyData` fields into one storage word. Intended for tests
/// and fixtures that need to mirror Tempo storage exactly.
pub fn encode_policy_data(policy_type: u8, admin: Address) -> U256 {
    insert_u8(
        insert_address(U256::ZERO, admin, POLICY_DATA_ADMIN_OFFSET),
        policy_type,
        0,
    )
}

/// Encode `CompoundPolicyData` into one storage word. Intended for tests and
/// fixtures that need to mirror Tempo storage exactly.
pub fn encode_compound_policy_data(
    sender_policy_id: u64,
    recipient_policy_id: u64,
    mint_recipient_policy_id: u64,
) -> U256 {
    let word = insert_u64(U256::ZERO, sender_policy_id, COMPOUND_SENDER_OFFSET);
    let word = insert_u64(word, recipient_policy_id, COMPOUND_RECIPIENT_OFFSET);
    insert_u64(
        word,
        mint_recipient_policy_id,
        COMPOUND_MINT_RECIPIENT_OFFSET,
    )
}

fn mapping_slot_u64(key: u64, slot: U256) -> U256 {
    mapping_slot(&key.to_be_bytes(), slot)
}

fn mapping_slot_address(key: Address, slot: U256) -> U256 {
    mapping_slot(key.as_slice(), slot)
}

fn mapping_slot(key: &[u8], slot: U256) -> U256 {
    debug_assert!(key.len() <= 32);
    let mut buf = [0u8; 64];
    buf[32 - key.len()..32].copy_from_slice(key);
    buf[32..].copy_from_slice(&slot.to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(buf).0)
}

fn extract_u8(word: U256, offset: usize, bytes: usize) -> u8 {
    ((word >> (offset * 8)) & mask(bytes)).to::<u8>()
}

fn extract_u64(word: U256, offset: usize, bytes: usize) -> u64 {
    ((word >> (offset * 8)) & mask(bytes)).to::<u64>()
}

fn extract_address(word: U256, offset: usize, bytes: usize) -> Address {
    debug_assert_eq!(bytes, 20);
    let value = (word >> (offset * 8)) & mask(bytes);
    let bytes = value.to_be_bytes::<32>();
    Address::from_slice(&bytes[12..])
}

fn insert_u8(word: U256, value: u8, offset: usize) -> U256 {
    insert_word(word, U256::from(value), offset, 1)
}

fn insert_u64(word: U256, value: u64, offset: usize) -> U256 {
    insert_word(word, U256::from(value), offset, 8)
}

fn insert_address(word: U256, value: Address, offset: usize) -> U256 {
    insert_word(word, U256::from_be_slice(value.as_slice()), offset, 20)
}

fn insert_word(word: U256, value: U256, offset: usize, bytes: usize) -> U256 {
    let mask = mask(bytes);
    let shift = offset * 8;
    (word & !(mask << shift)) | ((value & mask) << shift)
}

fn mask(bytes: usize) -> U256 {
    if bytes >= 32 {
        U256::MAX
    } else {
        (U256::ONE << (bytes * 8)) - U256::ONE
    }
}

/// Convert a `U256` storage slot to the `B256` shape used by node-side L1
/// storage readers.
pub fn slot_b256(slot: U256) -> B256 {
    B256::from(slot)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::*;

    #[test]
    fn policy_data_roundtrips_storage_word() {
        let admin = address!("0x1111111111111111111111111111111111111111");

        let word = encode_policy_data(POLICY_TYPE_BLACKLIST, admin);

        assert_eq!(
            decode_policy_data(word),
            Tip403PolicyData {
                policy_type: POLICY_TYPE_BLACKLIST,
                admin,
            }
        );
    }

    #[test]
    fn compound_data_roundtrips_storage_word() {
        let word = encode_compound_policy_data(2, 3, 4);

        assert_eq!(
            decode_compound_policy_data(word),
            Tip403CompoundPolicyData {
                sender_policy_id: 2,
                recipient_policy_id: 3,
                mint_recipient_policy_id: 4,
            }
        );
    }

    #[test]
    fn nested_policy_set_slot_is_stable() {
        let user = address!("0x2222222222222222222222222222222222222222");
        let policy_slot = mapping_slot_u64(7, POLICY_SET_SLOT);

        assert_eq!(
            policy_set_account_slot(7, user),
            mapping_slot_address(user, policy_slot)
        );
    }
}
