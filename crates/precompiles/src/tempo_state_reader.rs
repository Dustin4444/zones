//! `DynPrecompile` implementation for the TempoStateReader.
//!
//! The TempoStateReader is a **standalone precompile** (separate from the TempoState contract)
//! that allows zone system contracts to read Tempo L1 contract storage at a specific block height
//! during EVM execution. The caller provides the L1 block number to query, making the precompile
//! fully stateless.
//!
//! This precompile implements two functions:
//!
//! - `readStorageAt(address account, bytes32 slot, uint64 blockNumber) -> bytes32`
//! - `readStorageBatchAt(address account, bytes32[] slots, uint64 blockNumber) -> bytes32[]`
//!
//! Reads are served synchronously from a [`TempoStateReaderProvider`]. The zone node implements
//! that trait with a cache-first, RPC-fallback provider; prover guests can implement it with a
//! witness-backed reader.
//!
//! # Gas costs
//!
//! Each call is charged [`BASE_GAS`] plus [`PER_SLOT_GAS`] for every slot read.

use alloc::{format, vec::Vec};

use alloy_evm::precompiles::DynPrecompile;
use alloy_primitives::{Address, B256, Bytes};
use alloy_sol_types::{SolCall, SolError};
use revm::precompile::{PrecompileId, PrecompileOutput, PrecompileResult};
use tracing::{debug, error, warn};

alloy_sol_types::sol! {
    /// Read a single storage slot from a Tempo L1 contract at a specific block height.
    function readStorageAt(address account, bytes32 slot, uint64 blockNumber) external view returns (bytes32);

    /// Read multiple storage slots from a Tempo L1 contract at a specific block height.
    function readStorageBatchAt(address account, bytes32[] calldata slots, uint64 blockNumber) external view returns (bytes32[] memory);

    /// Returned when the precompile is invoked via `DELEGATECALL` instead of `CALL`.
    error DelegateCallNotAllowed();
}

/// Backend used by [`TempoStateReader`] to fetch L1 storage during EVM execution.
pub trait TempoStateReaderProvider {
    /// Error returned when a storage read cannot be served.
    type Error: core::fmt::Display;

    /// Read a storage slot synchronously at `block_number`.
    fn get_storage(
        &self,
        address: Address,
        slot: B256,
        block_number: u64,
    ) -> Result<B256, Self::Error>;
}

/// Fixed gas cost charged on every call.
const BASE_GAS: u64 = 200;

/// Additional gas charged per storage slot read.
const PER_SLOT_GAS: u64 = 200;

/// Factory for the TempoStateReader `DynPrecompile`.
///
/// The precompile is registered at a dedicated predeploy address (separate from the TempoState
/// contract) and handles `readStorageAt` and `readStorageBatchAt` calls by reading Tempo L1
/// contract storage via a [`TempoStateReaderProvider`].
///
/// The caller provides the L1 block number to query, making the precompile fully stateless.
/// Zone system contracts (ZoneInbox, ZoneConfig) pass the `tempoBlockNumber` from the
/// TempoState contract after `finalizeTempo` has been called.
///
/// # Restrictions
///
/// - Only direct `CALL`s are accepted; `DELEGATECALL` reverts with [`DelegateCallNotAllowed`].
/// - The precompile is **view-only** and never writes to EVM state.
pub struct TempoStateReader;

