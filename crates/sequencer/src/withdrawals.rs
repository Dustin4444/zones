//! Withdrawal collection store and L1 withdrawal processor for the zone sequencer.
//!
//! This module provides two main components:
//!
//! - [`WithdrawalStore`] — an in-memory store that holds [`abi::Withdrawal`] structs grouped by
//!   batch index. The L1 portal queue only stores hashes, so the sequencer must retain the actual
//!   withdrawal data to provide it when calling `processWithdrawals`.
//!
//! - [`WithdrawalProcessor`] — a background task that polls the ZonePortal withdrawal queue on
//!   **Tempo L1** and processes withdrawals by calling `processWithdrawals(withdrawals, remainingQueue)`.
//!
//! ## Data flow
//!
//! 1. Withdrawal requests originate on the **Zone L2** (`ZoneOutbox.requestWithdrawal`).
//! 2. The sequencer observes `WithdrawalRequested` events and stores the withdrawal data in the
//!    [`WithdrawalStore`].
//! 3. At batch finalization, the sequencer calls `finalizeWithdrawalBatch` on L2, which builds a
//!    hash chain. The proof then enqueues this hash chain into the portal's withdrawal queue on L1.
//! 4. The [`WithdrawalProcessor`] polls the portal queue on L1 and processes each withdrawal by
//!    providing the original data and the remaining queue hash.
//!
//! ## Batch-to-slot mapping
//!
//! The portal's withdrawal queue slots correspond to batch indices. The store's `batch_index`
//! should match the portal slot index. The caller (batch submitter) is responsible for tracking
//! which `batch_index` maps to which portal slot.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_network::ReceiptResponse;
use alloy_primitives::{Address, B256, U256};
use alloy_provider::{DynProvider, PendingTransactionBuilder};
use futures::future::join_all;
use parking_lot::Mutex;
use tempo_alloy::{TempoNetwork, provider::ext::TempoProviderExt};
use tokio::sync::Notify;
use tracing::{debug, error, info, instrument, warn};

use crate::{
    abi::{self, EMPTY_SENTINEL, MAX_WITHDRAWAL_GAS_LIMIT, ZonePortal},
    metrics::WithdrawalProcessorMetrics,
    nonce_keys::PROCESS_WITHDRAWAL_NONCE_KEY,
    settlement::{WITHDRAWAL_QUEUE_CAPACITY, find_processed_offset},
};
use tempo_alloy::rpc::TempoCallBuilderExt;

const PROCESS_WITHDRAWAL_CONFIRM_TIMEOUT: Duration = Duration::from_secs(30);
const PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS: u64 = 100_000;
const PROCESS_WITHDRAWAL_ITEM_OVERHEAD_GAS: u64 = 2_000_000;
const MAX_WITHDRAWALS_PER_PROCESS_BATCH: usize = 64;

/// Default gas budget for one `processWithdrawals` transaction.
pub const DEFAULT_MAX_WITHDRAWAL_BATCH_GAS: u64 = 10_000_000;

/// Default maximum number of ordered withdrawal transactions kept in flight.
pub const DEFAULT_MAX_IN_FLIGHT_WITHDRAWAL_BATCHES: usize = 4;

/// Default aggregate gas budget across concurrently in-flight withdrawal transactions.
pub const DEFAULT_MAX_IN_FLIGHT_WITHDRAWAL_GAS: u64 = 20_000_000;
#[cfg(test)]
const MAX_PROCESS_WITHDRAWAL_TX_GAS: u64 =
    PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS + process_withdrawal_item_gas(MAX_WITHDRAWAL_GAS_LIMIT);

/// Shared handle to the withdrawal store.
#[derive(Clone)]
pub struct SharedWithdrawalStore(Arc<Mutex<WithdrawalStore>>);

impl SharedWithdrawalStore {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(WithdrawalStore::new())))
    }

    pub fn lock(&self) -> parking_lot::MutexGuard<'_, WithdrawalStore> {
        self.0.lock()
    }
}

impl Default for SharedWithdrawalStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for the withdrawal processor.
#[derive(Debug, Clone)]
pub struct WithdrawalProcessorConfig {
    /// ZonePortal contract address on Tempo L1.
    pub portal_address: Address,
    /// Tempo L1 RPC URL (HTTP).
    pub l1_rpc_url: String,
    /// Fallback timeout for checking the withdrawal queue if no notification arrives.
    pub fallback_poll_interval: Duration,
    /// Address whose lane-2 nonces order withdrawal processing transactions.
    pub sequencer_address: Address,
    /// Gas and concurrency limits for withdrawal transaction planning.
    pub batch_limits: WithdrawalBatchLimits,
}

/// Limits applied while packing and pipelining `processWithdrawals` transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithdrawalBatchLimits {
    /// Maximum planned gas for one transaction. A single oversized withdrawal is still emitted
    /// so that it cannot permanently block the queue.
    pub max_batch_gas: u64,
    /// Maximum number of transactions to keep concurrently in flight.
    pub max_in_flight_batches: usize,
    /// Maximum sum of planned gas across the in-flight transaction window.
    pub max_in_flight_gas: u64,
}

impl WithdrawalBatchLimits {
    fn normalized(self) -> Self {
        Self {
            max_batch_gas: self.max_batch_gas.max(1),
            max_in_flight_batches: self.max_in_flight_batches.max(1),
            max_in_flight_gas: self.max_in_flight_gas.max(1),
        }
    }
}

impl Default for WithdrawalBatchLimits {
    fn default() -> Self {
        Self {
            max_batch_gas: DEFAULT_MAX_WITHDRAWAL_BATCH_GAS,
            max_in_flight_batches: DEFAULT_MAX_IN_FLIGHT_WITHDRAWAL_BATCHES,
            max_in_flight_gas: DEFAULT_MAX_IN_FLIGHT_WITHDRAWAL_GAS,
        }
    }
}

