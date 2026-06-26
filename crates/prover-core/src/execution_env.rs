use alloy_evm::{EvmEnv, env::BlockEnvironment, eth::EthBlockExecutionCtx};
use alloy_primitives::{Address, B256, Bytes, U256};
use revm::{
    context::{Block, BlockEnv},
    context_interface::cfg::{GasId, GasParams},
    primitives::OnceLock,
};
use tempo_chainspec::constants::gas::{SSTORE_CREATE_COST, SSTORE_SET_COST};
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

/// Complete block execution context shape expected by an alloy/reth-style
/// block executor, plus Tempo gas-bucket limits.
#[derive(Debug, Clone)]
pub struct ZoneBlockExecutionContext {
    pub inner: EthBlockExecutionCtx<'static>,
    pub general_gas_limit: u64,
    pub shared_gas_limit: u64,
}

/// Explicit non-environment block context inputs for Zone execution.
///
/// Gas bucket limits are not witness-controlled; they are derived from the
/// block gas limit and Tempo hardfork in [`ZoneCfgEnvConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ZoneBlockExecutionContextConfig {
    pub parent_beacon_block_root: Option<B256>,
    pub extra_data: Bytes,
}

impl ZoneBlockExecutionContextConfig {
    pub fn execution_context(
        &self,
        block: &PreparedZoneBlock,
        transaction_count: usize,
    ) -> ZoneBlockExecutionContext {
        let shared_gas_limit = zone_shared_gas_limit(block.cfg_env.spec, block.block_env.gas_limit);
        let general_gas_limit = zone_general_gas_limit(
            block.cfg_env.spec,
            block.block_env.gas_limit,
            shared_gas_limit,
        );

        ZoneBlockExecutionContext {
            inner: EthBlockExecutionCtx {
                parent_hash: block.parent_hash,
                parent_beacon_block_root: self.parent_beacon_block_root,
                ommers: &[],
                withdrawals: None,
                extra_data: self.extra_data.clone(),
                tx_count_hint: Some(transaction_count),
                slot_number: Some(block.block_env.slot_num),
            },
            general_gas_limit,
            shared_gas_limit,
        }
    }
}

/// Explicit EVM cfg inputs for Zone execution.
///
/// These values affect transaction validity and gas accounting, so they must be
/// proved or otherwise derived by the witness producer instead of defaulted by
/// prover-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ZoneCfgEnvConfig {
    pub chain_id: u64,
    pub spec: TempoHardfork,
    pub enable_amsterdam_eip8037: bool,
}

impl ZoneCfgEnvConfig {
    pub fn cfg_env(self) -> ZoneCfgEnv {
        let mut cfg = ZoneCfgEnv::new_with_spec_and_gas_params(
            self.spec,
            tempo_gas_params_with_amsterdam(self.spec, self.enable_amsterdam_eip8037),
        );
        cfg.chain_id = self.chain_id;
        cfg.tx_gas_limit_cap = self.spec.tx_gas_limit_cap();
        cfg.enable_amsterdam_eip8037 = self.enable_amsterdam_eip8037;
        cfg
    }
}

pub const fn zone_shared_gas_limit(spec: TempoHardfork, block_gas_limit: u64) -> u64 {
    spec.shared_gas_limit(block_gas_limit)
}

pub const fn zone_general_gas_limit(
    spec: TempoHardfork,
    block_gas_limit: u64,
    shared_gas_limit: u64,
) -> u64 {
    match spec.general_gas_limit() {
        Some(limit) => limit,
        None => block_gas_limit.saturating_sub(shared_gas_limit) / 2,
    }
}

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

// Forked from tempo-revm::gas_params so prover-core can stay no_std while
// using Tempo's gas schedule instead of generic Ethereum defaults.
const CONTRACT_CREATE_COST: u64 = 500_000;
const NEW_ACCOUNT_COST: u64 = 250_000;
const CODE_DEPOSIT_COST_T1: u64 = 1_000;
const EIP7702_PER_EMPTY_ACCOUNT_COST_T1: u64 = 12_500;

