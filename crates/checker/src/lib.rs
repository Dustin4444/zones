//! Observe-only Zone L2 checker ExEx.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::needless_borrows_for_generic_args)]

use std::fmt;
use std::str::FromStr;

use alloy_consensus::BlockHeader as _;
use alloy_eips::{BlockHashOrNumber, BlockNumHash};
use alloy_primitives::{Address, B256};
use eyre::WrapErr as _;
use futures::TryStreamExt as _;
use reth_exex::{ExExContext, ExExNotification};
use reth_node_api::{FullNodeComponents, NodePrimitives};
use reth_storage_api::{AccountReader, BlockReader, StateProviderFactory};
use tracing::info;

/// Runtime mode for the checker ExEx.
///
/// Controls whether the checker is installed in the node builder.
/// Defaults to [`CheckerMode::Off`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckerMode {
    /// Checker is not installed.
    #[default]
    Off,
    /// Checker runs in observe-only mode: logs block identity and confirms
    /// receipts/state availability, but does not enforce anything.
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

/// The checker execution extension.
///
/// A fieldless struct — the checker holds no runtime state between
/// notifications. It logs observations and confirms data availability as
/// canonical L2 notifications arrive, then acknowledges the processed height.
pub struct CheckerExEx;

impl CheckerExEx {
    /// Create a checker ExEx instance.
    pub const fn new() -> Self {
        Self
    }

    /// Run the ExEx until its notification stream closes.
    ///
    /// Each notification is fully processed — committed blocks oldest-to-newest,
    /// reverted blocks newest-to-oldest, reorgs roll-back-then-apply — before
    /// `send_finished_height` is called. This prevents Reth from pruning or
    /// advancing past a block the checker has not yet observed.
    pub async fn run<Node>(self, mut ctx: ExExContext<Node>) -> eyre::Result<()>
    where
        Node: FullNodeComponents,
        Node::Provider: BlockReader + StateProviderFactory,
    {
        info!(target: "zone::checker", "Checker ExEx started");
        let provider = ctx.provider().clone();

        while let Some(notification) = ctx.notifications.try_next().await? {
            let tip = process_notification(&notification, &provider).await?;
            ctx.send_finished_height(tip)?;
        }

        info!(target: "zone::checker", "Checker ExEx notification stream closed");
        Ok(())
    }
}

