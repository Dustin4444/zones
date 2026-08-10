use alloy_consensus::{
    Header, ReceiptWithBloom, Sealable as _, Signed, TxLegacy, transaction::Recovered,
};
use alloy_eips::Encodable2718 as _;
use alloy_network::TransactionResponse as _;
use alloy_primitives::{Address, B256, Bloom, Bytes, Log, LogData, Signature, U256};
use alloy_provider::ProviderBuilder;
use alloy_rpc_types_eth::{
    Block, BlockTransactions, Header as RpcHeader, Log as RpcLog, Transaction, TransactionReceipt,
};
use alloy_sol_types::{SolCall as _, SolEvent as _};
use alloy_transport::mock::Asserter;
use tempo_alloy::{
    TempoNetwork,
    rpc::{TempoHeaderResponse, TempoTransactionReceipt},
};
use tempo_primitives::{
    TempoHeader, TempoReceipt, TempoTxEnvelope, TempoTxType,
    transaction::{Call, TempoSignature, TempoTransaction},
};
use tempo_zone_contracts::ZonePortal;
use zone_precompiles::ecies::AUTHENTICATED_WITHDRAWAL_ENCRYPTED_SIZE;

use super::{authentication, calls, events, observe_l1};
use crate::observe::{
    abi::{DecodedPortalCall, ImportedTempoHeader, decode_portal_call},
    error::{
        AcquisitionError, AcquisitionSource, AuthenticatedDataEvidence, AuthenticatedTransaction,
        DataSource, ObservationError, PortalCallError, PortalCallFamily, ProtocolChain,
    },
    events::L1ProtocolEvent,
};

const BLOCK_NUMBER: u64 = 42;
const PORTAL: Address = Address::repeat_byte(0x42);
const EXTERNAL: Address = Address::repeat_byte(0xee);

mod exact_header;

fn rpc_log(log: Log, transaction_index: u64, misleading_log_index: u64) -> RpcLog {
    RpcLog {
        inner: log,
        block_hash: Some(B256::repeat_byte(0xfb)),
        block_number: Some(999),
        block_timestamp: None,
        transaction_hash: Some(B256::repeat_byte(0xfa)),
        transaction_index: Some(transaction_index),
        log_index: Some(misleading_log_index),
        removed: false,
    }
}

fn event_log<E: alloy_sol_types::SolEvent>(
    address: Address,
    event: E,
    transaction_index: u64,
    misleading_log_index: u64,
) -> RpcLog {
    rpc_log(
        Log {
            address,
            data: event.encode_log_data(),
        },
        transaction_index,
        misleading_log_index,
    )
}

fn batch_submitted_log(transaction_index: u64, misleading_log_index: u64) -> RpcLog {
    event_log(
        PORTAL,
        ZonePortal::BatchSubmitted {
            withdrawalBatchIndex: 1,
            withdrawalQueueIndex: U256::from(2),
            nextProcessedDepositQueueHash: B256::repeat_byte(3),
            nextBlockHash: B256::repeat_byte(4),
            withdrawalQueueHash: B256::repeat_byte(5),
            lastProcessedDepositNumber: 6,
        },
        transaction_index,
        misleading_log_index,
    )
}

fn withdrawal_processed_log(transaction_index: u64, misleading_log_index: u64) -> RpcLog {
    event_log(
        PORTAL,
        ZonePortal::WithdrawalProcessed {
            to: Address::repeat_byte(1),
            senderTag: B256::repeat_byte(2),
            token: Address::repeat_byte(3),
            amount: 4,
            callbackSuccess: true,
        },
        transaction_index,
        misleading_log_index,
    )
}

fn receipt(
    transaction_hash: B256,
    transaction_index: u64,
    success: bool,
    logs: Vec<RpcLog>,
) -> TempoTransactionReceipt {
    let mut bloom = Bloom::ZERO;
    for log in &logs {
        bloom.accrue_log(&log.inner);
    }
    TempoTransactionReceipt {
        inner: TransactionReceipt {
            inner: ReceiptWithBloom::new(
                TempoReceipt::<RpcLog> {
                    tx_type: TempoTxType::Legacy,
                    success,
                    cumulative_gas_used: 21_000 * (transaction_index + 1),
                    logs,
                },
                bloom,
            ),
            transaction_hash,
            transaction_index: Some(transaction_index),
            block_hash: None,
            block_number: None,
            gas_used: 21_000,
            effective_gas_price: 0,
            blob_gas_used: None,
            blob_gas_price: None,
            from: Address::repeat_byte(0x11),
            to: Some(PORTAL),
            contract_address: None,
        },
        fee_token: None,
        fee_payer: Address::ZERO,
    }
}

