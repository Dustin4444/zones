use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256, keccak256};

use crate::store::schema::ModelKey;

use super::leaf::{
    StoredDataSource, StoredEnvelopeRule, StoredImportedProjectionError, StoredModelError,
    StoredPortalCallError, StoredProtocolChain, StoredZoneProjectionError,
};

pub(super) const MAX_RECORD_SIZE: usize = 2_048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LocationKind {
    Block,
    Transaction(u64, B256),
    /// The authenticated error retained an index but not the transaction body/hash.
    TransactionIndex(u64),
    /// The authenticated error retained a hash but not its ordered block index.
    TransactionHash(B256),
    /// The authenticated error retained only its block-global log index.
    BlockLogIndex(u64),
    Log {
        transaction_index: u64,
        transaction_hash: B256,
        receipt_log_index: u64,
        block_log_index: u64,
    },
}

/// The authenticated block, transaction, or log coordinates available at the
/// failure site. Partial transaction/log variants never invent missing fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainLocation {
    pub(super) chain: StoredProtocolChain,
    pub(super) kind: LocationKind,
}

impl ChainLocation {
    pub(crate) const fn block(chain: StoredProtocolChain) -> Self {
        Self {
            chain,
            kind: LocationKind::Block,
        }
    }

    pub(crate) const fn transaction(chain: StoredProtocolChain, index: u64, hash: B256) -> Self {
        Self {
            chain,
            kind: LocationKind::Transaction(index, hash),
        }
    }

    pub(crate) const fn transaction_index(chain: StoredProtocolChain, index: u64) -> Self {
        Self {
            chain,
            kind: LocationKind::TransactionIndex(index),
        }
    }

    pub(crate) const fn transaction_hash(chain: StoredProtocolChain, hash: B256) -> Self {
        Self {
            chain,
            kind: LocationKind::TransactionHash(hash),
        }
    }

    pub(crate) const fn block_log_index(chain: StoredProtocolChain, block_log_index: u64) -> Self {
        Self {
            chain,
            kind: LocationKind::BlockLogIndex(block_log_index),
        }
    }

    pub(crate) const fn log(
        chain: StoredProtocolChain,
        transaction_index: u64,
        transaction_hash: B256,
        receipt_log_index: u64,
        block_log_index: u64,
    ) -> Self {
        Self {
            chain,
            kind: LocationKind::Log {
                transaction_index,
                transaction_hash,
                receipt_log_index,
                block_log_index,
            },
        }
    }

    pub(crate) const fn chain(self) -> StoredProtocolChain {
        self.chain
    }

