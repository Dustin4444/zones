use alloy_primitives::B256;

use crate::store::codec::{CodecError, Decoder, Encoder};

use super::{
    super::{
        leaf::{
            StoredDataSource, StoredEnvelopeRule, StoredImportedProjectionError, StoredModelError,
            StoredPortalCallError, StoredZoneProjectionError,
        },
        types::{ChainLocation, FindingKind, FindingSummary},
    },
    primitives::{
        decode_location, decode_optional_hash, decode_optional_model_key, decode_summary,
        decode_tip, encode_location, encode_optional_hash, encode_optional_model_key,
        encode_summary, encode_tip,
    },
};

pub(super) fn encode_kind(out: &mut Encoder, kind: &FindingKind) {
    out.u8(kind.code().0);
    match kind {
        FindingKind::InvalidEnvelope(location, code) => {
            out.u8(code.wire_tag());
            encode_location(out, *location);
        }
        FindingKind::MalformedAuthenticatedData(location, code, summary) => {
            out.u8(code.wire_tag());
            encode_location(out, *location);
            encode_summary(out, *summary);
        }
        FindingKind::PortalCallViolation(location, code, summary) => {
            out.u8(code.wire_tag());
            encode_location(out, *location);
            encode_summary(out, *summary);
        }
        FindingKind::ImportedProjectionViolation(location, code, summary) => {
            out.u8(code.wire_tag());
            encode_location(out, *location);
            encode_summary(out, *summary);
        }
        FindingKind::ZoneProjectionViolation(location, code, summary) => {
            out.u8(code.wire_tag());
            encode_location(out, *location);
            encode_summary(out, *summary);
        }
        FindingKind::ModelViolation(location, code, key, summary) => {
            out.u8(code.wire_tag());
            encode_location(out, *location);
            encode_optional_model_key(out, *key);
            encode_summary(out, *summary);
        }
        FindingKind::UnsupportedProtocolEvent(location, emitter, topic) => {
            encode_location(out, *location);
            out.address(*emitter);
            encode_optional_hash(out, *topic);
        }
        FindingKind::MalformedProtocolEvent(location, emitter, topic, summary) => {
            encode_location(out, *location);
            out.address(*emitter);
            out.hash(*topic);
            encode_summary(out, *summary);
        }
        FindingKind::ZoneContinuity(expected, number, parent)
        | FindingKind::TempoContinuity(expected, number, parent) => {
            encode_tip(out, *expected);
            out.u64(*number);
            out.hash(*parent);
        }
        FindingKind::PortalObservationIdentityMismatch(expected, actual) => {
            out.address(*expected);
            out.address(*actual);
        }
        FindingKind::PortalCreationBlockMismatch(expected, actual)
        | FindingKind::TempoBlockHashMismatch(expected, actual)
        | FindingKind::ProcessedDepositHashMismatch(expected, actual)
        | FindingKind::WithdrawalQueueHashMismatch(expected, actual) => {
            out.hash(*expected);
            out.hash(*actual);
        }
        FindingKind::PortalCreationMissing(hash) => out.hash(*hash),
        FindingKind::ImportedOutputCountMismatch(expected, actual)
        | FindingKind::TokenEnableCountMismatch(expected, actual)
        | FindingKind::DepositOutcomeCountMismatch(expected, actual)
        | FindingKind::ZoneOperationCountMismatch(expected, actual)
        | FindingKind::TempoBlockNumberMismatch(expected, actual)
        | FindingKind::ProcessedDepositNumberMismatch(expected, actual)
        | FindingKind::WithdrawalBatchIndexMismatch(expected, actual) => {
            out.u64(*expected);
            out.u64(*actual);
        }
        FindingKind::ImportedOutputMismatch(index, location, expected, actual)
        | FindingKind::TokenEnableMismatch(index, location, expected, actual)
        | FindingKind::DepositOutcomeMismatch(index, location, expected, actual)
        | FindingKind::ZoneOperationMismatch(index, location, expected, actual) => {
            out.u64(*index);
            encode_location(out, *location);
            encode_summary(out, *expected);
            encode_summary(out, *actual);
        }
        FindingKind::TempoBlockFinalizedMismatch(location, expected, actual)
        | FindingKind::TempoAdvancedMismatch(location, expected, actual)
        | FindingKind::BatchFinalizedMismatch(location, expected, actual) => {
            encode_location(out, *location);
            encode_summary(out, *expected);
            encode_summary(out, *actual);
        }
        FindingKind::CollateralDeficit(token, required, actual) => {
            out.address(*token);
            out.u256(*required);
            out.u256(*actual);
        }
        FindingKind::MissingSupply(token) => out.address(*token),
        FindingKind::SupplyMismatch(token, expected, actual) => {
            out.address(*token);
            out.u256(*expected);
            out.u256(*actual);
        }
    }
}

