//! Typed failures for the imported Tempo projection grammar.

use alloy_primitives::B256;

use crate::model::{
    encoding::WithdrawalDataError,
    events::{L1ProtocolEvent, PortalModelEvent},
};

/// Concrete model-driving L1 event families used in grammar errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportedEventKind {
    DepositMade,
    TokenEnabled,
    BatchSubmitted,
    WithdrawalProcessed,
    WithdrawalBounceBack,
    DepositBounceBack,
    DepositBounceBackPending,
    RefundClaimed,
    BouncebackGasUpdated,
    FactoryZoneCreated,
    KnownNonModel,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ImportedProjectionError {
    #[error("authenticated imported Tempo header has no base fee")]
    MissingBaseFee,
    #[error("L1 observation block hash mismatch: expected {expected}, got {actual}")]
    BlockHashMismatch { expected: B256, actual: B256 },
    #[error("L1 observation block number mismatch: expected {expected}, got {actual}")]
    BlockNumberMismatch { expected: u64, actual: u64 },
    #[error("L1 protocol transactions are not strictly ordered: previous {previous}, next {next}")]
    TransactionOrderMismatch { previous: usize, next: usize },
    #[error(
        "L1 outcome coordinate mismatch in transaction {transaction_index}: event points to transaction {event_transaction_index} hash {event_transaction_hash}, transaction hash is {transaction_hash}"
    )]
    OutcomeCoordinateMismatch {
        transaction_index: usize,
        transaction_hash: B256,
        event_transaction_index: usize,
        event_transaction_hash: B256,
    },
    #[error("invalid Portal creation event grammar in transaction {transaction_index}")]
    InvalidCreationGrammar { transaction_index: usize },
    #[error("invalid direct submitBatch event grammar in transaction {transaction_index}")]
    InvalidSubmitBatchGrammar { transaction_index: usize },
    #[error(
        "event {event:?} in transaction {transaction_index} requires authenticated direct calldata"
    )]
    DirectCallRequired {
        transaction_index: usize,
        event: ImportedEventKind,
    },
    #[error("unexpected event {event:?} in transaction {transaction_index}")]
    UnexpectedEvent {
        transaction_index: usize,
        event: ImportedEventKind,
    },
    #[error("deposit ciphertext length {actual} at block log {block_log_index} is not {expected}")]
    InvalidDepositCiphertextLength {
        block_log_index: usize,
        actual: usize,
        expected: usize,
    },
    #[error(
        "compressed deposit key parity {actual:#04x} at block log {block_log_index} is not 0x02 or 0x03"
    )]
    InvalidDepositKeyParity { block_log_index: usize, actual: u8 },
    #[error(
        "invalid authenticated withdrawal preimage {member_index} in transaction {transaction_index}: {source}"
    )]
    InvalidWithdrawalPreimage {
        transaction_index: usize,
        member_index: usize,
        #[source]
        source: WithdrawalDataError,
    },
    #[error(
        "processWithdrawals transaction {transaction_index} is missing events for member {member_index}"
    )]
    MissingWithdrawalOutcome {
        transaction_index: usize,
        member_index: usize,
    },
    #[error(
        "processWithdrawals transaction {transaction_index} member {member_index} has unexpected event {event:?}"
    )]
    UnexpectedWithdrawalOutcome {
        transaction_index: usize,
        member_index: usize,
        event: ImportedEventKind,
    },
    #[error(
        "processWithdrawals transaction {transaction_index} member {member_index} callback success mismatch: expected {expected}, got {actual}"
    )]
    WithdrawalCallbackSuccessMismatch {
        transaction_index: usize,
        member_index: usize,
        expected: bool,
        actual: bool,
    },
    #[error(
        "processWithdrawals transaction {transaction_index} has {remaining} extra model-driving events"
    )]
    ExtraWithdrawalOutcomes {
        transaction_index: usize,
        remaining: usize,
    },
}

pub(super) fn event_kind(event: &L1ProtocolEvent) -> ImportedEventKind {
    match event {
        L1ProtocolEvent::Portal(PortalModelEvent::DepositMade(_)) => ImportedEventKind::DepositMade,
        L1ProtocolEvent::Portal(PortalModelEvent::TokenEnabled(_)) => {
            ImportedEventKind::TokenEnabled
        }
        L1ProtocolEvent::Portal(PortalModelEvent::BatchSubmitted(_)) => {
            ImportedEventKind::BatchSubmitted
        }
        L1ProtocolEvent::Portal(PortalModelEvent::WithdrawalProcessed(_)) => {
            ImportedEventKind::WithdrawalProcessed
        }
        L1ProtocolEvent::Portal(PortalModelEvent::WithdrawalBounceBack(_)) => {
            ImportedEventKind::WithdrawalBounceBack
        }
        L1ProtocolEvent::Portal(PortalModelEvent::DepositBounceBack(_)) => {
            ImportedEventKind::DepositBounceBack
        }
        L1ProtocolEvent::Portal(PortalModelEvent::DepositBounceBackPending(_)) => {
            ImportedEventKind::DepositBounceBackPending
        }
        L1ProtocolEvent::Portal(PortalModelEvent::RefundClaimed(_)) => {
            ImportedEventKind::RefundClaimed
        }
        L1ProtocolEvent::Portal(PortalModelEvent::BouncebackGasUpdated(_)) => {
            ImportedEventKind::BouncebackGasUpdated
        }
        L1ProtocolEvent::FactoryZoneCreated(_) => ImportedEventKind::FactoryZoneCreated,
        L1ProtocolEvent::KnownNonModel => ImportedEventKind::KnownNonModel,
    }
}
