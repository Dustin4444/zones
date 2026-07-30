//! L1 batch submitter for the zone sequencer.
//!
//! This module handles **Tempo L1** interactions — all transactions go to the
//! [`ZonePortal`](crate::abi::ZonePortal) contract deployed on L1. The sequencer
//! signing key is used for every L1 transaction.
//!
//! [`BatchData`] is produced by the zone monitor and passed to the submitter.
//!
//! # POC limitations
//!
//! Proof validation is currently **skipped** by the stub verifier. Both direct
//! and ancestry submissions use empty proof bytes until real proof generation is
//! implemented.
//!
//! # Anchor modes
//!
//! | Gap | Mode | Description |
//! |-----|------|-------------|
//! | < configured effective window | Direct | Portal reads hash from EIP-2935. |
//! | ≥ configured effective window | Ancestry | Use a recent anchor and collect ancestry headers for the batch. |
//!
//! [`AnchorMode`] handles submissions whose `tempoBlockNumber` is outside the
//! configured direct window by falling back to ancestry mode — a recent anchor
//! block plus a locally validated parent-hash header chain.

use std::{collections::BTreeMap, fmt, path::PathBuf, sync::OnceLock};

use crate::{
    ZoneSequencerProvider,
    abi::{self, BlockTransition, DepositQueueTransition, IZoneInbox, IZoneOutbox, ZonePortal},
    attestation::{AttestationStore, SettlementAttestation, SettlementCertificate},
    pending_submission::PendingCombinedSubmissionStore,
};
use alloy_consensus::{
    Transaction, TxReceipt as _,
    transaction::{SignerRecoverable as _, TxHashRef as _},
};
use alloy_eips::BlockHashOrNumber;
use alloy_network::{EthereumWallet, NetworkWallet, ReceiptResponse};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_provider::{DynProvider, PendingTransactionBuilder, Provider};
use alloy_rlp::Encodable;
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, SolEvent, SolStruct, SolValue, eip712_domain};
use eyre::{OptionExt as _, Result};
use futures::{StreamExt, TryStreamExt};
use parking_lot::RwLock;
use schnellru::{ByLength, LruMap};
use tempo_alloy::{
    TempoNetwork,
    provider::ext::TempoProviderExt,
    rpc::{TempoCallBuilderExt, TempoTransactionReceipt, TempoTransactionRequest},
};
use tempo_primitives::{Block, TempoReceipt, TempoTxEnvelope, transaction::Call};
use tracing::{info, instrument, warn};

use crate::{
    nonce_keys::SUBMIT_BATCH_NONCE_KEY,
    withdrawals::{CombinedWithdrawalPlan, TEMPO_TRANSACTION_GAS_LIMIT, plan_combined_withdrawals},
};

/// EIP-2935 stores the last 8192 block hashes, so the usable window is 8191 blocks.
const DEFAULT_EIP2935_HISTORY_WINDOW: u64 = 8192 - 1;

/// Safety margin (~3 min at 500ms block time) to avoid race conditions where
/// the block falls out of the window between our check and on-chain execution.
const DEFAULT_EIP2935_SAFETY_MARGIN: u64 = 360;

/// Maximum number of encoded L1 headers retained between ancestry submissions.
///
/// At roughly 600 bytes per header, this caps payload storage near 150 MiB plus
/// map overhead while covering more than the current Zone E recovery gap.
const DEFAULT_ANCESTRY_HEADER_CACHE_CAPACITY: u32 = 262_144;

/// Maximum wait for one combined-transaction receipt before the retry loop resumes the exact hash.
const COMBINED_RECEIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Maximum number of pending withdrawal queue slots in the portal ring buffer.
pub(crate) const WITHDRAWAL_QUEUE_CAPACITY: u64 = 100;

/// Maximum block span for one bounded log query.
///
/// Native Zone reads no longer use this limit; it remains the bound for L1 portal log recovery.
pub(crate) const LOG_QUERY_BLOCK_CHUNK: u64 = 5_000;

/// A newly enqueued withdrawal slot is guaranteed to become the head only when the queue is empty.
const fn queue_allows_combined_submission(head: u64, tail: u64) -> bool {
    head == tail
}

/// EIP-2935 anchor limits used by the batch submitter.
///
/// Production uses the real 8191-block EIP-2935 history window with a safety
/// margin. This type exists primarily so tests can shrink that otherwise large
/// window and exercise ancestry behavior without mining thousands of L1 blocks.
/// Production code should normally use [`Default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchAnchorConfig {
    /// Total L1 block-hash history window to treat as available for EIP-2935
    /// anchoring.
    history_window: u64,
    /// Number of most-recent L1 blocks to avoid when choosing an anchor, reducing
    /// the chance that an anchor ages out before the on-chain transaction lands.
    safety_margin: u64,
}

impl BatchAnchorConfig {
    /// Build an anchor config with explicit limits.
    pub fn new(history_window: u64, safety_margin: u64) -> Result<Self> {
        if history_window == 0 {
            return Err(eyre::eyre!("EIP-2935 history window must be non-zero"));
        }

        if safety_margin >= history_window {
            return Err(eyre::eyre!(
                "EIP-2935 safety margin ({safety_margin}) must be smaller than history window ({history_window})"
            ));
        }

        Ok(Self {
            history_window,
            safety_margin,
        })
    }

    /// Configured history window in L1 blocks.
    pub const fn history_window(self) -> u64 {
        self.history_window
    }

    /// Configured safety margin in L1 blocks.
    pub const fn safety_margin(self) -> u64 {
        self.safety_margin
    }

    /// Effective direct-submission window after subtracting the safety margin.
    pub const fn effective_window(self) -> u64 {
        self.history_window - self.safety_margin
    }
}

impl Default for BatchAnchorConfig {
    fn default() -> Self {
        Self {
            history_window: DEFAULT_EIP2935_HISTORY_WINDOW,
            safety_margin: DEFAULT_EIP2935_SAFETY_MARGIN,
        }
    }
}

/// L1 transaction shape used for a successful batch submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchSubmissionMode {
    /// `submitBatch` was the transaction's only portal call. Any enqueued withdrawals remain for
    /// the standalone withdrawal processor.
    SubmitOnly,
    /// `submitBatch` and whole-slot `processWithdrawals` ran as ordered calls in one Tempo AA
    /// transaction.
    SubmitAndProcessWithdrawals,
}

impl fmt::Display for BatchSubmissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SubmitOnly => f.write_str("submit-only"),
            Self::SubmitAndProcessWithdrawals => f.write_str("submit-and-process-withdrawals"),
        }
    }
}

/// Confirmed L1 batch submission and its same-transaction withdrawal accounting.
#[derive(Debug, Clone)]
pub struct BatchSubmissionReceipt {
    /// `BatchSubmitted` decoded from the confirmed receipt.
    pub event: ZonePortal::BatchSubmitted,
    /// Whether the receipt came from one or two ordered portal calls.
    pub mode: BatchSubmissionMode,
    /// Confirmed L1 transaction hash.
    pub transaction_hash: B256,
    /// Number of `WithdrawalProcessed` events emitted by the combined second call.
    pub withdrawals_processed: usize,
}

/// A failed batch-submission attempt, including whether an atomic two-call transaction was sent.
///
/// The monitor uses this bit to retry submit-only after an ambiguous or reverted combined send.
/// Preparation failures and submit-only failures leave the fast path eligible for the next retry.
#[derive(Debug)]
pub struct BatchSubmissionError {
    source: eyre::Report,
    combined_transaction_hash: Option<B256>,
    combined_fallback_safe: bool,
}

impl BatchSubmissionError {
    /// Hash of the deterministic atomic request when it may already be known to L1.
    pub const fn combined_transaction_hash(&self) -> Option<B256> {
        self.combined_transaction_hash
    }

    /// Whether the combined transaction is confirmed reverted, so a different transaction may
    /// safely use the next committed settlement-lane nonce.
    pub const fn combined_fallback_safe(&self) -> bool {
        self.combined_fallback_safe
    }

    /// Underlying report retained for provider-specific revert decoding.
    pub const fn report(&self) -> &eyre::Report {
        &self.source
    }
}

impl fmt::Display for BatchSubmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}

impl std::error::Error for BatchSubmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug, Default)]
struct CombinedAttemptState {
    transaction_hash: Option<B256>,
    fallback_safe: bool,
}

struct BatchSubmissionOptions<'a> {
    withdrawals: &'a [abi::Withdrawal],
    max_withdrawal_batch_gas: u64,
    allow_combined: bool,
    validate_withdrawals: bool,
}

#[derive(Debug)]
struct PreparedCombinedSubmission {
    request: TempoTransactionRequest,
    envelope: TempoTxEnvelope,
    transaction_hash: B256,
}

#[derive(Debug)]
struct DecodedCombinedSubmission {
    envelope: TempoTxEnvelope,
    transaction_hash: B256,
    signer: Address,
    nonce: u64,
    submit: ZonePortal::submitBatchCall,
    process: ZonePortal::processWithdrawalsCall,
}

#[derive(Debug)]
enum CombinedTransactionOutcome {
    Receipt(Box<TempoTransactionReceipt>),
    NonceConsumedWithoutReceipt,
}

#[derive(Debug)]
enum WithdrawalReceiptEvent {
    Processed(ZonePortal::WithdrawalProcessed),
    DepositBounceBack(ZonePortal::DepositBounceBack),
    DepositBounceBackPending(ZonePortal::DepositBounceBackPending),
}

/// Submits zone batches to the ZonePortal contract on Tempo L1.
///
/// Holds a contract instance pointing at the portal, backed by a shared
/// [`DynProvider`] with the sequencer's signing wallet.
pub struct BatchSubmitter {
    /// ZonePortal contract address on Tempo L1 (used in tracing spans).
    portal_address: Address,
    /// Shared L1 provider (HTTP or WS) for querying the current block number
    /// (EIP-2935 window check). The same provider backs the `portal` contract
    /// instance.
    l1_provider: DynProvider<TempoNetwork>,
    /// ZonePortal contract instance for calling `submitBatch` and reading
    /// on-chain state such as `blockHash()`.
    portal: ZonePortal::ZonePortalInstance<DynProvider<TempoNetwork>, TempoNetwork>,
    /// Immutable portal and chain identifiers, populated by the first metadata multicall.
    stable_portal_metadata: OnceLock<StablePortalMetadata>,
    /// Local sequencer key used to produce a 1-of-1 TIP-1091 settlement certificate.
    signer: Option<PrivateKeySigner>,
    /// Concurrency for pipelined L1 header fetching in ancestry mode.
    l1_fetch_concurrency: usize,
    /// EIP-2935 history and safety-margin limits used for anchor decisions.
    anchor_config: BatchAnchorConfig,
    /// Signatures from followers attesting to the batch.
    attestation_store: Option<AttestationStore>,
    /// Exact signed combined envelope, persisted before broadcast and retained until finality.
    combined_submission_store: Option<PendingCombinedSubmissionStore>,
    /// Validated, RLP-encoded L1 headers retained across overlapping ancestry
    /// requests. Settlement batches are submitted in order, so later requests
    /// can reuse almost the entire preceding range.
    ancestry_header_cache: RwLock<LruMap<u64, CachedAncestryHeader>>,
}
impl BatchSubmitter {
    /// Create a batch submitter without a certificate signer.
    ///
    /// This is useful for read-only operations and tests. Batch submission returns an error.
    pub fn new(portal_address: Address, l1_provider: DynProvider<TempoNetwork>) -> Self {
        Self::with_anchor_config(portal_address, l1_provider, BatchAnchorConfig::default())
    }

    /// Create a new batch submitter with custom EIP-2935 anchor limits.
    pub fn with_anchor_config(
        portal_address: Address,
        l1_provider: DynProvider<TempoNetwork>,
        anchor_config: BatchAnchorConfig,
    ) -> Self {
        Self::with_optional_signer_and_anchor_config(
            portal_address,
            l1_provider,
            None,
            anchor_config,
        )
    }

    /// Create a batch submitter that signs TIP-1091 settlement certificates locally.
    pub fn with_signer_and_anchor_config(
        portal_address: Address,
        l1_provider: DynProvider<TempoNetwork>,
        signer: PrivateKeySigner,
        anchor_config: BatchAnchorConfig,
    ) -> Self {
        Self::with_optional_signer_and_anchor_config(
            portal_address,
            l1_provider,
            Some(signer),
            anchor_config,
        )
    }

    pub(crate) fn with_optional_signer_and_anchor_config(
        portal_address: Address,
        l1_provider: DynProvider<TempoNetwork>,
        signer: Option<PrivateKeySigner>,
        anchor_config: BatchAnchorConfig,
    ) -> Self {
        let portal = ZonePortal::new(portal_address, l1_provider.clone());
        Self {
            portal_address,
            l1_provider,
            portal,
            stable_portal_metadata: OnceLock::new(),
            signer,
            l1_fetch_concurrency: 16,
            anchor_config,
            attestation_store: None,
            combined_submission_store: None,
            ancestry_header_cache: RwLock::new(LruMap::new(ByLength::new(
                DEFAULT_ANCESTRY_HEADER_CACHE_CAPACITY,
            ))),
        }
    }

    /// Attach the shared store populated by leader and follower settlement signatures.
    pub fn set_attestation_store(&mut self, store: Option<AttestationStore>) {
        self.attestation_store = store;
    }

    /// Configure the durable singleton used for exact-envelope combined settlement recovery.
    pub fn set_combined_submission_store_path(
        &mut self,
        path: PathBuf,
        durable_root: PathBuf,
    ) -> Result<()> {
        self.combined_submission_store =
            Some(PendingCombinedSubmissionStore::new(path, durable_root)?);
        Ok(())
    }

    /// Whether a signed combined envelope is durably pending.
    pub fn has_pending_combined_submission(&self) -> Result<bool> {
        match &self.combined_submission_store {
            Some(store) => store.exists(),
            None => Ok(false),
        }
    }

    /// Submit a batch to the ZonePortal on Tempo L1.
    ///
    /// Resolves the anchor mode based on how old `tempo_block_number` is:
    ///
    /// - **Direct** — `tempo_block_number` is within the configured effective window,
    ///   the portal reads its hash directly from EIP-2935.
    /// - **Ancestry** — `tempo_block_number` is outside the effective window. A
    ///   recent anchor block is used and ancestry headers are collected (for
    ///   future prover integration).
    ///
    /// `verifierConfig` and `proof` are empty until real proof generation is
    /// implemented.
    ///
    /// This compatibility entry point preserves submit-only behavior for callers that do not have
    /// the withdrawal payloads. Sequencer code should use
    /// [`Self::submit_batch_with_withdrawals`] so it can select the atomic fast path.
    pub async fn submit_batch(&self, batch: &BatchData) -> Result<ZonePortal::BatchSubmitted> {
        let mut combined_attempt = CombinedAttemptState::default();
        self.submit_batch_inner(
            batch,
            BatchSubmissionOptions {
                withdrawals: &[],
                max_withdrawal_batch_gas: 0,
                allow_combined: false,
                validate_withdrawals: false,
            },
            &mut combined_attempt,
        )
        .await
        .map(|receipt| receipt.event)
    }

