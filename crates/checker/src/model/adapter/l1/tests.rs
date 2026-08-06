use alloy_consensus::Header;
use alloy_primitives::{Address, B256, Log, U256};
use alloy_sol_types::SolEvent as _;
use tempo_primitives::TempoHeader;
use tempo_zone_contracts::ZonePortal;

use super::{ImportedProjectionError, project_imported};
use crate::{
    model::{events::classify_l1_protocol_event, input::ImportedTempoOperation},
    observe::{ImportedTempoHeader, L1BlockObservation},
};

const BLOCK_NUMBER: u64 = 42;
const BASE_FEE: u64 = 7;
const PORTAL: Address = Address::repeat_byte(0x42);
const TRANSACTION_HASH: B256 = B256::repeat_byte(0x11);

fn imported_header(base_fee_per_gas: Option<u64>) -> ImportedTempoHeader {
    ImportedTempoHeader::for_test(TempoHeader {
        inner: Header {
            number: BLOCK_NUMBER,
            base_fee_per_gas,
            ..Default::default()
        },
        ..Default::default()
    })
}

fn bounceback_gas_updated(value: u64) -> crate::model::events::L1ProtocolEvent {
    classify_l1_protocol_event(
        PORTAL,
        &Log {
            address: PORTAL,
            data: ZonePortal::BouncebackGasUpdated {
                bouncebackGas: value,
            }
            .encode_log_data(),
        },
    )
    .unwrap()
    .unwrap()
}

#[test]
fn projects_minimal_authenticated_imported_block() {
    let imported = imported_header(Some(BASE_FEE));
    let observation = L1BlockObservation::for_test(
        imported.number(),
        imported.hash(),
        PORTAL,
        vec![(TRANSACTION_HASH, vec![bounceback_gas_updated(88)])],
    );

    let projection = project_imported(&observation, &imported).unwrap();

    assert_eq!(projection.input().tempo_block_number(), BLOCK_NUMBER);
    assert_eq!(projection.input().base_fee(), U256::from(BASE_FEE));
    assert!(matches!(
        projection.input().operations(),
        [ImportedTempoOperation::BouncebackGasUpdated(88)]
    ));
    assert!(projection.outputs().is_empty());
}

#[test]
fn rejects_observation_for_a_different_imported_hash() {
    let imported = imported_header(Some(BASE_FEE));
    let actual = B256::repeat_byte(0xa5);
    assert_ne!(actual, imported.hash());
    let observation = L1BlockObservation::for_test(imported.number(), actual, PORTAL, Vec::new());

    assert!(matches!(
        project_imported(&observation, &imported),
        Err(ImportedProjectionError::BlockHashMismatch {
            expected,
            actual: observed,
        }) if expected == imported.hash() && observed == actual
    ));
}

#[test]
fn missing_imported_base_fee_is_not_defaulted() {
    let imported = imported_header(None);
    let observation =
        L1BlockObservation::for_test(imported.number(), imported.hash(), PORTAL, Vec::new());

    assert!(matches!(
        project_imported(&observation, &imported),
        Err(ImportedProjectionError::MissingBaseFee)
    ));
}
