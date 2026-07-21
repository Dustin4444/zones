//! Fail-closed node-wide withdrawal checking.
//!
//! Zone tokens are non-transferable, so available balance is intentionally isolated by
//! `(user, token)`. A withdrawal can consume only deposits and refunds credited to its sender.
//! Before authorizing a signature, the sum for each token must also fit within the L1 Portal's
//! canonical TIP-20 balance.

use std::{collections::BTreeMap, future::Future, sync::Arc, time::Duration};

use alloy_consensus::{BlockHeader as _, TxReceipt as _};
use alloy_eips::{BlockHashOrNumber, BlockId, BlockNumHash};
use alloy_primitives::{Address, B256, Sealable as _, U256};
use alloy_provider::{DynProvider, Provider as _};
use alloy_sol_types::SolEvent as _;
use futures::StreamExt as _;
use reth_db::{
    DatabaseError,
    cursor::DbCursorRO,
    models::CompactU256,
    transaction::{DbTx, DbTxMut},
};
use reth_exex::{ExExContext, ExExHead, ExExNotification, ExExNotificationsStream};
use reth_node_api::FullNodeComponents;
use reth_primitives_traits::BlockBody as _;
use reth_provider::{DBProvider, DatabaseProviderFactory, HeaderProvider};
use reth_storage_api::{BlockNumReader, BlockReader, ReceiptProvider};
use schnellru::{ByLength, LruMap};
use tempo_alloy::TempoNetwork;
use tempo_contracts::precompiles::ITIP20;
use tempo_primitives::{Block, TempoHeader, TempoPrimitives, TempoReceipt};
use tempo_zone_contracts::{ZONE_INBOX_ADDRESS, ZoneInbox};
use tokio::sync::{mpsc, oneshot};

use crate::ZoneNode;

mod events;
mod schema;

use events::{CanonicalBlock, ZoneBalanceChange, parse_events};
#[cfg(test)]
use schema::register_tables;
use schema::{
    BalanceKey, BlockDelta, CHECKPOINT_KEY, Checkpoint, PreviousBacking, WithdrawalBacking,
    WithdrawalBlockDeltas, WithdrawalCheckerState, WithdrawalTokenBacking,
};
pub use schema::{RegisteredWithdrawalCheckerTables, register_withdrawal_checker_tables};

/// Bound Reth's ExEx backfill notification and our corresponding MDBX transaction size.
const EXEX_BACKFILL_MAX_BLOCKS: u64 = 1_024;
/// Retain enough undo history for 8,192 zone blocks (about 34 minutes at 4 blocks/second).
const BLOCK_DELTA_RETENTION: u64 = 8_192;
const DECISION_CHANNEL_CAPACITY: usize = 256;
const DECISION_TIMEOUT: Duration = Duration::from_secs(30);
const PORTAL_CHECK_CONCURRENCY: usize = 4;
const PORTAL_BALANCE_QUERY_CONCURRENCY: usize = 16;
const PORTAL_DECISION_CACHE_CAPACITY: u32 = 256;

#[derive(Debug, Clone)]
pub(crate) enum WithdrawalCheckDecision {
    Submit,
    Retry(WithdrawalCheckRetry),
    Halt(Arc<WithdrawalCheckError>),
}

#[derive(Debug, Clone)]
pub(crate) struct WithdrawalChecker {
    requests: mpsc::Sender<WithdrawalCheckRequest>,
}

#[derive(Debug)]
pub(crate) struct WithdrawalCheckRequest {
    pub(crate) target: BlockNumHash,
    deadline: tokio::time::Instant,
    response: oneshot::Sender<WithdrawalCheckDecision>,
}

pub(crate) fn withdrawal_checker_channel()
-> (WithdrawalChecker, mpsc::Receiver<WithdrawalCheckRequest>) {
    let (requests, receiver) = mpsc::channel(DECISION_CHANNEL_CAPACITY);
    (WithdrawalChecker { requests }, receiver)
}

impl WithdrawalChecker {
    pub(crate) fn unavailable() -> Self {
        let (requests, receiver) = mpsc::channel(1);
        drop(receiver);
        Self { requests }
    }

    pub(crate) async fn decide(&self, target: BlockNumHash) -> WithdrawalCheckDecision {
        let deadline = tokio::time::Instant::now() + DECISION_TIMEOUT;
        let decision = async {
            let (response, receiver) = oneshot::channel();
            self.requests
                .send(WithdrawalCheckRequest {
                    target,
                    deadline,
                    response,
                })
                .await
                .map_err(|_| ())?;
            receiver.await.map_err(|_| ())
        };

        match tokio::time::timeout_at(deadline, decision).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(())) => {
                WithdrawalCheckDecision::Halt(Arc::new(WithdrawalCheckError::Unavailable {
                    block: target.number,
                    detail: "decision actor closed",
                }))
            }
            Err(_) => WithdrawalCheckDecision::Retry(WithdrawalCheckRetry::Timeout {
                block: target.number,
            }),
        }
    }
}

impl WithdrawalCheckRequest {
    pub(crate) fn respond(self, decision: WithdrawalCheckDecision) {
        let _ = self.response.send(decision);
    }
}

#[derive(Clone)]
struct PortalBackingChecker {
    zone: u32,
    portal: Address,
    provider: DynProvider<TempoNetwork>,
}

impl PortalBackingChecker {
    async fn decide(&self, check: &PortalBackingCheck) -> WithdrawalCheckDecision {
        let block = BlockId::hash_canonical(check.l1_anchor.hash);
        let required = check
            .required
            .iter()
            .map(|(&token, &required)| (token, required));
        let mut balances = futures::stream::iter(required.collect::<Vec<_>>())
            .map(|(token, required)| {
                let provider = self.provider.clone();
                async move {
                    ITIP20::new(token, provider)
                        .balanceOf(self.portal)
                        .block(block)
                        .call()
                        .await
                        .map(|available| (token, required, available))
                }
            })
            // Preserve token order so simultaneous failures produce the same decision.
            .buffered(PORTAL_BALANCE_QUERY_CONCURRENCY);

        while let Some(balance) = balances.next().await {
            let (token, required, available) = match balance {
                Ok(balance) => balance,
                Err(error) => {
                    return WithdrawalCheckDecision::Retry(
                        WithdrawalCheckRetry::PortalBalanceUnavailable {
                            block: check.target.number,
                            detail: error.to_string(),
                        },
                    );
                }
            };
            if let Err(error) = Self::ensure_backed(
                self.zone,
                check.target.number,
                check.l1_anchor,
                token,
                required,
                available,
            ) {
                return WithdrawalCheckDecision::Halt(Arc::new(error));
            }
        }
        WithdrawalCheckDecision::Submit
    }