    /// Submit a batch with the exact withdrawal payloads available for receipt reconciliation.
    ///
    /// When `allow_combined` is true, a non-empty withdrawal slot may be processed in the same
    /// atomic Tempo AA transaction. The fast path is used only when the portal queue is freshly
    /// observed empty, the payload hash matches the submitted commitment, and the complete slot
    /// fits one planner-bounded call.
    ///
    /// Returns the decoded `BatchSubmitted` event together with transaction-mode and withdrawal
    /// receipt accounting.
    // TODO: pass real proof bytes once proof generation is implemented.
    #[instrument(skip_all, fields(
        portal = %self.portal_address,
        tempo_block = batch.tempo_block_number,
        prev_block_hash = %batch.prev_block_hash,
        next_block_hash = %batch.next_block_hash,
        withdrawal_queue_hash = %batch.withdrawal_queue_hash,
        withdrawal_batch_index = batch.withdrawal_batch_index,
    ))]
    pub async fn submit_batch_with_withdrawals(
        &self,
        batch: &BatchData,
        withdrawals: &[abi::Withdrawal],
        max_withdrawal_batch_gas: u64,
        allow_combined: bool,
    ) -> std::result::Result<BatchSubmissionReceipt, BatchSubmissionError> {
        let mut combined_attempt = CombinedAttemptState::default();
        self.submit_batch_inner(
            batch,
            BatchSubmissionOptions {
                withdrawals,
                max_withdrawal_batch_gas,
                allow_combined,
                validate_withdrawals: true,
            },
            &mut combined_attempt,
        )
        .await
        .map_err(|source| BatchSubmissionError {
            source,
            combined_transaction_hash: combined_attempt.transaction_hash,
            combined_fallback_safe: combined_attempt.fallback_safe,
        })
    }

    async fn submit_batch_inner(
        &self,
        batch: &BatchData,
        options: BatchSubmissionOptions<'_>,
        combined_attempt: &mut CombinedAttemptState,
    ) -> Result<BatchSubmissionReceipt> {
        let BatchSubmissionOptions {
            withdrawals,
            max_withdrawal_batch_gas,
            allow_combined,
            validate_withdrawals,
        } = options;
        if let Some(receipt) = self
            .resume_persisted_combined_submission(batch, withdrawals, combined_attempt)
            .await?
        {
            return Ok(receipt);
        }

        let combined_plan = if validate_withdrawals {
            plan_combined_withdrawals(
                withdrawals,
                batch.withdrawal_queue_hash,
                max_withdrawal_batch_gas,
            )?
        } else {
            Err(crate::withdrawals::CombinedWithdrawalFallback::NoWithdrawals)
        };
        let combined_plan = match combined_plan {
            Ok(plan) if allow_combined && self.combined_submission_store.is_some() => Some(plan),
            Ok(_) if allow_combined => {
                info!(
                    withdrawal_count = withdrawals.len(),
                    "Durable combined-submission storage is unavailable; using submit-only path"
                );
                None
            }
            Ok(_) => {
                info!(
                    withdrawal_count = withdrawals.len(),
                    "Combined settlement fast path disabled for this retry"
                );
                None
            }
            Err(reason) => {
                info!(
                    withdrawal_count = withdrawals.len(),
                    fallback_reason = %reason,
                    "Using submit-only settlement path"
                );
                None
            }
        };

        let block_transition = BlockTransition {
            prevBlockHash: batch.prev_block_hash,
            nextBlockHash: batch.next_block_hash,
        };

        let deposit_transition = DepositQueueTransition {
            prevProcessedHash: batch.prev_processed_deposit_hash,
            nextProcessedHash: batch.next_processed_deposit_hash,
            prevDepositNumber: batch.prev_deposit_number,
            nextDepositNumber: batch.next_deposit_number,
        };

        let verifier_config = Bytes::new();
        let signer = self.signer.as_ref();
        let metadata = self
            .read_submission_metadata(signer.map_or(Address::ZERO, PrivateKeySigner::address))
            .await?;
        self.validate_submission_metadata(batch, metadata)?;
        let (certificate, anchor_mode, current_l1_block) =
            if let Some(store) = &self.attestation_store {
                let threshold = metadata.sequencer_threshold as usize;
                info!(
                    zone_height = batch.zone_height,
                    threshold, "Waiting for settlement quorum"
                );
                let certificate = store
                    .wait_for_settlement(batch.zone_height, threshold)
                    .await;
                let anchor_mode = match self
                    .validate_certificate(batch, batch.zone_height, metadata, &certificate)
                    .await
                {
                    Ok(anchor_mode) => anchor_mode,
                    Err(err) => {
                        store.remove_settlement(batch.zone_height, certificate.digest);
                        return Err(err);
                    }
                };
                let current_l1_block = self.l1_provider.get_block_number().await?;
                (Some(certificate), anchor_mode, current_l1_block)
            } else {
                let (anchor_mode, current_l1_block) =
                    self.resolve_anchor_mode(batch.tempo_block_number).await?;
                (None, anchor_mode, current_l1_block)
            };
        let recent_tempo_block_number = anchor_mode.recent_block_number();

        let signatures = if let Some(certificate) = &certificate {
            certificate.signatures.clone()
        } else {
            // Legacy mode, where the 1-of-1 sequencer will self-sign the attestation
            let anchor_block_number = anchor_mode.anchor_block_number(batch.tempo_block_number);
            let anchor_block_hash = self
                .l1_provider
                .get_block_by_number(anchor_block_number.into())
                .await?
                .ok_or_eyre(format!("L1 anchor block {anchor_block_number} not found"))?
                .header
                .hash;
            let signer = signer
                .ok_or_eyre("TIP-1091 batch submission requires the local sequencer signer")?;
            eyre::ensure!(
                metadata.signer_is_sequencer,
                "local sequencer signer {} is not active in the portal sequencer set",
                signer.address()
            );
            vec![self.sign_settlement_attestation(
                signer,
                metadata,
                SettlementAttestationInput {
                    batch,
                    anchor_block_number,
                    anchor_block_hash,
                    block_transition: &block_transition,
                    deposit_transition: &deposit_transition,
                    verifier_config: &verifier_config,
                },
            )?]
        };

        // Refetch the committed lane nonce for every submission attempt. The provider's
        // process-local nonce cache advances before a send is known to have succeeded, so
        // relying on it after a failed send can create an unfillable 2D-nonce gap.
        let submission_signer =
            signer.ok_or_eyre("batch submission requires the local sequencer signer")?;
        let submission_address = submission_signer.address();
        let nonce = self
            .l1_provider
            .get_transaction_count_with_nonce_key(submission_address, SUBMIT_BATCH_NONCE_KEY)
            .await?;

        let combined_submission = if let Some(plan) = combined_plan {
            let request = Self::build_combined_submission_request(
                self.portal_address,
                submission_address,
                metadata.stable.chain_id,
                nonce,
                plan,
                ZonePortal::submitBatchCall {
                    tempoBlockNumber: batch.tempo_block_number,
                    recentTempoBlockNumber: recent_tempo_block_number,
                    blockTransition: block_transition.clone(),
                    depositQueueTransition: deposit_transition.clone(),
                    withdrawalQueueHash: batch.withdrawal_queue_hash,
                    verifierConfig: verifier_config.clone(),
                    proof: Bytes::new(),
                    nextZoneHeight: U256::from(batch.zone_height),
                    signatures: signatures.clone(),
                }
                .abi_encode()
                .into(),
                withdrawals,
            );
            let prepared = Self::prepare_combined_submission(submission_signer, request).await?;

            // Queue bounds are refreshed immediately before first broadcast. A transaction can
            // still race this read, but the ordered second call then reverts the whole AA
            // transaction. Only a confirmed revert permits submit-only fallback.
            let (queue_head, queue_tail) = self.read_withdrawal_queue_bounds().await?;
            if !queue_allows_combined_submission(queue_head, queue_tail) {
                info!(
                    queue_head,
                    queue_tail,
                    pending_slots = queue_tail.saturating_sub(queue_head),
                    "Withdrawal backlog disables combined settlement fast path"
                );
                None
            } else {
                match self
                    .l1_provider
                    .estimate_gas(prepared.request.clone())
                    .await
                {
                    Ok(estimated_gas) if estimated_gas <= plan.gas_limit => {
                        let store = self.combined_submission_store.as_ref().ok_or_eyre(
                            "combined submission selected without durable envelope storage",
                        )?;
                        store.persist(&prepared.envelope)?;
                        combined_attempt.transaction_hash = Some(prepared.transaction_hash);
                        info!(
                            queue_head,
                            withdrawal_count = withdrawals.len(),
                            planned_gas = plan.gas_limit,
                            estimated_gas,
                            transaction_hash = %prepared.transaction_hash,
                            anchor_mode = %anchor_mode,
                            "Combined settlement transaction passed gas preflight and was persisted"
                        );
                        Some(prepared)
                    }
                    Ok(estimated_gas) => {
                        warn!(
                            withdrawal_count = withdrawals.len(),
                            planned_gas = plan.gas_limit,
                            estimated_gas,
                            "Combined settlement estimate exceeds its gas budget; falling back to submit-only"
                        );
                        None
                    }
                    Err(error) => {
                        warn!(
                            withdrawal_count = withdrawals.len(),
                            planned_gas = plan.gas_limit,
                            %error,
                            "Combined settlement preflight failed; falling back to submit-only"
                        );
                        None
                    }
                }
            }
        } else {
            None
        };
        let submission_mode = if combined_submission.is_some() {
            BatchSubmissionMode::SubmitAndProcessWithdrawals
        } else {
            BatchSubmissionMode::SubmitOnly
        };

        info!(
            anchor_mode = %anchor_mode,
            recent_tempo_block_number,
            current_l1_block,
            batch_prev_block_hash = %batch.prev_block_hash,
            nonce_key = ?SUBMIT_BATCH_NONCE_KEY,
            nonce,
            submission_mode = %submission_mode,
            withdrawals_in_same_transaction =
                matches!(submission_mode, BatchSubmissionMode::SubmitAndProcessWithdrawals),
            "Submitting batch to ZonePortal on L1"
        );

        let receipt = if let Some(prepared) = combined_submission {
            match self
                .send_or_resume_combined(
                    prepared.envelope,
                    prepared.transaction_hash,
                    submission_address,
                    nonce,
                    combined_attempt,
                )
                .await?
            {
                CombinedTransactionOutcome::Receipt(receipt) => *receipt,
                CombinedTransactionOutcome::NonceConsumedWithoutReceipt => {
                    self.clear_pending_combined_submission(prepared.transaction_hash)?;
                    return Err(eyre::eyre!(
                        "settlement nonce {nonce} was consumed without a receipt for persisted combined transaction {}",
                        prepared.transaction_hash
                    ));
                }
            }
        } else {
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                self.portal
                    .submitBatch(
                        batch.tempo_block_number,
                        recent_tempo_block_number,
                        block_transition,
                        deposit_transition,
                        batch.withdrawal_queue_hash,
                        verifier_config,
                        Bytes::new(),
                        U256::from(batch.zone_height),
                        signatures,
                    )
                    .nonce_key(SUBMIT_BATCH_NONCE_KEY)
                    .nonce(nonce)
                    .max_fee_per_gas(crate::TEMPO_L1_MAX_FEE_PER_GAS)
                    .max_priority_fee_per_gas(0)
                    .send_sync(),
            )
            .await
            .map_err(|_| eyre::eyre!("submitBatch sync submission timed out after 30 seconds"))??
        };

        let tx_hash = receipt.transaction_hash();
        if !receipt.status() {
            if matches!(
                submission_mode,
                BatchSubmissionMode::SubmitAndProcessWithdrawals
            ) {
                combined_attempt.transaction_hash = Some(tx_hash);
                self.clear_pending_combined_submission(tx_hash)?;
                combined_attempt.fallback_safe = true;
            }
            return Err(eyre::eyre!(
                "{submission_mode} transaction {tx_hash} was included but reverted on L1"
            ));
        }

        let event = self.decode_batch_submitted(receipt.logs())?;
        eyre::ensure!(
            event.withdrawalQueueHash == batch.withdrawal_queue_hash,
            "confirmed {submission_mode} receipt committed withdrawal hash {}, expected {}",
            event.withdrawalQueueHash,
            batch.withdrawal_queue_hash
        );
        eyre::ensure!(
            event.withdrawalBatchIndex == batch.withdrawal_batch_index,
            "confirmed {submission_mode} receipt used withdrawal batch index {}, expected {}",
            event.withdrawalBatchIndex,
            batch.withdrawal_batch_index
        );
        let withdrawals_processed =
            self.verify_withdrawal_receipt(receipt.logs(), withdrawals, submission_mode)?;

        if let (Some(store), Some(_)) = (&self.attestation_store, &certificate) {
            store.remove_submitted(batch.zone_height);
        }
        if matches!(
            submission_mode,
            BatchSubmissionMode::SubmitAndProcessWithdrawals
        ) {
            self.clear_pending_combined_submission(tx_hash)?;
        }

        info!(
            %tx_hash,
            withdrawal_batch_index = event.withdrawalBatchIndex,
            withdrawal_queue_index = %event.withdrawalQueueIndex,
            submission_mode = %submission_mode,
            withdrawals_processed,
            "Batch submitted to L1"
        );

        Ok(BatchSubmissionReceipt {
            event,
            mode: submission_mode,
            transaction_hash: tx_hash,
            withdrawals_processed,
        })
    }

    /// Build the ordered two-call Tempo request. `submitBatch` must remain call 0 so call 1 sees
    /// the newly enqueued slot; the settlement nonce lane orders this transaction with every other
    /// batch submission.
    fn build_combined_submission_request(
        portal_address: Address,
        submission_address: Address,
        chain_id: u64,
        nonce: u64,
        plan: CombinedWithdrawalPlan,
        submit_batch_input: Bytes,
        withdrawals: &[abi::Withdrawal],
    ) -> TempoTransactionRequest {
        let mut request = TempoTransactionRequest {
            calls: vec![
                Call {
                    to: portal_address.into(),
                    value: U256::ZERO,
                    input: submit_batch_input,
                },
                Call {
                    to: portal_address.into(),
                    value: U256::ZERO,
                    input: ZonePortal::processWithdrawalsCall {
                        withdrawals: withdrawals.to_vec(),
                        remainingQueue: plan.remaining_queue,
                    }
                    .abi_encode()
                    .into(),
                },
            ],
            nonce_key: Some(SUBMIT_BATCH_NONCE_KEY),
            ..Default::default()
        };
        request.inner.from = Some(submission_address);
        request.inner.chain_id = Some(chain_id);
        request.inner.nonce = Some(nonce);
        request.inner.gas = Some(plan.gas_limit);
        request.inner.max_fee_per_gas = Some(crate::TEMPO_L1_MAX_FEE_PER_GAS);
        request.inner.max_priority_fee_per_gas = Some(0);
        request
    }

    /// Sign the complete AA request before it is durably persisted and broadcast.
    async fn prepare_combined_submission(
        signer: &PrivateKeySigner,
        request: TempoTransactionRequest,
    ) -> Result<PreparedCombinedSubmission> {
        let wallet = EthereumWallet::from(signer.clone());
        let envelope: TempoTxEnvelope =
            <EthereumWallet as NetworkWallet<TempoNetwork>>::sign_request(&wallet, request.clone())
                .await?;
        let transaction_hash = *envelope.tx_hash();
        Ok(PreparedCombinedSubmission {
            request,
            envelope,
            transaction_hash,
        })
    }

    /// Resolve a durable combined envelope before any mutable planner, certificate, or queue read.
    async fn resume_persisted_combined_submission(
        &self,
        batch: &BatchData,
        withdrawals: &[abi::Withdrawal],
        combined_attempt: &mut CombinedAttemptState,
    ) -> Result<Option<BatchSubmissionReceipt>> {
        let Some(decoded) = self.load_pending_combined_submission().await? else {
            return Ok(None);
        };
        combined_attempt.transaction_hash = Some(decoded.transaction_hash);

        let outcome = self
            .send_or_resume_combined(
                decoded.envelope.clone(),
                decoded.transaction_hash,
                decoded.signer,
                decoded.nonce,
                combined_attempt,
            )
            .await?;
        let receipt = match outcome {
            CombinedTransactionOutcome::Receipt(receipt) => *receipt,
            CombinedTransactionOutcome::NonceConsumedWithoutReceipt => {
                self.clear_pending_combined_submission(decoded.transaction_hash)?;
                return Err(eyre::eyre!(
                    "settlement nonce {} was consumed without a receipt for persisted combined transaction {}",
                    decoded.nonce,
                    decoded.transaction_hash
                ));
            }
        };

        if !receipt.status() {
            self.clear_pending_combined_submission(decoded.transaction_hash)?;
            combined_attempt.fallback_safe =
                Self::ensure_persisted_submission_matches_batch(&decoded, batch, withdrawals)
                    .is_ok();
            return Err(eyre::eyre!(
                "persisted combined transaction {} was included but reverted on L1",
                decoded.transaction_hash
            ));
        }

        let submission = self.validate_persisted_combined_receipt(&receipt, &decoded)?;
        let zone_height: u64 = decoded
            .submit
            .nextZoneHeight
            .try_into()
            .map_err(|_| eyre::eyre!("persisted combined zone height does not fit u64"))?;
        if let Some(store) = &self.attestation_store {
            store.remove_submitted(zone_height);
        }
        self.clear_pending_combined_submission(decoded.transaction_hash)?;
        Self::ensure_persisted_submission_matches_batch(&decoded, batch, withdrawals)?;
        eyre::ensure!(
            submission.event.withdrawalBatchIndex == batch.withdrawal_batch_index,
            "persisted combined receipt withdrawal batch index {} does not match local batch {}",
            submission.event.withdrawalBatchIndex,
            batch.withdrawal_batch_index
        );
        info!(
            transaction_hash = %decoded.transaction_hash,
            nonce = decoded.nonce,
            "Resolved durable combined settlement transaction"
        );
        Ok(Some(submission))
    }

    /// Finish any durable combined envelope before the monitor derives its portal anchor.
    ///
    /// Startup blocks here until the exact transaction has a receipt (or its nonce is
    /// definitively consumed), preventing a restart from constructing different calldata.
    pub async fn recover_persisted_combined_submission(&self) -> Result<()> {
        let Some(decoded) = self.load_pending_combined_submission().await? else {
            return Ok(());
        };
        let mut combined_attempt = CombinedAttemptState {
            transaction_hash: Some(decoded.transaction_hash),
            fallback_safe: false,
        };
        let outcome = self
            .send_or_resume_combined(
                decoded.envelope.clone(),
                decoded.transaction_hash,
                decoded.signer,
                decoded.nonce,
                &mut combined_attempt,
            )
            .await?;

        let receipt = match outcome {
            CombinedTransactionOutcome::Receipt(receipt) => *receipt,
            CombinedTransactionOutcome::NonceConsumedWithoutReceipt => {
                self.clear_pending_combined_submission(decoded.transaction_hash)?;
                warn!(
                    transaction_hash = %decoded.transaction_hash,
                    nonce = decoded.nonce,
                    "Cleared durable combined transaction after its settlement nonce was consumed without an observable receipt"
                );
                return Ok(());
            }
        };

        if !receipt.status() {
            self.clear_pending_combined_submission(decoded.transaction_hash)?;
            warn!(
                transaction_hash = %decoded.transaction_hash,
                nonce = decoded.nonce,
                "Recovered durable combined transaction with a confirmed revert"
            );
            return Ok(());
        }

        let submission = self.validate_persisted_combined_receipt(&receipt, &decoded)?;
        if let Some(store) = &self.attestation_store {
            let zone_height: u64 = decoded
                .submit
                .nextZoneHeight
                .try_into()
                .map_err(|_| eyre::eyre!("persisted combined zone height does not fit u64"))?;
            store.remove_submitted(zone_height);
        }
        self.clear_pending_combined_submission(decoded.transaction_hash)?;
        info!(
            transaction_hash = %decoded.transaction_hash,
            nonce = decoded.nonce,
            withdrawal_batch_index = submission.event.withdrawalBatchIndex,
            withdrawals_processed = submission.withdrawals_processed,
            "Recovered and reconciled durable combined settlement transaction"
        );
        Ok(())
    }

    async fn load_pending_combined_submission(&self) -> Result<Option<DecodedCombinedSubmission>> {
        let Some(store) = &self.combined_submission_store else {
            return Ok(None);
        };
        let Some(envelope) = store.load()? else {
            return Ok(None);
        };
        let transaction_hash = *envelope.tx_hash();
        let TempoTxEnvelope::AA(signed) = &envelope else {
            return Err(eyre::eyre!(
                "pending combined transaction {transaction_hash} is not a Tempo AA envelope"
            ));
        };
        let signer = signed.recover_signer().map_err(|error| {
            eyre::eyre!("failed recovering persisted transaction signer: {error}")
        })?;
        let expected_signer = self
            .signer
            .as_ref()
            .ok_or_eyre("pending combined transaction recovery requires the local signer")?
            .address();
        eyre::ensure!(
            signer == expected_signer,
            "pending combined transaction signer {signer} does not match configured signer {expected_signer}"
        );

        let transaction = signed.tx();
        let chain_id = self.l1_provider.get_chain_id().await?;
        eyre::ensure!(
            transaction.chain_id == chain_id,
            "pending combined transaction chain {} does not match L1 chain {chain_id}",
            transaction.chain_id
        );
        eyre::ensure!(
            transaction.nonce_key == SUBMIT_BATCH_NONCE_KEY,
            "pending combined transaction uses nonce key {}, expected {}",
            transaction.nonce_key,
            SUBMIT_BATCH_NONCE_KEY
        );
        eyre::ensure!(
            transaction.gas_limit <= TEMPO_TRANSACTION_GAS_LIMIT,
            "pending combined transaction gas {} exceeds Tempo cap {TEMPO_TRANSACTION_GAS_LIMIT}",
            transaction.gas_limit
        );
        eyre::ensure!(
            transaction.calls.len() == 2,
            "pending combined transaction has {} calls, expected 2",
            transaction.calls.len()
        );
        for (index, call) in transaction.calls.iter().enumerate() {
            eyre::ensure!(
                call.to == self.portal_address.into(),
                "pending combined call {index} targets {:?}, expected {}",
                call.to,
                self.portal_address
            );
            eyre::ensure!(
                call.value.is_zero(),
                "pending combined call {index} transfers unexpected value {}",
                call.value
            );
        }
        let submit = ZonePortal::submitBatchCall::abi_decode(&transaction.calls[0].input)
            .map_err(|error| eyre::eyre!("failed decoding persisted submitBatch call: {error}"))?;
        let process = ZonePortal::processWithdrawalsCall::abi_decode(&transaction.calls[1].input)
            .map_err(|error| {
            eyre::eyre!("failed decoding persisted processWithdrawals call: {error}")
        })?;
        eyre::ensure!(
            process.remainingQueue.is_zero(),
            "persisted combined transaction must exhaust its withdrawal slot"
        );
        let reconstructed_hash = abi::Withdrawal::queue_hash(&process.withdrawals);
        eyre::ensure!(
            reconstructed_hash == submit.withdrawalQueueHash,
            "persisted withdrawal payload hash {reconstructed_hash} does not match submitted hash {}",
            submit.withdrawalQueueHash
        );
        let nonce = transaction.nonce;

        Ok(Some(DecodedCombinedSubmission {
            envelope,
            transaction_hash,
            signer,
            nonce,
            submit,
            process,
        }))
    }

    fn ensure_persisted_submission_matches_batch(
        decoded: &DecodedCombinedSubmission,
        batch: &BatchData,
        withdrawals: &[abi::Withdrawal],
    ) -> Result<()> {
        let submit = &decoded.submit;
        eyre::ensure!(
            submit.tempoBlockNumber == batch.tempo_block_number
                && submit.blockTransition.prevBlockHash == batch.prev_block_hash
                && submit.blockTransition.nextBlockHash == batch.next_block_hash
                && submit.depositQueueTransition.prevProcessedHash
                    == batch.prev_processed_deposit_hash
                && submit.depositQueueTransition.nextProcessedHash
                    == batch.next_processed_deposit_hash
                && submit.depositQueueTransition.prevDepositNumber == batch.prev_deposit_number
                && submit.depositQueueTransition.nextDepositNumber == batch.next_deposit_number
                && submit.withdrawalQueueHash == batch.withdrawal_queue_hash
                && submit.nextZoneHeight == U256::from(batch.zone_height),
            "persisted combined transaction {} does not match pending zone batch {}",
            decoded.transaction_hash,
            batch.zone_height
        );
        eyre::ensure!(
            decoded.process.withdrawals == withdrawals,
            "persisted combined transaction {} withdrawal payloads do not match pending zone batch {}",
            decoded.transaction_hash,
            batch.zone_height
        );
        Ok(())
    }

    fn validate_persisted_combined_receipt(
        &self,
        receipt: &TempoTransactionReceipt,
        decoded: &DecodedCombinedSubmission,
    ) -> Result<BatchSubmissionReceipt> {
        let transaction_hash = receipt.transaction_hash();
        eyre::ensure!(
            transaction_hash == decoded.transaction_hash,
            "combined receipt hash {transaction_hash} does not match persisted transaction {}",
            decoded.transaction_hash
        );
        let event = self.decode_batch_submitted(receipt.logs())?;
        eyre::ensure!(
            event.nextBlockHash == decoded.submit.blockTransition.nextBlockHash
                && event.nextProcessedDepositQueueHash
                    == decoded.submit.depositQueueTransition.nextProcessedHash
                && event.lastProcessedDepositNumber
                    == decoded.submit.depositQueueTransition.nextDepositNumber
                && event.withdrawalQueueHash == decoded.submit.withdrawalQueueHash,
            "persisted combined receipt does not match the signed submitBatch call"
        );
        eyre::ensure!(
            event.withdrawalQueueIndex != abi::NO_QUEUE_INDEX,
            "persisted combined receipt emitted NO_QUEUE_INDEX for non-empty withdrawals"
        );
        let withdrawals_processed = self.verify_withdrawal_receipt(
            receipt.logs(),
            &decoded.process.withdrawals,
            BatchSubmissionMode::SubmitAndProcessWithdrawals,
        )?;
        Ok(BatchSubmissionReceipt {
            event,
            mode: BatchSubmissionMode::SubmitAndProcessWithdrawals,
            transaction_hash,
            withdrawals_processed,
        })
    }

    fn clear_pending_combined_submission(&self, transaction_hash: B256) -> Result<()> {
        self.combined_submission_store
            .as_ref()
            .ok_or_eyre("combined transaction exists without durable envelope storage")?
            .clear(transaction_hash)
    }

    /// Broadcast an already-persisted combined transaction, or resume waiting for its exact hash.
    async fn send_or_resume_combined(
        &self,
        envelope: TempoTxEnvelope,
        transaction_hash: B256,
        signer: Address,
        nonce: u64,
        combined_attempt: &mut CombinedAttemptState,
    ) -> Result<CombinedTransactionOutcome> {
        combined_attempt.transaction_hash = Some(transaction_hash);
        self.combined_submission_store
            .as_ref()
            .ok_or_eyre("combined transaction exists without durable envelope storage")?
            .ensure_durable(transaction_hash)?;

        if let Some(receipt) = self
            .l1_provider
            .get_transaction_receipt(transaction_hash)
            .await?
        {
            return Ok(CombinedTransactionOutcome::Receipt(Box::new(receipt)));
        }

        let committed_nonce = self
            .l1_provider
            .get_transaction_count_with_nonce_key(signer, SUBMIT_BATCH_NONCE_KEY)
            .await?;
        if committed_nonce > nonce {
            return Ok(CombinedTransactionOutcome::NonceConsumedWithoutReceipt);
        }
        eyre::ensure!(
            committed_nonce == nonce,
            "persisted combined transaction {transaction_hash} uses settlement nonce {nonce}, but L1 committed nonce is {committed_nonce}"
        );

        let already_known = self
            .l1_provider
            .get_transaction_by_hash(transaction_hash)
            .await?
            .is_some();
        if !already_known {
            match self.l1_provider.send_tx_envelope(envelope).await {
                Ok(pending) => {
                    eyre::ensure!(
                        *pending.tx_hash() == transaction_hash,
                        "combined settlement broadcast returned hash {}, expected {}",
                        pending.tx_hash(),
                        transaction_hash
                    );
                    info!(
                        %transaction_hash,
                        "Broadcast combined settlement transaction"
                    );
                }
                Err(error) => {
                    // The transport may have lost the response after L1 accepted the transaction.
                    // Keep the known local hash and let the retry loop resume it.
                    if let Some(receipt) = self
                        .l1_provider
                        .get_transaction_receipt(transaction_hash)
                        .await?
                    {
                        return Ok(CombinedTransactionOutcome::Receipt(Box::new(receipt)));
                    }
                    if self
                        .l1_provider
                        .get_transaction_by_hash(transaction_hash)
                        .await?
                        .is_none()
                    {
                        return Err(eyre::eyre!(
                            "combined settlement broadcast for {transaction_hash} failed and the transaction is not yet observable: {error}"
                        ));
                    }
                }
            }
        }

        let receipt_result =
            PendingTransactionBuilder::new(self.l1_provider.root().clone(), transaction_hash)
                .with_timeout(Some(COMBINED_RECEIPT_TIMEOUT))
                .get_receipt()
                .await;
        match receipt_result {
            Ok(receipt) => Ok(CombinedTransactionOutcome::Receipt(Box::new(receipt))),
            Err(error) => {
                if let Some(receipt) = self
                    .l1_provider
                    .get_transaction_receipt(transaction_hash)
                    .await?
                {
                    return Ok(CombinedTransactionOutcome::Receipt(Box::new(receipt)));
                }
                let committed_nonce = self
                    .l1_provider
                    .get_transaction_count_with_nonce_key(signer, SUBMIT_BATCH_NONCE_KEY)
                    .await?;
                if committed_nonce > nonce {
                    return Ok(CombinedTransactionOutcome::NonceConsumedWithoutReceipt);
                }
                Err(eyre::eyre!(
                    "combined settlement transaction {transaction_hash} receipt unavailable after {} seconds: {error}",
                    COMBINED_RECEIPT_TIMEOUT.as_secs()
                ))
            }
        }
    }

    /// Refresh just the FIFO bounds immediately before a possible combined send.
    async fn read_withdrawal_queue_bounds(&self) -> Result<(u64, u64)> {
        let (head, tail): (U256, U256) = self
            .l1_provider
            .multicall()
            .add(self.portal.withdrawalQueueHead())
            .add(self.portal.withdrawalQueueTail())
            .aggregate()
            .await?;
        let head = head
            .try_into()
            .map_err(|_| eyre::eyre!("withdrawal queue head overflow"))?;
        let tail = tail
            .try_into()
            .map_err(|_| eyre::eyre!("withdrawal queue tail overflow"))?;
        eyre::ensure!(
            head <= tail,
            "inconsistent withdrawal queue bounds before submission: head {head}, tail {tail}"
        );
        Ok((head, tail))
    }

    /// Verify that one successful receipt contains an ordered terminal event for every supplied
    /// withdrawal. Deposit bouncebacks use dedicated events instead of `WithdrawalProcessed`.
    fn verify_withdrawal_receipt(
        &self,
        logs: &[alloy_rpc_types_eth::Log],
        withdrawals: &[abi::Withdrawal],
        mode: BatchSubmissionMode,
    ) -> Result<usize> {
        let events = logs
            .iter()
            .filter(|log| log.address() == self.portal_address)
            .filter_map(|log| {
                if let Ok(event) = ZonePortal::WithdrawalProcessed::decode_log(&log.inner) {
                    return Some(WithdrawalReceiptEvent::Processed(event.data));
                }
                if let Ok(event) = ZonePortal::DepositBounceBack::decode_log(&log.inner) {
                    return Some(WithdrawalReceiptEvent::DepositBounceBack(event.data));
                }
                if let Ok(event) = ZonePortal::DepositBounceBackPending::decode_log(&log.inner) {
                    return Some(WithdrawalReceiptEvent::DepositBounceBackPending(event.data));
                }
                None
            })
            .collect::<Vec<_>>();

        if matches!(mode, BatchSubmissionMode::SubmitOnly) {
            eyre::ensure!(
                events.is_empty(),
                "confirmed submit-only receipt unexpectedly emitted {} withdrawal terminal events",
                events.len()
            );
            return Ok(0);
        }

        eyre::ensure!(
            events.len() == withdrawals.len(),
            "confirmed combined receipt emitted {} withdrawal terminal events, expected {}",
            events.len(),
            withdrawals.len()
        );

        for (index, (withdrawal, event)) in withdrawals.iter().zip(events).enumerate() {
            match (withdrawal.fallbackNonce, event) {
                (0, WithdrawalReceiptEvent::DepositBounceBack(event)) => {
                    eyre::ensure!(
                        event.tempoRefundRecipient == withdrawal.to
                            && event.token == withdrawal.token
                            && event.amount.checked_add(event.bouncebackFee)
                                == Some(withdrawal.amount),
                        "combined receipt deposit-bounceback event {index} does not match supplied withdrawal"
                    );
                }
                (0, WithdrawalReceiptEvent::DepositBounceBackPending(event)) => {
                    eyre::ensure!(
                        event.tempoRefundRecipient == withdrawal.to
                            && event.token == withdrawal.token
                            && event.amount.checked_add(event.bouncebackFee)
                                == Some(withdrawal.amount),
                        "combined receipt pending deposit-bounceback event {index} does not match supplied withdrawal"
                    );
                }
                (0, WithdrawalReceiptEvent::Processed(_)) => {
                    return Err(eyre::eyre!(
                        "combined receipt event {index} used WithdrawalProcessed for a deposit bounceback"
                    ));
                }
                (_, WithdrawalReceiptEvent::Processed(event)) => {
                    eyre::ensure!(
                        event.to == withdrawal.to
                            && event.senderTag == withdrawal.senderTag
                            && event.token == withdrawal.token
                            && event.amount == withdrawal.amount,
                        "combined receipt WithdrawalProcessed event {index} does not match supplied withdrawal"
                    );
                }
                (
                    _,
                    WithdrawalReceiptEvent::DepositBounceBack(_)
                    | WithdrawalReceiptEvent::DepositBounceBackPending(_),
                ) => {
                    return Err(eyre::eyre!(
                        "combined receipt event {index} used a deposit-bounceback event for a regular withdrawal"
                    ));
                }
            }
        }

        Ok(withdrawals.len())
    }

    fn sign_settlement_attestation(
        &self,
        signer: &PrivateKeySigner,
        metadata: PortalSubmissionMetadata,
        attestation: SettlementAttestationInput<'_>,
    ) -> Result<Bytes> {
        let SettlementAttestationInput {
            batch,
            anchor_block_number,
            anchor_block_hash,
            block_transition,
            deposit_transition,
            verifier_config,
        } = attestation;
        let domain = eip712_domain! {
            name: "ZonePortal",
            version: "1",
            chain_id: metadata.stable.chain_id,
            verifying_contract: self.portal_address,
        };
        let message = SettlementAttestation {
            zoneId: metadata.stable.zone_id,
            sequencerSetVersion: metadata.sequencer_set_version,
            zoneHeight: U256::from(batch.zone_height),
            withdrawalBatchIndex: U256::from(batch.withdrawal_batch_index),
            verifier: metadata.verifier,
            tempoBlockNumber: batch.tempo_block_number,
            anchorBlockNumber: anchor_block_number,
            anchorBlockHash: anchor_block_hash,
            blockTransitionHash: keccak256(block_transition.abi_encode()),
            depositQueueTransitionHash: keccak256(deposit_transition.abi_encode()),
            withdrawalQueueHash: batch.withdrawal_queue_hash,
            verifierConfigHash: keccak256(verifier_config),
        };
        let digest = message.eip712_signing_hash(&domain);
        let signature = signer.sign_hash_sync(&digest)?;
        let mut encoded = Vec::with_capacity(65);
        encoded.extend_from_slice(&signature.r().to_be_bytes::<32>());
        encoded.extend_from_slice(&signature.s().to_be_bytes::<32>());
        encoded.push(signature.v() as u8 + 27);
        Ok(encoded.into())
    }

    /// Read all mutable portal state needed for one submission at a single L1 block.
    ///
    /// The portal and chain identifiers are immutable, so the first call includes and caches
    /// them. Sequencer membership, verifier configuration, and queue state are deliberately
    /// refreshed on every submission.
    async fn read_submission_metadata(&self, signer: Address) -> Result<PortalSubmissionMetadata> {
        if let Some(stable) = self.stable_portal_metadata.get().copied() {
            let (
                queue_head,
                queue_tail,
                withdrawal_batch_index,
                sequencer_set_version,
                sequencer_threshold,
                signer_is_sequencer,
                verifier,
            ) = self
                .l1_provider
                .multicall()
                .add(self.portal.withdrawalQueueHead())
                .add(self.portal.withdrawalQueueTail())
                .add(self.portal.withdrawalBatchIndex())
                .add(self.portal.sequencerSetVersion())
                .add(self.portal.sequencerThreshold())
                .add(self.portal.isSequencer(signer))
                .add(self.portal.verifier())
                .aggregate()
                .await?;
            return Self::build_submission_metadata(
                RawPortalSubmissionMetadata {
                    queue_head,
                    queue_tail,
                    withdrawal_batch_index,
                    sequencer_set_version,
                    sequencer_threshold,
                    signer_is_sequencer,
                    verifier,
                },
                stable,
            );
        }

        let (
            queue_head,
            queue_tail,
            withdrawal_batch_index,
            sequencer_set_version,
            sequencer_threshold,
            signer_is_sequencer,
            verifier,
            zone_id,
            chain_id,
        ) = self
            .l1_provider
            .multicall()
            .add(self.portal.withdrawalQueueHead())
            .add(self.portal.withdrawalQueueTail())
            .add(self.portal.withdrawalBatchIndex())
            .add(self.portal.sequencerSetVersion())
            .add(self.portal.sequencerThreshold())
            .add(self.portal.isSequencer(signer))
            .add(self.portal.verifier())
            .add(self.portal.zoneId())
            .get_chain_id()
            .aggregate()
            .await?;
        let stable = StablePortalMetadata {
            zone_id,
            chain_id: chain_id
                .try_into()
                .map_err(|_| eyre::eyre!("Tempo L1 chain ID overflow"))?,
        };
        let _ = self.stable_portal_metadata.set(stable);
        Self::build_submission_metadata(
            RawPortalSubmissionMetadata {
                queue_head,
                queue_tail,
                withdrawal_batch_index,
                sequencer_set_version,
                sequencer_threshold,
                signer_is_sequencer,
                verifier,
            },
            stable,
        )
    }

    fn build_submission_metadata(
        raw: RawPortalSubmissionMetadata,
        stable: StablePortalMetadata,
    ) -> Result<PortalSubmissionMetadata> {
        Ok(PortalSubmissionMetadata {
            queue_head: raw
                .queue_head
                .try_into()
                .map_err(|_| eyre::eyre!("withdrawal queue head overflow"))?,
            queue_tail: raw
                .queue_tail
                .try_into()
                .map_err(|_| eyre::eyre!("withdrawal queue tail overflow"))?,
            withdrawal_batch_index: raw.withdrawal_batch_index,
            stable,
            sequencer_set_version: raw.sequencer_set_version,
            sequencer_threshold: raw.sequencer_threshold,
            signer_is_sequencer: raw.signer_is_sequencer,
            verifier: raw.verifier,
        })
    }

    fn validate_submission_metadata(
        &self,
        batch: &BatchData,
        metadata: PortalSubmissionMetadata,
    ) -> Result<()> {
        let expected_l2_index = metadata
            .withdrawal_batch_index
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("portal withdrawal batch index overflow"))?;
        eyre::ensure!(
            batch.withdrawal_batch_index == expected_l2_index,
            "withdrawal batch index mismatch for zone block {}: L2 finalized index {}, expected portal index + 1 ({expected_l2_index})",
            batch.zone_height,
            batch.withdrawal_batch_index,
        );
        eyre::ensure!(
            metadata.sequencer_threshold > 0,
            "portal sequencer threshold is zero"
        );
        if self.attestation_store.is_none() {
            eyre::ensure!(
                metadata.sequencer_threshold == 1,
                "minimal TIP-1091 compatibility supports only a 1-of-1 sequencer set; portal threshold is {}",
                metadata.sequencer_threshold
            );
        }
        if !batch.withdrawal_queue_hash.is_zero() {
            let pending = metadata.queue_tail.saturating_sub(metadata.queue_head);
            eyre::ensure!(
                pending < WITHDRAWAL_QUEUE_CAPACITY,
                "withdrawal queue full ({pending} pending slots, capacity {WITHDRAWAL_QUEUE_CAPACITY})"
            );
        }
        Ok(())
    }

    /// Decode the `BatchSubmitted` event from a confirmed `submitBatch` receipt's logs.
    fn decode_batch_submitted(
        &self,
        logs: &[alloy_rpc_types_eth::Log],
    ) -> Result<ZonePortal::BatchSubmitted> {
        logs.iter()
            .filter(|log| log.address() == self.portal_address)
            .find_map(|log| ZonePortal::BatchSubmitted::decode_log(&log.inner).ok())
            .map(|log| log.data)
            .ok_or_else(|| {
                eyre::eyre!("confirmed submitBatch receipt is missing the BatchSubmitted event")
            })
    }

    /// Validate that a collected certificate commits to the exact calldata this submitter will
    /// send, and derive the anchor mode from the signed statement instead of recomputing it.
    async fn validate_certificate(
        &self,
        batch: &BatchData,
        zone_height: u64,
        metadata: PortalSubmissionMetadata,
        certificate: &SettlementCertificate,
    ) -> Result<AnchorMode> {
        if certificate.height != zone_height {
            return Err(eyre::eyre!(
                "settlement certificate height {} does not match batch height {zone_height}",
                certificate.height
            ));
        }
        let attestation = &certificate.attestation;

        let expected_block_transition_hash = alloy_primitives::keccak256(
            (batch.prev_block_hash, batch.next_block_hash).abi_encode(),
        );
        let expected_deposit_transition_hash = alloy_primitives::keccak256(
            (
                batch.prev_processed_deposit_hash,
                batch.next_processed_deposit_hash,
                batch.prev_deposit_number,
                batch.next_deposit_number,
            )
                .abi_encode(),
        );

        // Run a bunch of checks to verify that whats in the attestation certificate is exactly what
        // we expect. `submitBatch` will revert if any of these are wrong, so we should catch it early.
        eyre::ensure!(
            attestation.zoneId == metadata.stable.zone_id,
            "certificate zone ID changed"
        );
        eyre::ensure!(
            attestation.sequencerSetVersion == metadata.sequencer_set_version,
            "certificate signer-set version changed"
        );
        eyre::ensure!(
            attestation.zoneHeight == U256::from(zone_height),
            "certificate zone height changed"
        );
        eyre::ensure!(
            attestation.withdrawalBatchIndex == U256::from(batch.withdrawal_batch_index),
            "certificate withdrawal batch index changed"
        );
        eyre::ensure!(
            attestation.verifier == metadata.verifier,
            "certificate verifier changed"
        );
        eyre::ensure!(
            attestation.tempoBlockNumber == batch.tempo_block_number,
            "certificate Tempo block changed"
        );
        eyre::ensure!(
            attestation.blockTransitionHash == expected_block_transition_hash,
            "certificate block transition changed"
        );
        eyre::ensure!(
            attestation.depositQueueTransitionHash == expected_deposit_transition_hash,
            "certificate deposit transition changed"
        );
        eyre::ensure!(
            attestation.withdrawalQueueHash == batch.withdrawal_queue_hash,
            "certificate withdrawal queue hash changed"
        );
        eyre::ensure!(
            attestation.verifierConfigHash == alloy_primitives::keccak256(Bytes::new()),
            "certificate verifier config changed"
        );

        let current_l1_block = self.l1_provider.get_block_number().await?;
        eyre::ensure!(
            attestation.anchorBlockNumber < current_l1_block,
            "certificate anchor block is not yet available through EIP-2935"
        );
        eyre::ensure!(
            current_l1_block.saturating_sub(attestation.anchorBlockNumber)
                < self.anchor_config.history_window(),
            "certificate anchor block fell outside the EIP-2935 history window"
        );

        let anchor = self
            .l1_provider
            .get_block_by_number(attestation.anchorBlockNumber.into())
            .await?
            .ok_or_eyre(format!(
                "missing certified L1 anchor block {}",
                attestation.anchorBlockNumber
            ))?;
        eyre::ensure!(
            anchor.header.hash == attestation.anchorBlockHash,
            "certificate anchor hash changed"
        );

        if attestation.anchorBlockNumber == batch.tempo_block_number {
            Ok(AnchorMode::Direct)
        } else {
            eyre::ensure!(
                attestation.anchorBlockNumber > batch.tempo_block_number,
                "certificate ancestry anchor does not follow its Tempo block"
            );
            let ancestry_headers = self
                .fetch_ancestry_headers(batch.tempo_block_number, attestation.anchorBlockNumber)
                .await?;
            Ok(AnchorMode::Ancestry {
                anchor_block: attestation.anchorBlockNumber,
                ancestry_headers,
            })
        }
    }

    /// Resolve the anchor mode for the given `tempo_block_number`.
    ///
    /// - **Direct** (gap < configured effective window): the portal reads the
    ///   hash directly from EIP-2935.
    /// - **Ancestry** (gap ≥ configured effective window): a recent L1 block
    ///   behind the configured safety margin is used as anchor. Ancestry headers
    ///   are collected and validated for future prover integration.
    async fn resolve_anchor_mode(&self, tempo_block_number: u64) -> Result<(AnchorMode, u64)> {
        let current_l1_block = self.l1_provider.get_block_number().await?;

        if tempo_block_number >= current_l1_block {
            return Err(eyre::eyre!(
                "tempo_block_number ({tempo_block_number}) is not yet confirmed on L1 \
                 (tip={current_l1_block}), will retry after L1 advances"
            ));
        }

        let gap = current_l1_block.saturating_sub(tempo_block_number);

        if gap < self.anchor_config.effective_window() {
            // The cache is only useful during ancestry recovery. Replace it
            // instead of clearing it so the hash table's allocation is freed.
            let has_cached_headers = !self.ancestry_header_cache.read().is_empty();
            if has_cached_headers {
                *self.ancestry_header_cache.write() =
                    LruMap::new(ByLength::new(DEFAULT_ANCESTRY_HEADER_CACHE_CAPACITY));
            }
            return Ok((AnchorMode::Direct, current_l1_block));
        }

        let anchor_block = current_l1_block.saturating_sub(self.anchor_config.safety_margin());
        let ancestry_headers = self
            .fetch_ancestry_headers(tempo_block_number, anchor_block)
            .await?;

        warn!(
            tempo_block_number,
            current_l1_block,
            anchor_block,
            gap,
            header_count = ancestry_headers.len(),
            total_bytes = ancestry_headers.iter().map(|h| h.len()).sum::<usize>(),
            "tempo_block_number outside EIP-2935 effective window, using ancestry mode"
        );

        Ok((
            AnchorMode::Ancestry {
                anchor_block,
                ancestry_headers,
            },
            current_l1_block,
        ))
    }

    /// Fetch and RLP-encode L1 block headers from `from + 1` to `to` (inclusive),
    /// validating the parent-hash chain and reusing cached overlapping headers.
    ///
    /// Returns headers in ascending block-number order. The first header's
    /// `parent_hash` is validated against the hash of block `from`, ensuring the
    /// chain is rooted at the expected block.
    async fn fetch_ancestry_headers(&self, from: u64, to: u64) -> Result<Vec<Bytes>> {
        use futures::stream;

        if to <= from {
            return Ok(Vec::new());
        }

        // Snapshot the cache without changing its LRU order. Network requests
        // and validation happen after the read lock is released.
        let (cached, missing) = {
            let cache = self.ancestry_header_cache.read();
            let mut cached = Vec::new();
            let mut missing = Vec::new();
            for block_number in from..=to {
                if let Some(header) = cache.peek(&block_number) {
                    cached.push((block_number, header.clone()));
                } else {
                    missing.push(block_number);
                }
            }
            (cached, missing)
        };
        let cache_hits = cached.len();

        // Fetch and encode only the cache misses.
        let fetched = stream::iter(missing.iter().copied())
            .map(|block_number| {
                let provider = &self.l1_provider;
                async move {
                    let header = provider
                        .get_header_by_number(block_number.into())
                        .await?
                        .ok_or_else(|| {
                            eyre::eyre!("L1 header not found for block {block_number}")
                        })?;
                    let header = header.inner.inner;
                    let mut encoded = Vec::with_capacity(600);
                    header.encode(&mut encoded);
                    let cached_header = CachedAncestryHeader {
                        parent_hash: header.inner.parent_hash,
                        hash: alloy_primitives::keccak256(&encoded),
                        encoded: Bytes::from(encoded),
                    };
                    Ok::<_, eyre::Report>((block_number, cached_header))
                }
            })
            .buffer_unordered(self.l1_fetch_concurrency)
            .try_collect::<Vec<_>>()
            .await?;

        // Pure resolution owns merging, ordering, completeness, duplicate, and
        // parent-hash validation. Do not mutate the cache unless it succeeds.
        let ResolvedAncestry {
            headers,
            fetched_headers,
        } = resolve_ancestry_headers(from, to, cached, fetched)?;
        let fetched_count = fetched_headers.len();

        // Commit only entries fetched from the snapshot's misses. Another task
        // may have filled one while the network requests were in flight.
        let mut cache = self.ancestry_header_cache.write();
        for (block_number, header) in fetched_headers {
            if let Some(existing) = cache.peek(&block_number) {
                if existing.hash != header.hash {
                    return Err(eyre::eyre!(
                        "conflicting L1 header at cached block {block_number}: \
                         cached={}, fetched={}",
                        existing.hash,
                        header.hash
                    ));
                }
                continue;
            }
            if !cache.insert(block_number, header) {
                return Err(eyre::eyre!(
                    "failed to cache L1 header for block {block_number}"
                ));
            }
        }

        info!(
            from,
            to,
            cache_hits,
            fetched = fetched_count,
            "resolved ancestry headers"
        );

        Ok(headers)
    }

    /// Read the current `blockHash` from the ZonePortal on L1.
    ///
    /// Used to resync the monitor's `prev_block_hash` after repeated submission
    /// failures, ensuring subsequent batches use the portal's actual state.
    pub async fn read_portal_block_hash(&self) -> Result<B256> {
        let hash = self.portal.blockHash().call().await?;
        Ok(hash)
    }

    /// Read the current withdrawal queue bounds in one Multicall3 request.
    async fn read_portal_withdrawal_queue_bounds(&self) -> Result<(u64, u64)> {
        let (head, tail) = self
            .l1_provider
            .multicall()
            .add(self.portal.withdrawalQueueHead())
            .add(self.portal.withdrawalQueueTail())
            .aggregate()
            .await?;
        Ok((
            head.try_into()
                .map_err(|_| eyre::eyre!("withdrawal queue head overflow"))?,
            tail.try_into()
                .map_err(|_| eyre::eyre!("withdrawal queue tail overflow"))?,
        ))
    }

    /// Re-populate the in-memory [`WithdrawalStore`](crate::withdrawals::WithdrawalStore)
    /// after a sequencer restart.
    ///
    /// The L1 portal stores only hash chains, not the actual [`Withdrawal`](abi::Withdrawal)
    /// structs. This method reconstructs them by:
    ///
    /// 1. Reading `withdrawalQueueHead` / `withdrawalQueueTail` from the **L1 portal**
    ///    to determine which slots are still pending.
    /// 2. Querying the `BatchSubmitted` event for each pending slot (plus the
    ///    predecessor for zone block range boundaries) via the indexed
    ///    `withdrawalQueueIndex` topic.
    /// 3. Resolving each event's `nextBlockHash` to a **zone L2** block number.
    /// 4. Fetching `WithdrawalRequested` events from the **zone L2** outbox in
    ///    the corresponding block range.
    /// 5. Reading the head slot's current on-chain hash for partial processing
    ///    detection.
    /// 6. Verifying the hash chain and trimming already-processed withdrawals.
    ///
    /// Returns a map of portal_slot → verified withdrawals ready to be stored.
    #[instrument(skip_all, fields(portal = %self.portal_address))]
    pub async fn fetch_pending_withdrawals<P: ZoneSequencerProvider>(
        &self,
        zone_provider: &P,
        outbox_address: Address,
    ) -> Result<BTreeMap<u64, Vec<abi::Withdrawal>>> {
        // Step 1: read pending slot range from the L1 portal.
        let (head, tail) = self.read_portal_withdrawal_queue_bounds().await?;

        if head >= tail {
            info!(head, tail, "No pending withdrawals to restore");
            return Ok(BTreeMap::new());
        }

        info!(
            head,
            tail,
            pending = tail - head,
            "Restoring pending withdrawals"
        );

        // Step 2: query BatchSubmitted events for pending slots [head, tail)
        // plus the predecessor (head-1) by their indexed withdrawalQueueIndex.
        let events = self
            .find_batch_events_by_index(head.saturating_sub(1), tail)
            .await?;

        // Step 3: resolve each L1 event's nextBlockHash to a zone L2 block number.
        // Maps portal_slot → last zone L2 block in that batch.
        let mut zone_end_by_slot: BTreeMap<u64, u64> = BTreeMap::new();
        for (&portal_slot, event) in &events {
            let block_number = zone_provider
                .block_number(event.nextBlockHash)?
                .ok_or_else(|| {
                    eyre::eyre!(
                        "zone block not found for hash {} (portal slot {portal_slot})",
                        event.nextBlockHash
                    )
                })?;
            zone_end_by_slot.insert(portal_slot, block_number);
        }

        // Step 4: fetch WithdrawalRequested events from zone L2 for each pending slot.
        let mut slot_withdrawals: BTreeMap<u64, Vec<abi::Withdrawal>> = BTreeMap::new();
        for portal_slot in head..tail {
            if !events.contains_key(&portal_slot) {
                continue;
            }
            let zone_end = zone_end_by_slot[&portal_slot];
            let zone_start = if portal_slot == 0 {
                1
            } else if let Some(prev_end) = zone_end_by_slot.get(&(portal_slot - 1)) {
                prev_end + 1
            } else {
                warn!(
                    portal_slot,
                    "predecessor event missing, cannot determine zone block range start"
                );
                continue;
            };
            let withdrawals =
                fetch_slot_withdrawals(zone_provider, outbox_address, zone_start, zone_end).await?;
            slot_withdrawals.insert(portal_slot, withdrawals);
        }

        // Step 5: read the head slot's current on-chain hash (for partial processing detection).
        let head_slot_hash = self
            .portal
            .withdrawalQueueSlot(U256::from(head % WITHDRAWAL_QUEUE_CAPACITY))
            .call()
            .await?;

        // Guard: verify the queue didn't change during the multi-RPC replay.
        let (head2, tail2) = self.read_portal_withdrawal_queue_bounds().await?;

        if head2 != head || tail2 != tail {
            eyre::bail!(
                "withdrawal queue changed during restore ({}..{} -> {}..{}), retry on next startup",
                head,
                tail,
                head2,
                tail2
            );
        }

        // Step 6: resolve all fetched data into verified withdrawal sets.
        resolve_pending_slots(head, tail, &events, &slot_withdrawals, head_slot_hash)
    }

    /// Fetch `BatchSubmitted` events for logical queue indices `[first_index, tail)`
    /// by walking L1 backwards in chunks while filtering by the indexed
    /// `withdrawalQueueIndex` topic. Logical queue indices never repeat
    /// (head/tail are non-wrapping counters), so the topic filter identifies
    /// each batch exactly without positional counting.
    ///
    /// The caller passes `first_index = head - 1` so the predecessor batch is
    /// included (its `nextBlockHash` bounds the zone block range of the first
    /// pending slot). When `head == 0` the predecessor does not exist; the
    /// caller falls back to zone block 1.
    async fn find_batch_events_by_index(
        &self,
        first_index: u64,
        tail: u64,
    ) -> Result<BTreeMap<u64, abi::ZonePortal::BatchSubmitted>> {
        if first_index >= tail {
            return Ok(BTreeMap::new());
        }

        let index_topics: Vec<B256> = (first_index..tail)
            .map(|index| B256::from(U256::from(index)))
            .collect();
        let needed = index_topics.len();

        let mut found = BTreeMap::new();
        let mut hi = self.l1_provider.get_block_number().await?;

        while found.len() < needed {
            let lo = backward_log_query_start(hi, 0);

            let events = self
                .portal
                .BatchSubmitted_filter()
                .topic2(index_topics.clone())
                .from_block(lo)
                .to_block(hi)
                .query()
                .await?;

            for (event, _) in events {
                let index: u64 = event.withdrawalQueueIndex.try_into().map_err(|_| {
                    eyre::eyre!("withdrawal queue index overflow in BatchSubmitted")
                })?;
                if found.insert(index, event).is_some() {
                    eyre::bail!("duplicate BatchSubmitted event for portal queue index {index}");
                }
            }

            if lo == 0 {
                break;
            }
            hi = lo - 1;
        }

        Ok(found)
    }
}

