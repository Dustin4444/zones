//! Event-derived prefetching for the L1 storage read by `advanceTempo`.
//!
//! Deposit execution is sequential, but the regular and decrypted deposit recipients are known
//! before payload construction starts. This module turns that data into bounded waves of exact-L1
//! storage reads so the payload builder normally observes cache hits instead of serial RPC calls.

use super::L1StateProvider;
use alloy_primitives::{Address, B256, U256, keccak256};
use eyre::{Result, WrapErr as _};
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};
use tempo_contracts::precompiles::TIP403_REGISTRY_ADDRESS;
use tempo_precompiles::{
    storage::StorageKey as _,
    tip403_registry::{
        ALLOW_ALL_POLICY_ID, PolicyType, REJECT_ALL_POLICY_ID, TIP403Registry,
        tip403_registry_slots,
    },
};
use tempo_primitives::TempoAddressExt as _;
use tempo_zone_contracts::ZONE_INBOX_ADDRESS;
use tracing::info;
use zone_primitives::constants::{
    PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT, PORTAL_ENCRYPTION_KEYS_SLOT,
};

type StorageSlot = (Address, B256);
type StorageValues = HashMap<StorageSlot, B256>;

/// Hardfork-sensitive controls for deposit storage prefetching.
#[derive(Debug, Clone, Copy)]
pub struct DepositPrefetchConfig {
    receive_policies: bool,
    registry_token_policies: bool,
    concurrency: usize,
}

impl DepositPrefetchConfig {
    /// Create a configuration for the hardforks active in the zone block being prepared and the
    /// operator's maximum number of concurrent L1 requests.
    pub const fn new(
        receive_policies: bool,
        registry_token_policies: bool,
        concurrency: usize,
    ) -> Self {
        Self {
            receive_policies,
            registry_token_policies,
            concurrency,
        }
    }

    #[cfg(test)]
    const fn all() -> Self {
        Self::new(true, true, 4)
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
        // T9 and T6 both follow T3, where these recipients fail validation before policy reads.
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

        let token_policies = self.registered_token_policies(&roots, config);
        let policy_and_receive_slots =
            self.policy_and_receive_slots(&roots, &token_policies, config);
        let policy_and_receive = self
            .prefetch_wave(
                provider,
                policy_and_receive_slots,
                config.concurrency,
                &mut all_slots,
            )
            .await?;

        let mint_policy_slots = self.mint_policy_slots(&token_policies, &policy_and_receive);
        let mint_policies = self
            .prefetch_wave(
                provider,
                mint_policy_slots,
                config.concurrency,
                &mut all_slots,
            )
            .await?;

        let compound_subpolicy_slots =
            self.compound_subpolicy_slots(&token_policies, &policy_and_receive, &mint_policies);
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
        slots: HashSet<StorageSlot>,
        concurrency: usize,
        all_slots: &mut HashSet<StorageSlot>,
    ) -> Result<StorageValues> {
        all_slots.extend(slots.iter().copied());
        provider
            .prefetch_storage(slots, self.block_number, concurrency)
            .await
            .wrap_err_with(|| {
                format!(
                    "failed to prefetch deposit L1 state at block {}",
                    self.block_number
                )
            })
    }

    fn root_slots(&self, config: DepositPrefetchConfig) -> Result<HashSet<StorageSlot>> {
        let mut slots = HashSet::new();
        if !self.portal.is_zero() {
            slots.insert((self.portal, PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT));
            for &key_index in &self.encryption_key_indices {
                let (x, metadata) = encryption_key_slots(key_index)?;
                slots.extend([(self.portal, x), (self.portal, metadata)]);
            }
        }

        for &token in &self.enabled_tokens {
            slots.insert(token_policy_binding_slot(token));
        }
        if config.registry_token_policies {
            for &token in &self.deposit_tokens {
                slots.insert(token_policy_binding_slot(token));
            }
        }
        if config.receive_policies {
            for mint in &self.mints {
                slots.insert(receive_policy_config_slot(mint.recipient));
            }
        }
        Ok(slots)
    }

    fn registered_token_policies(
        &self,
        roots: &StorageValues,
        config: DepositPrefetchConfig,
    ) -> HashMap<Address, u64> {
        if !config.registry_token_policies {
            return HashMap::new();
        }

        self.deposit_tokens
            .iter()
            .filter_map(|&token| {
                let value = roots.get(&token_policy_binding_slot(token))?;
                decode_token_policy_binding(*value).map(|policy_id| (token, policy_id))
            })
            .collect()
    }

