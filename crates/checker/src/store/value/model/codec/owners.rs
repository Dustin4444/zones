use crate::{
    model::constants::{
        AUTHENTICATED_WITHDRAWAL_SIZE, MAX_CALLBACK_DATA_SIZE, MAX_WITHDRAWAL_GAS_LIMIT,
    },
    store::codec::{CodecError, Decoder, Encoder},
};

use super::{
    super::types::{
        FallbackOwnerValue, PendingDepositValue, PendingWithdrawalValue, StoredSenderReveal,
        UserWithdrawalIdentityValue, UserWithdrawalRequestValue, WithdrawalValue,
    },
    core::{
        decode_bounce_back, decode_ordinary_deposit, encode_bounce_back, encode_ordinary_deposit,
    },
};

pub(super) fn encode_pending_deposit(out: &mut Encoder, value: &PendingDepositValue) {
    match value {
        PendingDepositValue::Ordinary(preimage) => {
            out.u8(0x00);
            encode_ordinary_deposit(out, preimage);
        }
        PendingDepositValue::WithdrawalBounceBack {
            withdrawal_zone_id,
            withdrawal_index,
            preimage,
        } => {
            out.u8(0x01);
            out.u32(*withdrawal_zone_id);
            out.u64(*withdrawal_index);
            encode_bounce_back(out, *preimage);
        }
    }
}

pub(super) fn decode_pending_deposit(
    input: &mut Decoder<'_>,
) -> Result<PendingDepositValue, CodecError> {
    match input.u8("pending-deposit kind")? {
        0x00 => Ok(PendingDepositValue::Ordinary(decode_ordinary_deposit(
            input,
        )?)),
        0x01 => Ok(PendingDepositValue::WithdrawalBounceBack {
            withdrawal_zone_id: input.u32("bounce-back withdrawal Zone ID")?,
            withdrawal_index: input.u64("bounce-back withdrawal index")?,
            preimage: decode_bounce_back(input)?,
        }),
        tag => Err(CodecError::UnknownTag {
            kind: "pending-deposit",
            tag,
        }),
    }
}

pub(super) fn encode_withdrawal(out: &mut Encoder, value: &WithdrawalValue) {
    match value {
        WithdrawalValue::Pending(pending) => {
            out.u8(0x00);
            encode_pending_withdrawal(out, pending);
        }
        WithdrawalValue::FinalizedUser {
            identity,
            request,
            encrypted_sender,
        } => {
            out.u8(0x01);
            encode_user_identity(out, *identity);
            encode_user_request(out, request);
            out.bytes("withdrawal encrypted sender", encrypted_sender);
        }
        WithdrawalValue::FinalizedFailedDeposit {
            deposit_portal,
            deposit_number,
            token,
            recipient,
            amount,
        } => {
            out.u8(0x02);
            out.address(*deposit_portal);
            out.u64(*deposit_number);
            out.address(*token);
            out.address(*recipient);
            out.u128(*amount);
        }
    }
}

pub(super) fn decode_withdrawal(input: &mut Decoder<'_>) -> Result<WithdrawalValue, CodecError> {
    match input.u8("withdrawal phase")? {
        0x00 => Ok(WithdrawalValue::Pending(decode_pending_withdrawal(input)?)),
        0x01 => Ok(WithdrawalValue::FinalizedUser {
            identity: decode_user_identity(input)?,
            request: decode_user_request(input)?,
            encrypted_sender: decode_finalized_encrypted_sender(input)?,
        }),
        0x02 => Ok(WithdrawalValue::FinalizedFailedDeposit {
            deposit_portal: input.address("failed-deposit Portal")?,
            deposit_number: required_nonzero_u64(input, "failed-deposit number")?,
            token: input.address("failed-deposit withdrawal token")?,
            recipient: input.address("failed-deposit withdrawal recipient")?,
            amount: input.u128("failed-deposit withdrawal amount")?,
        }),
        tag => Err(CodecError::UnknownTag {
            kind: "withdrawal phase",
            tag,
        }),
    }
}

