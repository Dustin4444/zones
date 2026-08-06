use std::num::NonZeroU64;

use alloy_consensus::{Header, Signed, TxLegacy, transaction::TxHashRef as _};
use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, Signature, U256};
use reth_primitives_traits::{RecoveredBlock, SealedBlock};
use tempo_primitives::{Block, BlockBody, TempoHeader, TempoTxEnvelope};

use crate::{
    check::finding::{
        Finding, FixedStateFinding, ImportedOutputFinding, ObservationFinding, ZoneOutputFinding,
    },
    model::{
        adapter::{ImportedProjectionError, ObservedImportedOutput, ZoneProjectionError},
        events::ProtocolEventError,
        output::ExpectedImportedTempoBlock,
        ownership::DepositId,
        transition::ModelError,
    },
    observe::{
        AuthenticatedDataEvidence, AuthenticatedTransaction, DataSource, EnvelopeLocation,
        EnvelopeRule, PortalCallError, ProtocolChain,
    },
    store::value::{FindingKind, FindingRecord, FindingStatus},
};

fn candidate() -> RecoveredBlock<Block> {
    let transaction = TempoTxEnvelope::Legacy(Signed::new_unhashed(
        TxLegacy::default(),
        Signature::new(U256::ONE, U256::from(2), false),
    ));
    let block = Block {
        header: TempoHeader {
            inner: Header {
                number: 17,
                parent_hash: B256::repeat_byte(0xa1),
                ..Default::default()
            },
            ..Default::default()
        },
        body: BlockBody {
            transactions: vec![transaction],
            ..Default::default()
        },
    };
    RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), vec![Address::ZERO])
}

fn imported_tip() -> BlockNumHash {
    BlockNumHash::new(900, B256::repeat_byte(0xa2))
}

#[test]
fn every_top_level_finding_family_has_a_durable_code() {
    let block = candidate();
    let imported = imported_tip();
    let findings = vec![
        (
            Finding::Observation(Box::new(ObservationFinding::InvalidEnvelope {
                location: EnvelopeLocation::Transaction(0),
                rule: EnvelopeRule::AdvanceSuccess,
            })),
            0x01,
        ),
        (
            Finding::Observation(Box::new(ObservationFinding::MalformedAuthenticatedData {
                kind: DataSource::AdvanceHeaderRlp,
                transaction: AuthenticatedTransaction::new(
                    ProtocolChain::ZoneL2,
                    0,
                    *block.body().transactions[0].tx_hash(),
                ),
                evidence: AuthenticatedDataEvidence::from_bytes(b"header rlp"),
                detail: "discarded display detail".to_owned(),
            })),
            0x02,
        ),
        (
            Finding::Observation(Box::new(ObservationFinding::ProtocolEvent {
                chain: ProtocolChain::TempoL1,
                transaction_index: 4,
                receipt_log_index: 2,
                block_log_index: 9,
                transaction_hash: B256::repeat_byte(0xa3),
                error: Box::new(ProtocolEventError::UnsupportedProtocolEvent {
                    emitter: Address::repeat_byte(0xa4),
                    topic0: None,
                }),
            })),
            0x03,
        ),
        (
            Finding::Observation(Box::new(ObservationFinding::ProtocolEvent {
                chain: ProtocolChain::TempoL1,
                transaction_index: 5,
                receipt_log_index: 3,
                block_log_index: 10,
                transaction_hash: B256::repeat_byte(0xb4),
                error: Box::new(ProtocolEventError::MalformedProtocolEvent {
                    emitter: Address::repeat_byte(0xb5),
                    topic0: B256::repeat_byte(0xb6),
                    event: "TokenEnabled",
                    reason: "discarded decoder formatting".to_owned(),
                }),
            })),
            0x04,
        ),
        (
            Finding::Observation(Box::new(ObservationFinding::PortalCall(
                PortalCallError::ConflictingFamilies {
                    transaction_hash: B256::repeat_byte(0xa5),
                },
            ))),
            0x05,
        ),
        (
            Finding::ZoneContinuity {
                expected_number: 16,
                expected_hash: B256::repeat_byte(0xa6),
                actual_number: 17,
                actual_parent: B256::repeat_byte(0xa7),
            },
            0x06,
        ),
        (
            Finding::TempoContinuity {
                expected_number: 899,
                expected_hash: B256::repeat_byte(0xa8),
                actual_number: 900,
                actual_parent: B256::repeat_byte(0xa9),
            },
            0x07,
        ),
        (
            Finding::PortalObservationIdentityMismatch {
                expected: Address::repeat_byte(0xaa),
                actual: Address::repeat_byte(0xab),
            },
            0x08,
        ),
        (
            Finding::PortalCreationBlockMismatch {
                expected: B256::repeat_byte(0xac),
                actual: B256::repeat_byte(0xad),
            },
            0x09,
        ),
        (
            Finding::PortalCreationMissing {
                block_hash: B256::repeat_byte(0xae),
            },
            0x0a,
        ),
        (
            Finding::ImportedProjection(ImportedProjectionError::MissingBaseFee),
            0x0b,
        ),
        (
            Finding::ZoneProjection(ZoneProjectionError::MissingTempoBlockFinalized),
            0x0c,
        ),
        (Finding::Model(ModelError::PortalNotCreated), 0x0d),
        (
            Finding::ImportedOutput(ImportedOutputFinding::Count {
                expected: 1,
                actual: 2,
            }),
            0x0e,
        ),
        (
            Finding::ZoneOutput(Box::new(ZoneOutputFinding::TokenEnableCount {
                expected: 1,
                actual: 2,
            })),
            0x11,
        ),
        (
            Finding::FixedState(FixedStateFinding::TempoBlockHash {
                expected: B256::repeat_byte(0xaf),
                actual: B256::repeat_byte(0xb0),
            }),
            0x19,
        ),
        (
            Finding::CollateralDeficit {
                token: Address::repeat_byte(0xb1),
                required: U256::from(3),
                actual: U256::from(2),
            },
            0x1f,
        ),
        (
            Finding::MissingSupply {
                token: Address::repeat_byte(0xb2),
            },
            0x20,
        ),
        (
            Finding::SupplyMismatch {
                token: Address::repeat_byte(0xb3),
                expected: U256::from(4),
                actual: U256::from(5),
            },
            0x21,
        ),
    ];

    for (finding, expected_code) in findings {
        let (key, record) =
            FindingRecord::from_candidate(&block, Some(imported), &finding).unwrap();
        assert_eq!(key.zone_height(), 17);
        assert_eq!(key.zone_hash(), block.hash());
        assert_eq!(key.ordinal(), 0);
        assert_eq!(record.zone_parent_hash(), B256::repeat_byte(0xa1));
        assert_eq!(record.imported_tempo(), Some(imported));
        assert_eq!(record.status(), FindingStatus::Canonical);
        assert_eq!(record.kind().code().0, expected_code);
    }
}

