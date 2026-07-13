//! TIP-403 state ingestion and raw L1 policy evaluation.
//!
//! [`PolicyEvaluator`] executes upstream Tempo TIP-20/TIP-403 logic against the same raw,
//! block-versioned L1 reader used by Zone EVM precompiles. The legacy [`PolicyCache`] remains
//! temporarily for subscriber token discovery and event compatibility, but it is not used for
//! encrypted-deposit authorization.

mod cache;
mod evaluator;
mod events;
mod metrics;
mod policy_set;

pub use cache::{CachedPolicy, CompoundData, PolicyCache, PolicyCacheInner};
pub use events::PolicyEvent;
pub use policy_set::PolicySet;
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

pub use evaluator::PolicyEvaluator;
pub use metrics::Tip403Metrics;
