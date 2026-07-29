//! Event-derived prefetching for the L1 storage read by `advanceTempo`.
//!
//! Deposit execution is sequential, but Portal events reveal most tokens and mint recipients before
//! payload construction. [`DepositPrefetchPlan`] first warms the directly derivable Portal slots,
//! then evaluates Tempo's canonical policy APIs in bounded dependency waves. Those APIs discover
//! the hardfork-sensitive TIP-20 and TIP-403 reads, avoiding a second, local implementation of
//! Tempo's policy storage layout.
//!
//! Portal reads use the shared [`L1StateProvider`] cache directly. Policy reads are delegated to a
//! [`PolicyCheckExecutor`] because they also require parent Zone state and a correctly configured
//! Tempo EVM. Both paths populate the same exact-child L1 cache used by payload execution.

use super::L1StateProvider;
use alloy_primitives::{Address, B256, U256};
use eyre::{Result, WrapErr as _};
use futures::{StreamExt as _, TryStreamExt as _};
use reth_primitives_traits::SealedHeader;
use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    sync::Arc,
    time::Instant,
};
use tempo_precompiles::{
    storage::{Slot, Storable},
    zone_factory::ZonePortalStorage,
};
use tempo_primitives::{TempoAddressExt as _, TempoHeader};
use tracing::{info, warn};

/// Zone and L1 block context shared by every canonical check in one prefetch operation.
///
/// An executor uses these values to open the parent Zone state, construct the child block
/// environment, and select the exact L1 anchor whose storage payload execution will observe.
#[derive(Debug, Clone)]
pub struct PrefetchCtx {
    /// Sealed Zone parent whose state backs each throwaway EVM.
    pub parent: SealedHeader<TempoHeader>,
    /// Exact child L1 block being prepared for `advanceTempo`.
    pub target_l1_block: u64,
    /// Child Zone block timestamp in whole seconds.
    pub timestamp: u64,
    /// Millisecond remainder paired with [`timestamp`](Self::timestamp).
    pub timestamp_millis_part: u64,
}

/// Executes synchronous canonical Tempo policy reads against a Zone parent state.
///
/// [`DepositPrefetchPlan`] owns dependency planning and bounded blocking-task scheduling; an
/// implementation only supplies the node-specific EVM execution needed for one check.
pub trait PolicyCheckExecutor: Debug + Send + Sync {
    /// Resolve the transfer policy currently governing `token`.
    fn transfer_policy(&self, ctx: &PrefetchCtx, token: Address) -> Result<u64>;

    /// Return whether `recipient` is authorized as a mint recipient by `policy_id`.
    fn is_mint_authorized(
        &self,
        ctx: &PrefetchCtx,
        policy_id: u64,
        recipient: Address,
    ) -> Result<bool>;

    /// Validate receipt of `token` from the Zone inbox by `recipient`.
    fn validate_receive_policy(
        &self,
        ctx: &PrefetchCtx,
        token: Address,
        recipient: Address,
    ) -> Result<()>;
}

/// Event-derived inputs needed to warm one finalized L1 block's deposit state.
///
/// Tokens from both enablement and deposit events share one deduplicated set because canonical
/// Tempo execution resolves both through the same transfer-policy API. Mint pairs retain only
/// recipients known before Zone execution; virtual recipients require Zone-local resolution and
/// therefore cannot have recipient-specific policy reads scheduled here. Encryption-key indices
/// remain symbolic until Portal slots are derived at prefetch time.
#[derive(Debug)]
pub struct DepositPrefetchPlan {
    block_number: u64,
    portal: Address,
    tokens: HashSet<Address>,
    mints: HashSet<(Address, Address)>,
    encryption_key_indices: HashSet<U256>,
}

impl DepositPrefetchPlan {
    pub(crate) fn new(block_number: u64, portal: Address) -> Self {
        Self {
            block_number,
            portal,
            tokens: HashSet::new(),
            mints: HashSet::new(),
            encryption_key_indices: HashSet::new(),
        }
    }

    pub(crate) fn add_token(&mut self, token: Address) {
        self.tokens.insert(token);
    }

    pub(crate) fn add_mint(&mut self, token: Address, recipient: Address) {
        // T9 and T6 both follow T3, where these recipients fail validation before policy reads.
        if recipient.is_zero() || recipient.is_tip20() {
            return;
        }
        self.tokens.insert(token);
        // Virtual recipients require Zone-local state to resolve their effective recipient. Their
        // token-derived policy reads are still prefetched.
        if !recipient.is_virtual() {
            self.mints.insert((token, recipient));
        }
    }

    pub(crate) fn add_encryption_key(&mut self, key_index: U256) {
        self.encryption_key_indices.insert(key_index);
    }

    #[cfg(test)]
    pub(crate) fn plans_token(&self, token: Address) -> bool {
        self.tokens.contains(&token)
    }

    #[cfg(test)]
    pub(crate) fn plans_encryption_key(&self, key_index: U256) -> bool {
        self.encryption_key_indices.contains(&key_index)
    }

