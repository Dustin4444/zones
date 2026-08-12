//! Authentication tests.

use super::*;

#[test]
fn receipt_root_and_bloom_are_authenticated_against_the_zone_header() {
    let (block, receipts) = basic_fixture();
    let (receipts_root, logs_bloom) = receipt_commitments(&receipts);

    let wrong_root = reseal_with_commitments(block.clone(), B256::repeat_byte(0xa1), logs_bloom);
    assert!(matches!(
        observe_l2_block(&wrong_root, &receipts),
        Err(ObservationError::Acquisition(AcquisitionError::Inconsistent {
            kind: AcquisitionSource::ZoneNotificationReceipts,
            expected,
            ..
        })) if expected.contains("receipts root")
    ));

    let wrong_bloom = reseal_with_commitments(block, receipts_root, Bloom::repeat_byte(0xb2));
    assert!(matches!(
        observe_l2_block(&wrong_bloom, &receipts),
        Err(ObservationError::Acquisition(AcquisitionError::Inconsistent {
            kind: AcquisitionSource::ZoneNotificationReceipts,
            expected,
            ..
        })) if expected.contains("logs bloom")
    ));
}

#[test]
fn opening_envelope_requires_system_identity_destination_and_success() {
    let receipts = vec![receipt(true, advance_logs(None))];
    let wrong_sender = recovered_block(
        vec![advance_transaction(ZONE_INBOX_ADDRESS)],
        vec![Address::repeat_byte(1)],
        &receipts,
    );
    assert!(matches!(
        observe_l2_block(&wrong_sender, &receipts),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::AdvanceSystemCaller,
            ..
        })
    ));

    let wrong_destination = recovered_block(
        vec![advance_transaction(Address::repeat_byte(2))],
        vec![Address::ZERO],
        &receipts,
    );
    assert!(matches!(
        observe_l2_block(&wrong_destination, &receipts),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::AdvanceDestination,
            ..
        })
    ));

    let (block, _) = basic_fixture();
    let failed_receipts = [receipt(false, vec![])];
    let block = reseal_with_receipts(block, &failed_receipts);
    assert!(matches!(
        observe_l2_block(&block, &failed_receipts),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::AdvanceSuccess,
            ..
        })
    ));
}
