//! ABI dispatch for the [`ZoneFeeManager`] precompile.

use alloy_primitives::Address;
use revm::precompile::PrecompileResult;
use tempo_contracts::precompiles::{IFeeManager, ITIPFeeAMM};
use tempo_precompiles::{
    Precompile as TempoPrecompile, charge_input_cost, dispatch, mutate_void, storage::StorageCtx,
    view,
};

use super::{ZoneFeeManager, ZonePortalReader};

impl<P: ZonePortalReader> TempoPrecompile for ZoneFeeManager<P> {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        if let Some(err) = charge_input_cost(&mut StorageCtx::default(), calldata) {
            return err;
        }

        dispatch!(
            calldata,
            |call| match call {
                IFeeManager::IFeeManagerCalls {
                    userTokens(call) => view(call, |c| self.user_tokens(c)),
                    validatorTokens(call) => {
                        view(call, |c| self.get_validator_token(c.validator))
                    },
                    collectedFees(call) => {
                        view(call, |c| self.collected_fees(c.validator, c.token))
                    },
                    setValidatorToken(call) => {
                        mutate_void(call, msg_sender, |s, c| self.set_validator_token(s, c))
                    },
                    setUserToken(call) => {
                        mutate_void(call, msg_sender, |s, c| self.set_user_token(s, c))
                    },
                    distributeFees(call) => mutate_void(call, msg_sender, |_, c| {
                        self.distribute_fees(c.validator, c.token)
                    }),
                }
                ITIPFeeAMM::ITIPFeeAMMCalls {
                    M(_) => self.fee_amm_disabled(),
                    N(_) => self.fee_amm_disabled(),
                    SCALE(_) => self.fee_amm_disabled(),
                    MIN_LIQUIDITY(_) => self.fee_amm_disabled(),
                    getPoolId(_) => self.fee_amm_disabled(),
                    getPool(_) => self.fee_amm_disabled(),
                    pools(_) => self.fee_amm_disabled(),
                    totalSupply(_) => self.fee_amm_disabled(),
                    liquidityBalances(_) => self.fee_amm_disabled(),
                    mint(_) => self.fee_amm_disabled(),
                    burn(_) => self.fee_amm_disabled(),
                    rebalanceSwap(_) => self.fee_amm_disabled(),
                }
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{L1StorageReader, TempoState, fee_manager::FeeAmmDisabled};

    use alloc::vec::Vec;
    use alloy_primitives::{B256, U256, address};
    use alloy_rlp::Encodable as _;
    use alloy_sol_types::{SolCall, SolError, SolInterface, SolValue};
    use revm::precompile::PrecompileError;
    use tempo_contracts::precompiles::{
        FeeManagerError, IFeeManager::IFeeManagerCalls, ITIPFeeAMM::ITIPFeeAMMCalls,
    };
    use tempo_precompiles::{
        TIP_FEE_MANAGER_ADDRESS,
        storage::{ContractStorage, Handler, StorageCtx, hashmap::HashMapStorageProvider},
        test_util::{TIP20Setup, assert_full_coverage, check_selector_coverage},
        tip_fee_manager::TipFeeManager,
    };
    use tempo_primitives::TempoHeader;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn expect_fee_manager_revert(result: &PrecompileResult, expected_error: FeeManagerError) {
        match result {
            Ok(output) => {
                assert!(output.is_revert());
                let decoded = FeeManagerError::abi_decode(&output.bytes).unwrap();
                assert_eq!(decoded, expected_error);
            }
            Err(err) => panic!("expected reverted output, got: {err:?}"),
        }
    }

    #[derive(Debug, Clone)]
    struct MockPortalReader {
        portal: Address,
        enabled: bool,
    }

    impl L1StorageReader for MockPortalReader {
        fn read_l1_storage(
            &self,
            account: Address,
            slot: B256,
            _block_number: u64,
        ) -> Result<B256, PrecompileError> {
            assert_eq!(account, self.portal);
            assert_ne!(slot, B256::ZERO);
            let mut bytes = [0u8; 32];
            bytes[31] = u8::from(self.enabled);
            Ok(B256::new(bytes))
        }
    }

    impl ZonePortalReader for MockPortalReader {
        fn portal_address(&self) -> Address {
            self.portal
        }
    }

    fn initialize_tempo_state() -> tempo_precompiles::Result<()> {
        let mut header = Vec::new();
        TempoHeader::default().encode(&mut header);
        TempoState::new().initialize(&header)
    }

    #[test]
    fn set_user_token_dispatch_accepts_enabled_non_usd_token() -> TestResult {
        let mut storage = HashMapStorageProvider::new(1);
        let admin = address!("0x0000000000000000000000000000000000000a11");
        let user = address!("0x0000000000000000000000000000000000000b0b");
        let portal = address!("0x0000000000000000000000000000000000000d0d");

        StorageCtx::enter(&mut storage, || {
            initialize_tempo_state()?;
            let token = TIP20Setup::create("Zone EUR", "zEUR", admin)
                .currency("EUR")
                .apply()?;
            let mut manager = ZoneFeeManager::new(MockPortalReader {
                portal,
                enabled: true,
            });

            let result = manager
                .call(
                    &IFeeManager::setUserTokenCall {
                        token: token.address(),
                    }
                    .abi_encode(),
                    user,
                )
                .expect("setUserToken precompile call should not hard fail");
            assert!(result.status.is_success());

            let result = manager
                .call(&IFeeManager::userTokensCall { user }.abi_encode(), user)
                .expect("userTokens precompile call should not hard fail");
            assert!(result.status.is_success());
            assert_eq!(
                Address::abi_decode(&result.bytes).expect("address return"),
                token.address()
            );

            Ok::<_, tempo_precompiles::error::TempoPrecompileError>(())
        })?;

        Ok(())
    }

    #[test]
    fn disabled_portal_token_reverts() -> TestResult {
        let mut storage = HashMapStorageProvider::new(1);
        let admin = address!("0x0000000000000000000000000000000000000a11");
        let user = address!("0x0000000000000000000000000000000000000b0b");
        let portal = address!("0x0000000000000000000000000000000000000d0d");

        StorageCtx::enter(&mut storage, || {
            initialize_tempo_state()?;
            let token = TIP20Setup::create("Zone USD", "zUSD", admin).apply()?;
            let mut manager = ZoneFeeManager::new(MockPortalReader {
                portal,
                enabled: false,
            });

            let result = manager.call(
                &IFeeManager::setUserTokenCall {
                    token: token.address(),
                }
                .abi_encode(),
                user,
            );
            expect_fee_manager_revert(&result, FeeManagerError::invalid_token());

            Ok::<_, tempo_precompiles::error::TempoPrecompileError>(())
        })?;

        Ok(())
    }

    #[test]
    fn fee_amm_methods_revert() -> TestResult {
        let mut storage = HashMapStorageProvider::new(1);
        let sender = address!("0x0000000000000000000000000000000000000b0b");

        StorageCtx::enter(&mut storage, || {
            let mut manager = ZoneFeeManager::new(MockPortalReader {
                portal: address!("0x0000000000000000000000000000000000000d0d"),
                enabled: true,
            });

            let result = manager
                .call(&ITIPFeeAMM::MCall {}.abi_encode(), sender)
                .expect("FeeAMM precompile call should revert, not hard fail");
            assert!(result.is_revert());
            assert_eq!(result.bytes, FeeAmmDisabled {}.abi_encode());

            Ok::<_, tempo_precompiles::error::TempoPrecompileError>(())
        })?;

        Ok(())
    }

    #[test]
    fn distribute_fees_dispatch() -> TestResult {
        let mut storage = HashMapStorageProvider::new(1);
        let admin = address!("0x0000000000000000000000000000000000000a11");
        let sequencer = address!("0x0000000000000000000000000000000000000c0c");
        let portal = address!("0x0000000000000000000000000000000000000d0d");

        StorageCtx::enter(&mut storage, || {
            let token = TIP20Setup::create("Zone USD", "zUSD", admin)
                .with_issuer(admin)
                .with_mint(TIP_FEE_MANAGER_ADDRESS, U256::from(3_000u64))
                .apply()?;
            TipFeeManager::new().collected_fees[sequencer][token.address()]
                .write(U256::from(3_000u64))?;

            let mut manager = ZoneFeeManager::new(MockPortalReader {
                portal,
                enabled: true,
            });
            let result = manager
                .call(
                    &IFeeManager::distributeFeesCall {
                        validator: sequencer,
                        token: token.address(),
                    }
                    .abi_encode(),
                    sequencer,
                )
                .expect("distributeFees precompile call should not hard fail");
            assert!(result.status.is_success());
            assert_eq!(
                TipFeeManager::new().collected_fees[sequencer][token.address()].read()?,
                U256::ZERO
            );

            Ok::<_, tempo_precompiles::error::TempoPrecompileError>(())
        })?;

        Ok(())
    }

    #[test]
    fn selector_coverage() -> TestResult {
        let mut storage = HashMapStorageProvider::new(1);
        StorageCtx::enter(&mut storage, || {
            let mut manager = ZoneFeeManager::new(MockPortalReader {
                portal: address!("0x0000000000000000000000000000000000000d0d"),
                enabled: true,
            });

            let fee_manager_unsupported = check_selector_coverage(
                &mut manager,
                IFeeManagerCalls::SELECTORS,
                "IFeeManager",
                IFeeManagerCalls::name_by_selector,
            );

            let amm_unsupported = check_selector_coverage(
                &mut manager,
                ITIPFeeAMMCalls::SELECTORS,
                "ITIPFeeAMM",
                ITIPFeeAMMCalls::name_by_selector,
            );

            assert_full_coverage([fee_manager_unsupported, amm_unsupported]);

            Ok::<_, tempo_precompiles::error::TempoPrecompileError>(())
        })?;

        Ok(())
    }
}
