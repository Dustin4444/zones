use super::*;

#[test]
fn refund_prefix_scan_returns_only_exact_account_credits() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let token = initialization.identity.portal_identity().initial_token();
    let recipient = Address::repeat_byte(0xa2);
    let other = Address::repeat_byte(0xa3);
    let mut mutations = vec![
        ModelMutation::put(
            ModelKey::PortalDepositCursor,
            ModelValue::PortalDepositCursor(CursorValue {
                hash: hash(0x40),
                number: 3,
            }),
        )
        .unwrap(),
        ModelMutation::put(
            ModelKey::ZoneProcessedDepositCursor,
            ModelValue::ZoneProcessedDepositCursor(CursorValue {
                hash: hash(0x40),
                number: 3,
            }),
        )
        .unwrap(),
        ModelMutation::put(
            ModelKey::Token(token),
            token_value_with_liabilities(0, 15, 0),
        )
        .unwrap(),
    ];
    mutations.extend(
        [(recipient, 1, 3), (recipient, 2, 5), (other, 3, 7)]
            .into_iter()
            .map(|(recipient, origin, amount)| {
                ModelMutation::put(
                    ModelKey::PortalRefundCredit {
                        token,
                        recipient,
                        origin,
                    },
                    ModelValue::PortalRefundCredit(amount),
                )
                .unwrap()
            }),
    );
    store
        .apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0x41,
            0x42,
            mutations,
        ))
        .unwrap();

    assert_eq!(
        store.portal_refund_credits(token, recipient).unwrap(),
        vec![
            RefundCredit {
                origin: 1,
                amount: 3
            },
            RefundCredit {
                origin: 2,
                amount: 5
            },
        ]
    );
    assert!(
        store
            .inbox_refund_credits(token, recipient)
            .unwrap()
            .is_empty()
    );

    let tx = store.database().tx_mut().unwrap();
    tx.put::<CheckerModelState>(
        ModelKey::PortalRefundCredit {
            token,
            recipient,
            origin: 4,
        },
        ModelValue::InboxRefundCredit(11),
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        store.portal_refund_credits(token, recipient),
        Err(StoreError::ModelKeyValueMismatch { .. })
    ));
}