fn consensus_receipts(
    receipts: &[TempoTransactionReceipt],
) -> Vec<ReceiptWithBloom<TempoReceipt<Log>>> {
    receipts
        .iter()
        .map(|receipt| {
            receipt
                .inner
                .inner
                .clone()
                .map_receipt(|receipt| receipt.map_logs(Into::into))
        })
        .collect()
}

fn anchor(
    receipts: Vec<TempoTransactionReceipt>,
) -> (ImportedTempoHeader, Vec<TempoTransactionReceipt>) {
    anchor_with_transactions(receipts, &[])
}

fn anchor_with_transactions(
    mut receipts: Vec<TempoTransactionReceipt>,
    transactions: &[TempoTxEnvelope],
) -> (ImportedTempoHeader, Vec<TempoTransactionReceipt>) {
    let consensus = consensus_receipts(&receipts);
    let receipts_root = alloy_consensus::proofs::calculate_receipt_root(&consensus);
    let transactions_root = alloy_consensus::proofs::calculate_transaction_root(transactions);
    let logs_bloom = consensus
        .iter()
        .fold(Bloom::ZERO, |bloom, receipt| bloom | receipt.bloom_ref());
    let header = TempoHeader {
        inner: Header {
            number: BLOCK_NUMBER,
            transactions_root,
            receipts_root,
            logs_bloom,
            ..Default::default()
        },
        ..Default::default()
    };
    let hash = header.hash_slow();
    for receipt in &mut receipts {
        receipt.inner.block_hash = Some(hash);
        receipt.inner.block_number = Some(BLOCK_NUMBER);
    }
    let imported = ImportedTempoHeader::for_test(header);
    (imported, receipts)
}

fn block_response(
    imported: &ImportedTempoHeader,
    envelopes: Vec<TempoTxEnvelope>,
) -> Block<Transaction<TempoTxEnvelope>, TempoHeaderResponse> {
    let transactions = envelopes
        .into_iter()
        .enumerate()
        .map(|(index, envelope)| rpc_transaction(envelope, imported, index as u64))
        .collect();
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
        uncles: vec![],
        transactions: BlockTransactions::Full(transactions),
        withdrawals: None,
    }
}

fn legacy_call(target: Address, calldata: Bytes) -> TempoTxEnvelope {
    TempoTxEnvelope::Legacy(Signed::new_unhashed(
        TxLegacy {
            to: target.into(),
            input: calldata,
            ..Default::default()
        },
        Signature::test_signature(),
    ))
}

fn aa_calls(calls: Vec<Call>) -> TempoTxEnvelope {
    TempoTxEnvelope::AA(
        TempoTransaction {
            calls,
            ..Default::default()
        }
        .into_signed(TempoSignature::from(Signature::test_signature())),
    )
}

fn rpc_transaction(
    envelope: TempoTxEnvelope,
    imported: &ImportedTempoHeader,
    transaction_index: u64,
) -> Transaction<TempoTxEnvelope> {
    Transaction {
        inner: Recovered::new_unchecked(envelope, Address::repeat_byte(0x11)),
        block_hash: Some(imported.hash()),
        block_number: Some(imported.number()),
        transaction_index: Some(transaction_index),
        effective_gas_price: None,
        block_timestamp: None,
    }
}

fn submit_batch_calldata() -> Bytes {
    ZonePortal::submitBatchCall {
        tempoBlockNumber: 1,
        recentTempoBlockNumber: 2,
        blockTransition: ZonePortal::BlockTransition {
            prevBlockHash: B256::repeat_byte(3),
            nextBlockHash: B256::repeat_byte(4),
        },
        depositQueueTransition: ZonePortal::DepositQueueTransition {
            prevProcessedHash: B256::repeat_byte(5),
            nextProcessedHash: B256::repeat_byte(6),
            prevDepositNumber: 7,
            nextDepositNumber: 8,
        },
        withdrawalQueueHash: B256::repeat_byte(9),
        verifierConfig: Bytes::from_static(b"config"),
        proof: Bytes::from_static(b"proof"),
        nextZoneHeight: U256::from(10),
        signatures: vec![Bytes::from_static(b"signature")],
    }
    .abi_encode()
    .into()
}

