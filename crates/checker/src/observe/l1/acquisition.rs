//! Exact-header and complete-receipt authentication.

use alloy_consensus::{BlockHeader as _, Sealable as _};
use alloy_eips::BlockId;
use alloy_network::{
    BlockResponse as _, ReceiptResponse as _, TransactionResponse as _,
    primitives::HeaderResponse as _,
};
use alloy_primitives::{B256, Bloom};
use alloy_provider::Provider;
use tempo_alloy::{TempoNetwork, rpc::TempoTransactionReceipt};
use tempo_primitives::{TempoHeader, TempoTxEnvelope};

use super::super::{
    abi::ImportedTempoHeader,
    error::{AcquisitionError, AcquisitionSource, ObservationError, ensure_acquisition_equal},
};

/// A full L1 block whose reported hash and transaction root have been checked.
pub(super) struct AuthenticatedBlock {
    header: ImportedTempoHeader,
    pub(super) transactions: Vec<TempoTransactionResponse>,
}

pub(super) type TempoTransactionResponse =
    <TempoNetwork as alloy_network::Network>::TransactionResponse;

/// Acquire and authenticate the header for one operator-selected exact hash.
pub(crate) async fn acquire_l1_header<P>(
    provider: &P,
    block_hash: B256,
) -> Result<ImportedTempoHeader, ObservationError>
where
    P: Provider<TempoNetwork>,
{
    Ok(acquire_exact_block(provider, block_hash).await?.header)
}

/// Fetch the exact imported block and require its full header to match L2's input.
pub(super) async fn acquire_block<P>(
    provider: &P,
    imported: &ImportedTempoHeader,
) -> Result<AuthenticatedBlock, ObservationError>
where
    P: Provider<TempoNetwork>,
{
    let block = acquire_exact_block(provider, imported.hash()).await?;
    authenticate_imported_header(imported, &block.header)?;
    Ok(block)
}

/// Fetch one hash-addressed block and authenticate its header and transaction set.
async fn acquire_exact_block<P>(
    provider: &P,
    block_hash: B256,
) -> Result<AuthenticatedBlock, ObservationError>
where
    P: Provider<TempoNetwork>,
{
    let block = provider
        .get_block_by_hash(block_hash)
        .full()
        .await
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Block, error))?
        .ok_or_else(|| AcquisitionError::missing(AcquisitionSource::L1Block, block_hash))?;

    let response_header = block.header();
    let fetched_header: &TempoHeader = response_header.as_ref();
    authenticate_header_hash(
        block_hash,
        response_header.hash(),
        fetched_header.hash_slow(),
    )?;

    let transactions = block
        .transactions()
        .as_transactions()
        .ok_or_else(|| {
            AcquisitionError::inconsistent(
                AcquisitionSource::L1Transaction,
                "complete transaction envelopes",
                "transaction hashes only",
            )
        })?
        .to_vec();
    authenticate_transactions(
        &ImportedTempoHeader::new(fetched_header.clone()),
        &transactions,
    )?;

    Ok(AuthenticatedBlock {
        header: ImportedTempoHeader::new(fetched_header.clone()),
        transactions,
    })
}

/// Fetch the complete receipt stream over RPC, independent of the block body.
pub(super) async fn fetch_receipts<P>(
    provider: &P,
    imported: &ImportedTempoHeader,
) -> Result<Vec<TempoTransactionReceipt>, ObservationError>
where
    P: Provider<TempoNetwork>,
{
    Ok(provider
        .get_block_receipts(BlockId::hash(imported.hash()))
        .await
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Receipts, error))?
        .ok_or_else(|| AcquisitionError::missing(AcquisitionSource::L1Receipts, imported.hash()))?)
}

/// Authenticate a fetched receipt stream against the imported header and block.
pub(super) fn verify_receipts(
    imported: &ImportedTempoHeader,
    block: &AuthenticatedBlock,
    receipts: Vec<TempoTransactionReceipt>,
) -> Result<Vec<TempoTransactionReceipt>, ObservationError> {
    let transaction_hashes = block
        .transactions
        .iter()
        .map(|transaction| transaction.tx_hash())
        .collect::<Vec<_>>();
    authenticate_receipts(imported, &transaction_hashes, &receipts)?;
    Ok(receipts)
}

/// Require transaction identities and their trie root to match the imported header.
pub(super) fn authenticate_transactions(
    imported: &ImportedTempoHeader,
    transactions: &[TempoTransactionResponse],
) -> Result<(), ObservationError> {
    let mut envelopes = Vec::<TempoTxEnvelope>::with_capacity(transactions.len());
    for (index, transaction) in transactions.iter().enumerate() {
        let envelope: &TempoTxEnvelope = transaction.as_ref();
        let computed_hash = alloy_eips::Encodable2718::trie_hash(envelope);
        ensure_acquisition_equal(
            AcquisitionSource::L1Transaction,
            format_args!("transaction {index} locally computed hash"),
            computed_hash,
            transaction.tx_hash(),
        )?;
        ensure_acquisition_equal(
            AcquisitionSource::L1Transaction,
            format_args!("transaction {index} block hash"),
            Some(imported.hash()),
            transaction.block_hash(),
        )?;
        ensure_acquisition_equal(
            AcquisitionSource::L1Transaction,
            format_args!("transaction {index} block number"),
            Some(imported.number()),
            transaction.block_number(),
        )?;
        ensure_acquisition_equal(
            AcquisitionSource::L1Transaction,
            format_args!("transaction {index} index"),
            Some(index as u64),
            transaction.transaction_index(),
        )?;
        envelopes.push(envelope.clone());
    }

    let computed_root = alloy_consensus::proofs::calculate_transaction_root(&envelopes);
    ensure_acquisition_equal(
        AcquisitionSource::L1Transaction,
        "transactions root",
        imported.header().transactions_root(),
        computed_root,
    )
}

/// Require both the RPC-reported and locally computed hashes to match the request.
fn authenticate_header_hash(
    expected: B256,
    reported: B256,
    computed: B256,
) -> Result<(), ObservationError> {
    ensure_acquisition_equal(
        AcquisitionSource::L1Block,
        "reported block hash",
        expected,
        reported,
    )?;
    ensure_acquisition_equal(
        AcquisitionSource::L1Block,
        "locally computed header hash",
        expected,
        computed,
    )
}

/// Require a fetched header to be the exact header carried by `advanceTempo`.
pub(super) fn authenticate_imported_header(
    imported: &ImportedTempoHeader,
    fetched: &ImportedTempoHeader,
) -> Result<(), ObservationError> {
    ensure_acquisition_equal(
        AcquisitionSource::L1Block,
        "block number",
        imported.number(),
        fetched.number(),
    )?;
    if fetched.header() != imported.header() {
        return Err(AcquisitionError::inconsistent(
            AcquisitionSource::L1Block,
            format!("exact imported header {}", imported.hash()),
            format!("different header with hash {}", fetched.hash()),
        )
        .into());
    }
    Ok(())
}

/// Require receipt identities, root, and bloom to match the imported header.
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
