//! Exact-header and complete-receipt authentication.

use alloy_consensus::{BlockHeader as _, Sealable as _};
use alloy_eips::BlockId;
use alloy_network::{BlockResponse as _, ReceiptResponse as _, primitives::HeaderResponse as _};
use alloy_primitives::{B256, Bloom};
use alloy_provider::Provider;
use tempo_alloy::{TempoNetwork, rpc::TempoTransactionReceipt};
use tempo_primitives::TempoHeader;

use super::{
    super::{
        abi::ImportedTempoHeader,
        error::{AcquisitionError, AcquisitionSource, ObservationError},
    },
    ensure_acquisition_equal,
};

pub(super) struct AuthenticatedBlock {
    pub(super) transaction_hashes: Vec<B256>,
}

pub(super) async fn acquire_block<P>(
    provider: &P,
    imported: &ImportedTempoHeader,
) -> Result<AuthenticatedBlock, ObservationError>
where
    P: Provider<TempoNetwork>,
{
    let block = provider
        .get_block_by_hash(imported.hash())
        .hashes()
        .await
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Block, error))?
        .ok_or_else(|| AcquisitionError::missing(AcquisitionSource::L1Block, imported.hash()))?;

    let response_header = block.header();
    let fetched_header: &TempoHeader = response_header.as_ref();
    authenticate_header(
        imported,
        response_header.hash(),
        fetched_header.hash_slow(),
        fetched_header,
    )?;

    Ok(AuthenticatedBlock {
        transaction_hashes: block.transactions().hashes().collect(),
    })
}

pub(super) async fn acquire_receipts<P>(
    provider: &P,
    imported: &ImportedTempoHeader,
    block: &AuthenticatedBlock,
) -> Result<Vec<TempoTransactionReceipt>, ObservationError>
where
    P: Provider<TempoNetwork>,
{
    let receipts = provider
        .get_block_receipts(BlockId::hash(imported.hash()))
        .await
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Receipts, error))?
        .ok_or_else(|| AcquisitionError::missing(AcquisitionSource::L1Receipts, imported.hash()))?;
    authenticate_receipts(imported, &block.transaction_hashes, &receipts)?;
    Ok(receipts)
}

pub(super) fn authenticate_header(
    imported: &ImportedTempoHeader,
    reported_hash: B256,
    computed_hash: B256,
    fetched_header: &TempoHeader,
) -> Result<(), ObservationError> {
    ensure_acquisition_equal(
        AcquisitionSource::L1Block,
        "reported block hash",
        imported.hash(),
        reported_hash,
    )?;
    ensure_acquisition_equal(
        AcquisitionSource::L1Block,
        "locally computed header hash",
        imported.hash(),
        computed_hash,
    )?;
    ensure_acquisition_equal(
        AcquisitionSource::L1Block,
        "block number",
        imported.number(),
        fetched_header.number(),
    )?;
    if fetched_header != imported.header() {
        return Err(AcquisitionError::inconsistent(
            AcquisitionSource::L1Block,
            format!("exact imported header {}", imported.hash()),
            format!("different header with hash {computed_hash}"),
        )
        .into());
    }
    Ok(())
}

pub(super) fn authenticate_receipts(
    imported: &ImportedTempoHeader,
    transaction_hashes: &[B256],
    receipts: &[TempoTransactionReceipt],
) -> Result<(), ObservationError> {
    ensure_acquisition_equal(
        AcquisitionSource::L1Receipts,
        "receipt cardinality",
        transaction_hashes.len(),
        receipts.len(),
    )?;

    for (index, (transaction_hash, receipt)) in transaction_hashes.iter().zip(receipts).enumerate()
    {
        ensure_acquisition_equal(
            AcquisitionSource::L1Receipts,
            format_args!("receipt {index} block hash"),
            Some(imported.hash()),
            receipt.block_hash(),
        )?;
        ensure_acquisition_equal(
            AcquisitionSource::L1Receipts,
            format_args!("receipt {index} block number"),
            Some(imported.number()),
            receipt.block_number(),
        )?;
        ensure_acquisition_equal(
            AcquisitionSource::L1Receipts,
            format_args!("receipt {index} transaction index"),
            Some(index as u64),
            receipt.transaction_index(),
        )?;
        ensure_acquisition_equal(
            AcquisitionSource::L1Receipts,
            format_args!("receipt {index} transaction hash"),
            *transaction_hash,
            receipt.transaction_hash(),
        )?;
    }

    let consensus_receipts = receipts
        .iter()
        .map(|receipt| {
            receipt
                .inner
                .inner
                .clone()
                .map_receipt(|receipt| receipt.map_logs(Into::into))
        })
        .collect::<Vec<_>>();
    let computed_root = alloy_consensus::proofs::calculate_receipt_root(&consensus_receipts);
    ensure_acquisition_equal(
        AcquisitionSource::L1Receipts,
        "receipts root",
        imported.header().receipts_root(),
        computed_root,
    )?;

    let computed_bloom = consensus_receipts
        .iter()
        .fold(Bloom::ZERO, |bloom, receipt| bloom | receipt.bloom_ref());
    ensure_acquisition_equal(
        AcquisitionSource::L1Receipts,
        "logs bloom",
        imported.header().logs_bloom(),
        computed_bloom,
    )
}
