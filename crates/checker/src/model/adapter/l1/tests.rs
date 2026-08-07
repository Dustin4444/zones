use alloy_consensus::Header;
use alloy_primitives::{Address, B256, Log, U256};
use alloy_sol_types::SolEvent as _;
use tempo_primitives::TempoHeader;
use tempo_zone_contracts::ZonePortal;

use super::{ImportedProjectionError, PortalCreationIdentityError, project_imported};
use crate::{
    model::{
        constants::ZONE_FACTORY_ADDRESS,
        events::{Factory, L1ProtocolEvent, Portal, classify_l1_protocol_event},
        input::ImportedTempoOperation,
        state::PortalIdentity,
    },
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

fn creation_events(zone_id: u32, initial_token: Address) -> Vec<L1ProtocolEvent> {
    [
        Log {
            address: PORTAL,
            data: Portal::TokenEnabled {
                token: initial_token,
                name: "Token".into(),
                symbol: "TOK".into(),
                currency: "USD".into(),
            }
            .encode_log_data(),
        },
        Log {
            address: ZONE_FACTORY_ADDRESS,
            data: Factory::ZoneCreated {
                zoneId: zone_id,
                portal: PORTAL,
                initialToken: initial_token,
                accessMode: true,
                gatewayMode: false,
                admin: Address::repeat_byte(0xa1),
                sequencers: Vec::new(),
                threshold: 0,
                verifier: Address::repeat_byte(0xa2),
            }
            .encode_log_data(),
        },
    ]
    .iter()
    .map(|log| {
        classify_l1_protocol_event(PORTAL, log)
            .unwrap()
            .expect("creation log is model-driving")
    })
    .collect()
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

    let projection = project_imported(&observation, &imported, imported.hash()).unwrap();

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
        project_imported(&observation, &imported, imported.hash()),
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
        project_imported(&observation, &imported, imported.hash()),
        Err(ImportedProjectionError::MissingBaseFee)
    ));
}

#[test]
fn exposes_the_sole_authenticated_portal_creation_identity() {
    let imported = imported_header(Some(BASE_FEE));
    let initial_token = Address::repeat_byte(0xc1);
    let observation = L1BlockObservation::for_test(
        imported.number(),
        imported.hash(),
        PORTAL,
        vec![(TRANSACTION_HASH, creation_events(7, initial_token))],
    );

    let projection = project_imported(&observation, &imported, imported.hash()).unwrap();

    assert_eq!(
        projection.sole_portal_creation_identity().unwrap(),
        PortalIdentity::new(PORTAL, 7, initial_token)
    );
}

#[test]
fn creation_identity_requires_exactly_one_creation_operation() {
    let imported = imported_header(Some(BASE_FEE));
    let empty =
        L1BlockObservation::for_test(imported.number(), imported.hash(), PORTAL, Vec::new());
    let projection = project_imported(&empty, &imported, imported.hash()).unwrap();
    assert_eq!(
        projection.sole_portal_creation_identity(),
        Err(PortalCreationIdentityError::Missing)
    );

    let initial_token = Address::repeat_byte(0xc1);
    let duplicate = L1BlockObservation::for_test(
        imported.number(),
        imported.hash(),
        PORTAL,
        vec![
            (TRANSACTION_HASH, creation_events(7, initial_token)),
            (B256::repeat_byte(0x12), creation_events(7, initial_token)),
        ],
    );
    let projection = project_imported(&duplicate, &imported, imported.hash()).unwrap();
    assert_eq!(
        projection.sole_portal_creation_identity(),
        Err(PortalCreationIdentityError::Multiple { count: 2 })
    );
}

#[test]
fn ignores_a_matching_factory_creation_after_the_configured_creation_block() {
    let imported = imported_header(Some(BASE_FEE));
    let configured_creation_hash = B256::repeat_byte(0xc7);
    assert_ne!(imported.hash(), configured_creation_hash);
    let observation = L1BlockObservation::for_test(
        imported.number(),
        imported.hash(),
        PORTAL,
        vec![(
            TRANSACTION_HASH,
            vec![
                creation_events(7, Address::repeat_byte(0xc1))
                    .into_iter()
                    .last()
                    .unwrap(),
            ],
        )],
    );

    let projection = project_imported(&observation, &imported, configured_creation_hash).unwrap();

    assert!(projection.input().operations().is_empty());
    assert!(projection.outputs().is_empty());
}
