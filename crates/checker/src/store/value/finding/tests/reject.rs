use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256};
use reth_codecs::{Compress, Decompress};

use crate::store::schema::ModelKey;

use super::{
    super::super::{
        ChainLocation, FindingKind, FindingRecord, FindingStatus, FindingSummary, StoredDataSource,
        StoredEnvelopeRule, StoredImportedProjectionError, StoredModelError, StoredPortalCallError,
        StoredProtocolChain, StoredZoneProjectionError,
    },
    fixtures::{hash, l1, l2, record, summary},
};
use crate::store::value::finding::types::MAX_RECORD_SIZE;

fn imported_tip() -> BlockNumHash {
    BlockNumHash::new(8, hash(0xbb))
}

#[test]
fn construction_rejects_bad_chain_combinations() {
    let invalid = [
        FindingKind::InvalidEnvelope(l1(), StoredEnvelopeRule::NonGenesis),
        FindingKind::MalformedAuthenticatedData(
            l1(),
            StoredDataSource::AdvanceTempoCalldata,
            summary(1),
        ),
        FindingKind::MalformedAuthenticatedData(
            l2(),
            StoredDataSource::PortalTransactionCalldata,
            summary(1),
        ),
        FindingKind::PortalCallViolation(
            l2(),
            StoredPortalCallError::UnsupportedNestedPortalCall,
            summary(1),
        ),
        FindingKind::ImportedProjectionViolation(
            l2(),
            StoredImportedProjectionError::MissingBaseFee,
            summary(1),
        ),
        FindingKind::ZoneProjectionViolation(
            l1(),
            StoredZoneProjectionError::MissingTempoBlockFinalized,
            summary(1),
        ),
        FindingKind::ImportedOutputMismatch(0, l2(), summary(1), summary(2)),
        FindingKind::TempoBlockFinalizedMismatch(l1(), summary(1), summary(2)),
        FindingKind::TokenEnableMismatch(0, l1(), summary(1), summary(2)),
        FindingKind::DepositOutcomeMismatch(0, l1(), summary(1), summary(2)),
        FindingKind::TempoAdvancedMismatch(l1(), summary(1), summary(2)),
        FindingKind::ZoneOperationMismatch(0, l1(), summary(1), summary(2)),
        FindingKind::BatchFinalizedMismatch(l1(), summary(1), summary(2)),
    ];
    for kind in invalid {
        assert!(
            FindingRecord::new(
                hash(1),
                Some(imported_tip()),
                FindingStatus::Canonical,
                kind
            )
            .is_none()
        );
    }
}

