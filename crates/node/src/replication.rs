//! Node-side leader block replication and follower import.

use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockHashOrNumber;
use alloy_primitives::B256;
use alloy_provider::DynProvider;
use alloy_rlp::Decodable as _;
use alloy_rpc_types_engine::ForkchoiceState;
use alloy_sol_types::SolCall as _;
use futures::{StreamExt as _, stream::BoxStream};
use reth_chain_state::PersistedBlockSubscriptions;
use reth_node_api::{ConsensusEngineHandle, PayloadTypes as _};
use reth_primitives_traits::{SealedBlock, SealedHeader};
use reth_provider::HeaderProvider;
use reth_storage_api::{BlockNumReader, BlockReader, ReceiptProvider, StateProviderFactory};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempo_alloy::TempoNetwork;
use tempo_primitives::{Block, TempoHeader, TempoTxEnvelope};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zone_l1::{L1BlockTracker, PolicyCache, TempoStateExt as _};
use zone_p2p::{P2pCommand, P2pEvent};
use zone_payload::{
    ZonePayloadTypes,
    abi::{ZONE_INBOX_ADDRESS, ZoneInbox},
};
use zone_sequencer::BatchAnchorConfig;

use crate::{
    settlement_attestation::build_settlement_attestation,
    withdrawal_checker::{WithdrawalCheckDecision, WithdrawalChecker},
};
use alloy_signer_local::PrivateKeySigner;
use zone_sequencer::attestation::{
    AttestationDomain, AttestationStore, BlockAck, SettlementAttestation, SignedBlockAck,
    SignedSettlementAttestation, SignedSigningRefusal, SigningRefusal,
};

type ValidatedL1Anchor = (u64, B256, u64, B256);

#[derive(Clone)]
/// Shared context for signed backfill proofs and batch-boundary settlement attestations.
pub(crate) struct AttestationContext {
    pub(crate) domain: AttestationDomain,
    pub(crate) signer: PrivateKeySigner,
    pub(crate) addresses: HashMap<zone_p2p::P2pPeerId, alloy_primitives::Address>,
    pub(crate) store: AttestationStore,
    pub(crate) l1_provider: DynProvider<TempoNetwork>,
    pub(crate) anchor_config: BatchAnchorConfig,
    pub(crate) withdrawal_checker: WithdrawalChecker,
    validated_l1_headers: Arc<Mutex<BTreeMap<u64, (B256, B256)>>>,
    validated_l1_anchors: Arc<Mutex<BTreeSet<ValidatedL1Anchor>>>,
}

const MAX_VALIDATED_L1_HEADERS: usize = 16_384;
const MAX_VALIDATED_L1_ANCHORS: usize = 120;

impl AttestationContext {
    pub(crate) fn new(
        domain: AttestationDomain,
        signer: PrivateKeySigner,
        addresses: HashMap<zone_p2p::P2pPeerId, alloy_primitives::Address>,
        store: AttestationStore,
        l1_provider: DynProvider<TempoNetwork>,
        anchor_config: BatchAnchorConfig,
        withdrawal_checker: WithdrawalChecker,
    ) -> Self {
        Self {
            domain,
            signer,
            addresses,
            store,
            l1_provider,
            anchor_config,
            withdrawal_checker,
            validated_l1_headers: Arc::default(),
            validated_l1_anchors: Arc::default(),
        }
    }

    pub(crate) fn cached_l1_headers(&self, start: u64, end: u64) -> BTreeMap<u64, (B256, B256)> {
        self.validated_l1_headers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .range(start..=end)
            .map(|(&number, &header)| (number, header))
            .collect()
    }

    pub(crate) fn cache_validated_l1_headers(
        &self,
        headers: impl IntoIterator<Item = (u64, B256, B256)>,
    ) -> eyre::Result<()> {
        let mut cache = self
            .validated_l1_headers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (number, parent_hash, hash) in headers {
            if let Some(existing) = cache.get(&number) {
                eyre::ensure!(
                    *existing == (parent_hash, hash),
                    "L1 header {number} conflicts with the validated ancestry cache"
                );
            } else {
                cache.insert(number, (parent_hash, hash));
            }
        }
        while cache.len() > MAX_VALIDATED_L1_HEADERS {
            cache.pop_first();
        }
        Ok(())
    }

    pub(crate) fn l1_anchor_is_validated(
        &self,
        tempo_number: u64,
        tempo_hash: B256,
        anchor_number: u64,
        anchor_hash: B256,
    ) -> bool {
        self.validated_l1_anchors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&(tempo_number, tempo_hash, anchor_number, anchor_hash))
    }

    pub(crate) fn cache_validated_l1_anchor(
        &self,
        tempo_number: u64,
        tempo_hash: B256,
        anchor_number: u64,
        anchor_hash: B256,
    ) {
        let mut anchors = self
            .validated_l1_anchors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anchors.insert((tempo_number, tempo_hash, anchor_number, anchor_hash));
        while anchors.len() > MAX_VALIDATED_L1_ANCHORS {
            anchors.pop_first();
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PersistedTip {
    number: u64,
    hash: B256,
}

pub(crate) struct EncodedPersistedBlock {
    number: u64,
    hash: B256,
    encoded: Vec<u8>,
}

/// Interface used by the replication task to keep track of blocks that are persisted vs broadcast
pub(crate) trait PersistedBlockSource: Clone + Send + Sync + 'static {
    fn last_block_number(&self) -> eyre::Result<u64>;
    fn persisted_block_stream(&self) -> BoxStream<'static, PersistedTip>;
    fn encoded_block_by_number(&self, number: u64) -> eyre::Result<EncodedPersistedBlock>;
}

impl<P> PersistedBlockSource for P
where
    P: PersistedBlockSubscriptions + BlockReader<Block = Block> + Clone + Send + Sync + 'static,
{
    fn last_block_number(&self) -> eyre::Result<u64> {
        Ok(BlockNumReader::last_block_number(self)?)
    }

    fn persisted_block_stream(&self) -> BoxStream<'static, PersistedTip> {
        PersistedBlockSubscriptions::persisted_block_stream(self)
            .map(|tip| PersistedTip {
                number: tip.number,
                hash: tip.hash,
            })
            .boxed()
    }

    fn encoded_block_by_number(&self, number: u64) -> eyre::Result<EncodedPersistedBlock> {
        let block = self
            .block_by_number(number)?
            .ok_or_else(|| eyre::eyre!("persisted zone block {number} is missing"))?;
        let sealed = SealedBlock::seal_slow(block);
        Ok(EncodedPersistedBlock {
            number: sealed.number(),
            hash: sealed.hash(),
            encoded: alloy_rlp::encode(sealed.into_block()),
        })
    }
}

