//! Event-derived prefetching for the L1 storage read by `advanceTempo`.
//!
//! Deposit execution is sequential, but the regular and decrypted deposit recipients are known
//! before payload construction starts. This module turns that data into bounded waves of exact-L1
//! storage reads so the payload builder normally observes cache hits instead of serial RPC calls.

use super::{L1StateProvider, provider::StorageSlot};
use alloy_primitives::{Address, B256, U256};
use eyre::{Result, WrapErr as _};
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};
use tempo_precompiles::{
    error::TempoPrecompileError,
    receive_policy_guard::RecoveryMode,
    storage::{Handler, Storable, StorageOps},
    tip403_registry::{ALLOW_ALL_POLICY_ID, PolicyType, REJECT_ALL_POLICY_ID, TIP403Registry},
    zone_factory::ZonePortalStorage,
};
use tempo_primitives::TempoAddressExt as _;
use tempo_zone_contracts::ZONE_INBOX_ADDRESS;
use tracing::info;

/// Immutable raw words returned by exactly one completed prefetch wave.
///
/// Values are decoded through their generated typed slots. Reads never fall back to the provider
/// cache, so an incomplete dependency wave fails visibly instead of silently decoding as zero.
struct StorageValues(HashMap<StorageSlot, B256>);

impl StorageValues {
    fn read<T: Storable>(&self, handler: &impl Handler<T>) -> Result<T> {
        let slot = handler.as_slot();
        let storage = AccountValues {
            address: slot.address(),
            words: &self.0,
        };
        T::load(&storage, slot.slot(), slot.ctx()).map_err(|error| {
            let StorageSlot(address, raw_slot) = (slot.address(), slot.slot()).into();
            eyre::eyre!(
                "failed to decode {} from prefetched storage at {address}[{raw_slot}]: {error}",
                std::any::type_name::<T>(),
            )
        })
    }
}

/// Read-only [`StorageOps`] view of one account in a wave's fetched values.
///
/// [`StorageOps::load`] receives only a slot number, so typed decoding binds the slot's account in
/// this adapter before calling [`Storable::load`].
struct AccountValues<'a> {
    address: Address,
    words: &'a HashMap<StorageSlot, B256>,
}

impl StorageOps for AccountValues<'_> {
    fn load(&self, slot: U256) -> tempo_precompiles::Result<U256> {
        let raw_slot = B256::from(slot.to_be_bytes::<32>());
        self.words
            .get(&StorageSlot(self.address, raw_slot))
            .copied()
            .map(Into::into)
            .ok_or_else(|| {
                TempoPrecompileError::Fatal(format!(
                    "prefetch wave is missing storage word {}[{raw_slot}]",
                    self.address
                ))
            })
    }

    fn store(&mut self, slot: U256, _value: U256) -> tempo_precompiles::Result<()> {
        Err(TempoPrecompileError::Fatal(format!(
            "attempted to write through read-only prefetched storage at {}[{}]",
            self.address, slot
        )))
    }
}

/// Deduplicated raw storage words required by one value-dependent prefetch wave.
///
/// Inserting a typed handler expands every contiguous word described by [`Storable::SLOTS`].
/// Packed fields naturally collapse to one [`StorageSlot`] through the underlying [`HashSet`].
#[derive(Debug, Default, PartialEq, Eq)]
struct PrefetchSlots(HashSet<StorageSlot>);

impl PrefetchSlots {
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Schedule every raw word occupied by a typed value.
    fn insert<T: Storable>(&mut self, handler: &impl Handler<T>) {
        let slot = handler.as_slot();
        for offset in 0..T::SLOTS {
            self.0
                .insert((slot.address(), slot.slot() + U256::from(offset)).into());
        }
    }
}

/// Hardfork-sensitive controls for deposit storage prefetching.
#[derive(Debug, Clone, Copy)]
pub struct DepositPrefetchConfig {
    registry_token_policies: bool,
    concurrency: usize,
}

impl DepositPrefetchConfig {
    /// Create a configuration for the hardforks active in the zone block being prepared and the
    /// operator's maximum number of concurrent L1 requests.
    pub const fn new(registry_token_policies: bool, concurrency: usize) -> Self {
        Self {
            registry_token_policies,
            concurrency,
        }
    }

