//! Zone L2 block monitor with integrated batch submission.
//!
//! Watches the **Zone L2** chain for new blocks, collecting withdrawal events and
//! reading on-chain state to produce [`BatchData`]. The current prover-backed
//! path submits one canonical Zone block per L1 batch because the Zone payload
//! builder finalizes withdrawals at the end of every block.
//!
//! ## Batch granularity
//!
//! Each Zone block is submitted as its own `submitBatch` range. This matches the
//! canonical block body, which already contains exactly one
//! `ZoneOutbox.finalizeWithdrawalBatch` system transaction as the final
//! transaction of that block. Reintroducing multi-block submissions requires the
//! payload builder and prover to agree on where withdrawal finalization happens.
//!
//! ## EIP-2935 and ancestry mode
//!
//! The portal verifies `tempoBlockNumber` via EIP-2935, which stores the last 8192
//! block hashes. When `tempoBlockNumber` is within this window the batch submitter
//! uses **direct mode** (reading the hash straight from EIP-2935). If the zone
//! falls behind (e.g. sequencer downtime >2 hours), the submitter automatically
//! switches to **ancestry mode**: it supplies a recent L1 block number that IS
//! within the EIP-2935 window, and the proof must include a block header chain
//! linking that anchor back to `tempoBlockNumber`.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use alloy_primitives::{Address, B256, U256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_rpc_client::RpcClient;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{ContractError, SolInterface as _};
use alloy_transport::layers::RetryBackoffLayer;
use eyre::{Result, WrapErr};
use tempo_alloy::TempoNetwork;
use tokio::sync::Notify;
use tracing::{error, info, instrument, warn};

use crate::{
    abi::{self, TempoState, ZoneInbox, ZoneOutbox, ZonePortal},
    rpc::rpc_connection_config,
    settlement::{
        BatchAnchorConfig, BatchData, BatchProofSource, BatchSubmitter, LOG_QUERY_BLOCK_CHUNK,
        PendingProverWitness, ProverWitnessSource, UnprovenBatchData, ZoneBlockSnapshot,
        derive_zone_block_hash_for_range, fetch_slot_withdrawals, log_query_ranges,
        resolve_zone_block_number_by_hash,
    },
    withdrawals::SharedWithdrawalStore,
};

/// Maximum number of times to retry a failed batch submission before resyncing.
const MAX_RETRIES: u32 = 3;

/// Initial delay between retries (doubles on each attempt).
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(2);

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn metric_u64(value: u64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::INFINITY)
}

fn metric_usize(value: usize) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::INFINITY)
}

#[derive(Debug, Clone, Default)]
struct FundsLedger {
    tokens: BTreeMap<Address, TokenFunds>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TokenFunds {
    unprocessed_deposits: U256,
    pending_refunds: U256,
    pending_withdrawals: U256,
}

impl FundsLedger {
    fn add_unprocessed_deposit(&mut self, token: Address, amount: u128) -> Result<()> {
        Self::add_amount(
            &mut self.entry_mut(token).unprocessed_deposits,
            U256::from(amount),
            "unprocessed deposit liability overflow",
        )
    }

    fn add_pending_refund(&mut self, token: Address, amount: u128) -> Result<()> {
        Self::add_amount(
            &mut self.entry_mut(token).pending_refunds,
            U256::from(amount),
            "pending refund liability overflow",
        )
    }

    fn claim_refund(&mut self, token: Address, amount: u128) -> Result<()> {
        let funds = self.entry_mut(token);
        let amount = U256::from(amount);
        let next = funds.pending_refunds.checked_sub(amount).ok_or_else(|| {
            eyre::eyre!(
                "portal refund claims exceed pending refunds for token {token}: claim={amount}, pending={}",
                funds.pending_refunds
            )
        })?;
        funds.pending_refunds = next;
        Ok(())
    }

    fn add_withdrawals(&mut self, withdrawals: &[abi::Withdrawal]) -> Result<()> {
        for withdrawal in withdrawals {
            let amount = U256::from(withdrawal.amount);
            let fee = U256::from(withdrawal.fee);
            let liability = amount.checked_add(fee).ok_or_else(|| {
                eyre::eyre!(
                    "withdrawal liability overflow for token {}",
                    withdrawal.token
                )
            })?;
            Self::add_amount(
                &mut self.entry_mut(withdrawal.token).pending_withdrawals,
                liability,
                "pending withdrawal liability overflow",
            )?;
        }
        Ok(())
    }

    fn liability(funds: &TokenFunds) -> Result<U256> {
        let deposits_and_refunds = funds
            .unprocessed_deposits
            .checked_add(funds.pending_refunds)
            .ok_or_else(|| eyre::eyre!("token funds liability overflow"))?;
        deposits_and_refunds
            .checked_add(funds.pending_withdrawals)
            .ok_or_else(|| eyre::eyre!("token funds liability overflow"))
    }

    fn iter(&self) -> impl Iterator<Item = (&Address, &TokenFunds)> {
        self.tokens.iter()
    }

    fn entry_mut(&mut self, token: Address) -> &mut TokenFunds {
        self.tokens.entry(token).or_default()
    }

