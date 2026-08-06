use crate::{
    model::constants::ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE,
    store::codec::{CodecError, Decoder, Encoder},
};

use super::super::types::{
    BounceBackDepositValue, CursorValue, OrdinaryDepositValue, PortalSettlementValue,
    StoredTokenPhase, TokenValue, ZoneBatchAccumulatorValue,
};

pub(super) fn encode_cursor(out: &mut Encoder, cursor: CursorValue) {
    out.hash(cursor.hash);
    out.u64(cursor.number);
}

pub(super) fn decode_cursor(
    input: &mut Decoder<'_>,
    field: &'static str,
) -> Result<CursorValue, CodecError> {
    Ok(CursorValue {
        hash: input.hash(field)?,
        number: input.u64(field)?,
    })
}

pub(super) fn encode_portal_settlement(out: &mut Encoder, value: PortalSettlementValue) {
    out.u64(value.withdrawal_batch_index);
    out.hash(value.block_hash);
    out.u64(value.last_synced_tempo_block_number);
    encode_cursor(out, value.last_submitted_deposit_cursor);
    out.u256(value.zone_height);
    out.u256(value.withdrawal_queue_head);
    out.u256(value.withdrawal_queue_tail);
}

pub(super) fn decode_portal_settlement(
    input: &mut Decoder<'_>,
) -> Result<PortalSettlementValue, CodecError> {
    Ok(PortalSettlementValue {
        withdrawal_batch_index: input.u64("Portal withdrawal batch index")?,
        block_hash: input.hash("Portal last submitted block hash")?,
        last_synced_tempo_block_number: input.u64("Portal last synced Tempo block")?,
        last_submitted_deposit_cursor: decode_cursor(
            input,
            "Portal last submitted deposit cursor",
        )?,
        zone_height: input.u256("Portal Zone height")?,
        withdrawal_queue_head: input.u256("Portal withdrawal queue head")?,
        withdrawal_queue_tail: input.u256("Portal withdrawal queue tail")?,
    })
}

pub(super) fn encode_zone_accumulator(out: &mut Encoder, value: ZoneBatchAccumulatorValue) {
    out.hash(value.last_withdrawal_queue_hash);
    out.u64(value.last_withdrawal_batch_index);
    out.hash(value.first_zone_parent_hash);
    encode_cursor(out, value.first_processed_deposit);
    out.u64(value.first_withdrawal_index);
}

pub(super) fn decode_zone_accumulator(
    input: &mut Decoder<'_>,
) -> Result<ZoneBatchAccumulatorValue, CodecError> {
    Ok(ZoneBatchAccumulatorValue {
        last_withdrawal_queue_hash: input.hash("Zone last withdrawal queue hash")?,
        last_withdrawal_batch_index: input.u64("Zone last withdrawal batch index")?,
        first_zone_parent_hash: input.hash("Zone batch first parent hash")?,
        first_processed_deposit: decode_cursor(input, "Zone batch first deposit cursor")?,
        first_withdrawal_index: input.u64("Zone batch first withdrawal index")?,
    })
}

pub(super) fn encode_token(out: &mut Encoder, value: TokenValue) {
    out.u8(match value.phase {
        StoredTokenPhase::PendingZoneEnable => 0x00,
        StoredTokenPhase::ZoneEnabled => 0x01,
    });
    out.u256(value.supply);
    out.u256(value.deposit_liability);
    out.u256(value.withdrawal_liability);
}

pub(super) fn decode_token(input: &mut Decoder<'_>) -> Result<TokenValue, CodecError> {
    let phase = match input.u8("token phase")? {
        0x00 => StoredTokenPhase::PendingZoneEnable,
        0x01 => StoredTokenPhase::ZoneEnabled,
        tag => {
            return Err(CodecError::UnknownTag {
                kind: "token phase",
                tag,
            });
        }
    };
    Ok(TokenValue {
        phase,
        supply: input.u256("token supply")?,
        deposit_liability: input.u256("token deposit liability")?,
        withdrawal_liability: input.u256("token withdrawal liability")?,
    })
}

pub(super) fn encode_ordinary_deposit(out: &mut Encoder, value: &OrdinaryDepositValue) {
    out.address(value.token);
    out.address(value.sender);
    out.u128(value.amount);
    out.address(value.tempo_refund_recipient);
    out.u256(value.key_index);
    out.hash(value.ephemeral_pubkey_x);
    out.u8(value.ephemeral_pubkey_y_parity);
    out.raw(&value.ciphertext);
    out.raw(&value.nonce);
    out.raw(&value.tag);
}

pub(super) fn decode_ordinary_deposit(
    input: &mut Decoder<'_>,
) -> Result<OrdinaryDepositValue, CodecError> {
    let token = input.address("ordinary-deposit token")?;
    let sender = input.address("ordinary-deposit sender")?;
    let amount = input.u128("ordinary-deposit amount")?;
    let tempo_refund_recipient = input.address("ordinary-deposit refund recipient")?;
    let key_index = input.u256("ordinary-deposit key index")?;
    let ephemeral_pubkey_x = input.hash("ordinary-deposit ephemeral key x")?;
    let ephemeral_pubkey_y_parity = input.u8("ordinary-deposit key parity")?;
    if !matches!(ephemeral_pubkey_y_parity, 0x02 | 0x03) {
        return Err(CodecError::Invalid {
            field: "ordinary-deposit key parity",
            reason: "compressed key prefix is neither 0x02 nor 0x03",
        });
    }
    let ciphertext = input
        .raw(
            "ordinary-deposit ciphertext",
            ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE,
        )?
        .to_vec();
    let nonce = input
        .raw("ordinary-deposit nonce", 12)?
        .try_into()
        .map_err(|_| CodecError::Truncated {
            field: "ordinary-deposit nonce",
        })?;
    let tag = input
        .raw("ordinary-deposit tag", 16)?
        .try_into()
        .map_err(|_| CodecError::Truncated {
            field: "ordinary-deposit tag",
        })?;
    Ok(OrdinaryDepositValue {
        token,
        sender,
        amount,
        tempo_refund_recipient,
        key_index,
        ephemeral_pubkey_x,
        ephemeral_pubkey_y_parity,
        ciphertext,
        nonce,
        tag,
    })
}

pub(super) fn encode_bounce_back(out: &mut Encoder, value: BounceBackDepositValue) {
    out.address(value.token);
    out.u64(value.fallback_nonce);
    out.u128(value.amount);
}

pub(super) fn decode_bounce_back(
    input: &mut Decoder<'_>,
) -> Result<BounceBackDepositValue, CodecError> {
    let value = BounceBackDepositValue {
        token: input.address("bounce-back token")?,
        fallback_nonce: input.u64("bounce-back fallback nonce")?,
        amount: input.u128("bounce-back amount")?,
    };
    if value.fallback_nonce == 0 || value.amount == 0 {
        return Err(CodecError::Invalid {
            field: "bounce-back deposit",
            reason: "fallback nonce and amount must be nonzero",
        });
    }
    Ok(value)
}