#[test]
fn transaction_location_uses_the_candidate_hash() {
    let block = candidate();
    let expected_hash = *block.body().transactions[0].tx_hash();
    let finding = Finding::Observation(Box::new(ObservationFinding::InvalidEnvelope {
        location: EnvelopeLocation::Transaction(0),
        rule: EnvelopeRule::AdvanceDestination,
    }));

    let (_, record) = FindingRecord::from_candidate(&block, None, &finding).unwrap();
    let FindingKind::InvalidEnvelope(location, _) = record.kind() else {
        panic!("wrong durable finding family");
    };
    assert_eq!(location.transaction_coordinate(), Some((0, expected_hash)));

    let log_hash = B256::repeat_byte(0xb7);
    let finding = Finding::Observation(Box::new(ObservationFinding::ProtocolEvent {
        chain: ProtocolChain::TempoL1,
        transaction_index: 6,
        receipt_log_index: 3,
        block_log_index: 11,
        transaction_hash: log_hash,
        error: Box::new(ProtocolEventError::UnsupportedProtocolEvent {
            emitter: Address::repeat_byte(0xb8),
            topic0: None,
        }),
    }));
    let (_, record) =
        FindingRecord::from_candidate(&block, Some(imported_tip()), &finding).unwrap();
    let FindingKind::UnsupportedProtocolEvent(location, ..) = record.kind() else {
        panic!("wrong durable finding family");
    };
    assert_eq!(location.transaction_coordinate(), Some((6, log_hash)));
    assert_eq!(location.log_coordinate(), Some((3, 11)));
}

#[test]
fn partial_authenticated_coordinates_are_typed_without_inventing_fields() {
    let block = candidate();
    let portal_hash = B256::repeat_byte(0xd1);
    let portal_call = Finding::Observation(Box::new(ObservationFinding::PortalCall(
        PortalCallError::ConflictingFamilies {
            transaction_hash: portal_hash,
        },
    )));
    let (_, record) =
        FindingRecord::from_candidate(&block, Some(imported_tip()), &portal_call).unwrap();
    let FindingKind::PortalCallViolation(location, ..) = record.kind() else {
        panic!("wrong durable finding family");
    };
    assert_eq!(location.transaction_hash_coordinate(), Some(portal_hash));
    assert_eq!(location.transaction_index_coordinate(), None);

    let indexed = Finding::ImportedProjection(ImportedProjectionError::InvalidCreationGrammar {
        transaction_index: 12,
    });
    let (_, record) =
        FindingRecord::from_candidate(&block, Some(imported_tip()), &indexed).unwrap();
    let FindingKind::ImportedProjectionViolation(location, ..) = record.kind() else {
        panic!("wrong durable finding family");
    };
    assert_eq!(location.transaction_index_coordinate(), Some(12));
    assert_eq!(location.transaction_hash_coordinate(), None);

    let block_log =
        Finding::ImportedProjection(ImportedProjectionError::InvalidDepositCiphertextLength {
            block_log_index: 13,
            actual: 31,
            expected: 32,
        });
    let (_, record) =
        FindingRecord::from_candidate(&block, Some(imported_tip()), &block_log).unwrap();
    let FindingKind::ImportedProjectionViolation(location, ..) = record.kind() else {
        panic!("wrong durable finding family");
    };
    assert_eq!(location.block_log_index_coordinate(), Some(13));
    assert_eq!(location.transaction_coordinate(), None);

    let exact_hash = B256::repeat_byte(0xd2);
    let exact = Finding::ImportedProjection(ImportedProjectionError::OutcomeCoordinateMismatch {
        transaction_index: 14,
        transaction_hash: exact_hash,
        event_transaction_index: 15,
        event_transaction_hash: B256::repeat_byte(0xd3),
    });
    let (_, record) = FindingRecord::from_candidate(&block, Some(imported_tip()), &exact).unwrap();
    let FindingKind::ImportedProjectionViolation(location, ..) = record.kind() else {
        panic!("wrong durable finding family");
    };
    assert_eq!(location.transaction_coordinate(), Some((14, exact_hash)));
}

