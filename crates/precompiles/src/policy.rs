//! Policy authorization trait for zone precompiles.
//!
//! Defines [`PolicyCheck`], an abstraction over the concrete `PolicyProvider`
//! so that the policy/token precompiles in this crate don't depend on tokio,
//! alloy providers, or any std-only infrastructure.

use alloy_primitives::Address;
use revm::precompile::PrecompileError;
use tempo_contracts::precompiles::ITIP403Registry::{BlockedReason, PolicyType};
use zone_primitives::policy::AuthRole;

/// Cached TIP-1028 receive-policy configuration for one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceivePolicy {
    pub has_receive_policy: bool,
    pub sender_policy_id: u64,
    pub sender_policy_type: PolicyType,
    pub token_filter_id: u64,
    pub token_filter_type: PolicyType,
    pub recovery_authority: Address,
}

impl ReceivePolicy {
    /// Default L1 receive-policy view for an account with no configured policy.
    pub const fn none() -> Self {
        Self {
            has_receive_policy: false,
            sender_policy_id: 0,
            sender_policy_type: PolicyType::WHITELIST,
            token_filter_id: 0,
            token_filter_type: PolicyType::WHITELIST,
            recovery_authority: Address::ZERO,
        }
    }
}

/// Result of applying an account receive policy to an inbound transfer or mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceivePolicyDecision {
    Authorized,
    Blocked {
        reason: BlockedReason,
        recovery_authority: Address,
    },
}

impl ReceivePolicyDecision {
    /// Returns the `validateReceivePolicy` ABI fields.
    pub const fn as_validate_return(self) -> (bool, BlockedReason) {
        match self {
            Self::Authorized => (true, BlockedReason::NONE),
            Self::Blocked { reason, .. } => (false, reason),
        }
    }
}

/// Authorization provider used by the TIP-403 proxy and zone TIP-20 precompiles.
///
/// Implementors resolve policy queries — either from an in-memory cache with
/// RPC fallback (zone node) or from a witness database (SP1 prover guest).
pub trait PolicyCheck {
    /// Check whether `user` is authorized under `policy_id` for the given `role`.
    fn is_authorized(
        &self,
        policy_id: u64,
        user: Address,
        role: AuthRole,
    ) -> Result<bool, PrecompileError>;

    /// Resolve the `transferPolicyId` for a token.
    fn resolve_transfer_policy_id(&self, token: Address) -> Result<u64, PrecompileError>;

    /// Resolve policy type and admin for a policy ID.
    ///
    /// Returns `Ok(Some((policy_type, admin)))` if the policy exists, `Ok(None)` otherwise.
    fn policy_type_sync(
        &self,
        policy_id: u64,
    ) -> Result<tempo_contracts::precompiles::ITIP403Registry::PolicyType, PrecompileError>;

    /// Resolve compound policy sub-IDs.
    ///
    /// Returns `(sender_policy_id, recipient_policy_id, mint_recipient_policy_id)`.
    fn compound_policy_data(&self, policy_id: u64) -> Result<(u64, u64, u64), PrecompileError>;

    /// Check whether a policy exists.
    fn policy_exists(&self, policy_id: u64) -> Result<bool, PrecompileError>;

    /// Return the highest known policy ID counter.
    fn policy_id_counter(&self) -> u64;

    /// Return an account's TIP-1028 receive-policy configuration.
    fn receive_policy(&self, account: Address) -> Result<ReceivePolicy, PrecompileError>;

    /// Validate an inbound transfer or mint against the receiver's TIP-1028 policy.
    fn validate_receive_policy(
        &self,
        token: Address,
        sender: Address,
        receiver: Address,
    ) -> Result<ReceivePolicyDecision, PrecompileError>;
}