#[test]
fn construction_rejects_locations_that_do_not_match_the_finding_leaf() {
    let invalid = [
        FindingKind::InvalidEnvelope(
            ChainLocation::transaction(StoredProtocolChain::ZoneL2, 1, hash(1)),
            StoredEnvelopeRule::NonGenesis,
        ),
        FindingKind::InvalidEnvelope(
            ChainLocation::block(StoredProtocolChain::ZoneL2),
            StoredEnvelopeRule::AdvanceSuccess,
        ),
        FindingKind::MalformedAuthenticatedData(
            ChainLocation::block(StoredProtocolChain::ZoneL2),
            StoredDataSource::AdvanceHeaderRlp,
            summary(1),
        ),
        FindingKind::UnsupportedProtocolEvent(
            ChainLocation::transaction(StoredProtocolChain::TempoL1, 1, hash(1)),
            Address::repeat_byte(1),
            None,
        ),
        FindingKind::PortalCallViolation(
            l1(),
            StoredPortalCallError::UnsupportedNestedPortalCall,
            summary(1),
        ),
        FindingKind::ImportedProjectionViolation(
            ChainLocation::transaction_index(StoredProtocolChain::TempoL1, 1),
            StoredImportedProjectionError::MissingBaseFee,
            summary(1),
        ),
        FindingKind::ImportedProjectionViolation(
            ChainLocation::transaction_index(StoredProtocolChain::TempoL1, 1),
            StoredImportedProjectionError::OutcomeCoordinateMismatch,
            summary(1),
        ),
        FindingKind::ImportedProjectionViolation(
            ChainLocation::block(StoredProtocolChain::TempoL1),
            StoredImportedProjectionError::InvalidCreationGrammar,
            summary(1),
        ),
        FindingKind::ImportedProjectionViolation(
            ChainLocation::transaction_index(StoredProtocolChain::TempoL1, 1),
            StoredImportedProjectionError::InvalidDepositKeyParity,
            summary(1),
        ),
        FindingKind::ZoneProjectionViolation(
            ChainLocation::block(StoredProtocolChain::ZoneL2),
            StoredZoneProjectionError::ReorderedTempoBlockFinalized,
            summary(1),
        ),
        FindingKind::ZoneProjectionViolation(
            ChainLocation::block(StoredProtocolChain::ZoneL2),
            StoredZoneProjectionError::InvalidWithdrawalRequest,
            summary(1),
        ),
        FindingKind::ZoneProjectionViolation(
            l2(),
            StoredZoneProjectionError::MissingTempoBlockFinalized,
            summary(1),
        ),
        FindingKind::ModelViolation(l2(), StoredModelError::PortalNotCreated, None, summary(1)),
        FindingKind::ImportedOutputMismatch(
            0,
            ChainLocation::block(StoredProtocolChain::TempoL1),
            summary(1),
            summary(2),
        ),
        FindingKind::TempoBlockFinalizedMismatch(
            ChainLocation::block(StoredProtocolChain::ZoneL2),
            summary(1),
            summary(2),
        ),
        FindingKind::BatchFinalizedMismatch(
            ChainLocation::transaction(StoredProtocolChain::ZoneL2, 1, hash(1)),
            summary(1),
            summary(2),
        ),
    ];

    for kind in invalid {
        assert!(
            FindingRecord::new(
                hash(1),
                Some(imported_tip()),
                FindingStatus::Canonical,
                kind
            )
            .is_none()
        );
    }
}

#[test]
fn only_pre_import_l2_failures_may_omit_the_imported_tip() {
    let early_envelope_failures = [
        (
            ChainLocation::block(StoredProtocolChain::ZoneL2),
            StoredEnvelopeRule::NonGenesis,
        ),
        (
            ChainLocation::block(StoredProtocolChain::ZoneL2),
            StoredEnvelopeRule::AdvancePresent,
        ),
        (
            ChainLocation::transaction(StoredProtocolChain::ZoneL2, 0, hash(1)),
            StoredEnvelopeRule::AdvanceSystemCaller,
        ),
        (
            ChainLocation::transaction(StoredProtocolChain::ZoneL2, 0, hash(1)),
            StoredEnvelopeRule::AdvanceDestination,
        ),
        (
            ChainLocation::transaction(StoredProtocolChain::ZoneL2, 0, hash(1)),
            StoredEnvelopeRule::AdvanceSuccess,
        ),
    ];
    for (location, leaf) in early_envelope_failures {
        assert!(
            FindingRecord::new(
                hash(1),
                None,
                FindingStatus::Canonical,
                FindingKind::InvalidEnvelope(location, leaf),
            )
            .is_some()
        );
    }

    let early_data_failures = [
        StoredDataSource::AdvanceTempoCalldata,
        StoredDataSource::AdvanceHeaderRlp,
        StoredDataSource::OrdinaryDepositData,
        StoredDataSource::WithdrawalBounceBackData,
    ];
    for source in early_data_failures {
        assert!(
            FindingRecord::new(
                hash(1),
                None,
                FindingStatus::Canonical,
                FindingKind::MalformedAuthenticatedData(
                    ChainLocation::transaction(StoredProtocolChain::ZoneL2, 0, hash(1)),
                    source,
                    summary(1),
                ),
            )
            .is_some()
        );
    }

    let late_l2_failures = [
        FindingKind::InvalidEnvelope(
            ChainLocation::transaction(StoredProtocolChain::ZoneL2, 1, hash(1)),
            StoredEnvelopeRule::SystemIdentity,
        ),
        FindingKind::MalformedAuthenticatedData(
            ChainLocation::transaction(StoredProtocolChain::ZoneL2, 1, hash(1)),
            StoredDataSource::FinalizationCalldata,
            summary(1),
        ),
        FindingKind::MalformedProtocolEvent(l2(), Address::repeat_byte(1), hash(1), summary(1)),
        FindingKind::MissingSupply(Address::repeat_byte(1)),
    ];
    for kind in late_l2_failures {
        assert!(FindingRecord::new(hash(1), None, FindingStatus::Canonical, kind).is_none());
    }
}

