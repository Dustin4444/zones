//! Typed L2 bridge facts extracted from canonical Zone Inbox/Outbox receipt logs.
//!
//! Facts are temporary: [`extract_l2_facts`] constructs an [`L2BlockFacts`],
//! the caller uses it to produce a log summary, then discards it. No
//! persistence exists yet.

#![allow(dead_code)]

use alloy_consensus::TxReceipt;
use alloy_primitives::{Address, B256, Log};
use alloy_sol_types::SolEvent;
use eyre::WrapErr as _;
use tempo_zone_contracts::{IZoneInbox, IZoneOutbox, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS};
use tracing::info;

// ---------------------------------------------------------------------------
// Fact model
// ---------------------------------------------------------------------------

/// The Tempo/L1 block anchor for a Zone L2 block, from `ZoneInbox.TempoAdvanced`.
///
/// Every non-genesis Zone L2 block must contain exactly one `TempoAdvanced`
/// event linking it to the imported Tempo/L1 block it was produced from.
#[derive(Debug)]
pub(super) struct L1Anchor {
    tempo_block_hash: B256,
    tempo_block_number: u64,
    deposits_processed: u64,
    processed_deposit_queue_hash: B256,
    last_processed_deposit_number: u64,
}

/// A deposit outcome from the Zone Inbox — either processed or failed.
#[derive(Debug)]
pub(super) struct DepositFact {
    deposit_hash: B256,
    token: Address,
    amount: u128,
    /// `true` for `DepositProcessed`, `false` for `DepositFailed`.
    processed: bool,
}

/// A withdrawal bounce-back, kept distinct from ordinary deposits.
///
/// Bounce-backs recycle existing Portal backing rather than introducing new
/// external backing, so they must not be collapsed into [`DepositFact`] for
/// later solvency accounting.
#[derive(Debug)]
pub(super) struct BounceBackFact {
    token: Address,
    amount: u128,
    /// `true` for `WithdrawalBounceBackProcessed`, `false` for `WithdrawalBounceBackPending`.
    processed: bool,
}

/// A Zone refund claim (`ZoneInbox.RefundClaimed`).
#[derive(Debug)]
pub(super) struct RefundClaimFact {
    token: Address,
    amount: u128,
}

/// A withdrawal request from the Zone Outbox, with principal and fee preserved
/// separately for later accounting.
#[derive(Debug)]
pub(super) struct WithdrawalRequestFact {
    withdrawal_index: u64,
    sender: Address,
    token: Address,
    principal: u128,
    fee: u128,
    fallback_nonce: u64,
    /// Inbox-generated failed-deposit refunds have the canonical zero sender.
    is_deposit_bounce_back: bool,
}

/// A finalized withdrawal batch boundary from the Zone Outbox.
#[derive(Debug)]
pub(super) struct BatchBoundaryFact {
    withdrawal_queue_hash: B256,
    withdrawal_batch_index: u64,
}

/// Per-block L2 bridge facts extracted from canonical Zone Inbox/Outbox logs.
#[derive(Debug)]
pub(super) struct L2BlockFacts {
    l2_block_number: u64,
    l2_block_hash: B256,
    anchor: L1Anchor,
    deposits: Vec<DepositFact>,
    bounce_backs: Vec<BounceBackFact>,
    refund_claims: Vec<RefundClaimFact>,
    enabled_tokens: Vec<Address>,
    withdrawal_requests: Vec<WithdrawalRequestFact>,
    /// At most one `BatchFinalized` per block; duplicates are a malformed-block error.
    batch_finalized: Option<BatchBoundaryFact>,
}