/// Data required to submit a single batch to the ZonePortal on L1.
///
/// Produced by the zone block builder and sent to [`BatchSubmitter`] via channel.
#[derive(Debug, Clone)]
pub struct BatchData {
    /// Zone L2 height committed by this batch.
    pub zone_height: u64,
    /// Tempo L1 block number for EIP-2935 verification.
    pub tempo_block_number: u64,
    /// Previous zone block hash (must match portal's current `blockHash`).
    pub prev_block_hash: B256,
    /// New zone block hash after this batch.
    pub next_block_hash: B256,
    /// Deposit queue: where the zone started processing.
    pub prev_processed_deposit_hash: B256,
    /// Deposit queue: where the zone processed up to.
    pub next_processed_deposit_hash: B256,
    /// Deposit counter at the start of processing.
    pub prev_deposit_number: u64,
    /// Deposit counter after processing.
    pub next_deposit_number: u64,
    /// Withdrawal queue hash for this batch (`B256::ZERO` if no withdrawals).
    pub withdrawal_queue_hash: B256,
    /// L2 withdrawal batch index validated against the portal before submission.
    pub withdrawal_batch_index: u64,
}

/// One L2 withdrawal batch finalized by `ZoneOutbox`.
#[derive(Debug, Clone)]
pub(crate) struct FinalizedBatch {
    /// Authoritative hash emitted by `BatchFinalized` and stored in `lastBatch()`.
    pub finalized_hash: B256,
    /// Authoritative L2 withdrawal batch index emitted by `BatchFinalized`.
    pub finalized_index: u64,
    /// Reconstructed withdrawal payloads for the off-chain processor store.
    pub withdrawals: Vec<abi::Withdrawal>,
}

