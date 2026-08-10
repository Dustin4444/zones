//! Ordered classification of authenticated receipt logs.

use alloy_network::ReceiptResponse as _;
use alloy_primitives::{Address, B256};
use tempo_alloy::rpc::TempoTransactionReceipt;

use crate::observe::events::{L1ProtocolEvent, PortalEvent, classify_l1_protocol_event};

use super::OrderedL1Outcome;
use crate::observe::error::{ObservationError, PortalCallError, PortalCallFamily, ProtocolChain};

pub(super) struct PendingTransaction {
    pub(super) transaction_index: usize,
    pub(super) transaction_hash: B256,
    pub(super) required_call: Option<PortalCallFamily>,
    pub(super) outcomes: Vec<OrderedL1Outcome>,
}

pub(super) fn ordered_transactions(
    portal: Address,
    transaction_hashes: &[B256],
    receipts: &[TempoTransactionReceipt],
) -> Result<Vec<PendingTransaction>, ObservationError> {
    debug_assert_eq!(transaction_hashes.len(), receipts.len());

    let mut transactions = Vec::new();
    let mut block_log_index = 0usize;
    for (transaction_index, (transaction_hash, receipt)) in
        transaction_hashes.iter().zip(receipts).enumerate()
    {
        if !receipt.status() {
            continue;
        }

        let mut required_call = None;
        let mut outcomes = Vec::new();
        for (receipt_log_index, log) in receipt.logs().iter().enumerate() {
            let log_index = block_log_index;
            block_log_index += 1;

            let Some(event) = classify_l1_protocol_event(portal, &log.inner).map_err(|error| {
                ObservationError::protocol_event(
                    ProtocolChain::TempoL1,
                    transaction_index,
                    receipt_log_index,
                    log_index,
                    *transaction_hash,
                    error,
                )
            })?
            else {
                continue;
            };
            if matches!(event, L1ProtocolEvent::KnownIgnored) {
                continue;
            }

            merge_requirement(
                &mut required_call,
                call_requirement(&event),
                *transaction_hash,
            )?;
            outcomes.push(OrderedL1Outcome { event });
        }

        if !outcomes.is_empty() {
            transactions.push(PendingTransaction {
                transaction_index,
                transaction_hash: *transaction_hash,
                required_call,
                outcomes,
            });
        }
    }
    Ok(transactions)
}

fn call_requirement(event: &L1ProtocolEvent) -> Option<PortalCallFamily> {
    match event {
        L1ProtocolEvent::Portal(PortalEvent::BatchSubmitted(_)) => {
            Some(PortalCallFamily::SubmitBatch)
        }
        L1ProtocolEvent::Portal(
            PortalEvent::WithdrawalProcessed(_)
            | PortalEvent::WithdrawalBounceBack(_)
            | PortalEvent::DepositBounceBack(_)
            | PortalEvent::DepositBounceBackPending(_),
        ) => Some(PortalCallFamily::ProcessWithdrawals),
        L1ProtocolEvent::Portal(
            PortalEvent::DepositMade(_)
            | PortalEvent::TokenEnabled(_)
            | PortalEvent::RefundClaimed(_)
            | PortalEvent::BouncebackGasUpdated(_),
        )
        | L1ProtocolEvent::FactoryZoneCreated(_)
        | L1ProtocolEvent::KnownIgnored => None,
    }
}

fn merge_requirement(
    current: &mut Option<PortalCallFamily>,
    next: Option<PortalCallFamily>,
    transaction_hash: B256,
) -> Result<(), ObservationError> {
    let Some(next) = next else {
        return Ok(());
    };
    match *current {
        None => {
            *current = Some(next);
            Ok(())
        }
        Some(required) if required == next => Ok(()),
        Some(_) => Err(PortalCallError::ConflictingFamilies { transaction_hash }.into()),
    }
}