pub(super) fn decode_kind(input: &mut Decoder<'_>) -> Result<FindingKind, CodecError> {
    let kind = match input.u8("finding kind")? {
        0x01 => {
            let (location, rule) = decode_location_after_leaf(
                input,
                "envelope rule",
                StoredEnvelopeRule::from_wire_tag,
            )?;
            FindingKind::InvalidEnvelope(location, rule)
        }
        0x02 => decode_categorized(
            input,
            "data source",
            StoredDataSource::from_wire_tag,
            FindingKind::MalformedAuthenticatedData,
        )?,
        0x03 => FindingKind::UnsupportedProtocolEvent(
            decode_location(input)?,
            input.address("protocol event emitter")?,
            decode_optional_hash(input)?,
        ),
        0x04 => FindingKind::MalformedProtocolEvent(
            decode_location(input)?,
            input.address("protocol event emitter")?,
            input.hash("protocol event topic")?,
            decode_summary(input)?,
        ),
        0x05 => decode_categorized(
            input,
            "Portal call",
            StoredPortalCallError::from_wire_tag,
            FindingKind::PortalCallViolation,
        )?,
        0x06 => decode_continuity(input, FindingKind::ZoneContinuity)?,
        0x07 => decode_continuity(input, FindingKind::TempoContinuity)?,
        0x08 => FindingKind::PortalObservationIdentityMismatch(
            input.address("expected Portal")?,
            input.address("actual Portal")?,
        ),
        0x09 => FindingKind::PortalCreationBlockMismatch(
            input.hash("expected creation block")?,
            input.hash("actual creation block")?,
        ),
        0x0a => FindingKind::PortalCreationMissing(input.hash("Portal creation block")?),
        0x0b => decode_categorized(
            input,
            "imported projection",
            StoredImportedProjectionError::from_wire_tag,
            FindingKind::ImportedProjectionViolation,
        )?,
        0x0c => decode_categorized(
            input,
            "Zone projection",
            StoredZoneProjectionError::from_wire_tag,
            FindingKind::ZoneProjectionViolation,
        )?,
        0x0d => decode_model_violation(input)?,
        0x0e => decode_count(input, FindingKind::ImportedOutputCountMismatch)?,
        0x0f => decode_indexed_summary(input, FindingKind::ImportedOutputMismatch)?,
        0x10 => decode_summary_pair(input, FindingKind::TempoBlockFinalizedMismatch)?,
        0x11 => decode_count(input, FindingKind::TokenEnableCountMismatch)?,
        0x12 => decode_indexed_summary(input, FindingKind::TokenEnableMismatch)?,
        0x13 => decode_count(input, FindingKind::DepositOutcomeCountMismatch)?,
        0x14 => decode_indexed_summary(input, FindingKind::DepositOutcomeMismatch)?,
        0x15 => decode_summary_pair(input, FindingKind::TempoAdvancedMismatch)?,
        0x16 => decode_count(input, FindingKind::ZoneOperationCountMismatch)?,
        0x17 => decode_indexed_summary(input, FindingKind::ZoneOperationMismatch)?,
        0x18 => decode_summary_pair(input, FindingKind::BatchFinalizedMismatch)?,
        0x19 => decode_hashes(input, FindingKind::TempoBlockHashMismatch)?,
        0x1a => decode_count(input, FindingKind::TempoBlockNumberMismatch)?,
        0x1b => decode_hashes(input, FindingKind::ProcessedDepositHashMismatch)?,
        0x1c => decode_count(input, FindingKind::ProcessedDepositNumberMismatch)?,
        0x1d => decode_hashes(input, FindingKind::WithdrawalQueueHashMismatch)?,
        0x1e => decode_count(input, FindingKind::WithdrawalBatchIndexMismatch)?,
        0x1f => FindingKind::CollateralDeficit(
            input.address("collateral token")?,
            input.u256("required collateral")?,
            input.u256("actual collateral")?,
        ),
        0x20 => FindingKind::MissingSupply(input.address("supply token")?),
        0x21 => FindingKind::SupplyMismatch(
            input.address("supply token")?,
            input.u256("expected supply")?,
            input.u256("actual supply")?,
        ),
        tag => {
            return Err(CodecError::UnknownTag {
                kind: "finding kind",
                tag,
            });
        }
    };
    Ok(kind)
}

