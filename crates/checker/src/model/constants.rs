//! Literal protocol constants owned by the checker.

use alloy_primitives::{Address, B256, U256, address, b256};

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

/// Native TIP-1091 factory address.
///
/// Pinned source: `crates/contracts/src/precompiles/zone_factory.rs:8`.
pub(crate) const ZONE_FACTORY_ADDRESS: Address =
    address!("0x5af2000000000000000000000000000000000000");

/// Native ZoneFactory portal-address prefix.
///
/// Pinned source: `tempo@9161413:crates/primitives/src/address.rs:77`.
pub(crate) const ZONE_PORTAL_ADDRESS_PREFIX: [u8; 12] = [0x5a, 0xd0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// The tail value used while folding a non-empty withdrawal queue.
///
/// Pinned source: `specs/ref-impls/src/libraries/WithdrawalQueueLib.sol:6-8`.
pub(crate) const EMPTY_WITHDRAWAL_QUEUE_SENTINEL: B256 =
    b256!("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");

/// The queue index emitted for an empty submitted batch.
///
/// Pinned source: `specs/ref-impls/src/libraries/WithdrawalQueueLib.sol:11-13`.
pub(crate) const NO_WITHDRAWAL_QUEUE_INDEX: U256 = U256::MAX;

/// Maximum number of non-empty batch slots retained by the Portal withdrawal ring.
///
/// Pinned source: `specs/ref-impls/src/libraries/WithdrawalQueueLib.sol:15-16`.
pub(crate) const WITHDRAWAL_QUEUE_CAPACITY: U256 = U256::from_limbs([100, 0, 0, 0]);

/// Fixed gas charged in addition to a user's callback gas limit.
///
/// Pinned source: `crates/precompiles/src/outbox/mod.rs:33` and
/// `specs/ref-impls/src/zone/ZoneOutbox.sol:46-48`.
pub(crate) const WITHDRAWAL_BASE_GAS: u64 = 50_000;

/// Maximum callback gas accepted by the pinned Outbox.
///
/// Pinned source: `specs/ref-impls/src/interfaces/IZone.sol:270-274`.
pub(crate) const MAX_WITHDRAWAL_GAS_LIMIT: u64 = 10_000_000;

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

/// Scale from Tempo's 18-decimal base fee to six-decimal TIP-20 units.
///
/// Pinned source: `specs/ref-impls/src/tempo/ZonePortal.sol:80-81`.
pub(crate) const BOUNCE_BACK_BASE_FEE_SCALE: u128 = 1_000_000_000_000;

/// TIP-20 total supply is stored in literal slot 8.
///
/// Pinned source: `tempo@9161413:crates/precompiles/src/tip20/mod.rs:74-103`
/// and `tempo@9161413:crates/precompiles/tests/storage_tests/solidity/testdata/
/// tip20.layout.json:78-84`.
pub(crate) const TIP20_TOTAL_SUPPLY_SLOT: U256 = U256::from_limbs([8, 0, 0, 0]);

/// Native deposit kind discriminator for withdrawal bounce-backs.
///
/// Pinned source: `crates/contracts/src/precompiles/zone_inbox.rs:30-36`.
pub(crate) const WITHDRAWAL_BOUNCE_BACK_DEPOSIT_KIND: u8 = 0;

/// Native deposit kind discriminator for ordinary deposits.
///
/// Pinned source: `crates/contracts/src/precompiles/zone_inbox.rs:30-36`.
pub(crate) const ORDINARY_DEPOSIT_KIND: u8 = 1;

/// Literal release-one configuration baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InitialConfig {
    pub(crate) tempo_gas_rate: u128,
    pub(crate) max_withdrawals_per_block: u32,
    pub(crate) bounceback_gas: u64,
}

/// The checker never infers its baseline from observed live configuration.
///
/// `tempo_gas_rate` and `max_withdrawals_per_block` are zero-initialized because
/// the generated `ZoneOutbox::__initialize` only installs the account code and
/// does not write scalar storage. Pinned sources:
/// `tempo@9161413:crates/precompiles-macros/src/layout.rs:167-170` and the fields
/// declared at `crates/precompiles/src/outbox/mod.rs:43-55`. `bounceback_gas` is
/// explicitly documented as defaulting to zero at
/// `specs/ref-impls/src/tempo/ZonePortal.sol:132-134`.
pub(crate) const INITIAL_CONFIG: InitialConfig = InitialConfig {
    tempo_gas_rate: 0,
    max_withdrawals_per_block: 0,
    bounceback_gas: 0,
};

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
            ZONE_FACTORY_ADDRESS,
            address!("5af2000000000000000000000000000000000000")
        );
        assert_eq!(
            ZONE_PORTAL_ADDRESS_PREFIX,
            [0x5a, 0xd0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(EMPTY_WITHDRAWAL_QUEUE_SENTINEL, B256::repeat_byte(0xff));
        assert_eq!(NO_WITHDRAWAL_QUEUE_INDEX, U256::MAX);
        assert_eq!(WITHDRAWAL_QUEUE_CAPACITY, U256::from(100));
        assert_eq!(WITHDRAWAL_BASE_GAS, 50_000);
        assert_eq!(MAX_CALLBACK_DATA_SIZE, 1_024);
        assert_eq!(MAX_DEPOSITS_PER_TEMPO_BLOCK, 230);
        assert_eq!(MAX_TOKENS_ENABLED_PER_TEMPO_BLOCK, 8);
        assert_eq!(MAX_TOKEN_NAME_BYTES, 64);
        assert_eq!(MAX_TOKEN_SYMBOL_BYTES, 31);
        assert_eq!(MAX_TOKEN_CURRENCY_BYTES, 31);
        assert_eq!(MAX_SEQUENCERS, 8);
        assert_eq!(COMPRESSED_PUBLIC_KEY_SIZE, 33);
        assert_eq!(BOUNCE_BACK_BASE_FEE_SCALE, 10_u128.pow(12));
        assert_eq!(TIP20_TOTAL_SUPPLY_SLOT, U256::from(8));
    }

    #[test]
    fn model_initial_configuration_is_literal_zero() {
        assert_eq!(
            INITIAL_CONFIG,
            InitialConfig {
                tempo_gas_rate: 0,
                max_withdrawals_per_block: 0,
                bounceback_gas: 0,
            }
        );
    }
}
