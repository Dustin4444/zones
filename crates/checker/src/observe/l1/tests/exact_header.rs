use alloy_primitives::B256;
use alloy_provider::ProviderBuilder;
use alloy_rpc_types_eth::{Block, Transaction};
use alloy_transport::mock::Asserter;
use tempo_alloy::{TempoNetwork, rpc::TempoHeaderResponse};
use tempo_primitives::TempoTxEnvelope;

use super::{anchor, assert_inconsistent, assert_unavailable, block_response};
use crate::observe::{AcquisitionError, AcquisitionSource, ObservationError, acquire_l1_header};

#[tokio::test]
async fn acquires_only_a_reported_and_computed_exact_hash() {
    let (imported, _) = anchor(vec![]);
    let requested = imported.hash();

    let asserter = Asserter::new();
    asserter.push_success(&Some(block_response(&imported, Vec::new())));
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter.clone());

    assert_eq!(
        acquire_l1_header(&provider, requested).await.unwrap(),
        imported
    );
    assert!(asserter.read_q().is_empty());

    let mut wrong_reported = block_response(&imported, Vec::new());
    wrong_reported.header.inner.hash = B256::repeat_byte(0xa1);
    let mut wrong_computed = block_response(&imported, Vec::new());
    wrong_computed.header.inner.inner.inner.gas_limit += 1;

    for response in [wrong_reported, wrong_computed] {
        let asserter = Asserter::new();
        asserter.push_success(&Some(response));
        let provider =
            ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);
        assert_inconsistent(
            acquire_l1_header(&provider, requested).await.unwrap_err(),
            AcquisitionSource::L1Block,
        );
    }
}

#[tokio::test]
async fn missing_and_unavailable_exact_headers_remain_acquisition_errors() {
    let requested = B256::repeat_byte(0x42);
    let asserter = Asserter::new();
    asserter
        .push_success(&Option::<Block<Transaction<TempoTxEnvelope>, TempoHeaderResponse>>::None);
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);
    assert!(matches!(
        acquire_l1_header(&provider, requested).await,
        Err(ObservationError::Acquisition(AcquisitionError::Missing {
            kind: AcquisitionSource::L1Block,
            ..
        }))
    ));

    let asserter = Asserter::new();
    asserter.push_failure_msg("block transport failure");
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);
    assert_unavailable(
        acquire_l1_header(&provider, requested).await.unwrap_err(),
        AcquisitionSource::L1Block,
    );
}