/// In-memory store for withdrawal data grouped by batch index.
///
/// The L1 portal queue only stores hash chains. The sequencer must keep the actual
/// [`abi::Withdrawal`] structs so it can provide them when calling `processWithdrawals`.
///
/// Withdrawals are grouped by batch index, where each batch is a `Vec<Withdrawal>` in FIFO order
/// (oldest first). The batch index corresponds to the portal's withdrawal queue slot index.
pub struct WithdrawalStore {
    batches: BTreeMap<u64, Vec<abi::Withdrawal>>,
}

impl WithdrawalStore {
    pub fn new() -> Self {
        Self {
            batches: BTreeMap::new(),
        }
    }

    /// Add a withdrawal to the given batch.
    ///
    /// Withdrawals within a batch are stored in FIFO order (oldest first).
    pub fn add_withdrawal(&mut self, batch_index: u64, withdrawal: abi::Withdrawal) {
        self.batches
            .entry(batch_index)
            .or_default()
            .push(withdrawal);
    }

    /// Set all withdrawals for a batch at once, replacing any existing data.
    pub fn add_batch(&mut self, batch_index: u64, withdrawals: Vec<abi::Withdrawal>) {
        self.batches.insert(batch_index, withdrawals);
    }

    /// Replace the entire store with an authoritative set of pending batches.
    pub(crate) fn replace_batches(&mut self, batches: BTreeMap<u64, Vec<abi::Withdrawal>>) {
        self.batches = batches;
    }

    /// Get all withdrawals for a batch.
    pub fn get_batch(&self, batch_index: u64) -> Option<&Vec<abi::Withdrawal>> {
        self.batches.get(&batch_index)
    }

    /// Remove a batch after all its withdrawals are processed.
    pub fn remove_batch(&mut self, batch_index: u64) {
        self.batches.remove(&batch_index);
    }

    pub fn has_batch(&self, batch_index: u64) -> bool {
        self.batches.contains_key(&batch_index)
    }

    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    /// Return the smallest and largest portal slot indices currently present.
    fn slot_range(&self) -> Option<(u64, u64)> {
        let first = *self.batches.keys().next()?;
        let last = *self.batches.keys().next_back()?;
        Some((first, last))
    }

    /// Return a compact summary of the store as `(batch_count, first_slot, last_slot)`.
    pub(crate) fn summary(&self) -> (usize, Option<u64>, Option<u64>) {
        let (first_slot, last_slot) = self
            .slot_range()
            .map_or((None, None), |(first, last)| (Some(first), Some(last)));
        (self.batch_count(), first_slot, last_slot)
    }

    /// Return the nearest populated slots before and after `slot`, if any exist.
    fn neighboring_slots(&self, slot: u64) -> (Option<u64>, Option<u64>) {
        let prev = self.batches.range(..slot).next_back().map(|(&idx, _)| idx);
        let next = self
            .batches
            .range(slot.saturating_add(1)..)
            .next()
            .map(|(&idx, _)| idx);
        (prev, next)
    }
}

impl Default for WithdrawalStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute the remaining queue hash after removing the first `processed_count` withdrawals.
///
/// This value is passed as `remainingQueue` to `processWithdrawals` on the portal contract.
///
/// - If `processed_count >= withdrawals.len()`, returns `B256::ZERO` (no remaining items).
/// - Otherwise, computes the hash chain over `withdrawals[processed_count..]` via
///   [`abi::Withdrawal::queue_hash`].
pub fn compute_remaining_queue(withdrawals: &[abi::Withdrawal], processed_count: usize) -> B256 {
    if processed_count >= withdrawals.len() {
        return B256::ZERO;
    }

    let remaining = &withdrawals[processed_count..];

    abi::Withdrawal::queue_hash(remaining)
}

// ---------------------------------------------------------------------------
//  Withdrawal processor
// ---------------------------------------------------------------------------

struct StoreSnapshot {
    batch_count: usize,
    first_slot: Option<u64>,
    last_slot: Option<u64>,
    prev_slot: Option<u64>,
    next_slot: Option<u64>,
    withdrawals: Option<Vec<abi::Withdrawal>>,
}

/// Return the extra outer-frame gas needed for EIP-150's 63/64 forwarding rule.
///
/// `ZonePortal.processWithdrawals` must keep enough gas in the caller frame for the
/// callback CALL to receive at least `gas_limit`. The cushion is `ceil(gas_limit / 63)`,
/// which compensates for the 1/64 of remaining gas that EIP-150 withholds from the call.
const fn eip150_cushion(gas_limit: u64) -> u64 {
    gas_limit / 63 + if gas_limit.is_multiple_of(63) { 0 } else { 1 }
}

/// Return the gas reserved for one withdrawal inside a `processWithdrawals` transaction.
///
/// The callback portion is capped at [`MAX_WITHDRAWAL_GAS_LIMIT`] before adding the
/// per-item portal/messenger overhead and the EIP-150 cushion. This keeps legacy over-cap
/// withdrawals submit-able while bounding the planner's gas accounting.
const fn process_withdrawal_item_gas(callback_gas_limit: u64) -> u64 {
    let bounded_callback_gas = if callback_gas_limit > MAX_WITHDRAWAL_GAS_LIMIT {
        MAX_WITHDRAWAL_GAS_LIMIT
    } else {
        callback_gas_limit
    };

    bounded_callback_gas
        + PROCESS_WITHDRAWAL_ITEM_OVERHEAD_GAS
        + eip150_cushion(bounded_callback_gas)
}

/// A gas-bounded `processWithdrawals` transaction planned from one portal queue slot.
#[derive(Debug, Clone)]
pub struct PlannedWithdrawalBatch {
    /// Index of the first withdrawal relative to the reconciled queue suffix.
    pub start_index: usize,
    /// FIFO-ordered withdrawals included in this transaction.
    pub withdrawals: Vec<abi::Withdrawal>,
    /// Queue hash expected after the transaction consumes all included withdrawals.
    pub remaining_queue: B256,
    /// Conservative outer transaction gas limit.
    pub gas_limit: u64,
}