#[test]
fn tempo_locations_require_an_authenticated_imported_tip() {
    let kinds = [
        FindingKind::MalformedAuthenticatedData(
            ChainLocation::transaction(StoredProtocolChain::TempoL1, 1, hash(1)),
            StoredDataSource::PortalTransactionCalldata,
            summary(1),
        ),
        FindingKind::UnsupportedProtocolEvent(l1(), Address::repeat_byte(1), None),
        FindingKind::PortalCallViolation(
            ChainLocation::transaction_hash(StoredProtocolChain::TempoL1, hash(1)),
            StoredPortalCallError::ConflictingFamilies,
            summary(1),
        ),
        FindingKind::ImportedProjectionViolation(
            ChainLocation::transaction_index(StoredProtocolChain::TempoL1, 1),
            StoredImportedProjectionError::InvalidCreationGrammar,
            summary(1),
        ),
        FindingKind::ImportedProjectionViolation(
            ChainLocation::block_log_index(StoredProtocolChain::TempoL1, 1),
            StoredImportedProjectionError::InvalidDepositKeyParity,
            summary(1),
        ),
        FindingKind::ImportedOutputMismatch(
            0,
            ChainLocation::transaction(StoredProtocolChain::TempoL1, 1, hash(1)),
            summary(1),
            summary(2),
        ),
        FindingKind::ImportedOutputMismatch(0, l1(), summary(1), summary(2)),
    ];

    for kind in kinds {
        assert!(FindingRecord::new(hash(1), None, FindingStatus::Canonical, kind).is_none());
    }
}

#[test]
fn envelope_and_kind_tags_fail_closed() {
    let bytes = record(FindingKind::ImportedProjectionViolation(
        ChainLocation::block(StoredProtocolChain::TempoL1),
        StoredImportedProjectionError::MissingBaseFee,
        summary(2),
    ))
    .compress();
    for (offset, bad, label) in [
        (0, 0xff, "version"),
        (33, 0xff, "optional tip"),
        (74, 0xff, "status"),
        (75, 0xff, "kind"),
        (76, 0x00, "leaf"),
        (77, 0xff, "chain"),
        (78, 0xff, "location"),
    ] {
        let mut corrupted = bytes.clone();
        corrupted[offset] = bad;
        assert!(
            FindingRecord::decompress(&corrupted).is_err(),
            "accepted {label}"
        );
    }

    let mut trailing = bytes;
    trailing.push(0);
    assert!(FindingRecord::decompress(&trailing).is_err());
    assert!(FindingRecord::decompress(&vec![0; MAX_RECORD_SIZE + 1]).is_err());
}

#[test]
fn malformed_optional_model_keys_fail_closed() {
    let value = FindingRecord::new(
        hash(1),
        Some(imported_tip()),
        FindingStatus::Canonical,
        FindingKind::ModelViolation(
            ChainLocation::block(StoredProtocolChain::ZoneL2),
            StoredModelError::PortalNotCreated,
            Some(ModelKey::Withdrawal(9)),
            summary(2),
        ),
    )
    .unwrap();
    let bytes = value.compress();
    assert_eq!(&bytes[75..82], &[0x0d, 0x01, 0x02, 0x00, 0x01, 0x09, 0x40]);

    let mut bad_presence = bytes.clone();
    bad_presence[79] = 0xff;
    assert!(FindingRecord::decompress(&bad_presence).is_err());

    let mut zero_length = bytes.clone();
    zero_length[80] = 0;
    assert!(FindingRecord::decompress(&zero_length).is_err());

    let mut truncated_length = bytes.clone();
    truncated_length[80] = 0xff;
    assert!(FindingRecord::decompress(&truncated_length).is_err());

    let mut short_key = bytes.clone();
    short_key[80] = 8;
    assert!(FindingRecord::decompress(&short_key).is_err());

    let mut unknown_key = bytes;
    unknown_key[81] = 0xff;
    assert!(FindingRecord::decompress(&unknown_key).is_err());

    let singleton = FindingRecord::new(
        hash(1),
        Some(imported_tip()),
        FindingStatus::Canonical,
        FindingKind::ModelViolation(
            ChainLocation::block(StoredProtocolChain::ZoneL2),
            StoredModelError::PortalNotCreated,
            Some(ModelKey::PortalConfig),
            summary(2),
        ),
    )
    .unwrap();
    let mut noncanonical_singleton = singleton.compress();
    assert_eq!(&noncanonical_singleton[79..82], &[0x01, 0x01, 0x00]);
    noncanonical_singleton[80] = 2;
    noncanonical_singleton.insert(82, 0);
    assert!(FindingRecord::decompress(&noncanonical_singleton).is_err());
}

