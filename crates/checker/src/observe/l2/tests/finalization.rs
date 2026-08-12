//! Finalization tests.

use super::*;

#[test]
fn finalization_is_unique_final_and_retains_its_event_output() {
    let advance = advance_transaction(ZONE_INBOX_ADDRESS);
    let finalize = finalization_transaction(ZONE_NUMBER);
    let finalize_hash = *finalize.tx_hash();
    let receipts = vec![
        receipt(true, advance_logs(None)),
        receipt(
            true,
            vec![Log {
                address: ZONE_OUTBOX_ADDRESS,
                data: IZoneOutbox::BatchFinalized {
                    withdrawalQueueHash: B256::repeat_byte(0xa1),
                    withdrawalBatchIndex: 91,
                }
                .encode_log_data(),
            }],
        ),
    ];
    let block = recovered_block(
        vec![advance, finalize],
        vec![Address::ZERO, Address::ZERO],
        &receipts,
    );
    let observation = observe_l2_block(&block, &receipts).unwrap();
    assert_eq!(
        observation.inputs.finalization.unwrap().transaction_hash,
        finalize_hash
    );
    assert!(matches!(
        &observation.outcomes.events.last().unwrap().event,
        L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::BatchFinalized(event))
            if event.withdrawalQueueHash == B256::repeat_byte(0xa1)
                && event.withdrawalBatchIndex == 91
    ));
}

#[test]
fn finalization_rejects_wrong_block_number_and_position() {
    let receipts = vec![receipt(true, advance_logs(None)), receipt(true, vec![])];
    let wrong_number = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            finalization_transaction(ZONE_NUMBER + 1),
        ],
        vec![Address::ZERO, Address::ZERO],
        &receipts,
    );
    assert!(matches!(
        observe_l2_block(&wrong_number, &receipts),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::FinalizationBlockNumber,
            ..
        })
    ));

    let misplaced_receipts = vec![
        receipt(true, advance_logs(None)),
        receipt(true, vec![]),
        receipt(true, vec![]),
    ];
    let misplaced = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            finalization_transaction(ZONE_NUMBER),
            user_transaction(0x71),
        ],
        vec![Address::ZERO, Address::ZERO, Address::repeat_byte(3)],
        &misplaced_receipts,
    );
    assert!(matches!(
        observe_l2_block(&misplaced, &misplaced_receipts),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::FinalizationPosition,
            ..
        })
    ));
}

#[test]
fn finalization_requires_system_identity_destination_and_success() {
    let finalization_receipts = vec![receipt(true, advance_logs(None)), receipt(true, vec![])];

    let wrong_sender = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            finalization_transaction(ZONE_NUMBER),
        ],
        vec![Address::ZERO, Address::repeat_byte(1)],
        &finalization_receipts,
    );
    assert!(matches!(
        observe_l2_block(&wrong_sender, &finalization_receipts),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::SystemIdentity,
            ..
        })
    ));

    let wrong_destination = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            system_transaction(
                Address::repeat_byte(2),
                finalization_transaction(ZONE_NUMBER).input().clone(),
            ),
        ],
        vec![Address::ZERO, Address::ZERO],
        &finalization_receipts,
    );
    assert!(matches!(
        observe_l2_block(&wrong_destination, &finalization_receipts),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::FinalizationDestination,
            ..
        })
    ));

    let failed_receipts = vec![receipt(true, advance_logs(None)), receipt(false, vec![])];
    let failed = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            finalization_transaction(ZONE_NUMBER),
        ],
        vec![Address::ZERO, Address::ZERO],
        &failed_receipts,
    );
    assert!(matches!(
        observe_l2_block(&failed, &failed_receipts),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::FinalizationSuccess,
            ..
        })
    ));
}

#[test]
fn malformed_finalization_calldata_has_its_own_error_class() {
    let receipts = vec![receipt(true, advance_logs(None)), receipt(true, vec![])];
    let block = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            system_transaction(ZONE_OUTBOX_ADDRESS, Bytes::from_static(b"bad")),
        ],
        vec![Address::ZERO, Address::ZERO],
        &receipts,
    );
    let transaction_hash = *block.body().transactions[1].tx_hash();
    let evidence = AuthenticatedDataEvidence::from_bytes(b"bad");
    assert!(matches!(
        observe_l2_block(&block, &receipts),
        Err(ObservationError::MalformedAuthenticatedData {
            kind: DataSource::FinalizationCalldata,
            transaction,
            evidence: actual_evidence,
            ..
        }) if transaction
            == AuthenticatedTransaction::new(ProtocolChain::ZoneL2, 1, transaction_hash)
            && actual_evidence == evidence
    ));
}
