use alloy_consensus::Header;
use alloy_eips::BlockNumHash;
use alloy_primitives::{B256, Bytes};
use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
use alloy_rpc_types_eth::{Block, BlockTransactions, Header as RpcHeader, Transaction};
use alloy_transport::mock::Asserter;
use reth_provider::test_utils::MockEthProvider;
use tempo_alloy::{TempoNetwork, rpc::TempoHeaderResponse};
use tempo_primitives::{TempoHeader, TempoPrimitives, TempoTxEnvelope};

use super::{
    FreshHistory, classify_fresh_history, genesis_anchor, header_tip, prove_ancestry,
    prove_descendants_after,
};
use crate::{
    check::finding::CheckError,
    observe::{AcquisitionError, AcquisitionSource, ImportedTempoHeader},
    runtime::{RuntimeError, bootstrap::error::BootstrapError},
};

type L1RpcBlock = Block<Transaction<TempoTxEnvelope>, TempoHeaderResponse>;

mod e2e;

#[test]
fn fresh_history_uses_l1_replay_when_creation_precedes_or_equals_genesis_anchor() {
    let anchor = BlockNumHash::new(10, B256::repeat_byte(0xa0));

    assert_eq!(
        classify_fresh_history(BlockNumHash::new(9, B256::repeat_byte(0x90)), anchor),
        FreshHistory::PortalPresentAtGenesisAnchor
    );
    assert_eq!(
        classify_fresh_history(BlockNumHash::new(10, B256::repeat_byte(0x91)), anchor),
        FreshHistory::PortalPresentAtGenesisAnchor
    );
}

#[test]
fn fresh_history_uses_zone_replay_when_creation_follows_genesis_anchor() {
    assert_eq!(
        classify_fresh_history(
            BlockNumHash::new(11, B256::repeat_byte(0xb1)),
            BlockNumHash::new(10, B256::repeat_byte(0xb0)),
        ),
        FreshHistory::PortalCreatedAfterGenesisAnchor
    );
}

#[test]
fn zero_genesis_checkpoint_is_an_explicit_unsupported_bootstrap_style() {
    let provider = MockEthProvider::<TempoPrimitives>::new();
    let genesis = BlockNumHash::new(0, B256::repeat_byte(0xc0));

    assert!(matches!(
        bootstrap_error(genesis_anchor(&provider, genesis).unwrap_err()),
        BootstrapError::UnsupportedBootstrapStyle
    ));
}

#[tokio::test]
async fn ancestry_returns_the_inclusive_ascending_canonical_path() {
    let ancestor = header(5, B256::repeat_byte(0xd0), Bytes::new());
    let middle = header(6, ancestor.hash(), Bytes::new());
    let descendant = header(7, middle.hash(), Bytes::new());
    let provider = provider_with([Some(block_response(&middle))]);

    let path = prove_ancestry(&provider, descendant.clone(), &ancestor)
        .await
        .unwrap();
    assert_eq!(
        path.iter().map(header_tip).collect::<Vec<_>>(),
        vec![
            header_tip(&ancestor),
            header_tip(&middle),
            header_tip(&descendant),
        ]
    );
}

#[tokio::test]
async fn strict_descendants_do_not_reacquire_an_equal_durable_boundary() {
    let boundary = header(5, B256::repeat_byte(0xd1), Bytes::new());
    let provider = provider_with(std::iter::empty::<Option<L1RpcBlock>>());

    let path = prove_descendants_after(&provider, boundary.clone(), header_tip(&boundary))
        .await
        .unwrap();

    assert!(path.is_empty());
}

