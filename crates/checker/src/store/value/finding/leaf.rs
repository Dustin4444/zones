//! Stable wire identities for finding subcategories.
//!
//! These enums deliberately mirror runtime diagnostic families without
//! serializing runtime error values. Explicit tags keep database compatibility
//! independent of declaration order in the observation and model layers.

use crate::{
    model::{
        adapter::{ImportedProjectionError, ZoneProjectionError},
        transition::ModelError,
    },
    observe::{DataSource, EnvelopeRule, PortalCallError, ProtocolChain},
};

macro_rules! stored_leaf {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident = $tag:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub(super) const fn wire_tag(self) -> u8 {
                match self {
                    $(Self::$variant => $tag),+
                }
            }

            pub(super) const fn from_wire_tag(tag: u8) -> Option<Self> {
                match tag {
                    $($tag => Some(Self::$variant)),+,
                    _ => None,
                }
            }
        }
    };
}

stored_leaf! {
    /// Protocol chain identity retained in a durable finding location.
    pub enum StoredProtocolChain {
        TempoL1 = 0x01,
        ZoneL2 = 0x02,
    }
}

impl From<ProtocolChain> for StoredProtocolChain {
    fn from(value: ProtocolChain) -> Self {
        match value {
            ProtocolChain::TempoL1 => Self::TempoL1,
            ProtocolChain::ZoneL2 => Self::ZoneL2,
        }
    }
}

impl From<StoredProtocolChain> for ProtocolChain {
    fn from(value: StoredProtocolChain) -> Self {
        match value {
            StoredProtocolChain::TempoL1 => Self::TempoL1,
            StoredProtocolChain::ZoneL2 => Self::ZoneL2,
        }
    }
}

stored_leaf! {
    /// Durable identity of a violated authenticated-envelope rule.
    pub enum StoredEnvelopeRule {
        NonGenesis = 0x01,
        AdvancePresent = 0x02,
        AdvanceSystemCaller = 0x03,
        AdvanceDestination = 0x04,
        AdvanceSuccess = 0x05,
        SystemIdentity = 0x06,
        FinalizationPosition = 0x07,
        FinalizationDestination = 0x08,
        FinalizationSuccess = 0x09,
        FinalizationBlockNumber = 0x0a,
    }
}

impl From<EnvelopeRule> for StoredEnvelopeRule {
    fn from(value: EnvelopeRule) -> Self {
        match value {
            EnvelopeRule::NonGenesis => Self::NonGenesis,
            EnvelopeRule::AdvancePresent => Self::AdvancePresent,
            EnvelopeRule::AdvanceSystemCaller => Self::AdvanceSystemCaller,
            EnvelopeRule::AdvanceDestination => Self::AdvanceDestination,
            EnvelopeRule::AdvanceSuccess => Self::AdvanceSuccess,
            EnvelopeRule::SystemIdentity => Self::SystemIdentity,
            EnvelopeRule::FinalizationPosition => Self::FinalizationPosition,
            EnvelopeRule::FinalizationDestination => Self::FinalizationDestination,
            EnvelopeRule::FinalizationSuccess => Self::FinalizationSuccess,
            EnvelopeRule::FinalizationBlockNumber => Self::FinalizationBlockNumber,
        }
    }
}

impl From<StoredEnvelopeRule> for EnvelopeRule {
    fn from(value: StoredEnvelopeRule) -> Self {
        match value {
            StoredEnvelopeRule::NonGenesis => Self::NonGenesis,
            StoredEnvelopeRule::AdvancePresent => Self::AdvancePresent,
            StoredEnvelopeRule::AdvanceSystemCaller => Self::AdvanceSystemCaller,
            StoredEnvelopeRule::AdvanceDestination => Self::AdvanceDestination,
            StoredEnvelopeRule::AdvanceSuccess => Self::AdvanceSuccess,
            StoredEnvelopeRule::SystemIdentity => Self::SystemIdentity,
            StoredEnvelopeRule::FinalizationPosition => Self::FinalizationPosition,
            StoredEnvelopeRule::FinalizationDestination => Self::FinalizationDestination,
            StoredEnvelopeRule::FinalizationSuccess => Self::FinalizationSuccess,
            StoredEnvelopeRule::FinalizationBlockNumber => Self::FinalizationBlockNumber,
        }
    }
}

