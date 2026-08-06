use alloy_consensus::Header;
use alloy_primitives::{B256, U256};
use tempo_primitives::TempoHeader;

use super::{ZoneProjectionError, project_zone};
use crate::{
    model::events::{Inbox, L2ProtocolEvent, TempoState},
    observe::{ImportedTempoHeader, L2BlockObservation},
};

const IMPORTED_NUMBER: u64 = 100;
const ZONE_NUMBER: u64 = 9;
const ZONE_HASH: B256 = B256::repeat_byte(0x29);
const ZONE_PARENT_HASH: B256 = B256::repeat_byte(0x19);
const ADVANCE_HASH: B256 = B256::repeat_byte(0xa0);
const STATE_ROOT: B256 = B256::repeat_byte(0x31);
const PROCESSED_QUEUE_HASH: B256 = B256::repeat_byte(0x41);

fn imported_header() -> ImportedTempoHeader {
    ImportedTempoHeader::for_test(TempoHeader {
        inner: Header {
            number: IMPORTED_NUMBER,
            state_root: STATE_ROOT,
            ..Default::default()
        },
        ..Default::default()
    })
}

fn tempo_block_finalized(imported: &ImportedTempoHeader) -> L2ProtocolEvent {
    L2ProtocolEvent::TempoState(TempoState::TempoStateEvents::TempoBlockFinalized(
        TempoState::TempoBlockFinalized {
            blockHash: imported.hash(),
            blockNumber: imported.number(),
            stateRoot: STATE_ROOT,
        },
    ))
}

fn tempo_advanced(imported: &ImportedTempoHeader) -> L2ProtocolEvent {
    L2ProtocolEvent::Inbox(Inbox::InboxEvents::TempoAdvanced(Inbox::TempoAdvanced {
        tempoBlockHash: imported.hash(),
        tempoBlockNumber: imported.number(),
        depositsProcessed: U256::ZERO,
        newProcessedDepositQueueHash: PROCESSED_QUEUE_HASH,
        lastProcessedDepositNumber: 0,
    }))
}

fn observation(events: Vec<L2ProtocolEvent>) -> L2BlockObservation {
    let imported = imported_header();
    L2BlockObservation::for_test(
        ZONE_NUMBER,
        ZONE_HASH,
        ZONE_PARENT_HASH,
        ADVANCE_HASH,
        imported,
        events,
    )
}

#[test]
fn projects_minimal_advance_grammar() {
    let imported = imported_header();
    let projection = project_zone(&observation(vec![
        tempo_block_finalized(&imported),
        tempo_advanced(&imported),
    ]))
    .unwrap();

    let context = projection.input().context();
    assert_eq!(context.block_number(), ZONE_NUMBER);
    assert_eq!(context.block_hash(), ZONE_HASH);
    assert!(projection.input().advance().enabled_tokens().is_empty());
    assert!(projection.input().advance().deposits().is_empty());
    assert!(projection.input().advance().outcomes().is_empty());
    assert!(projection.input().operations().is_empty());
    assert!(projection.input().finalization().is_none());

    let outputs = projection.outputs();
    assert_eq!(
        outputs.tempo_block_finalized().block_hash(),
        imported.hash()
    );
    assert_eq!(outputs.tempo_advanced().tempo_block_hash(), imported.hash());
    assert!(outputs.token_enables().is_empty());
    assert!(outputs.deposit_outcomes().is_empty());
    assert!(outputs.operations().is_empty());
    assert!(outputs.batch_finalized().is_none());
}

#[test]
fn rejects_missing_terminal_advance_output() {
    let imported = imported_header();

    assert!(matches!(
        project_zone(&observation(vec![tempo_block_finalized(&imported)])),
        Err(ZoneProjectionError::MissingTempoAdvanced)
    ));
}

#[test]
fn rejects_reordered_opening_advance_output() {
    let imported = imported_header();

    assert!(matches!(
        project_zone(&observation(vec![tempo_advanced(&imported)])),
        Err(ZoneProjectionError::ReorderedTempoBlockFinalized { .. })
    ));
}
