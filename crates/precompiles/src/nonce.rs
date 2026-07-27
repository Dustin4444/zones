//! Zone privacy rules for the upstream Tempo nonce-manager precompile.

use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use tempo_precompiles::{
    Precompile as _,
    dispatch::selector_from_calldata,
    nonce::{INonce, NonceManager},
};

use crate::{
    account_privacy::AccountPrivacy,
    execution::{CallCheck, CallRules},
    storage::{L1State, L1StorageReader},
};

#[derive(Clone)]
pub(crate) struct NonceRules<P> {
    privacy: AccountPrivacy<P>,
}

impl<P> NonceRules<P> {
    pub(crate) fn new(l1: L1State<P>) -> Self {
        Self {
            privacy: AccountPrivacy::new(l1),
        }
    }
}

impl<P: L1StorageReader> CallRules for NonceRules<P> {
    fn admit(&self, data: &[u8], caller: Address) -> CallCheck {
        if selector_from_calldata(data) != Some(INonce::getNonceCall::SELECTOR) {
            return CallCheck::Continue;
        }
        let Ok(call) = INonce::getNonceCall::abi_decode_raw(&data[4..]) else {
            return CallCheck::Continue;
        };
        self.privacy.authorize(caller, &[call.account])
    }
}

pub(crate) fn execute(data: &[u8], caller: Address) -> revm::precompile::PrecompileResult {
    NonceManager::new().call(data, caller)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use alloy_sol_types::SolError;
    use tempo_precompiles::storage::StorageCtx;
    use tempo_zone_contracts::Unauthorized;

    use crate::test_utils::{MockL1Reader, test_context, test_storage_provider};

    #[test]
    fn nonce_getter_allows_only_owner_or_sequencer() {
        let owner = Address::repeat_byte(0x11);
        let outsider = Address::repeat_byte(0x22);
        let sequencer = Address::repeat_byte(0x33);
        let portal = Address::repeat_byte(0x44);
        let reader = MockL1Reader::default();
        reader.seed_active_sequencer(portal, 0, sequencer);
        let rules = NonceRules::new(L1State::new(reader, portal));
        let call = INonce::getNonceCall {
            account: owner,
            nonceKey: U256::from(7),
        };
        let mut context = test_context();
        let mut storage = test_storage_provider(&mut context, u64::MAX, false);

        StorageCtx::enter(&mut storage, || {
            for caller in [owner, sequencer] {
                assert!(matches!(
                    rules.admit(&call.abi_encode(), caller),
                    CallCheck::Continue
                ));
            }
            let CallCheck::Revert(bytes) = rules.admit(&call.abi_encode(), outsider) else {
                panic!("another account's nonce must be private")
            };
            assert_eq!(bytes, Unauthorized {}.abi_encode());
        });
    }
}
