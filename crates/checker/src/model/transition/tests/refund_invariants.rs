use std::num::NonZeroU128;

use alloy_primitives::{Address, U256};

use super::support::*;
use crate::model::{
    input::{ImportedTempoOperation, RefundClaimInput, ZoneOperation},
    ownership::{
        InboxRefundId, InboxRefundOwner, PortalRefundId, PortalRefundOwner, RefundAccount,
    },
    transition::ModelError,
};

#[test]
fn persisted_refund_totals_must_equal_their_open_origins() {
    let token = token(0xad);
    let recipient = Address::repeat_byte(0xdd);
    let account = RefundAccount { token, recipient };

    let mut portal_state = created_state(token);
    portal_state.seed_portal_refund_for_test(
        PortalRefundId {
            token,
            recipient,
            failed_deposit: deposit_id(1),
        },
        PortalRefundOwner::Pending { amount: 10 },
    );
    portal_state.portal_refund_totals.insert(account, 9);
    let portal_before = portal_state.clone();
    assert_eq!(
        commit_imported(
            &mut portal_state,
            1,
            U256::ZERO,
            vec![ImportedTempoOperation::PortalRefundClaimed(
                RefundClaimInput::new(recipient, token, 9),
            )],
        ),
        Err(ModelError::PortalRefundAggregateStateMismatch {
            token,
            recipient,
            expected: 9,
            actual: 10,
        })
    );
    assert_eq!(portal_state, portal_before);

    let mut inbox_state = created_state(token);
    inbox_state.seed_inbox_refund_for_test(
        InboxRefundId {
            token,
            recipient,
            user_withdrawal: withdrawal_id(0),
        },
        InboxRefundOwner::Pending {
            amount: NonZeroU128::new(10).unwrap(),
        },
    );
    inbox_state.inbox_refund_totals.insert(account, 9);
    let inbox_before = inbox_state.clone();
    assert_eq!(
        commit_block(
            &mut inbox_state,
            1,
            vec![ZoneOperation::InboxRefundClaimed(RefundClaimInput::new(
                recipient, token, 9,
            ))],
            None,
        ),
        Err(ModelError::InboxRefundAggregateStateMismatch {
            token,
            recipient,
            expected: 9,
            actual: 10,
        })
    );
    assert_eq!(inbox_state, inbox_before);
}