    /// Warm all event-derived Portal and policy reads before payload construction.
    ///
    /// Work proceeds in four bounded phases:
    /// 1. directly derivable Portal queue and encryption-key slots;
    /// 2. one canonical transfer-policy lookup per token;
    /// 3. one mint authorization per unique `(policy_id, recipient)` pair;
    /// 4. receive-policy validation for every authorized `(token, recipient)` mint.
    ///
    /// Each wave is deduplicated and concurrent. Starting payload construction only after this
    /// method succeeds keeps unavoidable RPC latency outside sequential EVM execution.
    pub async fn prefetch<E: PolicyCheckExecutor + ?Sized + 'static>(
        &self,
        provider: &L1StateProvider,
        concurrency: usize,
        ctx: PrefetchCtx,
        executor: Arc<E>,
    ) -> Result<()> {
        let started = Instant::now();
        let l1_block = self.block_number;
        let portal_slots = self.prefetch_portal(provider, concurrency).await
            .inspect_err(|error| warn!(target: "zone::l1", %error, "failed to prefetch portal L1 state at block {l1_block}")).unwrap_or(0);
        if let Err(error) = self.prefetch_policies(concurrency, ctx, executor).await {
            warn!(target: "zone::l1", %error, "failed to prefetch policy L1 state at block {l1_block}");
        };
        info!(
            target: "zone::l1",
            l1_block,
            portal_slots,
            tokens = self.tokens.len(),
            mints = self.mints.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Prefetched deposit L1 state"
        );
        Ok(())
    }

    async fn prefetch_portal(
        &self,
        provider: &L1StateProvider,
        concurrency: usize,
    ) -> Result<usize> {
        let mut slots = HashSet::new();
        if !self.portal.is_zero() {
            let portal = ZonePortalStorage::new(self.portal);
            insert_slot(&mut slots, &portal.current_deposit_queue_hash);
            for &key_index in &self.encryption_key_indices {
                let index = usize::try_from(key_index)
                    .wrap_err_with(|| "encryption key index exceeds usize".to_string())?;
                let key = &portal.encryption_keys[index];
                insert_slot(&mut slots, &key.x);
                insert_slot(&mut slots, &key.y_parity); // packed with `activation_block`
            }
        }
        let slot_count = slots.len();
        provider
            .prefetch_storage(slots, self.block_number, concurrency)
            .await?;
        Ok(slot_count)
    }

    async fn prefetch_policies<E>(
        &self,
        concurrency: usize,
        ctx: PrefetchCtx,
        executor: Arc<E>,
    ) -> Result<()>
    where
        E: PolicyCheckExecutor + ?Sized + 'static,
    {
        // Wave 1: Fetch policy ids
        let policies: HashMap<Address, u64> = Self::prefetch_wave(
            concurrency,
            ctx.clone(),
            executor.clone(),
            self.tokens.iter().copied(),
            |exec, ctx, token| exec.transfer_policy(ctx, token).map(|id| (token, id)),
        )
        .try_collect()
        .await?;

        // Group tokens by policy id, and track their recipients.
        let mut tokens = HashMap::<(u64, Address), Vec<Address>>::with_capacity(self.mints.len());
        self.mints
            .iter()
            .filter_map(|(token, recipient)| Some(((*policies.get(token)?, *recipient), *token)))
            .for_each(|(key, token)| tokens.entry(key).or_default().push(token));

        // Wave 2: Fetch authorized mints
        let authorized_mints = Self::prefetch_wave(
            concurrency,
            ctx.clone(),
            executor.clone(),
            tokens,
            |exec, ctx, ((policy_id, recipient), tokens)| {
                exec.is_mint_authorized(ctx, policy_id, recipient)
                    .map(|is_auth| (recipient, tokens, is_auth))
            },
        ) // Only keep tokens where minting is authorized
        .try_fold(Vec::new(), |mut acc, (recp, tkns, is_auth)| async move {
            if is_auth {
                acc.extend(tkns.into_iter().map(|tkn| (tkn, recp)));
            }
            Ok(acc)
        })
        .await?;

        // Wave 3: Fetch receive policies on authorized minters
        Self::prefetch_wave(
            concurrency,
            ctx,
            executor,
            authorized_mints,
            |exec, ctx, (token, recipient)| exec.validate_receive_policy(ctx, token, recipient),
        )
        .try_for_each(|_| async { Ok(()) })
        .await
    }

    /// Execute one homogeneous dependency wave in bounded blocking tasks.
    ///
    /// Policy APIs are synchronous because they run inside an EVM context. Moving each operation
    /// to `spawn_blocking` keeps RPC fallback and EVM work off the async engine worker.
    fn prefetch_wave<E, I, O>(
        concurrency: usize,
        ctx: PrefetchCtx,
        executor: Arc<E>,
        checks: impl IntoIterator<Item = I, IntoIter: Send>,
        run_check: fn(&E, &PrefetchCtx, I) -> Result<O>,
    ) -> impl futures::Stream<Item = Result<O>>
    where
        E: PolicyCheckExecutor + ?Sized + 'static,
        I: Send + 'static,
        O: Send + 'static,
    {
        futures::stream::iter(checks)
            .map(move |check| {
                let (executor, ctx) = (executor.clone(), ctx.clone());
                async move {
                    tokio::task::spawn_blocking(move || run_check(&executor, &ctx, check))
                        .await
                        .wrap_err("deposit policy prewarm task panicked")?
                }
            })
            .buffer_unordered(concurrency.max(1))
    }
}

/// Add every raw word occupied by a generated typed storage value.
fn insert_slot<T: Storable>(slots: &mut HashSet<(Address, B256)>, slot: &Slot<T>) {
    for offset in 0..T::SLOTS {
        slots.insert((slot.address(), (slot.slot() + U256::from(offset)).into()));
    }
}