/// Broadcast every newly persisted leader block in canonical order.
pub(crate) async fn broadcast_persisted_blocks<P>(provider: P, commands: mpsc::Sender<P2pCommand>)
where
    P: PersistedBlockSource,
{
    // Handle race conditions carefully at startup. Read before subscribing, then reconcile after subscribing.
    // This closes both startup windows: a block persisted before the subscription is found by the
    // second read, while a block persisted after the subscription is retained by the stream.
    let mut last_broadcast = match provider.last_block_number() {
        Ok(number) => number,
        Err(err) => {
            tracing::error!(target: "zone::p2p", %err, "Failed reading persisted zone head");
            return;
        }
    };
    let mut persisted = provider.persisted_block_stream();
    let startup_tip = match provider.last_block_number() {
        Ok(number) => number,
        Err(err) => {
            tracing::error!(target: "zone::p2p", %err, "Failed reconciling persisted zone head");
            return;
        }
    };

    if let Err(err) =
        broadcast_persisted_range(&provider, &commands, &mut last_broadcast, startup_tip, None)
            .await
    {
        tracing::error!(target: "zone::p2p", %err, "Failed broadcasting persisted zone blocks");
        return;
    }

    while let Some(persisted_tip) = persisted.next().await {
        if persisted_tip.number < last_broadcast {
            tracing::error!(
                target: "zone::p2p",
                persisted = persisted_tip.number,
                last_broadcast,
                "Persisted zone head moved backwards"
            );
            return;
        }

        if let Err(err) = broadcast_persisted_range(
            &provider,
            &commands,
            &mut last_broadcast,
            persisted_tip.number,
            Some(persisted_tip.hash),
        )
        .await
        {
            tracing::error!(target: "zone::p2p", %err, "Failed broadcasting persisted zone blocks");
            return;
        }
    }
    debug!(target: "zone::p2p", "Persisted block stream closed");
}

async fn broadcast_persisted_range<P>(
    provider: &P,
    commands: &mpsc::Sender<P2pCommand>,
    last_broadcast: &mut u64,
    tip_number: u64,
    expected_tip_hash: Option<B256>,
) -> eyre::Result<()>
where
    P: PersistedBlockSource,
{
    for number in last_broadcast.saturating_add(1)..=tip_number {
        let block = provider.encoded_block_by_number(number)?;
        let number = block.number;
        let hash = block.hash;
        if number == tip_number
            && let Some(expected) = expected_tip_hash
            && hash != expected
        {
            eyre::bail!(
                "persisted zone block hash does not match notification at height {number}: expected={expected}, actual={hash}"
            );
        }
        commands
            .send(P2pCommand::BroadcastBlock(block.encoded))
            .await
            .map_err(|_| eyre::eyre!("P2P command channel closed"))?;
        debug!(target: "zone::p2p", number, ?hash, "Queued persisted block for followers");
        *last_broadcast = number;
    }
    Ok(())
}

const BACKFILL_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const BLOCK_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PENDING_BLOCKS: usize = 128;
const MAX_PENDING_BLOCK_BYTES: usize = 128 * 1024 * 1024;
const BACKFILL_PAGE_SIZE: u64 = 64;
const BACKFILL_SERVE_QUEUE_CAPACITY: usize = 8;
const SETTLEMENT_SIGNING_QUEUE_CAPACITY: usize = 8;

struct BackfillRequest {
    peer: zone_p2p::P2pPeerId,
    request_id: u64,
    start: u64,
}

struct BackfillServerTask(tokio::task::JoinHandle<()>);

impl Drop for BackfillServerTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct SigningWorkerTask(tokio::task::JoinHandle<()>);

impl Drop for SigningWorkerTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct SettlementSigningRequest {
    leader: zone_p2p::P2pPeerId,
    proposal: Vec<u8>,
}

fn verify_member_ack(
    peer: &zone_p2p::P2pPeerId,
    encoded_ack: &[u8],
    expected: BlockAck,
    domain: AttestationDomain,
    addresses: &HashMap<zone_p2p::P2pPeerId, alloy_primitives::Address>,
) -> eyre::Result<()> {
    let signed = SignedBlockAck::decode(encoded_ack)?;
    eyre::ensure!(
        signed.ack == expected,
        "signed ACK does not match its target"
    );
    let signer = signed.recover_signer(domain)?;
    let expected_signer = addresses
        .get(peer)
        .ok_or_else(|| eyre::eyre!("signed response came from an unknown peer"))?;
    eyre::ensure!(
        signer == *expected_signer,
        "ACK signer {signer} does not match authenticated peer {expected_signer}"
    );
    Ok(())
}

fn verify_backfill_block(
    peer: &zone_p2p::P2pPeerId,
    encoded: &[u8],
    encoded_ack: &[u8],
    domain: AttestationDomain,
    addresses: &HashMap<zone_p2p::P2pPeerId, alloy_primitives::Address>,
) -> eyre::Result<(u64, B256)> {
    let (number, hash) = encoded_block_identity(encoded)?;
    verify_member_ack(
        peer,
        encoded_ack,
        BlockAck::new(domain, number, hash),
        domain,
        addresses,
    )?;
    Ok((number, hash))
}

fn verify_backfill_tip(
    peer: &zone_p2p::P2pPeerId,
    target: alloy_eips::BlockNumHash,
    encoded_ack: &[u8],
    domain: AttestationDomain,
    addresses: &HashMap<zone_p2p::P2pPeerId, alloy_primitives::Address>,
) -> eyre::Result<()> {
    verify_member_ack(
        peer,
        encoded_ack,
        BlockAck::new(domain, target.number, target.hash),
        domain,
        addresses,
    )
}

/// Keep track of the backfill exactly. We'll buffer any live blocks received
/// during backfill.
struct BackfillProgress {
    target_tip: Option<u64>,
    received_completion: bool,
    needed: bool,
}

impl BackfillProgress {
    const fn new() -> Self {
        Self {
            target_tip: None,
            received_completion: false,
            needed: true,
        }
    }

