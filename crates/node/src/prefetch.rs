//! Node adapter for canonical Tempo deposit policy reads.
//!
//! The L1 crate owns event planning, dependency waves, and concurrency. This module supplies the
//! node-only boundary: opening parent Zone state and evaluating one policy operation in a throwaway
//! [`ZoneEvmConfig`] environment anchored to the exact child L1 block.

use std::fmt::Debug;

use alloy_consensus::BlockHeader as _;
use alloy_eips::eip4895::Withdrawals;
use alloy_primitives::{Address, B256};
use reth_evm::{ConfigureEvm as _, NextBlockEnvAttributes};
use reth_provider::StateProviderFactory;
use reth_revm::database::StateProviderDatabase;
use tempo_evm::TempoNextBlockEnvAttributes;
use tempo_precompiles::{
    storage::{StorageActions, StorageCtx},
    tip20::TIP20Token,
    tip403_registry::{AuthRole, TIP403Registry},
};
use tempo_zone_contracts::ZONE_INBOX_ADDRESS;
use zone_evm::ZoneEvmConfig;
use zone_l1::state::{PolicyCheckExecutor, PrefetchCtx};

/// Executes canonical Tempo L1 policy reads over independent throwaway Zone EVMs.
///
/// The executor is intentionally stateless per block. [`PrefetchCtx`] is supplied with each call so
/// one shared instance can safely serve concurrent checks without mutable anchor or block state.
#[derive(Debug, Clone)]
pub struct L1PolicyExecutor<P> {
    pub provider: P,
    pub evm_config: ZoneEvmConfig,
}

impl<P> L1PolicyExecutor<P>
where
    P: StateProviderFactory + Clone + Send + Sync + 'static,
{
    /// Run one canonical policy operation with storage writes disabled.
    ///
    /// A fresh EVM prevents journal/checkpoint state from leaking between concurrent checks. The
    /// L1 overlay still shares [`ZoneEvmConfig`]'s reader, so RPC fallbacks warm the cache later used
    /// by payload execution.
    fn with_storage<T, E>(
        &self,
        ctx: &PrefetchCtx,
        check: impl FnOnce() -> Result<T, E>,
    ) -> eyre::Result<T>
    where
        E: std::fmt::Display,
    {
        let parent = &ctx.parent;
        let state = self.provider.state_by_block_hash(parent.hash())?;
        let db = StateProviderDatabase::new(state.as_ref());
        let attributes = TempoNextBlockEnvAttributes {
            inner: NextBlockEnvAttributes {
                timestamp: ctx.timestamp,
                suggested_fee_recipient: Address::ZERO,
                prev_randao: B256::ZERO,
                gas_limit: parent.gas_limit(),
                parent_beacon_block_root: None,
                withdrawals: Some(Withdrawals::default()),
                extra_data: Default::default(),
                slot_number: None,
            },
            general_gas_limit: 0,
            shared_gas_limit: parent.gas_limit(),
            timestamp_millis_part: ctx.timestamp_millis_part,
            consensus_context: None,
            subblock_fee_recipients: Default::default(),
        };
        let env = self.evm_config.next_evm_env(parent, &attributes)?;
        let mut evm = self
            .evm_config
            .evm_factory()
            .new_prefetch_evm(db, env, ctx.target_l1_block);

        StorageCtx::enter_ctx(evm.ctx_mut(), StorageActions::disabled(), || {
            let mut storage = StorageCtx;
            let _checkpoint = storage.checkpoint();
            check()
        })
        .map_err(|error| eyre::eyre!(error.to_string()))
    }
}

impl<P> PolicyCheckExecutor for L1PolicyExecutor<P>
where
    P: StateProviderFactory + Clone + Debug + Send + Sync + 'static,
{
    fn transfer_policy(&self, ctx: &PrefetchCtx, token: Address) -> eyre::Result<u64> {
        self.with_storage(ctx, || {
            TIP20Token::from_address(token).and_then(|token| token.transfer_policy_id())
        })
    }

    fn is_mint_authorized(
        &self,
        ctx: &PrefetchCtx,
        policy_id: u64,
        recipient: Address,
    ) -> eyre::Result<bool> {
        self.with_storage(ctx, || {
            TIP403Registry::new().is_authorized_as(policy_id, recipient, AuthRole::mint_recipient())
        })
    }

    fn validate_receive_policy(
        &self,
        ctx: &PrefetchCtx,
        token: Address,
        recipient: Address,
    ) -> eyre::Result<()> {
        self.with_storage(ctx, || {
            TIP403Registry::new()
                .validate_receive_policy(token, ZONE_INBOX_ADDRESS, recipient)
                .map(|_| ())
        })
    }
}