/// Mutable extraction state while receipt logs for one block are decoded.
#[derive(Default)]
struct L2BlockFactsBuilder {
    anchor: Option<L1Anchor>,
    deposits: Vec<DepositFact>,
    bounce_backs: Vec<BounceBackFact>,
    refund_claims: Vec<RefundClaimFact>,
    enabled_tokens: Vec<Address>,
    withdrawal_requests: Vec<WithdrawalRequestFact>,
    batch_finalized: Option<BatchBoundaryFact>,
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Strictly decode a known event and reject non-canonical encodings that a
/// permissive ABI decoder could otherwise normalize.
fn decode_event<E: SolEvent>(log: &Log, name: &str, block: u64) -> eyre::Result<E> {
    let event = E::decode_log_validate(log)
        .wrap_err_with(|| format!("malformed {name} in block {block}"))?;
    eyre::ensure!(
        event.data.encode_log_data() == log.data,
        "non-canonical {name} encoding in block {block}"
    );
    Ok(event.data)
}

impl L2BlockFactsBuilder {
    /// Decode a known Zone Inbox event, ignoring unrelated Inbox logs.
    fn extract_inbox(&mut self, log: &Log, block: u64) -> eyre::Result<()> {
        let Some(topic) = log.topics().first() else {
            return Ok(());
        };

        match *topic {
            IZoneInbox::TempoAdvanced::SIGNATURE_HASH => {
                let event = decode_event::<IZoneInbox::TempoAdvanced>(log, "TempoAdvanced", block)?;
                let anchor = L1Anchor {
                    tempo_block_hash: event.tempoBlockHash,
                    tempo_block_number: event.tempoBlockNumber,
                    deposits_processed: u64::try_from(event.depositsProcessed)
                        .wrap_err("depositsProcessed overflows u64")?,
                    processed_deposit_queue_hash: event.newProcessedDepositQueueHash,
                    last_processed_deposit_number: event.lastProcessedDepositNumber,
                };
                if self.anchor.replace(anchor).is_some() {
                    eyre::bail!("duplicate TempoAdvanced in block {block}");
                }
            }
            IZoneInbox::DepositProcessed::SIGNATURE_HASH => {
                let event =
                    decode_event::<IZoneInbox::DepositProcessed>(log, "DepositProcessed", block)?;
                self.deposits.push(DepositFact {
                    deposit_hash: event.depositHash,
                    token: event.token,
                    amount: event.amount,
                    processed: true,
                });
            }
            IZoneInbox::DepositFailed::SIGNATURE_HASH => {
                let event = decode_event::<IZoneInbox::DepositFailed>(log, "DepositFailed", block)?;
                self.deposits.push(DepositFact {
                    deposit_hash: event.depositHash,
                    token: event.token,
                    amount: event.amount,
                    processed: false,
                });
            }
            IZoneInbox::WithdrawalBounceBackProcessed::SIGNATURE_HASH => {
                let event = decode_event::<IZoneInbox::WithdrawalBounceBackProcessed>(
                    log,
                    "WithdrawalBounceBackProcessed",
                    block,
                )?;
                self.bounce_backs.push(BounceBackFact {
                    token: event.token,
                    amount: event.amount,
                    processed: true,
                });
            }
            IZoneInbox::WithdrawalBounceBackPending::SIGNATURE_HASH => {
                let event = decode_event::<IZoneInbox::WithdrawalBounceBackPending>(
                    log,
                    "WithdrawalBounceBackPending",
                    block,
                )?;
                self.bounce_backs.push(BounceBackFact {
                    token: event.token,
                    amount: event.amount,
                    processed: false,
                });
            }
            IZoneInbox::RefundClaimed::SIGNATURE_HASH => {
                let event = decode_event::<IZoneInbox::RefundClaimed>(log, "RefundClaimed", block)?;
                self.refund_claims.push(RefundClaimFact {
                    token: event.token,
                    amount: event.amount,
                });
            }
            IZoneInbox::TokenEnabled::SIGNATURE_HASH => {
                let event = decode_event::<IZoneInbox::TokenEnabled>(log, "TokenEnabled", block)?;
                self.enabled_tokens.push(event.token);
            }
            _ => {}
        }
        Ok(())
    }

