//! Event-derived prefetching for the L1 storage read by `advanceTempo`.
//!
//! Deposit execution is sequential, but Portal events reveal most tokens and recipients before
//! payload construction starts. This module uses that data to prefetch typed Portal slots and run
//! canonical policy checks in bounded waves, so the payload builder normally observes exact-L1
//! cache hits instead of serial RPC calls.

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

/// Symbolic L1 reads derived from one finalized block's Portal events.
///
/// The plan retains tokens, known mint recipients, and encryption-key indices. At prefetch time it
/// derives typed Portal slots and runs canonical policy checks to discover dependent reads. Virtual
/// recipients still receive token-level prefetching, but their recipient-specific checks require
/// Zone-local resolution and cannot be scheduled here.
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
    /// Work is split into dependency waves:
    /// 1. Portal slots and transfer-policy lookup per token.
    /// 2. Mint authorizations for each unique `(policy_id, recipient)` pair.
    /// 3. Receive-policy validation for every authorized `(token, recipient)` mint.
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

        // Wave 1: Fetch Portal slots and transfer policy ids
        let shared_concurrency = (concurrency / 2).max(1);
        let (portal_result, policies_result) = tokio::join!(
            async {
                let slots = self.portal_slots()?;
                let slot_count = slots.len();
                provider
                    .prefetch_storage(slots, self.block_number, shared_concurrency)
                    .await?;
                Ok::<_, eyre::Report>(slot_count)
            },
            self.prefetch_wave(
                shared_concurrency,
                ctx.clone(),
                executor.clone(),
                self.tokens.iter().copied(),
                |exec, ctx, token| exec.transfer_policy(ctx, token).map(|id| (token, id)),
            )
            .try_collect::<HashMap<_, _>>()
        );
        let portal_slots = portal_result
            .inspect_err(|error| warn!(target: "zone::l1", %error, "failed to prefetch portal L1 state at block {l1_block}"))
            .unwrap_or(0);
        let policies = policies_result?;

        // Group tokens by policy id, and track their recipients.
        let mut tokens = HashMap::<(u64, Address), Vec<Address>>::with_capacity(self.mints.len());
        self.mints
            .iter()
            .filter_map(|(token, to)| Some(((*policies.get(token)?, *to), *token)))
            .for_each(|(key, token)| tokens.entry(key).or_default().push(token));

        // Wave 2: Fetch authorized mints
        let authorized_mints = self
            .prefetch_wave(
                concurrency,
                ctx.clone(),
                executor.clone(),
                tokens,
                |exec, ctx, ((policy_id, to), tokens)| {
                    exec.is_mint_authorized(ctx, policy_id, to)
                        .map(|is_auth| (to, tokens, is_auth))
                },
            ) // Only keep tokens where minting is authorized
            .try_fold(Vec::new(), |mut acc, (to, tokens, is_auth)| async move {
                if is_auth {
                    acc.extend(tokens.into_iter().map(|token| (token, to)));
                }
                Ok(acc)
            })
            .await?;

        // Wave 3: Fetch receive policies on authorized minters
        self.prefetch_wave(
            concurrency,
            ctx,
            executor,
            authorized_mints,
            |exec, ctx, (token, to)| exec.validate_receive_policy(ctx, token, to),
        )
        .try_for_each(|_| async { Ok(()) })
        .await?;

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

    #[inline]
    fn portal_slots(&self) -> Result<HashSet<(Address, B256)>> {
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
        Ok(slots)
    }

    /// Execute one homogeneous dependency wave in bounded blocking tasks.
    ///
    /// Policy APIs are synchronous because they run inside an EVM context. Moving each operation
    /// to `spawn_blocking` keeps RPC fallback and EVM work off the async engine worker.
    fn prefetch_wave<E, I, O>(
        &self,
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
        let l1_block = self.block_number;
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
            .inspect_err(move |error| warn!(target: "zone::l1", %error, "failed to prefetch policy L1 state at block {l1_block}"))
    }
}

/// Add every raw word occupied by a generated typed storage value.
fn insert_slot<T: Storable>(slots: &mut HashSet<(Address, B256)>, slot: &Slot<T>) {
    for offset in 0..T::SLOTS {
        slots.insert((slot.address(), (slot.slot() + U256::from(offset)).into()));
    }
}

#[cfg(test)]
mod tests {
    use crate::{L1StateCache, state::L1StateProviderConfig};

    use super::*;
    use alloy_primitives::address;
    use alloy_provider::{Provider as _, ProviderBuilder};
    use std::sync::Mutex;
    use tempo_alloy::TempoNetwork;

