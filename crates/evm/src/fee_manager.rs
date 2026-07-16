//! L1-aware implementation of Tempo's internal protocol fee hooks.

use alloy_evm::Database;
use alloy_primitives::{Address, U256};
use core::fmt;
use reth_evm::EvmInternals;
use tempo_evm::{ProtocolFeeContext, ProtocolFeeManager};
use tempo_precompiles::{
    error::Result,
    storage::{PrecompileStorageProvider, StorageCtx, evm::EvmPrecompileStorageProvider},
    tip_fee_manager::TipFeeManager,
};
use zone_l1::state::L1StateProvider;
use zone_precompiles::{L1StorageReader, storage::ZonePrecompileStorageProvider};

/// Tempo protocol fee hooks executed against the Zone's finalized L1 policy overlay.
#[derive(Clone)]
pub(crate) struct ZoneFeeManager<L1 = L1StateProvider> {
    l1_reader: L1,
}

impl<L1> fmt::Debug for ZoneFeeManager<L1> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZoneFeeManager").finish_non_exhaustive()
    }
}

impl<L1: L1StorageReader> ZoneFeeManager<L1> {
    pub(crate) const fn new(l1_reader: L1) -> Self {
        Self { l1_reader }
    }

    fn enter<DB: Database, R>(
        &self,
        ctx: ProtocolFeeContext<'_, DB>,
        f: impl FnOnce() -> Result<R>,
    ) -> Result<R> {
        let ProtocolFeeContext {
            journal,
            block_env,
            cfg,
            tx_env,
            actions,
        } = ctx;
        let internals = EvmInternals::new(journal, block_env, cfg, tx_env);
        let mut inner =
            EvmPrecompileStorageProvider::new_max_gas(internals, cfg).with_actions(actions);
        inner.set_tip1060_storage_credits(false);

        let mut storage = ZonePrecompileStorageProvider::try_new(inner, self.l1_reader.clone())
            .map_err(|error| error.into_error())?;
        StorageCtx::enter(&mut storage, f)
    }
}

impl<DB: Database, L1: L1StorageReader> ProtocolFeeManager<DB> for ZoneFeeManager<L1> {
    fn collect_fee_pre_tx(
        &self,
        ctx: ProtocolFeeContext<'_, DB>,
        fee_payer: Address,
        user_token: Address,
        max_amount: U256,
        beneficiary: Address,
        skip_liquidity_check: bool,
    ) -> Result<Address> {
        self.enter(ctx, || {
            TipFeeManager::new().collect_fee_pre_tx(
                fee_payer,
                user_token,
                max_amount,
                beneficiary,
                skip_liquidity_check,
            )
        })
    }

    fn collect_fee_post_tx(
        &self,
        ctx: ProtocolFeeContext<'_, DB>,
        fee_payer: Address,
        actual_spending: U256,
        refund_amount: U256,
        fee_token: Address,
        beneficiary: Address,
    ) -> Result<U256> {
        self.enter(ctx, || {
            TipFeeManager::new().collect_fee_post_tx(
                fee_payer,
                actual_spending,
                refund_amount,
                fee_token,
                beneficiary,
            )
        })
    }
}