    fn observe_block(&mut self, number: u64, best: u64) {
        self.target_tip = Some(self.target_tip.map_or(number, |tip| tip.max(number)));
        if number > best.saturating_add(1) {
            self.needed = true;
        }
    }

    fn refresh_after_import(&mut self, best: u64, first_pending: Option<u64>) {
        self.needed = !self.received_completion
            || self.target_tip.is_some_and(|tip| best < tip)
            || first_pending.is_some_and(|number| number > best.saturating_add(1));
    }

    fn complete(&mut self, tip: u64, best: u64, first_pending: Option<u64>) {
        self.received_completion = true;
        self.target_tip = Some(self.target_tip.map_or(tip, |target| target.max(tip)));
        self.needed = best < self.target_tip.unwrap_or(tip)
            || first_pending.is_some_and(|number| number > best.saturating_add(1));
    }

    fn request(&self, best: u64) -> Option<P2pCommand> {
        self.needed.then(|| P2pCommand::RequestBackfill {
            start: best.saturating_add(1),
        })
    }

    fn probe_after_inactivity(&mut self, best: u64) -> P2pCommand {
        self.needed = true;
        P2pCommand::RequestBackfill {
            start: best.saturating_add(1),
        }
    }
}

fn buffer_pending_block(
    pending: &mut BTreeMap<u64, Vec<u8>>,
    pending_bytes: &mut usize,
    number: u64,
    block: Vec<u8>,
) -> Option<u64> {
    if pending.contains_key(&number) {
        return None;
    }
    *pending_bytes = pending_bytes.saturating_add(block.len());
    pending.insert(number, block);

    let mut dropped = None;
    while pending.len() > MAX_PENDING_BLOCKS || *pending_bytes > MAX_PENDING_BLOCK_BYTES {
        let Some((dropped_number, dropped_block)) = pending.pop_last() else {
            break;
        };
        *pending_bytes = pending_bytes.saturating_sub(dropped_block.len());
        dropped = Some(dropped_number);
    }
    dropped
}

fn encoded_block_number(encoded: &[u8]) -> eyre::Result<u64> {
    encoded_block_identity(encoded).map(|(number, _)| number)
}

fn encoded_block_identity(encoded: &[u8]) -> eyre::Result<(u64, B256)> {
    let mut input = encoded;
    let block = Block::decode(&mut input)
        .map_err(|err| eyre::eyre!("invalid RLP-encoded zone block: {err}"))?;
    eyre::ensure!(
        input.is_empty(),
        "encoded zone block has {} trailing bytes",
        input.len()
    );
    let block = SealedBlock::seal_slow(block);
    Ok((block.number(), block.hash()))
}

async fn serve_backfill_page<P>(
    provider: &P,
    commands: &mpsc::Sender<P2pCommand>,
    attestation: &AttestationContext,
    peer: zone_p2p::P2pPeerId,
    request_id: u64,
    start: u64,
    signing_halted: &AtomicBool,
) -> eyre::Result<()>
where
    P: BlockNumReader
        + BlockReader<Block = Block>
        + HeaderProvider<Header = TempoHeader>
        + ReceiptProvider,
{
    eyre::ensure!(
        start != 0,
        "cannot serve genesis as a signed backfill block"
    );
    let tip = provider.best_block_number()?;
    let end = tip.min(start.saturating_add(BACKFILL_PAGE_SIZE.saturating_sub(1)));
    let mut signed_tip = None;
    for number in start..=end {
        let block = provider.block_by_number(number)?.ok_or_else(|| {
            eyre::eyre!("canonical block {number} is missing while serving backfill")
        })?;
        let signed =
            validated_backfill_ack(provider, number, attestation, signing_halted, commands).await?;
        let encoded_ack = signed.encode();
        if number == tip {
            signed_tip = Some((signed.ack.zoneBlockHash, encoded_ack.clone()));
        }
        commands
            .send(P2pCommand::SendBackfillBlock {
                peer: peer.clone(),
                request_id,
                block: alloy_rlp::encode(block),
                block_ack: encoded_ack,
            })
            .await
            .map_err(|_| eyre::eyre!("P2P command channel closed"))?;
    }
    let (tip_hash, tip_ack) = match signed_tip {
        Some(signed) => signed,
        None => {
            let signed =
                validated_backfill_ack(provider, tip, attestation, signing_halted, commands)
                    .await?;
            (signed.ack.zoneBlockHash, signed.encode())
        }
    };
    commands
        .send(P2pCommand::CompleteBackfill {
            peer,
            request_id,
            tip,
            tip_hash,
            tip_ack,
        })
        .await
        .map_err(|_| eyre::eyre!("P2P command channel closed"))?;
    Ok(())
}

async fn validated_backfill_ack<P>(
    provider: &P,
    number: u64,
    attestation: &AttestationContext,
    signing_halted: &AtomicBool,
    commands: &mpsc::Sender<P2pCommand>,
) -> eyre::Result<SignedBlockAck>
where
    P: HeaderProvider<Header = TempoHeader> + ReceiptProvider,
{
    ensure_block_receipts_persisted(provider, number)?;
    let ack = build_backfill_ack(provider, number, attestation.domain)?;
    let target = alloy_eips::BlockNumHash {
        number,
        hash: ack.zoneBlockHash,
    };
    match attestation.withdrawal_checker.decide(target).await {
        WithdrawalCheckDecision::Submit => {
            SignedBlockAck::sign(ack, attestation.domain, &attestation.signer)
        }
        WithdrawalCheckDecision::Retry(error) => {
            error.emit_withheld();
            eyre::bail!("withdrawal checker is not ready at block {number}")
        }
        WithdrawalCheckDecision::Halt(error) => {
            error.emit_withheld();
            signing_halted.store(true, Ordering::Release);
            commands
                .send(signing_refusal(target, attestation)?)
                .await
                .map_err(|_| {
                    eyre::eyre!("P2P command channel closed before signing refusal could be sent")
                })?;
            eyre::bail!("withdrawal checker halted at block {number}")
        }
    }
}

