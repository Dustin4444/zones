//! Zone-native fee custody without token preferences or FeeAMM routing.

use alloy_primitives::{Address, U256};
use revm::precompile::PrecompileResult;
use tempo_precompiles::{
    TIP_FEE_MANAGER_ADDRESS, charge_input_cost, dispatch,
    error::Result,
    mutate_void,
    storage::{Handler, Mapping, StorageCtx},
    tip20::{ITIP20, TIP20Token},
    tip403_registry::AuthRole,
    view,
};
use tempo_precompiles_macros::contract;
use tempo_zone_contracts::IZoneFeeManager;

/// Zone-owned fee balances at Tempo's canonical fee-manager address.
#[contract(addr = TIP_FEE_MANAGER_ADDRESS)]
pub struct ZoneFeeManager {
    collected_fees: Mapping<Address, Mapping<Address, U256>>,
}

impl ZoneFeeManager {
    /// Initializes the precompile account marker in genesis.
    pub fn initialize(&mut self) -> Result<()> {
        self.__initialize()
    }

    /// Returns fees accrued to `beneficiary` in `token`.
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
        if self.storage.spec().is_t8() {
            token.ensure_authorized_as(&[(fee_payer, AuthRole::sender())])?;
        } else {
            token.ensure_transfer_authorized(fee_payer, self.address)?;
        }
        token.transfer_fee_pre_tx(fee_payer, max_amount)?;
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
        TIP20Token::from_address(fee_token)?.transfer_fee_post_tx(
            fee_payer,
            refund_amount,
            actual_spending,
        )?;
        if !actual_spending.is_zero() {
            self.collected_fees[beneficiary][fee_token].sinc(actual_spending)?;
        }
        Ok(actual_spending)
    }

    /// Pays a beneficiary's accrued balance. Anyone may trigger the payout.
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

    pub(crate) fn call(&mut self, calldata: &[u8], sender: Address) -> PrecompileResult {
        if let Some(error) = charge_input_cost(&mut self.storage, calldata) {
            return error;
        }
        dispatch!(calldata, |call| match call {
            IZoneFeeManager::IZoneFeeManagerCalls {
                collectedFees(call) => view(call, |call| {
                    self.collected_fees(call.beneficiary, call.token)
                }),
                distributeFees(call) => mutate_void(call, sender, |_, call| {
                    self.distribute_fees(call.beneficiary, call.token)
                }),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempo_precompiles::{
        storage::{ContractStorage, StorageCtx, hashmap::HashMapStorageProvider},
        test_util::TIP20Setup,
        tip20::ITIP20,
    };

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
            manager.collect_fee_pre_tx(user, token.address(), U256::from(5_000))?;
            manager.collect_fee_post_tx(
                user,
                U256::from(3_000),
                U256::from(2_000),
                token.address(),
                beneficiary,
            )?;
            manager.distribute_fees(beneficiary, token.address())?;
            assert_eq!(
                token.balance_of(ITIP20::balanceOfCall {
                    account: beneficiary
                })?,
                U256::from(3_000)
            );
            Ok(())
        })
    }
}