    fn policy_and_receive_slots(
        &self,
        roots: &StorageValues,
        token_policies: &HashMap<Address, u64>,
        config: DepositPrefetchConfig,
    ) -> HashSet<StorageSlot> {
        let mut slots = HashSet::new();
        if token_policies
            .values()
            .any(|&policy_id| !is_builtin_policy(policy_id))
        {
            slots.insert(policy_counter_slot());
            slots.extend(
                token_policies
                    .values()
                    .copied()
                    .filter(|&policy_id| !is_builtin_policy(policy_id))
                    .map(policy_base_slot),
            );
        }

        if config.receive_policies {
            for mint in &self.mints {
                let Some(value) = roots.get(&receive_policy_config_slot(mint.recipient)) else {
                    continue;
                };
                let Some(receive) = decode_receive_policy(*value) else {
                    continue;
                };
                if !is_builtin_policy(receive.token_filter_id) {
                    slots.insert(policy_member_slot(receive.token_filter_id, mint.token));
                }
                if !is_builtin_policy(receive.sender_policy_id) {
                    slots.insert(policy_member_slot(
                        receive.sender_policy_id,
                        ZONE_INBOX_ADDRESS,
                    ));
                }
                if receive.has_third_party_recovery {
                    slots.insert(receive_policy_recovery_slot(mint.recipient));
                }
            }
        }
        slots
    }

    fn mint_policy_slots(
        &self,
        token_policies: &HashMap<Address, u64>,
        policy_values: &StorageValues,
    ) -> HashSet<StorageSlot> {
        let mut slots = HashSet::new();
        for (&token, &policy_id) in token_policies {
            if is_builtin_policy(policy_id) {
                continue;
            }
            let Some(base) = policy_values.get(&policy_base_slot(policy_id)) else {
                continue;
            };
            if decode_policy_type(*base) == PolicyType::COMPOUND as u8 {
                slots.insert(policy_compound_slot(policy_id));
            } else {
                // Simple authorization reads membership before validating the stored policy type.
                slots.extend(
                    self.mints
                        .iter()
                        .filter(|mint| mint.token == token)
                        .map(|mint| policy_member_slot(policy_id, mint.recipient)),
                );
            }
        }
        slots
    }

