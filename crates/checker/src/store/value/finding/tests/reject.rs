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
        assert!(FindingRecord::new(hash(1), None, FindingStatus::Canonical, kind).is_none());
    }
}

#[test]
fn envelope_and_kind_tags_fail_closed() {
    let bytes = record(FindingKind::ImportedProjectionViolation(
        l1(),
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
        None,
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
    assert_eq!(&bytes[35..42], &[0x0d, 0x01, 0x02, 0x00, 0x01, 0x09, 0x40]);

    let mut bad_presence = bytes.clone();
    bad_presence[39] = 0xff;
    assert!(FindingRecord::decompress(&bad_presence).is_err());

    let mut zero_length = bytes.clone();
    zero_length[40] = 0;
    assert!(FindingRecord::decompress(&zero_length).is_err());

    let mut truncated_length = bytes.clone();
    truncated_length[40] = 0xff;
    assert!(FindingRecord::decompress(&truncated_length).is_err());

    let mut short_key = bytes.clone();
    short_key[40] = 8;
    assert!(FindingRecord::decompress(&short_key).is_err());

    let mut unknown_key = bytes;
    unknown_key[41] = 0xff;
    assert!(FindingRecord::decompress(&unknown_key).is_err());

    let singleton = FindingRecord::new(
        hash(1),
        None,
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
    assert_eq!(&noncanonical_singleton[39..42], &[0x01, 0x01, 0x00]);
    noncanonical_singleton[40] = 2;
    noncanonical_singleton.insert(42, 0);
    assert!(FindingRecord::decompress(&noncanonical_singleton).is_err());
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
    let location = ChainLocation::block(StoredProtocolChain::TempoL1);
    let bytes = FindingRecord::new(
        B256::repeat_byte(1),
        None,
        FindingStatus::Canonical,
        FindingKind::UnsupportedProtocolEvent(location, Address::repeat_byte(2), None),
    )
    .unwrap()
    .compress();
    let mut corrupted = bytes;
    *corrupted.last_mut().unwrap() = 0xff;
    assert!(FindingRecord::decompress(&corrupted).is_err());
}