stored_leaf! {
    /// Durable identity of an authenticated byte surface.
    pub enum StoredDataSource {
        AdvanceTempoCalldata = 0x01,
        AdvanceHeaderRlp = 0x02,
        OrdinaryDepositData = 0x03,
        WithdrawalBounceBackData = 0x04,
        FinalizationCalldata = 0x05,
        ProcessWithdrawalsCalldata = 0x06,
        SubmitBatchCalldata = 0x07,
        PortalTransactionCalldata = 0x08,
    }
}

impl StoredDataSource {
    pub(super) const fn chain(self) -> StoredProtocolChain {
        match self {
            Self::AdvanceTempoCalldata
            | Self::AdvanceHeaderRlp
            | Self::OrdinaryDepositData
            | Self::WithdrawalBounceBackData
            | Self::FinalizationCalldata => StoredProtocolChain::ZoneL2,
            Self::ProcessWithdrawalsCalldata
            | Self::SubmitBatchCalldata
            | Self::PortalTransactionCalldata => StoredProtocolChain::TempoL1,
        }
    }
}

impl From<DataSource> for StoredDataSource {
    fn from(value: DataSource) -> Self {
        match value {
            DataSource::AdvanceTempoCalldata => Self::AdvanceTempoCalldata,
            DataSource::AdvanceHeaderRlp => Self::AdvanceHeaderRlp,
            DataSource::OrdinaryDepositData => Self::OrdinaryDepositData,
            DataSource::WithdrawalBounceBackData => Self::WithdrawalBounceBackData,
            DataSource::FinalizationCalldata => Self::FinalizationCalldata,
            DataSource::ProcessWithdrawalsCalldata => Self::ProcessWithdrawalsCalldata,
            DataSource::SubmitBatchCalldata => Self::SubmitBatchCalldata,
            DataSource::PortalTransactionCalldata => Self::PortalTransactionCalldata,
        }
    }
}

impl From<StoredDataSource> for DataSource {
    fn from(value: StoredDataSource) -> Self {
        match value {
            StoredDataSource::AdvanceTempoCalldata => Self::AdvanceTempoCalldata,
            StoredDataSource::AdvanceHeaderRlp => Self::AdvanceHeaderRlp,
            StoredDataSource::OrdinaryDepositData => Self::OrdinaryDepositData,
            StoredDataSource::WithdrawalBounceBackData => Self::WithdrawalBounceBackData,
            StoredDataSource::FinalizationCalldata => Self::FinalizationCalldata,
            StoredDataSource::ProcessWithdrawalsCalldata => Self::ProcessWithdrawalsCalldata,
            StoredDataSource::SubmitBatchCalldata => Self::SubmitBatchCalldata,
            StoredDataSource::PortalTransactionCalldata => Self::PortalTransactionCalldata,
        }
    }
}

stored_leaf! {
    /// Durable Portal call-reconciliation failure identity.
    pub enum StoredPortalCallError {
        UnsupportedNestedPortalCall = 0x01,
        ConflictingFamilies = 0x02,
        FamilyMismatch = 0x03,
        EmptyProcessWithOutcomes = 0x04,
    }
}

impl From<&PortalCallError> for StoredPortalCallError {
    fn from(value: &PortalCallError) -> Self {
        match value {
            PortalCallError::UnsupportedNestedPortalCall { .. } => {
                Self::UnsupportedNestedPortalCall
            }
            PortalCallError::ConflictingFamilies { .. } => Self::ConflictingFamilies,
            PortalCallError::FamilyMismatch { .. } => Self::FamilyMismatch,
            PortalCallError::EmptyProcessWithOutcomes { .. } => Self::EmptyProcessWithOutcomes,
        }
    }
}

