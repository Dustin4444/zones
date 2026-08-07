use alloy_primitives::{B256, Log, b256};

use super::{
    ProtocolEventError,
    common::{required_topic, strict_decode_interface, unsupported},
};

// Checker-owned native TempoState event ABI.
//
// Pinned source: `crates/contracts/src/precompiles/tempo_state.rs:5-8`.
alloy_sol_types::sol! {
    #[derive(Debug, PartialEq, Eq)]
    contract TempoState {
        event TempoBlockFinalized(
            bytes32 indexed blockHash,
            uint64 indexed blockNumber,
            bytes32 stateRoot
        );
    }
}

pub(super) const BLOCK_FINALIZED_TOPIC: B256 =
    b256!("dd85219569c3c880f014955916f426d1ca039714b59ce33e24f151f155ac26b9");

pub(super) fn decode(log: &Log) -> Result<TempoState::TempoStateEvents, ProtocolEventError> {
    match required_topic(log)? {
        BLOCK_FINALIZED_TOPIC => {}
        _ => return Err(unsupported(log)),
    }
    strict_decode_interface(log, "TempoState event")
}
