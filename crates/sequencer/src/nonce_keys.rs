//! Nonce key constants for zone sequencer L1 operations.
//!
//! Tempo's 2D nonce system allows each account to maintain independent nonce
//! counters ("lanes") keyed by a `U256` nonce key. Each sequencer operation
//! type normally uses a dedicated lane so that `submitBatch`, recovery
//! `processWithdrawals`, and admin transactions can be submitted concurrently
//! without nonce contention. A withdrawal backrun and its paired `submitBatch`
//! use a dedicated best-effort lane, which orders the pair without allowing a
//! stuck withdrawal to block the mandatory settlement lane.
//!
//! Nonce management is handled by [`NonceKeyFiller`](tempo_alloy::fillers::NonceKeyFiller)
//! in the provider pipeline — callers only need to set `.nonce_key(KEY)` on
//! each contract call.

use alloy_primitives::{U256, uint};

/// Nonce key for mandatory `submitBatch` calls.
pub const SUBMIT_BATCH_NONCE_KEY: U256 = uint!(1_U256);

/// Nonce key for recovery/backlog `processWithdrawals` calls.
pub const PROCESS_WITHDRAWAL_NONCE_KEY: U256 = uint!(2_U256);

/// Nonce key for best-effort `submitBatch` plus ordered withdrawal backruns.
///
/// Any uncertain transaction disables this lane for the process lifetime. Mandatory settlement
/// continues on [`SUBMIT_BATCH_NONCE_KEY`] while lane 2 recovers the withdrawal queue.
pub const WITHDRAWAL_BACKRUN_NONCE_KEY: U256 = uint!(4_U256);

/// Nonce key for admin operations (`enableToken`, `setZoneGasRate`, `setMaxTempoGasRate`,
/// `setBouncebackGas`, `setSequencerEncryptionKey`, `pause`, `abdicate`,
/// `pauseDeposits`, `resumeDeposits`). Low
/// frequency, shared key.
pub const ADMIN_OPS_NONCE_KEY: U256 = uint!(3_U256);
