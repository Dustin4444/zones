//! State tests.

use super::*;

#[test]
fn overlay_reads_writes_deletes_and_finishes_in_key_order() {
    let mut rows = State::awaiting(identity()).rows().clone();
    let token = identity().initial_token;
    rows.insert(
        StateKey::Token(token),
        StateValue::Token(TokenState::pending()),
    );
    let parent = State::from_rows(rows).unwrap();

    let mut overlay = Overlay::new(&parent);
    overlay.set(StateKey::Token(token), None);
    overlay.set(
        StateKey::Portal,
        Some(StateValue::Portal(PortalState::Created {
            identity: identity(),
            bounceback_gas: 5,
            deposit: crate::kernel::state::Cursor::ZERO,
            settlement: crate::kernel::state::Settlement::ZERO,
        })),
    );
    assert!(overlay.get(&StateKey::Token(token)).is_none());
    let delta = overlay.finish();
    assert_eq!(delta.writes()[0].0, StateKey::Portal);
    assert_eq!(delta.writes()[1], (StateKey::Token(token), None));
}

#[test]
fn state_key_family_order_is_stable() {
    let deposit = DepositId {
        portal: identity().portal,
        number: NonZeroU64::MIN,
    };
    let withdrawal = WithdrawalId {
        zone_id: ZONE_ID,
        index: 0,
    };
    let keys = [
        StateKey::Portal,
        StateKey::Zone,
        StateKey::Token(Address::ZERO),
        StateKey::Deposit(deposit),
        StateKey::Withdrawal(withdrawal),
        StateKey::Batch(BatchId {
            zone_id: ZONE_ID,
            index: NonZeroU64::MIN,
        }),
        StateKey::Fallback(FallbackId {
            zone_id: ZONE_ID,
            nonce: NonZeroU64::MIN,
        }),
        StateKey::PortalRefund(PortalRefundId {
            token: Address::ZERO,
            recipient: Address::ZERO,
            deposit,
        }),
        StateKey::InboxRefund(InboxRefundId {
            token: Address::ZERO,
            recipient: Address::ZERO,
            withdrawal,
        }),
    ];
    assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
}
