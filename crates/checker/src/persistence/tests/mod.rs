use crate::kernel::{
    Datum, Finding as FindingDetails, FindingCategory, FindingLocation, ImportedFacts,
    PortalIdentity, State, StateDelta, ZoneFacts, ZoneOperation, apply_imported, apply_zone,
};
use alloy_primitives::{Address, B256};
use reth_db::{
    Database, TableSet,
    cursor::{DbCursorRO, DbCursorRW},
    transaction::{DbTx, DbTxMut},
};
use tempfile::TempDir;

use super::{
    BlockNumHash, ChainCut, Coverage, Finding, FindingKey, Identity, JournalEntry, MetaValue,
    Persistence, PersistenceError, SCHEMA_VERSION, codec,
    schema::{Checkpoints, Findings, Journal, Meta, MetaKey, PersistenceTables},
};

fn block(number: u64, byte: u8) -> BlockNumHash {
    BlockNumHash {
        number,
        hash: B256::repeat_byte(byte),
    }
}

fn identity() -> Identity {
    Identity {
        l1_chain_id: 1,
        zone_chain_id: 2,
        zone_id: 7,
        portal: Address::repeat_byte(0x70),
        creation_block: B256::repeat_byte(0xc0),
        creation_height: 0,
    }
}

fn state() -> State {
    State::awaiting(PortalIdentity {
        portal: identity().portal,
        zone_id: identity().zone_id,
        initial_token: Address::repeat_byte(0x11),
    })
}

fn bootstrap() -> ChainCut {
    ChainCut {
        zone: block(0, 0x10),
        tempo: block(0, 0x20),
    }
}

fn entry(number: u64, parent: BlockNumHash) -> JournalEntry {
    JournalEntry {
        zone: block(number, 0x10u8.wrapping_add(number as u8)),
        parent,
        imported_tempo: block(number, 0x20u8.wrapping_add(number as u8)),
        imported_tempo_parent: block(
            number.saturating_sub(1),
            0x20u8.wrapping_add(number.saturating_sub(1) as u8),
        ),
        delta: StateDelta::default(),
    }
}

fn create() -> (TempDir, Persistence) {
    let directory = tempfile::tempdir().unwrap();
    let (store, snapshot) =
        Persistence::create(directory.path(), identity(), bootstrap(), state()).unwrap();
    assert_eq!(snapshot.meta.verified_zone_tip, bootstrap().zone);
    (directory, store)
}

fn current(store: &Persistence) -> super::Snapshot {
    store.load().unwrap()
}

fn apply(store: &Persistence, number: u64, parent: BlockNumHash) -> BlockNumHash {
    let snapshot = store.load().unwrap();
    let candidate = apply_zone(
        apply_imported(&snapshot.state, &ImportedFacts::default()).unwrap(),
        &ZoneFacts {
            operations: vec![ZoneOperation::UpdateTempoGasRate(u128::from(number))],
            ..ZoneFacts::default()
        },
    )
    .unwrap();
    let value = JournalEntry {
        delta: candidate.delta,
        ..entry(number, parent)
    };
    let tip = value.zone;
    store
        .apply(&current(store), value, tip, Coverage::Complete)
        .unwrap();
    tip
}

fn finding(zone: BlockNumHash) -> (FindingKey, Finding) {
    super::make_finding(
        zone,
        block(zone.number - 1, 0x10 + zone.number as u8 - 1),
        Some((
            block(zone.number, 0x20 + zone.number as u8),
            block(zone.number - 1, 0x20 + zone.number as u8 - 1),
        )),
        FindingDetails {
            category: FindingCategory::EffectMismatch,
            code: 9,
            location: Some(FindingLocation::ImportedOperation(3)),
            expected: Some(Datum::Code(1)),
            actual: Some(Datum::Code(2)),
        },
        "authenticated divergence".into(),
    )
    .unwrap()
}

mod coverage;
mod findings;
mod loading;
mod reorgs;
mod transactions;