    fn add_amount(field: &mut U256, amount: U256, context: &'static str) -> Result<()> {
        let next = (*field)
            .checked_add(amount)
            .ok_or_else(|| eyre::eyre!(context))?;
        *field = next;
        Ok(())
    }
}

/// Configuration for the [`ZoneMonitor`].
#[derive(Debug, Clone)]
pub struct ZoneMonitorConfig {
    /// ZoneOutbox contract address on Zone L2.
    pub outbox_address: Address,
    /// ZoneInbox contract address on Zone L2.
    pub inbox_address: Address,
    /// TempoState predeploy address on Zone L2 (usually [`abi::TEMPO_STATE_ADDRESS`]).
    pub tempo_state_address: Address,
    /// Zone L2 RPC URL.
    pub zone_rpc_url: String,
    /// Interval between WebSocket reconnection attempts for the zone RPC client.
    pub retry_connection_interval: Duration,
    /// How often to poll the zone L2 for new blocks (cheap RPC call).
    pub poll_interval: Duration,
    /// Maximum time to accumulate zone blocks before submitting a batch to L1.
    /// Blocks are aggregated during this window to reduce L1 tx count.
    /// A batch is submitted early if pending withdrawals are detected.
    pub batch_interval: Duration,
    /// ZonePortal contract address on Tempo L1.
    pub portal_address: Address,
    /// EIP-2935 history and safety-margin limits used by the batch submitter.
    pub batch_anchor_config: BatchAnchorConfig,
    /// Witness source used to turn unproven batch proposals into submit-ready
    /// [`BatchData`] before local native proof signing.
    pub prover_witness_source: Arc<dyn ProverWitnessSource>,
    /// Sequencer key used to produce native local verifier proofs for witnessed
    /// batches that do not carry externally produced proof material.
    pub native_proof_signer: PrivateKeySigner,
}

/// Monitors the Zone L2 chain for new blocks, aggregates them into batches, and
/// submits to the ZonePortal on L1.
///
/// Multiple zone blocks are combined into a single `submitBatch` call whenever
/// possible, reducing L1 transaction count. Local state only advances after a
/// successful L1 submission. On repeated failures the monitor resyncs from the
/// portal's on-chain `blockHash()`.
pub struct ZoneMonitor {
    config: ZoneMonitorConfig,
    /// Metrics for zone observation and L1 batch submission.
    metrics: crate::metrics::ZoneMonitorMetrics,
    /// Read-only HTTP provider pointed at the **Zone L2** RPC node.
    provider: DynProvider<TempoNetwork>,
    /// ZoneOutbox contract on **Zone L2** — source of `WithdrawalRequested` and
    /// `BatchFinalized` events.
    outbox: ZoneOutbox::ZoneOutboxInstance<DynProvider<TempoNetwork>, TempoNetwork>,
    /// ZoneInbox contract on **Zone L2** — queried for the processed deposit queue hash.
    inbox: ZoneInbox::ZoneInboxInstance<DynProvider<TempoNetwork>, TempoNetwork>,
    /// TempoState predeploy on **Zone L2** — provides the latest Tempo L1 block number
    /// as seen by the zone.
    tempo_state: TempoState::TempoStateInstance<DynProvider<TempoNetwork>, TempoNetwork>,
    /// Shared store for withdrawal data, written here and consumed by the
    /// [`WithdrawalProcessor`](crate::withdrawals::WithdrawalProcessor) on **Tempo L1**.
    withdrawal_store: SharedWithdrawalStore,
    /// Batch submitter for posting batches to the ZonePortal on **Tempo L1**.
    batch_submitter: BatchSubmitter,
    /// Notifier for the withdrawal processor — signalled after each successful
    /// batch submission so it can process newly enqueued withdrawal slots.
    withdrawal_notify: Arc<Notify>,
    /// Notifier from the withdrawal processor when the current portal head slot
    /// is missing from the in-memory store and a full portal resync is needed.
    repair_notify: Arc<Notify>,
    /// Last **Zone L2** block number that was successfully submitted to L1.
    last_submitted_zone_block: u64,
    /// Deposit queue hash from the previous block, used to construct the
    /// [`DepositQueueTransition`](crate::abi::DepositQueueTransition) for each batch.
    prev_processed_deposit_hash: B256,
    /// Deposit counter from the previous batch, used to construct the
    /// [`DepositQueueTransition`](crate::abi::DepositQueueTransition) for each batch.
    prev_processed_deposit_number: u64,
    /// Previous zone block hash, used as `prev_block_hash` in [`BatchData`].
    /// Initialized from the portal's on-chain `blockHash()` at startup.
    prev_zone_block_hash: B256,
    /// Tracks the portal's withdrawal queue tail position.
    /// The withdrawal store keys must match the portal's queue slot indices
    /// (not the L2 outbox's internal `withdrawalBatchIndex`). This counter is
    /// initialized from the portal’s on-chain `withdrawalQueueTail()` at startup,
    /// and incremented each time a batch with a non-zero
    /// `withdrawal_queue_hash` is successfully submitted to L1.
    portal_withdrawal_queue_tail: u64,
    /// Most recent zone block observed from the L2 RPC.
    latest_observed_zone_block: u64,
}

impl ZoneMonitor {
    /// Create a new zone monitor with integrated batch submission.
    ///
    /// Builds a read-only HTTP provider (no wallet) pointed at the Zone L2 RPC,
    /// instantiates the on-chain contract handles, and creates a [`BatchSubmitter`]
    /// backed by the shared `l1_provider` for posting batches to the ZonePortal on L1.
    pub async fn new(
        config: ZoneMonitorConfig,
        l1_provider: DynProvider<TempoNetwork>,
        withdrawal_store: SharedWithdrawalStore,
        withdrawal_notify: Arc<Notify>,
        repair_notify: Arc<Notify>,
    ) -> Result<Self> {
        let zone_rpc_url = config.zone_rpc_url.clone();
        let retry_layer = RetryBackoffLayer::new(
            u32::MAX,
            duration_millis_u64(config.retry_connection_interval),
            u64::MAX,
        );
        let client = RpcClient::builder()
            .layer(retry_layer)
            .connect_with_config(
                &config.zone_rpc_url,
                rpc_connection_config(config.retry_connection_interval),
            )
            .await
            .wrap_err_with(|| format!("failed to connect to Zone RPC at {zone_rpc_url}"))?;
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_client(client)
            .erased();

        Self::new_with_provider(
            config,
            provider,
            l1_provider,
            withdrawal_store,
            withdrawal_notify,
            repair_notify,
        )
        .await
    }

    async fn new_with_provider(
        config: ZoneMonitorConfig,
        provider: DynProvider<TempoNetwork>,
        l1_provider: DynProvider<TempoNetwork>,
        withdrawal_store: SharedWithdrawalStore,
        withdrawal_notify: Arc<Notify>,
        repair_notify: Arc<Notify>,
    ) -> Result<Self> {
        let metrics = crate::metrics::ZoneMonitorMetrics::default();
        let outbox = ZoneOutbox::new(config.outbox_address, provider.clone());
        let inbox = ZoneInbox::new(config.inbox_address, provider.clone());
        let tempo_state = TempoState::new(config.tempo_state_address, provider.clone());

        let genesis_tempo_block_number: u64 =
            ZonePortal::new(config.portal_address, l1_provider.clone())
                .genesisTempoBlockNumber()
                .call()
                .await
                .wrap_err("failed to read genesisTempoBlockNumber during zone monitor startup")?;

        let batch_submitter = BatchSubmitter::with_anchor_config_and_witness_source(
            config.portal_address,
            l1_provider,
            genesis_tempo_block_number,
            config.batch_anchor_config,
            config.native_proof_signer.clone(),
            config.prover_witness_source.clone(),
        );

        let (prev_zone_block_hash, portal_withdrawal_queue_tail) = tokio::try_join!(
            batch_submitter.read_portal_block_hash(),
            batch_submitter.read_portal_withdrawal_queue_tail(),
        )
        .wrap_err("failed to read portal state during zone monitor startup")?;

        let last_submitted_zone_block =
            Self::resolve_zone_block_number(&provider, prev_zone_block_hash).await;
        let prev_processed_deposit_hash = Self::read_processed_deposit_hash_at_block(
            &inbox,
            last_submitted_zone_block,
            B256::ZERO,
        )
        .await;
        let prev_processed_deposit_number =
            Self::read_processed_deposit_number_at_block(&inbox, last_submitted_zone_block).await;

        info!(
            last_submitted_zone_block,
            %prev_zone_block_hash,
            %prev_processed_deposit_hash,
            prev_processed_deposit_number,
            portal_withdrawal_queue_tail,
            "Initialized from portal state"
        );

        metrics
            .latest_zone_block_observed
            .set(metric_u64(last_submitted_zone_block));
        metrics
            .latest_zone_block_submitted_to_l1
            .set(metric_u64(last_submitted_zone_block));
        metrics.zone_to_l1_submission_lag_blocks.set(0.0);

        let monitor = Self {
            config,
            metrics,
            provider,
            outbox,
            inbox,
            tempo_state,
            withdrawal_store,
            batch_submitter,
            withdrawal_notify,
            repair_notify,
            last_submitted_zone_block,
            prev_processed_deposit_hash,
            prev_processed_deposit_number,
            prev_zone_block_hash,
            portal_withdrawal_queue_tail,
            latest_observed_zone_block: last_submitted_zone_block,
        };

        // Restore pending withdrawal data from zone L2 events so the
        // withdrawal processor can pick up where it left off.
        monitor
            .restore_pending_withdrawals_from_chain()
            .await
            .wrap_err("failed to restore pending withdrawals during zone monitor startup")?;

        Ok(monitor)
    }

