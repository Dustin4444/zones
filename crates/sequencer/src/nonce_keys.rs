//! Nonce key constants for zone sequencer L1 operations.
//!
//! Tempo's 2D nonce system allows each account to maintain independent nonce
//! counters ("lanes") keyed by a `U256` nonce key.
//!
//! Nonce management is handled by [`NonceKeyFiller`](tempo_alloy::fillers::NonceKeyFiller)
//! in the provider pipeline — callers only need to set `.nonce_key(KEY)` on
//! each contract call.

use alloy_primitives::{U256, uint};

/// Nonce key for `submitBatch` calls.
pub const SUBMIT_BATCH_NONCE_KEY: U256 = uint!(1_U256);

/// Nonce key for `processWithdrawals` calls.
pub const PROCESS_WITHDRAWAL_NONCE_KEY: U256 = uint!(2_U256);

/// Nonce key for ordered withdrawal submission.
pub const WITHDRAWAL_NONCE_KEY: U256 = uint!(4_U256);

/// Nonce key for admin operations (`enableToken`, `setZoneGasRate`, `setMaxTempoGasRate`,
/// `setBouncebackGas`, `setSequencerEncryptionKey`, `pause`, `abdicate`,
/// `pauseDeposits`, `resumeDeposits`). Low
/// frequency, shared key.
pub const ADMIN_OPS_NONCE_KEY: U256 = uint!(3_U256);
