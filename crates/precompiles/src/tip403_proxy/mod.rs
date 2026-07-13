//! Zone pre-execution rules and L1-backed execution for Tempo's TIP403 registry.
//!
//! The canonical registry address forwards allowed calls to
//! [`tempo_precompiles::tip403_registry::TIP403Registry`], which remains the source of
//! truth for registry behavior. [`TIP403Rules`] rejects mutations before forwarding
//! because policy state is managed on Tempo L1.
//!
//! Forwarded reads run against the zone's L1-backed storage provider and therefore
//! observe registry state at the finalized Tempo anchor.

use alloy_primitives::Address;
use alloy_sol_types::{SolCall, SolError};
use tempo_contracts::precompiles::{ITIP403Registry, TIP403_REGISTRY_ADDRESS};
use tempo_precompiles::storage::StorageCtx;

use crate::execution::{CallCheck, CallRules, ZoneCall};

/// Canonical TIP403 registry address, shared with Tempo L1.
pub const ZONE_TIP403_ADDRESS: Address = TIP403_REGISTRY_ADDRESS;

const TIP403_MUTATING_SELECTORS: &[[u8; 4]] = &[
    ITIP403Registry::createPolicyCall::SELECTOR,
    ITIP403Registry::createPolicyWithAccountsCall::SELECTOR,
    ITIP403Registry::setPolicyAdminCall::SELECTOR,
    ITIP403Registry::modifyPolicyWhitelistCall::SELECTOR,
    ITIP403Registry::modifyPolicyBlacklistCall::SELECTOR,
    ITIP403Registry::createCompoundPolicyCall::SELECTOR,
    ITIP403Registry::setReceivePolicyCall::SELECTOR,
];

alloy_sol_types::sol! {
    /// Returned when a mutation is attempted on the zone's L1-backed TIP403 registry.
    error ReadOnlyRegistry();
}

/// Rules that keep the zone registry read-only before upstream execution.
pub(crate) struct TIP403Rules;

