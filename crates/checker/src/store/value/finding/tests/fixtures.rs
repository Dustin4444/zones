use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};

use crate::store::schema::ModelKey;

use super::super::{
    ChainLocation, FindingKind, FindingRecord, FindingStatus, FindingSummary, StoredDataSource,
    StoredEnvelopeRule, StoredImportedProjectionError, StoredModelError, StoredPortalCallError,
    StoredProtocolChain, StoredZoneProjectionError,
};

pub(super) fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

pub(super) fn address(byte: u8) -> Address {
    Address::repeat_byte(byte)
}

pub(super) fn summary(byte: u8) -> FindingSummary {
    FindingSummary::new(u64::from(byte), hash(byte))
}

pub(super) fn l1() -> ChainLocation {
    ChainLocation::log(StoredProtocolChain::TempoL1, 2, hash(2), 3, 4)
}

pub(super) fn l2() -> ChainLocation {
    ChainLocation::log(StoredProtocolChain::ZoneL2, 5, hash(5), 6, 7)
}

pub(super) fn record(kind: FindingKind) -> FindingRecord {
    FindingRecord::new(
        hash(0xaa),
        Some(BlockNumHash::new(8, hash(0xbb))),
        FindingStatus::Canonical,
        kind,
    )
    .unwrap()
}

pub(super) fn kinds() -> Vec<(&'static str, FindingKind)> {
    vec![
        (
            "invalid envelope",
            FindingKind::InvalidEnvelope(
                ChainLocation::block(StoredProtocolChain::ZoneL2),
                StoredEnvelopeRule::NonGenesis,
            ),
        ),
        (
            "malformed authenticated data",
            FindingKind::MalformedAuthenticatedData(
                l1(),
                StoredDataSource::PortalTransactionCalldata,
                summary(1),
            ),
        ),
        (
            "unsupported event without topic",
            FindingKind::UnsupportedProtocolEvent(l1(), address(1), None),
        ),
        (
            "unsupported event with topic",
            FindingKind::UnsupportedProtocolEvent(
                ChainLocation::transaction(StoredProtocolChain::ZoneL2, 1, hash(1)),
                address(2),
                Some(hash(2)),
            ),
        ),
        (
            "malformed event",
            FindingKind::MalformedProtocolEvent(l2(), address(3), hash(3), summary(3)),
        ),
        (
            "portal call",
            FindingKind::PortalCallViolation(
                l1(),
                StoredPortalCallError::EmptyProcessWithOutcomes,
                summary(4),
            ),
        ),
        (
            "zone continuity",
            FindingKind::ZoneContinuity(BlockNumHash::new(1, hash(4)), 2, hash(5)),
        ),
        (
            "tempo continuity",
            FindingKind::TempoContinuity(BlockNumHash::new(2, hash(5)), 3, hash(6)),
        ),
        (
            "portal identity",
            FindingKind::PortalObservationIdentityMismatch(address(4), address(5)),
        ),
        (
            "creation block mismatch",
            FindingKind::PortalCreationBlockMismatch(hash(6), hash(7)),
        ),
        (
            "creation missing",
            FindingKind::PortalCreationMissing(hash(8)),
        ),
        (
            "imported projection",
            FindingKind::ImportedProjectionViolation(
                l1(),
                StoredImportedProjectionError::ExtraWithdrawalOutcomes,
                summary(5),
            ),
        ),
        (
            "zone projection",
            FindingKind::ZoneProjectionViolation(
                l2(),
                StoredZoneProjectionError::UnsupportedDepositKind,
                summary(6),
            ),
        ),
        (
            "model violation without key",
            FindingKind::ModelViolation(l2(), StoredModelError::PortalQueueId, None, summary(7)),
        ),
        (
            "model violation with key",
            FindingKind::ModelViolation(
                l2(),
                StoredModelError::WithdrawalQueue,
                Some(ModelKey::Withdrawal(9)),
                summary(8),
            ),
        ),
        (
            "imported count",
            FindingKind::ImportedOutputCountMismatch(1, 2),
        ),
        (
            "imported output",
            FindingKind::ImportedOutputMismatch(3, l1(), summary(9), summary(10)),
        ),
        (
            "tempo finalized",
            FindingKind::TempoBlockFinalizedMismatch(l2(), summary(11), summary(12)),
        ),
        ("token count", FindingKind::TokenEnableCountMismatch(4, 5)),
        (
            "token mismatch",
            FindingKind::TokenEnableMismatch(6, l2(), summary(13), summary(14)),
        ),
        (
            "deposit count",
            FindingKind::DepositOutcomeCountMismatch(7, 8),
        ),
        (
            "deposit mismatch",
            FindingKind::DepositOutcomeMismatch(9, l2(), summary(15), summary(16)),
        ),
        (
            "tempo advanced",
            FindingKind::TempoAdvancedMismatch(l2(), summary(17), summary(18)),
        ),
        (
            "operation count",
            FindingKind::ZoneOperationCountMismatch(10, 11),
        ),
        (
            "operation mismatch",
            FindingKind::ZoneOperationMismatch(12, l2(), summary(19), summary(20)),
        ),
        (
            "batch finalized",
            FindingKind::BatchFinalizedMismatch(l2(), summary(21), summary(22)),
        ),
        (
            "tempo hash",
            FindingKind::TempoBlockHashMismatch(hash(9), hash(10)),
        ),
        (
            "tempo number",
            FindingKind::TempoBlockNumberMismatch(13, 14),
        ),
        (
            "deposit hash",
            FindingKind::ProcessedDepositHashMismatch(hash(11), hash(12)),
        ),
        (
            "deposit number",
            FindingKind::ProcessedDepositNumberMismatch(15, 16),
        ),
        (
            "queue hash",
            FindingKind::WithdrawalQueueHashMismatch(hash(13), hash(14)),
        ),
        (
            "batch index",
            FindingKind::WithdrawalBatchIndexMismatch(17, 18),
        ),
        (
            "collateral deficit",
            FindingKind::CollateralDeficit(address(6), U256::from(19), U256::from(20)),
        ),
        ("missing supply", FindingKind::MissingSupply(address(7))),
        (
            "supply mismatch",
            FindingKind::SupplyMismatch(address(8), U256::from(21), U256::from(22)),
        ),
    ]
}
