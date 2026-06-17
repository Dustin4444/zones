//! TIP-403 policy cache, provider, and resolution task for the zone sequencer.
//!
//! This module tracks TIP-403 transfer policy state from Tempo L1:
//!
//! - [`PolicyCacheInner`] — block-versioned in-memory cache of policy data.
//! - [`PolicyProvider`] — cache-first, RPC-fallback authorization provider.
//! - [`PolicyResolutionTask`] — background task for pre-fetching authorization data.
//!
//! # Data flow
//!
//! ```text
//!                        L1
//!      (TIP403Registry, TIP-20 tokens, ZonePortal)
//!                  |                  ^
//!           events |                  | RPC fallback
//!                  |                  |
//!            L1Subscriber        PolicyProvider
//!                  |              |          |
//!            write |         read |          |
//!                  v              |          |
//!           PolicyCache-----+     engine + EVM
//!                  ^                        ^
//!                  |                        | pre-fetch
//!             seed (startup)    pool_prefetch + ResolutionTask
//! ```
//!
//! The [`L1Subscriber`](crate::l1::L1Subscriber) extracts policy events from
//! `eth_getBlockReceipts` and applies them (creates, set updates,
//! compound configs) to the [`PolicyCache`].
//! [`PolicyProvider`] serves authorization queries from cache, falling back to L1
//! RPC on miss and writing the result back for future lookups.
//!
//! # Startup sequence
//!
//! 1. [`PolicyCache::seed_token_policies`] — bulk-fetch current `transferPolicyId` values
//!    from L1 for all tracked tokens and populate the cache baseline.
//! 2. [`spawn_policy_resolution_task`] — start the background resolution task
//!    (processes pre-fetch requests from the pool and other callers).
//! 3. [`spawn_pool_prefetch_task`] — watch incoming pool transactions and submit
//!    sender/recipient addresses for cache warming.
//! 4. Create [`PolicyProvider`] instances — one for the engine payload builder,
//!    one for the EVM precompile, both backed by the same [`PolicyCache`].
//!
//! # Cache miss resolution
//!
//! The zone advances in lockstep with L1, so the L1Subscriber captures every
//! policy change for enabled tokens from the moment it starts. Cache misses only
//! occur for state that predates the subscriber — either because the zone was
//! created at an arbitrary L1 height or because the sequencer restarted with a
//! cold cache.
//!
//! On a miss, [`PolicyProvider::is_authorized`] falls back to an RPC call against
//! L1 at the block being evaluated and writes the result into the cache. This is
//! safe because L1 is authoritative and the zone never runs ahead of it: the
//! queried state is final for that block, and the subscriber will apply any future
//! changes that supersede it.
//!
//! # Key invariants
//!
//! - **Only the engine drives `advance()`**: the L1Subscriber writes events via
//!   `apply_events` but never advances the cache baseline. The engine calls
//!   `PolicyCache::advance()` after processing each L1 block, ensuring the
//!   cache never runs ahead of the engine's view.

mod cache;
mod events;
mod metrics;
pub mod provider;
pub mod task;

pub use cache::{CachedPolicy, CompoundData, PolicyCache, PolicyCacheInner};
pub use events::PolicyEvent;
use tempo_precompiles::tip403_registry::{ALLOW_ALL_POLICY_ID, REJECT_ALL_POLICY_ID};
pub use zone_primitives::policy::AuthRole;

/// Returns authorization result for built-in policies, `None` for user-created ones.
#[inline]
fn builtin_authorization(policy_id: u64) -> Option<bool> {
    match policy_id {
        ALLOW_ALL_POLICY_ID => Some(true),
        REJECT_ALL_POLICY_ID => Some(false),
        _ => None,
    }
}

pub use metrics::Tip403Metrics;
pub use provider::PolicyProvider;
pub use task::{
    PolicyResolutionTask, PolicyTaskHandle, PolicyTaskMessage, spawn_policy_resolution_task,
};

// Block-versioned policy sets for TIP-403 policy tracking.

