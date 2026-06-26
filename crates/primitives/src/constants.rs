//! Zone protocol constants shared between host and guest.

use alloy_primitives::{Address, B256, address};

/// Sentinel value for empty withdrawal queue slots.
pub const EMPTY_SENTINEL: B256 = B256::new([0xff; 32]);

/// Maximum callback gas a withdrawal may request.
///
/// The L1 processor adds fixed overhead plus an EIP-150 cushion, so this value
/// keeps the outer `processWithdrawal` transaction well below a 30M gas block.
pub const MAX_WITHDRAWAL_GAS_LIMIT: u64 = 10_000_000;

/// TempoState predeploy address on Zone L2.
pub const TEMPO_STATE_ADDRESS: Address = address!("0x1c00000000000000000000000000000000000000");

include!(concat!(env!("OUT_DIR"), "/tempo_state_slots.rs"));

/// ZoneInbox predeploy address on Zone L2.
pub const ZONE_INBOX_ADDRESS: Address = address!("0x1c00000000000000000000000000000000000001");

/// ZoneOutbox predeploy address on Zone L2.
pub const ZONE_OUTBOX_ADDRESS: Address = address!("0x1c00000000000000000000000000000000000002");

/// ZoneConfig predeploy address on Zone L2.
pub const ZONE_CONFIG_ADDRESS: Address = address!("0x1c00000000000000000000000000000000000003");

/// TempoStateReader precompile address on Zone L2.
pub const TEMPO_STATE_READER_ADDRESS: Address =
    address!("0x1c00000000000000000000000000000000000004");

/// ZoneTxContext precompile address on Zone L2.
pub const ZONE_TX_CONTEXT_ADDRESS: Address = address!("0x1c00000000000000000000000000000000000005");

/// Chaum-Pedersen verification precompile address.
pub const CHAUM_PEDERSEN_VERIFY_ADDRESS: Address =
    address!("0x1C00000000000000000000000000000000000100");

/// AES-GCM decryption precompile address.
pub const AES_GCM_DECRYPT_ADDRESS: Address = address!("0x1C00000000000000000000000000000000000101");

/// TIP-20 zone token factory precompile address.
pub const ZONE_TIP20_FACTORY_ADDRESS: Address =
    address!("0x20Fc000000000000000000000000000000000000");

/// Default zone token address (pathUSD TIP-20).
pub const ZONE_TOKEN_ADDRESS: Address = address!("0x20C0000000000000000000000000000000000000");

/// ZonePortal storage slot 0: `sequencer` (address).
pub const PORTAL_SEQUENCER_SLOT: B256 = B256::ZERO;

/// ZonePortal storage slot 1: `admin` (address).
pub const PORTAL_ADMIN_SLOT: B256 = {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    B256::new(bytes)
};

/// ZonePortal storage slot 2: `pendingSequencer` (address).
pub const PORTAL_PENDING_SEQUENCER_SLOT: B256 = {
    let mut bytes = [0u8; 32];
    bytes[31] = 2;
    B256::new(bytes)
};

/// Base offset for deriving **mainnet** zone chain IDs.
///
/// Each zone gets a unique EIP-155 chain ID derived from its on-chain zone ID
/// assigned by the `ZoneFactory` contract:
///
/// ```text
/// chain_id = ZONE_CHAIN_ID_BASE + (zone_id % ZONE_CHAIN_ID_RANGE)
/// ```
///
/// # Range safety
///
/// EIP-2294 and ENSIP-11 reserve bit 31 (`0x8000_0000`) for coin-type flags,
/// making chain IDs ≥ 2^31 (2,147,483,647) unsafe in parts of the ecosystem
/// (ENS multi-chain address resolution, some JavaScript tooling that uses
/// 32-bit integers, etc.).
///
/// The ranges are chosen so that both mainnet and testnet zones stay well below
/// that limit while remaining non-overlapping:
///
/// | Network  | Base            | Range size        | Chain ID span                         |
/// |----------|-----------------|-------------------|---------------------------------------|
/// | Mainnet  | `421_700_000`   | `1_002_610_000`   | `421_700_000 ..  1_424_310_000`       |
/// | Testnet  | `1_424_310_000` | `723_173_648`     | `1_424_310_000 .. 2_147_483_648`      |
///
/// Zone IDs wrap around via modular arithmetic so that chain IDs never leave
/// their designated range, even with arbitrarily large zone IDs. Because
/// wrapping can reuse a chain ID that was previously assigned to a different
/// zone, it is the responsibility of the sequencer to ensure — after deploying
/// a zone — that the derived chain ID does not correspond to any active chain,
/// including any zone that has previously used that chain ID.
pub const ZONE_CHAIN_ID_BASE: u64 = 421_700_000;

