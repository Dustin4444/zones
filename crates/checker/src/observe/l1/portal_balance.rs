//! Exact imported-block Portal token-balance acquisition.

use alloy_eips::BlockId;
use alloy_primitives::{Address, B256, U256};
use alloy_provider::Provider;
use tempo_alloy::TempoNetwork;
use tempo_contracts::precompiles::ITIP20;

use crate::observe::error::{AcquisitionError, AcquisitionSource};

/// Read one token's Portal balance at the exact imported Tempo block.
///
/// A timeout, missing archive state, or call failure is an acquisition failure,
/// not a zero balance or protocol finding.
pub(crate) async fn acquire_portal_token_balance<P>(
    provider: &P,
    token: Address,
    portal: Address,
    block_hash: B256,
) -> Result<U256, AcquisitionError>
where
    P: Provider<TempoNetwork>,
{
    ITIP20::new(token, provider)
        .balanceOf(portal)
        .block(BlockId::hash_canonical(block_hash))
        .call()
        .await
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::PortalCollateral, error))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Bytes, U256};
    use alloy_provider::ProviderBuilder;
    use alloy_sol_types::SolCall as _;
    use alloy_transport::mock::Asserter;
    use tempo_contracts::precompiles::ITIP20;

    use super::*;

    #[tokio::test]
    async fn exact_block_call_and_failures_remain_acquisition() {
        let token = Address::repeat_byte(0x20);
        let portal = Address::repeat_byte(0x42);
        let block_hash = B256::repeat_byte(0x51);
        let expected = U256::from(123_456_u64);
        let asserter = Asserter::new();
        asserter.push_success(&Bytes::from(ITIP20::balanceOfCall::abi_encode_returns(
            &expected,
        )));
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(asserter.clone());

        assert_eq!(
            acquire_portal_token_balance(&provider, token, portal, block_hash)
                .await
                .unwrap(),
            expected
        );
        assert!(asserter.read_q().is_empty());

        let asserter = Asserter::new();
        asserter.push_failure_msg("historical call unavailable");
        let provider =
            ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);
        assert!(matches!(
            acquire_portal_token_balance(&provider, token, portal, block_hash).await,
            Err(AcquisitionError::Unavailable {
                kind: AcquisitionSource::PortalCollateral,
                ..
            })
        ));
    }
}