    fn compound_subpolicy_slots(
        &self,
        token_policies: &HashMap<Address, u64>,
        policy_values: &StorageValues,
        mint_policy_values: &StorageValues,
    ) -> HashSet<StorageSlot> {
        let mut slots = HashSet::new();
        for (&token, &policy_id) in token_policies {
            let Some(base) = policy_values.get(&policy_base_slot(policy_id)) else {
                continue;
            };
            if decode_policy_type(*base) != PolicyType::COMPOUND as u8 {
                continue;
            }
            let Some(compound) = mint_policy_values.get(&policy_compound_slot(policy_id)) else {
                continue;
            };
            let mint_policy_id = decode_compound_mint_policy(*compound);
            if !is_builtin_policy(mint_policy_id) {
                slots.insert(policy_base_slot(mint_policy_id));
                slots.extend(
                    self.mints
                        .iter()
                        .filter(|mint| mint.token == token)
                        .map(|mint| policy_member_slot(mint_policy_id, mint.recipient)),
                );
            }
        }
        slots
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReceivePolicy {
    sender_policy_id: u64,
    token_filter_id: u64,
    has_third_party_recovery: bool,
}

fn registry_slot(slot: U256) -> StorageSlot {
    (
        TIP403_REGISTRY_ADDRESS,
        B256::from(slot.to_be_bytes::<32>()),
    )
}

fn token_policy_binding_slot(token: Address) -> StorageSlot {
    registry_slot(TIP403Registry::new().token_transfer_policies[token].base_slot())
}

fn policy_counter_slot() -> StorageSlot {
    registry_slot(TIP403Registry::new().policy_id_counter.slot())
}

fn policy_base_slot(policy_id: u64) -> StorageSlot {
    registry_slot(
        TIP403Registry::new().policy_records[policy_id]
            .base
            .base_slot(),
    )
}

fn policy_compound_slot(policy_id: u64) -> StorageSlot {
    registry_slot(
        TIP403Registry::new().policy_records[policy_id]
            .compound
            .base_slot(),
    )
}

fn policy_member_slot(policy_id: u64, account: Address) -> StorageSlot {
    registry_slot(TIP403Registry::new().policy_set[policy_id][account].slot())
}

fn receive_policy_config_slot(recipient: Address) -> StorageSlot {
    registry_slot(recipient.mapping_slot(tip403_registry_slots::RECEIVE_POLICIES))
}

fn receive_policy_recovery_slot(recipient: Address) -> StorageSlot {
    registry_slot(
        recipient
            .mapping_slot(tip403_registry_slots::RECEIVE_POLICIES)
            .wrapping_add(U256::ONE),
    )
}

fn encryption_key_slots(key_index: U256) -> Result<(B256, B256)> {
    let base: U256 = keccak256(PORTAL_ENCRYPTION_KEYS_SLOT.as_slice()).into();
    let x = key_index
        .checked_mul(U256::from(2))
        .and_then(|offset| base.checked_add(offset))
        .ok_or_else(|| eyre::eyre!("Portal encryption key slot overflow for index {key_index}"))?;
    let metadata = x
        .checked_add(U256::ONE)
        .ok_or_else(|| eyre::eyre!("Portal encryption key metadata slot overflow"))?;
    Ok((
        B256::from(x.to_be_bytes::<32>()),
        B256::from(metadata.to_be_bytes::<32>()),
    ))
}

fn is_builtin_policy(policy_id: u64) -> bool {
    matches!(policy_id, REJECT_ALL_POLICY_ID | ALLOW_ALL_POLICY_ID)
}

/// TIP-1092 packs `{ uint64 policyId; bool isSet; }` from the low-order byte upward.
fn decode_token_policy_binding(value: B256) -> Option<u64> {
    let value = U256::from_be_bytes(value.0);
    let is_set = packed_u8(value, 64) != 0;
    is_set.then(|| packed_u64(value, 0))
}

/// `PolicyData.policy_type` is the low-order byte of its packed storage slot.
fn decode_policy_type(value: B256) -> u8 {
    packed_u8(U256::from_be_bytes(value.0), 0)
}

/// `CompoundPolicyData` packs sender, recipient, then mint-recipient `uint64` policy IDs.
fn decode_compound_mint_policy(value: B256) -> u64 {
    packed_u64(U256::from_be_bytes(value.0), 128)
}

/// Decode the fields needed to derive reads from a packed TIP-1028 receive-policy configuration.
///
/// Layout, from the low-order byte: `bool`, sender `uint64`, sender type `uint8`, token-filter
/// `uint64`, token-filter type `uint8`, and recovery-mode `uint8`.
fn decode_receive_policy(value: B256) -> Option<ReceivePolicy> {
    let value = U256::from_be_bytes(value.0);
    if (value & U256::from(u8::MAX)) == U256::ZERO {
        return None;
    }
    Some(ReceivePolicy {
        sender_policy_id: packed_u64(value, 8),
        token_filter_id: packed_u64(value, 80),
        has_third_party_recovery: packed_u8(value, 152) == 2,
    })
}

fn packed_u8(value: U256, shift: usize) -> u8 {
    ((value >> shift) & U256::from(u8::MAX)).to::<u8>()
}

fn packed_u64(value: U256, shift: usize) -> u64 {
    ((value >> shift) & U256::from(u64::MAX)).to::<u64>()
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

    #[test]
    fn root_slots_are_deduplicated_across_deposits() -> Result<()> {
        let mut plan = DepositPrefetchPlan::new(7, PORTAL);
        plan.add_enabled_token(TOKEN);
        plan.add_mint(TOKEN, RECIPIENT);
        plan.add_mint(TOKEN, RECIPIENT);
        plan.add_encryption_key(U256::from(4));
        plan.add_encryption_key(U256::from(4));

        let slots = plan.root_slots(DepositPrefetchConfig::all())?;
        let (key_x, key_metadata) = encryption_key_slots(U256::from(4))?;
        assert_eq!(slots.len(), 5);
        assert!(slots.contains(&(PORTAL, PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT)));
        assert!(slots.contains(&(PORTAL, key_x)));
        assert!(slots.contains(&(PORTAL, key_metadata)));
        assert!(slots.contains(&token_policy_binding_slot(TOKEN)));
        assert!(slots.contains(&receive_policy_config_slot(RECIPIENT)));
        Ok(())
    }

    #[test]
    fn root_slots_follow_receive_and_registry_policy_hardforks() -> Result<()> {
        let mut plan = DepositPrefetchPlan::new(7, PORTAL);
        plan.add_enabled_token(ENABLED_TOKEN);
        plan.add_mint(TOKEN, RECIPIENT);

        let before_t6_t9 = plan.root_slots(DepositPrefetchConfig::new(false, false, 4))?;
        assert_eq!(
            before_t6_t9,
            HashSet::from([
                (PORTAL, PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT),
                token_policy_binding_slot(ENABLED_TOKEN),
            ])
        );

        let at_t6 = plan.root_slots(DepositPrefetchConfig::new(true, false, 4))?;
        assert_eq!(
            at_t6,
            HashSet::from([
                (PORTAL, PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT),
                token_policy_binding_slot(ENABLED_TOKEN),
                receive_policy_config_slot(RECIPIENT),
            ])
        );

        let at_t9 = plan.root_slots(DepositPrefetchConfig::new(true, true, 4))?;
        assert_eq!(
            at_t9,
            HashSet::from([
                (PORTAL, PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT),
                token_policy_binding_slot(ENABLED_TOKEN),
                token_policy_binding_slot(TOKEN),
                receive_policy_config_slot(RECIPIENT),
            ])
        );
        Ok(())
    }

    #[test]
    fn packed_policy_values_drive_dependent_slots() {
        let binding: U256 = U256::from(42) | (U256::ONE << 64usize);
        assert_eq!(
            decode_token_policy_binding(B256::from(binding.to_be_bytes::<32>())),
            Some(42)
        );
        assert_eq!(decode_token_policy_binding(B256::ZERO), None);

        let compound: U256 =
            U256::from(2) | (U256::from(3) << 64usize) | (U256::from(77) << 128usize);
        assert_eq!(
            decode_compound_mint_policy(B256::from(compound.to_be_bytes::<32>())),
            77
        );
    }

    #[test]
    fn packed_receive_policy_drives_memberships_and_recovery() {
        let sender_policy_id = 11u64;
        let token_filter_id = 12u64;
        let packed: U256 = U256::ONE
            | (U256::from(sender_policy_id) << 8usize)
            | (U256::from(1) << 72usize)
            | (U256::from(token_filter_id) << 80usize)
            | (U256::from(1) << 144usize)
            | (U256::from(2) << 152usize);

        assert_eq!(
            decode_receive_policy(B256::from(packed.to_be_bytes::<32>())),
            Some(ReceivePolicy {
                sender_policy_id,
                token_filter_id,
                has_third_party_recovery: true,
            })
        );
        assert_eq!(decode_receive_policy(B256::ZERO), None);
    }

    #[test]
    fn policy_values_expand_into_every_dependent_wave() {
        let outer_policy_id = 42u64;
        let mint_policy_id = 77u64;
        let sender_policy_id = 11u64;
        let token_filter_id = 12u64;

        let mut plan = DepositPrefetchPlan::new(7, PORTAL);
        plan.add_mint(TOKEN, RECIPIENT);

        let binding =
            B256::from((U256::from(outer_policy_id) | (U256::ONE << 64usize)).to_be_bytes::<32>());
        let receive = B256::from(
            (U256::ONE
                | (U256::from(sender_policy_id) << 8usize)
                | (U256::from(1u8) << 72usize)
                | (U256::from(token_filter_id) << 80usize)
                | (U256::from(1u8) << 144usize)
                | (U256::from(2u8) << 152usize))
                .to_be_bytes::<32>(),
        );
        let roots = StorageValues::from([
            (token_policy_binding_slot(TOKEN), binding),
            (receive_policy_config_slot(RECIPIENT), receive),
        ]);

        let token_policies = plan.registered_token_policies(&roots, DepositPrefetchConfig::all());
        assert_eq!(token_policies.get(&TOKEN), Some(&outer_policy_id));

        let second_wave =
            plan.policy_and_receive_slots(&roots, &token_policies, DepositPrefetchConfig::all());
        assert!(second_wave.contains(&policy_counter_slot()));
        assert!(second_wave.contains(&policy_base_slot(outer_policy_id)));
        assert!(second_wave.contains(&policy_member_slot(token_filter_id, TOKEN)));
        assert!(second_wave.contains(&policy_member_slot(sender_policy_id, ZONE_INBOX_ADDRESS)));
        assert!(second_wave.contains(&receive_policy_recovery_slot(RECIPIENT)));

        let compound_base = B256::from(U256::from(PolicyType::COMPOUND as u8).to_be_bytes::<32>());
        let second_values =
            StorageValues::from([(policy_base_slot(outer_policy_id), compound_base)]);
        let third_wave = plan.mint_policy_slots(&token_policies, &second_values);
        assert_eq!(
            third_wave,
            HashSet::from([policy_compound_slot(outer_policy_id)])
        );

        let compound = B256::from((U256::from(mint_policy_id) << 128usize).to_be_bytes::<32>());
        let third_values = StorageValues::from([(policy_compound_slot(outer_policy_id), compound)]);
        let fourth_wave =
            plan.compound_subpolicy_slots(&token_policies, &second_values, &third_values);
        assert_eq!(
            fourth_wave,
            HashSet::from([
                policy_base_slot(mint_policy_id),
                policy_member_slot(mint_policy_id, RECIPIENT),
            ])
        );

        let mut unresolved_recipient = DepositPrefetchPlan::new(7, PORTAL);
        unresolved_recipient.add_mint(TOKEN, VIRTUAL_RECIPIENT);
        assert_eq!(
            unresolved_recipient.mint_policy_slots(&token_policies, &second_values),
            HashSet::from([policy_compound_slot(outer_policy_id)]),
            "compound data remains token-derived when the effective recipient is Zone-local"
        );
        assert_eq!(
            unresolved_recipient.compound_subpolicy_slots(
                &token_policies,
                &second_values,
                &third_values,
            ),
            HashSet::from([policy_base_slot(mint_policy_id)]),
            "the sub-policy base remains derivable, but its membership does not"
        );
    }
}