fn encode_pending_withdrawal(out: &mut Encoder, value: &PendingWithdrawalValue) {
    match value {
        PendingWithdrawalValue::User {
            identity,
            request,
            sender_reveal,
        } => {
            out.u8(0x00);
            encode_user_identity(out, *identity);
            encode_user_request(out, request);
            out.u8(match sender_reveal {
                StoredSenderReveal::None => 0x00,
                StoredSenderReveal::Encrypted => 0x01,
            });
        }
        PendingWithdrawalValue::FailedDeposit {
            deposit_portal,
            deposit_number,
            token,
            recipient,
            amount,
        } => {
            out.u8(0x01);
            out.address(*deposit_portal);
            out.u64(*deposit_number);
            out.address(*token);
            out.address(*recipient);
            out.u128(*amount);
        }
    }
}

fn decode_pending_withdrawal(
    input: &mut Decoder<'_>,
) -> Result<PendingWithdrawalValue, CodecError> {
    match input.u8("pending-withdrawal origin")? {
        0x00 => {
            let identity = decode_user_identity(input)?;
            let request = decode_user_request(input)?;
            let sender_reveal = match input.u8("sender-reveal mode")? {
                0x00 => StoredSenderReveal::None,
                0x01 => StoredSenderReveal::Encrypted,
                tag => {
                    return Err(CodecError::UnknownTag {
                        kind: "sender-reveal mode",
                        tag,
                    });
                }
            };
            Ok(PendingWithdrawalValue::User {
                identity,
                request,
                sender_reveal,
            })
        }
        0x01 => {
            let deposit_portal = input.address("failed-deposit Portal")?;
            let deposit_number = required_nonzero_u64(input, "failed-deposit number")?;
            Ok(PendingWithdrawalValue::FailedDeposit {
                deposit_portal,
                deposit_number,
                token: input.address("failed-deposit withdrawal token")?,
                recipient: input.address("failed-deposit withdrawal recipient")?,
                amount: input.u128("failed-deposit withdrawal amount")?,
            })
        }
        tag => Err(CodecError::UnknownTag {
            kind: "pending-withdrawal origin",
            tag,
        }),
    }
}

fn encode_user_identity(out: &mut Encoder, value: UserWithdrawalIdentityValue) {
    out.address(value.sender);
    out.hash(value.transaction_hash);
    out.u64(value.fallback_nonce);
}

fn decode_user_identity(
    input: &mut Decoder<'_>,
) -> Result<UserWithdrawalIdentityValue, CodecError> {
    let value = UserWithdrawalIdentityValue {
        sender: input.address("withdrawal sender")?,
        transaction_hash: input.hash("withdrawal transaction hash")?,
        fallback_nonce: required_nonzero_u64(input, "withdrawal fallback nonce")?,
    };
    if value.transaction_hash.is_zero() {
        return Err(CodecError::Invalid {
            field: "withdrawal transaction hash",
            reason: "hash must be nonzero",
        });
    }
    Ok(value)
}

fn encode_user_request(out: &mut Encoder, value: &UserWithdrawalRequestValue) {
    out.address(value.token);
    out.address(value.recipient);
    out.u128(value.amount);
    out.hash(value.memo);
    out.u64(value.gas_limit);
    out.bytes("withdrawal callback data", &value.callback_data);
}

fn decode_user_request(input: &mut Decoder<'_>) -> Result<UserWithdrawalRequestValue, CodecError> {
    let value = UserWithdrawalRequestValue {
        token: input.address("withdrawal token")?,
        recipient: input.address("withdrawal recipient")?,
        amount: required_nonzero_u128(input, "withdrawal amount")?,
        memo: input.hash("withdrawal memo")?,
        gas_limit: input.u64("withdrawal gas limit")?,
        callback_data: input.bounded_bytes("withdrawal callback data", MAX_CALLBACK_DATA_SIZE)?,
    };
    validate_withdrawal_shape(value.gas_limit, &value.callback_data, &[])?;
    Ok(value)
}