stored_leaf! {
    /// Durable imported-Tempo projection failure identity.
    pub enum StoredImportedProjectionError {
        MissingBaseFee = 0x01,
        BlockHashMismatch = 0x02,
        BlockNumberMismatch = 0x03,
        TransactionOrderMismatch = 0x04,
        OutcomeCoordinateMismatch = 0x05,
        InvalidCreationGrammar = 0x06,
        InvalidSubmitBatchGrammar = 0x07,
        DirectCallRequired = 0x08,
        UnexpectedEvent = 0x09,
        InvalidDepositCiphertextLength = 0x0a,
        InvalidDepositKeyParity = 0x0b,
        InvalidWithdrawalPreimage = 0x0c,
        MissingWithdrawalOutcome = 0x0d,
        UnexpectedWithdrawalOutcome = 0x0e,
        WithdrawalCallbackSuccessMismatch = 0x0f,
        ExtraWithdrawalOutcomes = 0x10,
    }
}

impl From<&ImportedProjectionError> for StoredImportedProjectionError {
    fn from(value: &ImportedProjectionError) -> Self {
        match value {
            ImportedProjectionError::MissingBaseFee => Self::MissingBaseFee,
            ImportedProjectionError::BlockHashMismatch { .. } => Self::BlockHashMismatch,
            ImportedProjectionError::BlockNumberMismatch { .. } => Self::BlockNumberMismatch,
            ImportedProjectionError::TransactionOrderMismatch { .. } => {
                Self::TransactionOrderMismatch
            }
            ImportedProjectionError::OutcomeCoordinateMismatch { .. } => {
                Self::OutcomeCoordinateMismatch
            }
            ImportedProjectionError::InvalidCreationGrammar { .. } => Self::InvalidCreationGrammar,
            ImportedProjectionError::InvalidSubmitBatchGrammar { .. } => {
                Self::InvalidSubmitBatchGrammar
            }
            ImportedProjectionError::DirectCallRequired { .. } => Self::DirectCallRequired,
            ImportedProjectionError::UnexpectedEvent { .. } => Self::UnexpectedEvent,
            ImportedProjectionError::InvalidDepositCiphertextLength { .. } => {
                Self::InvalidDepositCiphertextLength
            }
            ImportedProjectionError::InvalidDepositKeyParity { .. } => {
                Self::InvalidDepositKeyParity
            }
            ImportedProjectionError::InvalidWithdrawalPreimage { .. } => {
                Self::InvalidWithdrawalPreimage
            }
            ImportedProjectionError::MissingWithdrawalOutcome { .. } => {
                Self::MissingWithdrawalOutcome
            }
            ImportedProjectionError::UnexpectedWithdrawalOutcome { .. } => {
                Self::UnexpectedWithdrawalOutcome
            }
            ImportedProjectionError::WithdrawalCallbackSuccessMismatch { .. } => {
                Self::WithdrawalCallbackSuccessMismatch
            }
            ImportedProjectionError::ExtraWithdrawalOutcomes { .. } => {
                Self::ExtraWithdrawalOutcomes
            }
        }
    }
}

stored_leaf! {
    /// Durable native-Zone projection failure identity.
    pub enum StoredZoneProjectionError {
        MissingTempoBlockFinalized = 0x01,
        ReorderedTempoBlockFinalized = 0x02,
        MissingTokenEnabled = 0x03,
        ReorderedTokenEnabled = 0x04,
        MissingDepositOutcome = 0x05,
        ReorderedDepositOutcome = 0x06,
        MissingDepositFailed = 0x07,
        ReorderedDepositFailed = 0x08,
        MissingTempoAdvanced = 0x09,
        ReorderedTempoAdvanced = 0x0a,
        ExtraAdvanceEvent = 0x0b,
        AdvanceTransactionHashMismatch = 0x0c,
        InvalidDepositKeyParity = 0x0d,
        InvalidDepositCiphertextLength = 0x0e,
        InvalidBounceBackRecipient = 0x0f,
        ZeroBounceBackNonce = 0x10,
        ZeroBounceBackAmount = 0x11,
        InvalidWithdrawalRequest = 0x12,
        UnexpectedPostAdvanceEvent = 0x13,
        BatchFinalizedWithoutEnvelope = 0x14,
        BatchFinalizedWrongTransaction = 0x15,
        MissingBatchFinalized = 0x16,
        ReorderedBatchFinalized = 0x17,
        ExtraFinalizationEvent = 0x18,
        UnsupportedDepositKind = 0x19,
    }
}

