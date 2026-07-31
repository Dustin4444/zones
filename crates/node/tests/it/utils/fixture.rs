//! Synthetic L1 fixture for deposit-queue injection tests.

use super::*;
use alloy_provider::{DynProvider, Provider};
use tempo_chainspec::spec::TEMPO_T0_BASE_FEE;
use tempo_zone_contracts::IZoneOutbox;

use alloy_consensus::Header;
use alloy_eips::NumHash;
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_rlp::Encodable;
use alloy_sol_types::SolValue;
use reth_primitives_traits::SealedHeader;
use std::sync::Mutex;
use tempo_contracts::precompiles::TIP403_REGISTRY_ADDRESS;
use tempo_precompiles::{
    PATH_USD_ADDRESS,
    storage::StorageKey,
    tip403_registry::{ALLOW_ALL_POLICY_ID, tip403_registry_slots},
};
use tempo_primitives::TempoHeader;
use tempo_zone_contracts::{
    PORTAL_IS_SEQUENCER_SLOT, PORTAL_MAX_TEMPO_GAS_RATE_SLOT, ZONE_OUTBOX_ADDRESS,
};
use zone_l1::{
    Deposit, DepositQueue, EnabledToken, L1Deposit, L1PortalEvents, L1StateCache,
    state::EnabledTokenRegistry,
};
use zone_precompiles::ZONE_FEE_MANAGER_ADDRESS;
use zone_primitives::constants::{PORTAL_ACCESS_MODE_SLOT, PORTAL_TOKEN_CONFIGS_SLOT};

/// Start a local zone node with an L1Fixture already seeded for `seed_blocks` blocks.
pub(crate) async fn start_local_zone_with_fixture(
    seed_blocks: u64,
) -> eyre::Result<(ZoneTestNode, L1Fixture)> {
    let zone = ZoneTestNode::start_local().await?;
    let fixture = L1Fixture::new();

    fixture.seed_l1_cache(
        zone.l1_state_cache(),
        zone.enabled_tokens(),
        Address::ZERO,
        Address::ZERO,
        seed_blocks,
    );
    Ok((zone, fixture))
}

/// Seed an existing L1Fixture's cache into a zone node's L1 state cache.
///
/// Use when multiple zones share the same fixture timeline — call once per zone.
pub(crate) fn seed_fixture_for_zone(fixture: &L1Fixture, zone: &ZoneTestNode, seed_blocks: u64) {
    fixture.seed_l1_cache(
        zone.l1_state_cache(),
        zone.enabled_tokens(),
        Address::ZERO,
        Address::ZERO,
        seed_blocks,
    );
}

/// A synthetic L1 block produced by [`L1Fixture`].
///
/// Clonable so the same block can be enqueued into multiple zone deposit queues,
/// simulating multiple zones observing the same L1 block.
#[derive(Clone)]
pub(crate) struct FixtureBlock {
    /// The L1 block header. Use `header.inner.number` to read the block number.
    pub header: TempoHeader,
}

/// Builder for creating realistic L1 block headers and deposits for injection
/// into a [`ZoneTestNode`]'s deposit queue.
///
/// Maintains monotonic block numbers and timestamps, and chains parent hashes
/// to mirror what the real L1Subscriber would produce.
pub(crate) struct L1Fixture {
    next_block_number: u64,
    next_timestamp: u64,
    last_hash: B256,
    /// Raw L1 caches seeded by this fixture, updated with state implied by injected deposits.
    caches: Mutex<Vec<L1StateCache>>,
    /// Enabled-token registries kept in sync with injected portal events.
    enabled_token_registries: Mutex<Vec<EnabledTokenRegistry>>,
}

impl L1Fixture {
    pub(crate) fn new() -> Self {
        // TempoState stores tempoBlockHash = keccak256(rlp(default TempoHeader)),
        // so the first injected L1 block must have parent_hash matching this.
        let genesis_header = TempoHeader::default();
        let mut rlp_buf = Vec::new();
        genesis_header.encode(&mut rlp_buf);
        let genesis_hash = keccak256(&rlp_buf);

        Self {
            next_block_number: 1,
            next_timestamp: 1_000_000,
            last_hash: genesis_hash,
            caches: Mutex::new(Vec::new()),
            enabled_token_registries: Mutex::new(Vec::new()),
        }
    }