fn decode_finalized_encrypted_sender(input: &mut Decoder<'_>) -> Result<Vec<u8>, CodecError> {
    let encrypted_sender =
        input.bounded_bytes("withdrawal encrypted sender", AUTHENTICATED_WITHDRAWAL_SIZE)?;
    if !matches!(encrypted_sender.len(), 0 | AUTHENTICATED_WITHDRAWAL_SIZE) {
        return Err(CodecError::Invalid {
            field: "withdrawal encrypted sender",
            reason: "length is neither zero nor the authenticated fixed size",
        });
    }
    Ok(encrypted_sender)
}

pub(super) fn encode_fallback(out: &mut Encoder, value: FallbackOwnerValue) {
    match value {
        FallbackOwnerValue::Held {
            withdrawal_zone_id,
            withdrawal_index,
            token,
            amount,
        } => {
            out.u8(0x00);
            encode_fallback_common(out, withdrawal_zone_id, withdrawal_index, token, amount);
        }
        FallbackOwnerValue::BounceBackQueued {
            withdrawal_zone_id,
            withdrawal_index,
            token,
            amount,
            deposit_portal,
            deposit_number,
        } => {
            out.u8(0x01);
            encode_fallback_common(out, withdrawal_zone_id, withdrawal_index, token, amount);
            out.address(deposit_portal);
            out.u64(deposit_number);
        }
    }
}

pub(super) fn decode_fallback(input: &mut Decoder<'_>) -> Result<FallbackOwnerValue, CodecError> {
    let tag = input.u8("fallback-owner phase")?;
    let withdrawal_zone_id = input.u32("fallback withdrawal Zone ID")?;
    let withdrawal_index = input.u64("fallback withdrawal index")?;
    let token = input.address("fallback token")?;
    let amount = required_nonzero_u128(input, "fallback amount")?;
    match tag {
        0x00 => Ok(FallbackOwnerValue::Held {
            withdrawal_zone_id,
            withdrawal_index,
            token,
            amount,
        }),
        0x01 => Ok(FallbackOwnerValue::BounceBackQueued {
            withdrawal_zone_id,
            withdrawal_index,
            token,
            amount,
            deposit_portal: input.address("fallback queued deposit Portal")?,
            deposit_number: required_nonzero_u64(input, "fallback queued deposit number")?,
        }),
        tag => Err(CodecError::UnknownTag {
            kind: "fallback-owner phase",
            tag,
        }),
    }
}

fn encode_fallback_common(
    out: &mut Encoder,
    zone_id: u32,
    withdrawal_index: u64,
    token: alloy_primitives::Address,
    amount: u128,
) {
    out.u32(zone_id);
    out.u64(withdrawal_index);
    out.address(token);
    out.u128(amount);
}

fn validate_withdrawal_shape(
    gas_limit: u64,
    callback_data: &[u8],
    encrypted_sender: &[u8],
) -> Result<(), CodecError> {
    if gas_limit > MAX_WITHDRAWAL_GAS_LIMIT {
        return Err(CodecError::Invalid {
            field: "withdrawal gas limit",
            reason: "value exceeds the protocol maximum",
        });
    }
    if callback_data.len() > MAX_CALLBACK_DATA_SIZE {
        return Err(CodecError::Invalid {
            field: "withdrawal callback data",
            reason: "value exceeds the protocol maximum",
        });
    }
    if !matches!(encrypted_sender.len(), 0 | AUTHENTICATED_WITHDRAWAL_SIZE) {
        return Err(CodecError::Invalid {
            field: "withdrawal encrypted sender",
            reason: "length is neither zero nor the authenticated fixed size",
        });
    }
    Ok(())
}

fn required_nonzero_u64(input: &mut Decoder<'_>, field: &'static str) -> Result<u64, CodecError> {
    let value = input.u64(field)?;
    if value == 0 {
        Err(CodecError::Invalid {
            field,
            reason: "value must be nonzero",
        })
    } else {
        Ok(value)
    }
}

fn required_nonzero_u128(input: &mut Decoder<'_>, field: &'static str) -> Result<u128, CodecError> {
    let value = input.u128(field)?;
    if value == 0 {
        Err(CodecError::Invalid {
            field,
            reason: "value must be nonzero",
        })
    } else {
        Ok(value)
    }
}
