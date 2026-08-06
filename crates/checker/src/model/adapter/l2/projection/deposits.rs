//! Authenticated deposit decoding and ordered outcome projection.

use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, FixedBytes};
use tempo_zone_contracts::{IZoneInbox, ZonePortal};

use crate::{
    model::{
        constants::ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE,
        encoding::{
            CompressedYParity, DepositPayload, DepositQueueMember, OrdinaryDeposit,
            WithdrawalBounceBackDeposit,
        },
        events::{Inbox, L2ProtocolEvent, Outbox},
        input::AuthenticatedDepositOutcome,
    },
    observe::ImportedDeposit,
};

use super::{
    super::{
        DepositInputKind, ObservedDepositFailed, ObservedDepositOutcome, ObservedDepositProcessed,
        ObservedWithdrawalBounceBackPending, ObservedWithdrawalBounceBackProcessed,
        ZoneProjectionError, event_kind,
    },
    cursor::{ZoneEventCursor, observed_position},
    withdrawal::observed_withdrawal,
};

pub(super) struct DepositPrefixProjection {
    pub(super) deposits: Vec<DepositQueueMember>,
    pub(super) inputs: Vec<AuthenticatedDepositOutcome>,
    pub(super) outputs: Vec<ObservedDepositOutcome>,
}

struct DepositMemberProjection {
    deposit: DepositQueueMember,
    input: AuthenticatedDepositOutcome,
    output: ObservedDepositOutcome,
}

pub(super) fn project_deposit_prefix(
    events: &mut ZoneEventCursor<'_>,
    deposits: &[ImportedDeposit],
) -> Result<DepositPrefixProjection, ZoneProjectionError> {
    let mut projected = DepositPrefixProjection {
        deposits: Vec::with_capacity(deposits.len()),
        inputs: Vec::with_capacity(deposits.len()),
        outputs: Vec::with_capacity(deposits.len()),
    };
    for (index, deposit) in deposits.iter().enumerate() {
        let member = if let Some(deposit) = deposit.as_ordinary() {
            project_ordinary_deposit(events, index, deposit)?
        } else if let Some(deposit) = deposit.as_withdrawal_bounce_back() {
            project_bounce_back_deposit(events, index, deposit)?
        } else {
            return Err(ZoneProjectionError::UnsupportedDepositKind { index });
        };
        projected.deposits.push(member.deposit);
        projected.inputs.push(member.input);
        projected.outputs.push(member.output);
    }
    Ok(projected)
}

fn project_ordinary_deposit(
    events: &mut ZoneEventCursor<'_>,
    index: usize,
    deposit: &ZonePortal::Deposit,
) -> Result<DepositMemberProjection, ZoneProjectionError> {
    let input = ordinary_deposit(index, deposit)?;
    let outcome = events.next_advance(ZoneProjectionError::MissingDepositOutcome {
        index,
        deposit_kind: DepositInputKind::Ordinary,
    })?;
    let position = observed_position(outcome);

    let (input_outcome, output) = match outcome.event() {
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::DepositProcessed(event)) => (
            AuthenticatedDepositOutcome::OrdinaryMinted {
                recipient: event.to,
                memo: event.memo,
            },
            ObservedDepositOutcome::OrdinaryMinted(ObservedDepositProcessed {
                position,
                deposit_hash: event.depositHash,
                sender: event.sender,
                to: event.to,
                token: event.token,
                amount: event.amount,
                memo: event.memo,
            }),
        ),
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::WithdrawalRequested(event)) => {
            let withdrawal = observed_withdrawal(position, event);
            let failure_outcome =
                events.next_advance(ZoneProjectionError::MissingDepositFailed { index })?;
            let failure_position = observed_position(failure_outcome);
            let failure = match failure_outcome.event() {
                L2ProtocolEvent::Inbox(Inbox::InboxEvents::DepositFailed(event)) => {
                    ObservedDepositFailed {
                        position: failure_position,
                        deposit_hash: event.depositHash,
                        sender: event.sender,
                        token: event.token,
                        amount: event.amount,
                    }
                }
                actual => {
                    return Err(ZoneProjectionError::ReorderedDepositFailed {
                        index,
                        actual: event_kind(actual),
                        position: failure_position,
                    });
                }
            };
            (
                AuthenticatedDepositOutcome::OrdinaryFailed,
                ObservedDepositOutcome::OrdinaryFailed {
                    withdrawal: Box::new(withdrawal),
                    failure,
                },
            )
        }
        actual => {
            return Err(ZoneProjectionError::ReorderedDepositOutcome {
                index,
                deposit_kind: DepositInputKind::Ordinary,
                actual: event_kind(actual),
                position,
            });
        }
    };

    Ok(DepositMemberProjection {
        deposit: DepositQueueMember::Ordinary(input),
        input: input_outcome,
        output,
    })
}