#[test]
fn semantically_invalid_wire_records_fail_closed() {
    let mut missing_required_tip = record(FindingKind::PortalCallViolation(
        ChainLocation::transaction_hash(StoredProtocolChain::TempoL1, hash(1)),
        StoredPortalCallError::ConflictingFamilies,
        summary(1),
    ))
    .compress();
    missing_required_tip[33] = 0;
    missing_required_tip.drain(34..74);
    assert!(FindingRecord::decompress(&missing_required_tip).is_err());

    let mut wrong_imported_projection_shape = record(FindingKind::ImportedProjectionViolation(
        ChainLocation::transaction_index(StoredProtocolChain::TempoL1, 1),
        StoredImportedProjectionError::InvalidCreationGrammar,
        summary(1),
    ))
    .compress();
    wrong_imported_projection_shape[76] = StoredImportedProjectionError::MissingBaseFee.wire_tag();
    assert!(FindingRecord::decompress(&wrong_imported_projection_shape).is_err());

    let mut wrong_zone_projection_shape = record(FindingKind::ZoneProjectionViolation(
        l2(),
        StoredZoneProjectionError::ReorderedTempoBlockFinalized,
        summary(1),
    ))
    .compress();
    wrong_zone_projection_shape[76] =
        StoredZoneProjectionError::MissingTempoBlockFinalized.wire_tag();
    assert!(FindingRecord::decompress(&wrong_zone_projection_shape).is_err());

    let mut wrong_envelope_shape = record(FindingKind::InvalidEnvelope(
        ChainLocation::transaction(StoredProtocolChain::ZoneL2, 0, hash(1)),
        StoredEnvelopeRule::AdvanceSuccess,
    ))
    .compress();
    wrong_envelope_shape[76] = StoredEnvelopeRule::NonGenesis.wire_tag();
    assert!(FindingRecord::decompress(&wrong_envelope_shape).is_err());
}

#[test]
fn summaries_and_record_accessors_are_stable() {
    let value = FindingSummary::from_bytes(b"finding evidence");
    assert_eq!(value.length(), 16);
    assert_eq!(
        value.hash(),
        alloy_primitives::keccak256(b"finding evidence")
    );

    let mut finding = FindingRecord::new(
        hash(3),
        Some(BlockNumHash::new(4, hash(4))),
        FindingStatus::Canonical,
        FindingKind::MissingSupply(Address::repeat_byte(5)),
    )
    .unwrap();
    assert_eq!(finding.zone_parent_hash(), hash(3));
    assert_eq!(
        finding.imported_tempo(),
        Some(BlockNumHash::new(4, hash(4)))
    );
    assert!(matches!(finding.kind(), FindingKind::MissingSupply(_)));
    finding.mark_orphaned();
    assert_eq!(finding.status(), FindingStatus::Orphaned);
}

#[test]
fn optional_topic_tags_fail_closed() {
    let bytes = FindingRecord::new(
        B256::repeat_byte(1),
        Some(imported_tip()),
        FindingStatus::Canonical,
        FindingKind::UnsupportedProtocolEvent(l1(), Address::repeat_byte(2), None),
    )
    .unwrap()
    .compress();
    let mut corrupted = bytes;
    *corrupted.last_mut().unwrap() = 0xff;
    assert!(FindingRecord::decompress(&corrupted).is_err());
}
