//! Zone protocol fee manager.
//!
//! The fee manager collects fees directly in the transaction's fee token and
//! pays them out to the block beneficiary.

use alloy_primitives::{Address, U256};
use revm::Database;
use tempo_precompiles::{TIP_FEE_MANAGER_ADDRESS, error::Result as TempoResult, tip20::TIP20Token};
use tempo_revm::ProtocolFeeManager;

/// Protocol fee manager for zone execution.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ZoneFeeManager;

impl ZoneFeeManager {
    /// Creates the zone protocol fee manager.
    pub(crate) const fn new() -> Self {
        Self
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
        let mut token = TIP20Token::from_address(user_token)?;
        token.ensure_transfer_authorized(fee_payer, TIP_FEE_MANAGER_ADDRESS)?;
        token.transfer_fee_pre_tx(fee_payer, max_amount)?;
        Ok(user_token)
    }

    fn collect_fee_post_tx(
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
            let _ = beneficiary;
            todo!("expose a pause-tolerant TIP20 fee payout helper")
        }

        Ok(actual_spending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use revm::database_interface::EmptyDB;
    use tempo_precompiles::{
        storage::{ContractStorage, Handler, StorageCtx, hashmap::HashMapStorageProvider},
        test_util::TIP20Setup,
        tip_fee_manager::{TipFeeManager, amm::PoolKey},
        tip20::{ITIP20, PAUSE_ROLE},
    };

    fn collect_fee_pre_tx(
        manager: &ZoneFeeManager,
        fee_payer: Address,
        user_token: Address,
        max_amount: U256,
        beneficiary: Address,
    ) -> TempoResult<Address> {
        <ZoneFeeManager as ProtocolFeeManager<EmptyDB>>::collect_fee_pre_tx(
            manager,
            fee_payer,
            user_token,
            max_amount,
            beneficiary,
            false,
        )
    }

    fn collect_fee_post_tx(
        manager: &ZoneFeeManager,
        fee_payer: Address,
        actual_spending: U256,
        refund_amount: U256,
        fee_token: Address,
        beneficiary: Address,
    ) -> TempoResult<U256> {
        <ZoneFeeManager as ProtocolFeeManager<EmptyDB>>::collect_fee_post_tx(
            manager,
            fee_payer,
            actual_spending,
            refund_amount,
            fee_token,
            beneficiary,
        )
    }

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
            collect_fee_pre_tx(
                &manager,
                user,
                token.address(),
                U256::from(5_000u64),
                beneficiary,
            )?;
            let credited = collect_fee_post_tx(
                &manager,
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

            let manager = ZoneFeeManager::new();
            collect_fee_pre_tx(
                &manager,
                user,
                token.address(),
                U256::from(1_000u64),
                beneficiary,
            )?;
            collect_fee_post_tx(
                &manager,
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

    #[test]
    fn post_tx_fee_payout_succeeds_when_token_is_paused() -> eyre::Result<()> {
        let mut storage = HashMapStorageProvider::new(1);
        let admin = Address::random();
        let user = Address::random();
        let beneficiary = Address::random();

        StorageCtx::enter(&mut storage, || {
            let mut token = TIP20Setup::create("Zone USD", "zUSD", admin)
                .with_issuer(admin)
                .with_role(admin, *PAUSE_ROLE)
                .with_mint(user, U256::from(10_000u64))
                .with_approval(user, TIP_FEE_MANAGER_ADDRESS, U256::MAX)
                .apply()?;

            let manager = ZoneFeeManager::new();
            collect_fee_pre_tx(
                &manager,
                user,
                token.address(),
                U256::from(5_000u64),
                beneficiary,
            )?;

            token.pause(admin, ITIP20::pauseCall {})?;
            assert!(token.paused()?);

            let credited = collect_fee_post_tx(
                &manager,
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

            Ok(())
        })
    }
}