async fn serve_backfill_requests<P>(
    provider: P,
    commands: mpsc::Sender<P2pCommand>,
    attestation: AttestationContext,
    mut requests: mpsc::Receiver<BackfillRequest>,
    signing_halted: Arc<AtomicBool>,
) where
    P: BlockNumReader
        + BlockReader<Block = Block>
        + HeaderProvider<Header = TempoHeader>
        + ReceiptProvider
        + Clone
        + Send
        + Sync
        + 'static,
{
    // One worker deliberately serializes page construction and sending, bounding serving
    // concurrency independently of the block import loop.
    while let Some(BackfillRequest {
        peer,
        request_id,
        start,
    }) = requests.recv().await
    {
        if signing_halted.load(Ordering::Acquire) {
            continue;
        }
        let result = serve_backfill_page(
            &provider,
            &commands,
            &attestation,
            peer,
            request_id,
            start,
            &signing_halted,
        )
        .await;
        if let Err(err) = result {
            tracing::error!(target: "zone::p2p", %err, start, "Failed serving block backfill");
        }
    }
}

async fn run_settlement_signer<P>(
    provider: P,
    commands: mpsc::Sender<P2pCommand>,
    attestation: AttestationContext,
    mut requests: mpsc::Receiver<SettlementSigningRequest>,
    signing_halted: Arc<AtomicBool>,
) where
    P: HeaderProvider<Header = TempoHeader> + ReceiptProvider,
{
    while let Some(SettlementSigningRequest { leader, proposal }) = requests.recv().await {
        let validated = async {
            let proposal = SettlementAttestation::decode(&proposal)?;
            let height: u64 = proposal
                .zoneHeight
                .try_into()
                .map_err(|_| eyre::eyre!("settlement height does not fit in u64"))?;
            let expected = build_settlement_attestation(
                &provider,
                height,
                &attestation,
                Some((proposal.anchorBlockNumber, proposal.anchorBlockHash)),
            )
            .await?
            .ok_or_else(|| eyre::eyre!("proposed block is not a batch boundary"))?;
            eyre::ensure!(
                proposal == expected.attestation,
                "settlement proposal does not match follower state"
            );
            Ok::<_, eyre::Report>((height, proposal, expected.target))
        }
        .await;
        let (height, proposal, target) = match validated {
            Ok(validated) => validated,
            Err(err) => {
                tracing::warn!(target: "zone::p2p", %leader, %err, "Rejected settlement proposal");
                continue;
            }
        };

        let command = if signing_halted.load(Ordering::Acquire) {
            signing_refusal(target, &attestation)
        } else {
            match attestation.withdrawal_checker.decide(target).await {
                WithdrawalCheckDecision::Submit => {
                    let signed = match SignedSettlementAttestation::sign(
                        proposal,
                        attestation.domain,
                        &attestation.signer,
                    ) {
                        Ok(signed) => signed,
                        Err(err) => {
                            tracing::error!(target: "zone::p2p", %leader, %err, height, "Failed signing settlement proposal");
                            continue;
                        }
                    };
                    info!(target: "zone::p2p", %leader, height, "Signed settlement proposal");
                    Ok(P2pCommand::SendSettlementSignature(signed.encode()))
                }
                WithdrawalCheckDecision::Retry(error) => {
                    error.emit_withheld();
                    continue;
                }
                WithdrawalCheckDecision::Halt(error) => {
                    error.emit_withheld();
                    signing_halted.store(true, Ordering::Release);
                    signing_refusal(target, &attestation)
                }
            }
        };
        let command = match command {
            Ok(command) => command,
            Err(error) => {
                tracing::error!(target: "zone::p2p", %error, height, "Failed signing terminal refusal");
                continue;
            }
        };
        if commands.send(command).await.is_err() {
            debug!(target: "zone::p2p", "P2P command channel closed before signing response could be sent");
            return;
        }
    }
}

