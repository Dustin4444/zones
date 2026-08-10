use alloy_primitives::{B256, Log, b256};
use tempo_zone_contracts::TempoState;

use super::{
    ProtocolEventError,
    common::{required_topic, strict_decode_interface, unsupported},
};

pub(super) const BLOCK_FINALIZED_TOPIC: B256 =
    b256!("dd85219569c3c880f014955916f426d1ca039714b59ce33e24f151f155ac26b9");

pub(super) fn decode(log: &Log) -> Result<TempoState::TempoStateEvents, ProtocolEventError> {
    if required_topic(log)? != BLOCK_FINALIZED_TOPIC {
        return Err(unsupported(log));
    }
    strict_decode_interface(log, "TempoState event")
}