    fn ensure_backed(
        zone: u32,
        zone_block: u64,
        l1_anchor: BlockNumHash,
        token: Address,
        required: U256,
        available: U256,
    ) -> Result<(), WithdrawalCheckError> {
        if required > available {
            return Err(WithdrawalCheckError::PortalBackingShortfall(Box::new(
                PortalBackingShortfallError {
                    zone,
                    block: zone_block,
                    l1_block: l1_anchor.number,
                    l1_hash: l1_anchor.hash,
                    token,
                    required,
                    available,
                },
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct PortalBackingCheck {
    target: BlockNumHash,
    l1_anchor: BlockNumHash,
    required: BTreeMap<Address, U256>,
}

#[derive(Debug)]
enum PreparedCheck {
    Pending(Checkpoint),
    Portal(PortalBackingCheck),
    Retry(WithdrawalCheckRetry),
}

type PortalCheckKey = (u64, B256);

#[derive(Debug)]
struct PortalCheckResult {
    key: PortalCheckKey,
    outcome: PortalCheckOutcome,
}

#[derive(Debug)]
enum PortalCheckOutcome {
    Pending(Checkpoint),
    Retry(WithdrawalCheckRetry),
    Decision {
        check: Arc<PortalBackingCheck>,
        decision: WithdrawalCheckDecision,
    },
    Fatal(Box<WithdrawalCheckError>),
}

#[derive(Debug)]
struct CachedPortalCheck {
    check: Arc<PortalBackingCheck>,
    decision: Option<WithdrawalCheckDecision>,
}

#[derive(Debug)]
struct PortalCheckCache {
    entries: LruMap<PortalCheckKey, CachedPortalCheck>,
}

enum CachedPortalCheckHit {
    Prepared(Arc<PortalBackingCheck>),
    Decision(WithdrawalCheckDecision),
}

impl PortalCheckCache {
    fn get(&mut self, target: BlockNumHash) -> Option<CachedPortalCheckHit> {
        let cached = self.entries.get(&(target.number, target.hash))?;
        match &cached.decision {
            Some(decision) => Some(CachedPortalCheckHit::Decision(decision.clone())),
            None => Some(CachedPortalCheckHit::Prepared(cached.check.clone())),
        }
    }

    fn closest(&self, target: BlockNumHash) -> Option<Arc<PortalBackingCheck>> {
        self.entries
            .iter()
            .map(|(_, cached)| cached)
            .filter(|cached| cached.check.target.number != target.number)
            .min_by_key(|cached| cached.check.target.number.abs_diff(target.number))
            .map(|cached| cached.check.clone())
    }

    fn insert(
        &mut self,
        check: Arc<PortalBackingCheck>,
        decision: Option<WithdrawalCheckDecision>,
    ) {
        self.entries.insert(
            (check.target.number, check.target.hash),
            CachedPortalCheck { check, decision },
        );
    }

    fn remove(&mut self, target: BlockNumHash) {
        self.entries.remove(&(target.number, target.hash));
    }
}

impl Default for PortalCheckCache {
    fn default() -> Self {
        Self {
            entries: LruMap::new(ByLength::new(PORTAL_DECISION_CACHE_CAPACITY)),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PortalCheckState {
    Ready,
    Running,
    WaitingForCheckpoint,
}

struct PendingPortalCheck {
    requests: Vec<WithdrawalCheckRequest>,
    state: PortalCheckState,
}

struct PortalChecks {
    checker: PortalBackingChecker,
    cache: PortalCheckCache,
    pending: BTreeMap<PortalCheckKey, PendingPortalCheck>,
    tasks: tokio::task::JoinSet<PortalCheckResult>,
}

impl PortalChecks {
    fn new(checker: PortalBackingChecker) -> Self {
        Self {
            checker,
            cache: PortalCheckCache::default(),
            pending: BTreeMap::new(),
            tasks: tokio::task::JoinSet::new(),
        }
    }

    fn queue<P>(
        &mut self,
        ledger: &WithdrawalLedger,
        provider: &P,
        request: WithdrawalCheckRequest,
    ) -> Result<(), WithdrawalCheckError>
    where
        P: Clone
            + DatabaseProviderFactory
            + HeaderProvider<Header = TempoHeader>
            + Send
            + Sync
            + 'static,
        P::Provider: BlockReader<Block = Block, Receipt = TempoReceipt>
            + BlockNumReader
            + HeaderProvider<Header = TempoHeader>
            + ReceiptProvider<Receipt = TempoReceipt>,
    {
        let target = request.target;
        let key = (target.number, target.hash);
        if let Some(pending) = self.pending.get_mut(&key) {
            pending.requests.push(request);
            return Ok(());
        }

        self.pending.insert(
            key,
            PendingPortalCheck {
                requests: vec![request],
                state: PortalCheckState::Ready,
            },
        );
        let cached = self.cache.get(target);
        if let Some(CachedPortalCheckHit::Decision(decision)) = cached {
            let canonical = provider
                .sealed_header(target.number)
                .map_err(|error| ledger.storage(error))?;
            match canonical {
                Some(canonical) if canonical.hash() != target.hash => {
                    self.respond(
                        key,
                        WithdrawalCheckDecision::Retry(
                            WithdrawalCheckRetry::CanonicalHashMismatch {
                                block: target.number,
                                canonical: canonical.hash(),
                                target: target.hash,
                            },
                        ),
                    );
                    return Ok(());
                }
                Some(_) => {
                    self.respond(key, decision);
                    return Ok(());
                }
                None => self.cache.remove(target),
            }
        }

        self.start_available(ledger, provider);
        Ok(())
    }

    fn start_available<P>(&mut self, ledger: &WithdrawalLedger, provider: &P)
    where
        P: Clone + DatabaseProviderFactory + Send + Sync + 'static,
        P::Provider: BlockReader<Block = Block, Receipt = TempoReceipt>
            + BlockNumReader
            + HeaderProvider<Header = TempoHeader>
            + ReceiptProvider<Receipt = TempoReceipt>,
    {
        while self.tasks.len() < PORTAL_CHECK_CONCURRENCY {
            let Some((&key, pending)) = self.pending.iter().find(|(_, pending)| {
                pending.state == PortalCheckState::Ready
                    && pending
                        .requests
                        .iter()
                        .any(|request| !request.response.is_closed())
            }) else {
                break;
            };
            let request = pending
                .requests
                .iter()
                .find(|request| !request.response.is_closed())
                .expect("an open request was selected");
            let target = request.target;
            let deadline = request.deadline;
            let baseline = match self.cache.get(target) {
                Some(CachedPortalCheckHit::Prepared(check)) => Some(check),
                Some(CachedPortalCheckHit::Decision(decision)) => {
                    self.respond(key, decision);
                    continue;
                }
                None => self.cache.closest(target),
            };
            self.pending
                .get_mut(&key)
                .expect("pending check exists")
                .state = PortalCheckState::Running;
            self.spawn(*ledger, provider.clone(), target, deadline, baseline);
        }
    }

    fn spawn<P>(
        &mut self,
        ledger: WithdrawalLedger,
        provider: P,
        target: BlockNumHash,
        deadline: tokio::time::Instant,
        baseline: Option<Arc<PortalBackingCheck>>,
    ) where
        P: DatabaseProviderFactory + Send + 'static,
        P::Provider: BlockReader<Block = Block, Receipt = TempoReceipt>
            + BlockNumReader
            + HeaderProvider<Header = TempoHeader>
            + ReceiptProvider<Receipt = TempoReceipt>,
    {
        let checker = self.checker.clone();
        self.tasks.spawn(async move {
            let key = (target.number, target.hash);
            let result = async {
                let prepared = tokio::task::spawn_blocking(move || {
                    ledger.portal_check_if_ready(&provider, target, baseline.as_deref())
                })
                .await
                .map_err(|error| {
                    WithdrawalCheckError::InvalidState(format!(
                        "Portal-check reconstruction task failed: {error}"
                    ))
                })?;
                let check = match prepared? {
                    PreparedCheck::Pending(checkpoint) => {
                        return Ok(PortalCheckOutcome::Pending(checkpoint));
                    }
                    PreparedCheck::Retry(error) => {
                        return Ok(PortalCheckOutcome::Retry(error));
                    }
                    PreparedCheck::Portal(check) => Arc::new(check),
                };
                let decision = match tokio::time::timeout_at(deadline, checker.decide(&check)).await
                {
                    Ok(decision) => decision,
                    Err(_) => WithdrawalCheckDecision::Retry(WithdrawalCheckRetry::Timeout {
                        block: target.number,
                    }),
                };
                Ok(PortalCheckOutcome::Decision { check, decision })
            }
            .await;
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => PortalCheckOutcome::Fatal(Box::new(error)),
            };
            PortalCheckResult { key, outcome }
        });
    }

    async fn join_next(&mut self) -> Option<Result<PortalCheckResult, tokio::task::JoinError>> {
        self.tasks.join_next().await
    }

    fn has_running(&self) -> bool {
        !self.tasks.is_empty()
    }

    fn park(&mut self, key: PortalCheckKey) {
        if let Some(pending) = self.pending.get_mut(&key) {
            pending.state = PortalCheckState::WaitingForCheckpoint;
        }
    }

    fn retry(&mut self, key: PortalCheckKey) {
        if let Some(pending) = self.pending.get_mut(&key) {
            pending.state = PortalCheckState::Ready;
        }
    }

    fn wake_waiting(&mut self) {
        for pending in self.pending.values_mut() {
            if pending.state == PortalCheckState::WaitingForCheckpoint {
                pending.state = PortalCheckState::Ready;
            }
        }
    }

    fn complete(&mut self, check: Arc<PortalBackingCheck>, decision: WithdrawalCheckDecision) {
        let key = (check.target.number, check.target.hash);
        let Some(pending) = self.pending.remove(&key) else {
            return;
        };
        let cached =
            (!matches!(&decision, WithdrawalCheckDecision::Retry(_))).then(|| decision.clone());
        self.cache.insert(check, cached);
        for request in pending.requests {
            // Cloning the decision is cheap: fatal errors are already shared through `Arc`.
            request.respond(decision.clone());
        }
    }

    fn respond(&mut self, key: PortalCheckKey, decision: WithdrawalCheckDecision) {
        if let Some(pending) = self.pending.remove(&key) {
            for request in pending.requests {
                request.respond(decision.clone());
            }
        }
    }

    fn contains(&self, key: PortalCheckKey) -> bool {
        self.pending.contains_key(&key)
    }

    fn len(&self) -> usize {
        self.pending
            .values()
            .map(|pending| pending.requests.len().max(1))
            .sum()
    }

    fn retain_open(&mut self) {
        for pending in self.pending.values_mut() {
            pending
                .requests
                .retain(|request| !request.response.is_closed());
        }
        self.pending.retain(|_, pending| {
            !pending.requests.is_empty() || pending.state == PortalCheckState::Running
        });
    }

    fn take_requests(&mut self) -> Vec<WithdrawalCheckRequest> {
        self.tasks.abort_all();
        std::mem::take(&mut self.pending)
            .into_values()
            .flat_map(|pending| pending.requests)
            .collect()
    }
}

/// Durable node-wide withdrawal ledger.
#[derive(Debug, Clone, Copy)]
struct WithdrawalLedger {
    zone: u32,
}

impl WithdrawalLedger {
    const fn new(zone: u32) -> Self {
        Self { zone }
    }

    /// Initialize a newly-created schema at genesis, or verify restoration of an existing schema.
    fn initialize<P>(
        &self,
        provider: &P,
        tables_are_new: bool,
    ) -> Result<Checkpoint, WithdrawalCheckError>
    where
        P: HeaderProvider<Header = TempoHeader> + DatabaseProviderFactory,
    {
        let genesis = provider
            .sealed_header(0)
            .map_err(|err| self.storage(err))?
            .ok_or_else(|| self.invalid_state("canonical genesis header is missing"))?;

        let provider_rw = provider
            .database_provider_rw()
            .map_err(|err| self.storage(err))?;
        let tx = provider_rw.tx_ref();
        let checkpoint = tx
            .get::<WithdrawalCheckerState>(CHECKPOINT_KEY)
            .map_err(|err| self.storage(err))?;

        if tables_are_new {
            let backing = tx
                .entries::<WithdrawalBacking>()
                .map_err(|err| self.storage(err))?;
            let token_backing = tx
                .entries::<WithdrawalTokenBacking>()
                .map_err(|err| self.storage(err))?;
            let deltas = tx
                .entries::<WithdrawalBlockDeltas>()
                .map_err(|err| self.storage(err))?;
            let state = tx
                .entries::<WithdrawalCheckerState>()
                .map_err(|err| self.storage(err))?;
            if checkpoint.is_some()
                || backing != 0
                || token_backing != 0
                || deltas != 0
                || state != 0
            {
                return Err(WithdrawalCheckError::InvalidState(
                    "fresh withdrawal-checker tables are not empty".to_string(),
                ));
            }
            let checkpoint = Checkpoint::new(self.zone, 0, genesis.hash());
            tx.put::<WithdrawalCheckerState>(CHECKPOINT_KEY, checkpoint)
                .map_err(|err| self.storage(err))?;
            provider_rw.commit().map_err(|err| self.storage(err))?;
            return Ok(checkpoint);
        }

        let checkpoint =
            checkpoint.ok_or_else(|| self.invalid_state("withdrawal checkpoint is missing"))?;
        let state_entries = tx
            .entries::<WithdrawalCheckerState>()
            .map_err(|err| self.storage(err))?;
        if state_entries != 1 {
            return Err(self.invalid_state(format!(
                "withdrawal checker state contains {state_entries} rows"
            )));
        }
        let checkpoint = checkpoint.into_validated()?;
        if checkpoint.zone != u64::from(self.zone) {
            return Err(self.invalid_state(format!(
                "stored checkpoint belongs to zone {}",
                checkpoint.zone
            )));
        }
        self.verify_token_backing(tx)?;
        provider_rw.commit().map_err(|err| self.storage(err))?;
        Ok(checkpoint)
    }

    fn process_notification<P>(
        &self,
        provider: &P,
        notification: &ExExNotification<TempoPrimitives>,
    ) -> Result<Checkpoint, WithdrawalCheckError>
    where
        P: DatabaseProviderFactory,
    {
        // Parse all newly-canonical receipts before opening the write transaction. Any malformed
        // event rejects the entire notification without changing ledger state.
        let committed = notification
            .committed_chain()
            .map(|chain| self.blocks_from_chain(&chain))
            .transpose()?;
        let reverted = notification
            .reverted_chain()
            .map(|chain| self.block_identities(&chain))
            .transpose()?;

        let provider_rw = provider
            .database_provider_rw()
            .map_err(|err| self.storage(err))?;
        let tx = provider_rw.tx_ref();
        let mut checkpoint = self.checkpoint_from_tx(tx)?;

        if let Some(reverted) = &reverted {
            for block in reverted.iter().rev() {
                if checkpoint.number != block.number || checkpoint.hash != block.hash {
                    return Err(self.invalid_state(format!(
                        "reverted block {} ({}) does not match checkpoint {} ({})",
                        block.number, block.hash, checkpoint.number, checkpoint.hash
                    )));
                }
                checkpoint = self.unwind_one(tx, checkpoint)?;
                if checkpoint.hash != block.parent_hash {
                    return Err(self.invalid_state(format!(
                        "undo record for block {} does not restore parent {}",
                        block.number, block.parent_hash
                    )));
                }
            }
        }

        if let Some(committed) = &committed {
            for block in committed {
                checkpoint = self.apply_block(tx, checkpoint, block)?;
            }
        }

        provider_rw.commit().map_err(|err| self.storage(err))?;
        Ok(checkpoint)
    }

    fn block_identities(
        &self,
        chain: &reth_provider::Chain<TempoPrimitives>,
    ) -> Result<Vec<CanonicalBlock>, WithdrawalCheckError> {
        if chain.is_empty() {
            return Err(WithdrawalCheckError::MalformedBlock {
                zone: self.zone,
                block: 0,
                detail: "ExEx notification contains an empty chain".to_string(),
            });
        }
        let blocks = chain
            .blocks_iter()
            .map(|block| CanonicalBlock {
                number: block.number(),
                hash: block.hash(),
                parent_hash: block.parent_hash(),
                events: Vec::new(),
            })
            .collect::<Vec<_>>();
        Ok(blocks)
    }

    fn blocks_from_chain(
        &self,
        chain: &reth_provider::Chain<TempoPrimitives>,
    ) -> Result<Vec<CanonicalBlock>, WithdrawalCheckError> {
        if chain.is_empty() {
            return Err(WithdrawalCheckError::MalformedBlock {
                zone: self.zone,
                block: 0,
                detail: "ExEx notification contains an empty chain".to_string(),
            });
        }
        if chain.blocks().len() != chain.execution_outcome().receipts().len() {
            return Err(WithdrawalCheckError::MalformedBlock {
                zone: self.zone,
                block: chain
                    .blocks()
                    .first_key_value()
                    .map_or(0, |(number, _)| *number),
                detail: format!(
                    "block/receipt-vector count mismatch: {} blocks, {} receipt vectors",
                    chain.blocks().len(),
                    chain.execution_outcome().receipts().len()
                ),
            });
        }

        let mut blocks = Vec::with_capacity(chain.len());
        for (block, receipts) in chain.blocks_and_receipts() {
            let number = block.number();
            let tx_hashes = block
                .body()
                .transaction_hashes_iter()
                .copied()
                .collect::<Vec<_>>();
            if tx_hashes.len() != receipts.len() {
                return Err(WithdrawalCheckError::MalformedBlock {
                    zone: self.zone,
                    block: number,
                    detail: format!(
                        "transaction/receipt count mismatch: {} transactions, {} receipts",
                        tx_hashes.len(),
                        receipts.len()
                    ),
                });
            }
            let transactions = tx_hashes
                .into_iter()
                .zip(receipts)
                .map(|(tx_hash, receipt)| (tx_hash, receipt.logs().to_vec()))
                .collect::<Vec<_>>();
            blocks.push(CanonicalBlock {
                number,
                hash: block.hash(),
                parent_hash: block.parent_hash(),
                events: parse_events(self.zone, number, &transactions)?,
            });
        }
        Ok(blocks)
    }

    /// Validate `target` once the durable checkpoint is on the current canonical branch.
    ///
    /// Reth can publish a replacement canonical block before the corresponding ExEx notification
    /// is handled. In that window the old checkpoint is merely behind, not corrupt, so callers
    /// must wait for reconciliation instead of permanently halting.
    fn portal_check_if_ready<P>(
        &self,
        provider: &P,
        target: BlockNumHash,
        baseline: Option<&PortalBackingCheck>,
    ) -> Result<PreparedCheck, WithdrawalCheckError>
    where
        P: DatabaseProviderFactory,
        P::Provider: BlockReader<Block = Block, Receipt = TempoReceipt>
            + BlockNumReader
            + HeaderProvider<Header = TempoHeader>
            + ReceiptProvider<Receipt = TempoReceipt>,
    {
        // One database snapshot binds the checkpoint, target, and reconstructed token totals.
        // The actor rechecks the target after the asynchronous L1 query before responding.
        let provider_ro = provider
            .database_provider_ro()
            .map_err(|error| self.storage(error))?;
        let checkpoint = self.checkpoint_from_tx(provider_ro.tx_ref())?;
        if checkpoint.number < target.number {
            return Ok(PreparedCheck::Pending(checkpoint));
        }

        let best = provider_ro
            .best_block_number()
            .map_err(|error| self.storage(error))?;
        let Some(canonical_checkpoint) = provider_ro
            .sealed_header(checkpoint.number)
            .map_err(|error| self.storage(error))?
        else {
            if checkpoint.number > best {
                return Ok(PreparedCheck::Pending(checkpoint));
            }
            return Err(self.invalid_state(format!(
                "canonical checkpoint header {} is missing below head {best}",
                checkpoint.number
            )));
        };
        if canonical_checkpoint.hash() != checkpoint.hash {
            return Ok(PreparedCheck::Pending(checkpoint));
        }

        let Some(canonical) = provider_ro
            .sealed_header(target.number)
            .map_err(|error| self.storage(error))?
        else {
            return Err(self.invalid_state(format!(
                "canonical target {} is missing despite checkpoint {}",
                target.number, checkpoint.number
            )));
        };
        if canonical.hash() != target.hash {
            return Ok(PreparedCheck::Retry(
                WithdrawalCheckRetry::CanonicalHashMismatch {
                    block: target.number,
                    canonical: canonical.hash(),
                    target: target.hash,
                },
            ));
        }

        let baseline = match baseline {
            Some(baseline) => provider_ro
                .sealed_header(baseline.target.number)
                .map_err(|error| self.storage(error))?
                .filter(|header| header.hash() == baseline.target.hash)
                .map(|_| baseline),
            None => None,
        };
        let (mut required, mut number) = match baseline {
            Some(baseline) => (baseline.required.clone(), baseline.target.number),
            None => (
                self.load_token_backing(provider_ro.tx_ref())?,
                checkpoint.number,
            ),
        };
        while number > target.number {
            let block = self.load_canonical_block(&provider_ro, number)?;
            self.unwind_token_backing(&mut required, &block)?;
            number -= 1;
        }
        while number < target.number {
            number += 1;
            let block = self.load_canonical_block(&provider_ro, number)?;
            self.apply_token_backing(&mut required, &block)?;
        }
        let l1_anchor = self.l1_anchor(&provider_ro, target.number)?;
        Ok(PreparedCheck::Portal(PortalBackingCheck {
            target,
            l1_anchor,
            required,
        }))
    }

    fn unwind_token_backing(
        &self,
        backing: &mut BTreeMap<Address, U256>,
        block: &CanonicalBlock,
    ) -> Result<(), WithdrawalCheckError> {
        for event in block.events.iter().rev() {
            let (token, previous) = match *event {
                ZoneBalanceChange::Credit { token, amount, .. } => {
                    let current = backing.get(&token).copied().unwrap_or_default();
                    let previous = current.checked_sub(amount).ok_or_else(|| {
                        self.invalid_state(format!(
                            "token backing underflow while reconstructing block {}",
                            block.number
                        ))
                    })?;
                    (token, previous)
                }
                ZoneBalanceChange::Debit {
                    token, requested, ..
                } => {
                    let current = backing.get(&token).copied().unwrap_or_default();
                    let previous = current.checked_add(requested).ok_or_else(|| {
                        self.invalid_state(format!(
                            "token backing overflow while reconstructing block {}",
                            block.number
                        ))
                    })?;
                    (token, previous)
                }
            };
            if previous.is_zero() {
                backing.remove(&token);
            } else {
                backing.insert(token, previous);
            }
        }
        Ok(())
    }

    fn apply_token_backing(
        &self,
        backing: &mut BTreeMap<Address, U256>,
        block: &CanonicalBlock,
    ) -> Result<(), WithdrawalCheckError> {
        for event in &block.events {
            let (token, current) = match *event {
                ZoneBalanceChange::Credit { token, amount, .. } => {
                    let previous = backing.get(&token).copied().unwrap_or_default();
                    let current = previous.checked_add(amount).ok_or_else(|| {
                        self.invalid_state(format!(
                            "token backing overflow while reconstructing block {}",
                            block.number
                        ))
                    })?;
                    (token, current)
                }
                ZoneBalanceChange::Debit {
                    token, requested, ..
                } => {
                    let previous = backing.get(&token).copied().unwrap_or_default();
                    let current = previous.checked_sub(requested).ok_or_else(|| {
                        self.invalid_state(format!(
                            "token backing underflow while reconstructing block {}",
                            block.number
                        ))
                    })?;
                    (token, current)
                }
            };
            if current.is_zero() {
                backing.remove(&token);
            } else {
                backing.insert(token, current);
            }
        }
        Ok(())
    }

    fn l1_anchor<P>(&self, provider: &P, block: u64) -> Result<BlockNumHash, WithdrawalCheckError>
    where
        P: ReceiptProvider<Receipt = TempoReceipt>,
    {
        let receipts = provider
            .receipts_by_block(BlockHashOrNumber::Number(block))
            .map_err(|error| self.storage(error))?
            .ok_or_else(|| {
                self.invalid_state(format!(
                    "canonical receipts for collateral checkpoint {block} are missing"
                ))
            })?;
        let mut anchor = None;
        for receipt in receipts {
            for log in receipt.logs() {
                if log.address != ZONE_INBOX_ADDRESS
                    || log.topics().first() != Some(&ZoneInbox::TempoAdvanced::SIGNATURE_HASH)
                {
                    continue;
                }
                let event =
                    ZoneInbox::TempoAdvanced::decode_log_validate(log).map_err(|error| {
                        WithdrawalCheckError::MalformedBlock {
                            zone: self.zone,
                            block,
                            detail: format!("invalid TempoAdvanced event: {error}"),
                        }
                    })?;
                if anchor
                    .replace(BlockNumHash {
                        number: event.tempoBlockNumber,
                        hash: event.tempoBlockHash,
                    })
                    .is_some()
                {
                    return Err(WithdrawalCheckError::MalformedBlock {
                        zone: self.zone,
                        block,
                        detail: "multiple TempoAdvanced events".to_string(),
                    });
                }
            }
        }
        anchor.ok_or_else(|| WithdrawalCheckError::MalformedBlock {
            zone: self.zone,
            block,
            detail: "missing TempoAdvanced event".to_string(),
        })
    }

    fn checkpoint_from_tx<T: DbTx>(&self, tx: &T) -> Result<Checkpoint, WithdrawalCheckError> {
        let checkpoint = tx
            .get::<WithdrawalCheckerState>(CHECKPOINT_KEY)
            .map_err(|err| self.storage(err))?
            .ok_or_else(|| self.invalid_state("withdrawal checkpoint is missing"))?
            .into_validated()?;
        if checkpoint.zone != u64::from(self.zone) {
            return Err(self.invalid_state(format!(
                "stored checkpoint belongs to zone {}",
                checkpoint.zone
            )));
        }
        Ok(checkpoint)
    }

    fn load_canonical_block<P>(
        &self,
        provider: &P,
        number: u64,
    ) -> Result<CanonicalBlock, WithdrawalCheckError>
    where
        P: BlockReader<Block = Block, Receipt = TempoReceipt>
            + HeaderProvider<Header = TempoHeader>,
    {
        let header = provider
            .sealed_header(number)
            .map_err(|err| self.storage(err))?
            .ok_or_else(|| self.invalid_state(format!("canonical header {number} is missing")))?;
        let block = provider
            .block_by_number(number)
            .map_err(|err| self.storage(err))?
            .ok_or_else(|| {
                self.invalid_state(format!("canonical block body {number} is missing"))
            })?;
        if block.header.hash_slow() != header.hash() {
            return Err(WithdrawalCheckError::MalformedBlock {
                zone: self.zone,
                block: number,
                detail: "canonical header and block body hashes differ".to_string(),
            });
        }
        let receipts = provider
            .receipts_by_block(BlockHashOrNumber::Number(number))
            .map_err(|err| self.storage(err))?
            .ok_or(WithdrawalCheckError::MalformedBlock {
                zone: self.zone,
                block: number,
                detail: "canonical receipts are missing".to_string(),
            })?;
        let tx_hashes = block
            .body
            .transaction_hashes_iter()
            .copied()
            .collect::<Vec<_>>();
        if tx_hashes.len() != receipts.len() {
            return Err(WithdrawalCheckError::MalformedBlock {
                zone: self.zone,
                block: number,
                detail: format!(
                    "transaction/receipt count mismatch: {} transactions, {} receipts",
                    tx_hashes.len(),
                    receipts.len()
                ),
            });
        }

        let transactions = tx_hashes
            .into_iter()
            .zip(receipts)
            .map(|(tx_hash, receipt)| (tx_hash, receipt.logs))
            .collect::<Vec<_>>();
        Ok(CanonicalBlock {
            number,
            hash: header.hash(),
            parent_hash: header.parent_hash(),
            events: parse_events(self.zone, number, &transactions)?,
        })
    }

    fn apply_block<T: DbTx + DbTxMut>(
        &self,
        tx: &T,
        checkpoint: Checkpoint,
        block: &CanonicalBlock,
    ) -> Result<Checkpoint, WithdrawalCheckError> {
        let expected_number = checkpoint.number.checked_add(1).ok_or_else(|| {
            WithdrawalCheckError::InvalidState(
                "block number overflow while extending checkpoint".to_string(),
            )
        })?;
        if block.number != expected_number || block.parent_hash != checkpoint.hash {
            return Err(self.invalid_state(format!(
                "block {} ({}) does not extend checkpoint {} ({})",
                block.number, block.hash, checkpoint.number, checkpoint.hash
            )));
        }
        if tx
            .get::<WithdrawalBlockDeltas>(block.number)
            .map_err(|err| self.storage(err))?
            .is_some()
        {
            return Err(self.invalid_state(format!(
                "undo record for block {} already exists",
                block.number
            )));
        }

        let mut previous = BTreeMap::<BalanceKey, Option<U256>>::new();
        for event in &block.events {
            let (key, current, next) = match *event {
                ZoneBalanceChange::Credit {
                    user,
                    token,
                    amount,
                    ..
                } => {
                    let key = BalanceKey::new(user, token);
                    let current = self.balance_from_tx(tx, key)?;
                    let next =
                        current
                            .unwrap_or_default()
                            .checked_add(amount)
                            .ok_or_else(|| {
                                WithdrawalCheckError::InvalidState(format!(
                                    "balance overflow in block {}, tx {}",
                                    block.number,
                                    event.tx_hash()
                                ))
                            })?;
                    (key, current, next)
                }
                ZoneBalanceChange::Debit {
                    tx_hash,
                    user,
                    token,
                    requested,
                } => {
                    let key = BalanceKey::new(user, token);
                    let current = self.balance_from_tx(tx, key)?;
                    let available = current.unwrap_or_default();
                    let Some(next) = available.checked_sub(requested) else {
                        return Err(WithdrawalCheckError::UnbackedWithdrawal(Box::new(
                            UnbackedWithdrawalError {
                                zone: self.zone,
                                block: block.number,
                                tx_hash,
                                user,
                                token,
                                requested,
                                available,
                            },
                        )));
                    };
                    (key, current, next)
                }
            };
            previous.entry(key).or_insert(current);
            let total = self
                .token_backing_from_tx(tx, key.token)?
                .unwrap_or_default();
            let next_total = total
                .checked_sub(current.unwrap_or_default())
                .and_then(|without_user| without_user.checked_add(next))
                .ok_or_else(|| {
                    self.invalid_state(format!(
                        "token backing arithmetic failed in block {}, tx {}",
                        block.number,
                        event.tx_hash()
                    ))
                })?;
            self.write_balance(tx, key, next)?;
            self.write_token_backing(tx, key.token, next_total)?;
        }

        let delta = BlockDelta::new(
            block.hash,
            block.parent_hash,
            previous
                .into_iter()
                .map(|(key, value)| PreviousBacking::new(key, value))
                .collect(),
        )?;
        tx.put::<WithdrawalBlockDeltas>(block.number, delta)
            .map_err(|err| self.storage(err))?;
        if let Some(prunable) = block.number.checked_sub(BLOCK_DELTA_RETENTION) {
            tx.delete::<WithdrawalBlockDeltas>(prunable, None)
                .map_err(|err| self.storage(err))?;
        }
        let checkpoint = Checkpoint::new(self.zone, block.number, block.hash);
        tx.put::<WithdrawalCheckerState>(CHECKPOINT_KEY, checkpoint)
            .map_err(|err| self.storage(err))?;
        Ok(checkpoint)
    }

    fn unwind_one<T: DbTx + DbTxMut>(
        &self,
        tx: &T,
        checkpoint: Checkpoint,
    ) -> Result<Checkpoint, WithdrawalCheckError> {
        if checkpoint.number == 0 {
            return Err(self.invalid_state("attempted to unwind genesis"));
        }
        let value = tx
            .get::<WithdrawalBlockDeltas>(checkpoint.number)
            .map_err(|err| self.storage(err))?
            .ok_or_else(|| {
                self.invalid_state(format!(
                    "undo record for block {} is missing",
                    checkpoint.number
                ))
            })?
            .into_validated()?;
        let delta = value;
        if delta.hash != checkpoint.hash {
            return Err(self.invalid_state(format!(
                "undo record for block {} does not match checkpoint",
                checkpoint.number
            )));
        }
        for previous in delta.previous {
            let key = previous.key();
            let current = self.balance_from_tx(tx, key)?.unwrap_or_default();
            let total = self
                .token_backing_from_tx(tx, key.token)?
                .unwrap_or_default();
            let restored_total = total
                .checked_sub(current)
                .and_then(|without_user| {
                    without_user.checked_add(previous.value.unwrap_or_default())
                })
                .ok_or_else(|| {
                    self.invalid_state(format!(
                        "token backing arithmetic failed while unwinding block {}",
                        checkpoint.number
                    ))
                })?;
            match previous.value {
                Some(balance) => self.write_balance(tx, key, balance)?,
                None => {
                    tx.delete::<WithdrawalBacking>(key, None)
                        .map_err(|err| self.storage(err))?;
                }
            }
            self.write_token_backing(tx, key.token, restored_total)?;
        }
        tx.delete::<WithdrawalBlockDeltas>(checkpoint.number, None)
            .map_err(|err| self.storage(err))?;
        let checkpoint = Checkpoint::new(self.zone, checkpoint.number - 1, delta.parent_hash);
        tx.put::<WithdrawalCheckerState>(CHECKPOINT_KEY, checkpoint)
            .map_err(|err| self.storage(err))?;
        Ok(checkpoint)
    }

    fn balance_from_tx<T: DbTx>(
        &self,
        tx: &T,
        key: BalanceKey,
    ) -> Result<Option<U256>, WithdrawalCheckError> {
        Ok(tx
            .get::<WithdrawalBacking>(key)
            .map_err(|err| self.storage(err))?
            .map(|balance| balance.0))
    }

    fn write_balance<T: DbTxMut>(
        &self,
        tx: &T,
        key: BalanceKey,
        balance: U256,
    ) -> Result<(), WithdrawalCheckError> {
        if balance.is_zero() {
            tx.delete::<WithdrawalBacking>(key, None)
                .map_err(|err| self.storage(err))?;
        } else {
            tx.put::<WithdrawalBacking>(key, CompactU256::from(balance))
                .map_err(|err| self.storage(err))?;
        }
        Ok(())
    }

    fn token_backing_from_tx<T: DbTx>(
        &self,
        tx: &T,
        token: Address,
    ) -> Result<Option<U256>, WithdrawalCheckError> {
        Ok(tx
            .get::<WithdrawalTokenBacking>(token)
            .map_err(|error| self.storage(error))?
            .map(|backing| backing.0))
    }

    fn write_token_backing<T: DbTxMut>(
        &self,
        tx: &T,
        token: Address,
        backing: U256,
    ) -> Result<(), WithdrawalCheckError> {
        if backing.is_zero() {
            tx.delete::<WithdrawalTokenBacking>(token, None)
                .map_err(|error| self.storage(error))?;
        } else {
            tx.put::<WithdrawalTokenBacking>(token, CompactU256::from(backing))
                .map_err(|error| self.storage(error))?;
        }
        Ok(())
    }

    /// Verify that the persisted token totals equal the sum of all user balances.
    fn verify_token_backing<T: DbTx>(&self, tx: &T) -> Result<(), WithdrawalCheckError> {
        let mut calculated = BTreeMap::<Address, U256>::new();
        let mut balances = tx
            .cursor_read::<WithdrawalBacking>()
            .map_err(|error| self.storage(error))?;
        for entry in balances.walk(None).map_err(|error| self.storage(error))? {
            let (key, balance) = entry.map_err(|error| self.storage(error))?;
            if balance.0.is_zero() {
                return Err(self.invalid_state("withdrawal backing contains a zero balance"));
            }
            let total = calculated.get(&key.token).copied().unwrap_or_default();
            let total = total.checked_add(balance.0).ok_or_else(|| {
                self.invalid_state(format!(
                    "user backing sum overflows for token {}",
                    key.token
                ))
            })?;
            calculated.insert(key.token, total);
        }
        drop(balances);

        let stored = self.load_token_backing(tx)?;
        if calculated != stored {
            return Err(
                self.invalid_state("per-token backing does not equal the sum of user balances")
            );
        }
        Ok(())
    }

    fn load_token_backing<T: DbTx>(
        &self,
        tx: &T,
    ) -> Result<BTreeMap<Address, U256>, WithdrawalCheckError> {
        let mut stored = BTreeMap::<Address, U256>::new();
        let mut totals = tx
            .cursor_read::<WithdrawalTokenBacking>()
            .map_err(|error| self.storage(error))?;
        for entry in totals.walk(None).map_err(|error| self.storage(error))? {
            let (token, backing) = entry.map_err(|error| self.storage(error))?;
            if backing.0.is_zero() {
                return Err(self.invalid_state("token backing contains a zero balance"));
            }
            stored.insert(token, backing.0);
        }
        Ok(stored)
    }

    fn storage(&self, error: impl std::fmt::Display) -> WithdrawalCheckError {
        self.invalid_state(format!("storage error: {error}"))
    }

    fn invalid_state(&self, detail: impl std::fmt::Display) -> WithdrawalCheckError {
        WithdrawalCheckError::InvalidState(format!("zone={} {detail}", self.zone))
    }
}

async fn halt_requests(
    mut requests: mpsc::Receiver<WithdrawalCheckRequest>,
    pending: Vec<WithdrawalCheckRequest>,
    error: WithdrawalCheckError,
) -> eyre::Result<()> {
    error.emit_failure();
    let error = Arc::new(error);
    for request in pending {
        request.respond(WithdrawalCheckDecision::Halt(error.clone()));
    }
    while let Some(request) = requests.recv().await {
        request.respond(WithdrawalCheckDecision::Halt(error.clone()));
    }
    futures::future::pending().await
}

/// Initialize and run the node-wide withdrawal checker ExEx.
///
/// Reth's public ExEx API does not expose its block-import write transaction. Consequently, each
/// notification is applied atomically to the four custom tables in a separate MDBX transaction.
/// The decision actor closes that gap by publishing a validated checkpoint only after this custom
/// transaction commits and withholding leader/follower attestation signatures until it observes
/// that checkpoint.
pub(crate) async fn launch_withdrawal_checker_exex<N>(
    mut ctx: ExExContext<N>,
    zone: u32,
    tables_are_new: bool,
    mut requests: mpsc::Receiver<WithdrawalCheckRequest>,
    l1_rpc_url: String,
    portal_address: Address,
    retry_connection_interval: Duration,
) -> eyre::Result<impl Future<Output = eyre::Result<()>> + Send>
where
    N: FullNodeComponents<Types = ZoneNode>,
    N::Provider:
        HeaderProvider<Header = TempoHeader> + DatabaseProviderFactory + Clone + Unpin + 'static,
    <N::Provider as DatabaseProviderFactory>::Provider: BlockReader<Block = Block, Receipt = TempoReceipt>
        + HeaderProvider<Header = TempoHeader>
        + ReceiptProvider<Receipt = TempoReceipt>,
{
    let l1_provider = alloy_provider::ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect_with_config(
            &l1_rpc_url,
            crate::rpc::rpc_connection_config(retry_connection_interval),
        )
        .await?
        .erased();
    let portal = PortalBackingChecker {
        zone,
        portal: portal_address,
        provider: l1_provider,
    };
    let ledger = WithdrawalLedger::new(zone);
    let checkpoint = ledger
        .initialize(ctx.provider(), tables_are_new)
        .inspect_err(WithdrawalCheckError::emit_failure)?;
    let finished = BlockNumHash {
        number: checkpoint.number,
        hash: checkpoint.hash,
    };
    ctx.set_notifications_with_head(ExExHead::new(finished));
    let mut backfill_thresholds = ctx.reth_config.stages.execution;
    backfill_thresholds.max_blocks = Some(EXEX_BACKFILL_MAX_BLOCKS);
    ctx.notifications
        .set_backfill_thresholds(backfill_thresholds.into());
    if ctx.send_finished_height(finished).is_err() {
        let error = WithdrawalCheckError::Unavailable {
            block: checkpoint.number,
            detail: "ExEx event channel closed",
        };
        error.emit_failure();
        return Err(eyre::Report::new(error));
    }

    Ok(async move {
        let mut checkpoint = checkpoint;
        let mut portal_checks = PortalChecks::new(portal);
        let mut requests_open = true;
        let mut cleanup = tokio::time::interval(Duration::from_secs(1));
        cleanup.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                request = requests.recv(), if requests_open && portal_checks.len() < DECISION_CHANNEL_CAPACITY => {
                    let Some(request) = request else {
                        requests_open = false;
                        continue;
                    };
                    match portal_checks.queue(
                        &ledger,
                        ctx.provider(),
                        request,
                    ) {
                        Ok(()) => {}
                        Err(error) => {
                            let requests_to_halt = portal_checks.take_requests();
                            return halt_requests(requests, requests_to_halt, error).await;
                        }
                    }
                }
                result = portal_checks.join_next(), if portal_checks.has_running() => {
                    let result = match result {
                        Some(Ok(result)) => result,
                        Some(Err(error)) => {
                            let requests_to_halt = portal_checks.take_requests();
                            return halt_requests(
                                requests,
                                requests_to_halt,
                                ledger.invalid_state(format!("Portal-check task failed: {error}")),
                            ).await;
                        }
                        None => continue,
                    };
                    let PortalCheckResult { key, outcome } = result;
                    if !portal_checks.contains(key) {
                        portal_checks.start_available(&ledger, ctx.provider());
                        continue;
                    }
                    let canonical = match ctx.provider().sealed_header(key.0) {
                        Ok(canonical) => canonical,
                        Err(error) => {
                            let requests_to_halt = portal_checks.take_requests();
                            return halt_requests(requests, requests_to_halt, ledger.storage(error)).await;
                        }
                    };
                    let Some(canonical) = canonical else {
                        portal_checks.park(key);
                        portal_checks.start_available(&ledger, ctx.provider());
                        continue;
                    };
                    if canonical.hash() != key.1 {
                        let decision = WithdrawalCheckDecision::Retry(
                            WithdrawalCheckRetry::CanonicalHashMismatch {
                                block: key.0,
                                canonical: canonical.hash(),
                                target: key.1,
                            },
                        );
                        portal_checks.respond(key, decision);
                        portal_checks.start_available(&ledger, ctx.provider());
                        continue;
                    }
                    match outcome {
                        PortalCheckOutcome::Pending(observed) => {
                            if checkpoint == observed {
                                portal_checks.park(key);
                            } else {
                                portal_checks.retry(key);
                            }
                        }
                        PortalCheckOutcome::Retry(error) => {
                            let decision = WithdrawalCheckDecision::Retry(error);
                            portal_checks.respond(key, decision);
                        }
                        PortalCheckOutcome::Decision { check, decision } => {
                            portal_checks.complete(check, decision);
                        }
                        PortalCheckOutcome::Fatal(error) => {
                            let requests_to_halt = portal_checks.take_requests();
                            return halt_requests(requests, requests_to_halt, *error).await;
                        }
                    }
                    portal_checks.start_available(&ledger, ctx.provider());
                }
                notification = ctx.notifications.next() => {
                    let result = match notification {
                        Some(Ok(notification)) => ledger.process_notification(ctx.provider(), &notification),
                        Some(Err(notification_error)) => {
                            tracing::error!(
                                target: "zone::withdrawal_checker",
                                %notification_error,
                                "Withdrawal checker ExEx notification failed"
                            );
                            Err(WithdrawalCheckError::Unavailable {
                                block: checkpoint.number,
                                detail: "ExEx notification failed",
                            })
                        }
                        None => Err(WithdrawalCheckError::Unavailable {
                            block: checkpoint.number,
                            detail: "ExEx notification stream closed",
                        }),
                    };

                    match result {
                        Ok(validated) => {
                            checkpoint = validated;
                            if ctx
                                .send_finished_height(BlockNumHash {
                                    number: checkpoint.number,
                                    hash: checkpoint.hash,
                                })
                                .is_err()
                            {
                                let error = WithdrawalCheckError::Unavailable {
                                    block: checkpoint.number,
                                    detail: "ExEx event channel closed",
                                };
                                let requests_to_halt = portal_checks.take_requests();
                                return halt_requests(requests, requests_to_halt, error).await;
                            }
                            portal_checks.wake_waiting();
                            portal_checks.start_available(&ledger, ctx.provider());
                        }
                        Err(error) => {
                            let requests_to_halt = portal_checks.take_requests();
                            return halt_requests(requests, requests_to_halt, error).await;
                        }
                    }
                }
                _ = cleanup.tick(), if portal_checks.len() != 0 => {
                    portal_checks.retain_open();
                }
            }
        }
    })
}

#[derive(Debug, thiserror::Error)]
#[error(
    "withdrawal balance check failed: zone={zone} block={block} tx_hash={tx_hash} user={user} token={token} requested_amount={requested} available_amount={available}"
)]
pub(crate) struct UnbackedWithdrawalError {
    zone: u32,
    block: u64,
    tx_hash: B256,
    user: Address,
    token: Address,
    requested: U256,
    available: U256,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "withdrawal backing exceeds L1 Portal custody: zone={zone} block={block} l1_block={l1_block} l1_hash={l1_hash} token={token} required_backing={required} portal_balance={available}"
)]
pub(crate) struct PortalBackingShortfallError {
    zone: u32,
    block: u64,
    l1_block: u64,
    l1_hash: B256,
    token: Address,
    required: U256,
    available: U256,
}

/// A temporary condition that must withhold this attempt without poisoning the checker.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum WithdrawalCheckRetry {
    #[error("withdrawal checker decision timed out at block {block}")]
    Timeout { block: u64 },
    #[error("canonical hash {canonical} does not match target {target} at block {block}")]
    CanonicalHashMismatch {
        block: u64,
        canonical: B256,
        target: B256,
    },
    #[error("L1 Portal balance is unavailable at zone block {block}: {detail}")]
    PortalBalanceUnavailable { block: u64, detail: String },
}

impl WithdrawalCheckRetry {
    pub(crate) const fn reason(&self) -> &'static str {
        match self {
            Self::Timeout { .. } => "checker_timeout",
            Self::CanonicalHashMismatch { .. } => "canonical_state_changed",
            Self::PortalBalanceUnavailable { .. } => "l1_portal_unavailable",
        }
    }

    pub(crate) fn emit_withheld(&self) {
        metrics::counter!(
            "withdrawal_signature_withheld_total",
            "reason" => self.reason()
        )
        .increment(1);
        tracing::warn!(
            target: "zone::withdrawal_checker",
            error = %self,
            reason = self.reason(),
            action = "retry",
            "Withdrawal checker temporarily withheld the target"
        );
    }
}

/// A fatal checker failure. Once observed, the actor halts and keeps rejecting requests.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WithdrawalCheckError {
    #[error(transparent)]
    UnbackedWithdrawal(Box<UnbackedWithdrawalError>),
    #[error(transparent)]
    PortalBackingShortfall(Box<PortalBackingShortfallError>),
    #[error(
        "malformed withdrawal-checker event: zone={zone} block={block} tx_hash={tx_hash} event={event} detail={detail}"
    )]
    MalformedEvent {
        zone: u32,
        block: u64,
        tx_hash: B256,
        event: &'static str,
        detail: String,
    },
    #[error("malformed canonical block: zone={zone} block={block} detail={detail}")]
    MalformedBlock {
        zone: u32,
        block: u64,
        detail: String,
    },
    #[error("invalid withdrawal-checker state: {0}")]
    InvalidState(String),
    #[error("withdrawal checker unavailable at block {block}: {detail}")]
    Unavailable { block: u64, detail: &'static str },
}