    /// Pre-populate the L1 state cache with values that `advanceTempo` will read
    /// via the TempoState precompile.
    ///
    /// Without a real L1, the precompile would fail with a hard error on cache miss.
    /// This seeds the cache so that `readTempoStorageSlot(portal, slot)` succeeds
    /// for each block we plan to inject.
    pub(crate) fn seed_l1_cache(
        &self,
        cache_handle: &L1StateCache,
        enabled_tokens: &EnabledTokenRegistry,
        portal_address: Address,
        sequencer: Address,
        num_blocks: u64,
    ) {
        let mut cache = cache_handle.lock();
        let deposit_queue_hash_slot = B256::with_last_byte(3);
        let refunds_slot = B256::with_last_byte(8);
        let sequencer_membership_slot =
            keccak256((sequencer, PORTAL_IS_SEQUENCER_SLOT).abi_encode());
        let path_usd_config_slot: B256 = PATH_USD_ADDRESS
            .mapping_slot(PORTAL_TOKEN_CONFIGS_SLOT.into())
            .into();
        let enabled_token_config = enabled_deposits_active_token_config();
        let max_tempo_gas_rate = B256::from(U256::from(1_000_000_000_000_000_000_u128));

        // Local fixtures have no RPC fallback. Transfers to protocol accounts still consult their
        // address-level receive policies, so seed their absence as baseline raw L1 state.
        for recipient in [ZONE_OUTBOX_ADDRESS, ZONE_FEE_MANAGER_ADDRESS] {
            let receive_policy_slot =
                recipient.mapping_slot(tip403_registry_slots::RECEIVE_POLICIES);
            cache.set(
                TIP403_REGISTRY_ADDRESS,
                B256::from(receive_policy_slot.to_be_bytes()),
                0,
                B256::ZERO,
            );
        }

        for block in 0..=num_blocks {
            cache.set(
                portal_address,
                sequencer_membership_slot,
                block,
                B256::with_last_byte(1),
            );
            // Deposit queue hash slot (3) — read by ZoneInbox after finalizeTempo.
            // The initial value is B256::ZERO (empty queue).
            cache.set(portal_address, deposit_queue_hash_slot, block, B256::ZERO);
            cache.set(portal_address, refunds_slot, block, B256::ZERO);
            // Synthetic fixtures use open account and gateway modes so their tests do not need
            // unrelated closed-loop membership setup or a reachable L1 RPC fallback.
            cache.set(portal_address, PORTAL_ACCESS_MODE_SLOT, block, B256::ZERO);
            // Permit the protocol-wide maximum in synthetic fixtures. Production values are
            // imported from the finalized ZonePortal storage slot.
            cache.set(
                portal_address,
                PORTAL_MAX_TEMPO_GAS_RATE_SLOT,
                block,
                max_tempo_gas_rate,
            );
            // Local fixtures treat pathUSD as the default enabled bridge token.
            // ZoneConfig reads the L1 ZonePortal TokenConfig mapping directly, so
            // seed the packed { enabled, depositsActive } value to avoid a dummy
            // RPC fallback on self-contained tests.
            cache.set(
                portal_address,
                path_usd_config_slot,
                block,
                enabled_token_config,
            );
        }

        // System transactions resolve their zero-address fee token before execution. Keep that
        // synthetic token permissive in RPC-free fixtures, matching the old policy-provider stub.
        seed_raw_tip403_token_policy(&mut cache, 0, Address::ZERO, ALLOW_ALL_POLICY_ID);
        seed_raw_tip403_token_policy(&mut cache, 0, PATH_USD_ADDRESS, ALLOW_ALL_POLICY_ID);
        drop(cache);
        self.caches.lock().unwrap().push(cache_handle.clone());
        self.enabled_token_registries
            .lock()
            .unwrap()
            .push(enabled_tokens.clone());
    }

    /// Build a TIP-403 checker and seed the token and account policy state it consumes.
    pub(crate) fn tip403_registry_check(
        &self,
        zone: &ZoneTestNode,
        token: Address,
        no_receive_policy_accounts: &[Address],
        block_number: u64,
        policy_id: u64,
    ) -> eyre::Result<Check403Registry> {
        for &account in no_receive_policy_accounts {
            self.seed_no_receive_policy_at(block_number, account)?;
        }
        seed_raw_tip403_token_policy(
            &mut zone.l1_state_cache().lock(),
            block_number,
            token,
            policy_id,
        );
        Ok(Check403Registry {
            provider: zone.provider(),
            token,
        })
    }

    /// Seed the absence of an address-level TIP-403 receive policy at the current Zone anchor.
    pub(crate) fn seed_no_receive_policy(&self, recipient: Address) -> eyre::Result<()> {
        let current_anchor = self.next_block_number.saturating_sub(1);
        self.seed_no_receive_policy_at(current_anchor, recipient)
    }