fn process_withdrawals_calldata(nonempty: bool) -> Bytes {
    let withdrawals = nonempty
        .then(|| ZonePortal::Withdrawal {
            token: Address::repeat_byte(1),
            senderTag: B256::repeat_byte(2),
            to: Address::repeat_byte(3),
            amount: 4,
            memo: B256::repeat_byte(5),
            gasLimit: 6,
            fallbackNonce: 7,
            callbackData: Bytes::new(),
            encryptedSender: Bytes::from(vec![8; AUTHENTICATED_WITHDRAWAL_ENCRYPTED_SIZE]),
        })
        .into_iter()
        .collect();
    ZonePortal::processWithdrawalsCall {
        withdrawals,
        remainingQueue: B256::repeat_byte(9),
    }
    .abi_encode()
    .into()
}

fn assert_inconsistent(error: ObservationError, source: AcquisitionSource) {
    assert!(matches!(
        error,
        ObservationError::Acquisition(AcquisitionError::Inconsistent { kind, .. }) if kind == source
    ));
}

fn assert_unavailable(error: ObservationError, source: AcquisitionSource) {
    assert!(matches!(
        error,
        ObservationError::Acquisition(AcquisitionError::Unavailable { kind, .. }) if kind == source
    ));
}

#[test]
fn imported_header_authentication_requires_number_and_exact_identity() {
    let (imported, _) = anchor(vec![]);
    authentication::authenticate_imported_header(&imported, &imported).unwrap();

    let wrong_number = ImportedTempoHeader::for_test(TempoHeader {
        inner: Header {
            number: imported.number() + 1,
            ..imported.header().inner.clone()
        },
        ..imported.header().clone()
    });
    assert_inconsistent(
        authentication::authenticate_imported_header(&imported, &wrong_number).unwrap_err(),
        AcquisitionSource::L1Block,
    );

    let mut different = imported.header().clone();
    different.inner.gas_limit += 1;
    let different = ImportedTempoHeader::for_test(different);
    assert_inconsistent(
        authentication::authenticate_imported_header(&imported, &different).unwrap_err(),
        AcquisitionSource::L1Block,
    );
}

#[test]
fn receipt_authentication_rejects_every_uncommitted_identity_field() {
    let tx_hash = B256::repeat_byte(0x10);
    let (imported, receipts) = anchor(vec![receipt(tx_hash, 0, true, vec![])]);
    authentication::authenticate_receipts(&imported, &[tx_hash], &receipts).unwrap();

    assert_inconsistent(
        authentication::authenticate_receipts(&imported, &[tx_hash, B256::ZERO], &receipts)
            .unwrap_err(),
        AcquisitionSource::L1Receipts,
    );
    for mutation in 0..4 {
        let mut tampered = receipts.clone();
        match mutation {
            0 => tampered[0].inner.block_hash = Some(B256::repeat_byte(1)),
            1 => tampered[0].inner.block_number = Some(imported.number() + 1),
            2 => tampered[0].inner.transaction_index = Some(1),
            3 => tampered[0].inner.transaction_hash = B256::repeat_byte(2),
            _ => unreachable!(),
        }
        assert_inconsistent(
            authentication::authenticate_receipts(&imported, &[tx_hash], &tampered).unwrap_err(),
            AcquisitionSource::L1Receipts,
        );
    }
}

