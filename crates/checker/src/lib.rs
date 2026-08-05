//! Observe-only Zone L2 checker ExEx.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::needless_borrows_for_generic_args)]

mod l1_facts;
mod l2_facts;

use std::fmt;
use std::str::FromStr;

use alloy_consensus::BlockHeader as _;
use alloy_consensus::Sealable as _;
use alloy_eips::{BlockId, BlockNumHash};
use alloy_network::{BlockResponse as _, primitives::HeaderResponse as _};
use alloy_primitives::{Address, B256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use eyre::WrapErr as _;
use futures::TryStreamExt as _;
use reth_exex::{ExExContext, ExExNotification};
use reth_node_api::{BlockBody as _, FullNodeComponents, NodePrimitives};
use reth_storage_api::{AccountReader, StateProviderFactory};
use tempo_alloy::TempoNetwork;
use tracing::{error, info};

use l1_facts::{authenticate_l1_block, extract_l1_facts, log_l1_facts, verify_l1_receipts};
use l2_facts::{extract_l2_facts, log_l2_facts};

// ---------------------------------------------------------------------------

/// Strictly decode a known event and reject non-canonical encodings that a
/// permissive ABI decoder could otherwise normalize.
///
/// Shared by both L1 and L2 fact extraction so decoding semantics stay
/// identical across layers.
pub(crate) fn decode_event<E: alloy_sol_types::SolEvent>(
    log: &alloy_primitives::Log,
    name: &str,
    block: u64,
) -> eyre::Result<E> {
    let event = E::decode_log_validate(log)
        .wrap_err_with(|| format!("malformed {name} in block {block}"))?;
    eyre::ensure!(
        event.data.encode_log_data() == log.data,
        "non-canonical {name} encoding in block {block}"
    );
    Ok(event.data)
}

// ---------------------------------------------------------------------------
// CLI mode
// ---------------------------------------------------------------------------

/// Runtime mode for the checker ExEx.
///
/// Controls whether the checker is installed in the node builder.
/// Defaults to [`CheckerMode::Off`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckerMode {
    /// Checker is not installed.
    #[default]
    Off,
    /// Checker runs in observe-only mode: logs block identity, confirms
    /// receipts/state availability, and extracts L2 bridge facts — but does
    /// not enforce anything.
    Observe,
}

impl fmt::Display for CheckerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Observe => f.write_str("observe"),
        }
    }
}

impl FromStr for CheckerMode {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "observe" => Ok(Self::Observe),
            other => Err(eyre::eyre!(
                "unsupported checker mode `{other}`, expected `off` or `observe`"
            )),
        }
    }
}

impl CheckerMode {
    /// Parse a checker mode from a string, returning a result compatible with
    /// clap's value parser interface.
    ///
    /// This avoids coupling the checker crate to clap while allowing the CLI
    /// layer to use it as a `value_parser`.
    pub fn parse(s: &str) -> Result<Self, eyre::Report> {
        s.parse()
    }
}

// ---------------------------------------------------------------------------
// ExEx
// ---------------------------------------------------------------------------

/// The checker execution extension.
///
/// The checker holds no runtime state between notifications. It logs
/// observations, confirms data availability, extracts L2 bridge facts, and
/// independently fetches/extracts the exact anchored Tempo/L1 Portal facts as
/// canonical L2 notifications arrive, then acknowledges the processed height.
pub struct CheckerExEx {
    /// Read-only Tempo L1 RPC URL for fetching exact L1 blocks anchored by
    /// `TempoAdvanced`. Passed from the node's `--l1.rpc-url`.
    l1_rpc_url: String,
    /// ZonePortal contract address on L1, from `--l1.portal-address`.
    portal_address: Address,
}

impl CheckerExEx {
    /// Create a checker ExEx instance with the given L1 RPC URL and portal
    /// address.
    pub fn new(l1_rpc_url: String, portal_address: Address) -> Self {
        Self {
            l1_rpc_url,
            portal_address,
        }
    }