#[test]
fn display_only_details_do_not_change_durable_identity() {
    let block = candidate();
    let lower = |detail: &str| {
        let finding =
            Finding::Observation(Box::new(ObservationFinding::MalformedAuthenticatedData {
                kind: DataSource::AdvanceHeaderRlp,
                transaction: AuthenticatedTransaction::new(
                    ProtocolChain::ZoneL2,
                    0,
                    *block.body().transactions[0].tx_hash(),
                ),
                evidence: AuthenticatedDataEvidence::from_bytes(b"header rlp"),
                detail: detail.to_owned(),
            }));
        FindingRecord::from_candidate(&block, None, &finding)
            .unwrap()
            .1
    };

    let first = lower("decoder formatting A");
    assert_eq!(first, lower("decoder formatting B"));
    let FindingKind::MalformedAuthenticatedData(location, _, summary) = first.kind() else {
        panic!("wrong durable finding family");
    };
    assert_eq!(
        location.transaction_coordinate(),
        Some((0, *block.body().transactions[0].tx_hash()))
    );
    assert_eq!(summary.length(), 10);
    assert_eq!(summary.hash(), alloy_primitives::keccak256(b"header rlp"));

    let lower_event = |reason: &str| {
        let finding = Finding::Observation(Box::new(ObservationFinding::ProtocolEvent {
            chain: ProtocolChain::TempoL1,
            transaction_index: 4,
            receipt_log_index: 2,
            block_log_index: 9,
            transaction_hash: B256::repeat_byte(0xc4),
            error: Box::new(ProtocolEventError::MalformedProtocolEvent {
                emitter: Address::repeat_byte(0xc5),
                topic0: B256::repeat_byte(0xc6),
                event: "DepositMade",
                reason: reason.to_owned(),
            }),
        }));
        FindingRecord::from_candidate(&block, Some(imported_tip()), &finding)
            .unwrap()
            .1
    };
    assert_eq!(lower_event("alloy error A"), lower_event("alloy error B"));
}

#[test]
fn mismatch_summaries_change_with_typed_semantic_values() {
    let block = candidate();
    let expected_id = DepositId {
        portal: Address::repeat_byte(0xc1),
        deposit_number: NonZeroU64::new(7).unwrap(),
    };
    let mut expected_block = ExpectedImportedTempoBlock::default();
    expected_block.push_deposit_append_for_test(expected_id, B256::repeat_byte(0xc2));
    let expected = expected_block.operations()[0].clone();
    let lower = |deposit_number| {
        let finding = Finding::ImportedOutput(ImportedOutputFinding::Mismatch {
            index: 0,
            expected: Box::new(expected.clone()),
            actual: Box::new(ObservedImportedOutput::deposit_append_for_test(
                B256::repeat_byte(0xc3),
                deposit_number,
            )),
        });
        FindingRecord::from_candidate(&block, Some(imported_tip()), &finding)
            .unwrap()
            .1
    };

    assert_ne!(lower(8), lower(9));
}

#[test]
fn all_fixed_state_variants_map_without_loss() {
    let block = candidate();
    let variants = [
        FixedStateFinding::TempoBlockHash {
            expected: B256::repeat_byte(1),
            actual: B256::repeat_byte(2),
        },
        FixedStateFinding::TempoBlockNumber {
            expected: 1,
            actual: 2,
        },
        FixedStateFinding::ProcessedDepositHash {
            expected: B256::repeat_byte(3),
            actual: B256::repeat_byte(4),
        },
        FixedStateFinding::ProcessedDepositNumber {
            expected: 3,
            actual: 4,
        },
        FixedStateFinding::WithdrawalQueueHash {
            expected: B256::repeat_byte(5),
            actual: B256::repeat_byte(6),
        },
        FixedStateFinding::WithdrawalBatchIndex {
            expected: 5,
            actual: 6,
        },
    ];

    for (offset, finding) in variants.into_iter().enumerate() {
        let finding = Finding::FixedState(finding);
        let (_, record) =
            FindingRecord::from_candidate(&block, Some(imported_tip()), &finding).unwrap();
        assert_eq!(record.kind().code().0, 0x19 + u8::try_from(offset).unwrap());
    }
}