/// Serve catch-up requests and import live/backfilled blocks in canonical order.
pub(crate) async fn run_block_sync<P>(
    provider: P,
    engine: ConsensusEngineHandle<ZonePayloadTypes>,
    mut events: mpsc::Receiver<P2pEvent>,
    commands: mpsc::Sender<P2pCommand>,
    role: zone_p2p::Role,
    attestation: AttestationContext,
    l1_block_tracker: L1BlockTracker,
    policy_cache: PolicyCache,
) where
    P: BlockNumReader
        + BlockReader<Block = Block>
        + HeaderProvider<Header = TempoHeader>
        + ReceiptProvider
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    // Keep track of all live blocks if we end up needing a backfill, so we can immediately catchup
    // This is capped to `MAX_PENDING_BLOCKS`.
    let mut pending = BTreeMap::<u64, Vec<u8>>::new();
    let mut pending_bytes = 0;
    let mut backfill = BackfillProgress::new();
    let (backfill_requests, backfill_request_rx) = mpsc::channel(BACKFILL_SERVE_QUEUE_CAPACITY);

    let signing_halted = Arc::new(AtomicBool::new(false));
    // Serve backfill requests in a separate task to avoid competing with live block import.
    let mut backfill_server = BackfillServerTask(tokio::spawn(serve_backfill_requests(
        provider.clone(),
        commands.clone(),
        attestation.clone(),
        backfill_request_rx,
        signing_halted.clone(),
    )));
    let (settlement_signing, settlement_signing_rx) =
        mpsc::channel(SETTLEMENT_SIGNING_QUEUE_CAPACITY);
    let mut settlement_signer = SigningWorkerTask(tokio::spawn(run_settlement_signer(
        provider.clone(),
        commands.clone(),
        attestation.clone(),
        settlement_signing_rx,
        signing_halted.clone(),
    )));

    // Always probe on startup to see if we're behind
    let mut retry = tokio::time::interval(BACKFILL_RETRY_INTERVAL);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // If we don't get blocks after 30s, probe to get the next block
    let inactivity = tokio::time::sleep(BLOCK_INACTIVITY_TIMEOUT);
    tokio::pin!(inactivity);

    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else {
                    debug!(target: "zone::p2p", "P2P event channel closed");
                    return;
                };
                match event {
                    P2pEvent::Started { .. } => {}

                    // Followers receive SettlementProposals from the Leader and batch boundaries, which they verify
                    // and if everything is correct, sign and return back to the leader.
                    P2pEvent::SettlementProposalReceived { leader, proposal } => {
                        if role != zone_p2p::Role::Follower {
                            continue;
                        }
                        if let Err(error) = settlement_signing.try_send(SettlementSigningRequest {
                            leader,
                            proposal,
                        }) {
                            tracing::warn!(target: "zone::p2p", %error, "Dropped settlement signing request because the bounded queue is full");
                        }
                    }

                    // Leaders received a signed SettmentAttestation from a follower
                    P2pEvent::SettlementSignatureReceived { follower, signature } => {
                        if role != zone_p2p::Role::Leader {
                            continue;
                        }

                        let result = async {
                            // Do some basic checks
                            let signed = SignedSettlementAttestation::decode(&signature)?;
                            let height: u64 = signed.attestation.zoneHeight.try_into()
                                .map_err(|_| eyre::eyre!("settlement height does not fit in u64"))?;
                            let signer = signed.recover_signer(attestation.domain)?;
                            let expected_signer = attestation.addresses.get(&follower)
                                .copied()
                                .ok_or_else(|| eyre::eyre!("unknown follower identity"))?;
                            eyre::ensure!(signer == expected_signer, "settlement signer does not match authenticated peer");
                            let (target, signatures) = attestation.store.insert_settlement_signature(
                                attestation.domain,
                                signer,
                                signed,
                            )?;
                            Ok::<_, eyre::Report>((height, target.hash, signer, signatures))
                        }.await;
                        match result {
                            Ok((height, block_hash, signer, signatures)) => info!(target: "zone::p2p", %follower, %signer, height, %block_hash, signatures, "Stored follower settlement signature"),
                            Err(err) => tracing::warn!(target: "zone::p2p", %follower, %err, "Rejected follower settlement signature"),
                        }
                    }
                    P2pEvent::RefusedToSign { follower, refusal } => {
                        if role != zone_p2p::Role::Leader {
                            continue;
                        }
                        let Some(signer) = attestation.addresses.get(&follower).copied() else {
                            tracing::warn!(target: "zone::p2p", %follower, "Rejected signing refusal from unknown follower");
                            continue;
                        };
                        let target = match verify_signing_refusal(
                            &refusal,
                            attestation.domain,
                            signer,
                        ) {
                            Ok(target) => target,
                            Err(error) => {
                                tracing::warn!(target: "zone::p2p", %follower, %error, "Rejected signing refusal");
                                continue;
                            }
                        };
                        let applied = attestation.store.refuse_to_sign(signer, target);
                        tracing::warn!(
                            target: "zone::p2p",
                            %follower,
                            %signer,
                            block = target.number,
                            block_hash = %target.hash,
                            applied_to_quorum = applied,
                            "Follower refused to sign after a terminal safety-check failure"
                        );
                    }
                    P2pEvent::BackfillRequested { peer, request_id, start } => {
                        if let Err(err) = backfill_requests.try_send(BackfillRequest { peer, request_id, start }) {
                            tracing::warn!(target: "zone::p2p", %err, start, queue_capacity = BACKFILL_SERVE_QUEUE_CAPACITY, "Dropped block backfill request because the serving queue is unavailable");
                        }
                    }

                    // Received a live || backfilled block.
                    event @ (P2pEvent::BlockReceived { .. } | P2pEvent::BackfillBlockReceived { .. }) => {
                        let (peer, block, block_ack, from_backfill) = match event {
                            P2pEvent::BlockReceived { leader_ed25519_public_key, block } => (leader_ed25519_public_key, block, None, false),
                            P2pEvent::BackfillBlockReceived { peer, block, block_ack } => (peer, block, Some(block_ack), true),
                            _ => unreachable!(),
                        };
                        if from_backfill && role != zone_p2p::Role::Follower {
                            continue;
                        }
                        let agreed_number = if from_backfill {
                            match verify_backfill_block(
                                &peer,
                                &block,
                                block_ack.as_deref().expect("backfill event includes an ACK"),
                                attestation.domain,
                                &attestation.addresses,
                            ) {
                                Ok((number, _)) => Some(number),
                                Err(err) => {
                                    tracing::warn!(target: "zone::p2p", %err, "Rejected backfill block without a valid member signature");
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        match agreed_number.map_or_else(|| encoded_block_number(&block), Ok) {
                            Ok(number) => {
                                inactivity
                                    .as_mut()
                                    .reset(tokio::time::Instant::now() + BLOCK_INACTIVITY_TIMEOUT);
                                let best = match provider.best_block_number() {
                                    Ok(best) => best,
                                    Err(err) => {
                                        tracing::error!(target: "zone::p2p", %err, "Failed reading local head");
                                        continue;
                                    }
                                };
                                if number <= best {
                                    if let Err(err) = import_peer_block(
                                        &provider,
                                        &engine,
                                        &l1_block_tracker,
                                        &policy_cache,
                                        &block,
                                    ).await {
                                        tracing::error!(target: "zone::p2p", %err, "Rejected duplicate or conflicting peer block");
                                    }
                                    continue;
                                }
                                backfill.observe_block(number, best);
                                if let Some(dropped) = buffer_pending_block(&mut pending, &mut pending_bytes, number, block) {
                                    tracing::warn!(target: "zone::p2p", dropped, pending_block_limit = MAX_PENDING_BLOCKS, pending_byte_limit = MAX_PENDING_BLOCK_BYTES, "Dropped far-future peer block because the pending block buffer is full");
                                }
                                if number > best.saturating_add(1) {
                                    info!(target: "zone::p2p", local_head = best, received = number, "Detected zone block gap; requesting backfill");
                                }
                                if let Err(err) = drain_pending_blocks(
                                    &provider,
                                    &engine,
                                    &l1_block_tracker,
                                    &policy_cache,
                                    &mut pending,
                                    &mut pending_bytes,
                                ).await {
                                    tracing::error!(target: "zone::p2p", %err, "Rejected peer block while draining backfill");
                                    backfill.needed = true;
                                } else {
                                    let new_best = match provider.best_block_number() {
                                        Ok(best) => best,
                                        Err(err) => {
                                            tracing::error!(target: "zone::p2p", %err, "Failed reading local head after importing peer blocks");
                                            continue;
                                        }
                                    };
                                    backfill.refresh_after_import(
                                        new_best,
                                        pending.first_key_value().map(|(&number, _)| number),
                                    );
                                }
                            }
                            Err(err) => tracing::error!(target: "zone::p2p", %err, "Rejected malformed peer block"),
                        }
                    }

                    // Backfill completed, flip to live blocks
                    P2pEvent::BackfillCompleted { peer, tip, tip_hash, tip_ack } => {
                        if role != zone_p2p::Role::Follower {
                            continue;
                        }
                        let target = alloy_eips::BlockNumHash { number: tip, hash: tip_hash };
                        if let Err(error) = verify_backfill_tip(
                            &peer,
                            target,
                            &tip_ack,
                            attestation.domain,
                            &attestation.addresses,
                        ) {
                            tracing::warn!(target: "zone::p2p", %peer, %error, tip, %tip_hash, "Rejected backfill completion without a valid signed tip");
                            continue;
                        }
                        let best = match provider.best_block_number() {
                            Ok(best) => best,
                            Err(err) => {
                                tracing::error!(target: "zone::p2p", %err, "Failed reading local head after backfill response");
                                continue;
                            }
                        };
                        if best >= target.number {
                            match provider.sealed_header(target.number) {
                                Ok(Some(header)) if header.hash() == target.hash => {}
                                Ok(Some(header)) => {
                                    tracing::error!(target: "zone::p2p", %peer, tip = target.number, expected = %target.hash, actual = %header.hash(), "Signed backfill tip conflicts with local canonical state");
                                    backfill.needed = true;
                                    continue;
                                }
                                Ok(None) => {
                                    tracing::error!(target: "zone::p2p", %peer, tip = target.number, "Signed backfill tip is missing locally");
                                    backfill.needed = true;
                                    continue;
                                }
                                Err(error) => {
                                    tracing::error!(target: "zone::p2p", %peer, %error, tip = target.number, "Failed reading signed backfill tip locally");
                                    backfill.needed = true;
                                    continue;
                                }
                            }
                        }
                        backfill.complete(
                            target.number,
                            best,
                            pending.first_key_value().map(|(&number, _)| number),
                        );
                        debug!(target: "zone::p2p", %peer, best, tip = target.number, tip_hash = %target.hash, backfill_needed = backfill.needed, "Completed block backfill response page");
                    }
                }
            }
            _ = retry.tick(), if backfill.needed && role == zone_p2p::Role::Follower => {
                let best = match provider.best_block_number() {
                    Ok(best) => best,
                    Err(err) => {
                        tracing::error!(target: "zone::p2p", %err, "Failed reading local head for backfill request");
                        continue;
                    }
                };

                // retry the backfill
                if let Some(command) = backfill.request(best)
                    && commands.send(command).await.is_err()
                {
                    debug!(target: "zone::p2p", "P2P command channel closed");
                    return;
                }
            }
            _ = &mut inactivity, if !backfill.needed && role == zone_p2p::Role::Follower => {
                // Reset before reading the provider so a transient provider error cannot leave an
                // elapsed sleep continuously ready and spin this loop.
                inactivity
                    .as_mut()
                    .reset(tokio::time::Instant::now() + BLOCK_INACTIVITY_TIMEOUT);
                let best = match provider.best_block_number() {
                    Ok(best) => best,
                    Err(err) => {
                        tracing::error!(target: "zone::p2p", %err, "Failed reading local head for inactivity backfill probe");
                        continue;
                    }
                };
                let command = backfill.probe_after_inactivity(best);
                info!(target: "zone::p2p", best, "No peer block received recently; probing for backfill");
                if commands.send(command).await.is_err() {
                    debug!(target: "zone::p2p", "P2P command channel closed");
                    return;
                }
            }
            result = &mut backfill_server.0 => {
                match result {
                    Ok(()) => tracing::error!(target: "zone::p2p", "Block backfill server stopped unexpectedly"),
                    Err(err) => tracing::error!(target: "zone::p2p", %err, "Block backfill server task failed"),
                }
                return;
            }
            result = &mut settlement_signer.0 => {
                match result {
                    Ok(()) => tracing::error!(target: "zone::p2p", "Settlement signing worker stopped unexpectedly"),
                    Err(err) => tracing::error!(target: "zone::p2p", %err, "Settlement signing worker failed"),
                }
                return;
            }
        }
    }
}