    #[cfg(test)]
    const fn all() -> Self {
        Self::new(true, 4)
    }
}

/// One deposit mint whose effective recipient is derivable before payload construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DepositMint {
    token: Address,
    recipient: Address,
}

/// Symbolic L1 reads derivable from one finalized block's Portal events.
///
/// The plan deliberately retains tokens, recipients, and encryption-key indices rather than raw
/// slots alone. TIP-403 token bindings and receive-policy configurations determine additional
/// slots, so [`prefetch`](Self::prefetch) resolves those dependencies in bounded waves.
///
/// Virtual recipients and withdrawal bounce-backs require Zone-local state to resolve their
/// effective mint recipient. Prefetching still follows their token policy, but cannot derive the
/// final policy-membership or receive-policy slots. Likewise, an unset T9 token binding falls back
/// to a Zone-local legacy policy ID, so only the event-derived binding itself can be prefetched.
#[derive(Debug)]
pub struct DepositPrefetchPlan {
    block_number: u64,
    portal: Address,
    enabled_tokens: HashSet<Address>,
    deposit_tokens: HashSet<Address>,
    mints: HashSet<DepositMint>,
    encryption_key_indices: HashSet<U256>,
}

impl DepositPrefetchPlan {
    pub(crate) fn new(block_number: u64, portal: Address) -> Self {
        Self {
            block_number,
            portal,
            enabled_tokens: HashSet::new(),
            deposit_tokens: HashSet::new(),
            mints: HashSet::new(),
            encryption_key_indices: HashSet::new(),
        }
    }

    pub(crate) fn add_enabled_token(&mut self, token: Address) {
        self.enabled_tokens.insert(token);
    }

    pub(crate) fn add_deposit_token(&mut self, token: Address) {
        self.deposit_tokens.insert(token);
    }

    pub(crate) fn add_mint(&mut self, token: Address, recipient: Address) {
        // Zones start at T7, after T3 made these recipients fail validation before policy reads.
        if recipient.is_zero() || recipient.is_tip20() {
            return;
        }
        self.deposit_tokens.insert(token);
        // Virtual recipients require Zone-local state to resolve their effective recipient. Their
        // token-derived policy reads are still prefetched.
        if !recipient.is_virtual() {
            self.mints.insert(DepositMint { token, recipient });
        }
    }

    pub(crate) fn add_encryption_key(&mut self, key_index: U256) {
        self.encryption_key_indices.insert(key_index);
    }

    #[cfg(test)]
    pub(crate) fn plans_deposit_token(&self, token: Address) -> bool {
        self.deposit_tokens.contains(&token)
    }

    #[cfg(test)]
    pub(crate) fn plans_encryption_key(&self, key_index: U256) -> bool {
        self.encryption_key_indices.contains(&key_index)
    }

    /// Prefetch every exact-child L1 slot derivable from the plan.
    ///
    /// Reads are split into dependency waves:
    /// 1. Portal slots, token-policy bindings, and receive-policy configurations.
    /// 2. Mint-policy base records and receive-policy memberships.
    /// 3. Simple-policy memberships or compound-policy records.
    /// 4. Compound mint sub-policy records and memberships.
    ///
    /// Each wave is deduplicated and concurrent. Starting payload construction only after this
    /// method succeeds keeps unavoidable RPC latency outside sequential EVM execution.
    pub async fn prefetch(
        &self,
        provider: &L1StateProvider,
        config: DepositPrefetchConfig,
    ) -> Result<()> {
        let started = Instant::now();
        let mut all_slots = HashSet::new();

        let root_slots = self.root_slots(config)?;
        let roots = self
            .prefetch_wave(provider, root_slots, config.concurrency, &mut all_slots)
            .await?;

        let token_policies = self.registered_token_policies(&roots, config)?;
        let policy_and_receive_slots = self.policy_and_receive_slots(&roots, &token_policies)?;
        let policy_and_receive = self
            .prefetch_wave(
                provider,
                policy_and_receive_slots,
                config.concurrency,
                &mut all_slots,
            )
            .await?;

        let mint_policy_slots = self.mint_policy_slots(&token_policies, &policy_and_receive)?;
        let mint_policies = self
            .prefetch_wave(
                provider,
                mint_policy_slots,
                config.concurrency,
                &mut all_slots,
            )
            .await?;

        let compound_subpolicy_slots =
            self.compound_subpolicy_slots(&token_policies, &policy_and_receive, &mint_policies)?;
        self.prefetch_wave(
            provider,
            compound_subpolicy_slots,
            config.concurrency,
            &mut all_slots,
        )
        .await?;

        info!(
            target: "zone::l1",
            l1_block = self.block_number,
            event_derived_mints = self.mints.len(),
            unique_slots = all_slots.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Prefetched deposit L1 state"
        );
        Ok(())
    }

