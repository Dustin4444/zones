//! Observation tests.

use super::*;

#[test]
fn observes_decoded_header_input_and_ordered_protocol_events() {
    let (block, receipts) = basic_fixture();
    let observation = observe_l2_block(&block, &receipts).unwrap();

    assert_eq!(observation.block_hash, block.hash());
    assert_eq!(observation.parent_hash(), ZONE_PARENT_HASH);
    assert_eq!(
        observation.inputs.advance_tempo.imported_header().hash(),
        imported_header().hash_slow()
    );
    assert_eq!(observation.outcomes.events.len(), 2);
}

#[test]
fn authenticated_inputs_do_not_require_matching_event_outputs() {
    let (block, _) = basic_fixture();
    let receipts = [receipt(true, Vec::new())];
    let block = reseal_with_receipts(block, &receipts);
    let observation = observe_l2_block(&block, &receipts).unwrap();

    assert_eq!(
        observation.inputs.advance_tempo.imported_header().hash(),
        imported_header().hash_slow()
    );
    assert!(observation.outcomes.events.is_empty());
}

#[test]
fn tempo_advanced_cursor_is_retained_for_later_evaluation() {
    let (block, mut receipts) = basic_fixture();
    receipts[0].logs[1] = Log {
        address: ZONE_INBOX_ADDRESS,
        data: IZoneInbox::TempoAdvanced {
            tempoBlockHash: imported_header().hash_slow(),
            tempoBlockNumber: imported_header().inner.number,
            depositsProcessed: U256::ZERO,
            newProcessedDepositQueueHash: B256::repeat_byte(0xa1),
            lastProcessedDepositNumber: 91,
        }
        .encode_log_data(),
    };

    let block = reseal_with_receipts(block, &receipts);
    let observation = observe_l2_block(&block, &receipts).unwrap();
    assert!(matches!(
        &observation.outcomes.events[1].event,
        L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TempoAdvanced(event))
            if event.newProcessedDepositQueueHash == B256::repeat_byte(0xa1)
                && event.lastProcessedDepositNumber == 91
    ));
}

#[test]
fn mismatched_tempo_advanced_is_retained_independently_from_calldata() {
    let (block, mut receipts) = basic_fixture();
    let forged_hash = B256::repeat_byte(0xee);
    receipts[0].logs = advance_logs(Some(forged_hash));

    let block = reseal_with_receipts(block, &receipts);
    let observation = observe_l2_block(&block, &receipts).unwrap();
    assert_eq!(
        observation.inputs.advance_tempo.imported_header().hash(),
        imported_header().hash_slow()
    );
    assert!(matches!(
        &observation.outcomes.events[1].event,
        L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TempoAdvanced(event))
            if event.tempoBlockHash == forged_hash
    ));
}

#[test]
fn mismatched_enabled_token_output_is_retained_for_later_evaluation() {
    let transactions = vec![advance_transaction_with_tokens(
        ZONE_INBOX_ADDRESS,
        vec![IZoneInbox::EnabledToken {
            token: Address::repeat_byte(0x71),
            name: "Token".into(),
            symbol: "TKN".into(),
            currency: "USD".into(),
        }],
    )];
    let mut logs = advance_logs(None);
    logs.push(token_enabled_log("TKN"));
    let mut receipts = vec![receipt(true, logs)];
    let block = recovered_block(transactions, vec![Address::ZERO], &receipts);

    let observation = observe_l2_block(&block, &receipts).unwrap();
    assert_eq!(observation.inputs.advance_tempo.enabled_tokens().len(), 1);

    receipts[0].logs[2] = token_enabled_log("BAD");
    let block = reseal_with_receipts(block, &receipts);
    let observation = observe_l2_block(&block, &receipts).unwrap();
    assert_eq!(
        observation.inputs.advance_tempo.enabled_tokens()[0].symbol,
        "TKN"
    );
    assert!(matches!(
        &observation.outcomes.events[2].event,
        L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TokenEnabled(event))
            if event.symbol == "BAD"
    ));
}

#[test]
fn notification_cardinality_errors_are_acquisition_failures() {
    let (block, receipts) = basic_fixture();
    let error = observe_l2_block(&block, &Vec::<TempoReceipt<Log>>::new()).unwrap_err();
    assert!(matches!(
        error,
        ObservationError::Acquisition(AcquisitionError::Inconsistent {
            kind: AcquisitionSource::ZoneNotificationReceipts,
            ..
        })
    ));

    let without_sender = recovered_block(
        vec![advance_transaction(ZONE_INBOX_ADDRESS)],
        vec![],
        &receipts,
    );
    let error = observe_l2_block(&without_sender, &receipts).unwrap_err();
    assert!(matches!(
        error,
        ObservationError::Acquisition(AcquisitionError::Inconsistent {
            kind: AcquisitionSource::ZoneNotificationBlock,
            ..
        })
    ));
}