async fn drain_pending_blocks<P>(
    provider: &P,
    engine: &reth_node_builder::ConsensusEngineHandle<ZonePayloadTypes>,
    l1_block_tracker: &L1BlockTracker,
    policy_cache: &PolicyCache,
    pending: &mut BTreeMap<u64, Vec<u8>>,
    pending_bytes: &mut usize,
) -> eyre::Result<()>
where
    P: reth_storage_api::BlockNumReader
        + reth_storage_api::HeaderProvider<Header = TempoHeader>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    loop {
        let next = provider.best_block_number()?.saturating_add(1);
        let Some(block) = pending.remove(&next) else {
            return Ok(());
        };
        *pending_bytes = pending_bytes.saturating_sub(block.len());
        import_peer_block(provider, engine, l1_block_tracker, policy_cache, &block).await?;
    }
}

async fn import_peer_block<P>(
    provider: &P,
    engine: &reth_node_builder::ConsensusEngineHandle<ZonePayloadTypes>,
    l1_block_tracker: &L1BlockTracker,
    policy_cache: &PolicyCache,
    encoded: &[u8],
) -> eyre::Result<()>
where
    P: BlockNumReader
        + HeaderProvider<Header = TempoHeader>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    // Check the received block
    let mut input = encoded;
    let block = Block::decode(&mut input)
        .map_err(|err| eyre::eyre!("invalid RLP-encoded zone block: {err}"))?;
    if !input.is_empty() {
        eyre::bail!("encoded zone block has {} trailing bytes", input.len());
    }

    let block = SealedBlock::seal_slow(block);
    let block_number = block.number();
    let hash = block.hash();
    let best_block = provider.best_block_number()?;

    // 1. Block number is correct
    if block_number <= best_block {
        let existing = provider.sealed_header(block_number)?.ok_or_else(|| {
            eyre::eyre!("missing local canonical header at height {block_number}")
        })?;
        if existing.hash() == hash {
            debug!(target: "zone::p2p", block_number, ?hash, "Ignoring duplicate peer block");
            return Ok(());
        }
        eyre::bail!(
            "peer block conflicts with canonical block at height {block_number}: local={}, received={hash}",
            existing.hash()
        );
    }

    let expected_number = best_block.saturating_add(1);
    if block_number != expected_number {
        eyre::bail!(
            "peer block gap: local head is {best_block}, received height {block_number}, expected {expected_number}"
        );
    }

    // 2. Block's parent hash is correct
    let parent = provider
        .sealed_header(best_block)?
        .ok_or_else(|| eyre::eyre!("missing local canonical head at height {best_block}"))?;
    if block.parent_hash() != parent.hash() {
        eyre::bail!(
            "peer block parent mismatch at height {block_number}: local={}, received={}",
            parent.hash(),
            block.parent_hash()
        );
    }

    // 3. Require the block to advance the local Tempo checkpoint by exactly
    // one independently observed L1 block.
    let l1_header = decode_advance_tempo_header(&block)?;
    let local = provider
        .state_by_block_hash(parent.hash())?
        .tempo_num_hash()?;
    validate_l1_checkpoint_transition(&l1_header, local.number, local.hash, block_number)?;
    let anchor = l1_header.num_hash();
    loop {
        match tokio::time::timeout(Duration::from_secs(30), l1_block_tracker.wait_for(anchor)).await
        {
            Ok(observed) => break observed?,
            Err(_) => warn!(
                target: "zone::p2p",
                block_number,
                l1_block = anchor.number,
                l1_hash = ?anchor.hash,
                "Peer block import is waiting for local L1 observation of its anchor"
            ),
        }
    }

    // 4. All txns in the block execute properly
    let payload = ZonePayloadTypes::block_to_payload(block, None);
    let status = engine.new_payload(payload).await?;
    if !status.is_valid() {
        eyre::bail!("execution engine rejected peer block {block_number} ({hash}): {status:?}");
    }

    // 5. Forkchoice
    let forkchoice = ForkchoiceState::same_hash(hash);
    let result = engine.fork_choice_updated(forkchoice, None).await?;
    if !result.is_valid() {
        eyre::bail!(
            "execution engine rejected forkchoice for block {block_number} ({hash}): {result:?}"
        );
    }

    // Mirror the leader engine only after the block is canonical locally.
    policy_cache.advance(anchor.number);
    l1_block_tracker.prune_through(anchor.number);

    info!(target: "zone::p2p", block_number, ?hash, "Imported canonical leader block");
    Ok(())
}