    fn seed_no_receive_policy_at(&self, block_number: u64, recipient: Address) -> eyre::Result<()> {
        // TODO(rusowsky): make `ReceivePolicy` public upstream to use the handlers
        let receive_policy_slot = recipient.mapping_slot(tip403_registry_slots::RECEIVE_POLICIES);
        for cache in self.caches.lock().unwrap().iter() {
            cache.lock().set(
                TIP403_REGISTRY_ADDRESS,
                B256::from(receive_policy_slot.to_be_bytes()),
                block_number,
                B256::ZERO,
            );
        }
        Ok(())
    }

    fn seed_regular_deposit_policy_state(&self, block_number: u64, deposits: &[Deposit]) {
        for deposit in deposits {
            self.seed_no_receive_policy_at(block_number, deposit.to)
                .expect("deposit receive-policy fixture seed must be admitted");
        }
    }

    fn seed_enabled_token_policy_state(&self, block_number: u64, tokens: &[EnabledToken]) {
        for cache in self.caches.lock().unwrap().iter() {
            let mut cache = cache.lock();
            for token in tokens {
                seed_raw_tip403_token_policy(
                    &mut cache,
                    block_number,
                    token.token,
                    ALLOW_ALL_POLICY_ID,
                );
            }
        }
    }

    fn apply_enabled_token_events(&self, tokens: &[EnabledToken]) {
        for registry in self.enabled_token_registries.lock().unwrap().iter() {
            registry
                .write()
                .extend(tokens.iter().map(|enabled| enabled.token));
        }
    }

    /// The next L1 block number this fixture will inject.
    pub(crate) fn next_anchor_number(&self) -> u64 {
        self.next_block_number
    }

    /// Build a [`TempoHeader`] for the next L1 block.
    fn next_header(&mut self) -> TempoHeader {
        let number = self.next_block_number;
        let timestamp = self.next_timestamp;
        let parent_hash = self.last_hash;

        let header = TempoHeader {
            inner: Header {
                number,
                timestamp,
                parent_hash,
                ..Default::default()
            },
            ..Default::default()
        };

        // Advance state: TempoState stores keccak256(rlp(header)) as tempoBlockHash,
        // so the next block's parent_hash must match this value.
        let mut rlp_buf = Vec::new();
        header.encode(&mut rlp_buf);
        self.last_hash = keccak256(&rlp_buf);
        self.next_block_number += 1;
        self.next_timestamp += 1; // 1s per L1 block

        // Synthetic injection bypasses the subscriber, so publish the same verified-receipt
        // coverage the subscriber would publish before the engine consumes this block.
        for cache in self.caches.lock().unwrap().iter() {
            cache.lock().invalidate_and_set_anchor(number, []);
        }

        header
    }

    /// Build the next L1 block without injecting it into any queue.
    ///
    /// Use with [`enqueue`](Self::enqueue) to broadcast the same block
    /// to multiple zone deposit queues.
    pub(crate) fn next_block(&mut self) -> FixtureBlock {
        let header = self.next_header();
        FixtureBlock { header }
    }

    /// Enqueue a pre-built block into a deposit queue with the given deposits.
    pub(crate) fn enqueue(
        &self,
        block: &FixtureBlock,
        queue: &DepositQueue,
        deposits: Vec<Deposit>,
    ) {
        self.seed_regular_deposit_policy_state(block.header.inner.number, &deposits);
        let l1_deposits = deposits.into_iter().map(L1Deposit::Regular).collect();
        let events = L1PortalEvents::from_deposits(l1_deposits);
        queue.enqueue(block.header.clone(), events);
    }

    /// Enqueue a pre-built block into a deposit queue with full portal events.
    pub(crate) fn enqueue_events(
        &self,
        block: &FixtureBlock,
        queue: &DepositQueue,
        events: L1PortalEvents,
    ) {
        let block_number = block.header.inner.number;
        self.seed_enabled_token_policy_state(block_number, &events.enabled_tokens);
        self.apply_enabled_token_events(&events.enabled_tokens);
        for deposit in &events.deposits {
            if let L1Deposit::Regular(deposit) = deposit {
                self.seed_no_receive_policy_at(block_number, deposit.to)
                    .expect("event receive-policy fixture seed must be admitted");
            }
        }
        queue.enqueue(block.header.clone(), events);
    }

