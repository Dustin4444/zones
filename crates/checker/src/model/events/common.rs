use std::mem::size_of;

use alloy_primitives::{B256, IntoLogData, Log};
use alloy_sol_types::SolEventInterface;

use super::ProtocolEventError;
use crate::model::constants::{
    MAX_TOKEN_CURRENCY_BYTES, MAX_TOKEN_NAME_BYTES, MAX_TOKEN_SYMBOL_BYTES,
};

pub(super) fn required_topic(log: &Log) -> Result<B256, ProtocolEventError> {
    log.topics()
        .first()
        .copied()
        .ok_or_else(|| unsupported(log))
}

pub(super) fn unsupported(log: &Log) -> ProtocolEventError {
    ProtocolEventError::UnsupportedProtocolEvent {
        emitter: log.address,
        topic0: log.topics().first().copied(),
    }
}

pub(super) fn malformed(
    log: &Log,
    event: &'static str,
    reason: impl Into<String>,
) -> ProtocolEventError {
    ProtocolEventError::MalformedProtocolEvent {
        emitter: log.address,
        topic0: log.topics().first().copied().unwrap_or(B256::ZERO),
        event,
        reason: reason.into(),
    }
}

/// Decode through a checker-owned generated event interface and reject any
/// encoding that does not round-trip byte-for-byte.
pub(super) fn strict_decode_interface<E>(
    log: &Log,
    emitter: &'static str,
) -> Result<E, ProtocolEventError>
where
    E: SolEventInterface + IntoLogData,
{
    let decoded = E::decode_log(log)
        .map_err(|error| malformed(log, emitter, error.to_string()))?
        .data;
    if decoded.to_log_data() != log.data {
        return Err(malformed(log, emitter, "non-canonical ABI encoding"));
    }
    Ok(decoded)
}

/// Bound a dynamic address-array count before Alloy allocates its decoded Vec.
/// Canonical offsets, elements, padding, and trailing bytes remain the
/// responsibility of strict decode/re-encode equality.
pub(super) fn preflight_address_array_count(
    log: &Log,
    event: &'static str,
    head_word: usize,
    maximum: usize,
) -> Result<(), ProtocolEventError> {
    let data = log.data.data.as_ref();
    let head_offset = head_word
        .checked_mul(32)
        .ok_or_else(|| malformed(log, event, "dynamic head offset overflow"))?;
    let tail_offset =
        read_usize_word(data, head_offset).map_err(|reason| malformed(log, event, reason))?;
    let count =
        read_usize_word(data, tail_offset).map_err(|reason| malformed(log, event, reason))?;
    if count > maximum {
        return Err(malformed(
            log,
            event,
            format!("address array length {count} exceeds {maximum}"),
        ));
    }
    Ok(())
}

pub(super) fn validate_token_metadata(
    log: &Log,
    event: &'static str,
    name: &str,
    symbol: &str,
    currency: &str,
) -> Result<(), ProtocolEventError> {
    validate_max_bytes(log, event, "name", name.len(), MAX_TOKEN_NAME_BYTES)?;
    validate_max_bytes(log, event, "symbol", symbol.len(), MAX_TOKEN_SYMBOL_BYTES)?;
    validate_max_bytes(
        log,
        event,
        "currency",
        currency.len(),
        MAX_TOKEN_CURRENCY_BYTES,
    )
}

pub(super) fn validate_max_bytes(
    log: &Log,
    event: &'static str,
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), ProtocolEventError> {
    if actual > maximum {
        return Err(malformed(
            log,
            event,
            format!("{field} byte length {actual} exceeds {maximum}"),
        ));
    }
    Ok(())
}

pub(super) fn validate_exact_bytes(
    log: &Log,
    event: &'static str,
    field: &'static str,
    actual: usize,
    expected: usize,
) -> Result<(), ProtocolEventError> {
    if actual != expected {
        return Err(malformed(
            log,
            event,
            format!("{field} byte length {actual}, expected {expected}"),
        ));
    }
    Ok(())
}

fn read_usize_word(data: &[u8], offset: usize) -> Result<usize, String> {
    let end = offset
        .checked_add(32)
        .ok_or_else(|| "ABI word offset overflow".to_owned())?;
    let word = data
        .get(offset..end)
        .ok_or_else(|| "ABI word exceeds log body".to_owned())?;
    let width = size_of::<usize>();
    if word[..32 - width].iter().any(|byte| *byte != 0) {
        return Err("ABI offset/length does not fit usize".to_owned());
    }
    let mut value = 0usize;
    for byte in &word[32 - width..] {
        value = (value << 8) | usize::from(*byte);
    }
    Ok(value)
}