#[test]
fn receipt_root_and_bloom_are_checked_against_the_imported_header() {
    let tx_hash = B256::repeat_byte(0x10);
    let log = event_log(
        PORTAL,
        ZonePortal::BouncebackGasUpdated { bouncebackGas: 7 },
        0,
        88,
    );
    let (imported, receipts) = anchor(vec![receipt(tx_hash, 0, true, vec![log])]);
    authentication::authenticate_receipts(&imported, &[tx_hash], &receipts).unwrap();

    let mut wrong_root_header = imported.header().clone();
    wrong_root_header.inner.receipts_root = B256::repeat_byte(0xaa);
    let wrong_root = ImportedTempoHeader::for_test(wrong_root_header);
    let mut root_bound_receipts = receipts.clone();
    root_bound_receipts[0].inner.block_hash = Some(wrong_root.hash());
    assert_inconsistent(
        authentication::authenticate_receipts(&wrong_root, &[tx_hash], &root_bound_receipts)
            .unwrap_err(),
        AcquisitionSource::L1Receipts,
    );

    let mut wrong_bloom_header = imported.header().clone();
    wrong_bloom_header.inner.logs_bloom = Bloom::repeat_byte(0xbb);
    let wrong_bloom = ImportedTempoHeader::for_test(wrong_bloom_header);
    let mut bloom_bound_receipts = receipts;
    bloom_bound_receipts[0].inner.block_hash = Some(wrong_bloom.hash());
    assert_inconsistent(
        authentication::authenticate_receipts(&wrong_bloom, &[tx_hash], &bloom_bound_receipts)
            .unwrap_err(),
        AcquisitionSource::L1Receipts,
    );
}

#[test]
fn authenticated_event_order_uses_receipt_vectors_not_rpc_log_metadata() {
    let hashes = [B256::repeat_byte(0x10), B256::repeat_byte(0x20)];
    let external = rpc_log(
        Log {
            address: EXTERNAL,
            data: LogData::new_unchecked(vec![B256::repeat_byte(0xff)], Bytes::new()),
        },
        91,
        900,
    );
    let config = event_log(
        PORTAL,
        ZonePortal::BouncebackGasUpdated { bouncebackGas: 7 },
        92,
        800,
    );
    let ignored = event_log(
        PORTAL,
        ZonePortal::LeaderUpdated {
            previousLeader: Address::repeat_byte(1),
            newLeader: Address::repeat_byte(2),
            epoch: 3,
            activationTempoBlock: 4,
        },
        93,
        700,
    );
    let operation = batch_submitted_log(94, 600);
    let (imported, receipts) = anchor(vec![
        receipt(hashes[0], 0, true, vec![external, config, ignored]),
        receipt(hashes[1], 1, true, vec![operation]),
    ]);
    authentication::authenticate_receipts(&imported, &hashes, &receipts).unwrap();

    let observed = events::ordered_transactions(PORTAL, &hashes, &receipts).unwrap();
    assert_eq!(observed.len(), 2);
    assert!(matches!(
        observed[0].outcomes[0].event,
        L1ProtocolEvent::Portal(ZonePortal::ZonePortalEvents::BouncebackGasUpdated(_))
    ));
    assert_eq!(
        observed[1].required_call,
        Some(PortalCallFamily::SubmitBatch)
    );
}

#[test]
fn authenticated_event_order_preserves_operation_before_config() {
    let hashes = [B256::repeat_byte(0x10), B256::repeat_byte(0x20)];
    let operation = batch_submitted_log(94, 600);
    let external = rpc_log(
        Log {
            address: EXTERNAL,
            data: LogData::new_unchecked(vec![B256::repeat_byte(0xff)], Bytes::new()),
        },
        91,
        900,
    );
    let config = event_log(
        PORTAL,
        ZonePortal::BouncebackGasUpdated { bouncebackGas: 7 },
        92,
        800,
    );
    let ignored = event_log(
        PORTAL,
        ZonePortal::LeaderUpdated {
            previousLeader: Address::repeat_byte(1),
            newLeader: Address::repeat_byte(2),
            epoch: 3,
            activationTempoBlock: 4,
        },
        93,
        700,
    );
    let (imported, receipts) = anchor(vec![
        receipt(hashes[0], 0, true, vec![operation]),
        receipt(hashes[1], 1, true, vec![external, config, ignored]),
    ]);
    authentication::authenticate_receipts(&imported, &hashes, &receipts).unwrap();

    let observed = events::ordered_transactions(PORTAL, &hashes, &receipts).unwrap();
    assert_eq!(observed.len(), 2);

    let operation = &observed[0].outcomes[0];
    assert!(matches!(
        operation.event,
        L1ProtocolEvent::Portal(ZonePortal::ZonePortalEvents::BatchSubmitted(_))
    ));
    assert_eq!(
        observed[0].required_call,
        Some(PortalCallFamily::SubmitBatch)
    );

    let config = &observed[1].outcomes[0];
    assert!(matches!(
        config.event,
        L1ProtocolEvent::Portal(ZonePortal::ZonePortalEvents::BouncebackGasUpdated(_))
    ));
    assert_eq!(observed[1].required_call, None);
}