    /// Run the ExEx until its notification stream closes.
    ///
    /// Each notification is fully processed — committed blocks oldest-to-newest,
    /// reverted blocks newest-to-oldest, reorgs roll-back-then-apply — before
    /// `send_finished_height` is called. This prevents Reth from pruning or
    /// advancing past a block the checker has not yet observed.
    ///
    /// The L1 provider connection is established lazily on the first
    /// notification that needs it, so a temporarily unavailable L1 RPC at
    /// startup does not prevent the ExEx from running.
    pub async fn run<Node>(self, mut ctx: ExExContext<Node>) -> eyre::Result<()>
    where
        Node: FullNodeComponents,
        Node::Provider: StateProviderFactory,
    {
        info!(target: "zone::checker", "Checker ExEx started");
        let provider = ctx.provider().clone();

        // Lazily-established read-only Tempo L1 provider. Connected on first
        // canonical block so a startup L1 outage does not terminate the ExEx.
        let mut l1_provider: Option<DynProvider<TempoNetwork>> = None;

        let mut acknowledgements_blocked = false;

        while let Some(notification) = ctx.notifications.try_next().await? {
            match process_notification(
                &notification,
                &provider,
                &mut l1_provider,
                &self.l1_rpc_url,
                self.portal_address,
            )
            .await
            {
                Ok(tip) if !acknowledgements_blocked => ctx.send_finished_height(tip)?,
                Ok(_) => {}
                Err(error) => {
                    // Observation failures must not terminate the Zone node. Keep the ExEx
                    // pruning watermark behind the failed notification so restart/replay can
                    // inspect it again instead of silently losing the gap.
                    acknowledgements_blocked = true;
                    error!(target: "zone::checker", %error, "L2/L1 fact extraction failed");
                }
            }
        }

        info!(target: "zone::checker", "Checker ExEx notification stream closed");
        Ok(())
    }
}

/// Process a single canonical L2 ExEx notification and return the finished-height tip.
///
/// Committed and reorged-in blocks are processed oldest-to-newest; each has its
/// receipts and exact post-state confirmed, its L2 bridge facts extracted, and
/// its anchored Tempo/L1 block independently fetched and fact-extracted.
/// Reverted and reorged-out blocks are processed newest-to-oldest and are
/// logged but not checked — their receipts are no longer canonical.
async fn process_notification<N, P>(
    notification: &ExExNotification<N>,
    provider: &P,
    l1_provider: &mut Option<DynProvider<TempoNetwork>>,
    l1_rpc_url: &str,
    portal_address: Address,
) -> eyre::Result<BlockNumHash>
where
    N: NodePrimitives,
    P: StateProviderFactory,
{
    match notification {
        ExExNotification::ChainCommitted { new } => {
            ensure_receipt_sets(new)?;
            for (block, receipts) in new.blocks_and_receipts() {
                let number = block.header().number();
                let hash = block.hash();
                let parent_hash = block.header().parent_hash();
                process_canonical_block(
                    provider,
                    l1_provider,
                    l1_rpc_url,
                    portal_address,
                    number,
                    hash,
                    receipts,
                )
                .await?;
                info!(target: "zone::checker", number, %hash, %parent_hash, "Committed block observed");
            }
            let tip = new.tip();
            Ok(BlockNumHash::new(tip.header().number(), tip.hash()))
        }
        ExExNotification::ChainReverted { old } => {
            for (&number, block) in old.blocks().iter().rev() {
                let hash = block.hash();
                let parent_hash = block.header().parent_hash();
                info!(target: "zone::checker", number, %hash, %parent_hash, "Reverted block observed");
            }
            let (&lowest_num, lowest_block) = old.blocks().iter().next().expect("non-empty chain");
            Ok(BlockNumHash::new(
                lowest_num.saturating_sub(1),
                lowest_block.header().parent_hash(),
            ))
        }
        ExExNotification::ChainReorged { old, new } => {
            // Roll back the old fork newest-to-oldest. Reorged-out blocks are
            // not fact-checked — their receipts are no longer canonical.
            for (&number, block) in old.blocks().iter().rev() {
                let hash = block.hash();
                let parent_hash = block.header().parent_hash();
                info!(target: "zone::checker", number, %hash, %parent_hash, "Reorged-out block observed");
            }
            // Apply the new fork oldest-to-newest using the same extraction
            // path as ordinary committed blocks.
            ensure_receipt_sets(new)?;
            for (block, receipts) in new.blocks_and_receipts() {
                let number = block.header().number();
                let hash = block.hash();
                let parent_hash = block.header().parent_hash();
                process_canonical_block(
                    provider,
                    l1_provider,
                    l1_rpc_url,
                    portal_address,
                    number,
                    hash,
                    receipts,
                )
                .await?;
                info!(target: "zone::checker", number, %hash, %parent_hash, "Reorged-in block observed");
            }
            let tip = new.tip();
            Ok(BlockNumHash::new(tip.header().number(), tip.hash()))
        }
    }
}

