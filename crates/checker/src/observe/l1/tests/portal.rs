//! Portal tests.

use super::*;

#[test]
fn one_receipt_cannot_imply_two_portal_call_families() {
    let tx_hash = B256::repeat_byte(0x10);
    let batch = batch_submitted_log(0, 0);
    let processed = withdrawal_processed_log(0, 1);
    let (imported, receipts) = anchor(vec![receipt(tx_hash, 0, true, vec![batch, processed])]);
    acquisition::authenticate_receipts(&imported, &[tx_hash], &receipts).unwrap();
    assert!(matches!(
        l1_events::ordered_transactions(PORTAL, &[tx_hash], &receipts),
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
        l1_portal::sole_portal_calldata(&direct, PORTAL, B256::ZERO).unwrap(),
        calldata.as_ref()
    );
    assert!(
        decode_portal_call(
            l1_portal::sole_portal_calldata(&direct, PORTAL, B256::ZERO).unwrap(),
            AuthenticatedTransaction::new(ProtocolChain::TempoL1, 0, B256::ZERO),
        )
        .unwrap()
        .as_submit_batch()
        .is_some()
    );

    let wrong_target = legacy_call(EXTERNAL, calldata.clone());
    assert!(matches!(
        l1_portal::sole_portal_calldata(&wrong_target, PORTAL, B256::ZERO),
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
        l1_portal::sole_portal_calldata(&multi, PORTAL, B256::ZERO),
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
        l1_portal::sole_portal_calldata(&one_aa, PORTAL, B256::ZERO).unwrap(),
        calldata.as_ref()
    );
}
