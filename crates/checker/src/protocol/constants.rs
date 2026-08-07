//! Literal protocol constants owned by the checker.

use alloy_primitives::{Address, U256, address};

/// Zone checkpoint predeploy.
///
/// Pinned source: `crates/primitives/src/constants.rs:25`.
pub(crate) const TEMPO_STATE_ADDRESS: Address =
    address!("0x1c00000000000000000000000000000000000000");

/// Zone deposit Inbox predeploy.
///
/// Pinned source: `crates/primitives/src/constants.rs:28`.
pub(crate) const ZONE_INBOX_ADDRESS: Address =
    address!("0x1c00000000000000000000000000000000000001");

/// Zone withdrawal Outbox predeploy.
///
/// Pinned source: `crates/primitives/src/constants.rs:31`.
pub(crate) const ZONE_OUTBOX_ADDRESS: Address =
    address!("0x1c00000000000000000000000000000000000002");

/// Zone-native fee manager predeploy.
///
/// Pinned source: `crates/primitives/src/constants.rs:47-52`.
pub(crate) const ZONE_FEE_MANAGER_ADDRESS: Address =
    address!("0xfeec000000000000000000000000000000000001");

/// Native TIP-1091 factory address.
///
/// Pinned source: `crates/contracts/src/precompiles/zone_factory.rs:8`.
pub(crate) const ZONE_FACTORY_ADDRESS: Address =
    address!("0x5af2000000000000000000000000000000000000");

/// Maximum callback payload accepted by the pinned Outbox.
///
/// Pinned source: `crates/precompiles/src/outbox/mod.rs:32`.
pub(crate) const MAX_CALLBACK_DATA_SIZE: usize = 1_024;

/// Maximum deposits imported from one Tempo block.
///
/// Pinned source: `specs/ref-impls/src/tempo/ZonePortal.sol:62`.
pub(crate) const MAX_DEPOSITS_PER_TEMPO_BLOCK: usize = 230;

/// Maximum token enablements imported from one Tempo block.
///
/// Pinned source: `specs/ref-impls/src/tempo/ZonePortal.sol:67`.
pub(crate) const MAX_TOKENS_ENABLED_PER_TEMPO_BLOCK: usize = 8;

/// Maximum UTF-8 byte length of an enabled token name.
///
/// Pinned source: `specs/ref-impls/src/tempo/ZonePortal.sol:72`.
pub(crate) const MAX_TOKEN_NAME_BYTES: usize = 64;

/// Maximum UTF-8 byte length of an enabled token symbol.
///
/// Pinned source: `specs/ref-impls/src/tempo/ZonePortal.sol:73`.
pub(crate) const MAX_TOKEN_SYMBOL_BYTES: usize = 31;

/// Maximum UTF-8 byte length of an enabled token currency code.
///
/// Pinned source: `specs/ref-impls/src/tempo/ZonePortal.sol:74`.
pub(crate) const MAX_TOKEN_CURRENCY_BYTES: usize = 31;

/// Maximum number of Portal sequencers.
///
/// Pinned source: `specs/ref-impls/src/tempo/ZonePortal.sol:92`.
pub(crate) const MAX_SEQUENCERS: usize = 8;

/// Exact SEC1 compressed public-key length.
///
/// Pinned source: `crates/precompiles/src/ecies.rs:29` and
/// `specs/ref-impls/src/zone/ZoneOutbox.sol:50-55`.
pub(crate) const COMPRESSED_PUBLIC_KEY_SIZE: usize = 33;

/// Exact encrypted-sender length when selective reveal is enabled.
///
/// Pinned source: `specs/ref-impls/src/zone/ZoneOutbox.sol:50-55`.
pub(crate) const AUTHENTICATED_WITHDRAWAL_SIZE: usize = 113;

/// Exact encrypted ordinary-deposit ciphertext length.
///
/// Pinned source: `specs/ref-impls/src/libraries/EncryptedDeposit.sol:29`.
pub(crate) const ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE: usize = 64;

/// TIP-20 total supply is stored in literal slot 8.
///
/// Pinned source: `tempo@9161413:crates/precompiles/src/tip20/mod.rs:74-103`
/// and `tempo@9161413:crates/precompiles/tests/storage_tests/solidity/testdata/
/// tip20.layout.json:78-84`.
pub(crate) const TIP20_TOTAL_SUPPLY_SLOT: U256 = U256::from_limbs([8, 0, 0, 0]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_literal_constants_are_pinned() {
        assert_eq!(
            TEMPO_STATE_ADDRESS,
            address!("1c00000000000000000000000000000000000000")
        );
        assert_eq!(
            ZONE_INBOX_ADDRESS,
            address!("1c00000000000000000000000000000000000001")
        );
        assert_eq!(
            ZONE_OUTBOX_ADDRESS,
            address!("1c00000000000000000000000000000000000002")
        );
        assert_eq!(
            ZONE_FEE_MANAGER_ADDRESS,
            address!("feec000000000000000000000000000000000001")
        );
        assert_eq!(
            ZONE_FACTORY_ADDRESS,
            address!("5af2000000000000000000000000000000000000")
        );
        assert_eq!(MAX_CALLBACK_DATA_SIZE, 1_024);
        assert_eq!(MAX_DEPOSITS_PER_TEMPO_BLOCK, 230);
        assert_eq!(MAX_TOKENS_ENABLED_PER_TEMPO_BLOCK, 8);
        assert_eq!(MAX_TOKEN_NAME_BYTES, 64);
        assert_eq!(MAX_TOKEN_SYMBOL_BYTES, 31);
        assert_eq!(MAX_TOKEN_CURRENCY_BYTES, 31);
        assert_eq!(MAX_SEQUENCERS, 8);
        assert_eq!(COMPRESSED_PUBLIC_KEY_SIZE, 33);
        assert_eq!(TIP20_TOTAL_SUPPLY_SLOT, U256::from(8));
    }
}
