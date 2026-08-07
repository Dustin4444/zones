//! Checker-owned fixed Zone storage access layouts.
//!
//! These keys and decoders are expected-value inputs for later exact-state
//! comparisons. They do not import production precompile layouts.

use alloy_primitives::{Address, B256, U256};

use super::constants::{
    TEMPO_STATE_ADDRESS, TIP20_TOTAL_SUPPLY_SLOT, ZONE_FEE_MANAGER_ADDRESS, ZONE_INBOX_ADDRESS,
    ZONE_OUTBOX_ADDRESS,
};

/// One literal account/slot pair read from exact Zone state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExactStateAccess {
    pub(crate) address: Address,
    pub(crate) slot: U256,
}

impl ExactStateAccess {
    pub(crate) const fn new(address: Address, slot: U256) -> Self {
        Self { address, slot }
    }

    /// The 32-byte big-endian key accepted by Reth's state provider.
    pub(crate) fn storage_key(self) -> B256 {
        B256::from(self.slot.to_be_bytes::<32>())
    }
}

const SLOT_ZERO: U256 = U256::ZERO;
const SLOT_ONE: U256 = U256::from_limbs([1, 0, 0, 0]);
const SLOT_TWO: U256 = U256::from_limbs([2, 0, 0, 0]);

/// `ZoneFeeManager.default_fee_token`: canonical Zone initial token at slot 0.
///
/// Pinned source: `crates/precompiles/src/zone_fee_manager/mod.rs:17-31` and
/// `crates/node/src/genesis.rs:101-112`.
pub(crate) const DEFAULT_FEE_TOKEN_ACCESS: ExactStateAccess =
    ExactStateAccess::new(ZONE_FEE_MANAGER_ADDRESS, SLOT_ZERO);

/// `TempoState.tempoBlockHash`: full word at slot 0.
///
/// Pinned source: `crates/precompiles/src/tempo_state.rs:31-38` and
/// `specs/ref-impls/src/tempo/TempoState.sol:17-21`.
pub(crate) const TEMPO_BLOCK_HASH_ACCESS: ExactStateAccess =
    ExactStateAccess::new(TEMPO_STATE_ADDRESS, SLOT_ZERO);

/// `TempoState.tempoBlockNumber`: `uint64` at slot 1, byte offset 0.
///
/// Pinned source: `crates/precompiles/src/tempo_state.rs:31-38` and the
/// generated layout from pinned `tempo@9161413`'s
/// `crates/precompiles-macros/src/packing.rs:287-332`.
pub(crate) const TEMPO_BLOCK_NUMBER_ACCESS: ExactStateAccess =
    ExactStateAccess::new(TEMPO_STATE_ADDRESS, SLOT_ONE);

/// `ZoneInbox.processedDepositQueueHash`: full word at slot 0.
///
/// Pinned source: `crates/precompiles/src/inbox/mod.rs:50-59` and
/// `specs/ref-impls/src/zone/ZoneInbox.sol:47-54`.
pub(crate) const INBOX_PROCESSED_DEPOSIT_HASH_ACCESS: ExactStateAccess =
    ExactStateAccess::new(ZONE_INBOX_ADDRESS, SLOT_ZERO);

/// `ZoneInbox.processedDepositNumber`: `uint64` at slot 1, byte offset 0.
///
/// Pinned source: `crates/precompiles/src/inbox/mod.rs:50-59` and
/// `specs/ref-impls/src/zone/ZoneInbox.sol:47-54`.
pub(crate) const INBOX_PROCESSED_DEPOSIT_NUMBER_ACCESS: ExactStateAccess =
    ExactStateAccess::new(ZONE_INBOX_ADDRESS, SLOT_ONE);

/// `ZoneOutbox.lastBatch().withdrawalQueueHash`: full word at slot 1.
///
/// Pinned source: `crates/precompiles/src/outbox/mod.rs:43-56`,
/// `specs/ref-impls/src/zone/ZoneOutbox.sol:64-76`, and the direct-slot
/// regression at `specs/ref-impls/test/zone/ZoneOutbox.t.sol:540-547`.
pub(crate) const OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS: ExactStateAccess =
    ExactStateAccess::new(ZONE_OUTBOX_ADDRESS, SLOT_ONE);

/// `ZoneOutbox.lastBatch().withdrawalBatchIndex`: low 64 bits of slot 2.
///
/// The rest of slot 2 is live packed state and must not be compared as part of
/// the batch index. Pinned source: `specs/ref-impls/src/zone/ZoneOutbox.sol:72-91`
/// and `specs/ref-impls/test/zone/ZoneOutbox.t.sol:540-547`.
pub(crate) const OUTBOX_LAST_BATCH_INDEX_ACCESS: ExactStateAccess =
    ExactStateAccess::new(ZONE_OUTBOX_ADDRESS, SLOT_TWO);