impl CallRules for TIP403Rules {
    fn check_call(&self, call: ZoneCall<'_>) -> CallCheck {
        if call
            .selector
            .is_some_and(|selector| TIP403_MUTATING_SELECTORS.contains(&selector))
        {
            return CallCheck::Return(Ok(
                StorageCtx::default().revert_output(ReadOnlyRegistry {}.abi_encode().into())
            ));
        }

        CallCheck::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_evm::precompiles::DynPrecompile;
    use alloy_primitives::{Address, Bytes, U256, address};
    use alloy_sol_types::SolCall;
    use revm::precompile::PrecompileResult;
    use tempo_precompiles::storage::StorageCtx;

    use crate::{
        create_tip403_precompile,
        test_utils::{
            MockL1Reader, TestCtx, call_precompile_with_gas, test_context, test_l1_env,
            test_storage_provider,
        },
    };

    struct RegistryHarness {
        ctx: TestCtx,
        precompile: DynPrecompile,
        caller: Address,
    }

    impl RegistryHarness {
        fn new(l1: MockL1Reader) -> eyre::Result<Self> {
            let mut ctx = test_context();
            {
                let mut storage = test_storage_provider(&mut ctx, u64::MAX, false);
                StorageCtx::enter(&mut storage, || -> eyre::Result<()> {
                    StorageCtx::default().sstore(
                        zone_primitives::constants::TEMPO_STATE_ADDRESS,
                        crate::tempo_state::slots::TEMPO_BLOCK_NUMBER,
                        U256::from(77u64),
                    )?;
                    Ok(())
                })?;
            }

            let env = test_l1_env(&ctx, l1);
            let precompile = create_tip403_precompile(&env);
            Ok(Self {
                ctx,
                precompile,
                caller: address!("0x0000000000000000000000000000000000000aaa"),
            })
        }

        fn call(&mut self, calldata: Bytes, is_static: bool) -> PrecompileResult {
            call_precompile_with_gas(
                &mut self.ctx,
                &self.precompile,
                self.caller,
                &calldata,
                100_000,
                is_static,
                crate::tip403_proxy::ZONE_TIP403_ADDRESS,
                crate::tip403_proxy::ZONE_TIP403_ADDRESS,
            )
        }
    }

    #[test]
    fn registry_reads_l1_policy_storage_through_overlay() -> eyre::Result<()> {
        let alice = address!("0x00000000000000000000000000000000000000a1");
        let bob = address!("0x00000000000000000000000000000000000000b2");
        let l1 = MockL1Reader::default();
        l1.seed_blacklist_policy(5, &[alice])?;
        let mut harness = RegistryHarness::new(l1)?;

        let counter = harness.call(
            ITIP403Registry::policyIdCounterCall {}.abi_encode().into(),
            true,
        )?;
        assert!(counter.is_success());
        assert_eq!(
            ITIP403Registry::policyIdCounterCall::abi_decode_returns(&counter.bytes)?,
            6
        );

        let policy_data = harness.call(
            ITIP403Registry::policyDataCall { policyId: 5 }
                .abi_encode()
                .into(),
            true,
        )?;
        assert!(policy_data.is_success());
        let decoded = ITIP403Registry::policyDataCall::abi_decode_returns(&policy_data.bytes)?;
        assert_eq!(decoded.policyType, ITIP403Registry::PolicyType::BLACKLIST);

        let alice_auth = harness.call(
            ITIP403Registry::isAuthorizedCall {
                policyId: 5,
                user: alice,
            }
            .abi_encode()
            .into(),
            true,
        )?;
        assert!(alice_auth.is_success());
        assert!(!ITIP403Registry::isAuthorizedCall::abi_decode_returns(
            &alice_auth.bytes
        )?);

        let bob_auth = harness.call(
            ITIP403Registry::isAuthorizedCall {
                policyId: 5,
                user: bob,
            }
            .abi_encode()
            .into(),
            true,
        )?;
        assert!(bob_auth.is_success());
        assert!(ITIP403Registry::isAuthorizedCall::abi_decode_returns(
            &bob_auth.bytes
        )?);

        Ok(())
    }

    #[test]
    fn registry_supports_directional_policy_selectors_at_current_hardfork() -> eyre::Result<()> {
        let blocked = address!("0x00000000000000000000000000000000000000a1");
        let l1 = MockL1Reader::default();
        l1.seed_blacklist_policy(5, &[blocked])?;
        let mut harness = RegistryHarness::new(l1)?;

        let sender = harness.call(
            ITIP403Registry::isAuthorizedSenderCall {
                policyId: 5,
                user: blocked,
            }
            .abi_encode()
            .into(),
            true,
        )?;
        assert!(sender.is_success());
        assert!(!ITIP403Registry::isAuthorizedSenderCall::abi_decode_returns(&sender.bytes)?);

        let recipient = harness.call(
            ITIP403Registry::isAuthorizedRecipientCall {
                policyId: 5,
                user: blocked,
            }
            .abi_encode()
            .into(),
            true,
        )?;
        assert!(recipient.is_success());
        assert!(!ITIP403Registry::isAuthorizedRecipientCall::abi_decode_returns(&recipient.bytes)?);

        let mint_recipient = harness.call(
            ITIP403Registry::isAuthorizedMintRecipientCall {
                policyId: 5,
                user: blocked,
            }
            .abi_encode()
            .into(),
            true,
        )?;
        assert!(mint_recipient.is_success());
        assert!(
            !ITIP403Registry::isAuthorizedMintRecipientCall::abi_decode_returns(
                &mint_recipient.bytes
            )?
        );

        Ok(())
    }

    #[test]
    fn registry_rejects_mutating_selectors() -> eyre::Result<()> {
        let mut harness = RegistryHarness::new(MockL1Reader::default())?;
        let result = harness.call(
            ITIP403Registry::createPolicyCall {
                admin: harness.caller,
                policyType: ITIP403Registry::PolicyType::BLACKLIST,
            }
            .abi_encode()
            .into(),
            false,
        )?;
        assert!(result.is_revert());
        assert_eq!(result.bytes, Bytes::from(ReadOnlyRegistry {}.abi_encode()));
        Ok(())
    }
}