impl PlannedWithdrawalBatch {
    fn end_index(&self) -> usize {
        self.start_index + self.withdrawals.len()
    }
}

/// Pure planner that packs queue-ordered withdrawals into a bounded in-flight window.
///
/// Keeping this component independent from RPC and storage makes the queue-hash, gas-budget, and
/// concurrency rules directly unit testable.
#[derive(Debug, Clone, Copy)]
pub struct WithdrawalBatchPlanner {
    limits: WithdrawalBatchLimits,
}

impl WithdrawalBatchPlanner {
    pub fn new(limits: WithdrawalBatchLimits) -> Self {
        Self {
            limits: limits.normalized(),
        }
    }

    /// Plan the next in-flight transaction window for a reconciled queue suffix.
    pub fn plan(&self, withdrawals: &[abi::Withdrawal]) -> Vec<PlannedWithdrawalBatch> {
        let mut batches = Vec::new();
        let mut start = 0;
        let mut in_flight_gas = 0u64;

        while start < withdrawals.len() && batches.len() < self.limits.max_in_flight_batches {
            let remaining_window_gas = self.limits.max_in_flight_gas.saturating_sub(in_flight_gas);
            let batch_budget = self.limits.max_batch_gas.min(remaining_window_gas);
            let first_item_gas = PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS
                .saturating_add(process_withdrawal_item_gas(withdrawals[start].gasLimit));

            // A singleton may exceed the per-transaction limit so an expensive FIFO item cannot
            // block progress. Only the first transaction may exceed the aggregate window limit;
            // later expensive items wait until the next reconciled window.
            if !batches.is_empty() && first_item_gas > remaining_window_gas {
                break;
            }

            let mut end = start;
            let mut gas_limit = PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS;
            while end < withdrawals.len() && end - start < MAX_WITHDRAWALS_PER_PROCESS_BATCH {
                let next_gas = gas_limit
                    .saturating_add(process_withdrawal_item_gas(withdrawals[end].gasLimit));
                if end > start && next_gas > batch_budget {
                    break;
                }

                gas_limit = next_gas;
                end += 1;
            }

            batches.push(PlannedWithdrawalBatch {
                start_index: start,
                withdrawals: withdrawals[start..end].to_vec(),
                remaining_queue: compute_remaining_queue(withdrawals, end),
                gas_limit,
            });
            in_flight_gas = in_flight_gas.saturating_add(gas_limit);
            start = end;
        }

        batches
    }
}

/// Outcome of submitting and confirming one `processWithdrawals` transaction.
enum SubmitOutcome {
    /// The transaction was included on L1 and succeeded.
    Confirmed,
    /// The transaction was included on L1 but reverted — the provided data does
    /// not match the portal's on-chain state.
    Reverted,
    /// The transaction was broadcast but no receipt was obtained in time, or it
    /// failed to send. The next cycle re-reads the on-chain slot hash and
    /// resumes from wherever the portal actually is.
    Unconfirmed,
}

struct PendingWithdrawalBatch {
    batch: PlannedWithdrawalBatch,
    nonce: u64,
    pending: PendingTransactionBuilder<TempoNetwork>,
}

/// Background task that processes withdrawals from the ZonePortal queue on Tempo L1.
///
/// The processor waits for a [`Notify`] signal from the batch submitter (indicating a batch
/// has landed on L1) and then drains the portal's withdrawal queue, slot by slot.
/// A fallback timeout ensures the processor still checks periodically if a notification
/// is missed.
///
/// The processor is idempotent: before submitting a slot it reads the slot's
/// current on-chain hash and trims withdrawals the portal has already consumed,
/// so it can safely run at any time from any state (crash, timeout, restart).
///
/// Withdrawals are gas-bounded into contract batches. Several consecutive-nonce transactions are
/// broadcast without waiting for receipts, then confirmed concurrently. On any uncertain or
/// reverted result, the next cycle reconciles the portal queue before planning more work.
pub struct WithdrawalProcessor {
    config: WithdrawalProcessorConfig,
    provider: DynProvider<TempoNetwork>,
    portal: ZonePortal::ZonePortalInstance<DynProvider<TempoNetwork>, TempoNetwork>,
    store: SharedWithdrawalStore,
    notify: Arc<Notify>,
    repair_notify: Arc<Notify>,
    metrics: WithdrawalProcessorMetrics,
}

impl WithdrawalProcessor {
    /// Create a new withdrawal processor from a shared L1 provider.
    ///
    /// The provider must already include the sequencer wallet for signing.
    pub fn new(
        config: WithdrawalProcessorConfig,
        provider: DynProvider<TempoNetwork>,
        store: SharedWithdrawalStore,
        notify: Arc<Notify>,
        repair_notify: Arc<Notify>,
    ) -> Self {
        let portal = ZonePortal::new(config.portal_address, provider.clone());

        Self {
            config,
            provider,
            portal,
            store,
            notify,
            repair_notify,
            metrics: WithdrawalProcessorMetrics::default(),
        }
    }

    /// Read the current store contents relevant to `slot` under a single lock.
    ///
    /// This keeps the diagnostic fields used in missing-slot logs consistent
    /// with each other and with the batch lookup result.
    fn capture_store_snapshot(&self, slot: u64) -> StoreSnapshot {
        let store = self.store.lock();
        let (batch_count, first_slot, last_slot) = store.summary();
        let (prev_slot, next_slot) = store.neighboring_slots(slot);

        StoreSnapshot {
            batch_count,
            first_slot,
            last_slot,
            prev_slot,
            next_slot,
            withdrawals: store.get_batch(slot).cloned(),
        }
    }

