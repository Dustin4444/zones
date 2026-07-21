//! Adapter between Tempo's protocol fee hooks and the Zone fee manager.

use alloy_evm::{Database, revm::context::Journal};
use alloy_primitives::{Address, U256};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_evm::{ProtocolFeeContext, ProtocolFeeManager};
use tempo_precompiles::{error::Result, storage::StorageActions, tip_fee_manager::FeeManagerError};
use tempo_revm::{TempoTx, TempoTxEnv};
use zone_precompiles::{L1StorageReader, ZoneFeeManager};

/// Resolves and collects fees without Tempo token preferences or FeeAMM settlement.
#[derive(Clone)]
pub(crate) struct ZoneProtocolFeeManager<L1> {
    l1_reader: L1,
}

impl<L1> core::fmt::Debug for ZoneProtocolFeeManager<L1> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ZoneProtocolFeeManager")
            .finish_non_exhaustive()
    }
}

impl<L1: L1StorageReader> ZoneProtocolFeeManager<L1> {
    pub(crate) const fn new(l1_reader: L1) -> Self {
        Self { l1_reader }
    }

    fn resolve_fee_token(&self, tx: &TempoTxEnv) -> Result<Address> {
        let Some(token) = tx
            .fee_token()
            .or_else(|| self.l1_reader.default_fee_token())
        else {
            return Err(FeeManagerError::invalid_token().into());
        };
        if !self.l1_reader.is_fee_token_enabled(token) {
            return Err(FeeManagerError::invalid_token().into());
        }
        Ok(token)
    }
}

impl<DB, L1> ProtocolFeeManager<DB> for ZoneProtocolFeeManager<L1>
where
    DB: Database,
    L1: L1StorageReader,
{
    fn get_fee_token(
        &self,
        _journal: &mut Journal<DB>,
        tx: &TempoTxEnv,
        _fee_payer: Address,
        _spec: TempoHardfork,
        _actions: StorageActions,
    ) -> Result<Address> {
        self.resolve_fee_token(tx)
    }

    fn collect_fee_pre_tx(
        &self,
        ctx: ProtocolFeeContext<'_, DB>,
        fee_payer: Address,
        fee_token: Address,
        max_amount: U256,
        _beneficiary: Address,
        _skip_liquidity_check: bool,
    ) -> Result<Address> {
        if !self.l1_reader.is_fee_token_enabled(fee_token) {
            return Err(FeeManagerError::invalid_token().into());
        }
        ctx.enter(|| ZoneFeeManager::new().collect_fee_pre_tx(fee_payer, fee_token, max_amount))
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
        ctx.enter(|| {
            ZoneFeeManager::new().collect_fee_post_tx(
                fee_payer,
                actual_spending,
                refund_amount,
                fee_token,
                beneficiary,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zone_precompiles::test_utils::MockL1Reader;

    #[test]
    fn resolves_only_cached_enabled_tokens() {
        let default = Address::repeat_byte(0x11);
        let explicit = Address::repeat_byte(0x22);
        let manager =
            ZoneProtocolFeeManager::new(MockL1Reader::with_enabled_tokens([default, explicit]));

        assert_eq!(
            manager.resolve_fee_token(&TempoTxEnv::default()).unwrap(),
            default
        );
        assert_eq!(
            manager
                .resolve_fee_token(&TempoTxEnv {
                    fee_token: Some(explicit),
                    ..Default::default()
                })
                .unwrap(),
            explicit
        );
        assert!(
            manager
                .resolve_fee_token(&TempoTxEnv {
                    fee_token: Some(Address::repeat_byte(0x33)),
                    ..Default::default()
                })
                .is_err()
        );
    }
}