/// Ensure every notified block has its corresponding execution receipt set.
fn ensure_receipt_sets<N>(chain: &reth_execution_types::Chain<N>) -> eyre::Result<()>
where
    N: NodePrimitives,
{
    let receipt_sets = chain.block_receipts_iter().count();
    if receipt_sets != chain.blocks().len() {
        eyre::bail!(
            "notification has {} blocks but {receipt_sets} receipt sets",
            chain.blocks().len()
        );
    }
    for (block, receipts) in chain.blocks_and_receipts() {
        let transactions = block.body().transactions().len();
        if receipts.len() != transactions {
            eyre::bail!(
                "block {} has {transactions} transactions but {} receipts",
                block.header().number(),
                receipts.len()
            );
        }
    }
    Ok(())
}

/// Confirm exact post-state for a canonical (committed or reorged-in) block,
/// extract L2 bridge facts from notification-local receipts, then fetch the
/// exact Tempo/L1 block anchored by `TempoAdvanced`, verify its identity and
/// receipt root, and independently extract ZonePortal L1 facts.
///
/// The state lookup uses the exact block hash to avoid checking the wrong fork
/// or head. Receipts come directly from the ExEx notification's executed chain.
/// The L1 block is fetched by the exact anchor hash (never by latest/head) so
/// the checker reads the same Tempo block the sequencer imported. The L1
/// provider connection is established lazily on first use so an L1 outage at
/// startup does not terminate the ExEx.
async fn process_canonical_block<P, R>(
    provider: &P,
    l1_provider: &mut Option<DynProvider<TempoNetwork>>,
    l1_rpc_url: &str,
    portal_address: Address,
    number: u64,
    hash: B256,
    receipts: &[R],
) -> eyre::Result<()>
where
    P: StateProviderFactory,
    R: alloy_consensus::TxReceipt<Log = alloy_primitives::Log>,
{
    let state = provider
        .state_by_block_hash(hash)
        .wrap_err_with(|| format!("failed to obtain state for block {hash}"))?;
    state
        .basic_account(&Address::ZERO)
        .wrap_err_with(|| format!("failed to read state for block {hash}"))?;

    let l2_facts = extract_l2_facts(number, hash, receipts)?;
    log_l2_facts(&l2_facts);

    // Fetch the exact Tempo/L1 block anchored by TempoAdvanced.
    let anchor = l2_facts.l1_anchor();
    let l1_hash = anchor.tempo_block_hash;
    let l1_number = anchor.tempo_block_number;

    // Connect to L1 lazily — only when we actually need to fetch an anchored
    // block. This keeps the ExEx running even if L1 is temporarily unavailable.
    if l1_provider.is_none() {
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect(l1_rpc_url)
            .await
            .wrap_err("checker failed to connect to L1 RPC")?
            .erased();
        *l1_provider = Some(provider);
    }
    let l1 = l1_provider
        .as_ref()
        .expect("L1 provider was just initialized");

    let block = l1
        .get_block_by_hash(l1_hash)
        .await
        .wrap_err_with(|| format!("failed to fetch L1 block {l1_number} ({l1_hash})"))?
        .ok_or_else(|| eyre::eyre!("L1 block {l1_number} ({l1_hash}) not found"))?;

    let fetched_hash = block.header().hash();
    let computed_hash = block.header().as_ref().hash_slow();
    let fetched_number = block.header().number();
    authenticate_l1_block(
        l1_hash,
        l1_number,
        fetched_hash,
        computed_hash,
        fetched_number,
    )?;

    let transaction_hashes = block.transactions().hashes().collect::<Vec<_>>();
    let expected_receipts_root = block.header().receipts_root();
    let expected_logs_bloom = block.header().logs_bloom();

    let receipts = l1
        .get_block_receipts(BlockId::hash(l1_hash))
        .await
        .wrap_err_with(|| format!("failed to fetch L1 receipts for block {l1_number} ({l1_hash})"))?
        .ok_or_else(|| eyre::eyre!("no receipts for L1 block {l1_number} ({l1_hash})"))?;

    verify_l1_receipts(
        BlockNumHash::new(l1_number, l1_hash),
        expected_receipts_root,
        expected_logs_bloom,
        &transaction_hashes,
        &receipts,
    )?;

    let l1_facts = extract_l1_facts(l1_number, l1_hash, portal_address, &receipts)?;
    log_l1_facts(&l1_facts, portal_address);

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{BlockBody, Header, SignableTransaction as _, TxLegacy};
    use alloy_primitives::{Log, Signature, U256};
    use alloy_provider::ProviderBuilder;
    use alloy_rpc_types_eth::{BlockTransactions, Header as RpcHeader};
    use alloy_sol_types::SolEvent as _;
    use alloy_transport::mock::Asserter;
    use reth_ethereum_primitives::{Block as EthBlock, EthPrimitives, Receipt};
    use reth_execution_types::{Chain, ExecutionOutcome};
    use reth_primitives_traits::{RecoveredBlock, SealedBlock};
    use reth_provider::test_utils::MockEthProvider;
    use std::{collections::BTreeMap, sync::Arc};
    use tempo_alloy::TempoNetwork;
    use tempo_alloy::rpc::{TempoHeaderResponse, TempoTransactionReceipt};
    use tempo_primitives::{TempoHeader, TempoTxEnvelope};
    use tempo_zone_contracts::{IZoneInbox, ZONE_INBOX_ADDRESS};

    /// L1 anchor values used by `anchor_receipt()`.
    const L1_NUMBER: u64 = 100;
    const PORTAL: Address = Address::repeat_byte(0x42);

    fn empty_l1_header() -> TempoHeader {
        let receipts_root = alloy_consensus::proofs::calculate_receipt_root::<
            alloy_consensus::ReceiptWithBloom<
                tempo_primitives::TempoReceipt<alloy_primitives::Log>,
            >,
        >(&[]);
        TempoHeader {
            inner: Header {
                number: L1_NUMBER,
                receipts_root,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn checker_mode_parse_display_default() {
        assert_eq!(CheckerMode::default(), CheckerMode::Off);
        assert_eq!("off".parse::<CheckerMode>().unwrap(), CheckerMode::Off);
        assert_eq!(
            "OBSERVE".parse::<CheckerMode>().unwrap(),
            CheckerMode::Observe
        );
        assert!("enforce".parse::<CheckerMode>().is_err());
        assert_eq!(CheckerMode::Off.to_string(), "off");
        assert_eq!(CheckerMode::Observe.to_string(), "observe");
    }

    type Block = RecoveredBlock<EthBlock>;

    fn make_block(number: u64, parent_hash: B256) -> Block {
        let header = Header {
            number,
            parent_hash,
            difficulty: U256::ZERO,
            ..Default::default()
        };
        let transaction =
            TxLegacy::default().into_signed(Signature::new(U256::from(1), U256::from(1), false));
        let block = EthBlock::new(
            header,
            BlockBody {
                transactions: vec![transaction.into()],
                ..Default::default()
            },
        );
        RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), vec![])
    }

    fn make_chain(blocks: Vec<Block>) -> Arc<Chain<EthPrimitives>> {
        let first_block = blocks
            .first()
            .map(|block| block.header().number())
            .unwrap_or(0);
        let receipts = blocks.iter().map(|_| vec![anchor_receipt()]).collect();
        let outcome: ExecutionOutcome<Receipt> = ExecutionOutcome::new(
            Default::default(),
            receipts,
            first_block,
            Default::default(),
        );
        Arc::new(Chain::new(blocks, outcome, BTreeMap::new()))
    }

    fn make_chain_without_receipts(blocks: Vec<Block>) -> Arc<Chain<EthPrimitives>> {
        Arc::new(Chain::new(blocks, Default::default(), BTreeMap::new()))
    }

    /// A minimal receipt with a valid `TempoAdvanced` anchor so the block
    /// passes fact extraction.
    fn anchor_receipt() -> Receipt {
        let l1_hash = empty_l1_header().hash_slow();
        let event = IZoneInbox::TempoAdvanced {
            tempoBlockHash: l1_hash,
            tempoBlockNumber: L1_NUMBER,
            depositsProcessed: U256::ZERO,
            newProcessedDepositQueueHash: B256::ZERO,
            lastProcessedDepositNumber: 0,
        };
        Receipt {
            tx_type: Default::default(),
            success: true,
            cumulative_gas_used: 0,
            logs: vec![Log {
                address: ZONE_INBOX_ADDRESS,
                data: event.encode_log_data(),
            }],
        }
    }

    fn setup_provider(blocks: &[(&Block, Vec<Receipt>)]) -> MockEthProvider {
        let provider = MockEthProvider::new();
        for (block, receipts) in blocks {
            let hash = block.hash();
            let number = block.header().number();
            provider.add_block(hash, (*block).clone().into_sealed_block().unseal());
            provider.extend_headers([(hash, block.header().clone())]);
            provider.add_receipts(number, receipts.clone());
        }
        provider
    }

    /// Build a mock L1 provider that responds to `get_block_by_hash` and
    /// `get_block_receipts` for the anchor L1 block.  The block has zero
    /// transactions and empty receipts, with a receipts root that matches.
    fn mock_l1_provider(call_count: usize) -> DynProvider<TempoNetwork> {
        let asserter = Asserter::new();
        let tempo_header = empty_l1_header();
        let header = TempoHeaderResponse {
            inner: RpcHeader {
                hash: tempo_header.hash_slow(),
                inner: tempo_header,
                total_difficulty: None,
                size: None,
            },
            timestamp_millis: 0,
        };

        let block = alloy_rpc_types_eth::Block {
            header,
            uncles: vec![],
            transactions:
                BlockTransactions::<alloy_rpc_types_eth::Transaction<TempoTxEnvelope>>::Hashes(
                    vec![],
                ),
            withdrawals: None,
        };

        for _ in 0..call_count {
            asserter.push_success(&Some(block.clone()));
            asserter.push_success(&Some(Vec::<TempoTransactionReceipt>::new()));
        }

        ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter)
            .erased()
    }

    #[tokio::test]
    async fn commit_returns_new_canonical_tip() {
        let b1 = make_block(1, B256::repeat_byte(1));
        let b2 = make_block(2, b1.hash());
        let b3 = make_block(3, b2.hash());
        let r = anchor_receipt();
        let provider = setup_provider(&[
            (&b1, vec![r.clone()]),
            (&b2, vec![r.clone()]),
            (&b3, vec![r]),
        ]);
        let mut l1 = Some(mock_l1_provider(3));
        let notification = ExExNotification::ChainCommitted {
            new: make_chain(vec![b1, b2, b3.clone()]),
        };
        let tip = process_notification(&notification, &provider, &mut l1, "", PORTAL)
            .await
            .unwrap();
        assert_eq!(tip, BlockNumHash::new(3, b3.hash()));
    }

    #[tokio::test]
    async fn revert_returns_height_before_reverted_range() {
        let parent = B256::repeat_byte(3);
        let b1 = make_block(1, parent);
        let b2 = make_block(2, b1.hash());
        let b3 = make_block(3, b2.hash());
        let notification = ExExNotification::ChainReverted {
            old: make_chain(vec![b1, b2, b3]),
        };
        // Reverted blocks don't touch the L1 provider.
        let mut l1 = Some(mock_l1_provider(0));
        let tip = process_notification(
            &notification,
            &MockEthProvider::<EthPrimitives>::new(),
            &mut l1,
            "",
            PORTAL,
        )
        .await
        .unwrap();
        assert_eq!(tip, BlockNumHash::new(0, parent));
    }

    #[tokio::test]
    async fn reorg_returns_new_canonical_tip() {
        let parent = B256::repeat_byte(4);
        let old1 = make_block(1, parent);
        let old2 = make_block(2, old1.hash());
        let new1 = make_block(1, parent);
        let new2 = make_block(2, new1.hash());
        let new3 = make_block(3, new2.hash());
        let r = anchor_receipt();
        let provider = setup_provider(&[
            (&new1, vec![r.clone()]),
            (&new2, vec![r.clone()]),
            (&new3, vec![r]),
        ]);
        let mut l1 = Some(mock_l1_provider(3));
        let notification = ExExNotification::ChainReorged {
            old: make_chain(vec![old1, old2]),
            new: make_chain(vec![new1, new2, new3.clone()]),
        };
        let tip = process_notification(&notification, &provider, &mut l1, "", PORTAL)
            .await
            .unwrap();
        assert_eq!(tip, BlockNumHash::new(3, new3.hash()));
    }

    #[tokio::test]
    async fn missing_receipts_is_error() {
        let b1 = make_block(1, B256::repeat_byte(5));
        let provider = MockEthProvider::<EthPrimitives>::new();
        let mut l1 = Some(mock_l1_provider(0));
        let notification = ExExNotification::ChainCommitted {
            new: make_chain_without_receipts(vec![b1]),
        };
        let result = process_notification(&notification, &provider, &mut l1, "", PORTAL).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("receipt"),
            "error should mention receipts"
        );
    }

    #[tokio::test]
    async fn receipt_count_must_match_transaction_count() {
        let b1 = make_block(1, B256::repeat_byte(6));
        let provider = setup_provider(&[(&b1, vec![anchor_receipt()])]);
        let mut l1 = Some(mock_l1_provider(0));
        let outcome: ExecutionOutcome<Receipt> = ExecutionOutcome::new(
            Default::default(),
            vec![vec![anchor_receipt(), anchor_receipt()]],
            1,
            Default::default(),
        );
        let notification: ExExNotification<EthPrimitives> = ExExNotification::ChainCommitted {
            new: Arc::new(Chain::new(vec![b1], outcome, BTreeMap::new())),
        };

        let error = process_notification(&notification, &provider, &mut l1, "", PORTAL)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("1 transactions but 2 receipts"));
    }

    #[tokio::test]
    async fn multi_block_commit_with_facts() {
        let b1 = make_block(1, B256::repeat_byte(7));
        let b2 = make_block(2, b1.hash());
        let b3 = make_block(3, b2.hash());
        let r = anchor_receipt();
        let provider = setup_provider(&[
            (&b1, vec![r.clone()]),
            (&b2, vec![r.clone()]),
            (&b3, vec![r]),
        ]);
        let mut l1 = Some(mock_l1_provider(3));
        let notification = ExExNotification::ChainCommitted {
            new: make_chain(vec![b1, b2, b3.clone()]),
        };
        let tip = process_notification(&notification, &provider, &mut l1, "", PORTAL)
            .await
            .unwrap();
        assert_eq!(tip, BlockNumHash::new(3, b3.hash()));
    }
}