impl WithdrawalCheckError {
    pub(crate) const fn reason(&self) -> &'static str {
        match self {
            Self::UnbackedWithdrawal(_) => "unbacked_withdrawal",
            Self::PortalBackingShortfall(_) => "portal_backing_shortfall",
            Self::MalformedEvent { .. } | Self::MalformedBlock { .. } => "malformed_event",
            Self::InvalidState(_) => "invalid_state",
            Self::Unavailable { .. } => "checker_unavailable",
        }
    }

    pub(crate) fn emit_withheld(&self) {
        metrics::counter!(
            "withdrawal_signature_withheld_total",
            "reason" => self.reason()
        )
        .increment(1);
        self.log("withheld");
    }

    fn emit_failure(&self) {
        metrics::counter!(
            "withdrawal_check_error_total",
            "reason" => self.reason()
        )
        .increment(1);
        self.log("failed");
    }

    fn log(&self, action: &'static str) {
        match self {
            Self::UnbackedWithdrawal(error) => tracing::error!(
                target: "zone::withdrawal_checker",
                zone = error.zone,
                block = error.block,
                tx_hash = %error.tx_hash,
                user = %error.user,
                token = %error.token,
                requested_amount = %error.requested,
                available_amount = %error.available,
                reason = self.reason(),
                action,
                "Withdrawal checker did not authorize the target"
            ),
            Self::PortalBackingShortfall(error) => tracing::error!(
                target: "zone::withdrawal_checker",
                zone = error.zone,
                block = error.block,
                l1_block = error.l1_block,
                l1_hash = %error.l1_hash,
                token = %error.token,
                required_backing = %error.required,
                portal_balance = %error.available,
                reason = self.reason(),
                action,
                "Withdrawal checker did not authorize the target"
            ),
            _ => tracing::error!(
                target: "zone::withdrawal_checker",
                error = %self,
                reason = self.reason(),
                action,
                "Withdrawal checker did not authorize the target"
            ),
        }
    }
}

