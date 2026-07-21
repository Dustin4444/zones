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
    collections::{BTreeMap, HashMap},
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

use crate::settlement_attestation::build_settlement_attestation;
use alloy_signer_local::PrivateKeySigner;
use zone_sequencer::attestation::{
    AttestationDomain, AttestationStore, BlockAck, SettlementAttestation, SignedBlockAck,
    SignedSettlementAttestation,
};

#[derive(Clone)]
/// Shared context for both per-block ACKs and batch-boundary settlement attestations.
pub(crate) struct AttestationContext {
    pub(crate) domain: AttestationDomain,
    pub(crate) signer: PrivateKeySigner,
    pub(crate) addresses: HashMap<zone_p2p::P2pPeerId, alloy_primitives::Address>,
    pub(crate) store: Option<AttestationStore>,
    pub(crate) l1_provider: DynProvider<TempoNetwork>,
    pub(crate) anchor_config: BatchAnchorConfig,
}

impl AttestationContext {
    pub(crate) fn new(
        domain: AttestationDomain,
        signer: PrivateKeySigner,
        addresses: HashMap<zone_p2p::P2pPeerId, alloy_primitives::Address>,
        store: Option<AttestationStore>,
        l1_provider: DynProvider<TempoNetwork>,
        anchor_config: BatchAnchorConfig,
    ) -> Self {
        Self {
            domain,
            signer,
            addresses,
            store,
            l1_provider,
            anchor_config,
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
const BACKFILL_PAGE_SIZE: u64 = 64;
const BACKFILL_SERVE_QUEUE_CAPACITY: usize = 8;

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
    number: u64,
    block: Vec<u8>,
) -> Option<u64> {
    if pending.contains_key(&number) {
        return None;
    }
    if pending.len() < MAX_PENDING_BLOCKS {
        pending.insert(number, block);
        return None;
    }

    let Some((&farthest, _)) = pending.last_key_value() else {
        pending.insert(number, block);
        return None;
    };
    if number < farthest {
        pending.pop_last();
        pending.insert(number, block);
        Some(farthest)
    } else {
        Some(number)
    }
}

fn encoded_block_number(encoded: &[u8]) -> eyre::Result<u64> {
    let mut input = encoded;
    let block = Block::decode(&mut input)
        .map_err(|err| eyre::eyre!("invalid RLP-encoded zone block: {err}"))?;
    eyre::ensure!(
        input.is_empty(),
        "encoded zone block has {} trailing bytes",
        input.len()
    );
    Ok(block.header.number())
}

fn serve_backfill_page<P>(
    provider: &P,
    commands: &mpsc::Sender<P2pCommand>,
    peer: zone_p2p::P2pPeerId,
    request_id: u64,
    start: u64,
) -> eyre::Result<()>
where
    P: BlockNumReader + BlockReader<Block = Block>,
{
    let tip = provider.best_block_number()?;
    let end = tip.min(start.saturating_add(BACKFILL_PAGE_SIZE.saturating_sub(1)));
    for number in start..=end {
        let block = provider.block_by_number(number)?.ok_or_else(|| {
            eyre::eyre!("canonical block {number} is missing while serving backfill")
        })?;
        commands
            .blocking_send(P2pCommand::SendBackfillBlock {
                peer: peer.clone(),
                request_id,
                block: alloy_rlp::encode(block),
            })
            .map_err(|_| eyre::eyre!("P2P command channel closed"))?;
    }
    commands
        .blocking_send(P2pCommand::CompleteBackfill {
            peer,
            request_id,
            tip,
        })
        .map_err(|_| eyre::eyre!("P2P command channel closed"))?;
    Ok(())
}

async fn serve_backfill_requests<P>(
    provider: P,
    commands: mpsc::Sender<P2pCommand>,
    mut requests: mpsc::Receiver<BackfillRequest>,
) where
    P: BlockNumReader + BlockReader<Block = Block> + Clone + Send + Sync + 'static,
{
    // One worker deliberately serializes page construction and sending, bounding serving
    // concurrency independently of the block import loop.
    while let Some(BackfillRequest {
        peer,
        request_id,
        start,
    }) = requests.recv().await
    {
        let page_provider = provider.clone();
        let page_commands = commands.clone();
        let page = tokio::task::spawn_blocking(move || {
            serve_backfill_page(&page_provider, &page_commands, peer, request_id, start)
        })
        .await;
        let result = match page {
            Ok(result) => result,
            Err(err) => Err(eyre::eyre!("backfill page worker failed: {err}")),
        };
        if let Err(err) = result {
            tracing::error!(target: "zone::p2p", %err, start, "Failed serving block backfill");
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
    let mut backfill = BackfillProgress::new();
    let mut backfilled_from: Option<u64> = None;
    let (backfill_requests, backfill_request_rx) = mpsc::channel(BACKFILL_SERVE_QUEUE_CAPACITY);

    // Serve backfill requests in a separate task to avoid competing the live blocks
    let mut backfill_server = BackfillServerTask(tokio::spawn(serve_backfill_requests(
        provider.clone(),
        commands.clone(),
        backfill_request_rx,
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

                    // Leader received a Block Ack
                    P2pEvent::BlockAckReceived { follower, ack } => {
                        if role != zone_p2p::Role::Leader {
                            tracing::warn!(target: "zone::p2p", %follower, "Ignoring block ACK event on follower");
                            continue;
                        }
                        let Some(expected_signer) = attestation.addresses.get(&follower).copied() else {
                            tracing::warn!(target: "zone::p2p", %follower, "Rejected block attestation from unknown follower");
                            continue;
                        };

                        match verify_block_ack(
                            &provider,
                            &ack,
                            attestation.domain,
                            expected_signer,
                        ) {
                            Ok((signer, height)) => info!(target: "zone::p2p", %follower, %signer, height, "Verified follower block ACK"),
                            Err(err) => tracing::warn!(target: "zone::p2p", %follower, %err, "Rejected follower block ACK"),
                        }
                    }

                    // Followers receive SettlementProposals from the Leader and batch boundaries, which they verify
                    // and if everything is correct, sign and return back to the leader.
                    P2pEvent::SettlementProposalReceived { leader, proposal } => {
                        if role != zone_p2p::Role::Follower {
                            continue;
                        }

                        let result = async {
                            let proposal = SettlementAttestation::decode(&proposal)?;
                            let height: u64 = proposal.zoneHeight.try_into()
                                .map_err(|_| eyre::eyre!("settlement height does not fit in u64"))?;

                            // 1. Build the attestation in the exact format ZonePortal expects
                            let expected = build_settlement_attestation(
                                &provider,
                                height,
                                &attestation,
                                Some((proposal.anchorBlockNumber, proposal.anchorBlockHash)),
                            ).await?.ok_or_else(|| eyre::eyre!("proposed block is not a batch boundary"))?;
                            eyre::ensure!(proposal == expected, "settlement proposal does not match follower state");

                            // 2. Sign it.
                            let signed = SignedSettlementAttestation::sign(
                                proposal,
                                attestation.domain,
                                &attestation.signer,
                            )?;

                            // 3. Send it back to leader
                            commands.send(P2pCommand::SendSettlementSignature(signed.encode()))
                                .await
                                .map_err(|_| eyre::eyre!("P2P command channel closed"))?;
                            Ok::<_, eyre::Report>(height)
                        }.await;
                        match result {
                            Ok(height) => info!(target: "zone::p2p", %leader, height, "Signed settlement proposal"),
                            Err(err) => tracing::warn!(target: "zone::p2p", %leader, %err, "Rejected settlement proposal"),
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
                            let expected = build_settlement_attestation(
                                &provider,
                                height,
                                &attestation,
                                Some((signed.attestation.anchorBlockNumber, signed.attestation.anchorBlockHash)),
                            ).await?.ok_or_else(|| eyre::eyre!("signed block is not a batch boundary"))?;
                            eyre::ensure!(signed.attestation == expected, "settlement signature does not match leader state");

                            let signer = signed.recover_signer(attestation.domain)?;
                            let expected_signer = attestation.addresses.get(&follower)
                                .copied()
                                .ok_or_else(|| eyre::eyre!("unknown follower identity"))?;
                            eyre::ensure!(signer == expected_signer, "settlement signer does not match authenticated peer");

                            // Store away the signature, we'll need this to `submitBatch`
                            let (_, signatures) = attestation
                                .store
                                .as_ref()
                                .expect("leader must have an attestation store")
                                .insert_settlement(attestation.domain, signer, signed);
                            Ok::<_, eyre::Report>((height, signer, signatures))
                        }.await;
                        match result {
                            Ok((height, signer, signatures)) => info!(target: "zone::p2p", %follower, %signer, height, signatures, "Stored follower settlement signature"),
                            Err(err) => tracing::warn!(target: "zone::p2p", %follower, %err, "Rejected follower settlement signature"),
                        }
                    }

                    // Leaders and Followers can receive and serve block backfill requests
                    P2pEvent::BackfillRequested { peer, request_id, start } => {
                        if let Err(err) = backfill_requests.try_send(BackfillRequest { peer, request_id, start }) {
                            tracing::warn!(target: "zone::p2p", %err, start, queue_capacity = BACKFILL_SERVE_QUEUE_CAPACITY, "Dropped block backfill request because the serving queue is unavailable");
                        }
                    }

                    // Received a live || backfilled block.
                    event @ (P2pEvent::BlockReceived { .. } | P2pEvent::BackfillBlockReceived { .. }) => {
                        let (block, from_backfill) = match event {
                            P2pEvent::BlockReceived { block, .. } => (block, false),
                            P2pEvent::BackfillBlockReceived { block, .. } => (block, true),
                            _ => unreachable!(),
                        };
                        match encoded_block_number(&block) {
                            Ok(number) => {
                                if from_backfill {
                                    backfilled_from = Some(backfilled_from.map_or(number, |first| first.min(number)));
                                }
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
                                    } else if role == zone_p2p::Role::Follower && !from_backfill {
                                        // For live blocks, ACK the block. The leader needs n-of-m quorum to sign
                                        // a batch before it can call `submitBatch`
                                        send_block_ack(
                                            &provider,
                                            number,
                                            attestation.domain,
                                            &attestation.signer,
                                            &commands,
                                        )
                                        .await;
                                    }
                                    continue;
                                }
                                backfill.observe_block(number, best);
                                if let Some(dropped) = buffer_pending_block(&mut pending, number, block) {
                                    tracing::warn!(target: "zone::p2p", dropped, pending_limit = MAX_PENDING_BLOCKS, "Dropped far-future peer block because the pending block buffer is full");
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
                                    if role == zone_p2p::Role::Follower
                                        && !from_backfill
                                        && backfilled_from.is_none()
                                    {
                                        for imported in best.saturating_add(1)..=new_best {
                                            // For live blocks, ACK the block. The leader needs n-of-m quorum to sign
                                            // a batch before it can call `submitBatch`
                                            send_block_ack(
                                                &provider,
                                                imported,
                                                attestation.domain,
                                                &attestation.signer,
                                                &commands,
                                            )
                                            .await;
                                        }
                                    }
                                }
                            }
                            Err(err) => tracing::error!(target: "zone::p2p", %err, "Rejected malformed peer block"),
                        }
                    }

                    // Backfill completed, flip to live blocks
                    P2pEvent::BackfillCompleted { peer, tip } => {
                        let best = match provider.best_block_number() {
                            Ok(best) => best,
                            Err(err) => {
                                tracing::error!(target: "zone::p2p", %err, "Failed reading local head after backfill response");
                                continue;
                            }
                        };
                        backfill.complete(
                            tip,
                            best,
                            pending.first_key_value().map(|(&number, _)| number),
                        );
                        if role == zone_p2p::Role::Follower
                            && let Some(first) = backfilled_from.take()
                        {
                            let recent_start = tip.saturating_sub(119).max(first);
                            for imported in recent_start..=best.min(tip) {
                                send_block_ack(
                                    &provider,
                                    imported,
                                    attestation.domain,
                                    &attestation.signer,
                                    &commands,
                                )
                                .await;
                            }
                        }
                        debug!(target: "zone::p2p", %peer, best, tip, backfill_needed = backfill.needed, "Completed block backfill response page");
                    }
                }
            }
            _ = retry.tick(), if backfill.needed => {
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
            _ = &mut inactivity, if !backfill.needed => {
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
        }
    }
}

async fn drain_pending_blocks<P>(
    provider: &P,
    engine: &reth_node_builder::ConsensusEngineHandle<ZonePayloadTypes>,
    l1_block_tracker: &L1BlockTracker,
    policy_cache: &PolicyCache,
    pending: &mut BTreeMap<u64, Vec<u8>>,
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

/// Followers ACK blocks received from a leader
fn build_block_ack<P>(
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

/// A follower needs to have persisted a block first before ACK-ing it.
fn ensure_block_receipts_persisted<P>(provider: &P, number: u64) -> eyre::Result<()>
where
    P: ReceiptProvider,
{
    provider
        .receipts_by_block(BlockHashOrNumber::Number(number))?
        .ok_or_else(|| eyre::eyre!("receipts for canonical block {number} are not persisted"))?;
    Ok(())
}

/// A follower will send a block ACK back to the leader after verifying and persisting it
async fn send_block_ack<P>(
    provider: &P,
    number: u64,
    domain: AttestationDomain,
    signer: &PrivateKeySigner,
    commands: &mpsc::Sender<P2pCommand>,
) where
    P: HeaderProvider<Header = TempoHeader> + ReceiptProvider,
{
    // FCU makes the block canonical before Reth necessarily exposes its durable receipts.
    // Wait for those receipts so an ACK is sent only after the block and its execution output
    // have crossed the persistence boundary configured for manifest mode.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let signed = loop {
        // save, build and sign a received live block
        let result = ensure_block_receipts_persisted(provider, number)
            .and_then(|_| build_block_ack(provider, number, domain))
            .and_then(|ack| SignedBlockAck::sign(ack, domain, signer));

        match result {
            Ok(signed) => break signed,
            Err(err) if tokio::time::Instant::now() < deadline => {
                debug!(target: "zone::p2p", %err, number, "Waiting for imported block receipts before attesting");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => {
                tracing::error!(target: "zone::p2p", %err, number, "Failed building follower block attestation after persistence timeout");
                return;
            }
        }
    };

    if commands
        .send(P2pCommand::SendBlockAck(signed.encode()))
        .await
        .is_err()
    {
        debug!(target: "zone::p2p", "P2P command channel closed before block ACK could be sent");
    }
}

/// Verify that a follower signed an ACK for the leader's canonical block at that height.
fn verify_block_ack<P>(
    provider: &P,
    encoded: &[u8],
    domain: AttestationDomain,
    expected_signer: alloy_primitives::Address,
) -> eyre::Result<(alloy_primitives::Address, u64)>
where
    P: HeaderProvider<Header = TempoHeader> + ReceiptProvider,
{
    let signed = SignedBlockAck::decode(encoded)?;
    let height: u64 = signed
        .ack
        .zoneHeight
        .try_into()
        .map_err(|_| eyre::eyre!("attested zone height does not fit in u64"))?;
    let expected = build_block_ack(provider, height, domain)?;
    eyre::ensure!(
        signed.ack == expected,
        "block ACK statement does not match the leader's canonical block"
    );
    let signer = signed.recover_signer(domain)?;
    eyre::ensure!(
        signer == expected_signer,
        "block ACK signer {signer} does not match authenticated peer address {expected_signer}"
    );
    Ok((signer, height))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use futures::{StreamExt as _, stream};

    use super::{
        BackfillProgress, EncodedPersistedBlock, MAX_PENDING_BLOCKS, PersistedBlockSource,
        PersistedTip, broadcast_persisted_blocks, buffer_pending_block,
    };
    use alloy_primitives::B256;
    use zone_p2p::P2pCommand;

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
        for number in 100..100 + MAX_PENDING_BLOCKS as u64 {
            assert_eq!(
                buffer_pending_block(&mut pending, number, vec![number as u8]),
                None
            );
        }
        assert_eq!(pending.len(), MAX_PENDING_BLOCKS);

        let farthest = 100 + MAX_PENDING_BLOCKS as u64 - 1;
        assert_eq!(
            buffer_pending_block(&mut pending, 99, vec![99]),
            Some(farthest)
        );
        assert_eq!(pending.len(), MAX_PENDING_BLOCKS);
        assert!(pending.contains_key(&99));
        assert!(!pending.contains_key(&farthest));

        let farther = farthest + 1;
        assert_eq!(
            buffer_pending_block(&mut pending, farther, vec![farther as u8]),
            Some(farther)
        );
        assert_eq!(pending.len(), MAX_PENDING_BLOCKS);
        assert!(!pending.contains_key(&farther));
    }
}