    /// Run the monitor loop. This method never returns under normal operation.
    ///
    /// Polls the zone L2 frequently (`poll_interval`) but only submits a batch
    /// to L1 when:
    /// - The `batch_interval` deadline has elapsed, OR
    /// - Pending withdrawals are detected (flush immediately for user experience)
    #[instrument(skip_all, fields(
        outbox = %self.config.outbox_address,
        inbox = %self.config.inbox_address,
    ))]
    pub async fn run(&mut self) -> Result<()> {
        info!(
            zone_rpc = %self.config.zone_rpc_url,
            batch_interval = ?self.config.batch_interval,
            poll_interval = ?self.config.poll_interval,
            "Zone monitor started"
        );

        let mut poll = tokio::time::interval(self.config.poll_interval);
        let mut batch_deadline = tokio::time::Instant::now();

        loop {
            tokio::select! {
                _ = poll.tick() => {}
                _ = self.repair_notify.notified() => {
                    self.repair_missing_withdrawal_slot().await;
                    continue;
                }
            }

            let Ok(latest_zone_block) = self.provider.get_block_number().await else {
                continue;
            };
            self.record_observed_zone_block(latest_zone_block);
            if latest_zone_block <= self.last_submitted_zone_block {
                continue;
            }

            let deadline_elapsed = tokio::time::Instant::now() >= batch_deadline;
            // Skip the eth_getLogs call when we'd submit anyway.
            if !deadline_elapsed && !self.has_pending_withdrawals(latest_zone_block).await {
                continue;
            }

            let Some(from) = self.last_submitted_zone_block.checked_add(1) else {
                error!("last submitted zone block overflowed u64");
                continue;
            };
            if let Err(e) = self.process_block_range(from, latest_zone_block).await {
                error!(from, to = latest_zone_block, error = %e, "Failed to process zone block range");
                continue;
            }

            let now = tokio::time::Instant::now();
            batch_deadline = match now.checked_add(self.config.batch_interval) {
                Some(deadline) => deadline,
                None => {
                    warn!(
                        batch_interval_ms = duration_millis_u64(self.config.batch_interval),
                        "batch interval overflowed tokio::time::Instant; submitting on next monitor tick"
                    );
                    now
                }
            };
        }
    }

    /// Rebuild the in-memory withdrawal store from authoritative chain state.
    ///
    /// The L1 portal only stores queue hashes, so the monitor reconstructs the
    /// pending withdrawal payloads from L1 + zone-L2 events and replaces the
    /// local store with that result. Used during startup and after a portal
    /// resync when local withdrawal data may be stale or missing.
    async fn restore_pending_withdrawals_from_chain(&self) -> Result<()> {
        let pending = match self
            .batch_submitter
            .fetch_pending_withdrawals(&self.provider, self.config.outbox_address)
            .await
        {
            Ok(pending) => pending,
            Err(err) => {
                self.metrics
                    .withdrawal_store_restore_failure_total
                    .increment(1);
                return Err(err);
            }
        };
        let restored_withdrawals = pending.values().map(Vec::len).sum::<usize>();
        let reconciled_first_slot = pending.keys().next().copied();
        let reconciled_last_slot = pending.keys().next_back().copied();

        let mut store = self.withdrawal_store.lock();
        let (previous_slots, previous_first_slot, previous_last_slot) = store.summary();
        store.replace_batches(pending);
        let reconciled_slots = store.batch_count();
        drop(store);

        if reconciled_slots > 0 {
            info!(
                previous_slots,
                previous_first_slot,
                previous_last_slot,
                reconciled_slots,
                reconciled_first_slot,
                reconciled_last_slot,
                restored_withdrawals,
                "Restored pending withdrawals from chain"
            );
            self.withdrawal_notify.notify_one();
        } else if previous_slots > 0 {
            info!(
                previous_slots,
                previous_first_slot,
                previous_last_slot,
                "Cleared stale withdrawal batches after restoring pending withdrawals from chain"
            );
        }

        Ok(())
    }

    /// Repair monitor state after the withdrawal processor reports a missing head slot.
    ///
    /// This intentionally goes through a full portal resync rather than only
    /// rebuilding the withdrawal store. An ambiguous `submitBatch` outcome can
    /// leave both the portal anchor and the in-memory withdrawal data stale, so
    /// the monitor first reloads the portal-confirmed anchor and then rebuilds
    /// pending withdrawals from chain state.
    async fn repair_missing_withdrawal_slot(&mut self) {
        warn!("Withdrawal processor reported a missing portal head slot");
        self.resync_from_portal().await;
    }

    /// Check if any zone blocks since `last_submitted_zone_block` contain finalized
    /// withdrawal batches that need to be submitted to L1.
    ///
    /// `pendingWithdrawalsCount()` is always 0 on committed blocks because
    /// `finalizeWithdrawalBatch` runs as the last tx in every zone block. The
    /// correct signal is `BatchFinalized` events with non-zero withdrawal hashes.
    async fn has_pending_withdrawals(&self, latest_block: u64) -> bool {
        let Some(from) = self.last_submitted_zone_block.checked_add(1) else {
            return false;
        };
        for (chunk_from, chunk_to) in log_query_ranges(from, latest_block) {
            match self
                .outbox
                .BatchFinalized_filter()
                .from_block(chunk_from)
                .to_block(chunk_to)
                .query()
                .await
            {
                Ok(events) => {
                    if events
                        .iter()
                        .any(|(event, _)| !event.withdrawalQueueHash.is_zero())
                    {
                        return true;
                    }
                }
                Err(e) => {
                    warn!(
                        from = chunk_from,
                        to = chunk_to,
                        error = %e,
                        "Failed to check for finalized withdrawal batches"
                    );
                    return false;
                }
            }
        }

        false
    }

    /// Process pending zone blocks as one prover-backed batch per canonical block.
    ///
    /// The canonical payload builder appends `finalizeWithdrawalBatch` to every
    /// block. Submitting multi-block ranges would therefore put finalization in
    /// intermediate witness blocks, which prover-core correctly rejects.
    #[instrument(skip(self), fields(from, to))]
    async fn process_block_range(&mut self, from: u64, to: u64) -> Result<()> {
        let block_count = to
            .checked_sub(from)
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| eyre::eyre!("invalid zone block range {from}..={to}"))?;
        info!(from, to, block_count, "Processing zone block range");

        let mut block = from;
        while block <= to {
            let block_state = self.fetch_block_snapshot(block, block).await?;
            self.process_block_range_single(block, block, block_state)
                .await?;
            block = match block.checked_add(1) {
                Some(next) => next,
                None => break,
            };
        }

        Ok(())
    }

    /// Process a block range as a single batch submission (direct or ancestry mode).
    async fn process_block_range_single(
        &mut self,
        from: u64,
        to: u64,
        end_state: ZoneBlockSnapshot,
    ) -> Result<()> {
        let all_withdrawals =
            fetch_slot_withdrawals(&self.outbox, &self.provider, from, to).await?;
        let withdrawal_queue_hash = abi::Withdrawal::queue_hash(&all_withdrawals);

        if !all_withdrawals.is_empty() {
            info!(
                from,
                to,
                count = all_withdrawals.len(),
                withdrawal_queue_hash = %withdrawal_queue_hash,
                "Collected finalized withdrawals from zone"
            );
        }

        let batch_data = UnprovenBatchData {
            tempo_block_number: end_state.tempo_block_number,
            prev_block_hash: self.prev_zone_block_hash,
            next_block_hash: end_state.block_hash,
            prev_processed_deposit_hash: self.prev_processed_deposit_hash,
            next_processed_deposit_hash: end_state.processed_deposit_hash,
            prev_deposit_number: self.prev_processed_deposit_number,
            next_deposit_number: end_state.processed_deposit_number,
            withdrawal_queue_hash,
        };
        let batch_data = self.attach_prover_witness(batch_data, from, to).await?;

        self.submit_batch_with_retry(&batch_data, to, all_withdrawals)
            .await?;

        Ok(())
    }

    async fn attach_prover_witness(
        &self,
        batch: UnprovenBatchData,
        from_zone_block: u64,
        to_zone_block: u64,
    ) -> Result<BatchData> {
        Ok(
            batch.with_proof(BatchProofSource::ProverWitness(PendingProverWitness {
                from_zone_block,
                to_zone_block,
            })),
        )
    }

    /// Read the zone state at block `to` and derive the compact ZoneHeader hash
    /// for the inclusive range `from..=to`, seeded by the portal-confirmed
    /// previous ZoneHeader hash.
    async fn fetch_block_snapshot(&self, from: u64, to: u64) -> Result<ZoneBlockSnapshot> {
        let tempo_call = self.tempo_state.tempoBlockNumber().block(to.into());
        let deposit_call = self.inbox.processedDepositQueueHash().block(to.into());
        let deposit_number_call = self.inbox.processedDepositNumber().block(to.into());
        let block_hash_fut =
            derive_zone_block_hash_for_range(&self.provider, from, to, self.prev_zone_block_hash);
        let (tempo_block_number, processed_deposit_hash, processed_deposit_number, block_hash) = tokio::join!(
            tempo_call.call(),
            deposit_call.call(),
            deposit_number_call.call(),
            block_hash_fut,
        );
        let tempo_block_number = tempo_block_number?;
        let processed_deposit_hash = processed_deposit_hash?;
        let processed_deposit_number = processed_deposit_number?;
        let block_hash = block_hash?;

        Ok(ZoneBlockSnapshot {
            tempo_block_number,
            processed_deposit_hash,
            processed_deposit_number,
            block_hash,
        })
    }

    async fn verify_funds_ledger(
        &self,
        next_processed_deposit_number: u64,
        current_batch_withdrawals: &[abi::Withdrawal],
    ) -> Result<()> {
        let ledger = self
            .build_funds_ledger(next_processed_deposit_number, current_batch_withdrawals)
            .await?;
        let portal = ZonePortal::new(
            self.config.portal_address,
            self.batch_submitter.l1_provider().clone(),
        );

        for (token, funds) in ledger.iter() {
            let liability = FundsLedger::liability(funds)?;
            if liability.is_zero() {
                continue;
            }

            let accounted_balance = portal.accountedBalance(*token).call().await?;
            if accounted_balance < liability {
                return Err(eyre::eyre!(
                    "portal accounted funds invariant failed for token {token}: \
                     accounted_balance={accounted_balance}, required_liability={liability}, \
                     unprocessed_deposits={}, pending_refunds={}, pending_withdrawals={}",
                    funds.unprocessed_deposits,
                    funds.pending_refunds,
                    funds.pending_withdrawals
                ));
            }

            info!(
                token = %token,
                accounted_balance = %accounted_balance,
                required_liability = %liability,
                unprocessed_deposits = %funds.unprocessed_deposits,
                pending_refunds = %funds.pending_refunds,
                pending_withdrawals = %funds.pending_withdrawals,
                "Portal accounted funds invariant passed"
            );
        }

        Ok(())
    }

    async fn build_funds_ledger(
        &self,
        next_processed_deposit_number: u64,
        current_batch_withdrawals: &[abi::Withdrawal],
    ) -> Result<FundsLedger> {
        let mut ledger = FundsLedger::default();

        let pending = self
            .batch_submitter
            .fetch_pending_withdrawals(&self.provider, self.config.outbox_address)
            .await?;
        for withdrawals in pending.values() {
            ledger.add_withdrawals(withdrawals)?;
        }
        ledger.add_withdrawals(current_batch_withdrawals)?;

        let portal = ZonePortal::new(
            self.config.portal_address,
            self.batch_submitter.l1_provider().clone(),
        );
        let genesis_tempo_block_number = self
            .batch_submitter
            .read_genesis_tempo_block_number()
            .await?;
        let l1_tip = self
            .batch_submitter
            .l1_provider()
            .get_block_number()
            .await?;

        let regular_deposits = portal
            .DepositMade_filter()
            .from_block(genesis_tempo_block_number)
            .to_block(l1_tip)
            .chunked()
            .chunk_size(LOG_QUERY_BLOCK_CHUNK)
            .concurrent(2)
            .query()
            .await?;
        for (event, _) in regular_deposits {
            if event.depositNumber > next_processed_deposit_number {
                ledger.add_unprocessed_deposit(event.token, event.netAmount)?;
            }
        }

        let encrypted_deposits = portal
            .EncryptedDepositMade_filter()
            .from_block(genesis_tempo_block_number)
            .to_block(l1_tip)
            .chunked()
            .chunk_size(LOG_QUERY_BLOCK_CHUNK)
            .concurrent(2)
            .query()
            .await?;
        for (event, _) in encrypted_deposits {
            if event.depositNumber > next_processed_deposit_number {
                ledger.add_unprocessed_deposit(event.token, event.netAmount)?;
            }
        }

        let withdrawal_bouncebacks = portal
            .WithdrawalBounceBack_filter()
            .from_block(genesis_tempo_block_number)
            .to_block(l1_tip)
            .chunked()
            .chunk_size(LOG_QUERY_BLOCK_CHUNK)
            .concurrent(2)
            .query()
            .await?;
        for (event, _) in withdrawal_bouncebacks {
            if event.depositNumber > next_processed_deposit_number {
                ledger.add_unprocessed_deposit(event.token, event.amount)?;
            }
        }

        let pending_refunds = portal
            .DepositBounceBackPending_filter()
            .from_block(genesis_tempo_block_number)
            .to_block(l1_tip)
            .chunked()
            .chunk_size(LOG_QUERY_BLOCK_CHUNK)
            .concurrent(2)
            .query()
            .await?;
        for (event, _) in pending_refunds {
            ledger.add_pending_refund(event.token, event.amount)?;
        }

        let claimed_refunds = portal
            .RefundClaimed_filter()
            .from_block(genesis_tempo_block_number)
            .to_block(l1_tip)
            .chunked()
            .chunk_size(LOG_QUERY_BLOCK_CHUNK)
            .concurrent(2)
            .query()
            .await?;
        for (event, _) in claimed_refunds {
            ledger.claim_refund(event.token, event.amount)?;
        }

        Ok(ledger)
    }

    /// Submit a `submitBatch` transaction to the ZonePortal on L1 with exponential
    /// backoff retry.
    ///
    /// On success:
    /// - Advances `prev_zone_block_hash`, `prev_processed_deposit_hash`, and
    ///   `last_submitted_zone_block` to reflect the submitted range.
    /// - Increments `portal_withdrawal_queue_tail` if the batch included withdrawals.
    /// - Notifies the [`WithdrawalProcessor`](crate::withdrawals::WithdrawalProcessor)
    ///   so it can finalize newly enqueued withdrawal slots.
    ///
    /// On failure (after [`MAX_RETRIES`] attempts with [`INITIAL_RETRY_DELAY`]
    /// doubling each time): resyncs the local submission anchor from the
    /// portal-confirmed zone block so the next poll starts from accepted
    /// on-chain state.
    async fn submit_batch_with_retry(
        &mut self,
        batch_data: &BatchData,
        last_zone_block: u64,
        withdrawals: Vec<abi::Withdrawal>,
    ) -> Result<()> {
        // Preflight: verify prev_zone_block_hash matches portal state.
        match self.batch_submitter.read_portal_block_hash().await {
            Ok(portal_hash) if portal_hash != batch_data.prev_block_hash => {
                warn!(
                    local_prev = %batch_data.prev_block_hash,
                    portal_hash = %portal_hash,
                    "prev_block_hash mismatch with portal, resyncing"
                );
                self.resync_from_portal().await;
                return Ok(());
            }
            Err(e) => {
                warn!(error = %e, "Failed preflight portal hash check, continuing with submission");
            }
            _ => {}
        }

        if let Err(err) = self
            .verify_funds_ledger(batch_data.next_deposit_number, &withdrawals)
            .await
        {
            self.metrics.funds_ledger_check_failure_total.increment(1);
            return Err(err.wrap_err("portal accounted funds invariant check failed"));
        }

        let mut delay = INITIAL_RETRY_DELAY;

        for attempt in 1..=MAX_RETRIES {
            let submit_started = std::time::Instant::now();
            match self.batch_submitter.submit_batch(batch_data).await {
                Ok(tx_hash) => {
                    self.metrics
                        .batch_submit_latency_seconds
                        .record(submit_started.elapsed().as_secs_f64());
                    let blocks_in_batch = last_zone_block
                        .checked_sub(self.last_submitted_zone_block)
                        .ok_or_else(|| eyre::eyre!("submitted zone block moved backwards"))?;
                    info!(
                        last_zone_block,
                        blocks_in_batch,
                        tempo_block_number = batch_data.tempo_block_number,
                        %tx_hash,
                        withdrawal_queue_hash = %batch_data.withdrawal_queue_hash,
                        "Batch successfully submitted to L1"
                    );
                    self.metrics.batch_submit_success_total.increment(1);
                    self.metrics
                        .batch_size_blocks
                        .record(metric_u64(blocks_in_batch));
                    self.metrics
                        .withdrawals_per_batch
                        .record(metric_usize(withdrawals.len()));

                    // Only advance local state on success.
                    self.prev_zone_block_hash = batch_data.next_block_hash;
                    self.prev_processed_deposit_hash = batch_data.next_processed_deposit_hash;
                    self.prev_processed_deposit_number = batch_data.next_deposit_number;
                    self.last_submitted_zone_block = last_zone_block;
                    self.metrics
                        .latest_zone_block_submitted_to_l1
                        .set(metric_u64(last_zone_block));
                    self.update_submission_lag();

                    // Store withdrawals and advance portal queue tail if this batch had withdrawals.
                    if !batch_data.withdrawal_queue_hash.is_zero() {
                        if !withdrawals.is_empty() {
                            let portal_slot = self.portal_withdrawal_queue_tail;
                            let count = withdrawals.len();
                            let mut store = self.withdrawal_store.lock();
                            store.add_batch(portal_slot, withdrawals);
                            info!(
                                portal_slot,
                                count, "Stored withdrawals for portal queue slot"
                            );
                        }
                        self.portal_withdrawal_queue_tail = self
                            .portal_withdrawal_queue_tail
                            .checked_add(1)
                            .ok_or_else(|| {
                                eyre::eyre!("portal withdrawal queue tail overflowed")
                            })?;
                    }

                    // Signal the withdrawal processor.
                    self.withdrawal_notify.notify_one();

                    return Ok(());
                }
                Err(e) => {
                    self.metrics
                        .batch_submit_latency_seconds
                        .record(submit_started.elapsed().as_secs_f64());
                    if attempt < MAX_RETRIES {
                        self.metrics.batch_submit_retry_total.increment(1);
                        warn!(
                            attempt,
                            max_retries = MAX_RETRIES,
                            delay_secs = delay.as_secs(),
                            error = %e,
                            "Batch submission failed, retrying"
                        );
                        tokio::time::sleep(delay).await;
                        delay = delay.saturating_mul(2);
                    } else {
                        self.metrics.batch_submit_failure_total.increment(1);
                        let revert_reason = decode_portal_revert(&e);
                        error!(
                            error = ?e,
                            revert_reason,
                            last_zone_block,
                            tempo_block_number = batch_data.tempo_block_number,
                            prev_block_hash = %batch_data.prev_block_hash,
                            next_block_hash = %batch_data.next_block_hash,
                            "Batch submission failed after {MAX_RETRIES} retries"
                        );
                    }
                }
            }
        }

        // All retries exhausted — resync from portal.
        self.resync_from_portal().await;

        Err(eyre::eyre!(
            "batch submission failed after {MAX_RETRIES} retries for zone block {last_zone_block}"
        ))
    }

    /// Resync the local submission anchor from portal-confirmed on-chain state.
    ///
    /// Called after exhausting retries or when a preflight hash mismatch is
    /// detected, so subsequent batches start from the portal's actual accepted
    /// zone block rather than stale local values.
    async fn resync_from_portal(&mut self) {
        self.metrics.resync_from_portal_total.increment(1);
        let old_hash = self.prev_zone_block_hash;
        let old_tail = self.portal_withdrawal_queue_tail;
        let old_last_submitted = self.last_submitted_zone_block;
        let (
            store_batches_before_resync,
            store_first_slot_before_resync,
            store_last_slot_before_resync,
        ) = {
            let store = self.withdrawal_store.lock();
            store.summary()
        };
        match tokio::try_join!(
            self.batch_submitter.read_portal_block_hash(),
            self.batch_submitter.read_portal_withdrawal_queue_tail(),
        ) {
            Ok((portal_hash, portal_tail)) => {
                let last_submitted_zone_block =
                    Self::resolve_zone_block_number(&self.provider, portal_hash).await;
                let deposit_hash = Self::read_processed_deposit_hash_at_block(
                    &self.inbox,
                    last_submitted_zone_block,
                    self.prev_processed_deposit_hash,
                )
                .await;
                let deposit_number = Self::read_processed_deposit_number_at_block(
                    &self.inbox,
                    last_submitted_zone_block,
                )
                .await;

                warn!(
                    old_prev_block_hash = %old_hash,
                    new_block_hash = %portal_hash,
                    old_last_submitted_zone_block = old_last_submitted,
                    new_last_submitted_zone_block = last_submitted_zone_block,
                    old_portal_tail = old_tail,
                    new_portal_tail = portal_tail,
                    store_batches_before_resync,
                    store_first_slot_before_resync,
                    store_last_slot_before_resync,
                    %deposit_hash,
                    deposit_number,
                    "Resynced from portal and zone state"
                );
                self.prev_zone_block_hash = portal_hash;
                self.portal_withdrawal_queue_tail = portal_tail;
                self.last_submitted_zone_block = last_submitted_zone_block;
                self.prev_processed_deposit_hash = deposit_hash;
                self.prev_processed_deposit_number = deposit_number;
                self.metrics
                    .latest_zone_block_submitted_to_l1
                    .set(metric_u64(last_submitted_zone_block));
                self.update_submission_lag();
                if let Err(e) = self.restore_pending_withdrawals_from_chain().await {
                    let (stale_store_batches, stale_store_first_slot, stale_store_last_slot) = {
                        let mut store = self.withdrawal_store.lock();
                        let summary = store.summary();
                        store.replace_batches(Default::default());
                        summary
                    };
                    error!(
                        error = %e,
                        stale_store_batches,
                        stale_store_first_slot,
                        stale_store_last_slot,
                        "Failed to restore pending withdrawals during portal resync; cleared local withdrawal store"
                    );
                }
            }
            Err(e) => {
                error!(
                    error = %e,
                    "Failed to read portal state during resync"
                );
            }
        }
    }

    async fn resolve_zone_block_number(
        provider: &DynProvider<TempoNetwork>,
        zone_block_hash: B256,
    ) -> u64 {
        match resolve_zone_block_number_by_hash(provider, zone_block_hash).await {
            Ok(Some(block_number)) => block_number,
            Ok(None) => {
                warn!(
                    %zone_block_hash,
                    "Portal blockHash not found on zone L2 — zone may have been reset. \
                     Starting from genesis."
                );
                0
            }
            Err(e) => {
                warn!(
                    %zone_block_hash,
                    error = %e,
                    "Failed to look up zone block by hash, starting from genesis"
                );
                0
            }
        }
    }

    async fn read_processed_deposit_hash_at_block(
        inbox: &ZoneInbox::ZoneInboxInstance<DynProvider<TempoNetwork>, TempoNetwork>,
        zone_block_number: u64,
        fallback: B256,
    ) -> B256 {
        if zone_block_number == 0 {
            return B256::ZERO;
        }

        match inbox
            .processedDepositQueueHash()
            .block(zone_block_number.into())
            .call()
            .await
        {
            Ok(hash) => hash,
            Err(e) => {
                warn!(
                    zone_block_number,
                    error = %e,
                    "Failed to read processedDepositQueueHash at portal-confirmed zone block"
                );
                fallback
            }
        }
    }

    async fn read_processed_deposit_number_at_block(
        inbox: &ZoneInbox::ZoneInboxInstance<DynProvider<TempoNetwork>, TempoNetwork>,
        zone_block_number: u64,
    ) -> u64 {
        if zone_block_number == 0 {
            return 0;
        }

        match inbox
            .processedDepositNumber()
            .block(zone_block_number.into())
            .call()
            .await
        {
            Ok(n) => n,
            Err(e) => {
                warn!(
                    zone_block_number,
                    error = %e,
                    "Failed to read processedDepositNumber at portal-confirmed zone block"
                );
                0
            }
        }
    }

    fn record_observed_zone_block(&mut self, latest_zone_block: u64) {
        self.latest_observed_zone_block = latest_zone_block;
        self.metrics
            .latest_zone_block_observed
            .set(metric_u64(latest_zone_block));
        self.update_submission_lag();
    }

    fn update_submission_lag(&self) {
        self.metrics
            .zone_to_l1_submission_lag_blocks
            .set(metric_u64(
                self.latest_observed_zone_block
                    .saturating_sub(self.last_submitted_zone_block),
            ));
    }
}