#[test]
fn malformed_and_unknown_configured_portal_logs_fail_closed_but_external_logs_do_not() {
    let tx_hash = B256::repeat_byte(0x10);
    let external = rpc_log(
        Log {
            address: EXTERNAL,
            data: LogData::new_unchecked(vec![B256::repeat_byte(0x77)], Bytes::new()),
        },
        0,
        0,
    );
    let (imported, receipts) = anchor(vec![receipt(tx_hash, 0, true, vec![external])]);
    authentication::authenticate_receipts(&imported, &[tx_hash], &receipts).unwrap();
    assert!(
        events::ordered_transactions(PORTAL, &[tx_hash], &receipts)
            .unwrap()
            .is_empty()
    );

    for log in [
        rpc_log(
            Log {
                address: PORTAL,
                data: LogData::new_unchecked(vec![B256::repeat_byte(0x66)], Bytes::new()),
            },
            0,
            0,
        ),
        rpc_log(
            Log {
                address: PORTAL,
                data: LogData::new_unchecked(
                    vec![ZonePortal::BouncebackGasUpdated::SIGNATURE_HASH],
                    Bytes::from_static(b"malformed"),
                ),
            },
            0,
            0,
        ),
    ] {
        let (imported, receipts) = anchor(vec![receipt(tx_hash, 0, true, vec![log])]);
        authentication::authenticate_receipts(&imported, &[tx_hash], &receipts).unwrap();
        assert!(matches!(
            events::ordered_transactions(PORTAL, &[tx_hash], &receipts),
            Err(ObservationError::ProtocolEvent { .. })
        ));
    }
}

#[test]
fn one_receipt_cannot_imply_two_portal_call_families() {
    let tx_hash = B256::repeat_byte(0x10);
    let batch = batch_submitted_log(0, 0);
    let processed = withdrawal_processed_log(0, 1);
    let (imported, receipts) = anchor(vec![receipt(tx_hash, 0, true, vec![batch, processed])]);
    authentication::authenticate_receipts(&imported, &[tx_hash], &receipts).unwrap();
    assert!(matches!(
        events::ordered_transactions(PORTAL, &[tx_hash], &receipts),
        Err(ObservationError::PortalCall(PortalCallError::ConflictingFamilies {
            transaction_hash
        })) if transaction_hash == tx_hash
    ));
}

#[test]
fn direct_portal_call_requires_one_top_level_target_for_legacy_and_aa() {
    let calldata = submit_batch_calldata();
    let direct = legacy_call(PORTAL, calldata.clone());
    assert_eq!(
        calls::sole_portal_calldata(&direct, PORTAL, B256::ZERO).unwrap(),
        calldata.as_ref()
    );
    assert!(
        decode_portal_call(
            calls::sole_portal_calldata(&direct, PORTAL, B256::ZERO).unwrap(),
            AuthenticatedTransaction::new(ProtocolChain::TempoL1, 0, B256::ZERO),
        )
        .unwrap()
        .as_submit_batch()
        .is_some()
    );

    let wrong_target = legacy_call(EXTERNAL, calldata.clone());
    assert!(matches!(
        calls::sole_portal_calldata(&wrong_target, PORTAL, B256::ZERO),
        Err(ObservationError::PortalCall(
            PortalCallError::UnsupportedNestedPortalCall {
                target: Some(EXTERNAL),
                ..
            }
        ))
    ));

    let multi = aa_calls(vec![
        Call {
            to: PORTAL.into(),
            value: U256::ZERO,
            input: calldata.clone(),
        },
        Call {
            to: EXTERNAL.into(),
            value: U256::ZERO,
            input: Bytes::new(),
        },
    ]);
    assert!(matches!(
        calls::sole_portal_calldata(&multi, PORTAL, B256::ZERO),
        Err(ObservationError::PortalCall(
            PortalCallError::UnsupportedNestedPortalCall { .. }
        ))
    ));

    let one_aa = aa_calls(vec![Call {
        to: PORTAL.into(),
        value: U256::ZERO,
        input: calldata.clone(),
    }]);
    assert_eq!(
        calls::sole_portal_calldata(&one_aa, PORTAL, B256::ZERO).unwrap(),
        calldata.as_ref()
    );
}