impl From<&ZoneProjectionError> for StoredZoneProjectionError {
    fn from(value: &ZoneProjectionError) -> Self {
        match value {
            ZoneProjectionError::MissingTempoBlockFinalized => Self::MissingTempoBlockFinalized,
            ZoneProjectionError::ReorderedTempoBlockFinalized { .. } => {
                Self::ReorderedTempoBlockFinalized
            }
            ZoneProjectionError::MissingTokenEnabled { .. } => Self::MissingTokenEnabled,
            ZoneProjectionError::ReorderedTokenEnabled { .. } => Self::ReorderedTokenEnabled,
            ZoneProjectionError::MissingDepositOutcome { .. } => Self::MissingDepositOutcome,
            ZoneProjectionError::ReorderedDepositOutcome { .. } => Self::ReorderedDepositOutcome,
            ZoneProjectionError::MissingDepositFailed { .. } => Self::MissingDepositFailed,
            ZoneProjectionError::ReorderedDepositFailed { .. } => Self::ReorderedDepositFailed,
            ZoneProjectionError::MissingTempoAdvanced => Self::MissingTempoAdvanced,
            ZoneProjectionError::ReorderedTempoAdvanced { .. } => Self::ReorderedTempoAdvanced,
            ZoneProjectionError::ExtraAdvanceEvent { .. } => Self::ExtraAdvanceEvent,
            ZoneProjectionError::AdvanceTransactionHashMismatch { .. } => {
                Self::AdvanceTransactionHashMismatch
            }
            ZoneProjectionError::InvalidDepositKeyParity { .. } => Self::InvalidDepositKeyParity,
            ZoneProjectionError::InvalidDepositCiphertextLength { .. } => {
                Self::InvalidDepositCiphertextLength
            }
            ZoneProjectionError::InvalidBounceBackRecipient { .. } => {
                Self::InvalidBounceBackRecipient
            }
            ZoneProjectionError::ZeroBounceBackNonce { .. } => Self::ZeroBounceBackNonce,
            ZoneProjectionError::ZeroBounceBackAmount { .. } => Self::ZeroBounceBackAmount,
            ZoneProjectionError::InvalidWithdrawalRequest { .. } => Self::InvalidWithdrawalRequest,
            ZoneProjectionError::UnexpectedPostAdvanceEvent { .. } => {
                Self::UnexpectedPostAdvanceEvent
            }
            ZoneProjectionError::BatchFinalizedWithoutEnvelope { .. } => {
                Self::BatchFinalizedWithoutEnvelope
            }
            ZoneProjectionError::BatchFinalizedWrongTransaction { .. } => {
                Self::BatchFinalizedWrongTransaction
            }
            ZoneProjectionError::MissingBatchFinalized { .. } => Self::MissingBatchFinalized,
            ZoneProjectionError::ReorderedBatchFinalized { .. } => Self::ReorderedBatchFinalized,
            ZoneProjectionError::ExtraFinalizationEvent { .. } => Self::ExtraFinalizationEvent,
            ZoneProjectionError::UnsupportedDepositKind { .. } => Self::UnsupportedDepositKind,
        }
    }
}

