//! Zone-native fee settlement without token preferences or FeeAMM routing.

mod dispatch;

use alloy_primitives::{Address, IntoLogData, U256};
use tempo_precompiles::{
    account_keychain::AccountKeychain,
    error::Result,
    storage::{Handler, StorageCtx},
    tip20::{Recipient, TIP20Event, TIP20Token},
    tip403_registry::AuthRole,
};
use tempo_precompiles_macros::contract;
pub use zone_primitives::constants::ZONE_FEE_MANAGER_ADDRESS;

/// Zone fee configuration and transaction-scoped fee escrow.
#[contract(addr = ZONE_FEE_MANAGER_ADDRESS)]
pub struct ZoneFeeManager {
    default_fee_token: Address,
}

impl ZoneFeeManager {
    /// Initializes the precompile account marker and canonical default fee token in genesis.
    pub fn initialize(&mut self, default_fee_token: Address) -> Result<()> {
        self.__initialize()?;
        self.default_fee_token.write(default_fee_token)
    }

    /// Returns the canonical default token used when a transaction omits `fee_token`.
    pub fn default_fee_token(&self) -> Result<Address> {
        self.default_fee_token.read()
    }

    /// Escrows the maximum fee directly in the selected token.
    pub fn collect_fee_pre_tx(
        &mut self,
        fee_payer: Address,
        fee_token: Address,
        max_amount: U256,
    ) -> Result<Address> {
        let mut token = TIP20Token::from_address(fee_token)?;
        token.ensure_authorized_as(&[(fee_payer, AuthRole::sender())])?;
        token.check_not_paused()?;
        token.check_and_update_spending_limit(fee_payer, max_amount)?;

        token.decrement_balance(fee_payer, max_amount)?;
        token.increment_balance(self.address, max_amount)?;
        Ok(fee_token)
    }

    /// Refunds unused gas and pays the net fee directly to the block beneficiary.
    pub fn collect_fee_post_tx(
        &mut self,
        fee_payer: Address,
        actual_spending: U256,
        refund_amount: U256,
        fee_token: Address,
        beneficiary: Address,
    ) -> Result<U256> {
        let mut token = TIP20Token::from_address(fee_token)?;
        StorageCtx.set_tip1060_storage_credit_minting(false);

        if !refund_amount.is_zero() {
            AccountKeychain::new().refund_spending_limit(fee_payer, fee_token, refund_amount)?;
            token._transfer(self.address, &Recipient::direct(fee_payer), refund_amount)?;
        }

        if !actual_spending.is_zero() {
            token._transfer(
                self.address,
                &Recipient::direct(beneficiary),
                actual_spending,
            )?;

            StorageCtx.emit_event(
                fee_token,
                TIP20Event::transfer(fee_payer, self.address, actual_spending).into_log_data(),
            )?;
        }

        Ok(actual_spending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_sol_types::SolError;
    use tempo_contracts::precompiles::UnknownFunctionSelector;
    use tempo_precompiles::{
        Precompile as _, TIP_FEE_MANAGER_ADDRESS,
        storage::{ContractStorage, StorageCtx, hashmap::HashMapStorageProvider},
        test_util::TIP20Setup,
        tip20::ITIP20,
    };

    #[test]
    fn settles_fees_directly_to_beneficiary() -> eyre::Result<()> {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, || {
            let (user, beneficiary) = (Address::random(), Address::random());
            let admin = Address::random();
            let token = TIP20Setup::create("USD", "USD", admin)
                .with_issuer(admin)
                .with_mint(user, U256::from(10_000))
                .apply()?;
            let mut manager = ZoneFeeManager::new();
            manager.initialize(token.address())?;
            assert_eq!(manager.default_fee_token()?, token.address());
            manager.collect_fee_pre_tx(user, token.address(), U256::from(5_000))?;
            manager.collect_fee_post_tx(
                user,
                U256::from(3_000),
                U256::from(2_000),
                token.address(),
                beneficiary,
            )?;
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall {
                    account: ZONE_FEE_MANAGER_ADDRESS,
                })?,
                U256::ZERO,
            );
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall {
                    account: TIP_FEE_MANAGER_ADDRESS,
                })?,
                U256::ZERO,
            );
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall {
                    account: beneficiary
                })?,
                U256::from(3_000)
            );
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall { account: user })?,
                U256::from(7_000),
            );
            Ok(())
        })
    }

    #[test]
    fn external_calls_are_unsupported() -> eyre::Result<()> {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, || {
            let mut manager = ZoneFeeManager::new();
            let calldata = [0xde, 0xad, 0xbe, 0xef];
            let output = manager.call(&calldata, Address::random())?;
            assert!(output.is_revert());
            let error = UnknownFunctionSelector::abi_decode(&output.bytes)?;
            assert_eq!(error.selector.as_slice(), calldata);

            Ok(())
        })
    }
}