/// Number of distinct mainnet zone chain IDs before wrapping.
///
/// Equal to `ZONE_CHAIN_ID_BASE_TESTNET - ZONE_CHAIN_ID_BASE`, keeping the
/// mainnet range strictly below the testnet range.
pub const ZONE_CHAIN_ID_RANGE: u64 = 1_002_610_000;

/// Base offset for deriving **testnet** (Moderato) zone chain IDs.
///
/// See [`ZONE_CHAIN_ID_BASE`] for range-safety rationale.
pub const ZONE_CHAIN_ID_BASE_TESTNET: u64 = 1_424_310_000;

/// Number of distinct testnet zone chain IDs before wrapping.
///
/// Equal to `2^31 - ZONE_CHAIN_ID_BASE_TESTNET`, keeping the testnet range
/// strictly below the EIP-2294 safe ceiling.
pub const ZONE_CHAIN_ID_RANGE_TESTNET: u64 = 723_173_648;

/// Derives the EIP-155 chain ID for a **mainnet** zone from its on-chain zone ID.
///
/// Wraps via modulo so the result always falls in
/// `[ZONE_CHAIN_ID_BASE, ZONE_CHAIN_ID_BASE + ZONE_CHAIN_ID_RANGE)`.
pub fn zone_chain_id(zone_id: u32) -> u64 {
    let offset = u64::from(zone_id).rem_euclid(ZONE_CHAIN_ID_RANGE);
    match ZONE_CHAIN_ID_BASE.checked_add(offset) {
        Some(chain_id) => chain_id,
        None => panic!("zone chain ID exceeds u64"),
    }
}

/// Derives the EIP-155 chain ID for a **testnet** zone from its on-chain zone ID.
///
/// Wraps via modulo so the result always falls in
/// `[ZONE_CHAIN_ID_BASE_TESTNET, ZONE_CHAIN_ID_BASE_TESTNET + ZONE_CHAIN_ID_RANGE_TESTNET)`.
pub fn zone_chain_id_testnet(zone_id: u32) -> u64 {
    let offset = u64::from(zone_id).rem_euclid(ZONE_CHAIN_ID_RANGE_TESTNET);
    match ZONE_CHAIN_ID_BASE_TESTNET.checked_add(offset) {
        Some(chain_id) => chain_id,
        None => panic!("testnet zone chain ID exceeds u64"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;

    #[test]
    fn tempo_state_slots_use_solidity_storage_key_endianness() {
        assert_eq!(TEMPO_BLOCK_HASH_SLOT, B256::from(U256::ZERO));
        assert_eq!(TEMPO_STATE_ROOT_SLOT, B256::from(U256::from(4)));
        assert_eq!(TEMPO_PACKED_SLOT, B256::from(U256::from(7)));

        let tempo_state_root_slot: U256 = TEMPO_STATE_ROOT_SLOT.into();
        let tempo_packed_slot: U256 = TEMPO_PACKED_SLOT.into();
        assert_eq!(tempo_state_root_slot, U256::from(4));
        assert_eq!(tempo_packed_slot, U256::from(7));
    }

    #[test]
    fn inbox_and_outbox_slots_match_solidity_storage_layout() {
        assert_eq!(ZONE_INBOX_PROCESSED_HASH_SLOT, U256::ZERO);
        assert_eq!(ZONE_INBOX_PROCESSED_NUMBER_SLOT, U256::from(1));
        assert_eq!(ZONE_OUTBOX_LAST_BATCH_HASH_SLOT, U256::from(1));
        assert_eq!(ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT, U256::from(2));
    }

    #[test]
    fn portal_deposit_queue_slot_matches_solidity_storage_layout() {
        assert_eq!(
            PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT,
            B256::from(U256::from(5))
        );

        let current_deposit_queue_hash_slot: U256 = PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT.into();
        assert_eq!(current_deposit_queue_hash_slot, U256::from(5));
    }
}