fn validate_l1_checkpoint_transition(
    l1_header: &SealedHeader<TempoHeader>,
    local_number: u64,
    local_hash: B256,
    zone_block_number: u64,
) -> eyre::Result<()> {
    if l1_header.number() != local_number.saturating_add(1) {
        eyre::bail!(
            "peer block {zone_block_number} advances Tempo to L1 block {}, but local checkpoint is {}; expected {}",
            l1_header.number(),
            local_number,
            local_number.saturating_add(1)
        );
    }
    if l1_header.parent_hash() != local_hash {
        eyre::bail!(
            "advanceTempo L1 header {} does not extend the local Tempo checkpoint: embedded parent {}, local hash {}",
            l1_header.number(),
            l1_header.parent_hash(),
            local_hash
        );
    }
    Ok(())
}

/// Decode the L1 header embedded in the first `ZoneInbox.advanceTempo` system transaction.
fn decode_advance_tempo_header(
    block: &SealedBlock<Block>,
) -> eyre::Result<SealedHeader<TempoHeader>> {
    let first_tx = block.body().transactions().next().ok_or_else(|| {
        eyre::eyre!("peer block has no transactions; expected an advanceTempo system tx")
    })?;
    let TempoTxEnvelope::Legacy(signed) = first_tx else {
        eyre::bail!("first transaction in peer block is not a legacy system transaction")
    };
    if !first_tx.is_system_tx() {
        eyre::bail!("first transaction in peer block is not a Tempo system transaction")
    }
    if signed.tx().to != ZONE_INBOX_ADDRESS.into() {
        eyre::bail!("first Tempo system transaction is not sent to ZoneInbox")
    }
    let call = ZoneInbox::advanceTempoCall::abi_decode(signed.tx().input.as_ref())
        .map_err(|err| eyre::eyre!("first transaction does not decode as advanceTempo: {err}"))?;

    let mut header_rlp = call.header.as_ref();
    let header = TempoHeader::decode(&mut header_rlp)
        .map_err(|err| eyre::eyre!("invalid RLP-encoded L1 header in advanceTempo: {err}"))?;
    if !header_rlp.is_empty() {
        eyre::bail!(
            "advanceTempo L1 header has {} trailing bytes",
            header_rlp.len()
        );
    }
    Ok(SealedHeader::seal_slow(header))
}

/// Build the signed statement returned only when serving an on-demand backfill page.
fn build_backfill_ack<P>(
    provider: &P,
    number: u64,
    domain: AttestationDomain,
) -> eyre::Result<BlockAck>
where
    P: HeaderProvider<Header = TempoHeader>,
{
    let header = provider
        .sealed_header(number)?
        .ok_or_else(|| eyre::eyre!("missing canonical header at attested height {number}"))?;
    Ok(BlockAck::new(domain, number, header.hash()))
}

/// A node must have persisted a block before using it in a backfill proof.
fn ensure_block_receipts_persisted<P>(provider: &P, number: u64) -> eyre::Result<()>
where
    P: ReceiptProvider,
{
    provider
        .receipts_by_block(BlockHashOrNumber::Number(number))?
        .ok_or_else(|| eyre::eyre!("receipts for canonical block {number} are not persisted"))?;
    Ok(())
}

fn signing_refusal(
    target: alloy_eips::BlockNumHash,
    attestation: &AttestationContext,
) -> eyre::Result<P2pCommand> {
    let refusal = SigningRefusal::new(attestation.domain, target);
    let signed = SignedSigningRefusal::sign(refusal, attestation.domain, &attestation.signer)?;
    Ok(P2pCommand::RefuseToSign(signed.encode()))
}

