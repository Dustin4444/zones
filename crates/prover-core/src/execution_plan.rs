use alloc::vec::Vec;

use alloy_consensus::{
    Signed, TxLegacy,
    transaction::{Recovered, SignerRecoverable},
};
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolCall;
use tempo_primitives::{
    TempoTxEnvelope,
    transaction::envelope::{TEMPO_SYSTEM_TX_SENDER, TEMPO_SYSTEM_TX_SIGNATURE},
};
use tempo_zone_contracts::{ZoneInbox, ZoneOutbox};
use zone_primitives::constants::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS};

use crate::{BatchWitness, ProverError, ZoneBlock};

pub type RecoveredTempoTx = Recovered<TempoTxEnvelope>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneExecutionPlan {
    pub blocks: Vec<ZoneBlockExecutionPlan>,
}

impl ZoneExecutionPlan {
    pub fn from_witness(witness: &BatchWitness) -> Result<Self, ProverError> {
        let blocks = witness
            .zone_blocks
            .iter()
            .enumerate()
            .map(|(block_index, block)| ZoneBlockExecutionPlan::from_block(block_index, block))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { blocks })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneBlockExecutionPlan {
    pub transactions: Vec<PlannedZoneTransaction>,
}

impl ZoneBlockExecutionPlan {
    pub fn from_block(block_index: usize, block: &ZoneBlock) -> Result<Self, ProverError> {
        let mut transactions = Vec::new();

        match &block.tempo_header_rlp {
            Some(header) => {
                transactions.push(PlannedZoneTransaction {
                    kind: PlannedZoneTransactionKind::AdvanceTempo,
                    tx: build_advance_tempo_tx(block, header.clone()),
                });
            }
            None if !block.deposits.is_empty()
                || !block.decryptions.is_empty()
                || !block.enabled_tokens.is_empty() =>
            {
                return Err(ProverError::DepositProcessingUnsupported { index: block_index });
            }
            None => {}
        }

        for (transaction_index, raw) in block.transactions.iter().enumerate() {
            let envelope = TempoTxEnvelope::decode_2718_exact(raw.as_ref()).map_err(|_| {
                ProverError::UserTransactionDecodeFailed {
                    block_index,
                    transaction_index,
                }
            })?;
            let tx = envelope.try_into_recovered().map_err(|_| {
                ProverError::UserTransactionSenderRecoveryFailed {
                    block_index,
                    transaction_index,
                }
            })?;

            transactions.push(PlannedZoneTransaction {
                kind: PlannedZoneTransactionKind::User { transaction_index },
                tx,
            });
        }

        match block.finalize_withdrawal_batch_count {
            Some(count) => {
                transactions.push(PlannedZoneTransaction {
                    kind: PlannedZoneTransactionKind::FinalizeWithdrawalBatch,
                    tx: build_finalize_withdrawal_batch_tx(
                        count,
                        block.number,
                        block.finalize_withdrawal_encrypted_senders.clone(),
                    ),
                });
            }
            None if !block.finalize_withdrawal_encrypted_senders.is_empty() => {
                return Err(ProverError::NonZeroWithdrawalFinalizationUnsupported);
            }
            None => {}
        }

        Ok(Self { transactions })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedZoneTransaction {
    pub kind: PlannedZoneTransactionKind,
    pub tx: RecoveredTempoTx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedZoneTransactionKind {
    AdvanceTempo,
    User { transaction_index: usize },
    FinalizeWithdrawalBatch,
}

fn build_advance_tempo_tx(block: &ZoneBlock, header: Bytes) -> RecoveredTempoTx {
    let calldata = ZoneInbox::advanceTempoCall {
        header,
        deposits: block.deposits.clone(),
        decryptions: block.decryptions.clone(),
        enabledTokens: block.enabled_tokens.clone(),
    }
    .abi_encode();

    let tx = TxLegacy {
        chain_id: None,
        nonce: 0,
        gas_price: 0,
        gas_limit: 0,
        to: ZONE_INBOX_ADDRESS.into(),
        value: U256::ZERO,
        input: calldata.into(),
    };

    Recovered::new_unchecked(
        TempoTxEnvelope::Legacy(Signed::new_unhashed(tx, TEMPO_SYSTEM_TX_SIGNATURE)),
        TEMPO_SYSTEM_TX_SENDER,
    )
}

fn build_finalize_withdrawal_batch_tx(
    count: U256,
    block_number: u64,
    encrypted_senders: Vec<Bytes>,
) -> RecoveredTempoTx {
    let calldata = ZoneOutbox::finalizeWithdrawalBatchCall {
        count,
        blockNumber: block_number,
        encryptedSenders: encrypted_senders,
    }
    .abi_encode();

    let tx = TxLegacy {
        chain_id: None,
        nonce: 0,
        gas_price: 0,
        gas_limit: 0,
        to: ZONE_OUTBOX_ADDRESS.into(),
        value: U256::ZERO,
        input: calldata.into(),
    };

    Recovered::new_unchecked(
        TempoTxEnvelope::Legacy(Signed::new_unhashed(tx, TEMPO_SYSTEM_TX_SIGNATURE)),
        TEMPO_SYSTEM_TX_SENDER,
    )
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use alloy_primitives::{Address, Bytes, U256, address};
    use alloy_sol_types::SolCall;
    use const_hex::FromHex;
    use tempo_zone_contracts::EnabledToken;

    use super::*;

    fn legacy_input(tx: &RecoveredTempoTx) -> &Bytes {
        match tx.inner() {
            TempoTxEnvelope::Legacy(signed) => &signed.tx().input,
            _ => panic!("expected legacy system transaction"),
        }
    }

    fn sample_block() -> ZoneBlock {
        ZoneBlock {
            number: 42,
            parent_hash: Default::default(),
            timestamp: 1,
            beneficiary: Address::ZERO,
            protocol_version: 0,
            tempo_header_rlp: Some(Bytes::from_static(&[0xc0])),
            deposits: vec![],
            decryptions: vec![],
            enabled_tokens: vec![EnabledToken {
                token: address!("0x0000000000000000000000000000000000001000"),
                name: "USD Test".into(),
                symbol: "USDT".into(),
                currency: "USD".into(),
            }],
            finalize_withdrawal_batch_count: Some(U256::from(1)),
            finalize_withdrawal_encrypted_senders: vec![Bytes::from_static(b"sender")],
            transactions: vec![],
        }
    }

    #[test]
    fn plans_system_transactions_in_consensus_order() {
        let plan = ZoneBlockExecutionPlan::from_block(0, &sample_block()).unwrap();

        assert_eq!(plan.transactions.len(), 2);
        assert_eq!(
            plan.transactions[0].kind,
            PlannedZoneTransactionKind::AdvanceTempo
        );
        assert_eq!(
            plan.transactions[1].kind,
            PlannedZoneTransactionKind::FinalizeWithdrawalBatch
        );
        assert_eq!(plan.transactions[0].tx.signer(), TEMPO_SYSTEM_TX_SENDER);
        assert_eq!(plan.transactions[1].tx.signer(), TEMPO_SYSTEM_TX_SENDER);

        let advance =
            ZoneInbox::advanceTempoCall::abi_decode(legacy_input(&plan.transactions[0].tx))
                .unwrap();
        assert_eq!(advance.header, Bytes::from_static(&[0xc0]));
        assert!(advance.deposits.is_empty());
        assert!(advance.decryptions.is_empty());
        assert_eq!(advance.enabledTokens.len(), 1);
        assert_eq!(advance.enabledTokens[0].symbol, "USDT");

        let finalize = ZoneOutbox::finalizeWithdrawalBatchCall::abi_decode(legacy_input(
            &plan.transactions[1].tx,
        ))
        .unwrap();
        assert_eq!(finalize.count, U256::from(1));
        assert_eq!(finalize.blockNumber, 42);
        assert_eq!(
            finalize.encryptedSenders,
            vec![Bytes::from_static(b"sender")]
        );
    }

    #[test]
    fn rejects_invalid_user_transaction_bytes() {
        let mut block = sample_block();
        block.tempo_header_rlp = None;
        block.enabled_tokens.clear();
        block.finalize_withdrawal_batch_count = None;
        block.finalize_withdrawal_encrypted_senders.clear();
        block.transactions.push(Bytes::from_static(b"not a tx"));

        let err = ZoneBlockExecutionPlan::from_block(7, &block).unwrap_err();
        assert_eq!(
            err,
            ProverError::UserTransactionDecodeFailed {
                block_index: 7,
                transaction_index: 0,
            }
        );
    }

    #[test]
    fn recovers_user_transaction_sender() {
        let raw = <Vec<u8>>::from_hex(
            "02f8b082053980018628048c5ec000831e84809420c000000000000000000000000000000000000080b844a9059cbb0000000000000000000000003c44cdddb6a900fa2b585dd299e03d12fa4293bc0000000000000000000000000000000000000000000000000000000005f5e100c001a0e7f78bca071cc3f0b41dabdee8b3b97c47ca8bfe3bf86861ba06cd97567d61f6a02ad11d6959be0eba004f1f3336c8b1c90aced228a00cbd5af990b519792e7b87",
        )
        .unwrap();
        let mut block = sample_block();
        block.tempo_header_rlp = None;
        block.enabled_tokens.clear();
        block.finalize_withdrawal_batch_count = None;
        block.finalize_withdrawal_encrypted_senders.clear();
        block.transactions.push(Bytes::from(raw));

        let plan = ZoneBlockExecutionPlan::from_block(0, &block).unwrap();

        assert_eq!(plan.transactions.len(), 1);
        assert_eq!(
            plan.transactions[0].kind,
            PlannedZoneTransactionKind::User {
                transaction_index: 0,
            }
        );
        assert_eq!(
            plan.transactions[0].tx.signer(),
            address!("0x70997970C51812dc3A010C7d01b50e0d17dC79C8")
        );
    }

    #[test]
    fn rejects_advance_tempo_payloads_without_header() {
        let mut block = sample_block();
        block.tempo_header_rlp = None;
        block.finalize_withdrawal_batch_count = None;
        block.finalize_withdrawal_encrypted_senders.clear();

        let err = ZoneBlockExecutionPlan::from_block(3, &block).unwrap_err();
        assert_eq!(err, ProverError::DepositProcessingUnsupported { index: 3 });
    }

    #[test]
    fn rejects_sender_payloads_without_finalization() {
        let mut block = sample_block();
        block.finalize_withdrawal_batch_count = None;

        let err = ZoneBlockExecutionPlan::from_block(0, &block).unwrap_err();
        assert_eq!(err, ProverError::NonZeroWithdrawalFinalizationUnsupported);
    }
}
