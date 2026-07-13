//! Stateless TIP-403 policy evaluation over finalized raw Tempo L1 state.
//!
//! [`PolicyEvaluator`] owns no policy cache or RPC client. It executes the upstream Tempo TIP-20
//! and TIP-403 implementations against an [`L1StorageReader`], so EVM execution and encrypted
//! deposit preparation share the same raw state, hardfork, and policy semantics.

use alloy_primitives::Address;
use eyre::Result;
use tempo_precompiles::{
    tip20::TIP20Token,
    tip403_registry::{AuthRole as TempoAuthRole, TIP403Registry},
};
use zone_precompiles::L1StorageReader;

use super::{AuthRole, metrics::Tip403Metrics};

/// Stateless upstream policy evaluator backed by an anchored raw L1 reader.
#[derive(Clone)]
pub struct PolicyEvaluator<R> {
    reader: R,
    metrics: Tip403Metrics,
}

impl<R> std::fmt::Debug for PolicyEvaluator<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyEvaluator").finish_non_exhaustive()
    }
}

impl<R: L1StorageReader> PolicyEvaluator<R> {
    /// Create an evaluator over `reader`.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            metrics: Tip403Metrics::default(),
        }
    }

    /// Return evaluator-level authorization metrics.
    pub fn metrics(&self) -> &Tip403Metrics {
        &self.metrics
    }

    /// Evaluate a token policy against raw L1 state at `block_number`.
    ///
    /// The token policy ID and all TIP-403 state are read under one storage context using the
    /// Tempo hardfork active at the same anchor.
    pub fn is_authorized(
        &self,
        token: Address,
        user: Address,
        block_number: u64,
        role: AuthRole,
    ) -> Result<bool> {
        self.metrics.authorization_checks_total.increment(1);

        let role = tempo_auth_role(role);

        self.reader
            .with_read_only_provider(block_number, || {
                let policy_id = TIP20Token::from_address_unchecked(token).transfer_policy_id()?;
                TIP403Registry::new().is_authorized_as(policy_id, user, role)
            })
            .map_err(|err| {
                eyre::eyre!("TIP-403 evaluation failed at L1 block {block_number}: {err}")
            })
    }

    /// Async engine adapter for [`Self::is_authorized`].
    ///
    /// Upstream precompile storage is synchronous, so evaluation runs on Tokio's blocking pool
    /// instead of blocking an async engine worker.
    pub async fn is_authorized_async(
        &self,
        token: Address,
        user: Address,
        block_number: u64,
        role: AuthRole,
    ) -> Result<bool> {
        let evaluator = self.clone();
        tokio::task::spawn_blocking(move || {
            evaluator.is_authorized(token, user, block_number, role)
        })
        .await
        .map_err(|err| eyre::eyre!("TIP-403 evaluation task failed: {err}"))?
    }
}

fn tempo_auth_role(role: AuthRole) -> TempoAuthRole {
    match role {
        AuthRole::Transfer => TempoAuthRole::Transfer,
        AuthRole::Sender => TempoAuthRole::Sender,
        AuthRole::Recipient => TempoAuthRole::Recipient,
        AuthRole::MintRecipient => TempoAuthRole::MintRecipient,
    }
}