stored_leaf! {
    /// Durable logical-model failure identity.
    pub enum StoredModelError {
        PortalNotCreated = 0x01,
        PortalAlreadyCreated = 0x02,
        PortalIdentityMismatch = 0x03,
        PortalAddressMismatch = 0x04,
        InitialTokenMismatch = 0x05,
        TokenAlreadyEnabled = 0x06,
        TokenNotPortalEnabled = 0x07,
        TokenNotZoneEnabled = 0x08,
        ZeroTempoRefundRecipient = 0x09,
        ZoneTokenEnableCountMismatch = 0x0a,
        ZoneTokenEnableMismatch = 0x0b,
        PortalDepositNumberOverflow = 0x0c,
        DepositOwnerCollision = 0x0d,
        FallbackOwnerMissing = 0x0e,
        FallbackOwnerMismatch = 0x0f,
        WithdrawalBounceBackAlreadyPending = 0x10,
        DepositOutcomeCountMismatch = 0x11,
        ProcessedDepositNumberOverflow = 0x12,
        PendingDepositMissing = 0x13,
        DepositPrefixMismatch = 0x14,
        DepositOutcomeKindMismatch = 0x15,
        WithdrawalIndexOverflow = 0x16,
        WithdrawalOwnerCollision = 0x17,
        WithdrawalBlockCapExceeded = 0x18,
        FallbackNonceOverflow = 0x19,
        FallbackOwnerCollision = 0x1a,
        FinalizationBlockNumberMismatch = 0x1b,
        FinalizationCountMismatch = 0x1c,
        FinalizationSenderCountMismatch = 0x1d,
        InvalidBatchWithdrawalRange = 0x1e,
        WithdrawalOwnerMissing = 0x1f,
        WithdrawalAlreadyFinalized = 0x20,
        WithdrawalBatchIndexOverflow = 0x21,
        BatchOwnerCollision = 0x22,
        PortalBatchIndexOverflow = 0x23,
        BatchOwnerMissing = 0x24,
        BatchAlreadySubmitted = 0x25,
        BatchTempoBlockMismatch = 0x26,
        BatchZoneHeightMismatch = 0x27,
        BatchBlockTransitionMismatch = 0x28,
        BatchDepositTransitionMismatch = 0x29,
        BatchWithdrawalQueueHashMismatch = 0x2a,
        PortalBlockContinuityMismatch = 0x2b,
        PortalDepositContinuityMismatch = 0x2c,
        PortalZoneHeightNotIncreasing = 0x2d,
        PortalDepositCursorBeyondQueue = 0x2e,
        InvalidPortalWithdrawalQueueProgress = 0x2f,
        PortalWithdrawalQueueFull = 0x30,
        PortalWithdrawalQueueCounterOverflow = 0x31,
        WithdrawalProcessingOutcomeCountMismatch = 0x32,
        PortalWithdrawalQueueEmpty = 0x33,
        PortalWithdrawalQueueHeadMissing = 0x34,
        PortalWithdrawalQueueHeadNotSubmitted = 0x35,
        PortalWithdrawalQueuePortalMismatch = 0x36,
        PortalWithdrawalQueueHeadMismatch = 0x37,
        WithdrawalProcessingLengthOverflow = 0x38,
        WithdrawalProcessingBeyondBatch = 0x39,
        WithdrawalProcessingExhaustedEarly = 0x3a,
        WithdrawalProcessingLeftSuffixAfterBatch = 0x3b,
        WithdrawalNotFinalizedForProcessing = 0x3c,
        WithdrawalProcessingPreimageMismatch = 0x3d,
        WithdrawalProcessingOutcomeMismatch = 0x3e,
        CallbackDepositsWithoutCallback = 0x3f,
        PortalRefundCollision = 0x40,
        RefundAggregateOverflow = 0x41,
        RetiredPortalRefundAggregateStateMismatch = 0x42,
        RetiredInboxRefundAggregateStateMismatch = 0x43,
        RefundClaimAmountMismatch = 0x44,
        InboxRefundCollision = 0x45,
        ZeroBounceBackRecipient = 0x46,
        Accounting = 0x47,
        Fee = 0x48,
        WithdrawalData = 0x49,
        BatchState = 0x4a,
        PortalQueueId = 0x4b,
        WithdrawalQueue = 0x4c,
    }
}

