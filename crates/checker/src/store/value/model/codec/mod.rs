mod batch;
mod core;
mod owners;

use crate::store::{
    codec::{CheckedCompact, CodecError, Decoder, Encoder, impl_value_codec},
    schema::{ModelKey, model_tag},
};

use super::types::*;
use batch::{decode_batch, encode_batch};
use core::{
    decode_cursor, decode_portal_settlement, decode_token, decode_zone_accumulator, encode_cursor,
    encode_portal_settlement, encode_token, encode_zone_accumulator,
};
use owners::{
    decode_fallback, decode_pending_deposit, decode_withdrawal, encode_fallback,
    encode_pending_deposit, encode_withdrawal,
};

impl ModelValue {
    pub(crate) const fn matches_key(&self, key: ModelKey) -> bool {
        matches!(
            (key, self),
            (ModelKey::PortalConfig, Self::PortalConfig { .. })
                | (ModelKey::ZoneConfig, Self::ZoneConfig { .. })
                | (ModelKey::PortalDepositCursor, Self::PortalDepositCursor(_))
                | (
                    ModelKey::ZoneProcessedDepositCursor,
                    Self::ZoneProcessedDepositCursor(_)
                )
                | (ModelKey::PortalSettlement, Self::PortalSettlement(_))
                | (
                    ModelKey::ZoneBatchAccumulator,
                    Self::ZoneBatchAccumulator(_)
                )
                | (
                    ModelKey::ZoneNextWithdrawalIndex,
                    Self::ZoneNextWithdrawalIndex(_)
                )
                | (
                    ModelKey::ZoneLastFallbackNonce,
                    Self::ZoneLastFallbackNonce(_)
                )
                | (ModelKey::Token(_), Self::Token(_))
                | (ModelKey::PendingDeposit(_), Self::PendingDeposit(_))
                | (ModelKey::Withdrawal(_), Self::Withdrawal(_))
                | (ModelKey::FallbackOwner(_), Self::FallbackOwner(_))
                | (ModelKey::Batch(_), Self::Batch(_))
                | (
                    ModelKey::PortalRefundCredit { .. },
                    Self::PortalRefundCredit(_)
                )
                | (
                    ModelKey::InboxRefundCredit { .. },
                    Self::InboxRefundCredit(_)
                )
        )
    }
}

impl CheckedCompact for ModelValue {
    fn encode_checked(&self, out: &mut Encoder) {
        out.version();
        match self {
            Self::PortalConfig { bounceback_gas } => {
                out.u8(model_tag::PORTAL_CONFIG);
                out.u64(*bounceback_gas);
            }
            Self::ZoneConfig {
                tempo_gas_rate,
                max_withdrawals_per_block,
            } => {
                out.u8(model_tag::ZONE_CONFIG);
                out.u128(*tempo_gas_rate);
                out.u32(*max_withdrawals_per_block);
            }
            Self::PortalDepositCursor(cursor) => {
                out.u8(model_tag::PORTAL_DEPOSIT_CURSOR);
                encode_cursor(out, *cursor);
            }
            Self::ZoneProcessedDepositCursor(cursor) => {
                out.u8(model_tag::ZONE_PROCESSED_DEPOSIT_CURSOR);
                encode_cursor(out, *cursor);
            }
            Self::PortalSettlement(value) => {
                out.u8(model_tag::PORTAL_SETTLEMENT);
                encode_portal_settlement(out, *value);
            }
            Self::ZoneBatchAccumulator(value) => {
                out.u8(model_tag::ZONE_BATCH_ACCUMULATOR);
                encode_zone_accumulator(out, *value);
            }
            Self::ZoneNextWithdrawalIndex(index) => {
                out.u8(model_tag::ZONE_NEXT_WITHDRAWAL_INDEX);
                out.u64(*index);
            }
            Self::ZoneLastFallbackNonce(nonce) => {
                out.u8(model_tag::ZONE_LAST_FALLBACK_NONCE);
                out.u64(*nonce);
            }
            Self::Token(value) => {
                out.u8(model_tag::TOKEN);
                encode_token(out, *value);
            }
            Self::PendingDeposit(value) => {
                out.u8(model_tag::PENDING_DEPOSIT);
                encode_pending_deposit(out, value);
            }
            Self::Withdrawal(value) => {
                out.u8(model_tag::WITHDRAWAL);
                encode_withdrawal(out, value);
            }
            Self::FallbackOwner(value) => {
                out.u8(model_tag::FALLBACK_OWNER);
                encode_fallback(out, *value);
            }
            Self::Batch(value) => {
                out.u8(model_tag::BATCH);
                encode_batch(out, *value);
            }
            Self::PortalRefundCredit(amount) => {
                out.u8(model_tag::PORTAL_REFUND_CREDIT);
                out.u128(*amount);
            }
            Self::InboxRefundCredit(amount) => {
                out.u8(model_tag::INBOX_REFUND_CREDIT);
                out.u128(*amount);
            }
        }
    }

