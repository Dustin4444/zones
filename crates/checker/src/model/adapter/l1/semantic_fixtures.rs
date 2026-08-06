use alloy_primitives::{Address, B256, U256};

use super::*;

impl ObservedImportedOutput {
    pub(crate) fn semantic_fixtures_for_test() -> Vec<Self> {
        let position =
            |transaction_index, hash, receipt_log_index, block_log_index| ObservedEventPosition {
                transaction_index,
                receipt_log_index,
                block_log_index,
                transaction_hash: B256::repeat_byte(hash),
            };
        vec![
            Self::DepositAppended(ObservedDepositAppend {
                position: position(1, 2, 3, 4),
                queue_hash: B256::repeat_byte(5),
                deposit_number: 6,
            }),
            Self::BatchSubmitted(ObservedSubmittedBatch {
                position: position(7, 8, 9, 10),
                withdrawal_batch_index: 11,
                withdrawal_queue_index: U256::from(12),
                next_processed_deposit_queue_hash: B256::repeat_byte(13),
                next_block_hash: B256::repeat_byte(14),
                withdrawal_queue_hash: B256::repeat_byte(15),
                last_processed_deposit_number: 16,
            }),
            Self::WithdrawalsProcessed(ObservedWithdrawalProcessing {
                transaction_index: 17,
                transaction_hash: B256::repeat_byte(18),
                members: vec![
                    ObservedProcessedWithdrawal::UserDelivered(ObservedUserWithdrawalDelivery {
                        callback_deposits: vec![ObservedDepositAppend {
                            position: position(19, 20, 21, 22),
                            queue_hash: B256::repeat_byte(23),
                            deposit_number: 24,
                        }],
                        processed: ObservedWithdrawalProcessed {
                            position: position(25, 26, 27, 28),
                            to: Address::repeat_byte(29),
                            sender_tag: B256::repeat_byte(30),
                            token: Address::repeat_byte(31),
                            amount: 32,
                            callback_success: true,
                        },
                    }),
                    ObservedProcessedWithdrawal::UserBounced(ObservedUserWithdrawalBounce {
                        append: ObservedWithdrawalBounceBackAppend {
                            position: position(33, 34, 35, 36),
                            queue_hash: B256::repeat_byte(37),
                            fallback_nonce: 38,
                            token: Address::repeat_byte(39),
                            amount: 40,
                            deposit_number: 41,
                        },
                        processed: ObservedWithdrawalProcessed {
                            position: position(42, 43, 44, 45),
                            to: Address::repeat_byte(46),
                            sender_tag: B256::repeat_byte(47),
                            token: Address::repeat_byte(48),
                            amount: 49,
                            callback_success: false,
                        },
                    }),
                    ObservedProcessedWithdrawal::FailedDepositPaid(ObservedDepositRefund {
                        position: position(50, 51, 52, 53),
                        recipient: Address::repeat_byte(54),
                        token: Address::repeat_byte(55),
                        amount: 56,
                        bounceback_fee: 57,
                    }),
                    ObservedProcessedWithdrawal::FailedDepositPending(ObservedDepositRefund {
                        position: position(58, 59, 60, 61),
                        recipient: Address::repeat_byte(62),
                        token: Address::repeat_byte(63),
                        amount: 64,
                        bounceback_fee: 65,
                    }),
                ],
            }),
            Self::RefundClaimed(ObservedRefundClaim {
                position: position(66, 67, 68, 69),
                recipient: Address::repeat_byte(70),
                token: Address::repeat_byte(71),
                amount: 72,
            }),
        ]
    }
}