#[test]
fn full_transaction_binding_checks_hash_block_number_index_and_root() {
    let envelope = legacy_call(PORTAL, submit_batch_calldata());
    let expected_hash = envelope.trie_hash();
    let (imported, _) = anchor_with_transactions(vec![], std::slice::from_ref(&envelope));
    let transaction = rpc_transaction(envelope, &imported, 0);
    assert_eq!(transaction.tx_hash(), expected_hash);
    authentication::authenticate_transactions(&imported, std::slice::from_ref(&transaction))
        .unwrap();

    let mut mutations = Vec::new();
    let mut wrong_hash = transaction.clone();
    wrong_hash.inner = Recovered::new_unchecked(
        legacy_call(PORTAL, process_withdrawals_calldata(false)),
        Address::repeat_byte(0x11),
    );
    mutations.push(wrong_hash);
    let mut wrong_block_hash = transaction.clone();
    wrong_block_hash.block_hash = Some(B256::repeat_byte(0xaa));
    mutations.push(wrong_block_hash);
    let mut wrong_block_number = transaction.clone();
    wrong_block_number.block_number = Some(imported.number() + 1);
    mutations.push(wrong_block_number);
    let mut wrong_index = transaction;
    wrong_index.transaction_index = Some(1);
    mutations.push(wrong_index);

    for transaction in mutations {
        assert_inconsistent(
            authentication::authenticate_transactions(&imported, &[transaction]).unwrap_err(),
            AcquisitionSource::L1Transaction,
        );
    }
}

#[test]
fn transaction_authentication_rejects_order_count_and_valid_uncommitted_portal_body() {
    let first = legacy_call(PORTAL, submit_batch_calldata());
    let second = legacy_call(PORTAL, process_withdrawals_calldata(true));
    let (imported, _) = anchor_with_transactions(vec![], &[first.clone(), second.clone()]);
    let transactions = vec![
        rpc_transaction(first.clone(), &imported, 0),
        rpc_transaction(second, &imported, 1),
    ];
    authentication::authenticate_transactions(&imported, &transactions).unwrap();

    let mut reordered = transactions.clone();
    reordered.swap(0, 1);
    assert_inconsistent(
        authentication::authenticate_transactions(&imported, &reordered).unwrap_err(),
        AcquisitionSource::L1Transaction,
    );
    assert_inconsistent(
        authentication::authenticate_transactions(&imported, &transactions[..1]).unwrap_err(),
        AcquisitionSource::L1Transaction,
    );

    let fake_portal_body = legacy_call(PORTAL, process_withdrawals_calldata(false));
    let (single_imported, _) = anchor_with_transactions(vec![], std::slice::from_ref(&first));
    let fake = rpc_transaction(fake_portal_body, &single_imported, 0);
    assert_inconsistent(
        authentication::authenticate_transactions(&single_imported, &[fake]).unwrap_err(),
        AcquisitionSource::L1Transaction,
    );
}

#[tokio::test]
async fn empty_process_withdrawals_without_events_causes_no_transaction_fetch() {
    let envelope = legacy_call(PORTAL, process_withdrawals_calldata(false));
    let tx_hash = envelope.trie_hash();
    let (imported, receipts) = anchor_with_transactions(
        vec![receipt(tx_hash, 0, true, vec![])],
        std::slice::from_ref(&envelope),
    );
    let block = block_response(&imported, vec![envelope]);
    let asserter = Asserter::new();
    asserter.push_success(&Some(block));
    asserter.push_success(&Some(receipts));
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter.clone());

    let observed = observe_l1(&provider, &imported, PORTAL).await.unwrap();
    assert!(observed.protocol_transactions.is_empty());
    assert!(asserter.read_q().is_empty());
}

#[tokio::test]
async fn eventful_submit_batch_fetches_once_and_decodes_direct_calldata() {
    let envelope = legacy_call(PORTAL, submit_batch_calldata());
    let tx_hash = envelope.trie_hash();
    let batch_event = batch_submitted_log(0, 500);
    let (imported, receipts) = anchor_with_transactions(
        vec![receipt(tx_hash, 0, true, vec![batch_event])],
        std::slice::from_ref(&envelope),
    );
    let block = block_response(&imported, vec![envelope]);
    let asserter = Asserter::new();
    asserter.push_success(&Some(block));
    asserter.push_success(&Some(receipts));
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter.clone());

    let observed = observe_l1(&provider, &imported, PORTAL).await.unwrap();
    assert_eq!(observed.protocol_transactions.len(), 1);
    assert!(
        observed.protocol_transactions[0]
            .direct_call
            .as_ref()
            .and_then(DecodedPortalCall::as_submit_batch)
            .is_some()
    );
    assert_eq!(observed.protocol_transactions[0].outcomes.len(), 1);
    assert!(asserter.read_q().is_empty());
}

