//! Typed failures for the native Zone projection grammar.

use alloy_primitives::{Address, B256};

use crate::model::{
    encoding::WithdrawalDataError,
    events::{Inbox, L2ProtocolEvent, Outbox, TempoState},
};

use super::ObservedZoneEventPosition;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DepositInputKind {
    Ordinary,
    WithdrawalBounceBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZoneEventKind {
    TempoBlockFinalized,
    TokenEnabled,
    DepositProcessed,
    DepositFailed,
    WithdrawalBounceBackProcessed,
    WithdrawalBounceBackPending,
    TempoAdvanced,
    WithdrawalRequested,
    RefundClaimed,
    BatchFinalized,
    TempoGasRateUpdated,
    MaxWithdrawalsPerBlockUpdated,
}

/// Projection failures describe the violated concrete protocol stage.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ZoneProjectionError {
    #[error("advance transaction is missing the leading TempoBlockFinalized event")]
    MissingTempoBlockFinalized,
    #[error("advance transaction expected TempoBlockFinalized, got {actual:?} at {position:?}")]
    ReorderedTempoBlockFinalized {
        actual: ZoneEventKind,
        position: ObservedZoneEventPosition,
    },
    #[error("advance token enable {index} is missing its TokenEnabled event")]
    MissingTokenEnabled { index: usize },
    #[error("advance token enable {index} expected TokenEnabled, got {actual:?} at {position:?}")]
    ReorderedTokenEnabled {
        index: usize,
        actual: ZoneEventKind,
        position: ObservedZoneEventPosition,
    },
    #[error("advance deposit {index} ({deposit_kind:?}) is missing its outcome event")]
    MissingDepositOutcome {
        index: usize,
        deposit_kind: DepositInputKind,
    },
    #[error(
        "advance deposit {index} ({deposit_kind:?}) got invalid outcome {actual:?} at {position:?}"
    )]
    ReorderedDepositOutcome {
        index: usize,
        deposit_kind: DepositInputKind,
        actual: ZoneEventKind,
        position: ObservedZoneEventPosition,
    },
    #[error("failed ordinary deposit {index} is missing its terminal DepositFailed event")]
    MissingDepositFailed { index: usize },
    #[error(
        "failed ordinary deposit {index} expected DepositFailed, got {actual:?} at {position:?}"
    )]
    ReorderedDepositFailed {
        index: usize,
        actual: ZoneEventKind,
        position: ObservedZoneEventPosition,
    },
    #[error("advance transaction is missing the terminal TempoAdvanced event")]
    MissingTempoAdvanced,
    #[error("advance transaction expected TempoAdvanced, got {actual:?} at {position:?}")]
    ReorderedTempoAdvanced {
        actual: ZoneEventKind,
        position: ObservedZoneEventPosition,
    },
    #[error("advance transaction has an extra {actual:?} event at {position:?}")]
    ExtraAdvanceEvent {
        actual: ZoneEventKind,
        position: ObservedZoneEventPosition,
    },
    #[error("transaction-zero event at {position:?} does not have advance hash {expected}")]
    AdvanceTransactionHashMismatch {
        expected: B256,
        position: ObservedZoneEventPosition,
    },
    #[error("ordinary deposit {index} has unsupported compressed-key prefix {actual:#04x}")]
    InvalidDepositKeyParity { index: usize, actual: u8 },
    #[error("ordinary deposit {index} ciphertext length {actual} does not equal {expected} bytes")]
    InvalidDepositCiphertextLength {
        index: usize,
        actual: usize,
        expected: usize,
    },
    #[error("withdrawal bounce-back deposit {index} encodes non-canonical recipient {recipient}")]
    InvalidBounceBackRecipient { index: usize, recipient: Address },
    #[error("withdrawal bounce-back deposit {index} has a zero fallback nonce")]
    ZeroBounceBackNonce { index: usize },
    #[error("withdrawal bounce-back deposit {index} has a zero amount")]
    ZeroBounceBackAmount { index: usize },
    #[error(
        "withdrawal event at transaction {transaction_index} has invalid request data: {source}"
    )]
    InvalidWithdrawalRequest {
        transaction_index: usize,
        #[source]
        source: WithdrawalDataError,
    },
    #[error("unexpected post-advance {actual:?} event at {position:?}")]
    UnexpectedPostAdvanceEvent {
        actual: ZoneEventKind,
        position: ObservedZoneEventPosition,
    },
    #[error("BatchFinalized at {position:?} has no authenticated finalization envelope")]
    BatchFinalizedWithoutEnvelope { position: ObservedZoneEventPosition },
    #[error(
        "BatchFinalized at {position:?} does not belong to finalization transaction {expected}"
    )]
    BatchFinalizedWrongTransaction {
        expected: B256,
        position: ObservedZoneEventPosition,
    },
    #[error("finalization transaction {transaction_hash} is missing BatchFinalized")]
    MissingBatchFinalized { transaction_hash: B256 },
    #[error("finalization transaction expected BatchFinalized, got {actual:?} at {position:?}")]
    ReorderedBatchFinalized {
        actual: ZoneEventKind,
        position: ObservedZoneEventPosition,
    },
    #[error("protocol event {actual:?} follows BatchFinalized at {position:?}")]
    ExtraFinalizationEvent {
        actual: ZoneEventKind,
        position: ObservedZoneEventPosition,
    },
    #[error("authenticated deposit {index} has no supported decoded kind")]
    UnsupportedDepositKind { index: usize },
}

pub(super) fn event_kind(event: &L2ProtocolEvent) -> ZoneEventKind {
    match event {
        L2ProtocolEvent::TempoState(TempoState::TempoStateEvents::TempoBlockFinalized(_)) => {
            ZoneEventKind::TempoBlockFinalized
        }
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::TokenEnabled(_)) => ZoneEventKind::TokenEnabled,
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::DepositProcessed(_)) => {
            ZoneEventKind::DepositProcessed
        }
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::DepositFailed(_)) => {
            ZoneEventKind::DepositFailed
        }
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::WithdrawalBounceBackProcessed(_)) => {
            ZoneEventKind::WithdrawalBounceBackProcessed
        }
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::WithdrawalBounceBackPending(_)) => {
            ZoneEventKind::WithdrawalBounceBackPending
        }
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::TempoAdvanced(_)) => {
            ZoneEventKind::TempoAdvanced
        }
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::WithdrawalRequested(_)) => {
            ZoneEventKind::WithdrawalRequested
        }
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::RefundClaimed(_)) => {
            ZoneEventKind::RefundClaimed
        }
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::BatchFinalized(_)) => {
            ZoneEventKind::BatchFinalized
        }
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::TempoGasRateUpdated(_)) => {
            ZoneEventKind::TempoGasRateUpdated
        }
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::MaxWithdrawalsPerBlockUpdated(_)) => {
            ZoneEventKind::MaxWithdrawalsPerBlockUpdated
        }
    }
}
