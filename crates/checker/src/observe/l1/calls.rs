//! Selective direct-Portal transaction acquisition and binding.

use alloy_network::TransactionResponse as _;
use alloy_primitives::{Address, B256, TxKind};
use alloy_provider::Provider;
use tempo_alloy::TempoNetwork;
use tempo_primitives::TempoTxEnvelope;

use super::super::{
    abi::{DecodedPortalCall, ImportedTempoHeader, decode_portal_call},
    error::{
        AcquisitionError, AcquisitionSource, AuthenticatedTransaction, ObservationError,
        PortalCallError, PortalCallFamily, ProtocolChain, ensure_acquisition_equal,
    },
};

type TempoTransactionResponse = <TempoNetwork as alloy_network::Network>::TransactionResponse;

pub(super) async fn acquire_direct_portal_call<P>(
    provider: &P,
    portal: Address,
    imported: &ImportedTempoHeader,
    transaction_index: usize,
    transaction_hash: B256,
    expected: PortalCallFamily,
) -> Result<DecodedPortalCall, ObservationError>
where
    P: Provider<TempoNetwork>,
{
    let transaction = provider
        .get_transaction_by_hash(transaction_hash)
        .await
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Transaction, error))?
        .ok_or_else(|| {
            AcquisitionError::missing(AcquisitionSource::L1Transaction, transaction_hash)
        })?;
    authenticate_transaction(&transaction, imported, transaction_index, transaction_hash)?;

    let envelope: &TempoTxEnvelope = transaction.as_ref();
    let calldata = sole_portal_calldata(envelope, portal, transaction_hash)?;
    let coordinate =
        AuthenticatedTransaction::new(ProtocolChain::TempoL1, transaction_index, transaction_hash);
    let decoded = decode_portal_call(calldata, coordinate)?;
    let actual = decoded.family();
    if actual != expected {
        return Err(PortalCallError::FamilyMismatch {
            transaction_hash,
            expected,
            actual,
        }
        .into());
    }
    if expected == PortalCallFamily::ProcessWithdrawals
        && !decoded.is_nonempty_process_withdrawals()
    {
        return Err(PortalCallError::EmptyProcessWithOutcomes { transaction_hash }.into());
    }
    Ok(decoded)
}

pub(super) fn authenticate_transaction(
    transaction: &TempoTransactionResponse,
    imported: &ImportedTempoHeader,
    transaction_index: usize,
    transaction_hash: B256,
) -> Result<(), ObservationError> {
    ensure_acquisition_equal(
        AcquisitionSource::L1Transaction,
        "transaction hash",
        transaction_hash,
        transaction.tx_hash(),
    )?;
    ensure_acquisition_equal(
        AcquisitionSource::L1Transaction,
        "transaction block hash",
        Some(imported.hash()),
        transaction.block_hash(),
    )?;
    ensure_acquisition_equal(
        AcquisitionSource::L1Transaction,
        "transaction block number",
        Some(imported.number()),
        transaction.block_number(),
    )?;
    ensure_acquisition_equal(
        AcquisitionSource::L1Transaction,
        "transaction index",
        Some(transaction_index as u64),
        transaction.transaction_index(),
    )
}

pub(super) fn sole_portal_calldata(
    envelope: &TempoTxEnvelope,
    portal: Address,
    transaction_hash: B256,
) -> Result<&[u8], ObservationError> {
    let mut calls = envelope.calls();
    let Some((kind, calldata)) = calls.next() else {
        return Err(PortalCallError::UnsupportedNestedPortalCall {
            transaction_hash,
            target: None,
        }
        .into());
    };
    let target = match kind {
        TxKind::Call(target) => Some(target),
        TxKind::Create => None,
    };
    if calls.next().is_some() || target != Some(portal) {
        return Err(PortalCallError::UnsupportedNestedPortalCall {
            transaction_hash,
            target,
        }
        .into());
    }
    Ok(calldata)
}
