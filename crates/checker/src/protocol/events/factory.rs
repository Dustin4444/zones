// Generated constructors inherit the immutable protocol event arity.
#![allow(clippy::too_many_arguments)]

use alloy_primitives::{B256, Log, b256};

use crate::protocol::constants::MAX_SEQUENCERS;

use super::{
    ProtocolEventError,
    common::{
        malformed, preflight_address_array_count, required_topic, strict_decode_interface,
        unsupported,
    },
};

// Checker-owned ZoneFactory event ABI.
//
// Pinned source: `crates/contracts/src/precompiles/zone_factory.rs:40-51`.
alloy_sol_types::sol! {
    #[derive(Debug, PartialEq, Eq)]
    contract Factory {
        event ZoneCreated(
            uint32 indexed zoneId,
            address indexed portal,
            address initialToken,
            bool accessMode,
            bool gatewayMode,
            address admin,
            address[] sequencers,
            uint8 threshold,
            address verifier
        );
        event OwnershipTransferred(
            address indexed previousOwner,
            address indexed newOwner
        );
    }
}

pub(super) const ZONE_CREATED_TOPIC: B256 =
    b256!("4f2c5b8ee43ce856328b02e8d2b193126eb1c13f34475bd902ecb9a1eaa826a4");
pub(super) const OWNERSHIP_TRANSFERRED_TOPIC: B256 =
    b256!("8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e0");

pub(super) fn decode(log: &Log) -> Result<Option<Factory::ZoneCreated>, ProtocolEventError> {
    let topic = required_topic(log)?;
    match topic {
        ZONE_CREATED_TOPIC => {
            // Four static body words precede `sequencers`.
            preflight_address_array_count(log, "ZoneCreated", 4, MAX_SEQUENCERS)?;
        }
        OWNERSHIP_TRANSFERRED_TOPIC => {}
        _ => return Err(unsupported(log)),
    }

    let decoded = strict_decode_interface::<Factory::FactoryEvents>(log, "ZoneFactory event")?;
    match decoded {
        Factory::FactoryEvents::ZoneCreated(event) => {
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
        Factory::FactoryEvents::OwnershipTransferred(_) => Ok(None),
    }
}