#[derive(Debug, Clone)]
pub(crate) struct FinalizedBatchLog {
    pub(crate) block_number: u64,
    tx_index: u64,
    log_index: u64,
    tx_hash: B256,
    withdrawal_queue_hash: B256,
    withdrawal_batch_index: u64,
}

/// Zone L2 state read at a specific block, used to populate [`BatchData`].
pub(crate) struct ZoneBlockSnapshot {
    /// Latest Tempo L1 block number as seen by the zone.
    pub tempo_block_number: u64,
    /// Cumulative hash of all deposits processed by the zone up to this block.
    pub processed_deposit_hash: B256,
    /// Total number of deposits processed by the zone up to this block.
    pub processed_deposit_number: u64,
    /// Zone L2 block hash.
    pub block_hash: B256,
}

struct SettlementAttestationInput<'a> {
    batch: &'a BatchData,
    anchor_block_number: u64,
    anchor_block_hash: B256,
    block_transition: &'a BlockTransition,
    deposit_transition: &'a DepositQueueTransition,
    verifier_config: &'a Bytes,
}

#[derive(Debug, Clone, Copy)]
struct StablePortalMetadata {
    zone_id: u32,
    chain_id: u64,
}

struct RawPortalSubmissionMetadata {
    queue_head: U256,
    queue_tail: U256,
    withdrawal_batch_index: u64,
    sequencer_set_version: u64,
    sequencer_threshold: u8,
    signer_is_sequencer: bool,
    verifier: Address,
}