fn ordinary_deposit(
    index: usize,
    deposit: &ZonePortal::Deposit,
) -> Result<OrdinaryDeposit, ZoneProjectionError> {
    let parity = match deposit.encrypted.ephemeralPubkeyYParity {
        0x02 => CompressedYParity::Even,
        0x03 => CompressedYParity::Odd,
        actual => return Err(ZoneProjectionError::InvalidDepositKeyParity { index, actual }),
    };
    let ciphertext: [u8; ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE] = deposit
        .encrypted
        .ciphertext
        .as_ref()
        .try_into()
        .map_err(|_| ZoneProjectionError::InvalidDepositCiphertextLength {
            index,
            actual: deposit.encrypted.ciphertext.len(),
            expected: ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE,
        })?;
    Ok(OrdinaryDeposit::new(
        deposit.token,
        deposit.sender,
        deposit.amount,
        deposit.tempoRefundRecipient,
        deposit.keyIndex,
        DepositPayload::new(
            deposit.encrypted.ephemeralPubkeyX,
            parity,
            FixedBytes::from(ciphertext),
            deposit.encrypted.nonce,
            deposit.encrypted.tag,
        ),
    ))
}

fn project_bounce_back_deposit(
    events: &mut ZoneEventCursor<'_>,
    index: usize,
    deposit: &IZoneInbox::WithdrawalBounceBackDeposit,
) -> Result<DepositMemberProjection, ZoneProjectionError> {
    let fallback_nonce = decode_fallback_nonce(index, deposit.to)?;
    let amount = NonZeroU128::new(deposit.amount)
        .ok_or(ZoneProjectionError::ZeroBounceBackAmount { index })?;
    let input = WithdrawalBounceBackDeposit::new(deposit.token, fallback_nonce, amount);
    let outcome = events.next_advance(ZoneProjectionError::MissingDepositOutcome {
        index,
        deposit_kind: DepositInputKind::WithdrawalBounceBack,
    })?;
    let position = observed_position(outcome);

    let (input_outcome, output) = match outcome.event() {
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::WithdrawalBounceBackProcessed(event)) => (
            AuthenticatedDepositOutcome::WithdrawalBounceBackMinted {
                recipient: event.zoneFallbackRecipient,
            },
            ObservedDepositOutcome::WithdrawalBounceBackMinted(
                ObservedWithdrawalBounceBackProcessed {
                    position,
                    zone_fallback_recipient: event.zoneFallbackRecipient,
                    token: event.token,
                    amount: event.amount,
                },
            ),
        ),
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::WithdrawalBounceBackPending(event)) => (
            AuthenticatedDepositOutcome::WithdrawalBounceBackPending {
                recipient: event.zoneFallbackRecipient,
            },
            ObservedDepositOutcome::WithdrawalBounceBackPending(
                ObservedWithdrawalBounceBackPending {
                    position,
                    zone_fallback_recipient: event.zoneFallbackRecipient,
                    token: event.token,
                    amount: event.amount,
                },
            ),
        ),
        actual => {
            return Err(ZoneProjectionError::ReorderedDepositOutcome {
                index,
                deposit_kind: DepositInputKind::WithdrawalBounceBack,
                actual: event_kind(actual),
                position,
            });
        }
    };

    Ok(DepositMemberProjection {
        deposit: DepositQueueMember::WithdrawalBounceBack(input),
        input: input_outcome,
        output,
    })
}

fn decode_fallback_nonce(
    index: usize,
    recipient: Address,
) -> Result<NonZeroU64, ZoneProjectionError> {
    let bytes = recipient.as_slice();
    if bytes[..12].iter().any(|byte| *byte != 0) {
        return Err(ZoneProjectionError::InvalidBounceBackRecipient { index, recipient });
    }
    let mut encoded = [0_u8; 8];
    encoded.copy_from_slice(&bytes[12..]);
    NonZeroU64::new(u64::from_be_bytes(encoded))
        .ok_or(ZoneProjectionError::ZeroBounceBackNonce { index })
}
