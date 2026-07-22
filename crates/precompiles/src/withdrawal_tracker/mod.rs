//! Native Zone-balance ledger for withdrawals.
//!
//! This precompile records non-transferable Zone balances independently from TIP-20 storage. Successful
//! ZoneInbox mints credit a `(user, token)` balance, and user-initiated ZoneOutbox withdrawals
//! debit `amount + fee`. Because the state is part of ordinary EVM execution, reverts and reorgs
//! roll the ledger back with the transaction or block that changed it.

mod dispatch;

use alloy_evm::precompiles::DynPrecompile;
use alloy_primitives::{Address, U256};
use tempo_precompiles::{
    Precompile as _,
    error::TempoPrecompileError,
    storage::{Handler, Mapping},
};
use tempo_precompiles_macros::contract;
use tempo_zone_contracts::WithdrawalTrackerError;
use zone_primitives::constants::WITHDRAWAL_TRACKER_ADDRESS;

use crate::ZoneResult;

/// Zone-side withdrawal-balance ledger.
#[contract(addr = WITHDRAWAL_TRACKER_ADDRESS)]
pub struct WithdrawalTracker {
    zone_balance: Mapping<Address, Mapping<Address, U256>>,
    zone_total_supply: Mapping<Address, U256>,
}

impl WithdrawalTracker {
    /// Creates the direct-call-only tracker over ordinary Zone storage.
    pub fn create(env: &crate::ZonePrecompileEnv) -> DynPrecompile {
        crate::execution::create_precompile(
            "WithdrawalTracker",
            env,
            crate::execution::NoCallRules,
            |data, caller| Self::new().call(data, caller),
        )
    }

    /// Initializes the precompile account marker.
    pub fn initialize(&mut self) -> tempo_precompiles::Result<()> {
        self.__initialize()
    }

    /// Returns the withdrawal balance attributed to `user` for `token`.
    pub fn zone_balance_of(
        &self,
        user: Address,
        token: Address,
    ) -> tempo_precompiles::Result<U256> {
        self.zone_balance[user][token].read()
    }

    /// Returns aggregate Zone supply for `token`.
    pub fn zone_total_supply_of(&self, token: Address) -> tempo_precompiles::Result<U256> {
        self.zone_total_supply[token].read()
    }

    /// Credits a successful deposit mint.
    pub fn record_deposit(
        &mut self,
        user: Address,
        token: Address,
        amount: U256,
    ) -> ZoneResult<()> {
        let balance = self.zone_balance_of(user, token)?;
        let total = self.zone_total_supply_of(token)?;
        let next_balance = balance
            .checked_add(amount)
            .ok_or_else(TempoPrecompileError::under_overflow)?;
        let next_total = total
            .checked_add(amount)
            .ok_or_else(TempoPrecompileError::under_overflow)?;

        self.zone_balance[user][token].write(next_balance)?;
        self.zone_total_supply[token].write(next_total)?;
        Ok(())
    }

    /// Debits a user withdrawal, including the fee locked at request time.
    pub fn record_withdrawal(
        &mut self,
        user: Address,
        token: Address,
        amount: U256,
        fee: U256,
    ) -> ZoneResult<()> {
        let requested = amount
            .checked_add(fee)
            .ok_or_else(TempoPrecompileError::under_overflow)?;
        let available = self.zone_balance_of(user, token)?;
        let Some(next_balance) = available.checked_sub(requested) else {
            return Err(WithdrawalTrackerError::insufficient_zone_balance(
                user, token, requested, available,
            )
            .into());
        };
        let next_total = self
            .zone_total_supply_of(token)?
            .checked_sub(requested)
            .ok_or_else(TempoPrecompileError::under_overflow)?;

        self.zone_balance[user][token].write(next_balance)?;
        self.zone_total_supply[token].write(next_total)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use tempo_precompiles::storage::{StorageCtx, hashmap::HashMapStorageProvider};

    const ALICE: Address = address!("0x00000000000000000000000000000000000000a1");
    const BOB: Address = address!("0x00000000000000000000000000000000000000b0");
    const TOKEN: Address = address!("0x20c0000000000000000000000000000000000000");
    const OTHER_TOKEN: Address = address!("0x20c0000000000000000000000000000000000001");

    #[test]
    fn tracks_zone_balances_and_zone_total_supply_by_token() -> eyre::Result<()> {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, || -> eyre::Result<()> {
            let mut tracker = WithdrawalTracker::new();
            tracker.record_deposit(ALICE, TOKEN, U256::from(100))?;
            tracker.record_deposit(BOB, TOKEN, U256::from(40))?;
            tracker.record_deposit(ALICE, OTHER_TOKEN, U256::from(30))?;
            tracker.record_withdrawal(ALICE, TOKEN, U256::from(60), U256::from(10))?;

            assert_eq!(tracker.zone_balance_of(ALICE, TOKEN)?, U256::from(30));
            assert_eq!(tracker.zone_balance_of(BOB, TOKEN)?, U256::from(40));
            assert_eq!(tracker.zone_balance_of(ALICE, OTHER_TOKEN)?, U256::from(30));
            assert_eq!(tracker.zone_balance_of(BOB, OTHER_TOKEN)?, U256::ZERO);
            assert_eq!(tracker.zone_total_supply_of(TOKEN)?, U256::from(70));
            assert_eq!(tracker.zone_total_supply_of(OTHER_TOKEN)?, U256::from(30));
            Ok(())
        })
    }

    #[test]
    fn rejects_withdrawal_over_zone_balance_without_changing_state() -> eyre::Result<()> {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, || -> eyre::Result<()> {
            let mut tracker = WithdrawalTracker::new();
            tracker.record_deposit(ALICE, TOKEN, U256::from(50))?;

            let error = tracker
                .record_withdrawal(ALICE, TOKEN, U256::from(45), U256::from(6))
                .unwrap_err();
            assert!(matches!(
                error,
                crate::ZonePrecompileError::WithdrawalTracker(
                    WithdrawalTrackerError::InsufficientZoneBalance(_)
                )
            ));
            assert_eq!(tracker.zone_balance_of(ALICE, TOKEN)?, U256::from(50));
            assert_eq!(tracker.zone_total_supply_of(TOKEN)?, U256::from(50));
            Ok(())
        })
    }
}