/// Exact TIP-20 total-supply access for one enabled Zone token.
pub(crate) const fn tip20_total_supply_access(token: Address) -> ExactStateAccess {
    ExactStateAccess::new(token, TIP20_TOTAL_SUPPLY_SLOT)
}

/// Decode a full-word `bytes32` commitment without an ABI or production helper.
pub(crate) fn decode_full_word_hash(word: U256) -> B256 {
    B256::from(word.to_be_bytes::<32>())
}

/// Decode a Solidity-compatible `uint64` at byte offset zero.
///
/// In particular, Outbox slot 2 also packs `maxWithdrawalsPerBlock`, the lazy
/// block counter, the current block number, and the last-finalized timestamp in
/// its upper 24 bytes. Pinned Tempo's packing decoder shifts by `offset * 8`
/// and masks the field width at
/// `tempo@9161413:crates/precompiles/src/storage/packing.rs:78-105`.
pub(crate) fn decode_low_u64(word: U256) -> u64 {
    (word & U256::from(u64::MAX)).to::<u64>()
}

/// Decode one canonically left-padded address word.
pub(crate) fn decode_address_word(word: U256) -> Option<Address> {
    let bytes = word.to_be_bytes::<32>();
    bytes[..12]
        .iter()
        .all(|byte| *byte == 0)
        .then(|| Address::from_slice(&bytes[12..]))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256, uint};

    use super::*;

    #[test]
    fn model_exact_state_accesses_match_literal_fixed_vectors() {
        let cases = [
            (
                TEMPO_BLOCK_HASH_ACCESS,
                address!("1c00000000000000000000000000000000000000"),
                b256!("0000000000000000000000000000000000000000000000000000000000000000"),
            ),
            (
                TEMPO_BLOCK_NUMBER_ACCESS,
                address!("1c00000000000000000000000000000000000000"),
                b256!("0000000000000000000000000000000000000000000000000000000000000001"),
            ),
            (
                INBOX_PROCESSED_DEPOSIT_HASH_ACCESS,
                address!("1c00000000000000000000000000000000000001"),
                b256!("0000000000000000000000000000000000000000000000000000000000000000"),
            ),
            (
                INBOX_PROCESSED_DEPOSIT_NUMBER_ACCESS,
                address!("1c00000000000000000000000000000000000001"),
                b256!("0000000000000000000000000000000000000000000000000000000000000001"),
            ),
            (
                OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS,
                address!("1c00000000000000000000000000000000000002"),
                b256!("0000000000000000000000000000000000000000000000000000000000000001"),
            ),
            (
                OUTBOX_LAST_BATCH_INDEX_ACCESS,
                address!("1c00000000000000000000000000000000000002"),
                b256!("0000000000000000000000000000000000000000000000000000000000000002"),
            ),
            (
                DEFAULT_FEE_TOKEN_ACCESS,
                address!("feec000000000000000000000000000000000001"),
                B256::ZERO,
            ),
        ];

        for (access, expected_address, expected_key) in cases {
            assert_eq!(access.address, expected_address);
            assert_eq!(access.storage_key(), expected_key);
        }

        let token = address!("20c00000000000000000000000000000000000aa");
        let supply = tip20_total_supply_access(token);
        assert_eq!(supply.address, token);
        assert_eq!(
            supply.storage_key(),
            b256!("0000000000000000000000000000000000000000000000000000000000000008")
        );
    }

    #[test]
    fn model_full_word_hash_decode_is_byte_exact() {
        let word = uint!(0x000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f_U256);
        assert_eq!(
            decode_full_word_hash(word),
            b256!("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
        );
    }

    #[test]
    fn model_low_u64_decode_ignores_other_outbox_slot_two_fields() {
        // High-to-low bytes: timestamp | current block | counted withdrawals |
        // max withdrawals | withdrawal batch index.
        let packed = uint!(0x4142434445464748313233343536373821222324111213140102030405060708_U256);
        assert_eq!(decode_low_u64(packed), 0x0102_0304_0506_0708);
    }

    #[test]
    fn model_address_word_decode_requires_canonical_padding() {
        let address = address!("20c0000000000000000000000000000000001234");
        let canonical = U256::from_be_slice(B256::left_padding_from(address.as_slice()).as_slice());
        assert_eq!(decode_address_word(canonical), Some(address));

        let malformed = canonical | (U256::from(1) << 248);
        assert_eq!(decode_address_word(malformed), None);
    }
}