use alloy_primitives::Address;
use std::collections::{BTreeMap, HashSet};

/// Block-versioned policy set for TIP-403 policy tracking.
///
/// Models a policy set as a baseline [`HashSet`] plus per-block deltas, matching the L1
/// event model where `WhitelistUpdated` and `BlacklistUpdated` events arrive as
/// `(address, add/remove)` updates per block.
///
/// Users not explicitly tracked are treated as "not in set", matching the L1 storage default
/// for `policy_set[policyId][user]`.
#[derive(Debug, Default)]
pub struct PolicySet {
    /// Addresses in the set at `baseline_height`.
    baseline: HashSet<Address>,
    /// Block height up to which the baseline is valid.
    baseline_height: u64,
    /// Per-block set updates above `baseline_height`.
    pending: BTreeMap<u64, Vec<PolicySetUpdate>>,
    /// All addresses for which we've ever recorded a set event. Survives `advance()` so
    /// we can distinguish "explicitly absent from the set" from "never observed by the subscriber".
    observed: HashSet<Address>,
}

impl PolicySet {
    /// Check if `user` is in the set at the given block height.
    ///
    /// Returns `false` for users with no recorded state, matching the L1 storage default.
    pub fn contains(&self, user: Address, block_number: u64) -> bool {
        if block_number <= self.baseline_height {
            return self.baseline.contains(&user);
        }

        // Scan pending blocks in reverse for the latest change affecting this user.
        for (_, updates) in self.pending.range(..=block_number).rev() {
            for update in updates.iter().rev() {
                if update.account == user {
                    return update.in_set;
                }
            }
        }

        self.baseline.contains(&user)
    }

    /// Returns `true` if we've ever recorded a set event for `user` (added or removed).
    ///
    /// When `false`, the caller should not trust [`contains`](Self::contains) returning `false`
    /// because the user may have been added before the subscriber started.
    pub fn is_known(&self, user: &Address) -> bool {
        self.observed.contains(user) || self.baseline.contains(user)
    }

    /// Record a set update at the given block height.
    ///
    /// Updates at or below the baseline height are ignored. The baseline represents finalized
    /// engine-consumed state and is only updated by [`advance`](Self::advance), which prevents
    /// delayed RPC fallback results from overwriting newer event-derived membership.
    pub fn record_status(&mut self, user: Address, block_number: u64, in_set: bool) {
        if block_number <= self.baseline_height {
            return;
        }

        self.observed.insert(user);
        self.pending
            .entry(block_number)
            .or_default()
            .push(PolicySetUpdate {
                account: user,
                in_set,
            });
    }

    /// Advance the baseline to `new_height`, folding pending deltas.
    pub fn advance(&mut self, new_height: u64) {
        if new_height <= self.baseline_height {
            return;
        }

        let to_apply: Vec<u64> = self.pending.range(..=new_height).map(|(k, _)| *k).collect();
        for block in to_apply {
            if let Some(updates) = self.pending.remove(&block) {
                for update in updates {
                    if update.in_set {
                        self.baseline.insert(update.account);
                    } else {
                        self.baseline.remove(&update.account);
                    }
                }
            }
        }

        self.baseline_height = new_height;
    }

    /// Equivalent to [`advance`](Self::advance).
    pub fn flatten(&mut self, min_block: u64) {
        self.advance(min_block);
    }

    /// Returns `true` if no set data has been recorded.
    pub fn is_empty(&self) -> bool {
        self.baseline.is_empty() && self.pending.is_empty()
    }

    /// Clears all set data and resets the baseline height.
    pub fn clear(&mut self) {
        self.baseline.clear();
        self.baseline_height = 0;
        self.pending.clear();
        self.observed.clear();
    }
}

/// A single set update within a block.
#[derive(Debug, Clone, Copy)]
pub(super) struct PolicySetUpdate {
    /// The address whose policy-set status changed.
    pub account: Address,
    /// Whether the address is in the policy set after this update.
    pub in_set: bool,
}
