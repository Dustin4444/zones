//! L1 state cache and provider for reading Tempo L1 contract storage from the zone.
//!
//! This module provides:
//!
//! - [`L1StateCache`] — a shared in-memory cache of L1 contract storage slots.
//! - [`L1StateCacheInner`] — the block-versioned cache storage guarded by [`L1StateCache`].
//! - [`L1StateProvider`] — a cache-first, RPC-fallback reader for `eth_getStorageAt`.
//! - [`DepositPrefetchPlan`] — event-derived storage reads needed to execute L1 deposits.
//!
//! TIP-20 and TIP-403 policy semantics are evaluated by Tempo's upstream precompiles. This
//! module only supplies their exact-block raw L1 storage view.

pub mod cache;
pub mod enabled_tokens;
mod prefetch;
pub mod provider;

pub use cache::{L1StateCache, L1StateCacheInner};
pub use enabled_tokens::EnabledTokenRegistry;
pub use prefetch::{DepositPrefetchConfig, DepositPrefetchPlan};
pub use provider::{L1StateProvider, L1StateProviderConfig};
