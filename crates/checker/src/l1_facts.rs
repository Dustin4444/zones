//! Typed Tempo L1 Portal facts extracted from canonical ZonePortal receipt logs.
//!
//! Like [`crate::l2_facts`], facts are temporary: they are constructed while
//! processing an L2 notification, used to produce a log summary, then discarded.

#![allow(dead_code)]

use alloy_eips::NumHash;
use alloy_network::ReceiptResponse as _;
use alloy_primitives::{Address, B256, Bloom, Log, U256};
use alloy_sol_types::SolEvent;
use tempo_alloy::rpc::TempoTransactionReceipt;
use tempo_zone_contracts::ZonePortal;
use tracing::info;

use crate::decode_event;

// ---------------------------------------------------------------------------
// Fact model
// ---------------------------------------------------------------------------

/// Per-block L1 Portal facts extracted from the ZonePortal on the exact Tempo
/// L1 block anchored by `ZoneInbox.TempoAdvanced`.
#[derive(Debug)]
pub(super) struct L1BlockFacts {
    l1_block_number: u64,
    l1_block_hash: B256,
    events: Vec<L1PortalFact>,
}

/// A single decoded ZonePortal L1 event, preserving canonical log order.
#[derive(Debug)]
pub(super) enum L1PortalFact {
    /// A user deposit escrowed on L1 — new external backing entering the bridge.
    DepositMade {
        token: Address,
        net_amount: u128,
        fee: u128,
        tempo_refund_recipient: Address,
        deposit_number: u64,
        deposit_queue_hash: B256,
    },
    /// A TIP-20 token newly enabled for bridging, with metadata.
    TokenEnabled {
        token: Address,
        name: String,
        symbol: String,
        currency: String,
    },
    /// A finalized withdrawal batch submitted to L1.
    BatchSubmitted {
        withdrawal_batch_index: u64,
        /// Logical queue index, or `U256::MAX` when the batch has no withdrawals.
        withdrawal_queue_index: U256,
        withdrawal_queue_hash: B256,
        next_block_hash: B256,
        last_processed_deposit_number: u64,
    },
    /// A withdrawal paid out on L1.
    WithdrawalProcessed {
        to: Address,
        sender_tag: B256,
        token: Address,
        amount: u128,
        callback_success: bool,
    },
    /// A withdrawal bounce-back — recycles existing Portal backing, not a new
    /// external deposit.  Kept distinct from [`Self::DepositMade`].
    WithdrawalBounceBack {
        token: Address,
        amount: u128,
        fallback_nonce: u64,
        deposit_number: u64,
        deposit_queue_hash: B256,
    },
    /// A deposit bounce-back processed on L1 (fee deducted, refund sent).
    DepositBounceBack {
        tempo_refund_recipient: Address,
        token: Address,
        amount: u128,
        bounceback_fee: u128,
    },
    /// A deposit bounce-back still pending on L1.
    DepositBounceBackPending {
        tempo_refund_recipient: Address,
        token: Address,
        amount: u128,
        bounceback_fee: u128,
    },
    /// A Zone refund claimed on L1.
    RefundClaimed {
        recipient: Address,
        token: Address,
        amount: u128,
    },
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Decode a known ZonePortal event from a single log, ignoring unknown topics.
///
/// Only logs emitted from `portal` are passed to this function. A known event
/// that fails ABI decoding returns a contextual error.
fn decode_portal_fact(log: &Log, block: u64) -> eyre::Result<Option<L1PortalFact>> {
    let Some(topic) = log.topics().first() else {
        return Ok(None);
    };

    let fact = match *topic {
        ZonePortal::DepositMade::SIGNATURE_HASH => {
            let e = decode_event::<ZonePortal::DepositMade>(log, "DepositMade", block)?;
            L1PortalFact::DepositMade {
                token: e.token,
                net_amount: e.netAmount,
                fee: e.fee,
                tempo_refund_recipient: e.tempoRefundRecipient,
                deposit_number: e.depositNumber,
                deposit_queue_hash: e.newCurrentDepositQueueHash,
            }
        }
        ZonePortal::TokenEnabled::SIGNATURE_HASH => {
            let e = decode_event::<ZonePortal::TokenEnabled>(log, "TokenEnabled", block)?;
            L1PortalFact::TokenEnabled {
                token: e.token,
                name: e.name,
                symbol: e.symbol,
                currency: e.currency,
            }
        }
        ZonePortal::BatchSubmitted::SIGNATURE_HASH => {
            let e = decode_event::<ZonePortal::BatchSubmitted>(log, "BatchSubmitted", block)?;
            L1PortalFact::BatchSubmitted {
                withdrawal_batch_index: e.withdrawalBatchIndex,
                withdrawal_queue_index: e.withdrawalQueueIndex,
                withdrawal_queue_hash: e.withdrawalQueueHash,
                next_block_hash: e.nextBlockHash,
                last_processed_deposit_number: e.lastProcessedDepositNumber,
            }
        }
        ZonePortal::WithdrawalProcessed::SIGNATURE_HASH => {
            let e =
                decode_event::<ZonePortal::WithdrawalProcessed>(log, "WithdrawalProcessed", block)?;
            L1PortalFact::WithdrawalProcessed {
                to: e.to,
                sender_tag: e.senderTag,
                token: e.token,
                amount: e.amount,
                callback_success: e.callbackSuccess,
            }
        }
        ZonePortal::WithdrawalBounceBack::SIGNATURE_HASH => {
            let e = decode_event::<ZonePortal::WithdrawalBounceBack>(
                log,
                "WithdrawalBounceBack",
                block,
            )?;
            L1PortalFact::WithdrawalBounceBack {
                token: e.token,
                amount: e.amount,
                fallback_nonce: e.fallbackNonce,
                deposit_number: e.depositNumber,
                deposit_queue_hash: e.newCurrentDepositQueueHash,
            }
        }
        ZonePortal::DepositBounceBack::SIGNATURE_HASH => {
            let e = decode_event::<ZonePortal::DepositBounceBack>(log, "DepositBounceBack", block)?;
            L1PortalFact::DepositBounceBack {
                tempo_refund_recipient: e.tempoRefundRecipient,
                token: e.token,
                amount: e.amount,
                bounceback_fee: e.bouncebackFee,
            }
        }
        ZonePortal::DepositBounceBackPending::SIGNATURE_HASH => {
            let e = decode_event::<ZonePortal::DepositBounceBackPending>(
                log,
                "DepositBounceBackPending",
                block,
            )?;
            L1PortalFact::DepositBounceBackPending {
                tempo_refund_recipient: e.tempoRefundRecipient,
                token: e.token,
                amount: e.amount,
                bounceback_fee: e.bouncebackFee,
            }
        }
        ZonePortal::RefundClaimed::SIGNATURE_HASH => {
            let e = decode_event::<ZonePortal::RefundClaimed>(log, "RefundClaimed", block)?;
            L1PortalFact::RefundClaimed {
                recipient: e.recipient,
                token: e.token,
                amount: e.amount,
            }
        }
        _ => return Ok(None),
    };

    Ok(Some(fact))
}

/// Decode ordered ZonePortal L1 facts from receipt logs.
///
/// Iterates receipts in transaction order and logs within each receipt in log
/// order, preserving canonical cross-event ordering. Only logs from `portal`
/// are considered. Failed receipts are skipped. Unknown Portal topics are
/// ignored. A known event that fails ABI decoding returns a contextual error.
pub(super) fn extract_l1_facts(
    l1_block_number: u64,
    l1_block_hash: B256,
    portal: Address,
    receipts: &[TempoTransactionReceipt],
) -> eyre::Result<L1BlockFacts> {
    let mut events = Vec::new();

    for receipt in receipts {
        // Failed transactions cannot contribute canonical Portal facts.
        if !receipt.status() {
            continue;
        }
        for log in receipt.logs() {
            if log.address() != portal {
                continue;
            }
            if let Some(fact) = decode_portal_fact(&log.inner, l1_block_number)? {
                events.push(fact);
            }
        }
    }

    Ok(L1BlockFacts {
        l1_block_number,
        l1_block_hash,
        events,
    })
}

/// Emit one concise structured info log per extracted L1 block.
pub(super) fn log_l1_facts(facts: &L1BlockFacts, portal: Address) {
    let mut deposit_made = 0u64;
    let mut token_enabled = 0u64;
    let mut batch_submitted = 0u64;
    let mut withdrawal_processed = 0u64;
    let mut withdrawal_bounce_back = 0u64;
    let mut deposit_bounce_back = 0u64;
    let mut deposit_bounce_back_pending = 0u64;
    let mut refund_claimed = 0u64;

    for fact in &facts.events {
        match fact {
            L1PortalFact::DepositMade { .. } => deposit_made += 1,
            L1PortalFact::TokenEnabled { .. } => token_enabled += 1,
            L1PortalFact::BatchSubmitted { .. } => batch_submitted += 1,
            L1PortalFact::WithdrawalProcessed { .. } => withdrawal_processed += 1,
            L1PortalFact::WithdrawalBounceBack { .. } => withdrawal_bounce_back += 1,
            L1PortalFact::DepositBounceBack { .. } => deposit_bounce_back += 1,
            L1PortalFact::DepositBounceBackPending { .. } => deposit_bounce_back_pending += 1,
            L1PortalFact::RefundClaimed { .. } => refund_claimed += 1,
        }
    }

    info!(
        target: "zone::checker",
        l1_block_number = facts.l1_block_number,
        l1_block_hash = %facts.l1_block_hash,
        %portal,
        deposit_made,
        token_enabled,
        batch_submitted,
        withdrawal_processed,
        withdrawal_bounce_back,
        deposit_bounce_back,
        deposit_bounce_back_pending,
        refund_claimed,
        "L1 Portal facts extracted",
    );
}

// ---------------------------------------------------------------------------
// Block / receipt authentication
// ---------------------------------------------------------------------------

/// Verify that a fetched L1 block matches the exact hash and number from the
/// `TempoAdvanced` anchor.  This prevents using a latest/head lookup or a
/// different fork after the anchor is obtained.
pub(super) fn authenticate_l1_block(
    anchor_hash: B256,
    anchor_number: u64,
    rpc_hash: B256,
    computed_hash: B256,
    block_number: u64,
) -> eyre::Result<()> {
    eyre::ensure!(
        rpc_hash == anchor_hash,
        "L1 block hash mismatch: anchor {anchor_hash}, RPC returned {rpc_hash}"
    );
    eyre::ensure!(
        computed_hash == anchor_hash,
        "L1 header hash mismatch: anchor {anchor_hash}, computed {computed_hash}"
    );
    eyre::ensure!(
        block_number == anchor_number,
        "L1 block number mismatch: anchor {anchor_number}, fetched {block_number}"
    );
    Ok(())
}

/// Verify that the number of receipts matches the number of transactions in
/// the block, that each receipt identifies the corresponding transaction and
/// anchored block, and that the receipts root and logs bloom match the header.
pub(super) fn verify_l1_receipts(
    block: NumHash,
    expected_receipts_root: B256,
    expected_logs_bloom: Bloom,
    transaction_hashes: &[B256],
    receipts: &[TempoTransactionReceipt],
) -> eyre::Result<()> {
    let block_number = block.number;
    let block_hash = block.hash;

    if receipts.len() != transaction_hashes.len() {
        eyre::bail!(
            "L1 block {block_number} ({block_hash}) has {} transactions but {} receipts",
            transaction_hashes.len(),
            receipts.len()
        );
    }

    for (index, (transaction_hash, receipt)) in transaction_hashes.iter().zip(receipts).enumerate()
    {
        eyre::ensure!(
            receipt.block_hash() == Some(block_hash),
            "receipt {index} has wrong block hash in L1 block {block_number} ({block_hash})"
        );
        eyre::ensure!(
            receipt.block_number() == Some(block_number),
            "receipt {index} has wrong block number in L1 block {block_number} ({block_hash})"
        );
        eyre::ensure!(
            receipt.transaction_index() == Some(index as u64),
            "receipt {index} has wrong transaction index in L1 block {block_number} ({block_hash})"
        );
        eyre::ensure!(
            receipt.transaction_hash() == *transaction_hash,
            "receipt {index} has wrong transaction hash in L1 block {block_number} ({block_hash})"
        );
    }

    let consensus_receipts = receipts
        .iter()
        .map(|receipt| {
            receipt
                .inner
                .inner
                .clone()
                .map_receipt(|receipt| receipt.map_logs(Into::into))
        })
        .collect::<Vec<_>>();

    let computed_receipts_root =
        alloy_consensus::proofs::calculate_receipt_root(&consensus_receipts);
    if computed_receipts_root != expected_receipts_root {
        eyre::bail!(
            "receipt root mismatch for L1 block {block_number} ({block_hash}): \
             expected {expected_receipts_root}, got {computed_receipts_root}"
        );
    }

    let computed_logs_bloom = consensus_receipts
        .iter()
        .fold(Bloom::ZERO, |bloom, receipt| bloom | receipt.bloom_ref());
    if computed_logs_bloom != expected_logs_bloom {
        eyre::bail!(
            "logs bloom mismatch for L1 block {block_number} ({block_hash}): \
             expected {expected_logs_bloom}, got {computed_logs_bloom}"
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{BlockHeader, Header, ReceiptWithBloom};
    use alloy_network::primitives::HeaderResponse as _;
    use alloy_primitives::{Bloom, U256, address};
    use alloy_rpc_types_eth::{Header as RpcHeader, TransactionReceipt};
    use tempo_alloy::rpc::{TempoHeaderResponse, TempoTransactionReceipt};
    use tempo_primitives::{TempoHeader, TempoReceipt, TempoTxType};

    const PORTAL: Address = address!("0x0000000000000000000000000000000000000abc");
    const L1_BLOCK: u64 = 100;
    const L1_HASH: B256 = B256::repeat_byte(0x10);

    fn receipt_with_logs(
        success: bool,
        logs: Vec<alloy_rpc_types_eth::Log>,
    ) -> TempoTransactionReceipt {
        let inner_receipt = TempoReceipt {
            tx_type: TempoTxType::Legacy,
            success,
            cumulative_gas_used: 0,
            logs,
        };
        TempoTransactionReceipt {
            inner: TransactionReceipt {
                inner: ReceiptWithBloom::new(inner_receipt, Bloom::ZERO),
                transaction_hash: B256::ZERO,
                transaction_index: Some(0),
                block_hash: Some(L1_HASH),
                block_number: Some(L1_BLOCK),
                gas_used: 0,
                effective_gas_price: 0,
                blob_gas_used: None,
                blob_gas_price: None,
                from: Address::ZERO,
                to: Some(Address::ZERO),
                contract_address: None,
            },
            fee_token: None,
            fee_payer: Address::ZERO,
        }
    }

    fn portal_log(event_data: alloy_primitives::LogData) -> alloy_rpc_types_eth::Log {
        alloy_rpc_types_eth::Log {
            inner: alloy_primitives::Log {
                address: PORTAL,
                data: event_data,
            },
            block_hash: Some(L1_HASH),
            block_number: Some(L1_BLOCK),
            block_timestamp: None,
            transaction_hash: Some(B256::ZERO),
            transaction_index: Some(0),
            log_index: Some(0),
            removed: false,
        }
    }

    fn deposit_made_log() -> alloy_rpc_types_eth::Log {
        portal_log(
            ZonePortal::DepositMade {
                newCurrentDepositQueueHash: B256::repeat_byte(0x01),
                sender: Address::repeat_byte(0x02),
                token: Address::repeat_byte(0x03),
                netAmount: 500,
                fee: 10,
                keyIndex: U256::ZERO,
                ephemeralPubkeyX: B256::ZERO,
                ephemeralPubkeyYParity: 0,
                ciphertext: Default::default(),
                nonce: [0u8; 12].into(),
                tag: [0u8; 16].into(),
                tempoRefundRecipient: Address::repeat_byte(0x04),
                depositNumber: 7,
            }
            .encode_log_data(),
        )
    }

    fn token_enabled_log() -> alloy_rpc_types_eth::Log {
        portal_log(
            ZonePortal::TokenEnabled {
                token: Address::repeat_byte(0x05),
                name: "Test".into(),
                symbol: "TST".into(),
                currency: "USD".into(),
            }
            .encode_log_data(),
        )
    }

    fn batch_submitted_log() -> alloy_rpc_types_eth::Log {
        portal_log(
            ZonePortal::BatchSubmitted {
                withdrawalBatchIndex: 3,
                withdrawalQueueIndex: U256::from(1),
                nextProcessedDepositQueueHash: B256::ZERO,
                nextBlockHash: B256::repeat_byte(0x06),
                withdrawalQueueHash: B256::repeat_byte(0x07),
                lastProcessedDepositNumber: 9,
            }
            .encode_log_data(),
        )
    }

    fn withdrawal_processed_log() -> alloy_rpc_types_eth::Log {
        portal_log(
            ZonePortal::WithdrawalProcessed {
                to: Address::repeat_byte(0x08),
                senderTag: B256::repeat_byte(0x09),
                token: Address::repeat_byte(0x0a),
                amount: 1000,
                callbackSuccess: true,
            }
            .encode_log_data(),
        )
    }

    fn withdrawal_bounce_back_log() -> alloy_rpc_types_eth::Log {
        portal_log(
            ZonePortal::WithdrawalBounceBack {
                newCurrentDepositQueueHash: B256::repeat_byte(0x0b),
                fallbackNonce: 42,
                token: Address::repeat_byte(0x0c),
                amount: 777,
                depositNumber: 5,
            }
            .encode_log_data(),
        )
    }

    fn deposit_bounce_back_log() -> alloy_rpc_types_eth::Log {
        portal_log(
            ZonePortal::DepositBounceBack {
                tempoRefundRecipient: Address::repeat_byte(0x0d),
                token: Address::repeat_byte(0x0e),
                amount: 300,
                bouncebackFee: 5,
            }
            .encode_log_data(),
        )
    }

    fn deposit_bounce_back_pending_log() -> alloy_rpc_types_eth::Log {
        portal_log(
            ZonePortal::DepositBounceBackPending {
                tempoRefundRecipient: Address::repeat_byte(0x0f),
                token: Address::repeat_byte(0x10),
                amount: 200,
                bouncebackFee: 3,
            }
            .encode_log_data(),
        )
    }

    fn refund_claimed_log() -> alloy_rpc_types_eth::Log {
        portal_log(
            ZonePortal::RefundClaimed {
                recipient: Address::repeat_byte(0x11),
                token: Address::repeat_byte(0x12),
                amount: 42,
            }
            .encode_log_data(),
        )
    }

    #[test]
    fn extract_all_portal_event_variants_in_order() {
        let receipts = vec![receipt_with_logs(
            true,
            vec![
                deposit_made_log(),
                token_enabled_log(),
                batch_submitted_log(),
                withdrawal_processed_log(),
                withdrawal_bounce_back_log(),
                deposit_bounce_back_log(),
                deposit_bounce_back_pending_log(),
                refund_claimed_log(),
            ],
        )];

        let facts = extract_l1_facts(L1_BLOCK, L1_HASH, PORTAL, &receipts).unwrap();

        assert_eq!(facts.events.len(), 8);
        assert!(matches!(
            facts.events[0],
            L1PortalFact::DepositMade {
                deposit_number: 7,
                ..
            }
        ));
        assert!(matches!(facts.events[1], L1PortalFact::TokenEnabled { .. }));
        assert!(matches!(
            facts.events[2],
            L1PortalFact::BatchSubmitted {
                withdrawal_batch_index: 3,
                ..
            }
        ));
        assert!(matches!(
            facts.events[3],
            L1PortalFact::WithdrawalProcessed {
                callback_success: true,
                ..
            }
        ));
        assert!(matches!(
            facts.events[4],
            L1PortalFact::WithdrawalBounceBack {
                fallback_nonce: 42,
                ..
            }
        ));
        assert!(matches!(
            facts.events[5],
            L1PortalFact::DepositBounceBack { .. }
        ));
        assert!(matches!(
            facts.events[6],
            L1PortalFact::DepositBounceBackPending { .. }
        ));
        assert!(matches!(
            facts.events[7],
            L1PortalFact::RefundClaimed { amount: 42, .. }
        ));
    }

    #[test]
    fn canonical_order_across_transactions() {
        let r1 = receipt_with_logs(true, vec![deposit_made_log()]);
        let r2 = receipt_with_logs(true, vec![withdrawal_processed_log(), token_enabled_log()]);
        let receipts = vec![r1, r2];

        let facts = extract_l1_facts(L1_BLOCK, L1_HASH, PORTAL, &receipts).unwrap();

        assert_eq!(facts.events.len(), 3);
        assert!(matches!(facts.events[0], L1PortalFact::DepositMade { .. }));
        assert!(matches!(
            facts.events[1],
            L1PortalFact::WithdrawalProcessed { .. }
        ));
        assert!(matches!(facts.events[2], L1PortalFact::TokenEnabled { .. }));
    }

    #[test]
    fn withdrawal_bounce_back_distinct_from_deposit_made() {
        let receipts = vec![receipt_with_logs(
            true,
            vec![deposit_made_log(), withdrawal_bounce_back_log()],
        )];

        let facts = extract_l1_facts(L1_BLOCK, L1_HASH, PORTAL, &receipts).unwrap();

        assert!(matches!(facts.events[0], L1PortalFact::DepositMade { .. }));
        assert!(matches!(
            facts.events[1],
            L1PortalFact::WithdrawalBounceBack { .. }
        ));
    }

    #[test]
    fn ignore_unknown_topics_wrong_addresses_and_failed_receipts() {
        let unknown_topic = portal_log(alloy_primitives::LogData::new_unchecked(
            vec![B256::repeat_byte(0xff)],
            alloy_primitives::Bytes::new(),
        ));
        let wrong_addr = alloy_rpc_types_eth::Log {
            inner: alloy_primitives::Log {
                address: Address::repeat_byte(0x99),
                data: deposit_made_log().inner.data,
            },
            ..Default::default()
        };
        let failed = receipt_with_logs(false, vec![deposit_made_log()]);

        let receipts = vec![
            receipt_with_logs(true, vec![unknown_topic, wrong_addr]),
            failed,
        ];

        let facts = extract_l1_facts(L1_BLOCK, L1_HASH, PORTAL, &receipts).unwrap();
        assert!(facts.events.is_empty());
    }

    #[test]
    fn reject_malformed_known_event() {
        let bad = portal_log(alloy_primitives::LogData::new_unchecked(
            vec![ZonePortal::DepositMade::SIGNATURE_HASH],
            alloy_primitives::Bytes::from(vec![0xde, 0xad]),
        ));
        let receipts = vec![receipt_with_logs(true, vec![bad])];
        let err = extract_l1_facts(L1_BLOCK, L1_HASH, PORTAL, &receipts).unwrap_err();
        assert!(err.to_string().contains("malformed DepositMade"));
    }

    #[test]
    fn reject_non_canonical_trailing_data() {
        let log = deposit_made_log();
        let mut bytes = log.inner.data.data.to_vec();
        bytes.extend([0u8; 32]);
        let trailing = portal_log(alloy_primitives::LogData::new_unchecked(
            log.inner.topics().to_vec(),
            alloy_primitives::Bytes::from(bytes),
        ));
        let receipts = vec![receipt_with_logs(true, vec![trailing])];
        let err = extract_l1_facts(L1_BLOCK, L1_HASH, PORTAL, &receipts).unwrap_err();
        assert!(err.to_string().contains("non-canonical DepositMade"));
    }

    #[test]
    fn authenticate_l1_block_validates_hash_and_number() {
        assert!(authenticate_l1_block(L1_HASH, L1_BLOCK, L1_HASH, L1_HASH, L1_BLOCK).is_ok());
        assert!(
            authenticate_l1_block(
                L1_HASH,
                L1_BLOCK,
                B256::repeat_byte(0xff),
                L1_HASH,
                L1_BLOCK
            )
            .is_err()
        );
        assert!(
            authenticate_l1_block(
                L1_HASH,
                L1_BLOCK,
                L1_HASH,
                B256::repeat_byte(0xff),
                L1_BLOCK
            )
            .is_err()
        );
        assert!(authenticate_l1_block(L1_HASH, L1_BLOCK, L1_HASH, L1_HASH, 999).is_err());
    }

    #[test]
    fn batch_submitted_preserves_no_queue_index() {
        let mut log = batch_submitted_log();
        log.inner.data = ZonePortal::BatchSubmitted {
            withdrawalBatchIndex: 3,
            withdrawalQueueIndex: U256::MAX,
            nextProcessedDepositQueueHash: B256::ZERO,
            nextBlockHash: B256::repeat_byte(0x06),
            withdrawalQueueHash: B256::ZERO,
            lastProcessedDepositNumber: 9,
        }
        .encode_log_data();
        let receipts = vec![receipt_with_logs(true, vec![log])];

        let facts = extract_l1_facts(L1_BLOCK, L1_HASH, PORTAL, &receipts).unwrap();
        assert!(matches!(
            facts.events[0],
            L1PortalFact::BatchSubmitted {
                withdrawal_queue_index: U256::MAX,
                ..
            }
        ));
    }

    #[test]
    fn verify_l1_receipts_rejects_count_mismatch() {
        let block = NumHash::new(L1_BLOCK, L1_HASH);
        let receipts = vec![receipt_with_logs(true, vec![])];
        let transaction_hashes = [B256::ZERO, B256::repeat_byte(1)];
        let err = verify_l1_receipts(
            block,
            B256::ZERO,
            Bloom::ZERO,
            &transaction_hashes,
            &receipts,
        )
        .unwrap_err();
        assert!(err.to_string().contains("2 transactions but 1 receipts"));
    }

    #[test]
    fn verify_l1_receipts_rejects_wrong_transaction_metadata() {
        let block = NumHash::new(L1_BLOCK, L1_HASH);
        let receipts = vec![receipt_with_logs(true, vec![])];
        let err = verify_l1_receipts(
            block,
            B256::ZERO,
            Bloom::ZERO,
            &[B256::repeat_byte(1)],
            &receipts,
        )
        .unwrap_err();
        assert!(err.to_string().contains("wrong transaction hash"));
    }

    #[test]
    fn verify_l1_receipts_rejects_root_mismatch() {
        let block = NumHash::new(L1_BLOCK, L1_HASH);
        let receipts = vec![receipt_with_logs(true, vec![deposit_made_log()])];
        let root = alloy_consensus::proofs::calculate_receipt_root(&[receipts[0]
            .inner
            .inner
            .clone()
            .map_receipt(|r| r.map_logs(Into::into))]);
        // pass a wrong root
        let err = verify_l1_receipts(
            block,
            B256::repeat_byte(0xff),
            Bloom::ZERO,
            &[B256::ZERO],
            &receipts,
        )
        .unwrap_err();
        assert!(err.to_string().contains("receipt root mismatch"));
        // pass correct root and bloom
        assert!(verify_l1_receipts(block, root, Bloom::ZERO, &[B256::ZERO], &receipts).is_ok());
    }

    #[test]
    fn verify_l1_receipts_rejects_bloom_mismatch() {
        let block = NumHash::new(L1_BLOCK, L1_HASH);
        let receipts = vec![receipt_with_logs(true, vec![deposit_made_log()])];
        let root = alloy_consensus::proofs::calculate_receipt_root(&[receipts[0]
            .inner
            .inner
            .clone()
            .map_receipt(|r| r.map_logs(Into::into))]);
        let err = verify_l1_receipts(
            block,
            root,
            Bloom::repeat_byte(0xff),
            &[B256::ZERO],
            &receipts,
        )
        .unwrap_err();
        assert!(err.to_string().contains("logs bloom mismatch"));
    }

    // --- Header/block helper tests for authentication ---

    fn make_header(number: u64) -> TempoHeader {
        TempoHeader {
            inner: Header {
                number,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn header_response(header: TempoHeader) -> TempoHeaderResponse {
        let hash = alloy_primitives::keccak256(alloy_rlp::encode(&header));
        TempoHeaderResponse {
            inner: RpcHeader {
                hash,
                inner: header,
                total_difficulty: None,
                size: None,
            },
            timestamp_millis: 0,
        }
    }

    #[test]
    fn header_response_provides_correct_hash_and_number() {
        let header = make_header(L1_BLOCK);
        let resp = header_response(header);
        assert_eq!(resp.number(), L1_BLOCK);
        let expected_hash = alloy_primitives::keccak256(alloy_rlp::encode(&make_header(L1_BLOCK)));
        assert_eq!(resp.hash(), expected_hash);
    }
}
