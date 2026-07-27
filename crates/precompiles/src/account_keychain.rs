//! Zone privacy rules for the upstream Tempo account-keychain precompile.

use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use tempo_precompiles::{
    Precompile as _,
    account_keychain::{
        AccountKeychain, IAccountKeychain, getAllowedCallsCall, getKeyCall, getRemainingLimitCall,
        getRemainingLimitWithPeriodCall, isKeyAuthorizationWitnessBurnedCall,
    },
    dispatch::selector_from_calldata,
};

use crate::{
    account_privacy::AccountPrivacy,
    execution::{CallCheck, CallRules},
    storage::{L1State, L1StorageReader},
};

#[derive(Clone)]
pub(crate) struct AccountKeychainRules<P> {
    privacy: AccountPrivacy<P>,
}

impl<P> AccountKeychainRules<P> {
    pub(crate) fn new(l1: L1State<P>) -> Self {
        Self {
            privacy: AccountPrivacy::new(l1),
        }
    }
}

fn account_from<C: SolCall>(args: &[u8], account: impl FnOnce(C) -> Address) -> Option<Address> {
    C::abi_decode_raw(args).ok().map(account)
}

impl<P: L1StorageReader> CallRules for AccountKeychainRules<P> {
    fn admit(&self, data: &[u8], caller: Address) -> CallCheck {
        let Some(selector) = selector_from_calldata(data) else {
            return CallCheck::Continue;
        };
        let args = &data[4..];
        let account = match selector {
            getKeyCall::SELECTOR => account_from::<getKeyCall>(args, |call| call.account),
            getRemainingLimitCall::SELECTOR => {
                account_from::<getRemainingLimitCall>(args, |call| call.account)
            }
            getRemainingLimitWithPeriodCall::SELECTOR => {
                account_from::<getRemainingLimitWithPeriodCall>(args, |call| call.account)
            }
            getAllowedCallsCall::SELECTOR => {
                account_from::<getAllowedCallsCall>(args, |call| call.account)
            }
            isKeyAuthorizationWitnessBurnedCall::SELECTOR => {
                account_from::<isKeyAuthorizationWitnessBurnedCall>(args, |call| call.account)
            }
            IAccountKeychain::isAdminKeyCall::SELECTOR => {
                account_from::<IAccountKeychain::isAdminKeyCall>(args, |call| call.account)
            }
            _ => return CallCheck::Continue,
        };

        account.map_or(CallCheck::Continue, |account| {
            self.privacy.authorize(caller, &[account])
        })
    }
}

pub(crate) fn execute(data: &[u8], caller: Address) -> revm::precompile::PrecompileResult {
    AccountKeychain::new().call(data, caller)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use alloy_sol_types::SolError;
    use tempo_precompiles::storage::StorageCtx;
    use tempo_zone_contracts::Unauthorized;

    use crate::test_utils::{MockL1Reader, test_context, test_storage_provider};

    fn assert_allowed(
        rules: &AccountKeychainRules<MockL1Reader>,
        call: impl SolCall,
        caller: Address,
    ) {
        assert!(matches!(
            rules.admit(&call.abi_encode(), caller),
            CallCheck::Continue
        ));
    }

    fn assert_unauthorized(
        rules: &AccountKeychainRules<MockL1Reader>,
        call: impl SolCall,
        caller: Address,
    ) {
        let CallCheck::Revert(bytes) = rules.admit(&call.abi_encode(), caller) else {
            panic!("private account read must revert")
        };
        assert_eq!(bytes, Unauthorized {}.abi_encode());
    }

    #[test]
    fn account_getters_allow_only_owner_or_sequencer() {
        let owner = Address::repeat_byte(0x11);
        let outsider = Address::repeat_byte(0x22);
        let sequencer = Address::repeat_byte(0x33);
        let key = Address::repeat_byte(0x44);
        let portal = Address::repeat_byte(0x55);
        let reader = MockL1Reader::default();
        reader.seed_active_sequencer(portal, 0, sequencer);
        let rules = AccountKeychainRules::new(L1State::new(reader, portal));
        let mut context = test_context();
        let mut storage = test_storage_provider(&mut context, u64::MAX, false);

        StorageCtx::enter(&mut storage, || {
            macro_rules! check {
                ($call:expr) => {{
                    let call = $call;
                    assert_allowed(&rules, call.clone(), owner);
                    assert_allowed(&rules, call.clone(), sequencer);
                    assert_unauthorized(&rules, call, outsider);
                }};
            }

            check!(getKeyCall {
                account: owner,
                keyId: key
            });
            check!(getRemainingLimitCall {
                account: owner,
                keyId: key,
                token: Address::repeat_byte(0x66),
            });
            check!(getRemainingLimitWithPeriodCall {
                account: owner,
                keyId: key,
                token: Address::repeat_byte(0x66),
            });
            check!(getAllowedCallsCall {
                account: owner,
                keyId: key
            });
            check!(isKeyAuthorizationWitnessBurnedCall {
                account: owner,
                witness: B256::repeat_byte(0x77),
            });
            check!(IAccountKeychain::isAdminKeyCall {
                account: owner,
                keyId: key
            });
        });
    }

    #[test]
    fn non_account_getter_is_unchanged() {
        let rules = AccountKeychainRules::new(L1State::new(MockL1Reader::default(), Address::ZERO));
        assert!(matches!(
            rules.admit(
                &IAccountKeychain::getTransactionKeyCall {}.abi_encode(),
                Address::repeat_byte(0x11)
            ),
            CallCheck::Continue
        ));
    }
}