#[derive(Debug, Clone, Copy)]
struct PortalSubmissionMetadata {
    queue_head: u64,
    queue_tail: u64,
    withdrawal_batch_index: u64,
    stable: StablePortalMetadata,
    sequencer_set_version: u64,
    sequencer_threshold: u8,
    signer_is_sequencer: bool,
    verifier: Address,
}
/// One validated L1 header retained for ancestry proof construction.
#[derive(Debug, Clone)]
struct CachedAncestryHeader {
    parent_hash: B256,
    hash: B256,
    encoded: Bytes,
}

/// A complete, ordered, parent-linked ancestry range.
///
/// `headers` excludes the base block at `from`; `fetched_headers` contains only
/// entries that the caller should commit to the cache after resolution succeeds.
#[derive(Debug)]
struct ResolvedAncestry {
    headers: Vec<Bytes>,
    fetched_headers: Vec<(u64, CachedAncestryHeader)>,
}

#[derive(Debug)]
struct RequestedWithdrawalLog {
    block_number: u64,
    tx_index: u64,
    log_index: u64,
    tx_hash: B256,
    event: abi::IZoneOutbox::WithdrawalRequested,
}

/// How the batch submitter anchors `tempoBlockNumber` for EIP-2935 verification.
///
/// Resolved by [`BatchSubmitter::resolve_anchor_mode`] inside `submit_batch`.
/// `submit_batch` can use ancestry mode when the batch-final block's
/// `tempoBlockNumber` has fallen outside the configured direct-submission
/// window.
#[allow(dead_code)] // Ancestry::ancestry_headers is collected but not yet consumed — available for prover integration
enum AnchorMode {
    /// `tempoBlockNumber` is within the effective EIP-2935 window — the portal
    /// reads its hash directly. No extra proof data required.
    Direct,
    /// `tempoBlockNumber` is outside the effective window. A recent L1 block is
    /// used as anchor, and the collected headers prove the parent-hash chain.
    Ancestry {
        /// Recent L1 block number within the EIP-2935 window, used as the
        /// on-chain anchor for hash verification.
        anchor_block: u64,
        /// RLP-encoded L1 block headers from `tempo_block_number + 1` to
        /// `anchor_block`, in ascending order. Available for the prover to
        /// consume when integrated.
        ancestry_headers: Vec<Bytes>,
    },
}

impl AnchorMode {
    /// Returns the `recentTempoBlockNumber` argument for `submitBatch`:
    /// `0` for direct mode, or the anchor block number for ancestry mode.
    const fn recent_block_number(&self) -> u64 {
        match self {
            Self::Direct => 0,
            Self::Ancestry { anchor_block, .. } => *anchor_block,
        }
    }

    const fn anchor_block_number(&self, tempo_block_number: u64) -> u64 {
        match self {
            Self::Direct => tempo_block_number,
            Self::Ancestry { anchor_block, .. } => *anchor_block,
        }
    }
}

impl fmt::Display for AnchorMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct => f.write_str("direct"),
            Self::Ancestry { .. } => f.write_str("ancestry"),
        }
    }
}

/// Merge cached and fetched headers into one validated ancestry range.
fn resolve_ancestry_headers(
    from: u64,
    to: u64,
    cached: Vec<(u64, CachedAncestryHeader)>,
    fetched: Vec<(u64, CachedAncestryHeader)>,
) -> Result<ResolvedAncestry> {
    debug_assert!(from < to, "caller skips empty ancestry ranges");

    let range_len = (to - from + 1) as usize;
    let fetched_count = fetched.len();
    let mut merged = vec![None; range_len];

    let mut insert = |block_number, header, was_fetched| -> Result<()> {
        if !(from..=to).contains(&block_number) {
            return Err(eyre::eyre!(
                "received out-of-range L1 header for block {block_number}; expected {from}..={to}"
            ));
        }
        let index = (block_number - from) as usize;
        if merged[index].replace((header, was_fetched)).is_some() {
            return Err(eyre::eyre!(
                "received duplicate L1 header for block {block_number}"
            ));
        }
        Ok(())
    };
    for (block_number, header) in cached {
        insert(block_number, header, false)?;
    }
    for (block_number, header) in fetched {
        insert(block_number, header, true)?;
    }

    let mut merged = merged.into_iter();
    let (base, base_was_fetched) = merged
        .next()
        .flatten()
        .ok_or_else(|| eyre::eyre!("L1 header not found for base block {from}"))?;
    let mut parent_hash = base.hash;
    let mut headers = Vec::with_capacity(range_len - 1);
    let mut fetched_headers = Vec::with_capacity(fetched_count);
    if base_was_fetched {
        fetched_headers.push((from, base));
    }

    for (block_number, entry) in ((from + 1)..=to).zip(merged) {
        let (header, was_fetched) =
            entry.ok_or_else(|| eyre::eyre!("L1 header not found for block {block_number}"))?;
        if header.parent_hash != parent_hash {
            return Err(eyre::eyre!(
                "parent-hash chain broken at block {block_number}: \
                 expected parent_hash={parent_hash}, got={}",
                header.parent_hash
            ));
        }
        parent_hash = header.hash;
        headers.push(header.encoded.clone());
        if was_fetched {
            fetched_headers.push((block_number, header));
        }
    }

    Ok(ResolvedAncestry {
        headers,
        fetched_headers,
    })
}

/// Pure function that resolves pre-fetched data into verified withdrawal sets
/// ready to be stored.
///
/// For each pending portal slot in `[head, tail)`:
/// 1. Skips slots with no `BatchSubmitted` event or no fetched withdrawals.
/// 2. Verifies the hash chain of fetched withdrawals matches the L1 event's
///    `withdrawalQueueHash`.
/// 3. For the head slot, trims already-processed withdrawals using
///    `head_slot_hash` (the current on-chain slot hash). The L1 portal
///    processes withdrawals one-by-one, updating the slot hash after each.
///    If the sequencer crashed mid-slot, some are already consumed but `head`
///    hasn't advanced yet.
/// 4. Non-head slots are always fully unprocessed.
///
/// Returns a map of portal_slot → verified withdrawals to store.
fn resolve_pending_slots(
    head: u64,
    tail: u64,
    events: &BTreeMap<u64, abi::ZonePortal::BatchSubmitted>,
    slot_withdrawals: &BTreeMap<u64, Vec<abi::Withdrawal>>,
    head_slot_hash: B256,
) -> Result<BTreeMap<u64, Vec<abi::Withdrawal>>> {
    let mut result: BTreeMap<u64, Vec<abi::Withdrawal>> = BTreeMap::new();

    for portal_slot in head..tail {
        let Some(event) = events.get(&portal_slot) else {
            eyre::bail!("no BatchSubmitted event found for pending portal slot {portal_slot}");
        };

        let Some(withdrawals) = slot_withdrawals.get(&portal_slot) else {
            eyre::bail!("no withdrawal data fetched for pending portal slot {portal_slot}");
        };

        if withdrawals.is_empty()
            || abi::Withdrawal::queue_hash(withdrawals) != event.withdrawalQueueHash
        {
            eyre::bail!("withdrawal hash mismatch or empty for portal slot {portal_slot}");
        }

        if portal_slot == head {
            match find_processed_offset(withdrawals, head_slot_hash) {
                Some(offset) => {
                    let remaining = withdrawals[offset..].to_vec();
                    if !remaining.is_empty() {
                        result.insert(portal_slot, remaining);
                    }
                }
                None => {
                    eyre::bail!("cannot determine processed offset for head slot {portal_slot}");
                }
            }
        } else {
            result.insert(portal_slot, withdrawals.clone());
        }
    }

    Ok(result)
}

/// Find the offset into `withdrawals` where the remaining hash chain matches
/// `current_slot_hash`. Returns `Some(0)` if no withdrawals have been processed,
/// `Some(n)` if n have been processed (n remaining), or `None` if no match is
/// found.
///
/// Also checks `offset == len` (all consumed, hash chain = `B256::ZERO`).
pub(crate) fn find_processed_offset(
    withdrawals: &[abi::Withdrawal],
    current_slot_hash: B256,
) -> Option<usize> {
    for offset in 0..=withdrawals.len() {
        let hash = abi::Withdrawal::queue_hash(&withdrawals[offset..]);
        if hash == current_slot_hash {
            return Some(offset);
        }
    }
    None
}

fn block_with_receipts<P: ZoneSequencerProvider>(
    provider: &P,
    number: u64,
) -> Result<(Block, Vec<TempoReceipt>)> {
    let block = provider
        .block_by_number(number)?
        .ok_or_else(|| eyre::eyre!("canonical zone block {number} not found"))?;
    let receipts = provider
        .receipts_by_block(BlockHashOrNumber::Number(number))?
        .ok_or_else(|| eyre::eyre!("receipts for canonical zone block {number} not found"))?;
    if block.body.transactions.len() != receipts.len() {
        return Err(eyre::eyre!(
            "zone block {number} has {} transactions but {} receipts",
            block.body.transactions.len(),
            receipts.len()
        ));
    }
    Ok((block, receipts))
}

/// Read the settlement commitments emitted by the deterministic system transaction in a zone
/// block.
pub(crate) fn read_zone_block_snapshot<P: ZoneSequencerProvider>(
    provider: &P,
    inbox_address: Address,
    number: u64,
) -> Result<ZoneBlockSnapshot> {
    let (_, receipts) = block_with_receipts(provider, number)?;
    let mut tempo_block_number = None;
    let mut processed_deposit_hash = None;
    let mut processed_deposit_number = None;

    for receipt in receipts {
        for log in receipt.logs() {
            if log.address != inbox_address
                || log.topics().first() != Some(&IZoneInbox::TempoAdvanced::SIGNATURE_HASH)
            {
                continue;
            }
            let event = IZoneInbox::TempoAdvanced::decode_log(log)
                .map_err(|err| eyre::eyre!("invalid TempoAdvanced log in block {number}: {err}"))?;
            if tempo_block_number.replace(event.tempoBlockNumber).is_some() {
                return Err(eyre::eyre!(
                    "zone block {number} contains more than one TempoAdvanced event"
                ));
            }
            processed_deposit_hash = Some(event.newProcessedDepositQueueHash);
            processed_deposit_number = Some(event.lastProcessedDepositNumber);
        }
    }

    Ok(ZoneBlockSnapshot {
        tempo_block_number: tempo_block_number
            .ok_or_else(|| eyre::eyre!("zone block {number} is missing TempoAdvanced"))?,
        processed_deposit_hash: processed_deposit_hash
            .ok_or_else(|| eyre::eyre!("zone block {number} is missing its deposit commitment"))?,
        processed_deposit_number: processed_deposit_number
            .ok_or_else(|| eyre::eyre!("zone block {number} is missing its deposit number"))?,
        block_hash: provider
            .block_hash(number)?
            .ok_or_else(|| eyre::eyre!("canonical zone block {number} is missing its hash"))?,
    })
}