    async fn prefetch_wave(
        &self,
        provider: &L1StateProvider,
        slots: PrefetchSlots,
        concurrency: usize,
        all_slots: &mut HashSet<StorageSlot>,
    ) -> Result<StorageValues> {
        all_slots.extend(slots.0.iter().copied());
        provider
            .prefetch_storage(slots.0, self.block_number, concurrency)
            .await
            .map(StorageValues)
            .wrap_err_with(|| {
                format!(
                    "failed to prefetch deposit L1 state at block {}",
                    self.block_number
                )
            })
    }

    fn root_slots(&self, config: DepositPrefetchConfig) -> Result<PrefetchSlots> {
        let registry = TIP403Registry::new();
        let mut wave = PrefetchSlots::default();
        if !self.portal.is_zero() {
            let portal = ZonePortalStorage::new(self.portal);
            wave.insert(&portal.current_deposit_queue_hash);
            for &index in &self.encryption_key_indices {
                let index = usize::try_from(index).wrap_err_with(|| {
                    format!("Portal encryption key index exceeds usize: {index}")
                })?;
                wave.insert(&portal.encryption_keys[index]);
            }
        }
        for &token in &self.enabled_tokens {
            wave.insert(&registry.token_transfer_policies[token]);
        }
        if config.registry_token_policies {
            for &token in &self.deposit_tokens {
                wave.insert(&registry.token_transfer_policies[token]);
            }
        }
        for mint in &self.mints {
            wave.insert(&registry.receive_policies[mint.recipient].config);
        }
        Ok(wave)
    }

    fn registered_token_policies(
        &self,
        roots: &StorageValues,
        config: DepositPrefetchConfig,
    ) -> Result<HashMap<Address, u64>> {
        if !config.registry_token_policies {
            return Ok(HashMap::new());
        }
        let mut result = HashMap::new();
        let registry = TIP403Registry::new();
        for &token in &self.deposit_tokens {
            let transfer_policy = roots.read(&registry.token_transfer_policies[token])?;
            if transfer_policy.is_set {
                result.insert(token, transfer_policy.policy_id);
            }
        }
        Ok(result)
    }

    fn policy_and_receive_slots(
        &self,
        roots: &StorageValues,
        token_policies: &HashMap<Address, u64>,
    ) -> Result<PrefetchSlots> {
        let registry = TIP403Registry::new();
        let mut slots = PrefetchSlots::default();
        if token_policies.values().any(|id| !is_builtin_policy(*id)) {
            slots.insert(&registry.policy_id_counter);
            for &id in token_policies
                .values()
                .filter(|id| !is_builtin_policy(**id))
            {
                slots.insert(&registry.policy_records[id].base);
            }
        }
        for mint in &self.mints {
            let receive = &registry.receive_policies[mint.recipient];
            let config = roots.read(&receive.config)?;
            if !config.has_receive_policy {
                continue;
            }
            if !is_builtin_policy(config.token_filter_id) {
                slots.insert(&registry.policy_set[config.token_filter_id][mint.token]);
            }
            if !is_builtin_policy(config.sender_policy_id) {
                slots.insert(&registry.policy_set[config.sender_policy_id][ZONE_INBOX_ADDRESS]);
            }
            if config.recovery_mode == RecoveryMode::ThirdParty {
                slots.insert(&receive.recovery_address);
            }
        }
        Ok(slots)
    }

    fn mint_policy_slots(
        &self,
        token_policies: &HashMap<Address, u64>,
        values: &StorageValues,
    ) -> Result<PrefetchSlots> {
        let registry = TIP403Registry::new();
        let mut slots = PrefetchSlots::default();
        for (&token, &id) in token_policies {
            if is_builtin_policy(id) {
                continue;
            }
            if values.read(&registry.policy_records[id].base)?.policy_type
                == PolicyType::COMPOUND as u8
            {
                slots.insert(&registry.policy_records[id].compound);
            } else {
                for mint in self.mints.iter().filter(|mint| mint.token == token) {
                    slots.insert(&registry.policy_set[id][mint.recipient]);
                }
            }
        }
        Ok(slots)
    }

