//! Node-side leader block replication and follower import.

use alloy_consensus::BlockHeader as _;
use alloy_primitives::B256;
use alloy_rlp::Decodable as _;
use alloy_rpc_types_engine::ForkchoiceState;
use alloy_sol_types::SolCall as _;
use futures::{StreamExt as _, stream::BoxStream};
use reth_chain_state::PersistedBlockSubscriptions;
use reth_node_api::PayloadTypes as _;
use reth_node_builder::ConsensusEngineHandle;
use reth_primitives_traits::{SealedBlock, SealedHeader};
use reth_storage_api::{BlockNumReader, BlockReader, HeaderProvider, StateProviderFactory};
use std::time::Duration;
use tempo_primitives::{Block, TempoHeader, TempoTxEnvelope};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zone_l1::{L1BlockObserver, PolicyCache, TempoStateExt as _};
use zone_p2p::{P2pCommand, P2pEvent};
use zone_payload::{
    ZonePayloadTypes,
    abi::{ZONE_INBOX_ADDRESS, ZoneInbox},
};

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

/// Decode, fully execute, and canonicalize blocks received by a follower.
pub(crate) async fn import_leader_blocks<P>(
    provider: P,
    engine: ConsensusEngineHandle<ZonePayloadTypes>,
    mut events: mpsc::Receiver<P2pEvent>,
    _commands: mpsc::Sender<P2pCommand>,
    l1_observer: L1BlockObserver,
    policy_cache: PolicyCache,
) where
    P: StateProviderFactory
        + BlockNumReader
        + HeaderProvider<Header = TempoHeader>
        + Clone
        + Send
        + Sync
        + 'static,
{
    while let Some(event) = events.recv().await {
        let P2pEvent::BlockReceived { block, .. } = event else {
            continue;
        };
        if let Err(err) =
            import_leader_block(&provider, &engine, &l1_observer, &policy_cache, &block).await
        {
            tracing::error!(target: "zone::p2p", %err, "Rejected leader block");
        }
    }
    debug!(target: "zone::p2p", "P2P event channel closed");
}

async fn import_leader_block<P>(
    provider: &P,
    engine: &ConsensusEngineHandle<ZonePayloadTypes>,
    l1_observer: &L1BlockObserver,
    policy_cache: &PolicyCache,
    encoded: &[u8],
) -> eyre::Result<()>
where
    P: StateProviderFactory
        + BlockNumReader
        + HeaderProvider<Header = TempoHeader>
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
            debug!(target: "zone::p2p", block_number, ?hash, "Ignoring duplicate leader block");
            return Ok(());
        }
        eyre::bail!(
            "leader block conflicts with canonical block at height {block_number}: local={}, received={hash}",
            existing.hash()
        );
    }

    let expected_number = best_block.saturating_add(1);
    if block_number != expected_number {
        eyre::bail!(
            "leader block gap: local head is {best_block}, received height {block_number}, expected {expected_number}; backfill is not implemented yet"
        );
    }

    // 2. Block's parent hash is correct
    let parent = provider
        .sealed_header(best_block)?
        .ok_or_else(|| eyre::eyre!("missing local canonical head at height {best_block}"))?;
    if block.parent_hash() != parent.hash() {
        eyre::bail!(
            "leader block parent mismatch at height {block_number}: local={}, received={}",
            parent.hash(),
            block.parent_hash()
        );
    }

    // 3. The block's advanceTempo system tx advances the local Tempo
    //    checkpoint by exactly one L1 block, and that L1 block has been
    //    independently observed (header + receipts validated, policy and L1
    //    state caches updated) before we execute against it.
    let l1_header = decode_advance_tempo_header(&block)?;
    let local = provider.latest()?.tempo_num_hash()?;
    if l1_header.number() != local.number + 1 {
        eyre::bail!(
            "leader block {block_number} advances Tempo to L1 block {}, but the local checkpoint \
             is {}; expected {}",
            l1_header.number(),
            local.number,
            local.number + 1
        );
    }
    if l1_header.parent_hash() != local.hash {
        eyre::bail!(
            "advanceTempo L1 header {} does not extend the local Tempo checkpoint: \
             embedded parent {}, local hash {}",
            l1_header.number(),
            l1_header.parent_hash(),
            local.hash
        );
    }
    let anchor = l1_header.num_hash();
    loop {
        match tokio::time::timeout(Duration::from_secs(30), l1_observer.wait_for(anchor)).await {
            Ok(observed) => break observed?,
            Err(_elapsed) => warn!(
                target: "zone::p2p",
                block_number,
                l1_block = anchor.number,
                l1_hash = ?anchor.hash,
                "Leader block import is waiting for local L1 observation of its anchor"
            ),
        }
    }

    // 4. All txns in the block execute properly
    let payload = ZonePayloadTypes::block_to_payload(block, None);
    let status = engine.new_payload(payload).await?;
    if !status.is_valid() {
        eyre::bail!("execution engine rejected leader block {block_number} ({hash}): {status:?}");
    }

    // Mirror the leader engine: fold observed policy deltas up to the consumed
    // anchor so the next block's execution resolves policy state at its
    // parent's L1 height.
    policy_cache.advance(anchor.number);

    // 5. Forkchoice
    let forkchoice = ForkchoiceState::same_hash(hash);
    let result = engine.fork_choice_updated(forkchoice, None).await?;
    if !result.is_valid() {
        eyre::bail!(
            "execution engine rejected forkchoice for block {block_number} ({hash}): {result:?}"
        );
    }

    info!(target: "zone::p2p", block_number, ?hash, "Imported canonical leader block");
    Ok(())
}