    pub(crate) const fn transaction_coordinate(self) -> Option<(u64, B256)> {
        match self.kind {
            LocationKind::Block => None,
            LocationKind::Transaction(index, hash) => Some((index, hash)),
            LocationKind::Log {
                transaction_index,
                transaction_hash,
                ..
            } => Some((transaction_index, transaction_hash)),
            LocationKind::TransactionIndex(_)
            | LocationKind::TransactionHash(_)
            | LocationKind::BlockLogIndex(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn transaction_index_coordinate(self) -> Option<u64> {
        match self.kind {
            LocationKind::Transaction(index, _) | LocationKind::TransactionIndex(index) => {
                Some(index)
            }
            LocationKind::Log {
                transaction_index, ..
            } => Some(transaction_index),
            LocationKind::Block
            | LocationKind::TransactionHash(_)
            | LocationKind::BlockLogIndex(_) => None,
        }
    }

    pub(crate) const fn transaction_hash_coordinate(self) -> Option<B256> {
        match self.kind {
            LocationKind::Transaction(_, hash) | LocationKind::TransactionHash(hash) => Some(hash),
            LocationKind::Log {
                transaction_hash, ..
            } => Some(transaction_hash),
            LocationKind::Block
            | LocationKind::TransactionIndex(_)
            | LocationKind::BlockLogIndex(_) => None,
        }
    }

    pub(crate) const fn log_coordinate(self) -> Option<(u64, u64)> {
        match self.kind {
            LocationKind::Log {
                receipt_log_index,
                block_log_index,
                ..
            } => Some((receipt_log_index, block_log_index)),
            LocationKind::Block
            | LocationKind::Transaction(..)
            | LocationKind::TransactionIndex(_)
            | LocationKind::TransactionHash(_)
            | LocationKind::BlockLogIndex(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn block_log_index_coordinate(self) -> Option<u64> {
        match self.kind {
            LocationKind::BlockLogIndex(index) => Some(index),
            LocationKind::Log {
                block_log_index, ..
            } => Some(block_log_index),
            LocationKind::Block
            | LocationKind::Transaction(..)
            | LocationKind::TransactionIndex(_)
            | LocationKind::TransactionHash(_) => None,
        }
    }

    fn is_block_on(self, chain: StoredProtocolChain) -> bool {
        self.chain() == chain && matches!(self.kind, LocationKind::Block)
    }

    fn is_transaction_on(self, chain: StoredProtocolChain) -> bool {
        self.chain() == chain && matches!(self.kind, LocationKind::Transaction(..))
    }

    fn is_transaction_index_on(self, chain: StoredProtocolChain) -> bool {
        self.chain() == chain && matches!(self.kind, LocationKind::TransactionIndex(_))
    }

    fn is_transaction_hash_on(self, chain: StoredProtocolChain) -> bool {
        self.chain() == chain && matches!(self.kind, LocationKind::TransactionHash(_))
    }

    fn is_block_log_index_on(self, chain: StoredProtocolChain) -> bool {
        self.chain() == chain && matches!(self.kind, LocationKind::BlockLogIndex(_))
    }

    fn is_log_on(self, chain: StoredProtocolChain) -> bool {
        self.chain() == chain && matches!(self.kind, LocationKind::Log { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingStatus {
    Canonical,
    Orphaned,
}

/// Fixed-size representation of dynamic or compound evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindingSummary {
    pub(super) length: u64,
    pub(super) hash: B256,
}

impl FindingSummary {
    pub(crate) const fn new(length: u64, hash: B256) -> Self {
        Self { length, hash }
    }

    /// Hash canonical semantic bytes, never `Debug` or `Display` output.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            length: u64::try_from(bytes.len()).expect("slice length must fit u64"),
            hash: keccak256(bytes),
        }
    }

    pub(crate) const fn length(self) -> u64 {
        self.length
    }

    pub(crate) const fn hash(self) -> B256 {
        self.hash
    }
}

/// Closed durable diagnostic sum. The first wire tag is the finding code.
/// Categorized families carry a typed, stable release-one leaf identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingKind {
    InvalidEnvelope(ChainLocation, StoredEnvelopeRule),
    MalformedAuthenticatedData(ChainLocation, StoredDataSource, FindingSummary),
    UnsupportedProtocolEvent(ChainLocation, Address, Option<B256>),
    MalformedProtocolEvent(ChainLocation, Address, B256, FindingSummary),
    PortalCallViolation(ChainLocation, StoredPortalCallError, FindingSummary),
    ZoneContinuity(BlockNumHash, u64, B256),
    TempoContinuity(BlockNumHash, u64, B256),
    PortalObservationIdentityMismatch(Address, Address),
    PortalCreationBlockMismatch(B256, B256),
    PortalCreationMissing(B256),
    ImportedProjectionViolation(ChainLocation, StoredImportedProjectionError, FindingSummary),
    ZoneProjectionViolation(ChainLocation, StoredZoneProjectionError, FindingSummary),
    ModelViolation(
        ChainLocation,
        StoredModelError,
        Option<ModelKey>,
        FindingSummary,
    ),
    ImportedOutputCountMismatch(u64, u64),
    ImportedOutputMismatch(u64, ChainLocation, FindingSummary, FindingSummary),
    TempoBlockFinalizedMismatch(ChainLocation, FindingSummary, FindingSummary),
    TokenEnableCountMismatch(u64, u64),
    TokenEnableMismatch(u64, ChainLocation, FindingSummary, FindingSummary),
    DepositOutcomeCountMismatch(u64, u64),
    DepositOutcomeMismatch(u64, ChainLocation, FindingSummary, FindingSummary),
    TempoAdvancedMismatch(ChainLocation, FindingSummary, FindingSummary),
    ZoneOperationCountMismatch(u64, u64),
    ZoneOperationMismatch(u64, ChainLocation, FindingSummary, FindingSummary),
    BatchFinalizedMismatch(ChainLocation, FindingSummary, FindingSummary),
    TempoBlockHashMismatch(B256, B256),
    TempoBlockNumberMismatch(u64, u64),
    ProcessedDepositHashMismatch(B256, B256),
    ProcessedDepositNumberMismatch(u64, u64),
    WithdrawalQueueHashMismatch(B256, B256),
    WithdrawalBatchIndexMismatch(u64, u64),
    CollateralDeficit(Address, U256, U256),
    MissingSupply(Address),
    SupplyMismatch(Address, U256, U256),
}

impl FindingKind {
    pub(crate) const fn code(&self) -> (u8, Option<u8>) {
        match self {
            Self::InvalidEnvelope(_, leaf) => (0x01, Some(leaf.wire_tag())),
            Self::MalformedAuthenticatedData(_, leaf, _) => (0x02, Some(leaf.wire_tag())),
            Self::UnsupportedProtocolEvent(..) => (0x03, None),
            Self::MalformedProtocolEvent(..) => (0x04, None),
            Self::PortalCallViolation(_, leaf, _) => (0x05, Some(leaf.wire_tag())),
            Self::ZoneContinuity(..) => (0x06, None),
            Self::TempoContinuity(..) => (0x07, None),
            Self::PortalObservationIdentityMismatch(..) => (0x08, None),
            Self::PortalCreationBlockMismatch(..) => (0x09, None),
            Self::PortalCreationMissing(_) => (0x0a, None),
            Self::ImportedProjectionViolation(_, leaf, _) => (0x0b, Some(leaf.wire_tag())),
            Self::ZoneProjectionViolation(_, leaf, _) => (0x0c, Some(leaf.wire_tag())),
            Self::ModelViolation(_, leaf, _, _) => (0x0d, Some(leaf.wire_tag())),
            Self::ImportedOutputCountMismatch(..) => (0x0e, None),
            Self::ImportedOutputMismatch(..) => (0x0f, None),
            Self::TempoBlockFinalizedMismatch(..) => (0x10, None),
            Self::TokenEnableCountMismatch(..) => (0x11, None),
            Self::TokenEnableMismatch(..) => (0x12, None),
            Self::DepositOutcomeCountMismatch(..) => (0x13, None),
            Self::DepositOutcomeMismatch(..) => (0x14, None),
            Self::TempoAdvancedMismatch(..) => (0x15, None),
            Self::ZoneOperationCountMismatch(..) => (0x16, None),
            Self::ZoneOperationMismatch(..) => (0x17, None),
            Self::BatchFinalizedMismatch(..) => (0x18, None),
            Self::TempoBlockHashMismatch(..) => (0x19, None),
            Self::TempoBlockNumberMismatch(..) => (0x1a, None),
            Self::ProcessedDepositHashMismatch(..) => (0x1b, None),
            Self::ProcessedDepositNumberMismatch(..) => (0x1c, None),
            Self::WithdrawalQueueHashMismatch(..) => (0x1d, None),
            Self::WithdrawalBatchIndexMismatch(..) => (0x1e, None),
            Self::CollateralDeficit(..) => (0x1f, None),
            Self::MissingSupply(_) => (0x20, None),
            Self::SupplyMismatch(..) => (0x21, None),
        }
    }

    fn has_valid_location(&self) -> bool {
        match self {
            Self::InvalidEnvelope(location, leaf) => match leaf {
                StoredEnvelopeRule::NonGenesis | StoredEnvelopeRule::AdvancePresent => {
                    location.is_block_on(StoredProtocolChain::ZoneL2)
                }
                StoredEnvelopeRule::AdvanceSystemCaller
                | StoredEnvelopeRule::AdvanceDestination
                | StoredEnvelopeRule::AdvanceSuccess
                | StoredEnvelopeRule::SystemIdentity
                | StoredEnvelopeRule::FinalizationPosition
                | StoredEnvelopeRule::FinalizationDestination
                | StoredEnvelopeRule::FinalizationSuccess
                | StoredEnvelopeRule::FinalizationBlockNumber => {
                    location.is_transaction_on(StoredProtocolChain::ZoneL2)
                }
            },
            Self::MalformedAuthenticatedData(location, source, _) => {
                location.is_transaction_on(source.chain())
            }
            Self::UnsupportedProtocolEvent(location, ..)
            | Self::MalformedProtocolEvent(location, ..) => {
                location.is_log_on(StoredProtocolChain::TempoL1)
                    || location.is_log_on(StoredProtocolChain::ZoneL2)
            }
            Self::PortalCallViolation(location, _, _) => {
                location.is_transaction_hash_on(StoredProtocolChain::TempoL1)
            }
            Self::ImportedProjectionViolation(location, leaf, _) => {
                valid_imported_projection_location(*location, *leaf)
            }
            Self::ZoneProjectionViolation(location, leaf, _) => {
                valid_zone_projection_location(*location, *leaf)
            }
            Self::ModelViolation(location, ..) => location.is_block_on(StoredProtocolChain::ZoneL2),
            Self::ImportedOutputMismatch(_, location, ..) => {
                location.is_transaction_on(StoredProtocolChain::TempoL1)
                    || location.is_log_on(StoredProtocolChain::TempoL1)
            }
            Self::TempoBlockFinalizedMismatch(location, ..)
            | Self::TokenEnableMismatch(_, location, ..)
            | Self::DepositOutcomeMismatch(_, location, ..)
            | Self::TempoAdvancedMismatch(location, ..)
            | Self::ZoneOperationMismatch(_, location, ..) => {
                location.is_log_on(StoredProtocolChain::ZoneL2)
            }
            Self::BatchFinalizedMismatch(location, ..) => {
                location.is_block_on(StoredProtocolChain::ZoneL2)
                    || location.is_log_on(StoredProtocolChain::ZoneL2)
            }
            Self::ZoneContinuity(..)
            | Self::TempoContinuity(..)
            | Self::PortalObservationIdentityMismatch(..)
            | Self::PortalCreationBlockMismatch(..)
            | Self::PortalCreationMissing(_)
            | Self::ImportedOutputCountMismatch(..)
            | Self::TokenEnableCountMismatch(..)
            | Self::DepositOutcomeCountMismatch(..)
            | Self::ZoneOperationCountMismatch(..)
            | Self::TempoBlockHashMismatch(..)
            | Self::TempoBlockNumberMismatch(..)
            | Self::ProcessedDepositHashMismatch(..)
            | Self::ProcessedDepositNumberMismatch(..)
            | Self::WithdrawalQueueHashMismatch(..)
            | Self::WithdrawalBatchIndexMismatch(..)
            | Self::CollateralDeficit(..)
            | Self::MissingSupply(_)
            | Self::SupplyMismatch(..) => true,
        }
    }

    fn requires_imported_tempo(&self) -> bool {
        match self {
            Self::InvalidEnvelope(_, leaf) => !matches!(
                leaf,
                StoredEnvelopeRule::NonGenesis
                    | StoredEnvelopeRule::AdvancePresent
                    | StoredEnvelopeRule::AdvanceSystemCaller
                    | StoredEnvelopeRule::AdvanceDestination
                    | StoredEnvelopeRule::AdvanceSuccess
            ),
            Self::MalformedAuthenticatedData(_, source, _) => !matches!(
                source,
                StoredDataSource::AdvanceTempoCalldata
                    | StoredDataSource::AdvanceHeaderRlp
                    | StoredDataSource::OrdinaryDepositData
                    | StoredDataSource::WithdrawalBounceBackData
            ),
            Self::UnsupportedProtocolEvent(..)
            | Self::MalformedProtocolEvent(..)
            | Self::PortalCallViolation(..)
            | Self::ZoneContinuity(..)
            | Self::TempoContinuity(..)
            | Self::PortalObservationIdentityMismatch(..)
            | Self::PortalCreationBlockMismatch(..)
            | Self::PortalCreationMissing(_)
            | Self::ImportedProjectionViolation(..)
            | Self::ZoneProjectionViolation(..)
            | Self::ModelViolation(..)
            | Self::ImportedOutputCountMismatch(..)
            | Self::ImportedOutputMismatch(..)
            | Self::TempoBlockFinalizedMismatch(..)
            | Self::TokenEnableCountMismatch(..)
            | Self::TokenEnableMismatch(..)
            | Self::DepositOutcomeCountMismatch(..)
            | Self::DepositOutcomeMismatch(..)
            | Self::TempoAdvancedMismatch(..)
            | Self::ZoneOperationCountMismatch(..)
            | Self::ZoneOperationMismatch(..)
            | Self::BatchFinalizedMismatch(..)
            | Self::TempoBlockHashMismatch(..)
            | Self::TempoBlockNumberMismatch(..)
            | Self::ProcessedDepositHashMismatch(..)
            | Self::ProcessedDepositNumberMismatch(..)
            | Self::WithdrawalQueueHashMismatch(..)
            | Self::WithdrawalBatchIndexMismatch(..)
            | Self::CollateralDeficit(..)
            | Self::MissingSupply(_)
            | Self::SupplyMismatch(..) => true,
        }
    }

    pub(super) fn is_valid(&self, imported_tempo: Option<BlockNumHash>) -> bool {
        self.has_valid_location() && (!self.requires_imported_tempo() || imported_tempo.is_some())
    }
}

fn valid_imported_projection_location(
    location: ChainLocation,
    leaf: StoredImportedProjectionError,
) -> bool {
    match leaf {
        StoredImportedProjectionError::MissingBaseFee
        | StoredImportedProjectionError::BlockHashMismatch
        | StoredImportedProjectionError::BlockNumberMismatch => {
            location.is_block_on(StoredProtocolChain::TempoL1)
        }
        StoredImportedProjectionError::OutcomeCoordinateMismatch => {
            location.is_transaction_on(StoredProtocolChain::TempoL1)
        }
        StoredImportedProjectionError::TransactionOrderMismatch
        | StoredImportedProjectionError::InvalidCreationGrammar
        | StoredImportedProjectionError::InvalidSubmitBatchGrammar
        | StoredImportedProjectionError::DirectCallRequired
        | StoredImportedProjectionError::UnexpectedEvent
        | StoredImportedProjectionError::InvalidWithdrawalPreimage
        | StoredImportedProjectionError::MissingWithdrawalOutcome
        | StoredImportedProjectionError::UnexpectedWithdrawalOutcome
        | StoredImportedProjectionError::WithdrawalCallbackSuccessMismatch
        | StoredImportedProjectionError::ExtraWithdrawalOutcomes => {
            location.is_transaction_index_on(StoredProtocolChain::TempoL1)
        }
        StoredImportedProjectionError::InvalidDepositCiphertextLength
        | StoredImportedProjectionError::InvalidDepositKeyParity => {
            location.is_block_log_index_on(StoredProtocolChain::TempoL1)
        }
    }
}

fn valid_zone_projection_location(
    location: ChainLocation,
    leaf: StoredZoneProjectionError,
) -> bool {
    match leaf {
        StoredZoneProjectionError::ReorderedTempoBlockFinalized
        | StoredZoneProjectionError::ReorderedTokenEnabled
        | StoredZoneProjectionError::ReorderedDepositOutcome
        | StoredZoneProjectionError::ReorderedDepositFailed
        | StoredZoneProjectionError::ReorderedTempoAdvanced
        | StoredZoneProjectionError::ExtraAdvanceEvent
        | StoredZoneProjectionError::AdvanceTransactionHashMismatch
        | StoredZoneProjectionError::UnexpectedPostAdvanceEvent
        | StoredZoneProjectionError::BatchFinalizedWithoutEnvelope
        | StoredZoneProjectionError::BatchFinalizedWrongTransaction
        | StoredZoneProjectionError::ReorderedBatchFinalized
        | StoredZoneProjectionError::ExtraFinalizationEvent => {
            location.is_log_on(StoredProtocolChain::ZoneL2)
        }
        StoredZoneProjectionError::InvalidWithdrawalRequest
        | StoredZoneProjectionError::MissingBatchFinalized => {
            location.is_transaction_on(StoredProtocolChain::ZoneL2)
        }
        StoredZoneProjectionError::MissingTempoBlockFinalized
        | StoredZoneProjectionError::MissingTokenEnabled
        | StoredZoneProjectionError::MissingDepositOutcome
        | StoredZoneProjectionError::MissingDepositFailed
        | StoredZoneProjectionError::MissingTempoAdvanced
        | StoredZoneProjectionError::InvalidDepositKeyParity
        | StoredZoneProjectionError::InvalidDepositCiphertextLength
        | StoredZoneProjectionError::InvalidBounceBackRecipient
        | StoredZoneProjectionError::ZeroBounceBackNonce
        | StoredZoneProjectionError::ZeroBounceBackAmount
        | StoredZoneProjectionError::UnsupportedDepositKind => {
            location.is_block_on(StoredProtocolChain::ZoneL2)
        }
    }
}

/// Value stored under one `FindingKey`; the key supplies Zone number and hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingRecord {
    pub(super) zone_parent_hash: B256,
    pub(super) imported_tempo: Option<BlockNumHash>,
    pub(super) status: FindingStatus,
    pub(super) kind: FindingKind,
}

impl FindingRecord {
    pub(crate) fn new(
        zone_parent_hash: B256,
        imported_tempo: Option<BlockNumHash>,
        status: FindingStatus,
        kind: FindingKind,
    ) -> Option<Self> {
        kind.is_valid(imported_tempo).then_some(Self {
            zone_parent_hash,
            imported_tempo,
            status,
            kind,
        })
    }

    pub(crate) const fn zone_parent_hash(&self) -> B256 {
        self.zone_parent_hash
    }

    pub(crate) const fn imported_tempo(&self) -> Option<BlockNumHash> {
        self.imported_tempo
    }

    pub(crate) const fn status(&self) -> FindingStatus {
        self.status
    }

    pub(crate) const fn kind(&self) -> &FindingKind {
        &self.kind
    }

    pub(crate) fn mark_orphaned(&mut self) {
        self.status = FindingStatus::Orphaned;
    }

    pub(crate) fn mark_canonical(&mut self) {
        self.status = FindingStatus::Canonical;
    }
}