const T4_SSTORE_SET_REGULAR: u64 = 20_000;
const T4_NEW_ACCOUNT_REGULAR: u64 = 25_000;
const T4_CREATE_REGULAR: u64 = 32_000;
const T4_CODE_DEPOSIT_REGULAR: u64 = 200;

const T4_SSTORE_SET_STATE: u64 = SSTORE_CREATE_COST - T4_SSTORE_SET_REGULAR;
const T4_NEW_ACCOUNT_STATE: u64 = NEW_ACCOUNT_COST - T4_NEW_ACCOUNT_REGULAR;
const T4_CREATE_STATE: u64 = CONTRACT_CREATE_COST - T4_CREATE_REGULAR;
const T4_CODE_DEPOSIT_STATE: u64 = 2_300;
const T4_SSTORE_SET_REFUND: u64 = T4_SSTORE_SET_STATE + 17_800;

#[inline]
pub fn tempo_gas_params_with_amsterdam(
    spec: TempoHardfork,
    amsterdam_eip8037_enabled: bool,
) -> GasParams {
    debug_assert!(
        !(spec.is_t7() && amsterdam_eip8037_enabled),
        "TODO(TIP-1016): generate combined TIP-1060 + EIP-8037 gas params before enabling both"
    );

    if amsterdam_eip8037_enabled {
        static TABLE: OnceLock<GasParams> = OnceLock::new();
        return TABLE.get_or_init(amsterdam_gas_params).clone();
    }

    if spec.is_t7() {
        static TABLE: OnceLock<GasParams> = OnceLock::new();
        return TABLE.get_or_init(t7_gas_params).clone();
    }

    if spec.is_t1() {
        static TABLE: OnceLock<GasParams> = OnceLock::new();
        return TABLE.get_or_init(t1_gas_params).clone();
    }

    GasParams::new_spec(spec.into())
}

#[inline]
pub fn tempo_gas_params(spec: TempoHardfork) -> GasParams {
    tempo_gas_params_with_amsterdam(spec, false)
}

fn t7_gas_params() -> GasParams {
    let mut gas_params = t1_gas_params();
    gas_params.override_gas([
        (GasId::sstore_set_without_load_cost(), SSTORE_SET_COST),
        (GasId::sstore_set_refund(), SSTORE_SET_COST),
        (GasId::sstore_clearing_slot_refund(), 0),
    ]);
    gas_params
}

fn amsterdam_gas_params() -> GasParams {
    let mut gas_params = GasParams::new_spec(TempoHardfork::T4.into());
    gas_params.override_gas([
        (GasId::sstore_set_without_load_cost(), T4_SSTORE_SET_REGULAR),
        (GasId::sstore_set_state_gas(), T4_SSTORE_SET_STATE),
        (GasId::sstore_set_refund(), T4_SSTORE_SET_REFUND),
        (GasId::tx_create_cost(), T4_CREATE_REGULAR),
        (GasId::create(), T4_CREATE_REGULAR),
        (GasId::create_state_gas(), T4_CREATE_STATE),
        (GasId::new_account_cost(), T4_NEW_ACCOUNT_REGULAR),
        (GasId::new_account_state_gas(), T4_NEW_ACCOUNT_STATE),
        (
            GasId::new_account_cost_for_selfdestruct(),
            T4_NEW_ACCOUNT_REGULAR,
        ),
        (GasId::code_deposit_cost(), T4_CODE_DEPOSIT_REGULAR),
        (GasId::code_deposit_state_gas(), T4_CODE_DEPOSIT_STATE),
        (
            GasId::tx_eip7702_per_empty_account_cost(),
            T4_NEW_ACCOUNT_REGULAR,
        ),
        (GasId::tx_eip7702_auth_refund(), 0),
        (GasId::tx_eip7702_state_gas_bytecode(), 0),
    ]);
    gas_params
}