fn decode_leaf<T>(
    input: &mut Decoder<'_>,
    family: &'static str,
    from_wire_tag: impl FnOnce(u8) -> Option<T>,
) -> Result<T, CodecError> {
    let tag = input.u8(family)?;
    from_wire_tag(tag).ok_or(CodecError::UnknownTag { kind: family, tag })
}

fn decode_location_after_leaf<T>(
    input: &mut Decoder<'_>,
    family: &'static str,
    from_wire_tag: impl FnOnce(u8) -> Option<T>,
) -> Result<(ChainLocation, T), CodecError> {
    let leaf = decode_leaf(input, family, from_wire_tag)?;
    Ok((decode_location(input)?, leaf))
}

fn decode_categorized<F, T>(
    input: &mut Decoder<'_>,
    family: &'static str,
    from_wire_tag: impl FnOnce(u8) -> Option<T>,
    build: F,
) -> Result<FindingKind, CodecError>
where
    F: FnOnce(ChainLocation, T, FindingSummary) -> FindingKind,
{
    let (location, leaf) = decode_location_after_leaf(input, family, from_wire_tag)?;
    Ok(build(location, leaf, decode_summary(input)?))
}

fn decode_model_violation(input: &mut Decoder<'_>) -> Result<FindingKind, CodecError> {
    let (location, leaf) =
        decode_location_after_leaf(input, "model violation", StoredModelError::from_wire_tag)?;
    Ok(FindingKind::ModelViolation(
        location,
        leaf,
        decode_optional_model_key(input)?,
        decode_summary(input)?,
    ))
}

fn decode_continuity<F>(input: &mut Decoder<'_>, build: F) -> Result<FindingKind, CodecError>
where
    F: FnOnce(alloy_eips::BlockNumHash, u64, B256) -> FindingKind,
{
    Ok(build(
        decode_tip(input)?,
        input.u64("actual block number")?,
        input.hash("actual parent hash")?,
    ))
}

fn decode_count<F>(input: &mut Decoder<'_>, build: F) -> Result<FindingKind, CodecError>
where
    F: FnOnce(u64, u64) -> FindingKind,
{
    Ok(build(
        input.u64("expected number")?,
        input.u64("actual number")?,
    ))
}

fn decode_hashes<F>(input: &mut Decoder<'_>, build: F) -> Result<FindingKind, CodecError>
where
    F: FnOnce(B256, B256) -> FindingKind,
{
    Ok(build(
        input.hash("expected hash")?,
        input.hash("actual hash")?,
    ))
}

fn decode_summary_pair<F>(input: &mut Decoder<'_>, build: F) -> Result<FindingKind, CodecError>
where
    F: FnOnce(ChainLocation, FindingSummary, FindingSummary) -> FindingKind,
{
    Ok(build(
        decode_location(input)?,
        decode_summary(input)?,
        decode_summary(input)?,
    ))
}

fn decode_indexed_summary<F>(input: &mut Decoder<'_>, build: F) -> Result<FindingKind, CodecError>
where
    F: FnOnce(u64, ChainLocation, FindingSummary, FindingSummary) -> FindingKind,
{
    Ok(build(
        input.u64("mismatch index")?,
        decode_location(input)?,
        decode_summary(input)?,
        decode_summary(input)?,
    ))
}