    /// Decode a known Zone Outbox event, ignoring unrelated Outbox logs.
    fn extract_outbox(&mut self, log: &Log, block: u64) -> eyre::Result<()> {
        let Some(topic) = log.topics().first() else {
            return Ok(());
        };

        match *topic {
            IZoneOutbox::WithdrawalRequested::SIGNATURE_HASH => {
                let event = decode_event::<IZoneOutbox::WithdrawalRequested>(
                    log,
                    "WithdrawalRequested",
                    block,
                )?;
                self.withdrawal_requests.push(WithdrawalRequestFact {
                    withdrawal_index: event.withdrawalIndex,
                    sender: event.sender,
                    token: event.token,
                    principal: event.amount,
                    fee: event.fee,
                    fallback_nonce: event.fallbackNonce,
                    is_deposit_bounce_back: event.sender.is_zero(),
                });
            }
            IZoneOutbox::BatchFinalized::SIGNATURE_HASH => {
                let event =
                    decode_event::<IZoneOutbox::BatchFinalized>(log, "BatchFinalized", block)?;
                let boundary = BatchBoundaryFact {
                    withdrawal_queue_hash: event.withdrawalQueueHash,
                    withdrawal_batch_index: event.withdrawalBatchIndex,
                };
                if self.batch_finalized.replace(boundary).is_some() {
                    eyre::bail!("duplicate BatchFinalized in block {block}");
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Finalize the block facts after all successful receipts have been scanned.
    fn finish(self, l2_block_number: u64, l2_block_hash: B256) -> eyre::Result<L2BlockFacts> {
        let anchor = self
            .anchor
            .ok_or_else(|| eyre::eyre!("block {l2_block_number} is missing TempoAdvanced"))?;
        Ok(L2BlockFacts {
            l2_block_number,
            l2_block_hash,
            anchor,
            deposits: self.deposits,
            bounce_backs: self.bounce_backs,
            refund_claims: self.refund_claims,
            enabled_tokens: self.enabled_tokens,
            withdrawal_requests: self.withdrawal_requests,
            batch_finalized: self.batch_finalized,
        })
    }
}

/// Decode canonical Zone Inbox/Outbox events from receipt logs and construct
/// typed per-block L2 facts.
///
/// Only logs emitted by `ZONE_INBOX_ADDRESS` or `ZONE_OUTBOX_ADDRESS` are
/// considered. A known event topic from the correct address that fails ABI
/// decoding returns a contextual error. Unrelated logs and matching topics from
/// the wrong address are silently ignored.
///
/// Every non-genesis block must contain exactly one `TempoAdvanced` anchor and
/// at most one `BatchFinalized`. Missing, duplicate, or malformed required
/// events are errors.
pub(super) fn extract_l2_facts<R>(
    l2_block_number: u64,
    l2_block_hash: B256,
    receipts: &[R],
) -> eyre::Result<L2BlockFacts>
where
    R: TxReceipt<Log = Log>,
{
    let mut facts = L2BlockFactsBuilder::default();

    for receipt in receipts {
        // Failed transactions cannot contribute canonical bridge facts. Their EVM logs are
        // normally empty, but checking status makes that invariant explicit at this boundary.
        if !receipt.status() {
            continue;
        }
        for log in receipt.logs() {
            match log.address {
                ZONE_INBOX_ADDRESS => facts.extract_inbox(log, l2_block_number)?,
                ZONE_OUTBOX_ADDRESS => facts.extract_outbox(log, l2_block_number)?,
                _ => {}
            }
        }
    }

    facts.finish(l2_block_number, l2_block_hash)
}

/// Emit one concise structured info log per successfully extracted L2 block.
pub(super) fn log_l2_facts(facts: &L2BlockFacts) {
    info!(
        target: "zone::checker",
        l2_block_number = facts.l2_block_number,
        l2_block_hash = %facts.l2_block_hash,
        l1_block_number = facts.anchor.tempo_block_number,
        l1_block_hash = %facts.anchor.tempo_block_hash,
        deposits_processed = facts.deposits.iter().filter(|d| d.processed).count(),
        deposits_failed = facts.deposits.iter().filter(|d| !d.processed).count(),
        bounce_backs_processed = facts.bounce_backs.iter().filter(|b| b.processed).count(),
        bounce_backs_pending = facts.bounce_backs.iter().filter(|b| !b.processed).count(),
        withdrawal_requests = facts.withdrawal_requests.len(),
        enabled_tokens = facts.enabled_tokens.len(),
        refund_claims = facts.refund_claims.len(),
        batch_finalized = facts.batch_finalized.is_some(),
        "L2 bridge facts extracted",
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, address};
    use reth_ethereum_primitives::Receipt;

    /// Build a receipt containing the given logs.
    fn receipt_with_logs(logs: Vec<Log>) -> Receipt {
        Receipt {
            tx_type: Default::default(),
            success: true,
            cumulative_gas_used: 0,
            logs,
        }
    }

    /// Build a valid `TempoAdvanced` log at the canonical Zone Inbox address.
    fn tempo_advanced_log(
        tempo_block_hash: B256,
        tempo_block_number: u64,
        deposits_processed: u64,
        queue_hash: B256,
        last_deposit_number: u64,
    ) -> Log {
        let event = IZoneInbox::TempoAdvanced {
            tempoBlockHash: tempo_block_hash,
            tempoBlockNumber: tempo_block_number,
            depositsProcessed: U256::from(deposits_processed),
            newProcessedDepositQueueHash: queue_hash,
            lastProcessedDepositNumber: last_deposit_number,
        };
        Log {
            address: ZONE_INBOX_ADDRESS,
            data: event.encode_log_data(),
        }
    }

    #[test]
    fn extract_all_fact_variants_and_ignore_noise() {
        let token_a = address!("0x000000000000000000000000000000000000a111");
        let token_b = address!("0x000000000000000000000000000000000000b222");

        let anchor =
            tempo_advanced_log(B256::repeat_byte(0x10), 100, 2, B256::repeat_byte(0x20), 5);
        let deposit_ok = Log {
            address: ZONE_INBOX_ADDRESS,
            data: IZoneInbox::DepositProcessed {
                depositHash: B256::repeat_byte(0xd0),
                sender: Address::ZERO,
                to: Address::ZERO,
                token: token_a,
                amount: 500,
                memo: B256::ZERO,
            }
            .encode_log_data(),
        };
        let deposit_fail = Log {
            address: ZONE_INBOX_ADDRESS,
            data: IZoneInbox::DepositFailed {
                depositHash: B256::repeat_byte(0xd1),
                sender: Address::ZERO,
                token: token_b,
                amount: 300,
            }
            .encode_log_data(),
        };
        // Bounce-back processed — must not collapse into deposits.
        let bounce_ok = Log {
            address: ZONE_INBOX_ADDRESS,
            data: IZoneInbox::WithdrawalBounceBackProcessed {
                zoneFallbackRecipient: Address::ZERO,
                token: token_a,
                amount: 777,
            }
            .encode_log_data(),
        };
        let bounce_pending = Log {
            address: ZONE_INBOX_ADDRESS,
            data: IZoneInbox::WithdrawalBounceBackPending {
                zoneFallbackRecipient: Address::ZERO,
                token: token_b,
                amount: 888,
            }
            .encode_log_data(),
        };
        let refund = Log {
            address: ZONE_INBOX_ADDRESS,
            data: IZoneInbox::RefundClaimed {
                recipient: Address::ZERO,
                token: token_a,
                amount: 42,
            }
            .encode_log_data(),
        };
        let token_enabled = Log {
            address: ZONE_INBOX_ADDRESS,
            data: IZoneInbox::TokenEnabled {
                token: token_b,
                name: "T".into(),
                symbol: "T".into(),
                currency: "USD".into(),
            }
            .encode_log_data(),
        };
        // Withdrawal request — principal and fee must stay separate.
        let withdrawal = Log {
            address: ZONE_OUTBOX_ADDRESS,
            data: IZoneOutbox::WithdrawalRequested {
                withdrawalIndex: 3,
                sender: Address::ZERO,
                token: token_a,
                to: Address::ZERO,
                amount: 1000,
                fee: 50,
                memo: B256::ZERO,
                gasLimit: 0,
                fallbackNonce: 0,
                data: Default::default(),
                revealTo: Default::default(),
            }
            .encode_log_data(),
        };
        let user_withdrawal = Log {
            address: ZONE_OUTBOX_ADDRESS,
            data: IZoneOutbox::WithdrawalRequested {
                withdrawalIndex: 4,
                sender: Address::repeat_byte(0x44),
                token: token_a,
                to: Address::ZERO,
                amount: 2000,
                fee: 75,
                memo: B256::ZERO,
                gasLimit: 0,
                fallbackNonce: 9,
                data: Default::default(),
                revealTo: Default::default(),
            }
            .encode_log_data(),
        };
        let batch = Log {
            address: ZONE_OUTBOX_ADDRESS,
            data: IZoneOutbox::BatchFinalized {
                withdrawalQueueHash: B256::repeat_byte(0xbb),
                withdrawalBatchIndex: 7,
            }
            .encode_log_data(),
        };
        // Unrelated log — different address, irrelevant topic.
        let noise = Log {
            address: Address::repeat_byte(0x99),
            data: anchor.data.clone(),
        };
        // Matching TempoAdvanced topic but wrong address — must be ignored.
        let wrong_addr = Log {
            address: Address::repeat_byte(0x88),
            data: anchor.data.clone(),
        };

        let receipts = vec![receipt_with_logs(vec![
            anchor,
            deposit_ok,
            deposit_fail,
            bounce_ok,
            bounce_pending,
            refund,
            token_enabled,
            withdrawal,
            user_withdrawal,
            batch,
            noise,
            wrong_addr,
        ])];

        let facts = extract_l2_facts(1, B256::repeat_byte(0x01), &receipts).unwrap();

        // Anchor.
        assert_eq!(facts.anchor.tempo_block_number, 100);
        assert_eq!(facts.anchor.tempo_block_hash, B256::repeat_byte(0x10));
        assert_eq!(facts.anchor.deposits_processed, 2);
        assert_eq!(facts.anchor.last_processed_deposit_number, 5);

        // Deposits: one processed, one failed.
        assert_eq!(facts.deposits.len(), 2);
        assert!(facts.deposits[0].processed);
        assert_eq!(facts.deposits[0].amount, 500);
        assert!(!facts.deposits[1].processed);
        assert_eq!(facts.deposits[1].amount, 300);

        // Bounce-backs distinct from deposits.
        assert_eq!(facts.bounce_backs.len(), 2);
        assert!(facts.bounce_backs[0].processed);
        assert_eq!(facts.bounce_backs[0].amount, 777);
        assert!(!facts.bounce_backs[1].processed);
        assert_eq!(facts.bounce_backs[1].amount, 888);

        // Refund claim.
        assert_eq!(facts.refund_claims.len(), 1);
        assert_eq!(facts.refund_claims[0].amount, 42);

        // Token enabled.
        assert_eq!(facts.enabled_tokens, vec![token_b]);

        // Withdrawal — principal and fee separate.
        assert_eq!(facts.withdrawal_requests.len(), 2);
        let w = &facts.withdrawal_requests[0];
        assert_eq!(w.withdrawal_index, 3);
        assert_eq!(w.principal, 1000);
        assert_eq!(w.fee, 50);
        assert!(w.is_deposit_bounce_back);
        let user = &facts.withdrawal_requests[1];
        assert!(!user.is_deposit_bounce_back);
        assert_eq!(user.sender, Address::repeat_byte(0x44));
        assert_eq!(user.fallback_nonce, 9);

        // Batch finalized.
        assert!(facts.batch_finalized.is_some());
        assert_eq!(
            facts
                .batch_finalized
                .as_ref()
                .unwrap()
                .withdrawal_batch_index,
            7
        );
    }

    #[test]
    fn reject_missing_anchor() {
        let receipts = vec![receipt_with_logs(vec![])];
        let err = extract_l2_facts(1, B256::ZERO, &receipts).unwrap_err();
        assert!(err.to_string().contains("missing TempoAdvanced"));
    }

    #[test]
    fn reject_duplicate_anchor() {
        let anchor = tempo_advanced_log(B256::ZERO, 1, 0, B256::ZERO, 0);
        let receipts = vec![receipt_with_logs(vec![anchor.clone(), anchor])];
        let err = extract_l2_facts(1, B256::ZERO, &receipts).unwrap_err();
        assert!(err.to_string().contains("duplicate TempoAdvanced"));
    }

    #[test]
    fn reject_duplicate_batch() {
        let anchor = tempo_advanced_log(B256::ZERO, 1, 0, B256::ZERO, 0);
        let batch = Log {
            address: ZONE_OUTBOX_ADDRESS,
            data: IZoneOutbox::BatchFinalized {
                withdrawalQueueHash: B256::ZERO,
                withdrawalBatchIndex: 0,
            }
            .encode_log_data(),
        };
        let receipts = vec![receipt_with_logs(vec![anchor, batch.clone(), batch])];
        let err = extract_l2_facts(1, B256::ZERO, &receipts).unwrap_err();
        assert!(err.to_string().contains("duplicate BatchFinalized"));
    }

    #[test]
    fn reject_malformed_known_event() {
        // Build a log with the TempoAdvanced topic but garbage data.
        let bad_data = Log {
            address: ZONE_INBOX_ADDRESS,
            data: alloy_primitives::LogData::new_unchecked(
                vec![IZoneInbox::TempoAdvanced::SIGNATURE_HASH],
                alloy_primitives::Bytes::from(vec![0xde, 0xad]),
            ),
        };
        let receipts = vec![receipt_with_logs(vec![bad_data])];
        let err = extract_l2_facts(1, B256::ZERO, &receipts).unwrap_err();
        assert!(err.to_string().contains("malformed TempoAdvanced"));
    }

    #[test]
    fn reject_non_canonical_trailing_event_data() {
        let anchor = tempo_advanced_log(B256::ZERO, 1, 0, B256::ZERO, 0);
        let mut bytes = anchor.data.data.to_vec();
        bytes.extend([0u8; 32]);
        let trailing = Log {
            address: anchor.address,
            data: alloy_primitives::LogData::new_unchecked(
                anchor.topics().to_vec(),
                alloy_primitives::Bytes::from(bytes),
            ),
        };
        let receipts = vec![receipt_with_logs(vec![trailing])];
        let error = extract_l2_facts(1, B256::ZERO, &receipts).unwrap_err();
        assert!(error.to_string().contains("non-canonical TempoAdvanced"));
    }
}