/// Decode the L1 header embedded in the block's first system transaction
/// (`ZoneInbox.advanceTempo`).
fn decode_advance_tempo_header(
    block: &SealedBlock<Block>,
) -> eyre::Result<SealedHeader<TempoHeader>> {
    // Do some basic checks on the `advanceTempo` txn
    let first_tx = block.body().transactions().next().ok_or_else(|| {
        eyre::eyre!("leader block has no transactions; expected an advanceTempo system tx")
    })?;
    let TempoTxEnvelope::Legacy(signed) = first_tx else {
        eyre::bail!("first transaction in leader block is not a legacy system transaction");
    };
    if !first_tx.is_system_tx() {
        eyre::bail!("first transaction in leader block is not a Tempo system transaction");
    }
    if signed.tx().to != ZONE_INBOX_ADDRESS.into() {
        eyre::bail!("first Tempo system transaction is not sent to ZoneInbox");
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use futures::{StreamExt as _, stream};

    use super::{
        EncodedPersistedBlock, PersistedBlockSource, PersistedTip, broadcast_persisted_blocks,
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

    #[test]
    fn decodes_advance_tempo_header_from_first_system_tx() {
        use alloy_consensus::BlockHeader as _;
        use reth_primitives_traits::{SealedBlock, SealedHeader};
        use tempo_primitives::{Block, TempoHeader};

        let l1_header = TempoHeader {
            inner: alloy_consensus::Header {
                number: 7,
                parent_hash: B256::repeat_byte(0x42),
                ..Default::default()
            },
            ..Default::default()
        };
        let prepared = zone_l1::PreparedL1Block {
            header: SealedHeader::seal_slow(l1_header),
            queued_deposits: vec![],
            decryptions: vec![],
            enabled_tokens: vec![],
        };
        let tx = zone_payload::build_advance_tempo_tx(&prepared);

        let block = SealedBlock::seal_slow(Block {
            header: TempoHeader::default(),
            body: alloy_consensus::BlockBody {
                transactions: vec![tx.into_inner()],
                ommers: vec![],
                withdrawals: None,
            },
        });

        let decoded = super::decode_advance_tempo_header(&block).unwrap();
        assert_eq!(decoded.number(), 7);
        assert_eq!(decoded.parent_hash(), B256::repeat_byte(0x42));
        assert_eq!(decoded.hash(), prepared.header.hash());
    }

    #[test]
    fn rejects_advance_tempo_calldata_not_sent_to_zone_inbox() {
        use alloy_consensus::{Signed, TxLegacy};
        use alloy_primitives::{Address, Bytes, U256};
        use alloy_rlp::Encodable as _;
        use alloy_sol_types::SolCall as _;
        use reth_primitives_traits::SealedBlock;
        use tempo_primitives::{
            Block, TempoHeader, TempoTxEnvelope, transaction::envelope::TEMPO_SYSTEM_TX_SIGNATURE,
        };

        let l1_header = TempoHeader {
            inner: alloy_consensus::Header {
                number: 7,
                parent_hash: B256::repeat_byte(0x42),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut header_rlp = Vec::new();
        l1_header.encode(&mut header_rlp);
        let calldata = zone_payload::abi::ZoneInbox::advanceTempoCall {
            header: Bytes::from(header_rlp),
            deposits: vec![],
            decryptions: vec![],
            enabledTokens: vec![],
        }
        .abi_encode();

        let tx = TxLegacy {
            chain_id: None,
            nonce: 0,
            gas_price: 0,
            gas_limit: 100_000,
            to: Address::repeat_byte(0x99).into(),
            value: U256::ZERO,
            input: calldata.into(),
        };

        let block = SealedBlock::seal_slow(Block {
            header: TempoHeader::default(),
            body: alloy_consensus::BlockBody {
                transactions: vec![TempoTxEnvelope::Legacy(Signed::new_unhashed(
                    tx,
                    TEMPO_SYSTEM_TX_SIGNATURE,
                ))],
                ommers: vec![],
                withdrawals: None,
            },
        });

        let err = super::decode_advance_tempo_header(&block)
            .expect_err("advanceTempo calldata not sent to ZoneInbox must be rejected");
        assert!(err.to_string().contains("ZoneInbox"));
    }

    #[test]
    fn rejects_leader_block_without_advance_tempo_tx() {
        use reth_primitives_traits::SealedBlock;
        use tempo_primitives::{Block, TempoHeader};

        let block = SealedBlock::seal_slow(Block {
            header: TempoHeader::default(),
            body: alloy_consensus::BlockBody {
                transactions: vec![],
                ommers: vec![],
                withdrawals: None,
            },
        });

        let err = super::decode_advance_tempo_header(&block).unwrap_err();
        assert!(err.to_string().contains("no transactions"));
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
}