    fn decode_checked(input: &mut Decoder<'_>) -> Result<Self, CodecError> {
        input.version()?;
        match input.u8("model value tag")? {
            model_tag::PORTAL_CONFIG => Ok(Self::PortalConfig {
                bounceback_gas: input.u64("Portal bounceback gas")?,
            }),
            model_tag::ZONE_CONFIG => Ok(Self::ZoneConfig {
                tempo_gas_rate: input.u128("Zone Tempo gas rate")?,
                max_withdrawals_per_block: input.u32("Zone max withdrawals per block")?,
            }),
            model_tag::PORTAL_DEPOSIT_CURSOR => Ok(Self::PortalDepositCursor(decode_cursor(
                input,
                "Portal deposit cursor",
            )?)),
            model_tag::ZONE_PROCESSED_DEPOSIT_CURSOR => Ok(Self::ZoneProcessedDepositCursor(
                decode_cursor(input, "Zone processed-deposit cursor")?,
            )),
            model_tag::PORTAL_SETTLEMENT => {
                Ok(Self::PortalSettlement(decode_portal_settlement(input)?))
            }
            model_tag::ZONE_BATCH_ACCUMULATOR => {
                Ok(Self::ZoneBatchAccumulator(decode_zone_accumulator(input)?))
            }
            model_tag::ZONE_NEXT_WITHDRAWAL_INDEX => Ok(Self::ZoneNextWithdrawalIndex(
                input.u64("Zone next withdrawal index")?,
            )),
            model_tag::ZONE_LAST_FALLBACK_NONCE => Ok(Self::ZoneLastFallbackNonce(
                input.u64("Zone last fallback nonce")?,
            )),
            model_tag::TOKEN => Ok(Self::Token(decode_token(input)?)),
            model_tag::PENDING_DEPOSIT => Ok(Self::PendingDeposit(decode_pending_deposit(input)?)),
            model_tag::WITHDRAWAL => Ok(Self::Withdrawal(decode_withdrawal(input)?)),
            model_tag::FALLBACK_OWNER => Ok(Self::FallbackOwner(decode_fallback(input)?)),
            model_tag::BATCH => Ok(Self::Batch(decode_batch(input)?)),
            model_tag::PORTAL_REFUND_CREDIT => Ok(Self::PortalRefundCredit(
                input.u128("Portal refund credit")?,
            )),
            model_tag::INBOX_REFUND_CREDIT => {
                let amount = input.u128("Inbox refund credit")?;
                require_nonzero_credit(amount, "Inbox refund credit")?;
                Ok(Self::InboxRefundCredit(amount))
            }
            tag => Err(CodecError::UnknownTag {
                kind: "model value",
                tag,
            }),
        }
    }
}

impl_value_codec!(ModelValue);

fn require_nonzero_credit(amount: u128, field: &'static str) -> Result<(), CodecError> {
    if amount == 0 {
        Err(CodecError::Invalid {
            field,
            reason: "credit must be nonzero",
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