#[tokio::test]
async fn eventful_process_withdrawals_fetches_once_and_retains_input_and_outcome() {
    let envelope = legacy_call(PORTAL, process_withdrawals_calldata(true));
    let tx_hash = envelope.trie_hash();
    let process_event = withdrawal_processed_log(0, 500);
    let (imported, receipts) = anchor_with_transactions(
        vec![receipt(tx_hash, 0, true, vec![process_event])],
        std::slice::from_ref(&envelope),
    );
    let block = block_response(&imported, vec![envelope]);
    let asserter = Asserter::new();
    asserter.push_success(&Some(block));
    asserter.push_success(&Some(receipts));
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter.clone());

    let observed = observe_l1(&provider, &imported, PORTAL).await.unwrap();
    let [transaction] = observed.protocol_transactions() else {
        panic!("expected one protocol transaction");
    };
    let call = transaction
        .direct_call()
        .and_then(DecodedPortalCall::as_process_withdrawals)
        .expect("authenticated processWithdrawals input");
    assert_eq!(call.withdrawals.len(), 1);
    assert_eq!(call.remainingQueue, B256::repeat_byte(9));
    let [outcome] = transaction.outcomes() else {
        panic!("expected one ordered outcome");
    };
    assert!(matches!(
        outcome.event(),
        L1ProtocolEvent::Portal(ZonePortal::ZonePortalEvents::WithdrawalProcessed(_))
    ));
    assert!(asserter.read_q().is_empty());
}

#[tokio::test]
async fn eventful_submit_batch_rejects_wrong_target_direct_call() {
    let envelope = legacy_call(EXTERNAL, submit_batch_calldata());
    let tx_hash = envelope.trie_hash();
    let batch_event = batch_submitted_log(0, 500);
    let (imported, receipts) = anchor_with_transactions(
        vec![receipt(tx_hash, 0, true, vec![batch_event])],
        std::slice::from_ref(&envelope),
    );
    let block = block_response(&imported, vec![envelope]);
    let asserter = Asserter::new();
    asserter.push_success(&Some(block));
    asserter.push_success(&Some(receipts));
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter.clone());

    assert!(matches!(
        observe_l1(&provider, &imported, PORTAL).await,
        Err(ObservationError::PortalCall(PortalCallError::UnsupportedNestedPortalCall {
            transaction_hash,
            target: Some(target),
        })) if transaction_hash == tx_hash && target == EXTERNAL
    ));
    assert!(asserter.read_q().is_empty());
}

#[tokio::test]
async fn authenticated_event_and_direct_call_family_must_match() {
    let envelope = legacy_call(PORTAL, submit_batch_calldata());
    let tx_hash = envelope.trie_hash();
    let process_event = withdrawal_processed_log(0, 0);
    let (imported, receipts) = anchor_with_transactions(
        vec![receipt(tx_hash, 0, true, vec![process_event])],
        std::slice::from_ref(&envelope),
    );
    let block = block_response(&imported, vec![envelope]);
    let asserter = Asserter::new();
    asserter.push_success(&Some(block));
    asserter.push_success(&Some(receipts));
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);

    assert!(matches!(
        observe_l1(&provider, &imported, PORTAL).await,
        Err(ObservationError::PortalCall(
            PortalCallError::FamilyMismatch {
                expected: PortalCallFamily::ProcessWithdrawals,
                actual: PortalCallFamily::SubmitBatch,
                ..
            }
        ))
    ));
}

