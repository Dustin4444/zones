//! Authentication tests.

use super::*;

#[test]
fn imported_header_authentication_requires_number_and_exact_identity() {
    let (imported, _) = anchor(vec![]);
    acquisition::authenticate_imported_header(&imported, &imported).unwrap();

    let wrong_number = ImportedTempoHeader::new(TempoHeader {
        inner: Header {
            number: imported.number() + 1,
            ..imported.header().inner.clone()
        },
        ..imported.header().clone()
    });
    assert_inconsistent(
        acquisition::authenticate_imported_header(&imported, &wrong_number).unwrap_err(),
        AcquisitionSource::L1Block,
    );

    let mut different = imported.header().clone();
    different.inner.gas_limit += 1;
    let different = ImportedTempoHeader::new(different);
    assert_inconsistent(
        acquisition::authenticate_imported_header(&imported, &different).unwrap_err(),
        AcquisitionSource::L1Block,
    );
}

#[test]
fn receipt_authentication_rejects_every_uncommitted_identity_field() {
    let tx_hash = B256::repeat_byte(0x10);
    let (imported, receipts) = anchor(vec![receipt(tx_hash, 0, true, vec![])]);
    acquisition::authenticate_receipts(&imported, &[tx_hash], &receipts).unwrap();

    assert_inconsistent(
        acquisition::authenticate_receipts(&imported, &[tx_hash, B256::ZERO], &receipts)
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
            acquisition::authenticate_receipts(&imported, &[tx_hash], &tampered).unwrap_err(),
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
    acquisition::authenticate_receipts(&imported, &[tx_hash], &receipts).unwrap();

    let mut wrong_root_header = imported.header().clone();
    wrong_root_header.inner.receipts_root = B256::repeat_byte(0xaa);
    let wrong_root = ImportedTempoHeader::new(wrong_root_header);
    let mut root_bound_receipts = receipts.clone();
    root_bound_receipts[0].inner.block_hash = Some(wrong_root.hash());
    assert_inconsistent(
        acquisition::authenticate_receipts(&wrong_root, &[tx_hash], &root_bound_receipts)
            .unwrap_err(),
        AcquisitionSource::L1Receipts,
    );

    let mut wrong_bloom_header = imported.header().clone();
    wrong_bloom_header.inner.logs_bloom = Bloom::repeat_byte(0xbb);
    let wrong_bloom = ImportedTempoHeader::new(wrong_bloom_header);
    let mut bloom_bound_receipts = receipts;
    bloom_bound_receipts[0].inner.block_hash = Some(wrong_bloom.hash());
    assert_inconsistent(
        acquisition::authenticate_receipts(&wrong_bloom, &[tx_hash], &bloom_bound_receipts)
            .unwrap_err(),
        AcquisitionSource::L1Receipts,
    );
}

#[test]
fn full_transaction_binding_checks_hash_block_number_index_and_root() {
    let envelope = legacy_call(PORTAL, submit_batch_calldata());
    let expected_hash = envelope.trie_hash();
    let (imported, _) = anchor_with_transactions(vec![], std::slice::from_ref(&envelope));
    let transaction = rpc_transaction(envelope, &imported, 0);
    assert_eq!(transaction.tx_hash(), expected_hash);
    acquisition::authenticate_transactions(&imported, std::slice::from_ref(&transaction)).unwrap();

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
            acquisition::authenticate_transactions(&imported, &[transaction]).unwrap_err(),
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
    acquisition::authenticate_transactions(&imported, &transactions).unwrap();

    let mut reordered = transactions.clone();
    reordered.swap(0, 1);
    assert_inconsistent(
        acquisition::authenticate_transactions(&imported, &reordered).unwrap_err(),
        AcquisitionSource::L1Transaction,
    );
    assert_inconsistent(
        acquisition::authenticate_transactions(&imported, &transactions[..1]).unwrap_err(),
        AcquisitionSource::L1Transaction,
    );

    let fake_portal_body = legacy_call(PORTAL, process_withdrawals_calldata(false));
    let (single_imported, _) = anchor_with_transactions(vec![], std::slice::from_ref(&first));
    let fake = rpc_transaction(fake_portal_body, &single_imported, 0);
    assert_inconsistent(
        acquisition::authenticate_transactions(&single_imported, &[fake]).unwrap_err(),
        AcquisitionSource::L1Transaction,
    );
}
