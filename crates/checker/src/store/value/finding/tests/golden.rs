use std::collections::BTreeSet;

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};
use reth_codecs::{Compress, Decompress};

use crate::store::schema::ModelKey;

use super::{
    super::super::{FindingKind, FindingRecord, FindingStatus},
    fixtures::{hash, kinds, record},
};
use crate::store::value::finding::StoredProtocolChain;
use crate::store::value::finding::types::{ChainLocation, FindingSummary, LocationKind};

#[derive(Default)]
struct Golden(Vec<u8>);

impl Golden {
    fn byte(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn u128(&mut self, value: u128) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn address(&mut self, value: Address) {
        self.0.extend_from_slice(value.as_slice());
    }

    fn hash(&mut self, value: B256) {
        self.0.extend_from_slice(value.as_slice());
    }

    fn u256(&mut self, value: U256) {
        self.0.extend_from_slice(&value.to_be_bytes::<32>());
    }

    fn finish(self) -> Vec<u8> {
        self.0
    }
}

fn golden_record(value: &FindingRecord) -> Vec<u8> {
    let mut out = Golden::default();
    out.byte(0x01);
    out.hash(value.zone_parent_hash);
    match value.imported_tempo {
        None => out.byte(0x00),
        Some(tip) => {
            out.byte(0x01);
            golden_tip(&mut out, tip);
        }
    }
    out.byte(match value.status {
        FindingStatus::Canonical => 0x00,
        FindingStatus::Orphaned => 0x01,
    });
    golden_kind(&mut out, &value.kind);
    out.finish()
}

fn golden_kind(out: &mut Golden, value: &FindingKind) {
    match value {
        FindingKind::InvalidEnvelope(location, leaf) => {
            out.byte(0x01);
            out.byte(leaf.wire_tag());
            golden_location(out, *location);
        }
        FindingKind::MalformedAuthenticatedData(location, leaf, summary) => {
            golden_categorized(out, 0x02, *location, leaf.wire_tag(), *summary);
        }
        FindingKind::UnsupportedProtocolEvent(location, emitter, topic) => {
            out.byte(0x03);
            golden_location(out, *location);
            out.address(*emitter);
            match topic {
                None => out.byte(0x00),
                Some(topic) => {
                    out.byte(0x01);
                    out.hash(*topic);
                }
            }
        }
        FindingKind::MalformedProtocolEvent(location, emitter, topic, summary) => {
            out.byte(0x04);
            golden_location(out, *location);
            out.address(*emitter);
            out.hash(*topic);
            golden_summary(out, *summary);
        }
        FindingKind::PortalCallViolation(location, leaf, summary) => {
            golden_categorized(out, 0x05, *location, leaf.wire_tag(), *summary);
        }
        FindingKind::ZoneContinuity(expected, number, parent) => {
            golden_continuity(out, 0x06, *expected, *number, *parent);
        }
        FindingKind::TempoContinuity(expected, number, parent) => {
            golden_continuity(out, 0x07, *expected, *number, *parent);
        }
        FindingKind::PortalObservationIdentityMismatch(expected, actual) => {
            out.byte(0x08);
            out.address(*expected);
            out.address(*actual);
        }
        FindingKind::PortalCreationBlockMismatch(expected, actual) => {
            golden_hash_pair(out, 0x09, *expected, *actual);
        }
        FindingKind::PortalCreationMissing(hash) => {
            out.byte(0x0a);
            out.hash(*hash);
        }
        FindingKind::ImportedProjectionViolation(location, leaf, summary) => {
            golden_categorized(out, 0x0b, *location, leaf.wire_tag(), *summary);
        }
        FindingKind::ZoneProjectionViolation(location, leaf, summary) => {
            golden_categorized(out, 0x0c, *location, leaf.wire_tag(), *summary);
        }
        FindingKind::ModelViolation(location, leaf, key, summary) => {
            out.byte(0x0d);
            out.byte(leaf.wire_tag());
            golden_location(out, *location);
            golden_optional_model_key(out, *key);
            golden_summary(out, *summary);
        }
        FindingKind::ImportedOutputCountMismatch(expected, actual) => {
            golden_count(out, 0x0e, *expected, *actual);
        }
        FindingKind::ImportedOutputMismatch(index, location, expected, actual) => {
            golden_indexed_summary(out, 0x0f, *index, *location, *expected, *actual);
        }
        FindingKind::TempoBlockFinalizedMismatch(location, expected, actual) => {
            golden_summary_pair(out, 0x10, *location, *expected, *actual);
        }
        FindingKind::TokenEnableCountMismatch(expected, actual) => {
            golden_count(out, 0x11, *expected, *actual);
        }
        FindingKind::TokenEnableMismatch(index, location, expected, actual) => {
            golden_indexed_summary(out, 0x12, *index, *location, *expected, *actual);
        }
        FindingKind::DepositOutcomeCountMismatch(expected, actual) => {
            golden_count(out, 0x13, *expected, *actual);
        }
        FindingKind::DepositOutcomeMismatch(index, location, expected, actual) => {
            golden_indexed_summary(out, 0x14, *index, *location, *expected, *actual);
        }
        FindingKind::TempoAdvancedMismatch(location, expected, actual) => {
            golden_summary_pair(out, 0x15, *location, *expected, *actual);
        }
        FindingKind::ZoneOperationCountMismatch(expected, actual) => {
            golden_count(out, 0x16, *expected, *actual);
        }
        FindingKind::ZoneOperationMismatch(index, location, expected, actual) => {
            golden_indexed_summary(out, 0x17, *index, *location, *expected, *actual);
        }
        FindingKind::BatchFinalizedMismatch(location, expected, actual) => {
            golden_summary_pair(out, 0x18, *location, *expected, *actual);
        }
        FindingKind::TempoBlockHashMismatch(expected, actual) => {
            golden_hash_pair(out, 0x19, *expected, *actual);
        }
        FindingKind::TempoBlockNumberMismatch(expected, actual) => {
            golden_count(out, 0x1a, *expected, *actual);
        }
        FindingKind::ProcessedDepositHashMismatch(expected, actual) => {
            golden_hash_pair(out, 0x1b, *expected, *actual);
        }
        FindingKind::ProcessedDepositNumberMismatch(expected, actual) => {
            golden_count(out, 0x1c, *expected, *actual);
        }
        FindingKind::WithdrawalQueueHashMismatch(expected, actual) => {
            golden_hash_pair(out, 0x1d, *expected, *actual);
        }
        FindingKind::WithdrawalBatchIndexMismatch(expected, actual) => {
            golden_count(out, 0x1e, *expected, *actual);
        }
        FindingKind::CollateralDeficit(token, required, actual) => {
            out.byte(0x1f);
            out.address(*token);
            out.u256(*required);
            out.u256(*actual);
        }
        FindingKind::MissingSupply(token) => {
            out.byte(0x20);
            out.address(*token);
        }
        FindingKind::SupplyMismatch(token, expected, actual) => {
            out.byte(0x21);
            out.address(*token);
            out.u256(*expected);
            out.u256(*actual);
        }
    }
}

fn golden_location(out: &mut Golden, value: ChainLocation) {
    out.byte(match value.chain {
        StoredProtocolChain::TempoL1 => 0x01,
        StoredProtocolChain::ZoneL2 => 0x02,
    });
    match value.kind {
        LocationKind::Block => out.byte(0x00),
        LocationKind::Transaction(index, hash) => {
            out.byte(0x01);
            out.u64(index);
            out.hash(hash);
        }
        LocationKind::Log {
            transaction_index,
            transaction_hash,
            receipt_log_index,
            block_log_index,
        } => {
            out.byte(0x02);
            out.u64(transaction_index);
            out.hash(transaction_hash);
            out.u64(receipt_log_index);
            out.u64(block_log_index);
        }
    }
}

fn golden_optional_model_key(out: &mut Golden, value: Option<ModelKey>) {
    match value {
        None => out.byte(0x00),
        Some(value) => {
            out.byte(0x01);
            let encoded = golden_model_key(value);
            out.byte(u8::try_from(encoded.len()).unwrap());
            out.0.extend_from_slice(&encoded);
        }
    }
}

fn golden_model_key(value: ModelKey) -> Vec<u8> {
    let mut out = Golden::default();
    match value {
        ModelKey::PortalConfig => out.byte(0x00),
        ModelKey::ZoneConfig => out.byte(0x01),
        ModelKey::PortalDepositCursor => out.byte(0x02),
        ModelKey::ZoneProcessedDepositCursor => out.byte(0x03),
        ModelKey::PortalSettlement => out.byte(0x04),
        ModelKey::ZoneBatchAccumulator => out.byte(0x05),
        ModelKey::ZoneNextWithdrawalIndex => out.byte(0x06),
        ModelKey::ZoneLastFallbackNonce => out.byte(0x07),
        ModelKey::Token(token) => {
            out.byte(0x20);
            out.address(token);
        }
        ModelKey::PendingDeposit(index) => golden_indexed_key(&mut out, 0x30, index),
        ModelKey::Withdrawal(index) => golden_indexed_key(&mut out, 0x40, index),
        ModelKey::FallbackOwner(index) => golden_indexed_key(&mut out, 0x50, index),
        ModelKey::Batch(index) => golden_indexed_key(&mut out, 0x60, index),
        ModelKey::PortalRefundCredit {
            token,
            recipient,
            origin,
        } => golden_refund_key(&mut out, 0x70, token, recipient, origin),
        ModelKey::InboxRefundCredit {
            token,
            recipient,
            origin,
        } => golden_refund_key(&mut out, 0x71, token, recipient, origin),
    }
    out.finish()
}

fn golden_indexed_key(out: &mut Golden, tag: u8, index: u64) {
    out.byte(tag);
    out.u64(index);
}

fn golden_refund_key(out: &mut Golden, tag: u8, token: Address, recipient: Address, origin: u64) {
    out.byte(tag);
    out.address(token);
    out.address(recipient);
    out.u64(origin);
}

fn golden_tip(out: &mut Golden, value: BlockNumHash) {
    out.u64(value.number);
    out.hash(value.hash);
}

fn golden_summary(out: &mut Golden, value: FindingSummary) {
    out.u64(value.length);
    out.hash(value.hash);
}

fn golden_categorized(
    out: &mut Golden,
    tag: u8,
    location: ChainLocation,
    leaf: u8,
    summary: FindingSummary,
) {
    out.byte(tag);
    out.byte(leaf);
    golden_location(out, location);
    golden_summary(out, summary);
}

fn golden_continuity(out: &mut Golden, tag: u8, expected: BlockNumHash, number: u64, parent: B256) {
    out.byte(tag);
    golden_tip(out, expected);
    out.u64(number);
    out.hash(parent);
}

fn golden_count(out: &mut Golden, tag: u8, expected: u64, actual: u64) {
    out.byte(tag);
    out.u64(expected);
    out.u64(actual);
}

fn golden_hash_pair(out: &mut Golden, tag: u8, expected: B256, actual: B256) {
    out.byte(tag);
    out.hash(expected);
    out.hash(actual);
}

fn golden_summary_pair(
    out: &mut Golden,
    tag: u8,
    location: ChainLocation,
    expected: FindingSummary,
    actual: FindingSummary,
) {
    out.byte(tag);
    golden_location(out, location);
    golden_summary(out, expected);
    golden_summary(out, actual);
}

fn golden_indexed_summary(
    out: &mut Golden,
    tag: u8,
    index: u64,
    location: ChainLocation,
    expected: FindingSummary,
    actual: FindingSummary,
) {
    out.byte(tag);
    out.u64(index);
    golden_location(out, location);
    golden_summary(out, expected);
    golden_summary(out, actual);
}

#[test]
fn every_finding_code_has_complete_independent_golden_bytes() {
    let mut covered = BTreeSet::new();
    for (name, kind) in kinds() {
        let value = record(kind);
        let expected = golden_record(&value);
        let actual = value.clone().compress();
        assert_eq!(actual, expected, "wire drift in {name}");
        assert_eq!(FindingRecord::decompress(&actual).unwrap(), value, "{name}");
        covered.insert(expected[75]);
        for cut in 0..actual.len() {
            assert!(
                FindingRecord::decompress(&actual[..cut]).is_err(),
                "truncation accepted for {name} at {cut}"
            );
        }
    }
    assert_eq!(covered, (1..=0x21).collect());
}

#[test]
fn record_envelope_variants_have_complete_golden_bytes() {
    let mut value = FindingRecord::new(
        hash(0x11),
        None,
        FindingStatus::Canonical,
        FindingKind::PortalCreationMissing(hash(0x22)),
    )
    .unwrap();
    assert_eq!(value.clone().compress(), golden_record(&value));

    value.mark_orphaned();
    assert_eq!(value.clone().compress(), golden_record(&value));
    assert_eq!(
        FindingRecord::decompress(&golden_record(&value)).unwrap(),
        value
    );
}