/// Fetch all zone block numbers in `[from, to]` that finalized a withdrawal batch.
///
/// This includes zero-withdrawal batches because they still advance the L2
/// withdrawal batch index and therefore require a matching L1 `submitBatch`.
pub(crate) async fn fetch_finalized_batch_boundaries<P: ZoneSequencerProvider>(
    provider: &P,
    outbox_address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<FinalizedBatchLog>> {
    if from > to {
        return Ok(Vec::new());
    }

    let boundaries = fetch_finalized_batch_logs(provider, outbox_address, from, to)?;
    if let Some(duplicate) = boundaries
        .windows(2)
        .find(|pair| pair[0].block_number == pair[1].block_number)
    {
        return Err(eyre::eyre!(
            "zone block {} contains more than one BatchFinalized event",
            duplicate[0].block_number
        ));
    }
    Ok(boundaries)
}

/// Fetch one finalized L2 withdrawal batch for a range ending at `to`.
///
/// The submitted hash and index come from the supplied `BatchFinalized` event.
/// Withdrawal structs are reconstructed from `WithdrawalRequested` logs in the
/// supplied boundary-aligned range so the off-chain processor can service the
/// portal queue.
pub(crate) async fn fetch_finalized_batch<P: ZoneSequencerProvider>(
    zone_provider: &P,
    outbox_address: Address,
    from: u64,
    target: &FinalizedBatchLog,
) -> Result<FinalizedBatch> {
    let to = target.block_number;
    let request_from = from;

    let requests = if request_from <= to {
        fetch_requested_withdrawal_logs(zone_provider, outbox_address, request_from, to)?
    } else {
        Vec::new()
    };

    let (block, _) = block_with_receipts(zone_provider, target.block_number)?;
    let finalize_tx = block
        .body
        .transactions
        .iter()
        .find(|tx| *tx.tx_hash() == target.tx_hash)
        .ok_or_else(|| {
            eyre::eyre!(
                "missing finalizeWithdrawalBatch tx {} for zone block {}",
                target.tx_hash,
                target.block_number
            )
        })?;
    let encrypted_senders =
        abi::IZoneOutbox::finalizeWithdrawalBatchCall::abi_decode(finalize_tx.input().as_ref())
            .map_err(|err| {
                eyre::eyre!(
                    "failed to decode finalizeWithdrawalBatch calldata for {}: {err}",
                    target.tx_hash
                )
            })?
            .encryptedSenders;

    if encrypted_senders.len() != requests.len() {
        return Err(eyre::eyre!(
            "encrypted sender count mismatch for batch ending at zone block {}: {} encrypted senders for {} requests",
            target.block_number,
            encrypted_senders.len(),
            requests.len()
        ));
    }

    let withdrawals = requests
        .into_iter()
        .zip(encrypted_senders)
        .map(|(request, encrypted_sender)| {
            abi::Withdrawal::from_requested_event(&request.event, request.tx_hash, encrypted_sender)
        })
        .collect::<Vec<_>>();

    let recomputed_hash = abi::Withdrawal::queue_hash(&withdrawals);
    if recomputed_hash != target.withdrawal_queue_hash {
        return Err(eyre::eyre!(
            "withdrawal hash mismatch for batch ending at zone block {}: event hash {}, reconstructed hash {}",
            target.block_number,
            target.withdrawal_queue_hash,
            recomputed_hash
        ));
    }

    Ok(FinalizedBatch {
        finalized_hash: target.withdrawal_queue_hash,
        finalized_index: target.withdrawal_batch_index,
        withdrawals,
    })
}

/// Fetch `WithdrawalRequested` events for one portal queue slot.
pub(crate) async fn fetch_slot_withdrawals(
    zone_provider: &impl ZoneSequencerProvider,
    outbox_address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<abi::Withdrawal>> {
    let boundaries =
        fetch_finalized_batch_boundaries(zone_provider, outbox_address, to, to).await?;
    let target = boundaries
        .into_iter()
        .next()
        .ok_or_else(|| eyre::eyre!("zone block {to} does not contain a BatchFinalized boundary"))?;
    Ok(
        fetch_finalized_batch(zone_provider, outbox_address, from, &target)
            .await?
            .withdrawals,
    )
}

fn fetch_requested_withdrawal_logs<P: ZoneSequencerProvider>(
    provider: &P,
    outbox_address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<RequestedWithdrawalLog>> {
    let mut requests = Vec::new();
    for block_number in from..=to {
        let (block, receipts) = block_with_receipts(provider, block_number)?;
        for (tx_index, (tx, receipt)) in block
            .body
            .transactions
            .iter()
            .zip(receipts.iter())
            .enumerate()
        {
            for (log_index, log) in receipt.logs().iter().enumerate() {
                if log.address != outbox_address
                    || log.topics().first()
                        != Some(&IZoneOutbox::WithdrawalRequested::SIGNATURE_HASH)
                {
                    continue;
                }
                requests.push(RequestedWithdrawalLog {
                    block_number,
                    tx_index: tx_index as u64,
                    log_index: log_index as u64,
                    tx_hash: *tx.tx_hash(),
                    event: IZoneOutbox::WithdrawalRequested::decode_log(log)
                        .map_err(|err| {
                            eyre::eyre!(
                                "invalid WithdrawalRequested log in zone block {block_number}: {err}"
                            )
                        })?
                        .data,
                });
            }
        }
    }
    requests.sort_by_key(|request| (request.block_number, request.tx_index, request.log_index));

    Ok(requests)
}

fn fetch_finalized_batch_logs<P: ZoneSequencerProvider>(
    provider: &P,
    outbox_address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<FinalizedBatchLog>> {
    let mut finalized_batches = Vec::new();
    for block_number in from..=to {
        let (block, receipts) = block_with_receipts(provider, block_number)?;
        for (tx_index, (tx, receipt)) in block
            .body
            .transactions
            .iter()
            .zip(receipts.iter())
            .enumerate()
        {
            for (log_index, log) in receipt.logs().iter().enumerate() {
                if log.address != outbox_address
                    || log.topics().first() != Some(&IZoneOutbox::BatchFinalized::SIGNATURE_HASH)
                {
                    continue;
                }
                let event = IZoneOutbox::BatchFinalized::decode_log(log).map_err(|err| {
                    eyre::eyre!("invalid BatchFinalized log in zone block {block_number}: {err}")
                })?;
                finalized_batches.push(FinalizedBatchLog {
                    block_number,
                    tx_index: tx_index as u64,
                    log_index: log_index as u64,
                    tx_hash: *tx.tx_hash(),
                    withdrawal_queue_hash: event.withdrawalQueueHash,
                    withdrawal_batch_index: event.withdrawalBatchIndex,
                });
            }
        }
    }
    finalized_batches.sort_by_key(|batch| (batch.block_number, batch.tx_index, batch.log_index));
    Ok(finalized_batches)
}

fn backward_log_query_start(hi: u64, floor: u64) -> u64 {
    hi.saturating_sub(LOG_QUERY_BLOCK_CHUNK - 1).max(floor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi;
    use alloy_consensus::{Header as ConsensusHeader, ReceiptWithBloom};
    use alloy_primitives::{B256, address};
    use alloy_provider::ProviderBuilder;
    use alloy_rpc_types_eth::{Header as RpcHeader, TransactionReceipt};
    use alloy_sol_types::SolValue;
    use alloy_transport::mock::Asserter;
    use proptest::prelude::*;
    use tempo_alloy::rpc::TempoHeaderResponse;
    use tempo_primitives::{TempoHeader, TempoTxType};

    fn abi_word(value: impl SolValue) -> Bytes {
        value.abi_encode().into()
    }

    fn abi_encode_multicall(values: Vec<Bytes>) -> Bytes {
        (U256::ZERO, values).abi_encode_params().into()
    }

    fn mock_l1_header(number: u64, parent_hash: B256) -> (TempoHeaderResponse, B256) {
        let header = TempoHeader {
            inner: ConsensusHeader {
                number,
                parent_hash,
                ..Default::default()
            },
            ..Default::default()
        };
        let hash = alloy_primitives::keccak256(alloy_rlp::encode(&header));
        (
            TempoHeaderResponse {
                inner: RpcHeader {
                    hash,
                    inner: header,
                    total_difficulty: None,
                    size: None,
                },
                timestamp_millis: 0,
            },
            hash,
        )
    }

    fn synthetic_ancestry(from: u64, payloads: &[Vec<u8>]) -> Vec<(u64, CachedAncestryHeader)> {
        let mut parent_hash = B256::ZERO;
        payloads
            .iter()
            .enumerate()
            .map(|(index, payload)| {
                let block_number = from + u64::try_from(index).unwrap();
                let mut encoded = Vec::with_capacity(size_of::<u64>() + payload.len());
                encoded.extend_from_slice(&block_number.to_be_bytes());
                encoded.extend_from_slice(payload);
                let encoded = Bytes::from(encoded);
                let hash = alloy_primitives::keccak256(&encoded);
                let header = CachedAncestryHeader {
                    parent_hash,
                    hash,
                    encoded,
                };
                parent_hash = hash;
                (block_number, header)
            })
            .collect()
    }

    fn ancestry_case() -> impl Strategy<Value = (u64, Vec<Vec<u8>>, Vec<u64>)> {
        (0_u64..10_000, 2_usize..33).prop_flat_map(|(from, len)| {
            (
                Just(from),
                proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..64), len),
                proptest::collection::vec(any::<u64>(), len),
            )
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn ancestry_resolution_is_independent_of_fetched_order(
            (from, payloads, order_keys) in ancestry_case(),
        ) {
            let chain = synthetic_ancestry(from, &payloads);
            let to = chain.last().unwrap().0;
            let expected = resolve_ancestry_headers(from, to, Vec::new(), chain.clone())
                .unwrap()
                .headers;

            let mut permuted = chain
                .into_iter()
                .zip(order_keys)
                .collect::<Vec<_>>();
            permuted.sort_by_key(|(_, key)| *key);
            let permuted = permuted
                .into_iter()
                .map(|(header, _)| header)
                .collect();

            let actual = resolve_ancestry_headers(from, to, Vec::new(), permuted)
                .unwrap()
                .headers;
            prop_assert_eq!(actual, expected);
        }

        #[test]
        fn ancestry_resolution_is_independent_of_cache_partition(
            (from, payloads, order_keys) in ancestry_case(),
            cache_mask in any::<u128>(),
        ) {
            let chain = synthetic_ancestry(from, &payloads);
            let to = chain.last().unwrap().0;
            let cold = resolve_ancestry_headers(from, to, Vec::new(), chain.clone())
                .unwrap()
                .headers;
            let (cached, fetched): (Vec<_>, Vec<_>) = chain
                .into_iter()
                .enumerate()
                .partition(|(index, _)| cache_mask & (1_u128 << index) != 0);
            let cached = cached.into_iter().map(|(_, header)| header).collect();
            let mut fetched = fetched
                .into_iter()
                .map(|(index, header)| (order_keys[index], header))
                .collect::<Vec<_>>();
            fetched.sort_by_key(|(order_key, _)| *order_key);
            let fetched = fetched
                .into_iter()
                .map(|(_, header)| header)
                .collect::<Vec<_>>();
            let mut expected_fetched = fetched
                .iter()
                .map(|(block_number, header)| (*block_number, header.hash))
                .collect::<Vec<_>>();
            expected_fetched.sort_by_key(|(block_number, _)| *block_number);

            let partitioned = resolve_ancestry_headers(from, to, cached, fetched)
                .unwrap();
            let actual_fetched = partitioned
                .fetched_headers
                .iter()
                .map(|(block_number, header)| (*block_number, header.hash))
                .collect::<Vec<_>>();
            prop_assert_eq!(partitioned.headers, cold);
            prop_assert_eq!(actual_fetched, expected_fetched);
        }

        #[test]
        fn ancestry_resolution_rejects_parent_hash_corruption(
            (from, payloads, _) in ancestry_case(),
            corrupt_index in any::<usize>(),
        ) {
            let mut chain = synthetic_ancestry(from, &payloads);
            let to = chain.last().unwrap().0;
            let corrupt_index = 1 + corrupt_index % (chain.len() - 1);
            chain[corrupt_index].1.parent_hash[0] ^= 1;

            prop_assert!(resolve_ancestry_headers(from, to, Vec::new(), chain).is_err());
        }

        #[test]
        fn ancestry_resolution_rejects_malformed_header_sets(
            (from, payloads, _) in ancestry_case(),
            malformed_index in any::<usize>(),
        ) {
            let chain = synthetic_ancestry(from, &payloads);
            let to = chain.last().unwrap().0;
            let malformed_index = malformed_index % chain.len();

            let mut missing = chain.clone();
            missing.remove(malformed_index);
            prop_assert!(
                resolve_ancestry_headers(from, to, Vec::new(), missing).is_err(),
                "missing header was accepted"
            );

            let mut duplicate = chain.clone();
            duplicate.push(chain[malformed_index].clone());
            prop_assert!(
                resolve_ancestry_headers(from, to, Vec::new(), duplicate).is_err(),
                "duplicate header was accepted"
            );

            let mut out_of_range = chain.clone();
            out_of_range.push((to + 1, chain[malformed_index].1.clone()));
            prop_assert!(
                resolve_ancestry_headers(from, to, Vec::new(), out_of_range).is_err(),
                "out-of-range header was accepted"
            );
        }

        #[test]
        fn ancestry_resolution_returns_exact_range_without_base(
            (from, payloads, _) in ancestry_case(),
        ) {
            let chain = synthetic_ancestry(from, &payloads);
            let to = chain.last().unwrap().0;
            let expected = chain[1..]
                .iter()
                .map(|(_, header)| header.encoded.clone())
                .collect::<Vec<_>>();

            let resolved = resolve_ancestry_headers(from, to, Vec::new(), chain).unwrap();
            prop_assert_eq!(resolved.headers.len(), usize::try_from(to - from).unwrap());
            prop_assert_eq!(resolved.headers, expected);
        }
    }

    fn test_withdrawal(to: Address, amount: u128) -> abi::Withdrawal {
        abi::Withdrawal {
            token: address!("0x0000000000000000000000000000000000001000"),
            senderTag: B256::repeat_byte(0x11),
            to,
            amount,
            memo: B256::ZERO,
            gasLimit: 0,
            fallbackNonce: 1,
            callbackData: Default::default(),
            encryptedSender: Default::default(),
        }
    }

    #[test]
    fn batch_anchor_config_validates_effective_window() {
        let config = BatchAnchorConfig::new(10, 4).unwrap();
        assert_eq!(config.history_window(), 10);
        assert_eq!(config.safety_margin(), 4);
        assert_eq!(config.effective_window(), 6);

        assert!(BatchAnchorConfig::new(0, 0).is_err());
        assert!(BatchAnchorConfig::new(10, 10).is_err());
        assert!(BatchAnchorConfig::new(10, 11).is_err());
    }

    #[test]
    fn combined_submission_requires_empty_fifo_queue() {
        assert!(queue_allows_combined_submission(7, 7));
        assert!(!queue_allows_combined_submission(7, 8));
        assert!(!queue_allows_combined_submission(7, 10));
    }

    #[test]
    fn combined_request_orders_calls_and_uses_settlement_nonce_lane() {
        let portal = Address::repeat_byte(0x11);
        let sender = Address::repeat_byte(0x22);
        let withdrawal = test_withdrawal(Address::repeat_byte(0x33), 42);
        let submit_input = Bytes::copy_from_slice(&ZonePortal::submitBatchCall::SELECTOR);
        let request = BatchSubmitter::build_combined_submission_request(
            portal,
            sender,
            42431,
            9,
            CombinedWithdrawalPlan {
                gas_limit: 7_500_000,
                remaining_queue: B256::ZERO,
            },
            submit_input.clone(),
            std::slice::from_ref(&withdrawal),
        );

        assert_eq!(request.calls.len(), 2);
        assert_eq!(request.calls[0].to, portal.into());
        assert_eq!(request.calls[0].input, submit_input);
        assert_eq!(request.calls[1].to, portal.into());
        let process = ZonePortal::processWithdrawalsCall::abi_decode(&request.calls[1].input)
            .expect("second call should decode as processWithdrawals");
        assert_eq!(process.withdrawals, vec![withdrawal]);
        assert_eq!(process.remainingQueue, B256::ZERO);
        assert_eq!(request.nonce_key, Some(SUBMIT_BATCH_NONCE_KEY));
        assert_eq!(request.inner.nonce, Some(9));
        assert_eq!(request.inner.gas, Some(7_500_000));
        assert_eq!(request.inner.chain_id, Some(42431));
    }

    #[tokio::test]
    async fn combined_request_hash_is_deterministic_for_restart_recovery() {
        let signer = PrivateKeySigner::random();
        let request = BatchSubmitter::build_combined_submission_request(
            Address::repeat_byte(0x11),
            signer.address(),
            42431,
            9,
            CombinedWithdrawalPlan {
                gas_limit: 7_500_000,
                remaining_queue: B256::ZERO,
            },
            Bytes::copy_from_slice(&ZonePortal::submitBatchCall::SELECTOR),
            &[test_withdrawal(Address::repeat_byte(0x33), 42)],
        );

        let first = BatchSubmitter::prepare_combined_submission(&signer, request.clone())
            .await
            .unwrap();
        let second = BatchSubmitter::prepare_combined_submission(&signer, request)
            .await
            .unwrap();

        assert_eq!(first.transaction_hash, second.transaction_hash);
        assert_eq!(first.envelope, second.envelope);
    }

    #[tokio::test]
    async fn durable_combined_store_refuses_mutated_same_nonce_envelope() {
        let signer = PrivateKeySigner::random();
        let path = std::env::temp_dir().join(format!(
            "zone-pending-combined-{}-{}.rlp",
            std::process::id(),
            signer.address()
        ));
        let store =
            PendingCombinedSubmissionStore::new(path, std::env::temp_dir()).expect("valid store");
        let first = BatchSubmitter::prepare_combined_submission(
            &signer,
            BatchSubmitter::build_combined_submission_request(
                Address::repeat_byte(0x11),
                signer.address(),
                42431,
                9,
                CombinedWithdrawalPlan {
                    gas_limit: 7_500_000,
                    remaining_queue: B256::ZERO,
                },
                Bytes::from_static(&[0x01]),
                &[test_withdrawal(Address::repeat_byte(0x33), 42)],
            ),
        )
        .await
        .unwrap();
        let mutated = BatchSubmitter::prepare_combined_submission(
            &signer,
            BatchSubmitter::build_combined_submission_request(
                Address::repeat_byte(0x11),
                signer.address(),
                42431,
                9,
                CombinedWithdrawalPlan {
                    gas_limit: 7_500_000,
                    remaining_queue: B256::ZERO,
                },
                Bytes::from_static(&[0x02]),
                &[test_withdrawal(Address::repeat_byte(0x33), 42)],
            ),
        )
        .await
        .unwrap();

        let forced_error = store
            .persist_with_after_link_hook(&first.envelope, || {
                Err(eyre::eyre!("forced failure after canonical hard link"))
            })
            .unwrap_err();
        assert!(forced_error.to_string().contains("forced failure"));
        assert_eq!(
            store.load().unwrap().unwrap().tx_hash(),
            &first.transaction_hash
        );

        // The idempotent retry must repeat and complete the durability barrier before it can
        // return success; a mutated same-nonce envelope still cannot replace the canonical link.
        store.persist(&first.envelope).unwrap();
        let error = store.persist(&mutated.envelope).unwrap_err();
        assert!(error.to_string().contains("refusing to replace"));
        assert_eq!(
            store.load().unwrap().unwrap().tx_hash(),
            &first.transaction_hash
        );
        store.clear(first.transaction_hash).unwrap();
        assert!(!store.exists().unwrap());
    }

    #[tokio::test]
    async fn durable_combined_envelope_restores_exact_batch_and_withdrawals() {
        let signer = PrivateKeySigner::random();
        let portal = Address::repeat_byte(0x11);
        let withdrawal = test_withdrawal(Address::repeat_byte(0x33), 42);
        let withdrawal_queue_hash = abi::Withdrawal::queue_hash(std::slice::from_ref(&withdrawal));
        let batch = BatchData {
            zone_height: 17,
            tempo_block_number: 100,
            prev_block_hash: B256::repeat_byte(0x41),
            next_block_hash: B256::repeat_byte(0x42),
            prev_processed_deposit_hash: B256::repeat_byte(0x51),
            next_processed_deposit_hash: B256::repeat_byte(0x52),
            prev_deposit_number: 3,
            next_deposit_number: 5,
            withdrawal_queue_hash,
            withdrawal_batch_index: 7,
        };
        let request = BatchSubmitter::build_combined_submission_request(
            portal,
            signer.address(),
            42431,
            9,
            CombinedWithdrawalPlan {
                gas_limit: 7_500_000,
                remaining_queue: B256::ZERO,
            },
            ZonePortal::submitBatchCall {
                tempoBlockNumber: batch.tempo_block_number,
                recentTempoBlockNumber: batch.tempo_block_number,
                blockTransition: BlockTransition {
                    prevBlockHash: batch.prev_block_hash,
                    nextBlockHash: batch.next_block_hash,
                },
                depositQueueTransition: DepositQueueTransition {
                    prevProcessedHash: batch.prev_processed_deposit_hash,
                    nextProcessedHash: batch.next_processed_deposit_hash,
                    prevDepositNumber: batch.prev_deposit_number,
                    nextDepositNumber: batch.next_deposit_number,
                },
                withdrawalQueueHash: batch.withdrawal_queue_hash,
                verifierConfig: Bytes::new(),
                proof: Bytes::new(),
                nextZoneHeight: U256::from(batch.zone_height),
                signatures: vec![Bytes::from_static(&[0x01])],
            }
            .abi_encode()
            .into(),
            std::slice::from_ref(&withdrawal),
        );
        let prepared = BatchSubmitter::prepare_combined_submission(&signer, request)
            .await
            .unwrap();
        let asserter = Asserter::new();
        asserter.push_success(&42431_u64);
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter)
            .erased();
        let path = std::env::temp_dir().join(format!(
            "zone-pending-combined-decode-{}-{}.rlp",
            std::process::id(),
            signer.address()
        ));
        let mut submitter = BatchSubmitter::with_signer_and_anchor_config(
            portal,
            provider,
            signer,
            BatchAnchorConfig::default(),
        );
        submitter
            .set_combined_submission_store_path(path, std::env::temp_dir())
            .unwrap();
        submitter
            .combined_submission_store
            .as_ref()
            .unwrap()
            .persist(&prepared.envelope)
            .unwrap();

        let decoded = submitter
            .load_pending_combined_submission()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(decoded.transaction_hash, prepared.transaction_hash);
        BatchSubmitter::ensure_persisted_submission_matches_batch(
            &decoded,
            &batch,
            std::slice::from_ref(&withdrawal),
        )
        .unwrap();

        let mut changed_batch = batch;
        changed_batch.next_block_hash = B256::repeat_byte(0x99);
        assert!(
            BatchSubmitter::ensure_persisted_submission_matches_batch(
                &decoded,
                &changed_batch,
                std::slice::from_ref(&withdrawal),
            )
            .is_err()
        );
        submitter
            .clear_pending_combined_submission(prepared.transaction_hash)
            .unwrap();
    }

    #[tokio::test]
    async fn startup_recovery_reconciles_and_clears_persisted_combined_receipt() {
        let signer = PrivateKeySigner::random();
        let portal = Address::repeat_byte(0x11);
        let withdrawal = test_withdrawal(Address::repeat_byte(0x33), 42);
        let withdrawal_queue_hash = abi::Withdrawal::queue_hash(std::slice::from_ref(&withdrawal));
        let next_block_hash = B256::repeat_byte(0x42);
        let next_deposit_hash = B256::repeat_byte(0x52);
        let request = BatchSubmitter::build_combined_submission_request(
            portal,
            signer.address(),
            42431,
            9,
            CombinedWithdrawalPlan {
                gas_limit: 7_500_000,
                remaining_queue: B256::ZERO,
            },
            ZonePortal::submitBatchCall {
                tempoBlockNumber: 100,
                recentTempoBlockNumber: 100,
                blockTransition: BlockTransition {
                    prevBlockHash: B256::repeat_byte(0x41),
                    nextBlockHash: next_block_hash,
                },
                depositQueueTransition: DepositQueueTransition {
                    prevProcessedHash: B256::repeat_byte(0x51),
                    nextProcessedHash: next_deposit_hash,
                    prevDepositNumber: 3,
                    nextDepositNumber: 5,
                },
                withdrawalQueueHash: withdrawal_queue_hash,
                verifierConfig: Bytes::new(),
                proof: Bytes::new(),
                nextZoneHeight: U256::from(17),
                signatures: vec![Bytes::from_static(&[0x01])],
            }
            .abi_encode()
            .into(),
            std::slice::from_ref(&withdrawal),
        );
        let prepared = BatchSubmitter::prepare_combined_submission(&signer, request)
            .await
            .unwrap();
        let batch_event = ZonePortal::BatchSubmitted {
            withdrawalBatchIndex: 7,
            withdrawalQueueIndex: U256::from(3),
            nextProcessedDepositQueueHash: next_deposit_hash,
            nextBlockHash: next_block_hash,
            withdrawalQueueHash: withdrawal_queue_hash,
            lastProcessedDepositNumber: 5,
        };
        let withdrawal_event = ZonePortal::WithdrawalProcessed {
            to: withdrawal.to,
            senderTag: withdrawal.senderTag,
            token: withdrawal.token,
            amount: withdrawal.amount,
            callbackSuccess: true,
        };
        let logs = vec![
            alloy_rpc_types_eth::Log {
                inner: alloy_primitives::Log {
                    address: portal,
                    data: batch_event.encode_log_data(),
                },
                ..Default::default()
            },
            alloy_rpc_types_eth::Log {
                inner: alloy_primitives::Log {
                    address: portal,
                    data: withdrawal_event.encode_log_data(),
                },
                ..Default::default()
            },
        ];
        let receipt = TempoTransactionReceipt {
            inner: TransactionReceipt {
                inner: ReceiptWithBloom::from(TempoReceipt {
                    tx_type: TempoTxType::AA,
                    success: true,
                    cumulative_gas_used: 1_000_000,
                    logs,
                }),
                transaction_hash: prepared.transaction_hash,
                transaction_index: Some(0),
                block_hash: Some(B256::repeat_byte(0x77)),
                block_number: Some(123),
                gas_used: 1_000_000,
                effective_gas_price: 1,
                blob_gas_used: None,
                blob_gas_price: None,
                from: signer.address(),
                to: Some(portal),
                contract_address: None,
            },
            fee_token: None,
            fee_payer: signer.address(),
        };
        let asserter = Asserter::new();
        asserter.push_success(&42431_u64);
        asserter.push_success(&Some(receipt));
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter.clone())
            .erased();
        let path = std::env::temp_dir().join(format!(
            "zone-pending-combined-recovery-{}-{}.rlp",
            std::process::id(),
            signer.address()
        ));
        let mut submitter = BatchSubmitter::with_signer_and_anchor_config(
            portal,
            provider,
            signer,
            BatchAnchorConfig::default(),
        );
        submitter
            .set_combined_submission_store_path(path, std::env::temp_dir())
            .unwrap();
        submitter
            .combined_submission_store
            .as_ref()
            .unwrap()
            .persist(&prepared.envelope)
            .unwrap();

        submitter
            .recover_persisted_combined_submission()
            .await
            .unwrap();
        assert!(!submitter.has_pending_combined_submission().unwrap());
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn ancestry_header_cache_fetches_only_new_suffix() {
        let asserter = Asserter::new();
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter.clone())
            .erased();
        let submitter = BatchSubmitter::new(Address::ZERO, provider);
        *submitter.ancestry_header_cache.write() = LruMap::new(ByLength::new(4));

        let mut parent_hash = B256::ZERO;
        let mut headers = Vec::new();
        for number in 10..=15 {
            let (header, hash) = mock_l1_header(number, parent_hash);
            headers.push(header);
            parent_hash = hash;
        }

        // The initial range fetches its base plus all ancestry headers.
        for header in &headers[..5] {
            asserter.push_success(header);
        }
        let first = submitter.fetch_ancestry_headers(10, 14).await.unwrap();
        let expected_first = headers[1..5]
            .iter()
            .map(|header| Bytes::from(alloy_rlp::encode(&header.inner.inner)))
            .collect::<Vec<_>>();
        assert_eq!(first, expected_first);
        assert_eq!(submitter.ancestry_header_cache.read().len(), 4);

        // The overlapping range reuses blocks 11..=14 and fetches only block 15.
        // If the implementation repeats any cached RPC call, the mock has no
        // additional response queued and the test fails.
        asserter.push_success(&headers[5]);
        let second = submitter.fetch_ancestry_headers(11, 15).await.unwrap();
        let expected_second = headers[2..6]
            .iter()
            .map(|header| Bytes::from(alloy_rlp::encode(&header.inner.inner)))
            .collect::<Vec<_>>();
        assert_eq!(second, expected_second);

        let cache = submitter.ancestry_header_cache.read();
        assert!(cache.peek(&11).is_none());
        for block_number in 12..=15 {
            assert!(cache.peek(&block_number).is_some());
        }
    }

    #[tokio::test]
    async fn ancestry_header_cache_hits_do_not_rewrite_entries() {
        let asserter = Asserter::new();
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter.clone())
            .erased();
        let submitter = BatchSubmitter::new(Address::ZERO, provider);
        *submitter.ancestry_header_cache.write() = LruMap::new(ByLength::new(4));

        let mut parent_hash = B256::ZERO;
        for number in 10..=13 {
            let (header, hash) = mock_l1_header(number, parent_hash);
            asserter.push_success(&header);
            parent_hash = hash;
        }

        submitter.fetch_ancestry_headers(10, 13).await.unwrap();
        assert_eq!(
            submitter
                .ancestry_header_cache
                .read()
                .peek_oldest()
                .map(|(block_number, _)| *block_number),
            Some(10)
        );

        // Resolving a fully cached range must not promote or replace every hit.
        submitter.fetch_ancestry_headers(10, 12).await.unwrap();
        assert_eq!(
            submitter
                .ancestry_header_cache
                .read()
                .peek_oldest()
                .map(|(block_number, _)| *block_number),
            Some(10)
        );
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn anchor_resolution_returns_observed_l1_tip() {
        let asserter = Asserter::new();
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter.clone())
            .erased();
        let submitter = BatchSubmitter::new(Address::ZERO, provider);

        asserter.push_success(&100_u64);
        let (mode, current_l1_block) = submitter.resolve_anchor_mode(99).await.unwrap();

        assert!(matches!(mode, AnchorMode::Direct));
        assert_eq!(current_l1_block, 100);
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn submission_metadata_multicalls_mutable_values_and_caches_stable_values() {
        let asserter = Asserter::new();
        let portal_address = Address::repeat_byte(0x22);
        let signer = Address::repeat_byte(0x33);
        let verifier = Address::repeat_byte(0x44);
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter.clone())
            .erased();
        let submitter = BatchSubmitter::new(portal_address, provider);

        asserter.push_success(&abi_encode_multicall(vec![
            abi_word(U256::from(3)),
            abi_word(U256::from(5)),
            abi_word(7_u64),
            abi_word(11_u64),
            abi_word(U256::from(1)),
            abi_word(true),
            abi_word(verifier),
            abi_word(42_u32),
            abi_word(U256::from(42431)),
        ]));
        let first = submitter.read_submission_metadata(signer).await.unwrap();
        assert_eq!(first.queue_head, 3);
        assert_eq!(first.queue_tail, 5);
        assert_eq!(first.withdrawal_batch_index, 7);
        assert_eq!(first.sequencer_set_version, 11);
        assert!(first.signer_is_sequencer);
        assert_eq!(first.verifier, verifier);
        assert_eq!(first.stable.zone_id, 42);
        assert_eq!(first.stable.chain_id, 42431);

        let next_verifier = Address::repeat_byte(0x55);
        asserter.push_success(&abi_encode_multicall(vec![
            abi_word(U256::from(4)),
            abi_word(U256::from(6)),
            abi_word(8_u64),
            abi_word(12_u64),
            abi_word(U256::from(1)),
            abi_word(false),
            abi_word(next_verifier),
        ]));
        let second = submitter.read_submission_metadata(signer).await.unwrap();
        assert_eq!(second.queue_head, 4);
        assert_eq!(second.queue_tail, 6);
        assert_eq!(second.withdrawal_batch_index, 8);
        assert_eq!(second.sequencer_set_version, 12);
        assert!(!second.signer_is_sequencer);
        assert_eq!(second.verifier, next_verifier);
        assert_eq!(second.stable.zone_id, 42);
        assert_eq!(second.stable.chain_id, 42431);
        assert!(asserter.read_q().is_empty());
    }

    #[test]
    fn submission_metadata_requires_one_of_one_without_attestation_store() {
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(Asserter::new())
            .erased();
        let mut submitter = BatchSubmitter::new(Address::ZERO, provider);
        let batch = BatchData {
            zone_height: 1,
            tempo_block_number: 1,
            prev_block_hash: B256::ZERO,
            next_block_hash: B256::repeat_byte(0x11),
            prev_processed_deposit_hash: B256::ZERO,
            next_processed_deposit_hash: B256::ZERO,
            prev_deposit_number: 0,
            next_deposit_number: 0,
            withdrawal_queue_hash: B256::ZERO,
            withdrawal_batch_index: 1,
        };
        let metadata = PortalSubmissionMetadata {
            queue_head: 0,
            queue_tail: 0,
            withdrawal_batch_index: 0,
            stable: StablePortalMetadata {
                zone_id: 1,
                chain_id: 1,
            },
            sequencer_set_version: 1,
            sequencer_threshold: 2,
            signer_is_sequencer: true,
            verifier: Address::ZERO,
        };

        let err = submitter
            .validate_submission_metadata(&batch, metadata)
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("supports only a 1-of-1 sequencer set")
        );

        submitter.set_attestation_store(Some(AttestationStore::default()));
        submitter
            .validate_submission_metadata(&batch, metadata)
            .unwrap();
    }

    #[tokio::test]
    async fn direct_anchor_resolution_drops_ancestry_cache() {
        let asserter = Asserter::new();
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter.clone())
            .erased();
        let submitter = BatchSubmitter::new(Address::ZERO, provider);

        let cached_header = CachedAncestryHeader {
            parent_hash: B256::ZERO,
            hash: B256::repeat_byte(0x11),
            encoded: Bytes::from_static(&[0x01]),
        };
        assert!(
            submitter
                .ancestry_header_cache
                .write()
                .insert(98, cached_header)
        );

        asserter.push_success(&100_u64);
        let (mode, _) = submitter.resolve_anchor_mode(99).await.unwrap();

        assert!(matches!(mode, AnchorMode::Direct));
        assert!(submitter.ancestry_header_cache.read().is_empty());
        assert!(asserter.read_q().is_empty());
    }

    #[test]
    fn find_offset_no_withdrawals_processed() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let w1 = test_withdrawal(address!("0x0000000000000000000000000000000000000002"), 200);
        let withdrawals = vec![w0, w1];
        let full_hash = abi::Withdrawal::queue_hash(&withdrawals);
        assert_eq!(find_processed_offset(&withdrawals, full_hash), Some(0));
    }

    #[test]
    fn find_offset_one_processed() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let w1 = test_withdrawal(address!("0x0000000000000000000000000000000000000002"), 200);
        let withdrawals = vec![w0, w1];
        let hash = abi::Withdrawal::queue_hash(&withdrawals[1..]);
        assert_eq!(find_processed_offset(&withdrawals, hash), Some(1));
    }

    #[test]
    fn find_offset_all_processed() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let withdrawals = vec![w0];
        // B256::ZERO = queue_hash(&[]), meaning all withdrawals have been consumed.
        assert_eq!(find_processed_offset(&withdrawals, B256::ZERO), Some(1));
    }

    #[test]
    fn find_offset_no_match() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let withdrawals = vec![w0];
        let random_hash = B256::from([0xdeu8; 32]);
        assert_eq!(find_processed_offset(&withdrawals, random_hash), None);
    }

    #[test]
    fn find_offset_single_withdrawal_unprocessed() {
        let w = test_withdrawal(address!("0x0000000000000000000000000000000000000042"), 999);
        let withdrawals = vec![w];
        let hash = abi::Withdrawal::queue_hash(&withdrawals);
        assert_eq!(find_processed_offset(&withdrawals, hash), Some(0));
    }

    #[test]
    fn find_offset_partial_three_withdrawals() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let w1 = test_withdrawal(address!("0x0000000000000000000000000000000000000002"), 200);
        let w2 = test_withdrawal(address!("0x0000000000000000000000000000000000000003"), 300);
        let withdrawals = vec![w0, w1, w2];
        let hash = abi::Withdrawal::queue_hash(&withdrawals[2..]);
        assert_eq!(find_processed_offset(&withdrawals, hash), Some(2));
    }

    #[test]
    fn backward_log_query_window_is_bounded() {
        let hi = 10_000;
        let lo = backward_log_query_start(hi, 0);
        assert_eq!(lo, hi - LOG_QUERY_BLOCK_CHUNK + 1);
        assert_eq!(hi - lo + 1, LOG_QUERY_BLOCK_CHUNK);
        assert_eq!(backward_log_query_start(100, 50), 50);
    }

    fn test_batch_event(withdrawal_queue_hash: B256) -> abi::ZonePortal::BatchSubmitted {
        abi::ZonePortal::BatchSubmitted {
            withdrawalBatchIndex: 0,
            withdrawalQueueIndex: U256::ZERO,
            nextProcessedDepositQueueHash: B256::ZERO,
            nextBlockHash: B256::ZERO,
            withdrawalQueueHash: withdrawal_queue_hash,
            lastProcessedDepositNumber: 0,
        }
    }

    #[test]
    fn decode_batch_submitted_from_receipt_logs() {
        use alloy_provider::ProviderBuilder;
        use alloy_transport::mock::Asserter;

        let portal_address = address!("0x7069DeC4E64Fd07334A0933eDe836C17259c9B23");
        let provider = ProviderBuilder::new_with_network::<tempo_alloy::TempoNetwork>()
            .connect_mocked_client(Asserter::new())
            .erased();
        let submitter = BatchSubmitter::new(portal_address, provider);

        let event = abi::ZonePortal::BatchSubmitted {
            withdrawalBatchIndex: 7,
            withdrawalQueueIndex: U256::from(3),
            nextProcessedDepositQueueHash: B256::repeat_byte(0x11),
            nextBlockHash: B256::repeat_byte(0x22),
            withdrawalQueueHash: B256::repeat_byte(0x33),
            lastProcessedDepositNumber: 9,
        };
        let log = alloy_rpc_types_eth::Log {
            inner: alloy_primitives::Log {
                address: portal_address,
                data: event.encode_log_data(),
            },
            ..Default::default()
        };
        let unrelated = alloy_rpc_types_eth::Log {
            inner: alloy_primitives::Log {
                address: Address::repeat_byte(0x99),
                data: event.encode_log_data(),
            },
            ..Default::default()
        };

        let decoded = submitter
            .decode_batch_submitted(&[unrelated.clone(), log])
            .unwrap();
        assert_eq!(decoded.withdrawalBatchIndex, 7);
        assert_eq!(decoded.withdrawalQueueIndex, U256::from(3));
        assert_eq!(decoded.nextBlockHash, B256::repeat_byte(0x22));

        assert!(submitter.decode_batch_submitted(&[unrelated]).is_err());
    }

    #[test]
    fn combined_receipt_reconciles_regular_and_deposit_bounceback_events() {
        let portal_address = address!("0x7069DeC4E64Fd07334A0933eDe836C17259c9B23");
        let provider = ProviderBuilder::new_with_network::<tempo_alloy::TempoNetwork>()
            .connect_mocked_client(Asserter::new())
            .erased();
        let submitter = BatchSubmitter::new(portal_address, provider);
        let regular = test_withdrawal(Address::repeat_byte(0x11), 42);
        let mut bounceback = test_withdrawal(Address::repeat_byte(0x44), 100);
        bounceback.fallbackNonce = 0;
        let processed = abi::ZonePortal::WithdrawalProcessed {
            to: regular.to,
            senderTag: regular.senderTag,
            token: regular.token,
            amount: regular.amount,
            callbackSuccess: true,
        };
        let processed_log = alloy_rpc_types_eth::Log {
            inner: alloy_primitives::Log {
                address: portal_address,
                data: processed.encode_log_data(),
            },
            ..Default::default()
        };
        let bounced = abi::ZonePortal::DepositBounceBack {
            tempoRefundRecipient: bounceback.to,
            token: bounceback.token,
            amount: 93,
            bouncebackFee: 7,
        };
        let bounced_log = alloy_rpc_types_eth::Log {
            inner: alloy_primitives::Log {
                address: portal_address,
                data: bounced.encode_log_data(),
            },
            ..Default::default()
        };
        let unrelated_log = alloy_rpc_types_eth::Log {
            inner: alloy_primitives::Log {
                address: Address::repeat_byte(0x99),
                data: processed.encode_log_data(),
            },
            ..Default::default()
        };

        assert_eq!(
            submitter
                .verify_withdrawal_receipt(
                    &[processed_log, unrelated_log, bounced_log],
                    &[regular, bounceback],
                    BatchSubmissionMode::SubmitAndProcessWithdrawals,
                )
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn finds_batch_events_by_logical_index_across_ring_wrap() {
        use alloy_provider::ProviderBuilder;
        use alloy_transport::mock::Asserter;

        let portal_address = address!("0x7069DeC4E64Fd07334A0933eDe836C17259c9B23");
        let asserter = Asserter::new();
        let provider = ProviderBuilder::new_with_network::<tempo_alloy::TempoNetwork>()
            .connect_mocked_client(asserter.clone())
            .erased();
        let submitter = BatchSubmitter::new(portal_address, provider);

        asserter.push_success(&10_000_u64);
        let logs: Vec<_> = [99_u64, 100, 101]
            .into_iter()
            .map(|index| {
                let event = abi::ZonePortal::BatchSubmitted {
                    withdrawalBatchIndex: index + 20,
                    withdrawalQueueIndex: U256::from(index),
                    nextProcessedDepositQueueHash: B256::ZERO,
                    nextBlockHash: B256::from(U256::from(index + 1)),
                    withdrawalQueueHash: B256::from(U256::from(index + 2)),
                    lastProcessedDepositNumber: 0,
                };
                alloy_rpc_types_eth::Log {
                    inner: alloy_primitives::Log {
                        address: portal_address,
                        data: event.encode_log_data(),
                    },
                    block_number: Some(9_900 + index),
                    ..Default::default()
                }
            })
            .collect();
        asserter.push_success(&logs);

        let events = submitter.find_batch_events_by_index(99, 102).await.unwrap();

        assert_eq!(
            events.keys().copied().collect::<Vec<_>>(),
            vec![99, 100, 101]
        );
        assert_eq!(events[&99].withdrawalBatchIndex, 119);
        assert_eq!(events[&100].withdrawalQueueIndex, U256::from(100));
        assert_eq!(events[&101].withdrawalQueueIndex, U256::from(101));
        assert!(asserter.read_q().is_empty());
    }

    #[test]
    fn resolve_empty_range() {
        let result =
            resolve_pending_slots(5, 5, &BTreeMap::new(), &BTreeMap::new(), B256::ZERO).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_single_slot_unprocessed() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let w1 = test_withdrawal(address!("0x0000000000000000000000000000000000000002"), 200);
        let withdrawals = vec![w0, w1];
        let full_hash = abi::Withdrawal::queue_hash(&withdrawals);

        let mut events = BTreeMap::new();
        events.insert(5, test_batch_event(full_hash));
        let mut slot_withdrawals = BTreeMap::new();
        slot_withdrawals.insert(5, withdrawals);

        let result = resolve_pending_slots(5, 6, &events, &slot_withdrawals, full_hash).unwrap();
        let returned = result.get(&5).unwrap();
        assert_eq!(returned.len(), 2);
        assert_eq!(abi::Withdrawal::queue_hash(returned), full_hash);
    }

    #[test]
    fn resolve_single_slot_partially_processed() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let w1 = test_withdrawal(address!("0x0000000000000000000000000000000000000002"), 200);
        let w2 = test_withdrawal(address!("0x0000000000000000000000000000000000000003"), 300);
        let withdrawals = vec![w0, w1, w2];
        let full_hash = abi::Withdrawal::queue_hash(&withdrawals);
        // head_slot_hash reflects that w0 has been processed (hash of remaining [w1, w2])
        let head_slot_hash = abi::Withdrawal::queue_hash(&withdrawals[1..]);

        let mut events = BTreeMap::new();
        events.insert(5, test_batch_event(full_hash));
        let mut slot_withdrawals = BTreeMap::new();
        slot_withdrawals.insert(5, withdrawals);

        let result =
            resolve_pending_slots(5, 6, &events, &slot_withdrawals, head_slot_hash).unwrap();
        let returned = result.get(&5).unwrap();
        assert_eq!(returned.len(), 2);
        assert_eq!(abi::Withdrawal::queue_hash(returned), head_slot_hash);
    }

    #[test]
    fn resolve_single_slot_fully_processed() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let withdrawals = vec![w0];
        let full_hash = abi::Withdrawal::queue_hash(&withdrawals);

        let mut events = BTreeMap::new();
        events.insert(5, test_batch_event(full_hash));
        let mut slot_withdrawals = BTreeMap::new();
        slot_withdrawals.insert(5, withdrawals);

        // B256::ZERO = queue_hash(&[]), all consumed. find_processed_offset returns
        // Some(1) (offset == len), so remaining is empty and slot is not stored.
        let result = resolve_pending_slots(5, 6, &events, &slot_withdrawals, B256::ZERO).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_multiple_slots() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let w1 = test_withdrawal(address!("0x0000000000000000000000000000000000000002"), 200);
        let w2 = test_withdrawal(address!("0x0000000000000000000000000000000000000003"), 300);

        let head_withdrawals = vec![w0];
        let tail_withdrawals = vec![w1, w2];

        let head_hash = abi::Withdrawal::queue_hash(&head_withdrawals);
        let tail_hash = abi::Withdrawal::queue_hash(&tail_withdrawals);

        let mut events = BTreeMap::new();
        events.insert(5, test_batch_event(head_hash));
        events.insert(6, test_batch_event(tail_hash));

        let mut slot_withdrawals = BTreeMap::new();
        slot_withdrawals.insert(5, head_withdrawals);
        slot_withdrawals.insert(6, tail_withdrawals);

        // head slot fully unprocessed (head_slot_hash == full hash of slot 5)
        let result = resolve_pending_slots(5, 7, &events, &slot_withdrawals, head_hash).unwrap();
        let slot5 = result.get(&5).unwrap();
        let slot6 = result.get(&6).unwrap();
        assert_eq!(slot5.len(), 1);
        assert_eq!(slot6.len(), 2);
        assert_eq!(abi::Withdrawal::queue_hash(slot5), head_hash);
        assert_eq!(abi::Withdrawal::queue_hash(slot6), tail_hash);
    }

    #[test]
    fn resolve_hash_mismatch_skipped() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let withdrawals = vec![w0];
        let wrong_hash = B256::from([0xabu8; 32]);

        let mut events = BTreeMap::new();
        events.insert(5, test_batch_event(wrong_hash));
        let mut slot_withdrawals = BTreeMap::new();
        slot_withdrawals.insert(5, withdrawals);

        let result = resolve_pending_slots(5, 6, &events, &slot_withdrawals, B256::ZERO);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_missing_event_skipped() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let withdrawals = vec![w0];

        let mut slot_withdrawals = BTreeMap::new();
        slot_withdrawals.insert(5, withdrawals);

        // No event for slot 5
        let result = resolve_pending_slots(5, 6, &BTreeMap::new(), &slot_withdrawals, B256::ZERO);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_head_partial_with_non_head_slot() {
        let w0 = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let w1 = test_withdrawal(address!("0x0000000000000000000000000000000000000002"), 200);
        let w2 = test_withdrawal(address!("0x0000000000000000000000000000000000000003"), 300);

        let head_withdrawals = vec![w0, w1];
        let non_head_withdrawals = vec![w2];

        let head_hash = abi::Withdrawal::queue_hash(&head_withdrawals);
        let non_head_hash = abi::Withdrawal::queue_hash(&non_head_withdrawals);
        // w0 already processed, head_slot_hash = hash of [w1] only
        let head_slot_hash = abi::Withdrawal::queue_hash(&head_withdrawals[1..]);

        let mut events = BTreeMap::new();
        events.insert(5, test_batch_event(head_hash));
        events.insert(6, test_batch_event(non_head_hash));

        let mut slot_withdrawals = BTreeMap::new();
        slot_withdrawals.insert(5, head_withdrawals);
        slot_withdrawals.insert(6, non_head_withdrawals);

        let result =
            resolve_pending_slots(5, 7, &events, &slot_withdrawals, head_slot_hash).unwrap();
        // Head slot trimmed to 1 remaining withdrawal
        assert_eq!(result.get(&5).unwrap().len(), 1);
        assert_eq!(
            abi::Withdrawal::queue_hash(result.get(&5).unwrap()),
            head_slot_hash
        );
        // Non-head slot fully present
        assert_eq!(result.get(&6).unwrap().len(), 1);
        assert_eq!(
            abi::Withdrawal::queue_hash(result.get(&6).unwrap()),
            non_head_hash
        );
    }

    #[test]
    fn resolve_empty_withdrawals_vec_skipped() {
        let mut events = BTreeMap::new();
        events.insert(5, test_batch_event(B256::from([0x11u8; 32])));

        let mut slot_withdrawals = BTreeMap::new();
        slot_withdrawals.insert(5, vec![]);

        let result = resolve_pending_slots(5, 6, &events, &slot_withdrawals, B256::ZERO);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_missing_withdrawals_data_skipped() {
        let w = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let hash = abi::Withdrawal::queue_hash(std::slice::from_ref(&w));

        let mut events = BTreeMap::new();
        events.insert(5, test_batch_event(hash));
        // slot_withdrawals has no entry for slot 5
        let slot_withdrawals = BTreeMap::new();

        let result = resolve_pending_slots(5, 6, &events, &slot_withdrawals, hash);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_head_slot_corrupted_hash_skipped() {
        let w = test_withdrawal(address!("0x0000000000000000000000000000000000000001"), 100);
        let withdrawals = vec![w];
        let full_hash = abi::Withdrawal::queue_hash(&withdrawals);
        // head_slot_hash doesn't match any tail of the withdrawal list
        let corrupted_hash = B256::from([0xdeu8; 32]);

        let mut events = BTreeMap::new();
        events.insert(5, test_batch_event(full_hash));
        let mut slot_withdrawals = BTreeMap::new();
        slot_withdrawals.insert(5, withdrawals);

        let result = resolve_pending_slots(5, 6, &events, &slot_withdrawals, corrupted_hash);
        assert!(result.is_err());
    }
}
