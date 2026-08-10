use alloy_primitives::{B256, Log, b256};
use tempo_zone_contracts::{MAX_SEQUENCERS, ZoneFactory as Factory};

use super::{
    ProtocolEventError,
    common::{
        malformed, preflight_address_array_count, required_topic, strict_decode_interface,
        unsupported,
    },
};

pub(super) const ZONE_CREATED_TOPIC: B256 =
    b256!("4f2c5b8ee43ce856328b02e8d2b193126eb1c13f34475bd902ecb9a1eaa826a4");
pub(super) const OWNERSHIP_TRANSFERRED_TOPIC: B256 =
    b256!("8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e0");

pub(super) fn decode(log: &Log) -> Result<Option<Factory::ZoneCreated>, ProtocolEventError> {
    let topic = required_topic(log)?;
    match topic {
        ZONE_CREATED_TOPIC => preflight_address_array_count(log, "ZoneCreated", 4, MAX_SEQUENCERS)?,
        OWNERSHIP_TRANSFERRED_TOPIC => {}
        _ => return Err(unsupported(log)),
    }

    match strict_decode_interface::<Factory::ZoneFactoryEvents>(log, "ZoneFactory event")? {
        Factory::ZoneFactoryEvents::ZoneCreated(event) => {
            if event.sequencers.len() > MAX_SEQUENCERS {
                return Err(malformed(
                    log,
                    "ZoneCreated",
                    format!(
                        "address array length {} exceeds {MAX_SEQUENCERS}",
                        event.sequencers.len()
                    ),
                ));
            }
            Ok(Some(event))
        }
        Factory::ZoneFactoryEvents::OwnershipTransferred(_) => Ok(None),
    }
}
