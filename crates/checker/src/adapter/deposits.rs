//! Converts authenticated ordinary deposits into kernel facts.

use alloy_primitives::FixedBytes;

use crate::{failure::Failure, kernel::OrdinaryDeposit};

use super::{AdapterFindingCode, failure};

/// Convert an `advanceTempo` ordinary deposit into checker-owned facts.
pub(super) fn ordinary_deposit(
    deposit: &tempo_zone_contracts::ZonePortal::Deposit,
) -> Result<OrdinaryDeposit, Failure> {
    Ok(OrdinaryDeposit {
        token: deposit.token,
        sender: deposit.sender,
        amount: deposit.amount,
        tempo_refund_recipient: deposit.tempoRefundRecipient,
        key_index: deposit.keyIndex,
        encrypted: crate::kernel::DepositPayload {
            ephemeral_pubkey_x: deposit.encrypted.ephemeralPubkeyX,
            ephemeral_pubkey_y_parity: deposit.encrypted.ephemeralPubkeyYParity,
            ciphertext: ciphertext(deposit.encrypted.ciphertext.as_ref(), "deposit")?,
            nonce: deposit.encrypted.nonce,
            tag: deposit.encrypted.tag,
        },
    })
}

/// Convert an ordinary deposit emitted by the Portal into checker-owned facts.
pub(super) fn ordinary_deposit_event(
    deposit: &tempo_zone_contracts::ZonePortal::DepositMade,
    context: &'static str,
) -> Result<OrdinaryDeposit, Failure> {
    Ok(OrdinaryDeposit {
        token: deposit.token,
        sender: deposit.sender,
        amount: deposit.netAmount,
        tempo_refund_recipient: deposit.tempoRefundRecipient,
        key_index: deposit.keyIndex,
        encrypted: crate::kernel::DepositPayload {
            ephemeral_pubkey_x: deposit.ephemeralPubkeyX,
            ephemeral_pubkey_y_parity: deposit.ephemeralPubkeyYParity,
            ciphertext: ciphertext(deposit.ciphertext.as_ref(), context)?,
            nonce: deposit.nonce,
            tag: deposit.tag,
        },
    })
}
/// Decode the fixed-size ciphertext authenticated by deposit calldata or events.
fn ciphertext(bytes: &[u8], context: &'static str) -> Result<FixedBytes<64>, Failure> {
    let ciphertext: [u8; 64] = bytes.try_into().map_err(|_| {
        failure(
            AdapterFindingCode::Grammar,
            format!("{context} ciphertext is not 64 bytes"),
        )
    })?;
    Ok(FixedBytes::from(ciphertext))
}