    fn compound_subpolicy_slots(
        &self,
        token_policies: &HashMap<Address, u64>,
        policies: &StorageValues,
        compounds: &StorageValues,
    ) -> Result<PrefetchSlots> {
        let registry = TIP403Registry::new();
        let mut slots = PrefetchSlots::default();
        for (&token, &id) in token_policies {
            if is_builtin_policy(id)
                || policies
                    .read(&registry.policy_records[id].base)?
                    .policy_type
                    != PolicyType::COMPOUND as u8
            {
                continue;
            }
            let mint_id = compounds
                .read(&registry.policy_records[id].compound)?
                .mint_recipient_policy_id;
            if !is_builtin_policy(mint_id) {
                slots.insert(&registry.policy_records[mint_id].base);
                for mint in self.mints.iter().filter(|mint| mint.token == token) {
                    slots.insert(&registry.policy_set[mint_id][mint.recipient]);
                }
            }
        }
        Ok(slots)
    }
}

fn is_builtin_policy(id: u64) -> bool {
    matches!(id, REJECT_ALL_POLICY_ID | ALLOW_ALL_POLICY_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    const PORTAL: Address = address!("1000000000000000000000000000000000000001");
    const TOKEN: Address = address!("20c0000000000000000000000000000000000001");
    const ENABLED_TOKEN: Address = address!("20c0000000000000000000000000000000000002");
    const RECIPIENT: Address = address!("3000000000000000000000000000000000000001");
    const VIRTUAL_RECIPIENT: Address = address!("01020304fdfdfdfdfdfdfdfdfdfd010203040506");

    fn storage_slot<T: Storable>(handler: &impl Handler<T>) -> StorageSlot {
        let slot = handler.as_slot();
        (slot.address(), slot.slot()).into()
    }

    fn stored_word<T: Storable>(handler: &impl Handler<T>, value: U256) -> (StorageSlot, B256) {
        (storage_slot(handler), value.into())
    }

    fn values<T: Storable>(handler: &impl Handler<T>, value: U256) -> StorageValues {
        StorageValues(HashMap::from([stored_word(handler, value)]))
    }

    #[test]
    fn root_slots_are_deduplicated_across_deposits() -> Result<()> {
        let mut plan = DepositPrefetchPlan::new(7, PORTAL);
        plan.add_enabled_token(TOKEN);
        plan.add_mint(TOKEN, RECIPIENT);
        plan.add_mint(TOKEN, RECIPIENT);
        plan.add_encryption_key(U256::from(4));
        plan.add_encryption_key(U256::from(4));

        let slots = plan.root_slots(DepositPrefetchConfig::all())?.0;
        let registry = TIP403Registry::new();
        let portal = ZonePortalStorage::new(PORTAL);
        assert_eq!(slots.len(), 5);
        assert!(slots.contains(&storage_slot(&portal.current_deposit_queue_hash)));
        let encryption_key = &portal.encryption_keys[4];
        let encryption_slot = encryption_key.as_slot();
        for offset in 0..2 {
            assert!(
                slots.contains(
                    &(
                        encryption_slot.address(),
                        encryption_slot.slot() + U256::from(offset)
                    )
                        .into()
                )
            );
        }
        assert!(slots.contains(&storage_slot(&registry.token_transfer_policies[TOKEN])));
        assert!(slots.contains(&storage_slot(&registry.receive_policies[RECIPIENT].config)));
        Ok(())
    }

    #[test]
    fn root_slots_follow_registry_policy_hardfork() -> Result<()> {
        let mut plan = DepositPrefetchPlan::new(7, PORTAL);
        plan.add_enabled_token(ENABLED_TOKEN);
        plan.add_mint(TOKEN, RECIPIENT);
        let registry = TIP403Registry::new();

        let before_t9 = plan.root_slots(DepositPrefetchConfig::new(false, 4))?.0;
        assert_eq!(before_t9.len(), 3);
        assert!(before_t9.contains(&storage_slot(
            &registry.token_transfer_policies[ENABLED_TOKEN]
        )));
        assert!(!before_t9.contains(&storage_slot(&registry.token_transfer_policies[TOKEN])));
        assert!(before_t9.contains(&storage_slot(&registry.receive_policies[RECIPIENT].config)));

        let at_t9 = plan.root_slots(DepositPrefetchConfig::all())?.0;
        assert_eq!(at_t9.len(), 4);
        assert!(at_t9.contains(&storage_slot(&registry.token_transfer_policies[TOKEN])));
        Ok(())
    }

    #[test]
    fn policy_values_expand_into_every_dependent_wave() -> Result<()> {
        let outer_policy_id = 42;
        let mint_policy_id = 77;
        let sender_policy_id = 11;
        let token_filter_id = 12;
        let registry = TIP403Registry::new();
        let handler = &registry.token_transfer_policies[TOKEN];
        let receive = &registry.receive_policies[RECIPIENT];
        let outer = &registry.policy_records[outer_policy_id];
        let mint = &registry.policy_records[mint_policy_id];
        let mut plan = DepositPrefetchPlan::new(7, PORTAL);
        plan.add_mint(TOKEN, RECIPIENT);

        let binding_value = U256::from(outer_policy_id) | (U256::ONE << 64usize);
        let receive_value = U256::ONE
            | (U256::from(sender_policy_id) << 8usize)
            | (U256::from(1) << 72usize)
            | (U256::from(token_filter_id) << 80usize)
            | (U256::from(1) << 144usize)
            | (U256::from(RecoveryMode::ThirdParty as u8) << 152usize);
        let roots = StorageValues(HashMap::from([
            stored_word(handler, binding_value),
            stored_word(&receive.config, receive_value),
        ]));
        let token_policies =
            plan.registered_token_policies(&roots, DepositPrefetchConfig::all())?;
        assert_eq!(token_policies.get(&TOKEN), Some(&outer_policy_id));

        let second = plan.policy_and_receive_slots(&roots, &token_policies)?.0;
        for slot in [
            storage_slot(&registry.policy_id_counter),
            storage_slot(&outer.base),
            storage_slot(&registry.policy_set[token_filter_id][TOKEN]),
            storage_slot(&registry.policy_set[sender_policy_id][ZONE_INBOX_ADDRESS]),
            storage_slot(&receive.recovery_address),
        ] {
            assert!(second.contains(&slot));
        }

        let p_values = values(&outer.base, U256::from(PolicyType::COMPOUND as u8));
        let third = plan.mint_policy_slots(&token_policies, &p_values)?.0;
        assert_eq!(third, HashSet::from([storage_slot(&outer.compound)]));

        let c_values = values(&outer.compound, U256::from(mint_policy_id) << 128usize);
        let fourth = plan.compound_subpolicy_slots(&token_policies, &p_values, &c_values)?;
        assert_eq!(
            fourth.0,
            HashSet::from([
                storage_slot(&mint.base),
                storage_slot(&registry.policy_set[mint_policy_id][RECIPIENT]),
            ])
        );

        let mut unresolved = DepositPrefetchPlan::new(7, PORTAL);
        unresolved.add_mint(TOKEN, VIRTUAL_RECIPIENT);
        assert_eq!(
            unresolved.mint_policy_slots(&token_policies, &p_values)?,
            PrefetchSlots(HashSet::from([storage_slot(&outer.compound)]))
        );
        assert_eq!(
            unresolved.compound_subpolicy_slots(&token_policies, &p_values, &c_values)?,
            PrefetchSlots(HashSet::from([storage_slot(&mint.base)]))
        );

        let built_in = HashMap::from([(TOKEN, ALLOW_ALL_POLICY_ID)]);
        let no_mints = DepositPrefetchPlan::new(7, PORTAL);
        assert!(
            no_mints
                .policy_and_receive_slots(&StorageValues(HashMap::new()), &built_in)?
                .is_empty()
        );
        assert!(no_mints.mint_policy_slots(&built_in, &p_values)?.is_empty());
        Ok(())
    }

    #[test]
    fn missing_typed_value_is_an_error() {
        let registry = TIP403Registry::new();
        assert!(
            StorageValues(HashMap::new())
                .read(&registry.policy_id_counter)
                .is_err()
        );
    }
}