impl TempoStateReader {
    /// Create a [`DynPrecompile`] that dispatches `readStorageAt` and
    /// `readStorageBatchAt` calls to `provider`.
    pub fn create<P>(provider: P) -> DynPrecompile
    where
        P: TempoStateReaderProvider + Send + Sync + 'static,
    {
        DynPrecompile::new_stateful(
            PrecompileId::Custom("TempoStateReader".into()),
            move |input| {
                if !input.is_direct_call() {
                    warn!(target: "zone::precompile", "TempoStateReader called via DELEGATECALL - rejecting");
                    return Ok(PrecompileOutput::revert(
                        0,
                        DelegateCallNotAllowed {}.abi_encode().into(),
                        input.reservoir,
                    ));
                }

                let data = input.data;
                if data.len() < 4 {
                    warn!(target: "zone::precompile", data_len = data.len(), "TempoStateReader called with insufficient data");
                    return Ok(PrecompileOutput::revert(0, Bytes::new(), input.reservoir));
                }

                let selector: [u8; 4] = data[..4].try_into().expect("len >= 4");

                let result = if selector == readStorageAtCall::SELECTOR {
                    debug!(target: "zone::precompile", "TempoStateReader: readStorageAt");
                    Self::handle_single_slot(&provider, data, input.reservoir)
                } else if selector == readStorageBatchAtCall::SELECTOR {
                    debug!(target: "zone::precompile", "TempoStateReader: readStorageBatchAt");
                    Self::handle_multi_slot(&provider, data, input.reservoir)
                } else {
                    warn!(target: "zone::precompile", selector = ?selector, "TempoStateReader: unknown selector");
                    Ok(PrecompileOutput::revert(0, Bytes::new(), input.reservoir))
                };

                match &result {
                    Ok(output) if output.bytes.is_empty() && output.gas_used == 0 => {
                        warn!(target: "zone::precompile", "TempoStateReader returned reverted output");
                    }
                    Err(e) => {
                        error!(target: "zone::precompile", %e, "TempoStateReader hard error");
                    }
                    _ => {}
                }

                result
            },
        )
    }

    /// Handle a `readStorageAt(address, bytes32, uint64)` call.
    ///
    /// Decodes the ABI calldata, performs a synchronous lookup via the provider at the specified
    /// L1 block number, and returns the ABI-encoded `bytes32` value. Returns a hard precompile
    /// error if the provider cannot serve the slot.
    fn handle_single_slot<P: TempoStateReaderProvider>(
        provider: &P,
        data: &[u8],
        reservoir: u64,
    ) -> PrecompileResult {
        let call = match readStorageAtCall::abi_decode(data) {
            Ok(call) => call,
            Err(_) => return Ok(PrecompileOutput::revert(0, Bytes::new(), reservoir)),
        };

        let value = provider
            .get_storage(call.account, call.slot, call.blockNumber)
            .map_err(|e| {
                crate::zone_rpc_error(format!(
                    "L1 storage unavailable for account={} slot={} block={}: {e}",
                    call.account, call.slot, call.blockNumber
                ))
            })?;

        let encoded = readStorageAtCall::abi_encode_returns(&value);
        Ok(PrecompileOutput::new(
            BASE_GAS + PER_SLOT_GAS,
            encoded.into(),
            reservoir,
        ))
    }

    /// Handle a `readStorageBatchAt(address, bytes32[], uint64)` call.
    ///
    /// Decodes the ABI calldata, performs a synchronous lookup for each slot at the specified
    /// L1 block number, and returns the ABI-encoded `bytes32[]` result. If **any** slot fails,
    /// the entire call fails with a hard precompile error.
    fn handle_multi_slot<P: TempoStateReaderProvider>(
        provider: &P,
        data: &[u8],
        reservoir: u64,
    ) -> PrecompileResult {
        let call = match readStorageBatchAtCall::abi_decode(data) {
            Ok(call) => call,
            Err(_) => return Ok(PrecompileOutput::revert(0, Bytes::new(), reservoir)),
        };

        let mut results = Vec::with_capacity(call.slots.len());
        for slot in &call.slots {
            let value = provider
                .get_storage(call.account, *slot, call.blockNumber)
                .map_err(|e| {
                    crate::zone_rpc_error(format!(
                        "L1 storage unavailable for account={} slot={} block={}: {e}",
                        call.account, slot, call.blockNumber
                    ))
                })?;
            results.push(value);
        }

        let encoded = readStorageBatchAtCall::abi_encode_returns(&results);
        Ok(PrecompileOutput::new(
            BASE_GAS + PER_SLOT_GAS * call.slots.len() as u64,
            encoded.into(),
            reservoir,
        ))
    }
}
