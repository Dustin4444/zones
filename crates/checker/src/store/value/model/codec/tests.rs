use alloy_primitives::{Address, B256, U256};
use reth_codecs::{Compress, Decompress};

use super::*;

#[derive(Default)]
struct Golden(Vec<u8>);

impl Golden {
    fn byte(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn u128(&mut self, value: u128) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn address(&mut self, value: Address) {
        self.0.extend_from_slice(value.as_slice());
    }

    fn hash(&mut self, value: B256) {
        self.0.extend_from_slice(value.as_slice());
    }

    fn u256(&mut self, value: U256) {
        self.0.extend_from_slice(&value.to_be_bytes::<32>());
    }

    fn bytes(&mut self, value: &[u8]) {
        self.u32(u32::try_from(value.len()).unwrap());
        self.0.extend_from_slice(value);
    }

    fn cursor(&mut self, value: CursorValue) {
        self.hash(value.hash);
        self.u64(value.number);
    }

    fn finish(self) -> Vec<u8> {
        self.0
    }
}

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn address(byte: u8) -> Address {
    Address::repeat_byte(byte)
}

fn cursor(byte: u8, number: u64) -> CursorValue {
    CursorValue {
        hash: hash(byte),
        number,
    }
}

fn user_identity() -> UserWithdrawalIdentityValue {
    UserWithdrawalIdentityValue {
        sender: address(0x11),
        transaction_hash: hash(0x22),
        fallback_nonce: 1,
    }
}

fn user_request() -> UserWithdrawalRequestValue {
    UserWithdrawalRequestValue {
        token: address(0x33),
        recipient: address(0x44),
        amount: 2,
        memo: hash(0x55),
        gas_limit: 3,
        callback_data: vec![4, 5],
    }
}

fn ordinary_deposit() -> OrdinaryDepositValue {
    OrdinaryDepositValue {
        token: address(0x66),
        sender: address(0x77),
        amount: 6,
        tempo_refund_recipient: address(0x88),
        key_index: U256::from(7),
        ephemeral_pubkey_x: hash(0x99),
        ephemeral_pubkey_y_parity: 0x02,
        ciphertext: vec![0xaa; 64],
        nonce: [0xbb; 12],
        tag: [0xcc; 16],
    }
}

fn finalized_batch() -> FinalizedBatchValue {
    FinalizedBatchValue {
        boundary: BatchBoundaryValue {
            first_zone_parent_hash: hash(0x10),
            final_zone_block_hash: hash(0x20),
            first_processed_deposit: cursor(0x30, 4),
            final_processed_deposit: cursor(0x40, 5),
            final_imported_tempo_block_number: 6,
            final_zone_height: 7,
        },
        members: BatchMembersValue {
            first_withdrawal_index: 8,
            member_count: 2,
            withdrawal_queue_hash: hash(0x50),
        },
    }
}

fn fixtures() -> Vec<(&'static str, ModelValue)> {
    let identity = user_identity();
    let request = user_request();
    let batch = finalized_batch();
    vec![
        (
            "portal config",
            ModelValue::PortalConfig { bounceback_gas: 1 },
        ),
        (
            "zone config",
            ModelValue::ZoneConfig {
                tempo_gas_rate: 2,
                max_withdrawals_per_block: 3,
            },
        ),
        (
            "portal cursor",
            ModelValue::PortalDepositCursor(cursor(0x01, 4)),
        ),
        (
            "zone cursor",
            ModelValue::ZoneProcessedDepositCursor(cursor(0x02, 5)),
        ),
        (
            "portal settlement",
            ModelValue::PortalSettlement(PortalSettlementValue {
                withdrawal_batch_index: 6,
                block_hash: hash(0x03),
                last_synced_tempo_block_number: 7,
                last_submitted_deposit_cursor: cursor(0x04, 8),
                zone_height: U256::from(9),
                withdrawal_queue_head: U256::from(10),
                withdrawal_queue_tail: U256::from(11),
            }),
        ),
        (
            "zone accumulator",
            ModelValue::ZoneBatchAccumulator(ZoneBatchAccumulatorValue {
                last_withdrawal_queue_hash: hash(0x05),
                last_withdrawal_batch_index: 12,
                first_zone_parent_hash: hash(0x06),
                first_processed_deposit: cursor(0x07, 13),
                first_withdrawal_index: 14,
            }),
        ),
        ("next withdrawal", ModelValue::ZoneNextWithdrawalIndex(15)),
        ("last fallback", ModelValue::ZoneLastFallbackNonce(16)),
        (
            "token pending",
            ModelValue::Token(TokenValue {
                phase: StoredTokenPhase::PendingZoneEnable,
                supply: U256::from(17),
                deposit_liability: U256::from(18),
                withdrawal_liability: U256::from(19),
            }),
        ),
        (
            "token enabled",
            ModelValue::Token(TokenValue {
                phase: StoredTokenPhase::ZoneEnabled,
                supply: U256::from(20),
                deposit_liability: U256::from(21),
                withdrawal_liability: U256::from(22),
            }),
        ),
        (
            "ordinary deposit",
            ModelValue::PendingDeposit(PendingDepositValue::Ordinary(ordinary_deposit())),
        ),
        (
            "bounce-back deposit",
            ModelValue::PendingDeposit(PendingDepositValue::WithdrawalBounceBack {
                withdrawal_zone_id: 23,
                withdrawal_index: 24,
                preimage: BounceBackDepositValue {
                    token: address(0x23),
                    fallback_nonce: 25,
                    amount: 26,
                },
            }),
        ),
        (
            "pending user without reveal",
            ModelValue::Withdrawal(WithdrawalValue::Pending(PendingWithdrawalValue::User {
                identity,
                request: request.clone(),
                sender_reveal: StoredSenderReveal::None,
            })),
        ),
        (
            "pending user with reveal",
            ModelValue::Withdrawal(WithdrawalValue::Pending(PendingWithdrawalValue::User {
                identity,
                request: request.clone(),
                sender_reveal: StoredSenderReveal::Encrypted,
            })),
        ),
        (
            "pending failed deposit",
            ModelValue::Withdrawal(WithdrawalValue::Pending(
                PendingWithdrawalValue::FailedDeposit {
                    deposit_portal: address(0x24),
                    deposit_number: 27,
                    token: address(0x25),
                    recipient: address(0x26),
                    amount: 28,
                },
            )),
        ),
        (
            "finalized user without encrypted sender",
            ModelValue::Withdrawal(WithdrawalValue::FinalizedUser {
                identity,
                request: request.clone(),
                encrypted_sender: Vec::new(),
            }),
        ),
        (
            "finalized user with encrypted sender",
            ModelValue::Withdrawal(WithdrawalValue::FinalizedUser {
                identity,
                request,
                encrypted_sender: vec![0x27; 113],
            }),
        ),
        (
            "finalized failed deposit",
            ModelValue::Withdrawal(WithdrawalValue::FinalizedFailedDeposit {
                deposit_portal: address(0x28),
                deposit_number: 29,
                token: address(0x29),
                recipient: address(0x2a),
                amount: 30,
            }),
        ),
        (
            "held fallback",
            ModelValue::FallbackOwner(FallbackOwnerValue::Held {
                withdrawal_zone_id: 31,
                withdrawal_index: 32,
                token: address(0x2b),
                amount: 33,
            }),
        ),
        (
            "queued fallback",
            ModelValue::FallbackOwner(FallbackOwnerValue::BounceBackQueued {
                withdrawal_zone_id: 34,
                withdrawal_index: 35,
                token: address(0x2c),
                amount: 36,
                deposit_portal: address(0x2d),
                deposit_number: 37,
            }),
        ),
        (
            "finalized batch",
            ModelValue::Batch(BatchValue::Finalized(batch)),
        ),
        (
            "submitted batch",
            ModelValue::Batch(BatchValue::Submitted {
                batch,
                portal: address(0x2e),
                logical_queue_index: U256::from(38),
                next_processing_ordinal: 1,
                remaining_queue_hash: hash(0x2f),
            }),
        ),
        ("portal refund", ModelValue::PortalRefundCredit(39)),
        ("inbox refund", ModelValue::InboxRefundCredit(40)),
    ]
}

fn golden_model(value: &ModelValue) -> Vec<u8> {
    let mut out = Golden::default();
    out.byte(0x02);
    match value {
        ModelValue::PortalConfig { bounceback_gas } => {
            out.byte(0x00);
            out.u64(*bounceback_gas);
        }
        ModelValue::ZoneConfig {
            tempo_gas_rate,
            max_withdrawals_per_block,
        } => {
            out.byte(0x01);
            out.u128(*tempo_gas_rate);
            out.u32(*max_withdrawals_per_block);
        }
        ModelValue::PortalDepositCursor(value) => {
            out.byte(0x02);
            out.cursor(*value);
        }
        ModelValue::ZoneProcessedDepositCursor(value) => {
            out.byte(0x03);
            out.cursor(*value);
        }
        ModelValue::PortalSettlement(value) => {
            out.byte(0x04);
            golden_portal_settlement(&mut out, *value);
        }
        ModelValue::ZoneBatchAccumulator(value) => {
            out.byte(0x05);
            golden_zone_accumulator(&mut out, *value);
        }
        ModelValue::ZoneNextWithdrawalIndex(index) => {
            out.byte(0x06);
            out.u64(*index);
        }
        ModelValue::ZoneLastFallbackNonce(nonce) => {
            out.byte(0x07);
            out.u64(*nonce);
        }
        ModelValue::Token(value) => {
            out.byte(0x20);
            golden_token(&mut out, *value);
        }
        ModelValue::PendingDeposit(value) => {
            out.byte(0x30);
            golden_pending_deposit(&mut out, value);
        }
        ModelValue::Withdrawal(value) => {
            out.byte(0x40);
            golden_withdrawal(&mut out, value);
        }
        ModelValue::FallbackOwner(value) => {
            out.byte(0x50);
            golden_fallback(&mut out, *value);
        }
        ModelValue::Batch(value) => {
            out.byte(0x60);
            golden_batch(&mut out, *value);
        }
        ModelValue::PortalRefundCredit(amount) => {
            out.byte(0x70);
            out.u128(*amount);
        }
        ModelValue::InboxRefundCredit(amount) => {
            out.byte(0x71);
            out.u128(*amount);
        }
    }
    out.finish()
}

fn golden_portal_settlement(out: &mut Golden, value: PortalSettlementValue) {
    out.u64(value.withdrawal_batch_index);
    out.hash(value.block_hash);
    out.u64(value.last_synced_tempo_block_number);
    out.cursor(value.last_submitted_deposit_cursor);
    out.u256(value.zone_height);
    out.u256(value.withdrawal_queue_head);
    out.u256(value.withdrawal_queue_tail);
}

fn golden_zone_accumulator(out: &mut Golden, value: ZoneBatchAccumulatorValue) {
    out.hash(value.last_withdrawal_queue_hash);
    out.u64(value.last_withdrawal_batch_index);
    out.hash(value.first_zone_parent_hash);
    out.cursor(value.first_processed_deposit);
    out.u64(value.first_withdrawal_index);
}

fn golden_token(out: &mut Golden, value: TokenValue) {
    out.byte(match value.phase {
        StoredTokenPhase::PendingZoneEnable => 0x00,
        StoredTokenPhase::ZoneEnabled => 0x01,
    });
    out.u256(value.supply);
    out.u256(value.deposit_liability);
    out.u256(value.withdrawal_liability);
}

fn golden_pending_deposit(out: &mut Golden, value: &PendingDepositValue) {
    match value {
        PendingDepositValue::Ordinary(value) => {
            out.byte(0x00);
            out.address(value.token);
            out.address(value.sender);
            out.u128(value.amount);
            out.address(value.tempo_refund_recipient);
            out.u256(value.key_index);
            out.hash(value.ephemeral_pubkey_x);
            out.byte(value.ephemeral_pubkey_y_parity);
            out.0.extend_from_slice(&value.ciphertext);
            out.0.extend_from_slice(&value.nonce);
            out.0.extend_from_slice(&value.tag);
        }
        PendingDepositValue::WithdrawalBounceBack {
            withdrawal_zone_id,
            withdrawal_index,
            preimage,
        } => {
            out.byte(0x01);
            out.u32(*withdrawal_zone_id);
            out.u64(*withdrawal_index);
            out.address(preimage.token);
            out.u64(preimage.fallback_nonce);
            out.u128(preimage.amount);
        }
    }
}

fn golden_withdrawal(out: &mut Golden, value: &WithdrawalValue) {
    match value {
        WithdrawalValue::Pending(value) => {
            out.byte(0x00);
            golden_pending_withdrawal(out, value);
        }
        WithdrawalValue::FinalizedUser {
            identity,
            request,
            encrypted_sender,
        } => {
            out.byte(0x01);
            golden_user_identity(out, *identity);
            golden_user_request(out, request);
            out.bytes(encrypted_sender);
        }
        WithdrawalValue::FinalizedFailedDeposit {
            deposit_portal,
            deposit_number,
            token,
            recipient,
            amount,
        } => {
            out.byte(0x02);
            out.address(*deposit_portal);
            out.u64(*deposit_number);
            out.address(*token);
            out.address(*recipient);
            out.u128(*amount);
        }
    }
}

fn golden_pending_withdrawal(out: &mut Golden, value: &PendingWithdrawalValue) {
    match value {
        PendingWithdrawalValue::User {
            identity,
            request,
            sender_reveal,
        } => {
            out.byte(0x00);
            golden_user_identity(out, *identity);
            golden_user_request(out, request);
            out.byte(match sender_reveal {
                StoredSenderReveal::None => 0x00,
                StoredSenderReveal::Encrypted => 0x01,
            });
        }
        PendingWithdrawalValue::FailedDeposit {
            deposit_portal,
            deposit_number,
            token,
            recipient,
            amount,
        } => {
            out.byte(0x01);
            out.address(*deposit_portal);
            out.u64(*deposit_number);
            out.address(*token);
            out.address(*recipient);
            out.u128(*amount);
        }
    }
}

fn golden_user_identity(out: &mut Golden, value: UserWithdrawalIdentityValue) {
    out.address(value.sender);
    out.hash(value.transaction_hash);
    out.u64(value.fallback_nonce);
}

fn golden_user_request(out: &mut Golden, value: &UserWithdrawalRequestValue) {
    out.address(value.token);
    out.address(value.recipient);
    out.u128(value.amount);
    out.hash(value.memo);
    out.u64(value.gas_limit);
    out.bytes(&value.callback_data);
}

fn golden_fallback(out: &mut Golden, value: FallbackOwnerValue) {
    let (tag, zone_id, index, token, amount, queued) = match value {
        FallbackOwnerValue::Held {
            withdrawal_zone_id,
            withdrawal_index,
            token,
            amount,
        } => (
            0x00,
            withdrawal_zone_id,
            withdrawal_index,
            token,
            amount,
            None,
        ),
        FallbackOwnerValue::BounceBackQueued {
            withdrawal_zone_id,
            withdrawal_index,
            token,
            amount,
            deposit_portal,
            deposit_number,
        } => (
            0x01,
            withdrawal_zone_id,
            withdrawal_index,
            token,
            amount,
            Some((deposit_portal, deposit_number)),
        ),
    };
    out.byte(tag);
    out.u32(zone_id);
    out.u64(index);
    out.address(token);
    out.u128(amount);
    if let Some((portal, number)) = queued {
        out.address(portal);
        out.u64(number);
    }
}

fn golden_batch(out: &mut Golden, value: BatchValue) {
    match value {
        BatchValue::Finalized(batch) => {
            out.byte(0x00);
            golden_finalized_batch(out, batch);
        }
        BatchValue::Submitted {
            batch,
            portal,
            logical_queue_index,
            next_processing_ordinal,
            remaining_queue_hash,
        } => {
            out.byte(0x01);
            golden_finalized_batch(out, batch);
            out.address(portal);
            out.u256(logical_queue_index);
            out.u64(next_processing_ordinal);
            out.hash(remaining_queue_hash);
        }
    }
}

fn golden_finalized_batch(out: &mut Golden, value: FinalizedBatchValue) {
    out.hash(value.boundary.first_zone_parent_hash);
    out.hash(value.boundary.final_zone_block_hash);
    out.cursor(value.boundary.first_processed_deposit);
    out.cursor(value.boundary.final_processed_deposit);
    out.u64(value.boundary.final_imported_tempo_block_number);
    out.u64(value.boundary.final_zone_height);
    out.u64(value.members.first_withdrawal_index);
    out.u64(value.members.member_count);
    out.hash(value.members.withdrawal_queue_hash);
}

mod golden_tests;
mod reject_tests;
