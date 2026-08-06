use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256, U256};
use tempo_zone_contracts::{IZoneInbox, ZonePortal};

use super::super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    ownership::{
        DepositId, InboxRefundId, InboxRefundOwner, PortalRefundId, PortalRefundOwner,
        RefundAccount, WithdrawalId,
    },
};

#[tokio::test]
async fn portal_refund_claim_closes_each_origin_and_reconciles_the_aggregate_event() {
    let imported = imported_header(0);
    let recipient = Address::repeat_byte(0x71);
    let origins = [(1_u64, 55_u128), (2, 45)];
    let total = origins.iter().map(|(_, amount)| amount).sum::<u128>();
    let credits = origins.map(|(deposit_number, _)| PortalRefundId {
        token: INITIAL_TOKEN,
        recipient,
        failed_deposit: DepositId {
            portal: portal(),
            deposit_number: NonZeroU64::new(deposit_number).unwrap(),
        },
    });
    let mut model = created_model(TokenAccounting {
        supply: U256::ZERO,
        deposit_liability: U256::from(total),
        withdrawal_liability: U256::ZERO,
    });
    for (credit, (_, amount)) in credits.into_iter().zip(origins) {
        model.seed_portal_refund_for_test(credit, PortalRefundOwner::Pending { amount });
    }
    let l1 = vec![l1_transaction(
        1,
        None,
        vec![portal_event(ZonePortal::RefundClaimed {
            recipient,
            token: INITIAL_TOKEN,
            amount: total,
        })],
    )];
    let l2 = zone_observation(
        &imported,
        Vec::new(),
        Vec::new(),
        advance_logs(&imported, Vec::new(), B256::ZERO, 0),
        Vec::new(),
        None,
    );
    let exact = ExactPostState::from_model(&model).with_supply(INITIAL_TOKEN, U256::ZERO);
    let checker = run_valid_block(model, &imported, l1, &l2, &[U256::ZERO], exact, false).await;

    let state = checker.model();
    for credit in credits {
        assert!(state.portal_refund(credit).is_none());
    }
    assert_eq!(
        state.portal_refund_total(RefundAccount {
            token: INITIAL_TOKEN,
            recipient,
        }),
        0
    );
    assert_eq!(
        state.token(INITIAL_TOKEN).unwrap().accounting(),
        TokenAccounting::ZERO
    );
}

#[tokio::test]
async fn inbox_refund_claim_closes_each_origin_and_reconciles_the_aggregate_event() {
    let imported = imported_header(0);
    let recipient = Address::repeat_byte(0x72);
    let origins = [(8_u64, 60_u128), (9, 40)];
    let total = origins.iter().map(|(_, amount)| amount).sum::<u128>();
    let credits = origins.map(|(withdrawal_index, _)| InboxRefundId {
        token: INITIAL_TOKEN,
        recipient,
        user_withdrawal: WithdrawalId {
            zone_id: ZONE_ID,
            withdrawal_index,
        },
    });
    let mut model = created_model(TokenAccounting {
        supply: U256::ZERO,
        deposit_liability: U256::ZERO,
        withdrawal_liability: U256::from(total),
    });
    for (credit, (_, amount)) in credits.into_iter().zip(origins) {
        model.seed_inbox_refund_for_test(
            credit,
            InboxRefundOwner::Pending {
                amount: NonZeroU128::new(amount).unwrap(),
            },
        );
    }
    let l2 = zone_observation(
        &imported,
        Vec::new(),
        Vec::new(),
        advance_logs(&imported, Vec::new(), B256::ZERO, 0),
        vec![ZoneUserTransaction {
            sender: recipient,
            logs: vec![zone_log(
                crate::model::constants::ZONE_INBOX_ADDRESS,
                IZoneInbox::RefundClaimed {
                    recipient,
                    token: INITIAL_TOKEN,
                    amount: total,
                },
            )],
        }],
        None,
    );
    let exact = ExactPostState::from_model(&model).with_supply(INITIAL_TOKEN, U256::from(total));
    let checker = run_valid_block(
        model,
        &imported,
        Vec::new(),
        &l2,
        &[U256::from(total)],
        exact,
        false,
    )
    .await;

    let state = checker.model();
    for credit in credits {
        assert!(state.inbox_refund(credit).is_none());
    }
    assert_eq!(
        state.inbox_refund_total(RefundAccount {
            token: INITIAL_TOKEN,
            recipient,
        }),
        0
    );
    assert_eq!(
        state.token(INITIAL_TOKEN).unwrap().accounting(),
        TokenAccounting {
            supply: U256::from(total),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        }
    );
}