/// Spawn the zone monitor as a background task.
///
/// The monitor polls the Zone L2 for new blocks, aggregates them into batches,
/// and submits each batch to the ZonePortal on Tempo L1. Local state only
/// advances on successful submission.
///
/// The `l1_provider` must already include the sequencer wallet for signing L1 transactions.
pub fn spawn_zone_monitor(
    config: ZoneMonitorConfig,
    l1_provider: DynProvider<TempoNetwork>,
    withdrawal_store: SharedWithdrawalStore,
    withdrawal_notify: Arc<Notify>,
    repair_notify: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut monitor = loop {
            match ZoneMonitor::new(
                config.clone(),
                l1_provider.clone(),
                withdrawal_store.clone(),
                withdrawal_notify.clone(),
                repair_notify.clone(),
            )
            .await
            {
                Ok(monitor) => break monitor,
                Err(e) => {
                    error!(error = %e, "Zone monitor failed to start, retrying in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        };

        loop {
            if let Err(e) = monitor.run().await {
                error!(error = %e, "Zone monitor failed, restarting in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    })
}

/// Try to decode a ZonePortal revert reason from an eyre error chain.
///
/// Extracts hex-encoded revert data from the error's display string and decodes
/// it using alloy's `ContractError`, which handles standard `Revert(string)`,
/// `Panic(uint256)`, and ZonePortal custom errors (`NotSequencer`, etc.).
fn decode_portal_revert(err: &eyre::Report) -> Option<String> {
    let msg = format!("{err}");
    let start = msg.find("data: \"0x")?.checked_add("data: \"".len())?;
    let end = msg[start..].find('"')?.checked_add(start)?;
    let bytes = alloy_primitives::hex::decode(&msg[start..end]).ok()?;
    let error = ContractError::<ZonePortal::ZonePortalErrors>::abi_decode(&bytes).ok()?;
    Some(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_network::Network;
    use alloy_primitives::{Bytes, U64, U256};
    use alloy_rpc_types_eth::{Block, Header, Log};
    use alloy_transport::mock::Asserter;
    use tempo_alloy::rpc::TempoHeaderResponse;
    use tempo_primitives::TempoHeader;

    use crate::settlement::BatchProofMaterial;

    fn mock_provider(asserter: Asserter) -> DynProvider<TempoNetwork> {
        ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter)
            .erased()
    }

    fn unavailable_witness_source() -> Arc<dyn ProverWitnessSource> {
        Arc::new(crate::settlement::UnavailableProverWitnessSource::new(
            "test witness source unavailable",
        ))
    }

    fn abi_encode_b256(value: B256) -> Bytes {
        Bytes::copy_from_slice(value.as_slice())
    }

    fn abi_encode_u64(value: u64) -> Bytes {
        Bytes::copy_from_slice(&U256::from(value).to_be_bytes::<32>())
    }

    fn push_empty_log_queries(asserter: &Asserter, count: usize) {
        let logs: Vec<Log> = Vec::new();
        for _ in 0..count {
            asserter.push_success(&logs);
        }
    }

    fn mock_block(hash: B256, number: u64) -> <TempoNetwork as Network>::BlockResponse {
        let mut inner = TempoHeader::default();
        inner.inner.number = number;

        let header = TempoHeaderResponse {
            inner: Header {
                hash,
                inner,
                total_difficulty: None,
                size: None,
            },
            timestamp_millis: 0,
        };

        Block::empty(header)
    }

    #[test]
    fn funds_ledger_tracks_withdrawal_amount_and_fee() {
        let token = Address::repeat_byte(0x10);
        let mut ledger = FundsLedger::default();

        ledger
            .add_withdrawals(&[abi::Withdrawal {
                token,
                senderTag: B256::ZERO,
                to: Address::repeat_byte(0x20),
                amount: 100,
                fee: 7,
                memo: B256::ZERO,
                gasLimit: 0,
                fallbackRecipient: Address::repeat_byte(0x30),
                callbackData: Default::default(),
                encryptedSender: Default::default(),
            }])
            .unwrap();

        let funds = ledger.tokens.get(&token).unwrap();
        assert_eq!(funds.pending_withdrawals, U256::from(107));
        assert_eq!(FundsLedger::liability(funds).unwrap(), U256::from(107));
    }

    #[test]
    fn funds_ledger_sums_independent_liability_buckets() {
        let token = Address::repeat_byte(0x10);
        let mut ledger = FundsLedger::default();

        ledger.add_unprocessed_deposit(token, 100).unwrap();
        ledger.add_pending_refund(token, 25).unwrap();
        ledger
            .add_withdrawals(&[abi::Withdrawal {
                token,
                senderTag: B256::ZERO,
                to: Address::repeat_byte(0x20),
                amount: 50,
                fee: 5,
                memo: B256::ZERO,
                gasLimit: 0,
                fallbackRecipient: Address::repeat_byte(0x30),
                callbackData: Default::default(),
                encryptedSender: Default::default(),
            }])
            .unwrap();

        let funds = ledger.tokens.get(&token).unwrap();
        assert_eq!(FundsLedger::liability(funds).unwrap(), U256::from(180));
    }

    #[test]
    fn funds_ledger_rejects_refund_claims_without_pending_refunds() {
        let token = Address::repeat_byte(0x10);
        let mut ledger = FundsLedger::default();

        let err = ledger.claim_refund(token, 1).unwrap_err();

        assert!(
            err.to_string()
                .contains("portal refund claims exceed pending refunds")
        );
    }

    fn test_monitor(l1: Asserter, zone: Asserter) -> ZoneMonitor {
        let portal_address = Address::repeat_byte(0x11);
        let config = ZoneMonitorConfig {
            outbox_address: Address::repeat_byte(0x22),
            inbox_address: Address::repeat_byte(0x33),
            tempo_state_address: Address::repeat_byte(0x44),
            zone_rpc_url: "http://unused.test".to_string(),
            retry_connection_interval: Duration::from_millis(100),
            poll_interval: Duration::from_secs(1),
            batch_interval: Duration::from_secs(1),
            portal_address,
            batch_anchor_config: BatchAnchorConfig::default(),
            prover_witness_source: unavailable_witness_source(),
            native_proof_signer: PrivateKeySigner::random(),
        };
        let zone_provider = mock_provider(zone);
        let l1_provider = mock_provider(l1);

        ZoneMonitor {
            config,
            metrics: crate::metrics::ZoneMonitorMetrics::default(),
            provider: zone_provider.clone(),
            outbox: ZoneOutbox::new(Address::repeat_byte(0x22), zone_provider.clone()),
            inbox: ZoneInbox::new(Address::repeat_byte(0x33), zone_provider.clone()),
            tempo_state: TempoState::new(Address::repeat_byte(0x44), zone_provider),
            withdrawal_store: SharedWithdrawalStore::new(),
            batch_submitter: BatchSubmitter::new(
                portal_address,
                l1_provider,
                0,
                PrivateKeySigner::random(),
            ),
            withdrawal_notify: Arc::new(Notify::new()),
            repair_notify: Arc::new(Notify::new()),
            last_submitted_zone_block: 10,
            prev_processed_deposit_hash: B256::repeat_byte(0xaa),
            prev_processed_deposit_number: 0,
            prev_zone_block_hash: B256::repeat_byte(0xbb),
            portal_withdrawal_queue_tail: 3,
            latest_observed_zone_block: 50,
        }
    }

    fn unproven_batch_data() -> UnprovenBatchData {
        UnprovenBatchData {
            tempo_block_number: 123,
            prev_block_hash: B256::repeat_byte(0x99),
            next_block_hash: B256::repeat_byte(0x55),
            prev_processed_deposit_hash: B256::repeat_byte(0x77),
            next_processed_deposit_hash: B256::repeat_byte(0x66),
            prev_deposit_number: 0,
            next_deposit_number: 0,
            withdrawal_queue_hash: B256::ZERO,
        }
    }

    #[tokio::test]
    async fn attach_prover_witness_records_pending_range() {
        let monitor = test_monitor(Asserter::new(), Asserter::new());

        let batch = monitor
            .attach_prover_witness(unproven_batch_data(), 11, 12)
            .await
            .unwrap();

        assert!(matches!(
            batch.proof,
            BatchProofSource::ProverWitness(PendingProverWitness {
                from_zone_block: 11,
                to_zone_block: 12
            })
        ));
    }

    #[tokio::test]
    async fn new_returns_error_when_startup_l1_read_fails() {
        let l1 = Asserter::new();
        let zone = Asserter::new();
        let portal_address = Address::repeat_byte(0x11);
        let config = ZoneMonitorConfig {
            outbox_address: Address::repeat_byte(0x22),
            inbox_address: Address::repeat_byte(0x33),
            tempo_state_address: Address::repeat_byte(0x44),
            zone_rpc_url: "http://unused.test".to_string(),
            retry_connection_interval: Duration::from_millis(100),
            poll_interval: Duration::from_secs(1),
            batch_interval: Duration::from_secs(1),
            portal_address,
            batch_anchor_config: BatchAnchorConfig::default(),
            prover_witness_source: unavailable_witness_source(),
            native_proof_signer: PrivateKeySigner::random(),
        };

        l1.push_failure_msg("boom");

        let err = match ZoneMonitor::new_with_provider(
            config,
            mock_provider(zone.clone()),
            mock_provider(l1.clone()),
            SharedWithdrawalStore::new(),
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
        )
        .await
        {
            Ok(_) => panic!("zone monitor startup should fail"),
            Err(err) => err,
        };

        assert!(
            err.to_string()
                .contains("failed to read genesisTempoBlockNumber during zone monitor startup")
        );
        assert!(l1.read_q().is_empty());
        assert!(zone.read_q().is_empty());
    }

    #[tokio::test]
    async fn resync_uses_portal_confirmed_zone_block_for_processed_deposit_hash() {
        let l1 = Asserter::new();
        let zone = Asserter::new();

        let portal_hash = B256::from(U256::from(7).to_be_bytes::<32>());
        let confirmed_zone_block = 42;
        let confirmed_deposit_hash = B256::repeat_byte(0x33);

        l1.push_success(&abi_encode_b256(portal_hash));
        l1.push_success(&abi_encode_u64(7));
        l1.push_success(&abi_encode_u64(7));
        l1.push_success(&abi_encode_u64(7));

        zone.push_success(&Some(mock_block(portal_hash, confirmed_zone_block)));
        zone.push_success(&abi_encode_b256(confirmed_deposit_hash));

        let mut monitor = test_monitor(l1.clone(), zone.clone());

        monitor.resync_from_portal().await;

        assert_eq!(monitor.prev_zone_block_hash, portal_hash);
        assert_eq!(monitor.last_submitted_zone_block, confirmed_zone_block);
        assert_eq!(monitor.prev_processed_deposit_hash, confirmed_deposit_hash);
        assert_eq!(monitor.portal_withdrawal_queue_tail, 7);
        assert!(zone.read_q().is_empty());
    }

    #[tokio::test]
    async fn repair_missing_withdrawal_slot_resyncs_portal_and_rebuilds_withdrawal_store() {
        let l1 = Asserter::new();
        let zone = Asserter::new();

        let portal_hash = B256::from(U256::from(7).to_be_bytes::<32>());
        let confirmed_zone_block = 42;
        let confirmed_deposit_hash = B256::repeat_byte(0x33);

        l1.push_success(&abi_encode_b256(portal_hash));
        l1.push_success(&abi_encode_u64(7));
        l1.push_success(&abi_encode_u64(7));
        l1.push_success(&abi_encode_u64(7));

        zone.push_success(&Some(mock_block(portal_hash, confirmed_zone_block)));
        zone.push_success(&abi_encode_b256(confirmed_deposit_hash));

        let mut monitor = test_monitor(l1.clone(), zone.clone());
        monitor.withdrawal_store.lock().add_withdrawal(
            3,
            abi::Withdrawal {
                token: Address::repeat_byte(0x10),
                senderTag: B256::repeat_byte(0x11),
                to: Address::repeat_byte(0x12),
                amount: 100,
                fee: 0,
                memo: B256::ZERO,
                gasLimit: 0,
                fallbackRecipient: Address::repeat_byte(0x12),
                callbackData: Default::default(),
                encryptedSender: Default::default(),
            },
        );

        monitor.repair_missing_withdrawal_slot().await;

        let store = monitor.withdrawal_store.lock();
        assert_eq!(store.batch_count(), 0);
        assert_eq!(monitor.prev_zone_block_hash, portal_hash);
        assert_eq!(monitor.last_submitted_zone_block, confirmed_zone_block);
        assert_eq!(monitor.prev_processed_deposit_hash, confirmed_deposit_hash);
        assert_eq!(monitor.portal_withdrawal_queue_tail, 7);
        assert!(zone.read_q().is_empty());
    }

    #[tokio::test]
    async fn resync_clears_stale_withdrawal_store_when_restore_fails() {
        let l1 = Asserter::new();
        let zone = Asserter::new();

        let portal_hash = B256::from(U256::from(7).to_be_bytes::<32>());
        let confirmed_zone_block = 42;
        let confirmed_deposit_hash = B256::repeat_byte(0x33);

        l1.push_success(&abi_encode_b256(portal_hash));
        l1.push_success(&abi_encode_u64(7));
        l1.push_failure_msg("head read failed");
        l1.push_failure_msg("tail read failed");

        zone.push_success(&Some(mock_block(portal_hash, confirmed_zone_block)));
        zone.push_success(&abi_encode_b256(confirmed_deposit_hash));

        let mut monitor = test_monitor(l1.clone(), zone.clone());
        monitor.withdrawal_store.lock().add_withdrawal(
            3,
            abi::Withdrawal {
                token: Address::repeat_byte(0x10),
                senderTag: B256::repeat_byte(0x11),
                to: Address::repeat_byte(0x12),
                amount: 100,
                fee: 0,
                memo: B256::ZERO,
                gasLimit: 0,
                fallbackRecipient: Address::repeat_byte(0x12),
                callbackData: Default::default(),
                encryptedSender: Default::default(),
            },
        );

        monitor.resync_from_portal().await;

        let store = monitor.withdrawal_store.lock();
        assert_eq!(store.batch_count(), 0);
        assert_eq!(monitor.prev_zone_block_hash, portal_hash);
        assert_eq!(monitor.last_submitted_zone_block, confirmed_zone_block);
        assert_eq!(monitor.prev_processed_deposit_hash, confirmed_deposit_hash);
        assert_eq!(monitor.portal_withdrawal_queue_tail, 7);
        assert!(zone.read_q().is_empty());
    }

    #[tokio::test]
    async fn preflight_hash_mismatch_resyncs_to_portal_confirmed_anchor() {
        let l1 = Asserter::new();
        let zone = Asserter::new();

        let portal_hash = B256::from(U256::from(7).to_be_bytes::<32>());
        let confirmed_zone_block = 42;
        let confirmed_deposit_hash = B256::repeat_byte(0x33);

        l1.push_success(&abi_encode_b256(portal_hash));
        l1.push_success(&abi_encode_b256(portal_hash));
        l1.push_success(&abi_encode_u64(7));
        l1.push_success(&abi_encode_u64(7));
        l1.push_success(&abi_encode_u64(7));

        zone.push_success(&Some(mock_block(portal_hash, confirmed_zone_block)));
        zone.push_success(&abi_encode_b256(confirmed_deposit_hash));

        let mut monitor = test_monitor(l1.clone(), zone.clone());
        let batch_data = BatchData {
            tempo_block_number: 123,
            prev_block_hash: B256::repeat_byte(0x99),
            next_block_hash: B256::repeat_byte(0x55),
            prev_processed_deposit_hash: B256::repeat_byte(0x77),
            next_processed_deposit_hash: B256::repeat_byte(0x66),
            prev_deposit_number: 0,
            next_deposit_number: 0,
            withdrawal_queue_hash: B256::ZERO,
            proof: BatchProofSource::Prebuilt(
                BatchProofMaterial::new(
                    Bytes::from_static(b"test-config"),
                    Bytes::from_static(b"test-proof"),
                )
                .unwrap(),
            ),
        };

        monitor
            .submit_batch_with_retry(&batch_data, 20, Vec::new())
            .await
            .unwrap();

        assert_eq!(monitor.prev_zone_block_hash, portal_hash);
        assert_eq!(monitor.last_submitted_zone_block, confirmed_zone_block);
        assert_eq!(monitor.prev_processed_deposit_hash, confirmed_deposit_hash);
        assert_eq!(monitor.portal_withdrawal_queue_tail, 7);
        assert_ne!(monitor.prev_zone_block_hash, batch_data.next_block_hash);
        assert_ne!(
            monitor.prev_processed_deposit_hash,
            batch_data.next_processed_deposit_hash
        );
        assert!(l1.read_q().is_empty());
        assert!(zone.read_q().is_empty());
    }

    #[tokio::test]
    async fn funds_ledger_failure_stops_before_batch_submission() {
        let l1 = Asserter::new();
        let zone = Asserter::new();
        let portal_hash = B256::repeat_byte(0xbb);
        let token = Address::repeat_byte(0x10);

        l1.push_success(&abi_encode_b256(portal_hash)); // preflight blockHash
        l1.push_success(&abi_encode_u64(0)); // withdrawalQueueHead
        l1.push_success(&abi_encode_u64(0)); // withdrawalQueueTail
        l1.push_success(&abi_encode_u64(0)); // genesisTempoBlockNumber
        l1.push_success(&U64::from(100)); // L1 tip for event scans
        push_empty_log_queries(&l1, 5);
        l1.push_success(&Bytes::from(U256::from(40).to_be_bytes::<32>())); // accountedBalance

        let mut monitor = test_monitor(l1.clone(), zone.clone());
        let batch_data = BatchData {
            tempo_block_number: 123,
            prev_block_hash: portal_hash,
            next_block_hash: B256::repeat_byte(0x55),
            prev_processed_deposit_hash: B256::repeat_byte(0x77),
            next_processed_deposit_hash: B256::repeat_byte(0x66),
            prev_deposit_number: 0,
            next_deposit_number: 0,
            withdrawal_queue_hash: B256::repeat_byte(0x44),
            proof: BatchProofSource::Prebuilt(
                BatchProofMaterial::new(
                    Bytes::from_static(b"test-config"),
                    Bytes::from_static(b"test-proof"),
                )
                .unwrap(),
            ),
        };
        let withdrawals = vec![abi::Withdrawal {
            token,
            senderTag: B256::ZERO,
            to: Address::repeat_byte(0x20),
            amount: 50,
            fee: 1,
            memo: B256::ZERO,
            gasLimit: 0,
            fallbackRecipient: Address::repeat_byte(0x30),
            callbackData: Default::default(),
            encryptedSender: Default::default(),
        }];

        let err = monitor
            .submit_batch_with_retry(&batch_data, 20, withdrawals)
            .await
            .unwrap_err();

        assert!(
            format!("{err:?}").contains("portal accounted funds invariant check failed"),
            "{err:?}"
        );
        assert_eq!(monitor.last_submitted_zone_block, 10);
        assert_eq!(monitor.prev_zone_block_hash, portal_hash);
        assert!(l1.read_q().is_empty());
        assert!(zone.read_q().is_empty());
    }
}
