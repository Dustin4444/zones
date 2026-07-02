use alloc::vec::Vec;

use alloy_consensus::{
    Signed, Transaction, TxLegacy,
    transaction::{Recovered, SignerRecoverable},
};
use alloy_eips::eip2718::Decodable2718;
use alloy_evm::FromRecoveredTx;
use alloy_primitives::{Address, Bytes, TxKind, U256};
use alloy_sol_types::SolCall;
use tempo_precompiles::tip20::ITIP20;
use tempo_primitives::{
    TempoAddressExt, TempoTxEnvelope,
    transaction::envelope::{TEMPO_SYSTEM_TX_SENDER, TEMPO_SYSTEM_TX_SIGNATURE},
};
use tempo_zone_contracts::{ZoneInbox, ZoneOutbox};
use zone_primitives::constants::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS};

use crate::{
    BatchWitness, ProverError, ZoneAdvanceTempo, ZoneBlock, ZoneTempoImport, ZoneTxEnv,
    ZoneWithdrawalFinalization,
};

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

        match &block.tempo_import {
            ZoneTempoImport::Advance(import) => {
                transactions.push(PlannedZoneTransaction {
                    kind: PlannedZoneTransactionKind::AdvanceTempo,
                    tx: build_advance_tempo_tx(import),
                });
            }
            ZoneTempoImport::None => {}
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
            validate_user_transaction(block_index, transaction_index, &tx)?;

            transactions.push(PlannedZoneTransaction {
                kind: PlannedZoneTransactionKind::User { transaction_index },
                tx,
            });
        }

        match &block.withdrawal_finalization {
            ZoneWithdrawalFinalization::Finalize(finalization) => {
                transactions.push(PlannedZoneTransaction {
                    kind: PlannedZoneTransactionKind::FinalizeWithdrawalBatch,
                    tx: build_finalize_withdrawal_batch_tx(
                        finalization.count,
                        block.number,
                        finalization.encrypted_senders.clone(),
                    ),
                });
            }
            ZoneWithdrawalFinalization::None => {}
        }

        Ok(Self { transactions })
    }
}

fn validate_user_transaction(
    block_index: usize,
    transaction_index: usize,
    tx: &RecoveredTempoTx,
) -> Result<(), ProverError> {
    if !tx.inner().value().is_zero() {
        return Err(ProverError::UserTransactionValueUnsupported {
            block_index,
            transaction_index,
            value: tx.inner().value(),
        });
    }

    if tx
        .inner()
        .authorization_list()
        .is_some_and(|authorizations| !authorizations.is_empty())
    {
        return Err(ProverError::UserTransactionAuthorizationListUnsupported {
            block_index,
            transaction_index,
        });
    }

    let tx_env = ZoneTxEnv::from_recovered_tx(tx.inner(), tx.signer());
    let mut saw_call = false;
    for (call_index, (target, input)) in tx_env.calls().enumerate() {
        saw_call = true;
        let TxKind::Call(target) = *target else {
            return Err(ProverError::UserTransactionCreateUnsupported {
                block_index,
                transaction_index,
                call_index,
            });
        };

        if !is_allowed_user_call(target, input) {
            return Err(ProverError::UserTransactionTargetUnsupported {
                block_index,
                transaction_index,
                call_index,
                target,
            });
        }
    }

    if !saw_call {
        return Err(ProverError::UserTransactionCreateUnsupported {
            block_index,
            transaction_index,
            call_index: 0,
        });
    }

    Ok(())
}

fn is_allowed_user_call(target: Address, input: &[u8]) -> bool {
    target.is_tip20() && call_selector(input).is_some_and(is_allowed_tip20_user_transfer_selector)
}

fn is_allowed_tip20_user_transfer_selector(selector: [u8; 4]) -> bool {
    matches!(
        selector,
        ITIP20::transferCall::SELECTOR
            | ITIP20::transferWithMemoCall::SELECTOR
            | ITIP20::transferFromCall::SELECTOR
            | ITIP20::transferFromWithMemoCall::SELECTOR
    )
}

