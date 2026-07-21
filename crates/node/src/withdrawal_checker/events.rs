use alloy_primitives::{Address, B256, Log, U256};
use alloy_sol_types::SolEvent as _;
use tempo_zone_contracts::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS, ZoneInbox, ZoneOutbox};

use super::WithdrawalCheckError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ZoneBalanceChange {
    Credit {
        tx_hash: B256,
        user: Address,
        token: Address,
        amount: U256,
    },
    Debit {
        tx_hash: B256,
        user: Address,
        token: Address,
        requested: U256,
    },
}

impl ZoneBalanceChange {
    pub(super) const fn tx_hash(self) -> B256 {
        match self {
            Self::Credit { tx_hash, .. } | Self::Debit { tx_hash, .. } => tx_hash,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CanonicalBlock {
    pub(super) number: u64,
    pub(super) hash: B256,
    pub(super) parent_hash: B256,
    pub(super) events: Vec<ZoneBalanceChange>,
}

pub(super) fn parse_events(
    zone: u32,
    block: u64,
    transactions: &[(B256, Vec<Log>)],
) -> Result<Vec<ZoneBalanceChange>, WithdrawalCheckError> {
    let mut events = Vec::new();
    for (tx_hash, logs) in transactions {
        for log in logs {
            let Some(signature) = log.topics().first().copied() else {
                continue;
            };
            let malformed = |event: &'static str, err: alloy_sol_types::Error| {
                WithdrawalCheckError::MalformedEvent {
                    zone,
                    block,
                    tx_hash: *tx_hash,
                    event,
                    detail: err.to_string(),
                }
            };

            let event = if log.address == ZONE_INBOX_ADDRESS
                && signature == ZoneInbox::DepositProcessed::SIGNATURE_HASH
            {
                let event = ZoneInbox::DepositProcessed::decode_log_validate(log)
                    .map_err(|err| malformed("DepositProcessed", err))?;
                Some(ZoneBalanceChange::Credit {
                    tx_hash: *tx_hash,
                    user: event.to,
                    token: event.token,
                    amount: U256::from(event.amount),
                })
            } else if log.address == ZONE_INBOX_ADDRESS
                && signature == ZoneInbox::EncryptedDepositProcessed::SIGNATURE_HASH
            {
                let event = ZoneInbox::EncryptedDepositProcessed::decode_log_validate(log)
                    .map_err(|err| malformed("EncryptedDepositProcessed", err))?;
                Some(ZoneBalanceChange::Credit {
                    tx_hash: *tx_hash,
                    user: event.to,
                    token: event.token,
                    amount: U256::from(event.amount),
                })
            } else if log.address == ZONE_INBOX_ADDRESS
                && signature == ZoneInbox::WithdrawalBounceBackProcessed::SIGNATURE_HASH
            {
                let event = ZoneInbox::WithdrawalBounceBackProcessed::decode_log_validate(log)
                    .map_err(|err| malformed("WithdrawalBounceBackProcessed", err))?;
                Some(ZoneBalanceChange::Credit {
                    tx_hash: *tx_hash,
                    user: event.fallbackRecipient,
                    token: event.token,
                    amount: U256::from(event.amount),
                })
            } else if log.address == ZONE_INBOX_ADDRESS
                && signature == ZoneInbox::RefundClaimed::SIGNATURE_HASH
            {
                let event = ZoneInbox::RefundClaimed::decode_log_validate(log)
                    .map_err(|err| malformed("RefundClaimed", err))?;
                Some(ZoneBalanceChange::Credit {
                    tx_hash: *tx_hash,
                    user: event.recipient,
                    token: event.token,
                    amount: U256::from(event.amount),
                })
            } else if log.address == ZONE_OUTBOX_ADDRESS
                && signature == ZoneOutbox::WithdrawalRequested::SIGNATURE_HASH
            {
                let event = ZoneOutbox::WithdrawalRequested::decode_log_validate(log)
                    .map_err(|err| malformed("WithdrawalRequested", err))?;
                if event.sender.is_zero() && event.fee == 0 && event.fallbackNonce == 0 {
                    None
                } else {
                    let requested = U256::from(event.amount)
                        .checked_add(U256::from(event.fee))
                        .ok_or_else(|| {
                            WithdrawalCheckError::InvalidState(format!(
                                "withdrawal amount plus fee overflow in block {block}, tx {tx_hash}"
                            ))
                        })?;
                    Some(ZoneBalanceChange::Debit {
                        tx_hash: *tx_hash,
                        user: event.sender,
                        token: event.token,
                        requested,
                    })
                }
            } else {
                None
            };

            events.extend(event);
        }
    }
    Ok(events)
}