    /// Inject an L1 block with enabled tokens (no deposits) into the queue.
    pub(crate) fn inject_enabled_tokens(
        &mut self,
        queue: &DepositQueue,
        tokens: Vec<EnabledToken>,
    ) {
        let header = self.next_header();
        self.seed_enabled_token_policy_state(header.inner.number, &tokens);
        self.apply_enabled_token_events(&tokens);
        let events = L1PortalEvents {
            deposits: vec![],
            enabled_tokens: tokens,
            leader_transitions: vec![],
        };
        queue.enqueue(header, events);
    }

    /// Inject an empty L1 block (no deposits) into the queue.
    pub(crate) fn inject_empty_block(&mut self, queue: &DepositQueue) -> NumHash {
        let header = self.next_header();
        let anchor = SealedHeader::seal_slow(header.clone()).num_hash();
        queue.enqueue(header, L1PortalEvents::default());
        anchor
    }

    /// Inject `n` empty L1 blocks (no deposits) into the queue.
    pub(crate) fn inject_empty_blocks(&mut self, queue: &DepositQueue, n: u64) {
        for _ in 0..n {
            self.inject_empty_block(queue);
        }
    }

    /// Inject an L1 block with the given deposits into the queue.
    pub(crate) fn inject_deposits(
        &mut self,
        queue: &DepositQueue,
        deposits: Vec<Deposit>,
    ) -> NumHash {
        let header = self.next_header();
        self.seed_regular_deposit_policy_state(header.inner.number, &deposits);
        let anchor = SealedHeader::seal_slow(header.clone()).num_hash();
        let l1_deposits = deposits.into_iter().map(L1Deposit::Regular).collect();
        let events = L1PortalEvents::from_deposits(l1_deposits);
        queue.enqueue(header, events);
        anchor
    }
}

/// Create a [`Deposit`] with zero fee, a zero memo, and the sender as its own
/// Tempo refund recipient.
pub(crate) fn make_deposit(token: Address, sender: Address, to: Address, amount: u128) -> Deposit {
    Deposit {
        token,
        sender,
        to,
        amount,
        fee: 0,
        tempo_refund_recipient: sender,
        memo: B256::ZERO,
    }
}

/// Submit `requestWithdrawal` transactions from `dev_address` for each amount,
/// inject one empty L1 block to include them, and return the zone block that
/// contains them (asserting they all landed in the same block).
pub(crate) async fn submit_withdrawal(
    fixture: &mut L1Fixture,
    zone: &ZoneTestNode,
    provider: &DynProvider,
    dev_address: Address,
    amount: u128,
) -> eyre::Result<u64> {
    submit_withdrawals(fixture, zone, provider, dev_address, &[amount]).await
}

pub(crate) async fn submit_withdrawals(
    fixture: &mut L1Fixture,
    zone: &ZoneTestNode,
    provider: &DynProvider,
    dev_address: Address,
    amounts: &[u128],
) -> eyre::Result<u64> {
    eyre::ensure!(
        !amounts.is_empty(),
        "at least one withdrawal amount is required"
    );

    let outbox = IZoneOutbox::new(ZONE_OUTBOX_ADDRESS, provider.clone());
    let nonce = provider
        .get_transaction_count(dev_address)
        .pending()
        .await?;
    let mut pending = Vec::with_capacity(amounts.len());
    for (offset, amount) in amounts.iter().copied().enumerate() {
        pending.push(
            outbox
                .requestWithdrawal(
                    PATH_USD_ADDRESS,
                    dev_address,
                    amount,
                    B256::ZERO,
                    0,
                    dev_address,
                    Bytes::new(),
                    Bytes::new(),
                )
                .nonce(nonce + offset as u64)
                .gas_price(TEMPO_T0_BASE_FEE as u128)
                .gas(WITHDRAWAL_TX_GAS)
                .send()
                .await?,
        );
    }

    fixture.inject_empty_block(zone.deposit_queue());
    let mut withdrawal_block = None;
    for pending_tx in pending {
        let receipt = pending_tx.get_receipt().await?;
        assert!(
            receipt.status(),
            "withdrawal should succeed (gas used: {})",
            receipt.gas_used
        );
        let block_number = receipt
            .block_number
            .ok_or_else(|| eyre::eyre!("withdrawal receipt missing block number"))?;
        if let Some(expected) = withdrawal_block {
            eyre::ensure!(
                block_number == expected,
                "withdrawals were included in different blocks: {expected} and {block_number}"
            );
        } else {
            withdrawal_block = Some(block_number);
        }
    }

    withdrawal_block.ok_or_else(|| eyre::eyre!("withdrawal block missing"))
}
