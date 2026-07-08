//! Zone protocol fee manager.
//!
//! Zones do not use Tempo's FeeAMM path. Fees are collected in the transaction's
//! fee token and paid directly to the block beneficiary.

use alloy_primitives::{Address, U256};
use revm::Database;
use tempo_precompiles::{
    TIP_FEE_MANAGER_ADDRESS,
    error::Result as TempoResult,
    tip20::{ITIP20, TIP20Token},
};
use tempo_revm::ProtocolFeeManager;

/// Protocol fee manager for zone execution.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ZoneFeeManager;

impl ZoneFeeManager {
    /// Creates the zone protocol fee manager.
    pub(crate) const fn new() -> Self {
        Self
    }

    fn collect_fee_pre_tx_inner(
        &self,
        fee_payer: Address,
        user_token: Address,
        max_amount: U256,
    ) -> TempoResult<Address> {
        let mut token = TIP20Token::from_address(user_token)?;
        token.ensure_transfer_authorized(fee_payer, TIP_FEE_MANAGER_ADDRESS)?;
        token.transfer_fee_pre_tx(fee_payer, max_amount)?;
        Ok(user_token)
    }

    fn collect_fee_post_tx_inner(
        &self,
        fee_payer: Address,
        actual_spending: U256,
        refund_amount: U256,
        fee_token: Address,
        beneficiary: Address,
    ) -> TempoResult<U256> {
        let mut token = TIP20Token::from_address(fee_token)?;
        token.transfer_fee_post_tx(fee_payer, refund_amount, actual_spending)?;

        if !actual_spending.is_zero() {
            token.transfer(
                TIP_FEE_MANAGER_ADDRESS,
                ITIP20::transferCall {
                    to: beneficiary,
                    amount: actual_spending,
                },
            )?;
        }

        Ok(actual_spending)
    }
}

impl<DB: Database> ProtocolFeeManager<DB> for ZoneFeeManager {
    fn collect_fee_pre_tx(
        &self,
        fee_payer: Address,
        user_token: Address,
        max_amount: U256,
        _beneficiary: Address,
        _skip_liquidity_check: bool,
    ) -> TempoResult<Address> {
        self.collect_fee_pre_tx_inner(fee_payer, user_token, max_amount)
    }

    fn collect_fee_post_tx(
        &self,
        fee_payer: Address,
        actual_spending: U256,
        refund_amount: U256,
        fee_token: Address,
        beneficiary: Address,
    ) -> TempoResult<U256> {
        self.collect_fee_post_tx_inner(
            fee_payer,
            actual_spending,
            refund_amount,
            fee_token,
            beneficiary,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempo_precompiles::{
        storage::{ContractStorage, Handler, StorageCtx, hashmap::HashMapStorageProvider},
        test_util::TIP20Setup,
        tip_fee_manager::{TipFeeManager, amm::PoolKey},
    };

    #[test]
    fn fees_are_paid_directly_to_beneficiary() -> eyre::Result<()> {
        let mut storage = HashMapStorageProvider::new(1);
        let admin = Address::random();
        let user = Address::random();
        let beneficiary = Address::random();

        StorageCtx::enter(&mut storage, || {
            let token = TIP20Setup::create("Zone USD", "zUSD", admin)
                .with_issuer(admin)
                .with_mint(user, U256::from(10_000u64))
                .with_approval(user, TIP_FEE_MANAGER_ADDRESS, U256::MAX)
                .apply()?;

            let manager = ZoneFeeManager::new();
            manager.collect_fee_pre_tx_inner(user, token.address(), U256::from(5_000u64))?;
            let credited = manager.collect_fee_post_tx_inner(
                user,
                U256::from(3_000u64),
                U256::from(2_000u64),
                token.address(),
                beneficiary,
            )?;

            assert_eq!(credited, U256::from(3_000u64));
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall { account: user })?,
                U256::from(7_000u64)
            );
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall {
                    account: beneficiary,
                })?,
                U256::from(3_000u64)
            );
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall {
                    account: TIP_FEE_MANAGER_ADDRESS,
                })?,
                U256::ZERO
            );
            assert_eq!(
                TipFeeManager::new().collected_fees[beneficiary][token.address()].read()?,
                U256::ZERO
            );

            Ok(())
        })
    }

    #[test]
    fn fee_amm_pools_are_not_touched() -> eyre::Result<()> {
        let mut storage = HashMapStorageProvider::new(1);
        let admin = Address::random();
        let user = Address::random();
        let beneficiary = Address::random();

        StorageCtx::enter(&mut storage, || {
            let token = TIP20Setup::create("Zone USD", "zUSD", admin)
                .with_issuer(admin)
                .with_mint(user, U256::from(10_000u64))
                .with_approval(user, TIP_FEE_MANAGER_ADDRESS, U256::MAX)
                .apply()?;
            let other = TIP20Setup::create("Other USD", "oUSD", admin).apply()?;

            ZoneFeeManager::new().collect_fee_pre_tx_inner(
                user,
                token.address(),
                U256::from(1_000u64),
            )?;
            ZoneFeeManager::new().collect_fee_post_tx_inner(
                user,
                U256::from(1_000u64),
                U256::ZERO,
                token.address(),
                beneficiary,
            )?;

            let pool = TipFeeManager::new().pools
                [PoolKey::new(token.address(), other.address()).get_id()]
            .read()?;
            assert_eq!(pool.reserve_user_token, 0);
            assert_eq!(pool.reserve_validator_token, 0);

            Ok(())
        })
    }
}
