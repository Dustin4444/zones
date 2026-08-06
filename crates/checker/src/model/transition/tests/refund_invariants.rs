use std::num::NonZeroU128;

use alloy_primitives::{Address, U256};

use super::{
    super::{ModelError, ModelTransition, refunds as refund_transitions},
    support::*,
};
use crate::model::{
    accounting::TokenAccounting,
    input::RefundClaimInput,
    ownership::{
        InboxRefundId, InboxRefundOwner, PortalRefundId, PortalRefundOwner, RefundAccount,
    },
};

#[test]
fn refund_totals_are_derived_from_per_origin_credits() {
    let token = token(0xad);
    let recipient = Address::repeat_byte(0xdd);
    let account = RefundAccount { token, recipient };
    let mut state = created_state(token);

    for (origin, amount) in [(1, 10), (2, 7)] {
        state.seed_portal_refund_for_test(
            PortalRefundId {
                token,
                recipient,
                failed_deposit: deposit_id(origin),
            },
            PortalRefundOwner::Pending { amount },
        );
    }
    assert_eq!(state.portal_refund_total(account), 17);

    for (origin, amount) in [(3, 11), (4, 13)] {
        state.seed_inbox_refund_for_test(
            InboxRefundId {
                token,
                recipient,
                user_withdrawal: withdrawal_id(origin),
            },
            InboxRefundOwner::Pending {
                amount: NonZeroU128::new(amount).unwrap(),
            },
        );
    }
    assert_eq!(state.inbox_refund_total(account), 24);
}

#[test]
fn credit_creation_checks_same_candidate_aggregate_overflow() {
    let token = token(0xa6);
    let recipient = Address::repeat_byte(0x61);
    let state = created_state(token);
    let mut candidate = ModelTransition::new(&state);
    refund_transitions::create_portal_credit(
        &mut candidate,
        portal_refund_id(token, recipient, 1),
        u128::MAX,
    )
    .unwrap();
    assert_eq!(
        refund_transitions::create_portal_credit(
            &mut candidate,
            portal_refund_id(token, recipient, 2),
            1,
        ),
        Err(ModelError::RefundAggregateOverflow { token, recipient })
    );

    refund_transitions::create_inbox_credit(
        &mut candidate,
        inbox_refund_id(token, recipient, 1),
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(u128::MAX).unwrap(),
        },
    )
    .unwrap();
    assert_eq!(
        refund_transitions::create_inbox_credit(
            &mut candidate,
            inbox_refund_id(token, recipient, 2),
            InboxRefundOwner::Pending {
                amount: NonZeroU128::new(1).unwrap(),
            },
        ),
        Err(ModelError::RefundAggregateOverflow { token, recipient })
    );
    assert!(state.portal_refunds().is_empty());
    assert!(state.inbox_refunds.is_empty());
}

#[test]
fn credit_creation_checks_cold_parent_aggregate_overflow() {
    let token = token(0xac);
    let recipient = Address::repeat_byte(0x6c);
    let mut state = created_state(token);
    seed_portal_credit(&mut state, portal_refund_id(token, recipient, 1), u128::MAX);
    seed_inbox_credit(&mut state, inbox_refund_id(token, recipient, 1), u128::MAX);

    let mut candidate = ModelTransition::new(&state);
    assert_eq!(
        refund_transitions::create_portal_credit(
            &mut candidate,
            portal_refund_id(token, recipient, 2),
            1,
        ),
        Err(ModelError::RefundAggregateOverflow { token, recipient })
    );
    assert_eq!(
        refund_transitions::create_inbox_credit(
            &mut candidate,
            inbox_refund_id(token, recipient, 2),
            InboxRefundOwner::Pending {
                amount: NonZeroU128::new(1).unwrap(),
            },
        ),
        Err(ModelError::RefundAggregateOverflow { token, recipient })
    );
}

#[test]
fn derived_refund_totals_reset_after_same_candidate_claims() {
    let token = token(0xab);
    let recipient = Address::repeat_byte(0x6b);
    let account = RefundAccount { token, recipient };
    let mut state = created_state(token);
    seed_portal_credit(&mut state, portal_refund_id(token, recipient, 1), 2);
    seed_inbox_credit(&mut state, inbox_refund_id(token, recipient, 1), 2);
    state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(12),
            withdrawal_liability: U256::from(12),
        },
    );

    let mut candidate = ModelTransition::new(&state);
    refund_transitions::create_portal_credit(
        &mut candidate,
        portal_refund_id(token, recipient, 2),
        3,
    )
    .unwrap();
    refund_transitions::claim_portal(&mut candidate, RefundClaimInput::new(recipient, token, 5))
        .unwrap();
    refund_transitions::create_portal_credit(
        &mut candidate,
        portal_refund_id(token, recipient, 3),
        u128::MAX,
    )
    .unwrap();
    assert_eq!(candidate.portal_refund_total(account), u128::MAX);

    refund_transitions::create_inbox_credit(
        &mut candidate,
        inbox_refund_id(token, recipient, 2),
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(3).unwrap(),
        },
    )
    .unwrap();
    refund_transitions::claim_inbox(&mut candidate, RefundClaimInput::new(recipient, token, 5))
        .unwrap();
    refund_transitions::create_inbox_credit(
        &mut candidate,
        inbox_refund_id(token, recipient, 3),
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(u128::MAX).unwrap(),
        },
    )
    .unwrap();
    assert_eq!(candidate.inbox_refund_total(account), u128::MAX);
}