impl From<&ModelError> for StoredModelError {
    fn from(value: &ModelError) -> Self {
        match value {
            ModelError::PortalNotCreated => Self::PortalNotCreated,
            ModelError::PortalAlreadyCreated => Self::PortalAlreadyCreated,
            ModelError::PortalIdentityMismatch { .. } => Self::PortalIdentityMismatch,
            ModelError::PortalAddressMismatch { .. } => Self::PortalAddressMismatch,
            ModelError::InitialTokenMismatch { .. } => Self::InitialTokenMismatch,
            ModelError::TokenAlreadyEnabled { .. } => Self::TokenAlreadyEnabled,
            ModelError::TokenNotPortalEnabled { .. } => Self::TokenNotPortalEnabled,
            ModelError::TokenNotZoneEnabled { .. } => Self::TokenNotZoneEnabled,
            ModelError::ZeroTempoRefundRecipient => Self::ZeroTempoRefundRecipient,
            ModelError::ZoneTokenEnableCountMismatch { .. } => Self::ZoneTokenEnableCountMismatch,
            ModelError::ZoneTokenEnableMismatch { .. } => Self::ZoneTokenEnableMismatch,
            ModelError::PortalDepositNumberOverflow => Self::PortalDepositNumberOverflow,
            ModelError::DepositOwnerCollision { .. } => Self::DepositOwnerCollision,
            ModelError::FallbackOwnerMissing { .. } => Self::FallbackOwnerMissing,
            ModelError::FallbackOwnerMismatch { .. } => Self::FallbackOwnerMismatch,
            ModelError::WithdrawalBounceBackAlreadyPending { .. } => {
                Self::WithdrawalBounceBackAlreadyPending
            }
            ModelError::DepositOutcomeCountMismatch { .. } => Self::DepositOutcomeCountMismatch,
            ModelError::ProcessedDepositNumberOverflow => Self::ProcessedDepositNumberOverflow,
            ModelError::PendingDepositMissing { .. } => Self::PendingDepositMissing,
            ModelError::DepositPrefixMismatch { .. } => Self::DepositPrefixMismatch,
            ModelError::DepositOutcomeKindMismatch { .. } => Self::DepositOutcomeKindMismatch,
            ModelError::WithdrawalIndexOverflow => Self::WithdrawalIndexOverflow,
            ModelError::WithdrawalOwnerCollision { .. } => Self::WithdrawalOwnerCollision,
            ModelError::WithdrawalBlockCapExceeded { .. } => Self::WithdrawalBlockCapExceeded,
            ModelError::FallbackNonceOverflow => Self::FallbackNonceOverflow,
            ModelError::FallbackOwnerCollision { .. } => Self::FallbackOwnerCollision,
            ModelError::FinalizationBlockNumberMismatch { .. } => {
                Self::FinalizationBlockNumberMismatch
            }
            ModelError::FinalizationCountMismatch { .. } => Self::FinalizationCountMismatch,
            ModelError::FinalizationSenderCountMismatch { .. } => {
                Self::FinalizationSenderCountMismatch
            }
            ModelError::InvalidBatchWithdrawalRange { .. } => Self::InvalidBatchWithdrawalRange,
            ModelError::WithdrawalOwnerMissing { .. } => Self::WithdrawalOwnerMissing,
            ModelError::WithdrawalAlreadyFinalized { .. } => Self::WithdrawalAlreadyFinalized,
            ModelError::WithdrawalBatchIndexOverflow => Self::WithdrawalBatchIndexOverflow,
            ModelError::BatchOwnerCollision { .. } => Self::BatchOwnerCollision,
            ModelError::PortalBatchIndexOverflow => Self::PortalBatchIndexOverflow,
            ModelError::BatchOwnerMissing { .. } => Self::BatchOwnerMissing,
            ModelError::BatchAlreadySubmitted { .. } => Self::BatchAlreadySubmitted,
            ModelError::BatchTempoBlockMismatch { .. } => Self::BatchTempoBlockMismatch,
            ModelError::BatchZoneHeightMismatch { .. } => Self::BatchZoneHeightMismatch,
            ModelError::BatchBlockTransitionMismatch { .. } => Self::BatchBlockTransitionMismatch,
            ModelError::BatchDepositTransitionMismatch { .. } => {
                Self::BatchDepositTransitionMismatch
            }
            ModelError::BatchWithdrawalQueueHashMismatch { .. } => {
                Self::BatchWithdrawalQueueHashMismatch
            }
            ModelError::PortalBlockContinuityMismatch { .. } => Self::PortalBlockContinuityMismatch,
            ModelError::PortalDepositContinuityMismatch { .. } => {
                Self::PortalDepositContinuityMismatch
            }
            ModelError::PortalZoneHeightNotIncreasing { .. } => Self::PortalZoneHeightNotIncreasing,
            ModelError::PortalDepositCursorBeyondQueue { .. } => {
                Self::PortalDepositCursorBeyondQueue
            }
            ModelError::InvalidPortalWithdrawalQueueProgress { .. } => {
                Self::InvalidPortalWithdrawalQueueProgress
            }
            ModelError::PortalWithdrawalQueueFull => Self::PortalWithdrawalQueueFull,
            ModelError::PortalWithdrawalQueueCounterOverflow => {
                Self::PortalWithdrawalQueueCounterOverflow
            }
            ModelError::WithdrawalProcessingOutcomeCountMismatch { .. } => {
                Self::WithdrawalProcessingOutcomeCountMismatch
            }
            ModelError::PortalWithdrawalQueueEmpty => Self::PortalWithdrawalQueueEmpty,
            ModelError::PortalWithdrawalQueueHeadMissing => Self::PortalWithdrawalQueueHeadMissing,
            ModelError::PortalWithdrawalQueueHeadNotSubmitted => {
                Self::PortalWithdrawalQueueHeadNotSubmitted
            }
            ModelError::PortalWithdrawalQueuePortalMismatch { .. } => {
                Self::PortalWithdrawalQueuePortalMismatch
            }
            ModelError::PortalWithdrawalQueueHeadMismatch { .. } => {
                Self::PortalWithdrawalQueueHeadMismatch
            }
            ModelError::WithdrawalProcessingLengthOverflow { .. } => {
                Self::WithdrawalProcessingLengthOverflow
            }
            ModelError::WithdrawalProcessingBeyondBatch { .. } => {
                Self::WithdrawalProcessingBeyondBatch
            }
            ModelError::WithdrawalProcessingExhaustedEarly => {
                Self::WithdrawalProcessingExhaustedEarly
            }
            ModelError::WithdrawalProcessingLeftSuffixAfterBatch => {
                Self::WithdrawalProcessingLeftSuffixAfterBatch
            }
            ModelError::WithdrawalNotFinalizedForProcessing { .. } => {
                Self::WithdrawalNotFinalizedForProcessing
            }
            ModelError::WithdrawalProcessingPreimageMismatch { .. } => {
                Self::WithdrawalProcessingPreimageMismatch
            }
            ModelError::WithdrawalProcessingOutcomeMismatch { .. } => {
                Self::WithdrawalProcessingOutcomeMismatch
            }
            ModelError::CallbackDepositsWithoutCallback { .. } => {
                Self::CallbackDepositsWithoutCallback
            }
            ModelError::PortalRefundCollision { .. } => Self::PortalRefundCollision,
            ModelError::RefundAggregateOverflow { .. } => Self::RefundAggregateOverflow,
            ModelError::RefundClaimAmountMismatch { .. } => Self::RefundClaimAmountMismatch,
            ModelError::InboxRefundCollision { .. } => Self::InboxRefundCollision,
            ModelError::ZeroBounceBackRecipient { .. } => Self::ZeroBounceBackRecipient,
            ModelError::Accounting(_) => Self::Accounting,
            ModelError::Fee(_) => Self::Fee,
            ModelError::WithdrawalData(_) => Self::WithdrawalData,
            ModelError::BatchState(_) => Self::BatchState,
            ModelError::PortalQueueId(_) => Self::PortalQueueId,
            ModelError::WithdrawalQueue(_) => Self::WithdrawalQueue,
        }
    }
}