    /// Run the processor loop. This method never returns under normal operation.
    ///
    /// Waits for a notification from the batch submitter (or a fallback timeout) before
    /// checking the L1 withdrawal queue.
    #[instrument(skip_all, fields(portal = %self.config.portal_address))]
    pub async fn run(&mut self) -> eyre::Result<()> {
        info!(l1_rpc = %self.config.l1_rpc_url, "Withdrawal processor started");

        loop {
            tokio::select! {
                _ = self.notify.notified() => {
                    debug!("Woken by batch submission notification");
                }
                _ = tokio::time::sleep(self.config.fallback_poll_interval) => {
                    debug!("Fallback poll interval elapsed");
                }
            }

            if let Err(e) = self.process_queue().await {
                error!(error = %e, "Withdrawal processing cycle failed");
            }
        }
    }

    /// Drain the portal's withdrawal queue on Tempo L1, slot by slot, until the
    /// queue is empty or a withdrawal cannot be processed.
    ///
    /// For each head slot the processor reads the slot's current on-chain hash
    /// and trims withdrawals the portal has already consumed
    /// ([`find_processed_offset`]), so a crash, timeout, or restart mid-slot
    /// resumes exactly where the portal is.
    #[instrument(skip_all)]
    async fn process_queue(&mut self) -> eyre::Result<()> {
        // loop through all the slots
        loop {
            let head_call = self.portal.withdrawalQueueHead();
            let tail_call = self.portal.withdrawalQueueTail();
            let (head, tail): (U256, U256) = tokio::try_join!(head_call.call(), tail_call.call())?;

            let head_val: u64 = head.try_into().map_err(|_| eyre::eyre!("head overflow"))?;
            let tail_val: u64 = tail.try_into().map_err(|_| eyre::eyre!("tail overflow"))?;
            let StoreSnapshot {
                batch_count: store_batch_count,
                first_slot: store_first_slot,
                last_slot: store_last_slot,
                prev_slot: prev_store_slot,
                next_slot: next_store_slot,
                withdrawals,
            } = self.capture_store_snapshot(head_val);
            self.record_queue_metrics(head_val, tail_val, store_batch_count);

            if head_val == tail_val {
                debug!("Withdrawal queue empty, nothing to process");
                return Ok(());
            }

            let pending_slots = tail_val - head_val;
            info!(
                head = head_val,
                tail = tail_val,
                pending_slots,
                "Withdrawal queue has pending slots"
            );

            let withdrawals = match withdrawals {
                Some(w) if !w.is_empty() => w,
                _ => {
                    self.repair_notify.notify_one();
                    warn!(
                        slot = head_val,
                        tail = tail_val,
                        pending_slots,
                        store_batches = store_batch_count,
                        store_first_slot,
                        store_last_slot,
                        prev_store_slot,
                        next_store_slot,
                        "No withdrawal data in store for current head slot"
                    );
                    return Ok(());
                }
            };

            // Read the head slot's current on-chain hash and skip
            // withdrawals the portal has already consumed.
            let slot_hash = self
                .portal
                .withdrawalQueueSlot(U256::from(head_val % WITHDRAWAL_QUEUE_CAPACITY))
                .call()
                .await?;

            if slot_hash == EMPTY_SENTINEL {
                // The slot was fully consumed and head advanced between our reads.
                // Re-check on the next cycle.
                debug!(
                    slot = head_val,
                    "Head slot already consumed; skipping cycle"
                );
                return Ok(());
            }

            let Some(offset) = find_processed_offset(&withdrawals, slot_hash) else {
                error!(
                    slot = head_val,
                    on_chain_slot_hash = %slot_hash,
                    store_queue_hash = %abi::Withdrawal::queue_hash(&withdrawals),
                    "Store data does not match the head slot's on-chain hash; requesting repair"
                );
                self.repair_notify.notify_one();
                return Ok(());
            };

            if offset > 0 {
                info!(
                    slot = head_val,
                    processed = offset,
                    remaining = withdrawals.len() - offset,
                    "Trimmed withdrawals already consumed by the portal"
                );
            }

            let remaining = &withdrawals[offset..];
            if remaining.is_empty() {
                // Defensive: queue_hash never produces B256::ZERO for a pending head
                // slot, but if it happens drop the stale batch and wait for the portal.
                warn!(
                    slot = head_val,
                    "Head slot fully processed but head not advanced"
                );
                self.store.lock().remove_batch(head_val);
                return Ok(());
            }

            let planner = WithdrawalBatchPlanner::new(self.config.batch_limits);
            let planned_batches = planner.plan(remaining);
            let planned_withdrawals = planned_batches
                .last()
                .map_or(0, PlannedWithdrawalBatch::end_index);
            let planned_gas = planned_batches
                .iter()
                .fold(0u64, |total, batch| total.saturating_add(batch.gas_limit));
            info!(
                slot = head_val,
                remaining_withdrawals = remaining.len(),
                planned_withdrawals,
                planned_transactions = planned_batches.len(),
                planned_gas,
                "Processing withdrawal window"
            );
            let slot_started_at = Instant::now();
            let outcome = self
                .submit_and_confirm_batches(head_val, offset, remaining.len(), planned_batches)
                .await?;
            self.record_slot_duration(slot_started_at.elapsed());

            match outcome {
                SubmitOutcome::Confirmed if planned_withdrawals == remaining.len() => {
                    // The entire slot confirmed — safe to remove. Continue the loop to drain any
                    // further pending slots.
                    self.store.lock().remove_batch(head_val);
                    info!(
                        slot = head_val,
                        count = remaining.len(),
                        "Slot fully processed and removed from store"
                    );
                }
                SubmitOutcome::Confirmed => {
                    // The configured in-flight window was smaller than the slot. Re-read the
                    // on-chain suffix before planning the next window.
                    continue;
                }
                SubmitOutcome::Reverted => {
                    self.repair_notify.notify_one();
                    return Ok(());
                }
                SubmitOutcome::Unconfirmed => {
                    // The next cycle re-reads the on-chain slot hash and resumes from wherever the
                    // portal actually is.
                    return Ok(());
                }
            }
        }
    }