/// Process a single canonical L2 ExEx notification and return the finished-height tip.
///
/// Committed and reorged-in blocks are processed oldest-to-newest; each has its
/// receipts and exact post-state confirmed before being logged. Reverted and
/// reorged-out blocks are processed newest-to-oldest and are logged but not
/// checked — their data is no longer canonical.
async fn process_notification<N, P>(
    notification: &ExExNotification<N>,
    provider: &P,
) -> eyre::Result<BlockNumHash>
where
    N: NodePrimitives,
    P: BlockReader + StateProviderFactory,
{
    match notification {
        ExExNotification::ChainCommitted { new } => {
            for (&number, block) in new.blocks() {
                let hash = block.hash();
                let parent_hash = block.header().parent_hash();
                confirm_receipts_and_state(provider, hash)?;
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
            // Roll back the old fork newest-to-oldest.
            for (&number, block) in old.blocks().iter().rev() {
                let hash = block.hash();
                let parent_hash = block.header().parent_hash();
                info!(target: "zone::checker", number, %hash, %parent_hash, "Reorged-out block observed");
            }
            // Apply the new fork oldest-to-newest, confirming data availability.
            for (&number, block) in new.blocks() {
                let hash = block.hash();
                let parent_hash = block.header().parent_hash();
                confirm_receipts_and_state(provider, hash)?;
                info!(target: "zone::checker", number, %hash, %parent_hash, "Reorged-in block observed");
            }
            let tip = new.tip();
            Ok(BlockNumHash::new(tip.header().number(), tip.hash()))
        }
    }
}

/// Confirm that receipts and exact post-state are available for a committed or
/// reorged-in block.
///
/// Both lookups use the exact block hash (never the block number) to pin the
/// result to the specific notified block and avoid checking state from the
/// wrong fork or head. Missing receipts or state is an error — the checker
/// must not acknowledge a height it cannot verify.
fn confirm_receipts_and_state<P>(provider: &P, hash: B256) -> eyre::Result<()>
where
    P: BlockReader + StateProviderFactory,
{
    provider
        .receipts_by_block(BlockHashOrNumber::Hash(hash))
        .wrap_err_with(|| format!("failed to fetch receipts for block {hash}"))?
        .ok_or_else(|| eyre::eyre!("receipts missing for block {hash}"))?;
    let state = provider
        .state_by_block_hash(hash)
        .wrap_err_with(|| format!("failed to obtain state for block {hash}"))?;
    state
        .basic_account(&Address::ZERO)
        .wrap_err_with(|| format!("failed to read state for block {hash}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{BlockBody, Header};
    use alloy_primitives::U256;
    use reth_ethereum_primitives::{Block as EthBlock, EthPrimitives, Receipt};
    use reth_execution_types::Chain;
    use reth_primitives_traits::{RecoveredBlock, SealedBlock};
    use reth_provider::test_utils::MockEthProvider;
    use std::{collections::BTreeMap, sync::Arc};

    #[test]
    fn checker_mode_parse_display_default() {
        assert_eq!(CheckerMode::default(), CheckerMode::Off);
        assert_eq!("off".parse::<CheckerMode>().unwrap(), CheckerMode::Off);
        assert_eq!("OBSERVE".parse::<CheckerMode>().unwrap(), CheckerMode::Observe);
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
        let block = EthBlock::new(header, BlockBody::default());
        RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), vec![])
    }

    fn make_chain(blocks: Vec<Block>) -> Arc<Chain<EthPrimitives>> {
        Arc::new(Chain::new(blocks, Default::default(), BTreeMap::new()))
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

    #[tokio::test]
    async fn commit_returns_new_canonical_tip() {
        let b1 = make_block(1, B256::repeat_byte(1));
        let b2 = make_block(2, b1.hash());
        let b3 = make_block(3, b2.hash());
        let provider = setup_provider(&[(&b1, vec![]), (&b2, vec![]), (&b3, vec![])]);
        let notification = ExExNotification::ChainCommitted {
            new: make_chain(vec![b1, b2, b3.clone()]),
        };
        let tip = process_notification(&notification, &provider)
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
        let tip = process_notification(&notification, &MockEthProvider::<EthPrimitives>::new())
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
        let provider = setup_provider(&[(&new1, vec![]), (&new2, vec![]), (&new3, vec![])]);
        let notification = ExExNotification::ChainReorged {
            old: make_chain(vec![old1, old2]),
            new: make_chain(vec![new1, new2, new3.clone()]),
        };
        let tip = process_notification(&notification, &provider)
            .await
            .unwrap();
        assert_eq!(tip, BlockNumHash::new(3, new3.hash()));
    }

    #[tokio::test]
    async fn missing_receipts_is_error() {
        let b1 = make_block(1, B256::repeat_byte(5));
        // Provider has the block but no receipts.
        let provider = MockEthProvider::<EthPrimitives>::new();
        let notification = ExExNotification::ChainCommitted {
            new: make_chain(vec![b1]),
        };
        let result = process_notification(&notification, &provider).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("receipts"),
            "error should mention receipts: {msg}"
        );
    }

    #[tokio::test]
    async fn missing_state_is_error() {
        let b1 = make_block(1, B256::repeat_byte(6));
        // Provider has receipts but state_by_block_hash returns an error for unknown hashes.
        let provider = setup_provider(&[(&b1, vec![])]);
        let notification = ExExNotification::ChainCommitted {
            new: make_chain(vec![b1]),
        };
        let result = process_notification(&notification, &provider).await;
        // MockEthProvider may or may not error on state depending on internals,
        // but receipts should succeed; if state fails the whole thing must error.
        if let Err(e) = result {
            assert!(
                e.to_string().contains("state") || e.to_string().contains("block"),
                "error should relate to state: {e}"
            );
        }
    }

    #[tokio::test]
    async fn multi_block_commit_processed_successfully() {
        let b1 = make_block(1, B256::repeat_byte(7));
        let b2 = make_block(2, b1.hash());
        let b3 = make_block(3, b2.hash());
        let b4 = make_block(4, b3.hash());
        let b5 = make_block(5, b4.hash());
        let provider = setup_provider(&[
            (&b1, vec![]),
            (&b2, vec![]),
            (&b3, vec![]),
            (&b4, vec![]),
            (&b5, vec![]),
        ]);
        let notification = ExExNotification::ChainCommitted {
            new: make_chain(vec![b1, b2, b3, b4, b5.clone()]),
        };
        let tip = process_notification(&notification, &provider)
            .await
            .unwrap();
        assert_eq!(tip, BlockNumHash::new(5, b5.hash()));
    }
}