fn call_selector(input: &[u8]) -> Option<[u8; 4]> {
    let selector = input.get(..4)?;
    Some([selector[0], selector[1], selector[2], selector[3]])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedZoneTransaction {
    pub kind: PlannedZoneTransactionKind,
    pub tx: RecoveredTempoTx,
}

impl PlannedZoneTransaction {
    pub fn tx_env(&self) -> ZoneTxEnv {
        ZoneTxEnv::from_recovered_tx(self.tx.inner(), self.tx.signer())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedZoneTransactionKind {
    AdvanceTempo,
    User { transaction_index: usize },
    FinalizeWithdrawalBatch,
}

fn build_advance_tempo_tx(import: &ZoneAdvanceTempo) -> RecoveredTempoTx {
    let calldata = ZoneInbox::advanceTempoCall {
        header: import.header_rlp.clone(),
        deposits: import.deposits.clone(),
        decryptions: import.decryptions.clone(),
        enabledTokens: import.enabled_tokens.clone(),
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

    use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
    use alloy_eips::eip2718::Encodable2718;
    use alloy_network::TxSignerSync;
    use alloy_primitives::{Address, B256, Bytes, U256, address};
    use alloy_signer_local::PrivateKeySigner;
    use alloy_sol_types::SolCall;
    use const_hex::FromHex;
    use revm::context::Transaction;
    use tempo_chainspec::hardfork::TempoHardfork;
    use tempo_zone_contracts::EnabledToken;
    use zone_primitives::constants::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS};

    use crate::{ZoneBlockEnvWitness, ZoneCfgEnvWitness};

    use super::*;

    fn legacy_input(tx: &RecoveredTempoTx) -> &Bytes {
        match tx.inner() {
            TempoTxEnvelope::Legacy(signed) => &signed.tx().input,
            _ => panic!("expected legacy system transaction"),
        }
    }

    fn block_env() -> ZoneBlockEnvWitness {
        ZoneBlockEnvWitness {
            gas_limit: 30_000_000,
            basefee: 0,
            difficulty: U256::ZERO,
            prevrandao: B256::ZERO,
            slot_num: 0,
            timestamp_millis_part: 0,
        }
    }

    fn cfg_env() -> ZoneCfgEnvWitness {
        ZoneCfgEnvWitness {
            chain_id: 421_700_001,
            spec: TempoHardfork::T1,
            enable_amsterdam_eip8037: false,
        }
    }

    fn execution_context() -> crate::ZoneBlockExecutionContextWitness {
        crate::ZoneBlockExecutionContextWitness {
            parent_beacon_block_root: B256::ZERO,
            extra_data: Bytes::new(),
        }
    }

    fn sample_block() -> ZoneBlock {
        ZoneBlock {
            number: 42,
            parent_hash: Default::default(),
            timestamp: 1,
            beneficiary: Address::ZERO,
            protocol_version: 0,
            cfg_env: cfg_env(),
            execution_context: execution_context(),
            block_env: block_env(),
            tempo_import: ZoneTempoImport::advance(
                Bytes::from_static(&[0xc0]),
                vec![],
                vec![],
                vec![EnabledToken {
                    token: address!("0x0000000000000000000000000000000000001000"),
                    name: "USD Test".into(),
                    symbol: "USDT".into(),
                    currency: "USD".into(),
                }],
            ),
            withdrawal_finalization: ZoneWithdrawalFinalization::finalize(
                U256::from(1),
                vec![Bytes::from_static(b"sender")],
            ),
            transactions: vec![],
        }
    }

    fn clear_system_transactions(block: &mut ZoneBlock) {
        block.tempo_import = ZoneTempoImport::none();
        block.withdrawal_finalization = ZoneWithdrawalFinalization::none();
    }

    fn signed_user_call(target: Address, input: Bytes, value: U256) -> Bytes {
        let signer: PrivateKeySigner =
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                .parse()
                .expect("test private key should parse");

        let mut tx = TxEip1559 {
            chain_id: 421_700_001,
            nonce: 0,
            gas_limit: 150_000,
            max_fee_per_gas: 1,
            max_priority_fee_per_gas: 1,
            to: target.into(),
            value,
            input,
            ..Default::default()
        };
        let signature = signer
            .sign_transaction_sync(&mut tx)
            .expect("test transaction should sign");
        Bytes::from(TxEnvelope::Eip1559(tx.into_signed(signature)).encoded_2718())
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
        clear_system_transactions(&mut block);
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
    fn rejects_eip4844_user_transaction_bytes() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        block.transactions.push(Bytes::from_static(&[0x03, 0xc0]));

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
    fn rejects_native_value_user_transaction() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        block.transactions.push(signed_user_call(
            address!("0x20c0000000000000000000000000000000000000"),
            Bytes::new(),
            U256::from(1),
        ));

        let err = ZoneBlockExecutionPlan::from_block(7, &block).unwrap_err();
        assert_eq!(
            err,
            ProverError::UserTransactionValueUnsupported {
                block_index: 7,
                transaction_index: 0,
                value: U256::from(1),
            }
        );
    }

    #[test]
    fn admits_user_call_to_tip20_transfer_selector() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        let input = ITIP20::transferCall {
            to: address!("0x0000000000000000000000000000000000001000"),
            amount: U256::from(1),
        }
        .abi_encode()
        .into();
        block.transactions.push(signed_user_call(
            address!("0x20c0000000000000000000000000000000000000"),
            input,
            U256::ZERO,
        ));

        let plan = ZoneBlockExecutionPlan::from_block(7, &block).unwrap();

        assert_eq!(plan.transactions.len(), 1);
        assert_eq!(
            plan.transactions[0].kind,
            PlannedZoneTransactionKind::User {
                transaction_index: 0,
            }
        );
    }

    #[test]
    fn rejects_user_call_to_tip20_system_mint_selector() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        let token = address!("0x20c0000000000000000000000000000000000000");
        let input = ITIP20::mintCall {
            to: address!("0x0000000000000000000000000000000000001000"),
            amount: U256::from(1),
        }
        .abi_encode()
        .into();
        block
            .transactions
            .push(signed_user_call(token, input, U256::ZERO));

        let err = ZoneBlockExecutionPlan::from_block(7, &block).unwrap_err();
        assert_eq!(
            err,
            ProverError::UserTransactionTargetUnsupported {
                block_index: 7,
                transaction_index: 0,
                call_index: 0,
                target: token,
            }
        );
    }

    #[test]
    fn rejects_user_call_to_tip20_approve_selector() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        let token = address!("0x20c0000000000000000000000000000000000000");
        let input = ITIP20::approveCall {
            spender: address!("0x0000000000000000000000000000000000001000"),
            amount: U256::from(1),
        }
        .abi_encode()
        .into();
        block
            .transactions
            .push(signed_user_call(token, input, U256::ZERO));

        let err = ZoneBlockExecutionPlan::from_block(7, &block).unwrap_err();
        assert_eq!(
            err,
            ProverError::UserTransactionTargetUnsupported {
                block_index: 7,
                transaction_index: 0,
                call_index: 0,
                target: token,
            }
        );
    }

    #[test]
    fn rejects_user_call_to_tip403_policy_proxy() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        let target = zone_precompiles::ZONE_TIP403_PROXY_ADDRESS;
        block.transactions.push(signed_user_call(
            target,
            Bytes::from_static(&[0x00, 0x00, 0x00, 0x00]),
            U256::ZERO,
        ));

        let err = ZoneBlockExecutionPlan::from_block(7, &block).unwrap_err();
        assert_eq!(
            err,
            ProverError::UserTransactionTargetUnsupported {
                block_index: 7,
                transaction_index: 0,
                call_index: 0,
                target,
            }
        );
    }

    #[test]
    fn rejects_user_call_to_system_advance_tempo_selector() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        let input = ZoneInbox::advanceTempoCall {
            header: Bytes::from_static(&[0xc0]),
            deposits: vec![],
            decryptions: vec![],
            enabledTokens: vec![],
        }
        .abi_encode()
        .into();
        block
            .transactions
            .push(signed_user_call(ZONE_INBOX_ADDRESS, input, U256::ZERO));

        let err = ZoneBlockExecutionPlan::from_block(7, &block).unwrap_err();
        assert_eq!(
            err,
            ProverError::UserTransactionTargetUnsupported {
                block_index: 7,
                transaction_index: 0,
                call_index: 0,
                target: ZONE_INBOX_ADDRESS,
            }
        );
    }

    #[test]
    fn rejects_user_call_to_claim_refund_selector() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        let input = ZoneInbox::claimRefundCall {
            token: address!("0x20c0000000000000000000000000000000000000"),
        }
        .abi_encode()
        .into();
        block
            .transactions
            .push(signed_user_call(ZONE_INBOX_ADDRESS, input, U256::ZERO));

        assert_eq!(
            ZoneBlockExecutionPlan::from_block(7, &block).unwrap_err(),
            ProverError::UserTransactionTargetUnsupported {
                block_index: 7,
                transaction_index: 0,
                call_index: 0,
                target: ZONE_INBOX_ADDRESS,
            }
        );
    }

    #[test]
    fn rejects_user_call_to_system_finalize_withdrawal_selector() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        let input = ZoneOutbox::finalizeWithdrawalBatchCall {
            count: U256::ZERO,
            blockNumber: block.number,
            encryptedSenders: vec![],
        }
        .abi_encode()
        .into();
        block
            .transactions
            .push(signed_user_call(ZONE_OUTBOX_ADDRESS, input, U256::ZERO));

        let err = ZoneBlockExecutionPlan::from_block(7, &block).unwrap_err();
        assert_eq!(
            err,
            ProverError::UserTransactionTargetUnsupported {
                block_index: 7,
                transaction_index: 0,
                call_index: 0,
                target: ZONE_OUTBOX_ADDRESS,
            }
        );
    }

    #[test]
    fn rejects_user_call_to_system_deposit_bounce_back_selector() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        let input = ZoneOutbox::enqueueDepositBounceBackCall {
            token: address!("0x20c0000000000000000000000000000000000000"),
            amount: 1,
            bouncebackRecipient: address!("0x0000000000000000000000000000000000001000"),
        }
        .abi_encode()
        .into();
        block
            .transactions
            .push(signed_user_call(ZONE_OUTBOX_ADDRESS, input, U256::ZERO));

        let err = ZoneBlockExecutionPlan::from_block(7, &block).unwrap_err();
        assert_eq!(
            err,
            ProverError::UserTransactionTargetUnsupported {
                block_index: 7,
                transaction_index: 0,
                call_index: 0,
                target: ZONE_OUTBOX_ADDRESS,
            }
        );
    }

    #[test]
    fn rejects_user_call_to_request_withdrawal_selector() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);
        let input = ZoneOutbox::requestWithdrawalCall {
            token: address!("0x20c0000000000000000000000000000000000000"),
            to: address!("0x0000000000000000000000000000000000001000"),
            amount: 1,
            memo: B256::ZERO,
            gasLimit: 0,
            fallbackRecipient: address!("0x0000000000000000000000000000000000001000"),
            data: Bytes::new(),
            revealTo: Bytes::new(),
        }
        .abi_encode()
        .into();
        block
            .transactions
            .push(signed_user_call(ZONE_OUTBOX_ADDRESS, input, U256::ZERO));

        assert_eq!(
            ZoneBlockExecutionPlan::from_block(7, &block).unwrap_err(),
            ProverError::UserTransactionTargetUnsupported {
                block_index: 7,
                transaction_index: 0,
                call_index: 0,
                target: ZONE_OUTBOX_ADDRESS,
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
        clear_system_transactions(&mut block);
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
    fn planned_transactions_materialize_zone_tx_envs() {
        let plan = ZoneBlockExecutionPlan::from_block(0, &sample_block()).unwrap();

        let env = plan.transactions[0].tx_env();

        assert_eq!(env.caller(), TEMPO_SYSTEM_TX_SENDER);
        assert_eq!(env.kind(), ZONE_INBOX_ADDRESS.into());
        assert!(env.is_system_tx);
    }

    #[test]
    fn skips_system_transactions_when_variants_are_none() {
        let mut block = sample_block();
        clear_system_transactions(&mut block);

        let plan = ZoneBlockExecutionPlan::from_block(0, &block).unwrap();

        assert!(plan.transactions.is_empty());
    }
}