#[tokio::test]
async fn eventful_empty_process_withdrawals_fails_closed() {
    let envelope = legacy_call(PORTAL, process_withdrawals_calldata(false));
    let tx_hash = envelope.trie_hash();
    let process_event = withdrawal_processed_log(0, 0);
    let (imported, receipts) = anchor_with_transactions(
        vec![receipt(tx_hash, 0, true, vec![process_event])],
        std::slice::from_ref(&envelope),
    );
    let block = block_response(&imported, vec![envelope]);
    let asserter = Asserter::new();
    asserter.push_success(&Some(block));
    asserter.push_success(&Some(receipts));
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);

    assert!(matches!(
        observe_l1(&provider, &imported, PORTAL).await,
        Err(ObservationError::PortalCall(
            PortalCallError::EmptyProcessWithOutcomes { .. }
        ))
    ));
}

#[tokio::test]
async fn eventful_malformed_direct_calldata_fails_closed() {
    let mut malformed = submit_batch_calldata().to_vec();
    malformed.push(0);
    let evidence = AuthenticatedDataEvidence::from_bytes(&malformed);
    let envelope = legacy_call(PORTAL, malformed.into());
    let tx_hash = envelope.trie_hash();
    let batch_event = batch_submitted_log(0, 0);
    let (imported, receipts) = anchor_with_transactions(
        vec![receipt(tx_hash, 0, true, vec![batch_event])],
        std::slice::from_ref(&envelope),
    );
    let block = block_response(&imported, vec![envelope]);
    let asserter = Asserter::new();
    asserter.push_success(&Some(block));
    asserter.push_success(&Some(receipts));
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);

    assert!(matches!(
        observe_l1(&provider, &imported, PORTAL).await,
        Err(ObservationError::MalformedAuthenticatedData {
            kind: DataSource::SubmitBatchCalldata,
            transaction,
            evidence: actual_evidence,
            ..
        }) if transaction
            == AuthenticatedTransaction::new(ProtocolChain::TempoL1, 0, tx_hash)
            && actual_evidence == evidence
    ));
}

#[tokio::test]
async fn missing_block_receipts_and_incomplete_transaction_blocks_are_source_classified() {
    let (empty_imported, _) = anchor(vec![]);
    let asserter = Asserter::new();
    asserter
        .push_success(&Option::<Block<Transaction<TempoTxEnvelope>, TempoHeaderResponse>>::None);
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);
    assert!(matches!(
        observe_l1(&provider, &empty_imported, PORTAL).await,
        Err(ObservationError::Acquisition(AcquisitionError::Missing {
            kind: AcquisitionSource::L1Block,
            ..
        }))
    ));

    let block = block_response(&empty_imported, vec![]);
    let asserter = Asserter::new();
    asserter.push_success(&Some(block));
    asserter.push_success(&Option::<Vec<TempoTransactionReceipt>>::None);
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);
    assert!(matches!(
        observe_l1(&provider, &empty_imported, PORTAL).await,
        Err(ObservationError::Acquisition(AcquisitionError::Missing {
            kind: AcquisitionSource::L1Receipts,
            ..
        }))
    ));

    let envelope = legacy_call(PORTAL, submit_batch_calldata());
    let tx_hash = envelope.trie_hash();
    let (imported, _) = anchor_with_transactions(vec![], std::slice::from_ref(&envelope));
    let mut block = block_response(&imported, vec![envelope]);
    block.transactions = BlockTransactions::Hashes(vec![tx_hash]);
    let asserter = Asserter::new();
    asserter.push_success(&Some(block));
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);
    assert!(matches!(
        observe_l1(&provider, &imported, PORTAL).await,
        Err(ObservationError::Acquisition(
            AcquisitionError::Inconsistent {
                kind: AcquisitionSource::L1Transaction,
                ..
            }
        ))
    ));
}

#[tokio::test]
async fn transport_failures_are_unavailable_and_source_classified() {
    let (empty_imported, _) = anchor(vec![]);
    let asserter = Asserter::new();
    asserter.push_failure_msg("block transport failure");
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);
    assert_unavailable(
        observe_l1(&provider, &empty_imported, PORTAL)
            .await
            .unwrap_err(),
        AcquisitionSource::L1Block,
    );

    let block = block_response(&empty_imported, vec![]);
    let asserter = Asserter::new();
    asserter.push_success(&Some(block));
    asserter.push_failure_msg("receipt transport failure");
    let provider =
        ProviderBuilder::new_with_network::<TempoNetwork>().connect_mocked_client(asserter);
    assert_unavailable(
        observe_l1(&provider, &empty_imported, PORTAL)
            .await
            .unwrap_err(),
        AcquisitionSource::L1Receipts,
    );
}
