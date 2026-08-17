//! Events tests.

use super::*;

#[test]
fn outcomes_preserve_config_before_operation_order() {
    let (user_hash, observation) = observe_user_logs(vec![
        tempo_gas_rate_updated_log(),
        withdrawal_requested_log(),
    ]);
    let user_events = &observation.outcomes().events()[2..];
    assert_eq!(
        user_events[0].position(),
        L2EventPosition {
            transaction_index: 1,
            receipt_log_index: 0,
            block_log_index: 2,
            transaction_hash: user_hash,
            transaction_sender: Address::repeat_byte(0x44),
        }
    );
    assert_eq!(
        user_events[1].position(),
        L2EventPosition {
            transaction_index: 1,
            receipt_log_index: 1,
            block_log_index: 3,
            transaction_hash: user_hash,
            transaction_sender: Address::repeat_byte(0x44),
        }
    );
    assert!(matches!(
        user_events[0].event(),
        L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::TempoGasRateUpdated(_))
    ));
    assert!(matches!(
        user_events[1].event(),
        L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::WithdrawalRequested(event))
            if event.sender == Address::repeat_byte(0x45)
    ));
}

#[test]
fn outcomes_preserve_operation_before_config_order() {
    let (user_hash, observation) = observe_user_logs(vec![
        withdrawal_requested_log(),
        tempo_gas_rate_updated_log(),
    ]);
    let user_events = &observation.outcomes().events()[2..];
    assert_eq!(
        user_events[0].position(),
        L2EventPosition {
            transaction_index: 1,
            receipt_log_index: 0,
            block_log_index: 2,
            transaction_hash: user_hash,
            transaction_sender: Address::repeat_byte(0x44),
        }
    );
    assert_eq!(
        user_events[1].position(),
        L2EventPosition {
            transaction_index: 1,
            receipt_log_index: 1,
            block_log_index: 3,
            transaction_hash: user_hash,
            transaction_sender: Address::repeat_byte(0x44),
        }
    );
    assert!(matches!(
        user_events[0].event(),
        L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::WithdrawalRequested(_))
    ));
    assert!(matches!(
        user_events[1].event(),
        L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::TempoGasRateUpdated(_))
    ));
}

#[test]
fn protocol_event_surface_fails_closed_and_external_logs_are_ignored() {
    let (block, mut receipts) = basic_fixture();
    receipts[0].logs.insert(
        0,
        Log {
            address: Address::repeat_byte(0x99),
            data: LogData::new_unchecked(vec![B256::repeat_byte(0xaa)], Bytes::new()),
        },
    );
    let block = reseal_with_receipts(block, &receipts);
    let observation = observe_l2_block(&block, &receipts).unwrap();
    assert_eq!(observation.outcomes.events.len(), 2);

    let (block, mut receipts) = basic_fixture();
    let transaction_hash = *block.body().transactions[0].tx_hash();
    receipts[0].logs.insert(
        0,
        Log {
            address: ZONE_INBOX_ADDRESS,
            data: LogData::new_unchecked(vec![B256::repeat_byte(0xff)], Bytes::new()),
        },
    );
    let block = reseal_with_receipts(block, &receipts);
    let (error, imported_tempo) = observe_l2_block_with_context(&block, &receipts)
        .unwrap_err()
        .into_parts();
    assert_eq!(
        imported_tempo,
        Some(BlockNumHash::new(100, imported_header().hash_slow()))
    );
    assert!(matches!(
        error,
        ObservationError::ProtocolEvent {
            chain: ProtocolChain::ZoneL2,
            transaction_index: 0,
            receipt_log_index: 0,
            block_log_index: 0,
            transaction_hash: actual_hash,
            error,
        } if actual_hash == transaction_hash
            && matches!(error.as_ref(), crate::observe::events::ProtocolEventError::UnsupportedProtocolEvent { .. })
    ));

    let (block, mut receipts) = basic_fixture();
    receipts[0].logs.insert(
        0,
        Log {
            address: ZONE_INBOX_ADDRESS,
            data: LogData::new_unchecked(
                vec![IZoneInbox::TempoAdvanced::SIGNATURE_HASH],
                Bytes::from(vec![0xde, 0xad]),
            ),
        },
    );
    let block = reseal_with_receipts(block, &receipts);
    assert!(matches!(
        observe_l2_block(&block, &receipts),
        Err(ObservationError::ProtocolEvent { error, .. })
            if matches!(error.as_ref(), crate::observe::events::ProtocolEventError::MalformedProtocolEvent { .. })
    ));
}

#[test]
fn deposit_rejected_is_unsupported_not_a_failed_deposit() {
    let (block, mut receipts) = basic_fixture();
    receipts[0].logs.insert(
        0,
        Log {
            address: ZONE_INBOX_ADDRESS,
            data: IZoneInbox::DepositRejected {
                depositHash: B256::repeat_byte(0x01),
                sender: Address::repeat_byte(0x02),
                depositType: IZoneInbox::DepositType::Deposit,
                token: Address::repeat_byte(0x03),
                amount: 4,
                tempoRefundRecipient: Address::repeat_byte(0x05),
            }
            .encode_log_data(),
        },
    );
    let block = reseal_with_receipts(block, &receipts);
    assert!(matches!(
        observe_l2_block(&block, &receipts),
        Err(ObservationError::ProtocolEvent { error, .. })
            if matches!(error.as_ref(), crate::observe::events::ProtocolEventError::UnsupportedProtocolEvent { .. })
    ));
}