    const PORTAL: Address = address!("1000000000000000000000000000000000000001");
    const TOKEN_A: Address = address!("20c0000000000000000000000000000000000001");
    const TOKEN_B: Address = address!("20c0000000000000000000000000000000000002");
    const TOKEN_C: Address = address!("20c0000000000000000000000000000000000003");
    const TOKEN_D: Address = address!("20c0000000000000000000000000000000000004");
    const TOKEN_E: Address = address!("20c0000000000000000000000000000000000005");
    const RECIPIENT: Address = address!("3000000000000000000000000000000000000001");
    const DENIED: Address = address!("3000000000000000000000000000000000000002");
    const VIRTUAL_RECIPIENT: Address = address!("01020304fdfdfdfdfdfdfdfdfdfd010203040506");

    #[derive(Debug, Default)]
    struct MockExecutor {
        policy_calls: Mutex<Vec<Address>>,
        authorization_calls: Mutex<Vec<(u64, Address)>>,
        receive_calls: Mutex<Vec<(Address, Address)>>,
    }

    impl PolicyCheckExecutor for MockExecutor {
        fn transfer_policy(&self, _ctx: &PrefetchCtx, token: Address) -> Result<u64> {
            self.policy_calls.lock().unwrap().push(token);
            Ok(if token == TOKEN_C { 8 } else { 7 })
        }

        fn is_mint_authorized(
            &self,
            _ctx: &PrefetchCtx,
            policy_id: u64,
            recipient: Address,
        ) -> Result<bool> {
            self.authorization_calls
                .lock()
                .unwrap()
                .push((policy_id, recipient));
            Ok(recipient != DENIED)
        }

        fn validate_receive_policy(
            &self,
            _ctx: &PrefetchCtx,
            token: Address,
            recipient: Address,
        ) -> Result<()> {
            self.receive_calls.lock().unwrap().push((token, recipient));
            Ok(())
        }
    }

    fn ctx() -> PrefetchCtx {
        PrefetchCtx {
            parent: SealedHeader::seal_slow(TempoHeader::default()),
            target_l1_block: 7,
            timestamp: 1,
            timestamp_millis_part: 0,
        }
    }

    #[test]
    fn portal_slots_use_typed_layout_and_deduplicate_keys() -> Result<()> {
        let mut plan = DepositPrefetchPlan::new(7, PORTAL);
        plan.add_encryption_key(U256::from(4));
        plan.add_encryption_key(U256::from(4));

        let slots = plan.portal_slots()?;
        let portal = ZonePortalStorage::new(PORTAL);
        let key = &portal.encryption_keys[4];
        assert_eq!(slots.len(), 3, "queue plus two encryption-key words");
        for slot in [&portal.current_deposit_queue_hash, &key.x] {
            assert!(slots.contains(&(slot.address(), slot.slot().into())));
        }
        assert!(slots.contains(&(key.y_parity.address(), key.y_parity.slot().into())));
        Ok(())
    }

    #[tokio::test]
    async fn policy_waves_deduplicate_and_gate_receive_checks() -> Result<()> {
        let mut plan = DepositPrefetchPlan::new(7, Address::ZERO);
        plan.add_mint(TOKEN_A, RECIPIENT);
        plan.add_mint(TOKEN_A, RECIPIENT);
        plan.add_mint(TOKEN_B, RECIPIENT);
        plan.add_mint(TOKEN_C, DENIED);
        plan.add_mint(TOKEN_D, VIRTUAL_RECIPIENT);
        plan.add_mint(TOKEN_E, Address::ZERO);
        plan.add_mint(TOKEN_E, TOKEN_A);

        let rpc = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect("http://127.0.0.1:1")
            .await?
            .erased();
        let provider = L1StateProvider::new_raw(
            L1StateProviderConfig::default(),
            L1StateCache::default(),
            rpc,
            tokio::runtime::Handle::current(),
        );
        let executor = Arc::new(MockExecutor::default());
        plan.prefetch(&provider, 2, ctx(), executor.clone()).await?;

        let policy_calls = executor.policy_calls.lock().unwrap();
        assert_eq!(policy_calls.len(), 4, "each planned token is resolved once");
        assert_eq!(
            policy_calls.iter().copied().collect::<HashSet<_>>(),
            HashSet::from([TOKEN_A, TOKEN_B, TOKEN_C, TOKEN_D]),
            "virtual recipients retain token-level reads, while invalid recipients are discarded"
        );
        drop(policy_calls);
        let authorization_calls = executor.authorization_calls.lock().unwrap();
        assert_eq!(authorization_calls.len(), 2);
        assert_eq!(
            authorization_calls.iter().copied().collect::<HashSet<_>>(),
            HashSet::from([(7, RECIPIENT), (8, DENIED)]),
            "tokens sharing a policy and recipient use one authorization check"
        );
        drop(authorization_calls);
        let receive_calls = executor.receive_calls.lock().unwrap();
        assert_eq!(receive_calls.len(), 2);
        assert_eq!(
            receive_calls.iter().copied().collect::<HashSet<_>>(),
            HashSet::from([(TOKEN_A, RECIPIENT), (TOKEN_B, RECIPIENT)]),
            "only authorized mints reach receive-policy validation"
        );
        Ok(())
    }
}
