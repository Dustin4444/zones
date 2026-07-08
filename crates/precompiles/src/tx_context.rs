//! Transaction-hash execution context for authenticated withdrawals.
//!
//! The zone outbox needs the real hash of the currently executing user
//! transaction so it can commit `senderTag = keccak256(sender || txHash)`.
//! Block executors publish that hash before EVM execution, and this precompile
//! exposes it to Solidity at the fixed ZoneTxContext address.

use alloc::vec::Vec;

#[cfg(feature = "std")]
use std::{cell::RefCell, thread_local};

#[cfg(not(feature = "std"))]
use core::cell::UnsafeCell;

use alloy_evm::precompiles::{DynPrecompile, PrecompileInput};
use alloy_primitives::{B256, Bytes, keccak256};
use alloy_sol_types::{SolCall, SolError};
use revm::precompile::{PrecompileId, PrecompileOutput};
use tracing::{debug, warn};

alloy_sol_types::sol! {
    function currentTxHash() external returns (bytes32);
    error DelegateCallNotAllowed();
}

#[cfg(feature = "std")]
thread_local! {
    static CURRENT_TX_HASH: RefCell<Option<B256>> = const { RefCell::new(None) };
}

#[cfg(not(feature = "std"))]
struct CurrentTxHashCell(UnsafeCell<Option<B256>>);

#[cfg(not(feature = "std"))]
// The no_std prover guest executes this code single-threaded. Native node and
// host builds use the std thread-local above.
unsafe impl Sync for CurrentTxHashCell {}

#[cfg(not(feature = "std"))]
static CURRENT_TX_HASH: CurrentTxHashCell = CurrentTxHashCell(UnsafeCell::new(None));

/// Guard that clears the current tx hash when dropped.
pub struct TxHashGuard;

impl Drop for TxHashGuard {
    fn drop(&mut self) {
        clear_current_tx_hash();
    }
}

/// Publish the current executing transaction hash for the duration of EVM execution.
pub fn set_current_tx_hash(tx_hash: B256) -> TxHashGuard {
    set_current_tx_hash_inner(Some(tx_hash));
    TxHashGuard
}

#[cfg(feature = "std")]
fn set_current_tx_hash_inner(tx_hash: Option<B256>) {
    CURRENT_TX_HASH.with(|slot| {
        *slot.borrow_mut() = tx_hash;
    });
}

#[cfg(not(feature = "std"))]
fn set_current_tx_hash_inner(tx_hash: Option<B256>) {
    // SAFETY: the no_std prover guest is single-threaded, and the guard clears
    // this cell before execution returns to the caller.
    unsafe {
        *CURRENT_TX_HASH.0.get() = tx_hash;
    }
}

fn clear_current_tx_hash() {
    set_current_tx_hash_inner(None);
}

#[cfg(feature = "std")]
fn current_tx_hash() -> Option<B256> {
    CURRENT_TX_HASH.with(|slot| *slot.borrow())
}

#[cfg(not(feature = "std"))]
fn current_tx_hash() -> Option<B256> {
    // SAFETY: the no_std prover guest is single-threaded.
    unsafe { *CURRENT_TX_HASH.0.get() }
}

fn synthetic_tx_hash(input: &PrecompileInput<'_>) -> B256 {
    let mut bytes = Vec::with_capacity(16 + 20 + 20 + 32 + 32 + 32 + input.data.len());
    bytes.extend_from_slice(b"zone-tx-context");
    bytes.extend_from_slice(input.caller.as_slice());
    bytes.extend_from_slice(input.target_address.as_slice());
    bytes.extend_from_slice(&input.value.to_be_bytes::<32>());
    bytes.extend_from_slice(&input.internals.block_number().to_be_bytes::<32>());
    bytes.extend_from_slice(&input.internals.block_timestamp().to_be_bytes::<32>());
    bytes.extend_from_slice(input.data);
    keccak256(bytes)
}

/// `DynPrecompile` implementation that returns the currently executing zone tx hash.
pub struct ZoneTxContext;

impl ZoneTxContext {
    pub fn create() -> DynPrecompile {
        DynPrecompile::new_stateful(PrecompileId::Custom("ZoneTxContext".into()), move |input| {
            if !input.is_direct_call() {
                warn!(
                    target: "zone::precompile",
                    "ZoneTxContext called via DELEGATECALL - rejecting"
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
            if selector != currentTxHashCall::SELECTOR {
                warn!(
                    target: "zone::precompile",
                    ?selector,
                    "ZoneTxContext: unknown selector"
                );
                return Ok(PrecompileOutput::revert(0, Bytes::new(), input.reservoir));
            }

            debug!(target: "zone::precompile", "ZoneTxContext: currentTxHash");

            let tx_hash = current_tx_hash().unwrap_or_else(|| synthetic_tx_hash(&input));
            let encoded = currentTxHashCall::abi_encode_returns(&tx_hash);
            Ok(PrecompileOutput::new(20, encoded.into(), input.reservoir))
        })
    }
}
