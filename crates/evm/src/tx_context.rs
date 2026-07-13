//! Transaction execution context for authenticated withdrawals.
//!
//! The zone outbox uses Tempo's sender-scoped unique transaction identifier as the public
//! `senderTag`. This precompile reads the identifier directly from the concrete [`TempoTxEnv`]
//! through [`EvmInternals`](alloy_evm::EvmInternals), avoiding any executor-side context.

use alloy_evm::precompiles::DynPrecompile;
use alloy_primitives::Bytes;
use alloy_sol_types::{SolCall, SolError};
use revm::precompile::{PrecompileId, PrecompileOutput};
use tempo_revm::TempoTxEnv;
use tracing::{debug, warn};

alloy_sol_types::sol! {
    function currentUniqueTxIdentifier() external returns (bytes32);
    error DelegateCallNotAllowed();
}

/// `DynPrecompile` implementation that returns the current Tempo transaction's unique identifier.
pub(crate) struct ZoneTxContext;

impl ZoneTxContext {
    pub(crate) fn create() -> DynPrecompile {
        DynPrecompile::new_stateful(PrecompileId::Custom("ZoneTxContext".into()), move |input| {
            if !input.is_direct_call() {
                warn!(
                    target: "zone::precompile",
                    "ZoneTxContext called via DELEGATECALL — rejecting"
                );
                return Ok(PrecompileOutput::revert(
                    0,
                    DelegateCallNotAllowed {}.abi_encode().into(),
                    input.reservoir,
                ));
            }

            let data = input.data;
            if data.len() < 4 {
                warn!(
                    target: "zone::precompile",
                    data_len = data.len(),
                    "ZoneTxContext called with insufficient data"
                );
                return Ok(PrecompileOutput::revert(0, Bytes::new(), input.reservoir));
            }

            let selector: [u8; 4] = data[..4].try_into().expect("len >= 4");
            if selector != currentUniqueTxIdentifierCall::SELECTOR {
                warn!(
                    target: "zone::precompile",
                    ?selector,
                    "ZoneTxContext: unknown selector"
                );
                return Ok(PrecompileOutput::revert(0, Bytes::new(), input.reservoir));
            }

            debug!(target: "zone::precompile", "ZoneTxContext: currentUniqueTxIdentifier");

            // This downcast is guaranteed by ZoneEvmFactory: its context is TempoContext<DB>,
            // whose transaction type is TempoTxEnv. EvmInternals only erases that concrete type
            // when it constructs the precompile input.
            let tx_env = input
                .internals
                .tx_env_downcast_ref::<TempoTxEnv>()
                .expect("ZoneTxContext requires TempoTxEnv");
            // Regular transaction execution always populates this while constructing TempoTxEnv;
            // it can only be absent in manually constructed or synthetic environments.
            let Some(unique_tx_identifier) = tx_env.unique_tx_identifier() else {
                warn!(
                    target: "zone::precompile",
                    "ZoneTxContext: unique transaction identifier is not set"
                );
                return Ok(PrecompileOutput::revert(0, Bytes::new(), input.reservoir));
            };
            let encoded = currentUniqueTxIdentifierCall::abi_encode_returns(&unique_tx_identifier);

            Ok(PrecompileOutput::new(20, encoded.into(), input.reservoir))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_evm::{
        EvmInternals,
        precompiles::{Precompile, PrecompileInput},
        revm::Context,
    };
    use alloy_primitives::{Address, B256, U256};
    use revm::{MainContext, context::CfgEnv, database::EmptyDB};
    use tempo_chainspec::hardfork::TempoHardfork;
    use tempo_evm::TempoBlockEnv;

    fn call_with_identifier(unique_tx_identifier: Option<B256>) -> PrecompileOutput {
        let mut ctx = Context::mainnet()
            .with_db(EmptyDB::default())
            .with_block(TempoBlockEnv::default())
            .with_cfg(CfgEnv::<TempoHardfork>::default())
            .with_tx(TempoTxEnv {
                unique_tx_identifier,
                ..Default::default()
            });
        let calldata = currentUniqueTxIdentifierCall {}.abi_encode();
        let precompile = ZoneTxContext::create();

        precompile
            .call(PrecompileInput {
                data: &calldata,
                gas: u64::MAX,
                reservoir: 0,
                caller: Address::ZERO,
                value: U256::ZERO,
                target_address: Address::ZERO,
                is_static: false,
                bytecode_address: Address::ZERO,
                internals: EvmInternals::from_context(&mut ctx),
            })
            .expect("precompile call should not fail")
    }

    #[test]
    fn returns_unique_transaction_identifier_from_tempo_tx_env() {
        let unique_tx_identifier = B256::repeat_byte(0x42);
        let output = call_with_identifier(Some(unique_tx_identifier));

        assert_eq!(
            output.bytes,
            currentUniqueTxIdentifierCall::abi_encode_returns(&unique_tx_identifier)
        );
    }

    #[test]
    fn reverts_when_unique_transaction_identifier_is_not_set() {
        let output = call_with_identifier(None);

        assert!(output.is_revert());
        assert!(output.bytes.is_empty());
    }
}
