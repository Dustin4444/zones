//! L1 state cache and provider for reading Tempo L1 contract storage from the zone.
//!
//! This module provides:
//!
//! - [`L1StateCache`] — a shared in-memory cache of L1 contract storage slots.
//! - [`L1StateCacheInner`] — the block-versioned cache storage guarded by [`L1StateCache`].
//! - [`L1StateProvider`] — a cache-first, RPC-fallback reader for `eth_getStorageAt`.
//! - [`DepositPrefetchPlan`] — event-derived Portal and canonical policy reads needed by deposits.
//! - [`PolicyCheckExecutor`] — the node-specific execution boundary for canonical policy reads.
//! - [`PrefetchCtx`] — exact Zone/L1 child context shared by one prefetch operation.

pub mod cache;
pub mod enabled_tokens;
mod prefetch;
pub mod provider;

pub use cache::{L1StateCache, L1StateCacheInner};
pub use enabled_tokens::EnabledTokenRegistry;
pub use prefetch::{DepositPrefetchPlan, PolicyCheckExecutor, PrefetchCtx};
pub use provider::{L1StateProvider, L1StateProviderConfig};