#[tokio::test]
async fn equal_height_with_a_different_hash_is_not_accepted_as_ancestry() {
    let ancestor = header(5, B256::repeat_byte(0xe0), Bytes::from_static(b"ancestor"));
    let descendant = header(
        5,
        B256::repeat_byte(0xe0),
        Bytes::from_static(b"descendant"),
    );
    let provider = provider_with(std::iter::empty::<Option<L1RpcBlock>>());

    let error = prove_ancestry(&provider, descendant.clone(), &ancestor)
        .await
        .unwrap_err();
    assert!(matches!(
        bootstrap_error(error),
        BootstrapError::TempoAncestryNotLinked {
            descendant: actual_descendant,
            expected_ancestor,
            reached,
        } if actual_descendant == header_tip(&descendant)
            && expected_ancestor == header_tip(&ancestor)
            && reached == header_tip(&descendant)
    ));
}

#[tokio::test]
async fn ancestry_rejects_a_parent_with_a_nonconsecutive_number() {
    let parent = header(3, B256::repeat_byte(0xf0), Bytes::new());
    let descendant = header(5, parent.hash(), Bytes::new());
    let provider = provider_with([Some(block_response(&parent))]);

    let error = prove_ancestry(&provider, descendant.clone(), &parent)
        .await
        .unwrap_err();
    assert!(matches!(
        bootstrap_error(error),
        BootstrapError::NonConsecutiveTempoAncestry {
            child,
            expected_parent,
            actual_parent,
        } if child == header_tip(&descendant)
            && expected_parent == BlockNumHash::new(4, parent.hash())
            && actual_parent == header_tip(&parent)
    ));
}

#[tokio::test]
async fn missing_l1_ancestry_remains_an_explicit_acquisition_error() {
    let ancestor = header(3, B256::repeat_byte(0x11), Bytes::new());
    let missing_parent = B256::repeat_byte(0x12);
    let descendant = header(5, missing_parent, Bytes::new());
    let provider = provider_with([None]);

    assert!(matches!(
        prove_ancestry(&provider, descendant, &ancestor).await,
        Err(RuntimeError::Check(CheckError::Acquisition(
            AcquisitionError::Missing {
                kind: AcquisitionSource::L1Block,
                ..
            }
        )))
    ));
}

#[tokio::test]
async fn tampered_l1_ancestry_header_remains_an_explicit_acquisition_error() {
    let ancestor = header(4, B256::repeat_byte(0x21), Bytes::new());
    let parent = header(5, ancestor.hash(), Bytes::new());
    let descendant = header(6, parent.hash(), Bytes::new());
    let mut tampered = block_response(&parent);
    tampered.header.inner.inner.inner.gas_limit += 1;
    let provider = provider_with([Some(tampered)]);

    assert!(matches!(
        prove_ancestry(&provider, descendant, &ancestor).await,
        Err(RuntimeError::Check(CheckError::Acquisition(
            AcquisitionError::Inconsistent {
                kind: AcquisitionSource::L1Block,
                ..
            }
        )))
    ));
}

fn header(number: u64, parent_hash: B256, extra_data: Bytes) -> ImportedTempoHeader {
    ImportedTempoHeader::for_test(TempoHeader {
        inner: Header {
            number,
            parent_hash,
            extra_data,
            ..Default::default()
        },
        ..Default::default()
    })
}

fn block_response(imported: &ImportedTempoHeader) -> L1RpcBlock {
    Block {
        header: TempoHeaderResponse {
            inner: RpcHeader {
                hash: imported.hash(),
                inner: imported.header().clone(),
                total_difficulty: None,
                size: None,
            },
            timestamp_millis: 0,
        },
        uncles: Vec::new(),
        transactions: BlockTransactions::Hashes(Vec::new()),
        withdrawals: None,
    }
}

fn provider_with(
    responses: impl IntoIterator<Item = Option<L1RpcBlock>>,
) -> DynProvider<TempoNetwork> {
    let asserter = Asserter::new();
    for response in responses {
        asserter.push_success(&response);
    }
    ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect_mocked_client(asserter)
        .erased()
}

fn bootstrap_error(error: RuntimeError) -> BootstrapError {
    let RuntimeError::Bootstrap(error) = error else {
        panic!("expected bootstrap error, got {error:?}");
    };
    *error
}