fn verify_signing_refusal(
    encoded: &[u8],
    domain: AttestationDomain,
    expected_signer: alloy_primitives::Address,
) -> eyre::Result<alloy_eips::BlockNumHash> {
    let signed = SignedSigningRefusal::decode(encoded)?;
    eyre::ensure!(
        signed.recover_signer(domain)? == expected_signer,
        "refusal signer does not match authenticated peer"
    );
    signed.refusal.target(domain)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use futures::{StreamExt as _, stream};

    use super::{
        BackfillProgress, EncodedPersistedBlock, MAX_PENDING_BLOCKS, PersistedBlockSource,
        PersistedTip, broadcast_persisted_blocks, buffer_pending_block, verify_backfill_tip,
        verify_signing_refusal,
    };
    use alloy_primitives::{Address, B256};
    use alloy_signer_local::PrivateKeySigner;
    use commonware_cryptography::{Signer as _, ed25519::PrivateKey};
    use zone_p2p::P2pCommand;
    use zone_sequencer::attestation::{
        AttestationDomain, BlockAck, SignedBlockAck, SignedSigningRefusal, SigningRefusal,
    };

    fn attestation_domain() -> AttestationDomain {
        AttestationDomain {
            l1_chain_id: 1,
            portal_address: Address::repeat_byte(0xee),
            zone_id: 7,
            sequencer_set_version: 1,
        }
    }

    #[test]
    fn backfill_tip_requires_an_exact_member_ack() {
        let peer = PrivateKey::from_seed(1).public_key();
        let signer = PrivateKeySigner::random();
        let addresses = HashMap::from([(peer.clone(), signer.address())]);
        let domain = attestation_domain();
        let canonical_tip = alloy_eips::BlockNumHash {
            number: 7,
            hash: B256::repeat_byte(7),
        };
        let completion_ack = SignedBlockAck::sign(
            BlockAck::new(domain, canonical_tip.number, canonical_tip.hash),
            domain,
            &signer,
        )
        .unwrap()
        .encode();
        verify_backfill_tip(&peer, canonical_tip, &completion_ack, domain, &addresses).unwrap();
        assert!(
            verify_backfill_tip(
                &peer,
                alloy_eips::BlockNumHash {
                    number: 7,
                    hash: B256::repeat_byte(0xff),
                },
                &completion_ack,
                domain,
                &addresses,
            )
            .is_err()
        );
    }

    #[test]
    fn terminal_refusal_requires_the_authenticated_members_signature() {
        let signer = PrivateKeySigner::random();
        let other = PrivateKeySigner::random();
        let domain = attestation_domain();
        let target = alloy_eips::BlockNumHash {
            number: 7,
            hash: B256::repeat_byte(7),
        };
        let encoded =
            SignedSigningRefusal::sign(SigningRefusal::new(domain, target), domain, &signer)
                .unwrap()
                .encode();

        assert_eq!(
            verify_signing_refusal(&encoded, domain, signer.address()).unwrap(),
            target
        );
        assert!(verify_signing_refusal(&encoded, domain, other.address()).is_err());
    }

    #[derive(Clone)]
    struct StartupRaceSource {
        reads: Arc<AtomicUsize>,
        tip: PersistedTip,
    }

    impl PersistedBlockSource for StartupRaceSource {
        fn last_block_number(&self) -> eyre::Result<u64> {
            let read = self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(if read == 0 {
                self.tip.number - 1
            } else {
                self.tip.number
            })
        }

        fn persisted_block_stream(&self) -> futures::stream::BoxStream<'static, PersistedTip> {
            stream::iter([self.tip]).boxed()
        }

        fn encoded_block_by_number(&self, number: u64) -> eyre::Result<EncodedPersistedBlock> {
            assert_eq!(number, self.tip.number);
            Ok(EncodedPersistedBlock {
                number,
                hash: self.tip.hash,
                encoded: vec![number as u8],
            })
        }
    }

    #[tokio::test]
    async fn broadcasts_block_persisted_during_startup_reconciliation_once() {
        let source = StartupRaceSource {
            reads: Arc::new(AtomicUsize::new(0)),
            tip: PersistedTip {
                number: 1,
                hash: B256::repeat_byte(0x11),
            },
        };
        let (commands, mut command_rx) = tokio::sync::mpsc::channel(4);

        broadcast_persisted_blocks(source, commands).await;

        assert_eq!(
            command_rx.recv().await,
            Some(P2pCommand::BroadcastBlock(vec![1]))
        );
        assert_eq!(command_rx.recv().await, None);
    }

    #[test]
    fn requests_backfill_again_when_live_block_reveals_gap_after_completion() {
        const LOCAL_HEAD: u64 = 10;

        let mut backfill = BackfillProgress::new();
        assert_eq!(
            backfill.request(LOCAL_HEAD),
            Some(P2pCommand::RequestBackfill {
                start: LOCAL_HEAD + 1,
            })
        );

        // The first response catches the follower up to the responder's snapshot tip.
        backfill.complete(LOCAL_HEAD, LOCAL_HEAD, None);
        assert_eq!(backfill.request(LOCAL_HEAD), None);

        // N+1's live broadcast is missed, then N+2 arrives and remains buffered behind the gap.
        let live_block = LOCAL_HEAD + 2;
        backfill.observe_block(live_block, LOCAL_HEAD);
        backfill.refresh_after_import(LOCAL_HEAD, Some(live_block));

        // The retry starts at the missing N+1 rather than skipping to the received N+2.
        assert_eq!(
            backfill.request(LOCAL_HEAD),
            Some(P2pCommand::RequestBackfill {
                start: LOCAL_HEAD + 1,
            })
        );
    }

    #[test]
    fn inactivity_probe_restarts_backfill_retries() {
        const LOCAL_HEAD: u64 = 10;

        let mut backfill = BackfillProgress::new();
        backfill.complete(LOCAL_HEAD, LOCAL_HEAD, None);
        assert_eq!(backfill.request(LOCAL_HEAD), None);

        assert_eq!(
            backfill.probe_after_inactivity(LOCAL_HEAD),
            P2pCommand::RequestBackfill {
                start: LOCAL_HEAD + 1,
            }
        );
        assert_eq!(
            backfill.request(LOCAL_HEAD),
            Some(P2pCommand::RequestBackfill {
                start: LOCAL_HEAD + 1,
            })
        );
    }

    #[test]
    fn pending_block_limit_keeps_blocks_closest_to_local_head() {
        let mut pending = std::collections::BTreeMap::new();
        let mut pending_bytes = 0;
        for number in 100..100 + MAX_PENDING_BLOCKS as u64 {
            assert_eq!(
                buffer_pending_block(&mut pending, &mut pending_bytes, number, vec![number as u8],),
                None
            );
        }
        assert_eq!(pending.len(), MAX_PENDING_BLOCKS);

        let farthest = 100 + MAX_PENDING_BLOCKS as u64 - 1;
        assert_eq!(
            buffer_pending_block(&mut pending, &mut pending_bytes, 99, vec![99]),
            Some(farthest)
        );
        assert_eq!(pending.len(), MAX_PENDING_BLOCKS);
        assert!(pending.contains_key(&99));
        assert!(!pending.contains_key(&farthest));

        let farther = farthest + 1;
        assert_eq!(
            buffer_pending_block(
                &mut pending,
                &mut pending_bytes,
                farther,
                vec![farther as u8],
            ),
            Some(farther)
        );
        assert_eq!(pending.len(), MAX_PENDING_BLOCKS);
        assert!(!pending.contains_key(&farther));
    }
}