impl From<DatabaseError> for WithdrawalCheckError {
    fn from(error: DatabaseError) -> Self {
        Self::InvalidState(format!("storage error: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Bytes, Log, LogData};
    use alloy_sol_types::SolEvent;
    use reth_db::{
        DatabaseEnv,
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        table::{Compress, Decode, Decompress, Encode},
        test_utils::{TempDatabase, tempdir_path},
    };
    use std::sync::Arc;
    use tempo_zone_contracts::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS, ZoneInbox, ZoneOutbox};

    const ZONE: u32 = 7;

    fn address(byte: u8) -> Address {
        Address::repeat_byte(byte)
    }

    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }

    fn event_log<E: SolEvent>(address: Address, event: E) -> Log {
        Log {
            address,
            data: event.encode_log_data(),
        }
    }

    fn deposit(user: Address, token: Address, amount: u128) -> Log {
        event_log(
            ZONE_INBOX_ADDRESS,
            ZoneInbox::DepositProcessed {
                depositHash: hash(0x10),
                sender: address(0x11),
                to: user,
                token,
                amount,
                memo: B256::ZERO,
            },
        )
    }

    fn encrypted_deposit(user: Address, token: Address, amount: u128) -> Log {
        event_log(
            ZONE_INBOX_ADDRESS,
            ZoneInbox::EncryptedDepositProcessed {
                depositHash: hash(0x12),
                sender: address(0x13),
                to: user,
                token,
                amount,
                memo: B256::ZERO,
            },
        )
    }

    fn bounceback(user: Address, token: Address, amount: u128) -> Log {
        event_log(
            ZONE_INBOX_ADDRESS,
            ZoneInbox::WithdrawalBounceBackProcessed {
                fallbackRecipient: user,
                token,
                amount,
            },
        )
    }

    fn refund_claim(user: Address, token: Address, amount: u128) -> Log {
        event_log(
            ZONE_INBOX_ADDRESS,
            ZoneInbox::RefundClaimed {
                recipient: user,
                token,
                amount,
            },
        )
    }

    fn withdrawal(
        user: Address,
        token: Address,
        amount: u128,
        fee: u128,
        fallback_nonce: u64,
    ) -> Log {
        event_log(
            ZONE_OUTBOX_ADDRESS,
            ZoneOutbox::WithdrawalRequested {
                withdrawalIndex: 1,
                sender: user,
                token,
                to: address(0x20),
                amount,
                fee,
                memo: B256::ZERO,
                gasLimit: 100_000,
                fallbackNonce: fallback_nonce,
                data: Bytes::new(),
                revealTo: Bytes::new(),
            },
        )
    }

    fn test_database() -> Arc<TempDatabase<DatabaseEnv>> {
        let path = tempdir_path();
        let database = init_db(&path, DatabaseArguments::test()).unwrap();
        assert!(register_tables(database).unwrap());
        let database = init_db(&path, DatabaseArguments::test()).unwrap();
        Arc::new(TempDatabase::new(database, path))
    }

    fn seed_genesis(database: &TempDatabase<DatabaseEnv>, hash: B256) {
        let tx = database.tx_mut().unwrap();
        tx.put::<WithdrawalCheckerState>(CHECKPOINT_KEY, Checkpoint::new(ZONE, 0, hash))
            .unwrap();
        tx.commit().unwrap();
    }

    fn parsed_block(
        ledger: &WithdrawalLedger,
        number: u64,
        block_hash: B256,
        parent_hash: B256,
        transactions: Vec<(B256, Vec<Log>)>,
    ) -> Result<CanonicalBlock, WithdrawalCheckError> {
        Ok(CanonicalBlock {
            number,
            hash: block_hash,
            parent_hash,
            events: parse_events(ledger.zone, number, &transactions)?,
        })
    }

    fn apply(
        database: &TempDatabase<DatabaseEnv>,
        ledger: &WithdrawalLedger,
        block: &CanonicalBlock,
    ) -> Result<Checkpoint, WithdrawalCheckError> {
        let tx = database.tx_mut().unwrap();
        let checkpoint = ledger.checkpoint_from_tx(&tx)?;
        let checkpoint = ledger.apply_block(&tx, checkpoint, block)?;
        tx.commit().unwrap();
        Ok(checkpoint)
    }

    fn balance(
        database: &TempDatabase<DatabaseEnv>,
        ledger: &WithdrawalLedger,
        user: Address,
        token: Address,
    ) -> Option<U256> {
        let tx = database.tx().unwrap();
        ledger
            .balance_from_tx(&tx, BalanceKey::new(user, token))
            .unwrap()
    }

    fn token_backing(
        database: &TempDatabase<DatabaseEnv>,
        ledger: &WithdrawalLedger,
        token: Address,
    ) -> Option<U256> {
        let tx = database.tx().unwrap();
        ledger.token_backing_from_tx(&tx, token).unwrap()
    }

    #[tokio::test]
    async fn decision_channel_returns_only_the_actor_response() {
        let (checker, mut requests) = withdrawal_checker_channel();
        let target = BlockNumHash {
            number: 9,
            hash: hash(9),
        };
        let decision = tokio::spawn(async move { checker.decide(target).await });

        let request = requests.recv().await.unwrap();
        assert_eq!(request.target, target);
        request.respond(WithdrawalCheckDecision::Submit);

        assert!(matches!(
            decision.await.unwrap(),
            WithdrawalCheckDecision::Submit
        ));
    }

    #[tokio::test]
    async fn unavailable_decision_actor_fails_closed() {
        let decision = WithdrawalChecker::unavailable()
            .decide(BlockNumHash {
                number: 9,
                hash: hash(9),
            })
            .await;

        assert!(matches!(
            decision,
            WithdrawalCheckDecision::Halt(error)
                if matches!(*error, WithdrawalCheckError::Unavailable { block: 9, .. })
        ));
    }

    #[tokio::test]
    async fn retry_does_not_poison_later_decisions() {
        let (checker, mut requests) = withdrawal_checker_channel();
        let target = BlockNumHash {
            number: 9,
            hash: hash(9),
        };

        let first = tokio::spawn({
            let checker = checker.clone();
            async move { checker.decide(target).await }
        });
        requests
            .recv()
            .await
            .unwrap()
            .respond(WithdrawalCheckDecision::Retry(
                WithdrawalCheckRetry::CanonicalHashMismatch {
                    block: 9,
                    canonical: hash(8),
                    target: hash(9),
                },
            ));
        assert!(matches!(
            first.await.unwrap(),
            WithdrawalCheckDecision::Retry(error)
                if error.reason() == "canonical_state_changed"
        ));

        let second = tokio::spawn(async move { checker.decide(target).await });
        requests
            .recv()
            .await
            .unwrap()
            .respond(WithdrawalCheckDecision::Submit);
        assert!(matches!(
            second.await.unwrap(),
            WithdrawalCheckDecision::Submit
        ));
    }

    #[test]
    fn table_codecs_roundtrip() {
        let key = BalanceKey::new(address(1), address(2));
        let encoded = key.encode();
        assert_eq!(&encoded[..20], address(1).as_slice());
        assert_eq!(&encoded[20..], address(2).as_slice());
        assert_eq!(BalanceKey::decode(&encoded).unwrap(), key);

        assert_eq!(Checkpoint::bitflag_encoded_bytes(), 2);
        assert_eq!(Checkpoint::bitflag_unused_bits(), 4);
        assert_eq!(PreviousBacking::bitflag_encoded_bytes(), 1);
        assert_eq!(PreviousBacking::bitflag_unused_bits(), 7);
        assert_eq!(BlockDelta::bitflag_encoded_bytes(), 1);
        assert_eq!(BlockDelta::bitflag_unused_bits(), 4);

        let checkpoint = Checkpoint::new(ZONE, 9, hash(9));
        let encoded = checkpoint.compress();
        assert_eq!(Checkpoint::decompress(&encoded).unwrap(), checkpoint);

        let delta = BlockDelta::new(
            hash(9),
            hash(8),
            vec![PreviousBacking::new(
                BalanceKey::new(address(1), address(2)),
                Some(U256::from(3)),
            )],
        )
        .unwrap();
        let encoded = delta.clone().compress();
        assert_eq!(BlockDelta::decompress(&encoded).unwrap(), delta);
    }

    #[test]
    fn portal_backing_accepts_covered_totals_and_rejects_shortfalls() {
        struct Case {
            required: u64,
            available: u64,
            accepted: bool,
        }

        let anchor = BlockNumHash {
            number: 42,
            hash: hash(0x42),
        };
        let token = address(2);
        for case in [
            Case {
                required: 100,
                available: 100,
                accepted: true,
            },
            Case {
                required: 101,
                available: 100,
                accepted: false,
            },
        ] {
            let result = PortalBackingChecker::ensure_backed(
                ZONE,
                9,
                anchor,
                token,
                U256::from(case.required),
                U256::from(case.available),
            );
            match (result, case.accepted) {
                (Ok(()), true) => {}
                (Err(WithdrawalCheckError::PortalBackingShortfall(error)), false) => {
                    assert_eq!(error.zone, ZONE);
                    assert_eq!(error.block, 9);
                    assert_eq!(error.l1_block, anchor.number);
                    assert_eq!(error.l1_hash, anchor.hash);
                    assert_eq!(error.token, token);
                    assert_eq!(error.required, U256::from(case.required));
                    assert_eq!(error.available, U256::from(case.available));
                }
                (result, _) => panic!("unexpected Portal backing result: {result:?}"),
            }
        }
    }

    #[test]
    fn portal_backing_is_reconstructed_at_the_exact_target() {
        let ledger = WithdrawalLedger::new(ZONE);
        let token = address(2);
        let expected = BTreeMap::from([(token, U256::from(100))]);
        let mut backing = expected.clone();
        let later_block = CanonicalBlock {
            number: 2,
            hash: hash(2),
            parent_hash: hash(1),
            events: vec![
                ZoneBalanceChange::Credit {
                    tx_hash: hash(0x21),
                    user: address(1),
                    token,
                    amount: U256::from(20),
                },
                ZoneBalanceChange::Debit {
                    tx_hash: hash(0x22),
                    user: address(1),
                    token,
                    requested: U256::from(50),
                },
            ],
        };

        ledger
            .apply_token_backing(&mut backing, &later_block)
            .unwrap();
        assert_eq!(backing, BTreeMap::from([(token, U256::from(70))]));

        ledger
            .unwind_token_backing(&mut backing, &later_block)
            .unwrap();
        assert_eq!(backing, expected);
    }

    #[test]
    fn prepared_portal_checks_are_reused_for_adjacent_targets() {
        let mut cache = PortalCheckCache::default();
        let prepared = Arc::new(PortalBackingCheck {
            target: BlockNumHash {
                number: 9,
                hash: hash(9),
            },
            l1_anchor: BlockNumHash {
                number: 90,
                hash: hash(90),
            },
            required: BTreeMap::from([(address(2), U256::from(100))]),
        });
        cache.insert(prepared.clone(), None);

        assert!(matches!(
            cache.get(prepared.target),
            Some(CachedPortalCheckHit::Prepared(check)) if Arc::ptr_eq(&check, &prepared)
        ));
        assert!(matches!(
            cache.closest(BlockNumHash { number: 10, hash: hash(10) }),
            Some(check) if Arc::ptr_eq(&check, &prepared)
        ));

        cache.insert(prepared.clone(), Some(WithdrawalCheckDecision::Submit));
        assert!(matches!(
            cache.get(prepared.target),
            Some(CachedPortalCheckHit::Decision(
                WithdrawalCheckDecision::Submit
            ))
        ));
    }

    #[test]
    fn multiple_deposits_and_prior_withdrawals_are_accounted_in_order() {
        let database = test_database();
        seed_genesis(&database, hash(0));
        let ledger = WithdrawalLedger::new(ZONE);
        let user = address(1);
        let token = address(2);
        let first = parsed_block(
            &ledger,
            1,
            hash(1),
            hash(0),
            vec![
                (hash(0x21), vec![deposit(user, token, 70)]),
                (hash(0x22), vec![deposit(user, token, 50)]),
            ],
        )
        .unwrap();
        apply(&database, &ledger, &first).unwrap();
        let second = parsed_block(
            &ledger,
            2,
            hash(2),
            hash(1),
            vec![(hash(0x23), vec![withdrawal(user, token, 100, 10, 1)])],
        )
        .unwrap();
        apply(&database, &ledger, &second).unwrap();
        assert_eq!(
            balance(&database, &ledger, user, token),
            Some(U256::from(10))
        );
        assert_eq!(
            token_backing(&database, &ledger, token),
            Some(U256::from(10))
        );
    }

    #[test]
    fn balances_are_separate_by_user_and_token() {
        let database = test_database();
        seed_genesis(&database, hash(0));
        let ledger = WithdrawalLedger::new(ZONE);
        let user_a = address(1);
        let user_b = address(2);
        let token_a = address(3);
        let token_b = address(4);
        let block = parsed_block(
            &ledger,
            1,
            hash(1),
            hash(0),
            vec![
                (hash(0x31), vec![deposit(user_a, token_a, 10)]),
                (hash(0x32), vec![deposit(user_b, token_a, 20)]),
                (hash(0x33), vec![deposit(user_a, token_b, 30)]),
                (hash(0x34), vec![withdrawal(user_a, token_a, 3, 0, 1)]),
            ],
        )
        .unwrap();
        apply(&database, &ledger, &block).unwrap();
        assert_eq!(
            balance(&database, &ledger, user_a, token_a),
            Some(U256::from(7))
        );
        assert_eq!(
            balance(&database, &ledger, user_b, token_a),
            Some(U256::from(20))
        );
        assert_eq!(
            balance(&database, &ledger, user_a, token_b),
            Some(U256::from(30))
        );
        assert_eq!(
            token_backing(&database, &ledger, token_a),
            Some(U256::from(27))
        );
        assert_eq!(
            token_backing(&database, &ledger, token_b),
            Some(U256::from(30))
        );
    }

    #[test]
    fn withdrawals_cannot_consume_another_users_deposit() {
        #[derive(Clone, Copy)]
        enum Expected {
            Accepted,
            Rejected { available: u128 },
        }

        struct Case {
            name: &'static str,
            bob_withdrawal: u128,
            expected: Expected,
        }

        for case in [
            Case {
                name: "own deposit succeeds",
                bob_withdrawal: 50,
                expected: Expected::Accepted,
            },
            Case {
                name: "pooled deposits are rejected",
                bob_withdrawal: 100,
                expected: Expected::Rejected { available: 50 },
            },
        ] {
            let database = test_database();
            seed_genesis(&database, hash(0));
            let ledger = WithdrawalLedger::new(ZONE);
            let alice = address(1);
            let bob = address(2);
            let token = address(3);
            let deposits = parsed_block(
                &ledger,
                1,
                hash(1),
                hash(0),
                vec![
                    (hash(0x35), vec![deposit(alice, token, 50)]),
                    (hash(0x36), vec![deposit(bob, token, 50)]),
                ],
            )
            .unwrap();
            apply(&database, &ledger, &deposits).unwrap();
            let withdrawal = parsed_block(
                &ledger,
                2,
                hash(2),
                hash(1),
                vec![(
                    hash(0x37),
                    vec![withdrawal(bob, token, case.bob_withdrawal, 0, 1)],
                )],
            )
            .unwrap();

            match (apply(&database, &ledger, &withdrawal), case.expected) {
                (Ok(_), Expected::Accepted) => {}
                (
                    Err(WithdrawalCheckError::UnbackedWithdrawal(error)),
                    Expected::Rejected { available },
                ) => {
                    assert_eq!(error.user, bob, "{}", case.name);
                    assert_eq!(
                        error.requested,
                        U256::from(case.bob_withdrawal),
                        "{}",
                        case.name
                    );
                    assert_eq!(error.available, U256::from(available), "{}", case.name);
                }
                (result, _) => panic!("{}: unexpected result {result:?}", case.name),
            }
            assert_eq!(
                balance(&database, &ledger, alice, token),
                Some(U256::from(50)),
                "{}",
                case.name
            );
            assert_eq!(
                balance(&database, &ledger, bob, token),
                match case.expected {
                    Expected::Accepted => None,
                    Expected::Rejected { available } => Some(U256::from(available)),
                },
                "{}",
                case.name
            );
            assert_eq!(
                token_backing(&database, &ledger, token),
                Some(U256::from(match case.expected {
                    Expected::Accepted => 50,
                    Expected::Rejected { .. } => 100,
                })),
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn encrypted_deposits_bouncebacks_and_refund_claims_credit_the_recipient() {
        let database = test_database();
        seed_genesis(&database, hash(0));
        let ledger = WithdrawalLedger::new(ZONE);
        let user = address(1);
        let token = address(2);
        let block = parsed_block(
            &ledger,
            1,
            hash(1),
            hash(0),
            vec![(
                hash(0x41),
                vec![
                    encrypted_deposit(user, token, 11),
                    bounceback(user, token, 9),
                    refund_claim(user, token, 7),
                ],
            )],
        )
        .unwrap();
        apply(&database, &ledger, &block).unwrap();
        assert_eq!(
            balance(&database, &ledger, user, token),
            Some(U256::from(27))
        );
        assert_eq!(
            token_backing(&database, &ledger, token),
            Some(U256::from(27))
        );
    }

    #[test]
    fn protocol_deposit_bounceback_withdrawal_is_ignored() {
        let ledger = WithdrawalLedger::new(ZONE);
        let events = parse_events(
            ledger.zone,
            1,
            &[(
                hash(0x51),
                vec![withdrawal(Address::ZERO, address(2), u128::MAX, 0, 0)],
            )],
        )
        .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn same_block_withdrawal_before_deposit_fails_closed() {
        let database = test_database();
        seed_genesis(&database, hash(0));
        let ledger = WithdrawalLedger::new(ZONE);
        let user = address(1);
        let token = address(2);
        let block = parsed_block(
            &ledger,
            1,
            hash(1),
            hash(0),
            vec![
                (hash(0x61), vec![withdrawal(user, token, 1, 0, 1)]),
                (hash(0x62), vec![deposit(user, token, 1)]),
            ],
        )
        .unwrap();
        let error = apply(&database, &ledger, &block).unwrap_err();
        assert!(matches!(error, WithdrawalCheckError::UnbackedWithdrawal(_)));
        assert_eq!(balance(&database, &ledger, user, token), None);
    }

    #[test]
    fn malformed_relevant_event_fails_closed() {
        let ledger = WithdrawalLedger::new(ZONE);
        let mut log = deposit(address(1), address(2), 1);
        log.data = LogData::new_unchecked(log.topics().to_vec(), Bytes::new());
        let error = parse_events(ledger.zone, 4, &[(hash(0x71), vec![log])]).unwrap_err();
        assert!(matches!(
            error,
            WithdrawalCheckError::MalformedEvent {
                zone: ZONE,
                block: 4,
                tx_hash,
                event: "DepositProcessed",
                ..
            } if tx_hash == hash(0x71)
        ));
    }

    #[test]
    fn unbacked_withdrawal_error_contains_diagnostics() {
        let database = test_database();
        seed_genesis(&database, hash(0));
        let ledger = WithdrawalLedger::new(ZONE);
        let user = address(1);
        let token = address(2);
        let tx_hash = hash(0x81);
        let block = parsed_block(
            &ledger,
            1,
            hash(1),
            hash(0),
            vec![(tx_hash, vec![withdrawal(user, token, 8, 2, 1)])],
        )
        .unwrap();
        let error = apply(&database, &ledger, &block).unwrap_err();
        let WithdrawalCheckError::UnbackedWithdrawal(error) = error else {
            panic!("unexpected error: {error}");
        };
        assert_eq!(error.zone, ZONE);
        assert_eq!(error.block, 1);
        assert_eq!(error.tx_hash, tx_hash);
        assert_eq!(error.user, user);
        assert_eq!(error.token, token);
        assert_eq!(error.requested, U256::from(10));
        assert_eq!(error.available, U256::ZERO);
    }

    #[test]
    fn restart_restores_checkpoint_balances_and_deltas() {
        let database = test_database();
        let path = database.path();
        seed_genesis(&database, hash(0));
        let ledger = WithdrawalLedger::new(ZONE);
        let user = address(1);
        let token = address(2);
        let block = parsed_block(
            &ledger,
            1,
            hash(1),
            hash(0),
            vec![(hash(0x91), vec![deposit(user, token, 12)])],
        )
        .unwrap();
        apply(&database, &ledger, &block).unwrap();

        let database = Arc::try_unwrap(database).ok().unwrap().into_inner_db();
        drop(database);
        let database = init_db(&path, DatabaseArguments::test()).unwrap();
        assert!(!register_tables(database).unwrap());
        let database = TempDatabase::new(init_db(&path, DatabaseArguments::test()).unwrap(), path);
        let tx = database.tx().unwrap();
        assert_eq!(ledger.checkpoint_from_tx(&tx).unwrap().number, 1);
        drop(tx);
        assert_eq!(
            balance(&database, &ledger, user, token),
            Some(U256::from(12))
        );
        assert_eq!(
            token_backing(&database, &ledger, token),
            Some(U256::from(12))
        );
        let tx = database.tx().unwrap();
        assert!(tx.get::<WithdrawalBlockDeltas>(1).unwrap().is_some());
    }

    #[test]
    fn mismatched_token_aggregate_fails_closed() {
        let database = test_database();
        seed_genesis(&database, hash(0));
        let ledger = WithdrawalLedger::new(ZONE);
        let user = address(1);
        let token = address(2);
        let block = parsed_block(
            &ledger,
            1,
            hash(1),
            hash(0),
            vec![(hash(0x92), vec![deposit(user, token, 12)])],
        )
        .unwrap();
        apply(&database, &ledger, &block).unwrap();

        let tx = database.tx_mut().unwrap();
        tx.put::<WithdrawalTokenBacking>(token, CompactU256::from(U256::from(11)))
            .unwrap();
        tx.commit().unwrap();

        let tx = database.tx().unwrap();
        let error = ledger.verify_token_backing(&tx).unwrap_err();
        assert!(matches!(error, WithdrawalCheckError::InvalidState(_)));
    }

    #[test]
    fn block_delta_retention_is_bounded() {
        let database = test_database();
        seed_genesis(&database, hash(0));
        let ledger = WithdrawalLedger::new(ZONE);
        let parent_hash = hash(0xa0);
        let tx = database.tx_mut().unwrap();
        tx.put::<WithdrawalCheckerState>(
            CHECKPOINT_KEY,
            Checkpoint::new(ZONE, BLOCK_DELTA_RETENTION, parent_hash),
        )
        .unwrap();
        tx.put::<WithdrawalBlockDeltas>(1, BlockDelta::new(hash(1), hash(0), Vec::new()).unwrap())
            .unwrap();
        tx.commit().unwrap();

        let block = CanonicalBlock {
            number: BLOCK_DELTA_RETENTION + 1,
            hash: hash(0xa1),
            parent_hash,
            events: Vec::new(),
        };
        apply(&database, &ledger, &block).unwrap();

        let tx = database.tx().unwrap();
        assert!(tx.get::<WithdrawalBlockDeltas>(1).unwrap().is_none());
        assert_eq!(
            ledger.checkpoint_from_tx(&tx).unwrap().number,
            BLOCK_DELTA_RETENTION + 1
        );
        assert!(
            tx.get::<WithdrawalBlockDeltas>(BLOCK_DELTA_RETENTION + 1)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn reorg_unwind_restores_old_values_before_applying_new_branch() {
        let database = test_database();
        seed_genesis(&database, hash(0));
        let ledger = WithdrawalLedger::new(ZONE);
        let user = address(1);
        let token = address(2);
        let first = parsed_block(
            &ledger,
            1,
            hash(1),
            hash(0),
            vec![(hash(0xa1), vec![deposit(user, token, 10)])],
        )
        .unwrap();
        apply(&database, &ledger, &first).unwrap();
        let old_second = parsed_block(
            &ledger,
            2,
            hash(2),
            hash(1),
            vec![(hash(0xa2), vec![withdrawal(user, token, 3, 0, 1)])],
        )
        .unwrap();
        apply(&database, &ledger, &old_second).unwrap();
        assert_eq!(
            balance(&database, &ledger, user, token),
            Some(U256::from(7))
        );

        let new_second = parsed_block(
            &ledger,
            2,
            hash(3),
            hash(1),
            vec![(hash(0xa3), vec![deposit(user, token, 5)])],
        )
        .unwrap();
        let tx = database.tx_mut().unwrap();
        let checkpoint = ledger.checkpoint_from_tx(&tx).unwrap();
        let checkpoint = ledger.unwind_one(&tx, checkpoint).unwrap();
        let checkpoint = ledger.apply_block(&tx, checkpoint, &new_second).unwrap();
        tx.commit().unwrap();
        assert_eq!(checkpoint.hash, hash(3));
        assert_eq!(
            balance(&database, &ledger, user, token),
            Some(U256::from(15))
        );
        assert_eq!(
            token_backing(&database, &ledger, token),
            Some(U256::from(15))
        );
    }
}
