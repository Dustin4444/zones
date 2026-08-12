//! Strict Zone Outbox event decoding.

use alloy_primitives::{B256, Log, b256};
use tempo_zone_contracts::IZoneOutbox as Outbox;
use zone_precompiles::{ecies::COMPRESSED_PUBLIC_KEY_SIZE, outbox::MAX_CALLBACK_DATA_SIZE};

use super::{
    ProtocolEventError, malformed, required_topic, strict_decode_interface, unsupported,
    validate_max_bytes,
};

pub(super) const WITHDRAWAL_REQUESTED_TOPIC: B256 =
    b256!("34ca953f3eed14157d2f660c7e92a5bd9c05be0d61a188830f3bf1cb7d094f96");
pub(super) const BATCH_FINALIZED_TOPIC: B256 =
    b256!("ec4aff46c65f485f4b15e3c2edadda1d57d002995f5aa262a27c76b9a680ec16");
pub(super) const TEMPO_GAS_RATE_UPDATED_TOPIC: B256 =
    b256!("6f864cce5237e12ffc9a99fc6c59af17222c2bbb3457690cc8753ab16b5d715e");
pub(super) const MAX_WITHDRAWALS_PER_BLOCK_UPDATED_TOPIC: B256 =
    b256!("5340f6cf6e1274bffd0c7188c75f885ed6c90cd6b0879a646290a92f84a6dce3");

pub(super) fn decode(log: &Log) -> Result<Outbox::IZoneOutboxEvents, ProtocolEventError> {
    match required_topic(log)? {
        WITHDRAWAL_REQUESTED_TOPIC
        | BATCH_FINALIZED_TOPIC
        | TEMPO_GAS_RATE_UPDATED_TOPIC
        | MAX_WITHDRAWALS_PER_BLOCK_UPDATED_TOPIC => {}
        _ => return Err(unsupported(log)),
    }
    let decoded = strict_decode_interface::<Outbox::IZoneOutboxEvents>(log, "ZoneOutbox event")?;
    if let Outbox::IZoneOutboxEvents::WithdrawalRequested(event) = &decoded {
        validate_max_bytes(
            log,
            "WithdrawalRequested",
            "data",
            event.data.len(),
            MAX_CALLBACK_DATA_SIZE,
        )?;
        if !matches!(event.revealTo.len(), 0 | COMPRESSED_PUBLIC_KEY_SIZE) {
            return Err(malformed(
                log,
                "WithdrawalRequested",
                format!(
                    "revealTo byte length {}, expected 0 or {COMPRESSED_PUBLIC_KEY_SIZE}",
                    event.revealTo.len()
                ),
            ));
        }
    }
    Ok(decoded)
}
