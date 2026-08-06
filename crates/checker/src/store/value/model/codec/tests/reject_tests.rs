use super::*;

#[test]
fn unknown_top_level_and_nested_tags_fail_closed() {
    assert!(ModelValue::decompress(&[0x02, 0xff]).is_err());

    let mut cases = vec![];
    let token = ModelValue::Token(TokenValue {
        phase: StoredTokenPhase::ZoneEnabled,
        supply: U256::ZERO,
        deposit_liability: U256::ZERO,
        withdrawal_liability: U256::ZERO,
    });
    cases.push((golden_model(&token), 2, "token phase"));
    cases.push((
        golden_model(&ModelValue::PendingDeposit(PendingDepositValue::Ordinary(
            ordinary_deposit(),
        ))),
        2,
        "deposit kind",
    ));
    cases.push((
        golden_model(&ModelValue::Withdrawal(WithdrawalValue::FinalizedUser {
            identity: user_identity(),
            request: user_request(),
            encrypted_sender: Vec::new(),
        })),
        2,
        "withdrawal phase",
    ));
    cases.push((
        golden_model(&ModelValue::Withdrawal(WithdrawalValue::Pending(
            PendingWithdrawalValue::FailedDeposit {
                deposit_portal: address(1),
                deposit_number: 1,
                token: address(2),
                recipient: address(3),
                amount: 1,
            },
        ))),
        3,
        "withdrawal origin",
    ));
    cases.push((
        golden_model(&ModelValue::FallbackOwner(FallbackOwnerValue::Held {
            withdrawal_zone_id: 1,
            withdrawal_index: 1,
            token: address(1),
            amount: 1,
        })),
        2,
        "fallback phase",
    ));
    cases.push((
        golden_model(&ModelValue::Batch(BatchValue::Finalized(finalized_batch()))),
        2,
        "batch phase",
    ));
    for (mut bytes, offset, name) in cases {
        bytes[offset] = 0xff;
        assert!(ModelValue::decompress(&bytes).is_err(), "accepted {name}");
    }

    let pending_user =
        ModelValue::Withdrawal(WithdrawalValue::Pending(PendingWithdrawalValue::User {
            identity: user_identity(),
            request: user_request(),
            sender_reveal: StoredSenderReveal::None,
        }));
    let mut bytes = golden_model(&pending_user);
    *bytes.last_mut().unwrap() = 0xff;
    assert!(ModelValue::decompress(&bytes).is_err());
}

#[test]
fn semantic_bounds_fail_closed() {
    let mut ordinary = golden_model(&ModelValue::PendingDeposit(PendingDepositValue::Ordinary(
        ordinary_deposit(),
    )));
    ordinary[143] = 0x04;
    assert!(ModelValue::decompress(&ordinary).is_err());

    let invalid_values = [
        ModelValue::PendingDeposit(PendingDepositValue::WithdrawalBounceBack {
            withdrawal_zone_id: 1,
            withdrawal_index: 1,
            preimage: BounceBackDepositValue {
                token: address(1),
                fallback_nonce: 0,
                amount: 1,
            },
        }),
        ModelValue::Withdrawal(WithdrawalValue::FinalizedUser {
            identity: user_identity(),
            request: user_request(),
            encrypted_sender: vec![1],
        }),
        ModelValue::InboxRefundCredit(0),
        ModelValue::Batch(BatchValue::Submitted {
            batch: finalized_batch(),
            portal: address(1),
            logical_queue_index: U256::MAX,
            next_processing_ordinal: 0,
            remaining_queue_hash: hash(1),
        }),
        ModelValue::Batch(BatchValue::Submitted {
            batch: finalized_batch(),
            portal: address(1),
            logical_queue_index: U256::ZERO,
            next_processing_ordinal: 2,
            remaining_queue_hash: hash(1),
        }),
        ModelValue::Batch(BatchValue::Submitted {
            batch: finalized_batch(),
            portal: address(1),
            logical_queue_index: U256::ZERO,
            next_processing_ordinal: 0,
            remaining_queue_hash: B256::ZERO,
        }),
    ];
    for value in invalid_values {
        assert!(ModelValue::decompress(&golden_model(&value)).is_err());
    }

    let mut trailing = golden_model(&ModelValue::PortalConfig { bounceback_gas: 1 });
    trailing.push(0);
    assert!(ModelValue::decompress(&trailing).is_err());
}
