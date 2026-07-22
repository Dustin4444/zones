//! Zone-native fee custody without token preferences or FeeAMM routing.

mod dispatch;

use alloy_primitives::{Address, IntoLogData, U256};
use tempo_precompiles::{
    account_keychain::AccountKeychain,
    error::{Result, TempoPrecompileError},
    storage::{Handler, Mapping, StorageCtx},
    tip20::{ITIP20, TIP20Event, TIP20Token},
    tip403_registry::AuthRole,
};
use tempo_precompiles_macros::contract;
use tempo_zone_contracts::IZoneFeeManager;
pub use zone_primitives::constants::ZONE_FEE_MANAGER_ADDRESS;

/// Zone-owned fee balances at the Zone-native fee-manager address.
#[contract(addr = ZONE_FEE_MANAGER_ADDRESS)]
pub struct ZoneFeeManager {
    collected_fees: Mapping<Address, Mapping<Address, U256>>,
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

    /// Returns aggregate fees accrued to `beneficiary` in `token`.
    ///
    /// External dispatch restricts this read to the beneficiary.
    pub fn collected_fees(&self, beneficiary: Address, token: Address) -> Result<U256> {
        self.collected_fees[beneficiary][token].read()
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

        let reward_recipient = token.update_rewards(fee_payer)?;
        if !reward_recipient.is_zero() {
            let opted_in_supply = U256::from(token.get_opted_in_supply()?)
                .checked_sub(max_amount)
                .ok_or_else(TempoPrecompileError::under_overflow)?;
            token.set_opted_in_supply(
                opted_in_supply
                    .try_into()
                    .map_err(|_| TempoPrecompileError::under_overflow())?,
            )?;
        }

        token.decrement_balance(fee_payer, max_amount)?;
        token.increment_balance(self.address, max_amount)?;
        Ok(fee_token)
    }

    /// Refunds unused gas and credits the net fee to the block beneficiary.
    pub fn collect_fee_post_tx(
        &mut self,
        fee_payer: Address,
        actual_spending: U256,
        refund_amount: U256,
        fee_token: Address,
        beneficiary: Address,
    ) -> Result<U256> {
        let mut token = TIP20Token::from_address(fee_token)?;
        StorageCtx.emit_event(
            fee_token,
            TIP20Event::transfer(fee_payer, self.address, actual_spending).into_log_data(),
        )?;

        if !refund_amount.is_zero() {
            AccountKeychain::new().refund_spending_limit(fee_payer, fee_token, refund_amount)?;

            let reward_recipient = token.update_rewards(fee_payer)?;
            if !reward_recipient.is_zero() {
                let opted_in_supply = U256::from(token.get_opted_in_supply()?)
                    .checked_add(refund_amount)
                    .ok_or_else(TempoPrecompileError::under_overflow)?;
                token.set_opted_in_supply(
                    opted_in_supply
                        .try_into()
                        .map_err(|_| TempoPrecompileError::under_overflow())?,
                )?;
            }

            token.decrement_balance(self.address, refund_amount)?;
            token.increment_balance(fee_payer, refund_amount)?;
        }

        if !actual_spending.is_zero() {
            self.collected_fees[beneficiary][fee_token].sinc(actual_spending)?;
        }
        Ok(actual_spending)
    }

    /// Pays a beneficiary's accrued balance.
    ///
    /// External dispatch restricts this action to the beneficiary.
    pub fn distribute_fees(&mut self, beneficiary: Address, token: Address) -> Result<()> {
        StorageCtx.set_tip1060_storage_credit_minting(false);
        let amount = self.collected_fees(beneficiary, token)?;
        if amount.is_zero() {
            return Ok(());
        }
        self.collected_fees[beneficiary][token].write(U256::ZERO)?;
        TIP20Token::from_address(token)?.transfer(
            self.address,
            ITIP20::transferCall {
                to: beneficiary,
                amount,
            },
        )?;
        self.emit_event(IZoneFeeManager::FeesDistributed {
            beneficiary,
            token,
            amount,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Bytes;
    use alloy_sol_types::{SolCall, SolError};
    use tempo_precompiles::{
        Precompile as _, TIP_FEE_MANAGER_ADDRESS,
        storage::{ContractStorage, StorageCtx, hashmap::HashMapStorageProvider},
        test_util::TIP20Setup,
        tip20::ITIP20,
    };
    use tempo_zone_contracts::Unauthorized;

    #[test]
    fn accrues_and_distributes_direct_fees() -> eyre::Result<()> {
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
                U256::from(3_000),
            );
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall {
                    account: TIP_FEE_MANAGER_ADDRESS,
                })?,
                U256::ZERO,
            );
            let output = manager.call(
                &IZoneFeeManager::distributeFeesCall {
                    beneficiary,
                    token: token.address(),
                }
                .abi_encode(),
                beneficiary,
            )?;
            assert!(output.is_success());
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall {
                    account: beneficiary
                })?,
                U256::from(3_000)
            );
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall {
                    account: ZONE_FEE_MANAGER_ADDRESS,
                })?,
                U256::ZERO,
            );
            Ok(())
        })
    }

    #[test]
    fn only_beneficiary_can_read_or_distribute_fees() -> eyre::Result<()> {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, || {
            let beneficiary = Address::random();
            let outsider = Address::random();
            let token = Address::random();
            let amount = U256::from(3_000);
            let mut manager = ZoneFeeManager::new();
            manager.initialize(token)?;
            manager.collected_fees[beneficiary][token].write(amount)?;

            let read = IZoneFeeManager::collectedFeesCall { beneficiary, token };
            let unauthorized_read = manager.call(&read.abi_encode(), outsider)?;
            assert!(unauthorized_read.is_revert());
            assert_eq!(
                unauthorized_read.bytes,
                Bytes::from(Unauthorized {}.abi_encode())
            );

            let authorized_read = manager.call(&read.abi_encode(), beneficiary)?;
            assert!(authorized_read.is_success());
            assert_eq!(
                IZoneFeeManager::collectedFeesCall::abi_decode_returns(&authorized_read.bytes)?,
                amount
            );

            let distribute = IZoneFeeManager::distributeFeesCall { beneficiary, token };
            let unauthorized_distribution = manager.call(&distribute.abi_encode(), outsider)?;
            assert!(unauthorized_distribution.is_revert());
            assert_eq!(
                unauthorized_distribution.bytes,
                Bytes::from(Unauthorized {}.abi_encode())
            );
            assert_eq!(manager.collected_fees(beneficiary, token)?, amount);

            Ok(())
        })
    }
}