    /// Broadcast a window of consecutive-nonce `processWithdrawals` transactions, then wait for
    /// all broadcast receipts concurrently.
    async fn submit_and_confirm_batches(
        &self,
        slot: u64,
        offset: usize,
        total: usize,
        batches: Vec<PlannedWithdrawalBatch>,
    ) -> eyre::Result<SubmitOutcome> {
        let first_nonce = self
            .provider
            .get_transaction_count_with_nonce_key(
                self.config.sequencer_address,
                PROCESS_WITHDRAWAL_NONCE_KEY,
            )
            .await?;
        let mut pending_batches = Vec::with_capacity(batches.len());
        let mut broadcast_failed = false;

        // Broadcast in nonce order without waiting for inclusion. Once accepted by the RPC, all
        // transactions remain concurrently in flight and Tempo's lane-2 nonce ordering preserves
        // the portal queue dependency between them.
        for (batch_index, batch) in batches.into_iter().enumerate() {
            let nonce = first_nonce
                .checked_add(batch_index as u64)
                .ok_or_else(|| eyre::eyre!("processWithdrawals nonce overflow at {first_nonce}"))?;
            let absolute_start = offset + batch.start_index;
            let withdrawal_count = batch.withdrawals.len();
            let has_callback = batch.withdrawals.iter().any(|w| w.gasLimit > 0);

            for (item_index, withdrawal) in batch.withdrawals.iter().enumerate() {
                if withdrawal.gasLimit > MAX_WITHDRAWAL_GAS_LIMIT {
                    warn!(
                        slot,
                        index = absolute_start + item_index,
                        requested_gas_limit = withdrawal.gasLimit,
                        max_gas_limit = MAX_WITHDRAWAL_GAS_LIMIT,
                        "withdrawal callback gas exceeds protocol cap; reserving bounded gas"
                    );
                }
            }

            info!(
                slot,
                batch_index,
                nonce,
                start_index = absolute_start,
                withdrawal_count,
                total,
                gas_limit = batch.gas_limit,
                has_callback,
                expected_remaining_queue = %batch.remaining_queue,
                "📤 Broadcasting withdrawal batch to L1"
            );

            let call = self
                .portal
                .processWithdrawals(batch.withdrawals.clone(), batch.remaining_queue)
                .nonce_key(PROCESS_WITHDRAWAL_NONCE_KEY)
                .nonce(nonce)
                .gas(batch.gas_limit);

            match call.send().await {
                Ok(pending) => {
                    self.metrics
                        .withdrawals_processed_total
                        .increment(withdrawal_count as u64);
                    self.metrics.batches_submitted_total.increment(1);
                    self.metrics
                        .withdrawals_per_batch
                        .record(withdrawal_count as f64);
                    pending_batches.push(PendingWithdrawalBatch {
                        batch,
                        nonce,
                        pending,
                    });
                }
                Err(e) => {
                    self.metrics
                        .withdrawals_failed_total
                        .increment(withdrawal_count as u64);
                    error!(
                        slot,
                        batch_index,
                        nonce,
                        start_index = absolute_start,
                        withdrawal_count,
                        error = %e,
                        "processWithdrawals tx failed to send; stopping at the first nonce gap"
                    );
                    broadcast_failed = true;
                    break;
                }
            }
        }

        let receipt_results =
            join_all(pending_batches.into_iter().map(|pending_batch| async move {
                let tx_hash = *pending_batch.pending.tx_hash();
                let receipt = pending_batch
                    .pending
                    .with_timeout(Some(PROCESS_WITHDRAWAL_CONFIRM_TIMEOUT))
                    .get_receipt()
                    .await;
                (pending_batch.batch, pending_batch.nonce, tx_hash, receipt)
            }))
            .await;

        let mut outcome = if broadcast_failed {
            SubmitOutcome::Unconfirmed
        } else {
            SubmitOutcome::Confirmed
        };

        // Interpret receipts in nonce order even though they were awaited concurrently.
        for (batch, nonce, tx_hash, receipt) in receipt_results {
            let withdrawal_count = batch.withdrawals.len();
            match receipt {
                Ok(receipt) if receipt.status() => {
                    self.metrics
                        .withdrawals_confirmed_total
                        .increment(withdrawal_count as u64);
                    self.metrics.batches_confirmed_total.increment(1);
                    info!(
                        slot,
                        nonce,
                        %tx_hash,
                        start_index = offset + batch.start_index,
                        withdrawal_count,
                        "✅ Withdrawal batch confirmed on L1"
                    );
                }
                Ok(_) => {
                    self.metrics
                        .withdrawals_failed_total
                        .increment(withdrawal_count as u64);
                    self.metrics.withdrawals_reverted_total.increment(1);
                    error!(
                        slot,
                        nonce,
                        %tx_hash,
                        start_index = offset + batch.start_index,
                        withdrawal_count,
                        expected_remaining_queue = %batch.remaining_queue,
                        "processWithdrawals tx reverted; later planned batches are invalid"
                    );
                    outcome = SubmitOutcome::Reverted;
                    break;
                }
                Err(e) => {
                    self.metrics
                        .withdrawals_failed_total
                        .increment(withdrawal_count as u64);
                    error!(
                        slot,
                        nonce,
                        %tx_hash,
                        start_index = offset + batch.start_index,
                        withdrawal_count,
                        expected_remaining_queue = %batch.remaining_queue,
                        error = %e,
                        "processWithdrawals tx not confirmed; queue will be reconciled"
                    );
                    outcome = SubmitOutcome::Unconfirmed;
                    break;
                }
            }
        }

        Ok(outcome)
    }

    fn record_queue_metrics(&mut self, head: u64, tail: u64, store_batch_count: usize) {
        self.metrics.portal_queue_head.set(head as f64);
        self.metrics.portal_queue_tail.set(tail as f64);
        self.metrics
            .portal_queue_pending_slots
            .set((tail.saturating_sub(head)) as f64);
        self.metrics.store_batch_count.set(store_batch_count as f64);
    }

    fn record_slot_duration(&self, duration: Duration) {
        self.metrics
            .slot_processing_duration_seconds
            .record(duration.as_secs_f64());
    }
}

