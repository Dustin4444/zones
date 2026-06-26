use alloy_evm::{EvmEnv, env::BlockEnvironment};
use alloy_primitives::{Address, B256, U256};
use revm::context::{Block, BlockEnv};
use tempo_chainspec::hardfork::TempoHardfork;

use crate::PreparedZoneBlock;

pub use revm::context_interface::block::BlobExcessGasAndPrice;

/// Tempo-typed revm cfg environment for Zone stateless execution.
///
/// The caller must construct this with canonical Tempo gas params. prover-core
/// keeps this as an explicit input boundary until the no_std Tempo executor/gas
/// parameter path is forked or reused directly.
pub type ZoneCfgEnv = revm::context::CfgEnv<TempoHardfork>;

/// Complete EVM environment shape expected by a revm-backed Zone executor.
pub type ZoneEvmEnv = EvmEnv<TempoHardfork, ZoneBlockEnv>;

/// Explicit EVM block environment inputs for Zone execution.
///
/// These values are execution-relevant. The stateless executor must receive
/// them from proved witness data or another canonical block source; prover-core
/// deliberately does not invent production defaults for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ZoneBlockEnvConfig {
    pub gas_limit: u64,
    pub basefee: u64,
    pub difficulty: U256,
    pub prevrandao: Option<B256>,
    pub blob_excess_gas_and_price: Option<BlobExcessGasAndPrice>,
    pub slot_num: u64,
    pub timestamp_millis_part: u64,
}

/// no_std-compatible Tempo/Zone block environment for revm execution.
///
/// This mirrors Tempo's `TempoBlockEnv` shape while staying local to
/// prover-core so the stateless path does not depend on the std-bound Tempo
/// executor crates.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ZoneBlockEnv {
    pub inner: BlockEnv,
    pub timestamp_millis_part: u64,
}

impl ZoneBlockEnv {
    pub fn from_prepared_block(block: &PreparedZoneBlock, config: ZoneBlockEnvConfig) -> Self {
        Self {
            inner: BlockEnv {
                number: U256::from(block.number),
                beneficiary: block.beneficiary,
                timestamp: U256::from(block.timestamp),
                gas_limit: config.gas_limit,
                basefee: config.basefee,
                difficulty: config.difficulty,
                prevrandao: config.prevrandao,
                blob_excess_gas_and_price: config.blob_excess_gas_and_price,
                slot_num: config.slot_num,
            },
            timestamp_millis_part: config.timestamp_millis_part,
        }
    }

    pub fn timestamp_millis(&self) -> U256 {
        self.inner
            .timestamp
            .saturating_mul(U256::from(1000_u64))
            .saturating_add(U256::from(self.timestamp_millis_part))
    }
}

impl Block for ZoneBlockEnv {
    #[inline]
    fn number(&self) -> U256 {
        self.inner.number()
    }

    #[inline]
    fn beneficiary(&self) -> Address {
        self.inner.beneficiary()
    }

    #[inline]
    fn timestamp(&self) -> U256 {
        self.inner.timestamp()
    }

    #[inline]
    fn gas_limit(&self) -> u64 {
        self.inner.gas_limit()
    }

    #[inline]
    fn basefee(&self) -> u64 {
        self.inner.basefee()
    }

    #[inline]
    fn difficulty(&self) -> U256 {
        self.inner.difficulty()
    }

    #[inline]
    fn prevrandao(&self) -> Option<B256> {
        self.inner.prevrandao()
    }

    #[inline]
    fn blob_excess_gas_and_price(&self) -> Option<BlobExcessGasAndPrice> {
        self.inner.blob_excess_gas_and_price()
    }

    #[inline]
    fn slot_num(&self) -> u64 {
        self.inner.slot_num()
    }
}

impl BlockEnvironment for ZoneBlockEnv {
    fn inner_mut(&mut self) -> &mut BlockEnv {
        &mut self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    fn prepared_block() -> PreparedZoneBlock {
        PreparedZoneBlock {
            number: 9,
            parent_hash: B256::repeat_byte(0x11),
            timestamp: 123,
            beneficiary: address!("0x0000000000000000000000000000000000001000"),
            protocol_version: 1,
            block_env: env_config(),
        }
    }

    fn env_config() -> ZoneBlockEnvConfig {
        ZoneBlockEnvConfig {
            gas_limit: 30_000_000,
            basefee: 7,
            difficulty: U256::from(8_u64),
            prevrandao: Some(B256::repeat_byte(0x22)),
            blob_excess_gas_and_price: Some(BlobExcessGasAndPrice::new(0, 1)),
            slot_num: 3,
            timestamp_millis_part: 456,
        }
    }

    #[test]
    fn builds_revm_block_env_from_prepared_zone_block_and_explicit_config() {
        let block = prepared_block();
        let config = env_config();
        let env = ZoneBlockEnv::from_prepared_block(&block, config);

        assert_eq!(env.number(), U256::from(block.number));
        assert_eq!(env.beneficiary(), block.beneficiary);
        assert_eq!(env.timestamp(), U256::from(block.timestamp));
        assert_eq!(env.gas_limit(), config.gas_limit);
        assert_eq!(env.basefee(), config.basefee);
        assert_eq!(env.difficulty(), config.difficulty);
        assert_eq!(env.prevrandao(), config.prevrandao);
        assert_eq!(
            env.blob_excess_gas_and_price(),
            config.blob_excess_gas_and_price
        );
        assert_eq!(env.slot_num(), config.slot_num);
        assert_eq!(env.timestamp_millis_part, config.timestamp_millis_part);
    }

    #[test]
    fn timestamp_millis_matches_tempo_saturating_semantics() {
        let block = prepared_block();
        let mut config = env_config();
        config.timestamp_millis_part = 999;

        let env = ZoneBlockEnv::from_prepared_block(&block, config);

        assert_eq!(env.timestamp_millis(), U256::from(123_999_u64));
    }

    #[test]
    fn timestamp_millis_saturates_instead_of_overflowing() {
        let env = ZoneBlockEnv {
            inner: BlockEnv {
                timestamp: U256::MAX,
                ..BlockEnv::default()
            },
            timestamp_millis_part: u64::MAX,
        };

        assert_eq!(env.timestamp_millis(), U256::MAX);
    }

    #[test]
    fn block_environment_exposes_mutable_inner_block_env() {
        let mut env = ZoneBlockEnv::from_prepared_block(&prepared_block(), env_config());

        env.inner_mut().gas_limit = 1;

        assert_eq!(env.gas_limit(), 1);
    }

    #[test]
    fn zone_evm_env_pairs_tempo_cfg_with_zone_block_env() {
        let cfg = ZoneCfgEnv::new_with_spec(TempoHardfork::T1);
        let block_env = ZoneBlockEnv::from_prepared_block(&prepared_block(), env_config());

        let env = ZoneEvmEnv::new(cfg, block_env.clone());

        assert_eq!(env.cfg_env.spec, TempoHardfork::T1);
        assert_eq!(env.block_env, block_env);
    }
}