fn t1_gas_params() -> GasParams {
    let mut gas_params = GasParams::new_spec(TempoHardfork::T1.into());
    gas_params.override_gas([
        (GasId::sstore_set_without_load_cost(), SSTORE_CREATE_COST),
        (GasId::tx_create_cost(), CONTRACT_CREATE_COST),
        (GasId::create(), CONTRACT_CREATE_COST),
        (GasId::new_account_cost(), NEW_ACCOUNT_COST),
        (GasId::new_account_cost_for_selfdestruct(), NEW_ACCOUNT_COST),
        (GasId::code_deposit_cost(), CODE_DEPOSIT_COST_T1),
        (
            GasId::tx_eip7702_per_empty_account_cost(),
            EIP7702_PER_EMPTY_ACCOUNT_COST_T1,
        ),
        (GasId::tx_eip7702_auth_refund(), 0),
    ]);
    gas_params
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    fn cfg_config() -> ZoneCfgEnvConfig {
        ZoneCfgEnvConfig {
            chain_id: 421_700_001,
            spec: TempoHardfork::T1,
            enable_amsterdam_eip8037: false,
        }
    }

    fn execution_context_config() -> ZoneBlockExecutionContextConfig {
        ZoneBlockExecutionContextConfig {
            parent_beacon_block_root: Some(B256::repeat_byte(0x33)),
            extra_data: Bytes::from_static(b"zone"),
        }
    }

    fn prepared_block() -> PreparedZoneBlock {
        PreparedZoneBlock {
            number: 9,
            parent_hash: B256::repeat_byte(0x11),
            timestamp: 123,
            beneficiary: address!("0x0000000000000000000000000000000000001000"),
            protocol_version: 1,
            cfg_env: cfg_config(),
            execution_context: execution_context_config(),
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
        let cfg = cfg_config().cfg_env();
        let block_env = ZoneBlockEnv::from_prepared_block(&prepared_block(), env_config());

        let env = ZoneEvmEnv::new(cfg, block_env.clone());

        assert_eq!(env.cfg_env.spec, TempoHardfork::T1);
        assert_eq!(env.cfg_env.chain_id, 421_700_001);
        assert_eq!(
            env.cfg_env.tx_gas_limit_cap,
            TempoHardfork::T1.tx_gas_limit_cap()
        );
        assert_eq!(env.block_env, block_env);
    }

    #[test]
    fn block_execution_context_matches_alloy_executor_shape() {
        let block = prepared_block();
        let context = execution_context_config().execution_context(&block, 3);

        assert_eq!(context.inner.parent_hash, block.parent_hash);
        assert_eq!(
            context.inner.parent_beacon_block_root,
            execution_context_config().parent_beacon_block_root
        );
        assert_eq!(context.inner.ommers.len(), 0);
        assert!(context.inner.withdrawals.is_none());
        assert_eq!(context.inner.extra_data, Bytes::from_static(b"zone"));
        assert_eq!(context.inner.tx_count_hint, Some(3));
        assert_eq!(context.inner.slot_number, Some(block.block_env.slot_num));
        assert_eq!(context.general_gas_limit, 30_000_000);
        assert_eq!(context.shared_gas_limit, block.block_env.gas_limit / 10);
    }

    #[test]
    fn derives_tempo_gas_buckets_from_spec_and_block_gas_limit() {
        assert_eq!(zone_shared_gas_limit(TempoHardfork::T4, 30_000_000), 0);
        assert_eq!(
            zone_general_gas_limit(TempoHardfork::Genesis, 30_000_000, 3_000_000),
            13_500_000
        );
        assert_eq!(
            zone_general_gas_limit(TempoHardfork::T1, 30_000_000, 3_000_000),
            30_000_000
        );
    }

    #[test]
    fn tempo_cfg_env_uses_explicit_tip_1016_flag() {
        let mut config = cfg_config();
        config.spec = TempoHardfork::T4;
        config.enable_amsterdam_eip8037 = false;
        assert!(!config.cfg_env().enable_amsterdam_eip8037);

        config.enable_amsterdam_eip8037 = true;
        assert!(config.cfg_env().enable_amsterdam_eip8037);
    }

    #[test]
    fn tempo_gas_params_apply_tip_1000_overrides() {
        let gas_params = tempo_gas_params(TempoHardfork::T1);

        assert_eq!(
            gas_params.get(GasId::sstore_set_without_load_cost()),
            SSTORE_CREATE_COST
        );
        assert_eq!(
            gas_params.get(GasId::tx_create_cost()),
            CONTRACT_CREATE_COST
        );
    }
}