/// Spawn the withdrawal processor as a background task.
///
/// The processor waits for notifications from the batch submitter (via `notify`) and then
/// processes withdrawals from the ZonePortal queue on Tempo L1.
///
/// The `provider` must already include the sequencer wallet for signing L1 transactions.
pub fn spawn_withdrawal_processor(
    config: WithdrawalProcessorConfig,
    provider: DynProvider<TempoNetwork>,
    store: SharedWithdrawalStore,
    notify: Arc<Notify>,
    repair_notify: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut processor =
            WithdrawalProcessor::new(config, provider, store, notify, repair_notify);
        loop {
            if let Err(e) = processor.run().await {
                error!(error = %e, "Withdrawal processor failed, restarting in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::EMPTY_SENTINEL;
    use alloy_primitives::{Bytes, U256, address, keccak256};
    use alloy_provider::{Provider, ProviderBuilder};
    use alloy_sol_types::SolValue;
    use alloy_transport::mock::Asserter;
    use tempo_alloy::TempoNetwork;
    use tokio::time::timeout;

    fn mock_provider(asserter: Asserter) -> DynProvider<TempoNetwork> {
        ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter)
            .erased()
    }

    fn abi_encode_u64(value: u64) -> Bytes {
        Bytes::copy_from_slice(&U256::from(value).to_be_bytes::<32>())
    }

    fn test_withdrawal(to: Address, amount: u128) -> abi::Withdrawal {
        abi::Withdrawal {
            token: address!("0x0000000000000000000000000000000000001000"),
            senderTag: B256::repeat_byte(0x11),
            to,
            amount,
            fee: 0,
            memo: B256::ZERO,
            gasLimit: 0,
            fallbackNonce: 1,
            callbackData: Default::default(),
            encryptedSender: Default::default(),
        }
    }

    #[test]
    fn empty_queue_hash_is_zero() {
        assert_eq!(abi::Withdrawal::queue_hash(&[]), B256::ZERO);
    }

    #[test]
    fn single_withdrawal_queue_hash() {
        let w = test_withdrawal(address!("0x0000000000000000000000000000000000000042"), 1000);
        let hash = abi::Withdrawal::queue_hash(std::slice::from_ref(&w));

        let expected = keccak256((w, EMPTY_SENTINEL).abi_encode_params());
        assert_eq!(hash, expected);
    }

    #[test]
    fn two_withdrawal_queue_hash() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000042"), 100);
        let w1 = test_withdrawal(address!("0x0000000000000000000000000000000000000043"), 200);

        let hash = abi::Withdrawal::queue_hash(&[w0.clone(), w1.clone()]);

        let inner = keccak256((w1, EMPTY_SENTINEL).abi_encode_params());
        let expected = keccak256((w0, inner).abi_encode_params());
        assert_eq!(hash, expected);
    }

    #[test]
    fn withdrawal_hash_requires_param_encoding() {
        let w = abi::Withdrawal {
            token: address!("0x20c0000000000000000000000000000000000000"),
            senderTag: B256::repeat_byte(0x22),
            to: address!("0x70997970c51812dc3a010c7d01b50e0d17dc79c8"),
            amount: 500_000,
            fee: 0,
            memo: B256::ZERO,
            gasLimit: 0,
            fallbackNonce: 1,
            callbackData: Default::default(),
            encryptedSender: Default::default(),
        };

        let tuple_value_hash = keccak256((w.clone(), EMPTY_SENTINEL).abi_encode());
        let param_hash = keccak256((w, EMPTY_SENTINEL).abi_encode_params());

        assert_ne!(
            tuple_value_hash, param_hash,
            "tuple-value encoding must differ from Solidity abi.encode(args...) here"
        );
    }

    #[test]
    fn remaining_queue_single_item_is_hash() {
        let w = test_withdrawal(address!("0x0000000000000000000000000000000000000042"), 1000);
        let expected = abi::Withdrawal::queue_hash(std::slice::from_ref(&w));
        assert_eq!(compute_remaining_queue(&[w], 0), expected);
    }

    #[test]
    fn remaining_queue_all_consumed() {
        let w = test_withdrawal(address!("0x0000000000000000000000000000000000000042"), 1000);
        assert_eq!(
            compute_remaining_queue(std::slice::from_ref(&w), 1),
            B256::ZERO
        );
        assert_eq!(compute_remaining_queue(&[w], 5), B256::ZERO);
    }

    #[test]
    fn remaining_queue_partial() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000042"), 100);
        let w1 = test_withdrawal(address!("0x0000000000000000000000000000000000000043"), 200);
        let w2 = test_withdrawal(address!("0x0000000000000000000000000000000000000044"), 300);

        let remaining = compute_remaining_queue(&[w0, w1.clone(), w2.clone()], 1);
        let expected = abi::Withdrawal::queue_hash(&[w1, w2]);
        assert_eq!(remaining, expected);
    }

    #[test]
    fn callback_tx_gas_limit_is_capped_below_l1_block_limit() {
        let at_cap = PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS
            + process_withdrawal_item_gas(MAX_WITHDRAWAL_GAS_LIMIT);
        let over_cap = PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS
            + process_withdrawal_item_gas(MAX_WITHDRAWAL_GAS_LIMIT + 1);

        assert_eq!(over_cap, at_cap);
        assert_eq!(at_cap, MAX_PROCESS_WITHDRAWAL_TX_GAS);
        assert_eq!(
            at_cap,
            MAX_WITHDRAWAL_GAS_LIMIT
                + PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS
                + PROCESS_WITHDRAWAL_ITEM_OVERHEAD_GAS
                + MAX_WITHDRAWAL_GAS_LIMIT.div_ceil(63)
        );
        assert!(at_cap < 30_000_000);
    }

    fn planner(
        max_batch_gas: u64,
        max_in_flight_batches: usize,
        max_in_flight_gas: u64,
    ) -> WithdrawalBatchPlanner {
        WithdrawalBatchPlanner::new(WithdrawalBatchLimits {
            max_batch_gas,
            max_in_flight_batches,
            max_in_flight_gas,
        })
    }

    fn simple_withdrawals(count: usize) -> Vec<abi::Withdrawal> {
        (0..count)
            .map(|i| test_withdrawal(Address::with_last_byte((i + 1) as u8), (i + 1) as u128))
            .collect()
    }

    #[test]
    fn planner_packs_by_gas_and_preserves_full_queue_suffixes() {
        let withdrawals = simple_withdrawals(3);
        let one = PROCESS_WITHDRAWAL_ITEM_OVERHEAD_GAS;
        let plans =
            planner(PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS + 2 * one, 4, 10_000_000).plan(&withdrawals);

        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].start_index, 0);
        assert_eq!(plans[0].withdrawals.len(), 2);
        assert_eq!(plans[0].withdrawals[0].amount, withdrawals[0].amount);
        assert_eq!(plans[0].withdrawals[1].amount, withdrawals[1].amount);
        assert_eq!(
            plans[0].remaining_queue,
            abi::Withdrawal::queue_hash(&withdrawals[2..])
        );
        assert_eq!(plans[1].start_index, 2);
        assert_eq!(plans[1].withdrawals.len(), 1);
        assert_eq!(plans[1].withdrawals[0].amount, withdrawals[2].amount);
        assert_eq!(plans[1].remaining_queue, B256::ZERO);
    }

    #[test]
    fn planner_bounds_the_in_flight_gas_window() {
        let withdrawals = simple_withdrawals(10);
        let one_batch = PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS + PROCESS_WITHDRAWAL_ITEM_OVERHEAD_GAS;
        let plans = planner(one_batch, 10, one_batch * 2).plan(&withdrawals);

        assert_eq!(plans.len(), 2);
        assert_eq!(
            plans.iter().map(|batch| batch.gas_limit).sum::<u64>(),
            one_batch * 2
        );
        assert_eq!(
            plans[1].remaining_queue,
            abi::Withdrawal::queue_hash(&withdrawals[2..])
        );
    }

    #[test]
    fn planner_bounds_the_number_of_in_flight_transactions() {
        let withdrawals = simple_withdrawals(5);
        let one_batch = PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS + PROCESS_WITHDRAWAL_ITEM_OVERHEAD_GAS;
        let plans = planner(one_batch, 2, u64::MAX).plan(&withdrawals);

        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].start_index, 0);
        assert_eq!(plans[1].start_index, 1);
        assert_eq!(
            plans[1].remaining_queue,
            abi::Withdrawal::queue_hash(&withdrawals[2..])
        );
    }

    #[test]
    fn planner_emits_an_oversized_head_as_a_singleton() {
        let mut withdrawals = simple_withdrawals(2);
        withdrawals[0].gasLimit = MAX_WITHDRAWAL_GAS_LIMIT;
        let plans = planner(1_000_000, 4, 20_000_000).plan(&withdrawals);

        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].withdrawals.len(), 1);
        assert!(plans[0].gas_limit > 1_000_000);
        assert_eq!(
            plans[0].remaining_queue,
            abi::Withdrawal::queue_hash(&withdrawals[1..])
        );
    }

    #[test]
    fn planner_defers_an_oversized_item_that_does_not_fit_the_current_window() {
        let mut withdrawals = simple_withdrawals(2);
        withdrawals[1].gasLimit = MAX_WITHDRAWAL_GAS_LIMIT;
        let simple_gas = PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS + PROCESS_WITHDRAWAL_ITEM_OVERHEAD_GAS;
        let callback_gas = PROCESS_WITHDRAWAL_TX_OVERHEAD_GAS
            + process_withdrawal_item_gas(MAX_WITHDRAWAL_GAS_LIMIT);
        let planner = planner(simple_gas, 4, simple_gas + callback_gas - 1);

        let first_window = planner.plan(&withdrawals);
        assert_eq!(first_window.len(), 1);
        assert_eq!(first_window[0].withdrawals[0].amount, withdrawals[0].amount);

        let next_window = planner.plan(&withdrawals[1..]);
        assert_eq!(next_window.len(), 1);
        assert_eq!(next_window[0].gas_limit, callback_gas);
    }

    #[test]
    fn planner_caps_calldata_even_when_gas_budget_is_large() {
        let withdrawals = simple_withdrawals(MAX_WITHDRAWALS_PER_PROCESS_BATCH + 1);
        let plans = planner(u64::MAX, 2, u64::MAX).plan(&withdrawals);

        assert_eq!(plans.len(), 2);
        assert_eq!(
            plans[0].withdrawals.len(),
            MAX_WITHDRAWALS_PER_PROCESS_BATCH
        );
        assert_eq!(plans[1].withdrawals.len(), 1);
        assert_eq!(plans[1].remaining_queue, B256::ZERO);
    }

    #[test]
    fn planner_normalizes_zero_limits_without_stalling() {
        let withdrawals = simple_withdrawals(1);
        let plans = planner(0, 0, 0).plan(&withdrawals);

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].withdrawals.len(), 1);
        assert_eq!(plans[0].withdrawals[0].amount, withdrawals[0].amount);
    }

    #[test]
    fn store_operations() {
        let mut store = WithdrawalStore::new();
        let w = test_withdrawal(address!("0x0000000000000000000000000000000000000042"), 100);

        assert_eq!(store.batch_count(), 0);
        assert!(!store.has_batch(0));

        store.add_withdrawal(0, w.clone());
        assert!(store.has_batch(0));
        assert_eq!(store.batch_count(), 1);
        assert_eq!(store.get_batch(0).unwrap().len(), 1);

        store.add_withdrawal(0, w);
        assert_eq!(store.get_batch(0).unwrap().len(), 2);

        store.remove_batch(0);
        assert!(!store.has_batch(0));
        assert_eq!(store.batch_count(), 0);
    }

    #[test]
    fn store_slot_index_must_match_portal_tail() {
        // Demonstrates that withdrawals must be stored under the portal's actual
        // queue tail index. If the monitor starts with tail=0 but the portal is
        // at tail=5, withdrawals end up in slot 0 while the withdrawal processor
        // looks for them in slot 5.
        let mut store = WithdrawalStore::new();
        let w = test_withdrawal(address!("0x0000000000000000000000000000000000000042"), 100);

        // Simulate storing under the wrong slot (tail=0 when portal is at 5).
        store.add_withdrawal(0, w.clone());
        assert!(store.has_batch(0));
        assert!(
            !store.has_batch(5),
            "withdrawal processor would look at slot 5 and find nothing"
        );

        // Correct: store under the portal's actual tail.
        let portal_tail = 5u64;
        store.add_withdrawal(portal_tail, w);
        assert!(store.has_batch(portal_tail));
    }

    #[test]
    fn store_add_batch() {
        let mut store = WithdrawalStore::new();
        let addr = address!("0x0000000000000000000000000000000000000042");
        let batch: Vec<_> = (0..3).map(|i| test_withdrawal(addr, i * 100)).collect();

        store.add_batch(0, batch);
        assert!(store.has_batch(0));
        assert_eq!(store.get_batch(0).unwrap().len(), 3);

        // Calling add_batch again replaces existing data (idempotent).
        let more: Vec<_> = (0..2).map(|i| test_withdrawal(addr, i * 200)).collect();
        store.add_batch(0, more);
        assert_eq!(store.get_batch(0).unwrap().len(), 2);

        store.add_batch(1, vec![test_withdrawal(addr, 999)]);
        assert_eq!(store.batch_count(), 2);
    }

    #[test]
    fn store_replace_batches_reconciles_authoritative_view() {
        let mut store = WithdrawalStore::new();
        let addr = address!("0x0000000000000000000000000000000000000042");

        store.add_batch(0, vec![test_withdrawal(addr, 100)]);
        store.add_batch(9, vec![test_withdrawal(addr, 900)]);

        let mut reconciled = BTreeMap::new();
        reconciled.insert(5, vec![test_withdrawal(addr, 500)]);
        reconciled.insert(6, vec![test_withdrawal(addr, 600)]);

        store.replace_batches(reconciled);

        assert!(!store.has_batch(0));
        assert!(!store.has_batch(9));
        assert!(store.has_batch(5));
        assert!(store.has_batch(6));
        assert_eq!(store.batch_count(), 2);
    }

    fn abi_encode_b256(value: B256) -> Bytes {
        Bytes::copy_from_slice(value.as_slice())
    }

    fn test_processor(
        l1: Asserter,
        store: SharedWithdrawalStore,
        repair_notify: Arc<Notify>,
    ) -> WithdrawalProcessor {
        let config = WithdrawalProcessorConfig {
            portal_address: address!("0x7069DeC4E64Fd07334A0933eDe836C17259c9B23"),
            l1_rpc_url: "http://unused.test".to_string(),
            fallback_poll_interval: Duration::from_secs(1),
            sequencer_address: Address::repeat_byte(0x77),
            batch_limits: WithdrawalBatchLimits::default(),
        };
        WithdrawalProcessor::new(
            config,
            mock_provider(l1),
            store,
            Arc::new(Notify::new()),
            repair_notify,
        )
    }

    #[tokio::test]
    async fn process_queue_requests_monitor_resync_when_head_slot_missing() {
        let l1 = Asserter::new();
        l1.push_success(&abi_encode_u64(51));
        l1.push_success(&abi_encode_u64(71));

        let repair_notify = Arc::new(Notify::new());
        let mut processor = test_processor(
            l1.clone(),
            SharedWithdrawalStore::new(),
            repair_notify.clone(),
        );

        processor.process_queue().await.unwrap();

        timeout(Duration::from_millis(50), repair_notify.notified())
            .await
            .expect("missing head slot should request a monitor resync");
        assert!(l1.read_q().is_empty());
    }

    #[tokio::test]
    async fn process_queue_requests_repair_when_store_data_mismatches_slot_hash() {
        let l1 = Asserter::new();
        // head = 5, tail = 6, slot hash that matches no suffix of the stored batch.
        l1.push_success(&abi_encode_u64(5));
        l1.push_success(&abi_encode_u64(6));
        l1.push_success(&abi_encode_b256(B256::repeat_byte(0xde)));

        let store = SharedWithdrawalStore::new();
        store.lock().add_batch(
            5,
            vec![test_withdrawal(
                address!("0x0000000000000000000000000000000000000042"),
                100,
            )],
        );

        let repair_notify = Arc::new(Notify::new());
        let mut processor = test_processor(l1.clone(), store, repair_notify.clone());

        processor.process_queue().await.unwrap();

        timeout(Duration::from_millis(50), repair_notify.notified())
            .await
            .expect("mismatched slot hash should request a monitor resync");
        assert!(l1.read_q().is_empty());
    }

    #[tokio::test]
    async fn process_queue_skips_cycle_when_head_slot_already_consumed() {
        let l1 = Asserter::new();
        // head = 5, tail = 6, slot already contains EMPTY_SENTINEL (head advanced
        // between our head read and the slot read).
        l1.push_success(&abi_encode_u64(5));
        l1.push_success(&abi_encode_u64(6));
        l1.push_success(&abi_encode_b256(EMPTY_SENTINEL));

        let store = SharedWithdrawalStore::new();
        store.lock().add_batch(
            5,
            vec![test_withdrawal(
                address!("0x0000000000000000000000000000000000000042"),
                100,
            )],
        );

        let repair_notify = Arc::new(Notify::new());
        let mut processor = test_processor(l1.clone(), store.clone(), repair_notify.clone());

        processor.process_queue().await.unwrap();

        // No repair requested and the batch stays in the store.
        assert!(
            timeout(Duration::from_millis(50), repair_notify.notified())
                .await
                .is_err()
        );
        assert!(store.lock().has_batch(5));
        assert!(l1.read_q().is_empty());
    }
}
