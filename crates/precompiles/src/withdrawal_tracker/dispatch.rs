//! ABI dispatch and caller authorization for [`WithdrawalTracker`](super::WithdrawalTracker).

use alloy_primitives::Address;
use revm::precompile::PrecompileResult;
use tempo_precompiles::{
    Precompile as TempoPrecompile, charge_input_cost, dispatch,
    dispatch::typed::{mutate_void, view},
    storage::StorageCtx,
};
use tempo_zone_contracts::{IWithdrawalTracker, WithdrawalTrackerError};
use zone_primitives::constants::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS};

use crate::ZonePrecompileError;

use super::WithdrawalTracker;

impl TempoPrecompile for WithdrawalTracker {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        if let Some(err) = charge_input_cost(&mut self.storage, calldata) {
            return err;
        }

        dispatch!(calldata, |call| match call {
            IWithdrawalTracker::IWithdrawalTrackerCalls {
                zoneBalance(call) => view(call, |call| self.zone_balance_of(call.user, call.token)),
                zoneTotalSupply(call) => view(call, |call| self.zone_total_supply_of(call.token)),
                deposit(call) => {
                    if msg_sender != ZONE_INBOX_ADDRESS {
                        return StorageCtx.error_result(ZonePrecompileError::from(
                            WithdrawalTrackerError::only_zone_inbox(),
                        ));
                    }
                    mutate_void(call, msg_sender, |_sender, call| {
                        self.record_deposit(call.user, call.token, call.amount)
                    })
                },
                withdraw(call) => {
                    if msg_sender != ZONE_OUTBOX_ADDRESS {
                        return StorageCtx.error_result(ZonePrecompileError::from(
                            WithdrawalTrackerError::only_zone_outbox(),
                        ));
                    }
                    mutate_void(call, msg_sender, |_sender, call| {
                        self.record_withdrawal(call.user, call.token, call.amount, call.fee)
                    })
                },
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_sol_types::{SolCall, SolInterface};
    use tempo_precompiles::storage::hashmap::HashMapStorageProvider;

    #[test]
    fn mutations_are_restricted_to_the_inbox_and_outbox() {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, || {
            let mut tracker = WithdrawalTracker::new();
            let unauthorized = Address::repeat_byte(0x11);

            let deposit = IWithdrawalTracker::depositCall {
                user: Address::ZERO,
                token: Address::ZERO,
                amount: Default::default(),
            };
            let output = tracker.call(&deposit.abi_encode(), unauthorized).unwrap();
            assert_eq!(
                output.bytes,
                WithdrawalTrackerError::only_zone_inbox().abi_encode()
            );

            let withdraw = IWithdrawalTracker::withdrawCall {
                user: Address::ZERO,
                token: Address::ZERO,
                amount: Default::default(),
                fee: Default::default(),
            };
            let output = tracker.call(&withdraw.abi_encode(), unauthorized).unwrap();
            assert_eq!(
                output.bytes,
                WithdrawalTrackerError::only_zone_outbox().abi_encode()
            );
        });
    }
}
