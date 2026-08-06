use alloy_primitives::{Address, B256, Bytes, U256};

use super::*;

impl ObservedZoneOutputs {
    pub(crate) fn semantic_fixture_for_test() -> Self {
        let position = |transaction_index, hash, receipt_log_index, block_log_index| {
            ObservedZoneEventPosition {
                transaction_index,
                receipt_log_index,
                block_log_index,
                transaction_hash: B256::repeat_byte(hash),
            }
        };
        Self {
            tempo_block_finalized: ObservedTempoBlockFinalized {
                position: position(1, 2, 3, 4),
                block_hash: B256::repeat_byte(5),
                block_number: 6,
                state_root: B256::repeat_byte(7),
            },
            token_enables: vec![ObservedTokenEnabled {
                position: position(8, 9, 10, 11),
                token: Address::repeat_byte(12),
                name: "name".to_owned(),
                symbol: "SYM".to_owned(),
                currency: "CUR".to_owned(),
            }],
            deposit_outcomes: vec![
                ObservedDepositOutcome::OrdinaryMinted(ObservedDepositProcessed {
                    position: position(13, 14, 15, 16),
                    deposit_hash: B256::repeat_byte(17),
                    sender: Address::repeat_byte(18),
                    to: Address::repeat_byte(19),
                    token: Address::repeat_byte(20),
                    amount: 21,
                    memo: B256::repeat_byte(22),
                }),
                ObservedDepositOutcome::OrdinaryFailed {
                    withdrawal: Box::new(ObservedWithdrawalRequested {
                        position: position(23, 24, 25, 26),
                        withdrawal_index: 27,
                        sender: Address::repeat_byte(28),
                        token: Address::repeat_byte(29),
                        to: Address::repeat_byte(30),
                        amount: 31,
                        fee: 32,
                        memo: B256::repeat_byte(33),
                        gas_limit: 34,
                        fallback_nonce: 35,
                        data: Bytes::from_static(&[36, 37]),
                        reveal_to: Bytes::from_static(&[38]),
                    }),
                    failure: ObservedDepositFailed {
                        position: position(39, 40, 41, 42),
                        deposit_hash: B256::repeat_byte(43),
                        sender: Address::repeat_byte(44),
                        token: Address::repeat_byte(45),
                        amount: 46,
                    },
                },
                ObservedDepositOutcome::WithdrawalBounceBackMinted(
                    ObservedWithdrawalBounceBackProcessed {
                        position: position(47, 48, 49, 50),
                        zone_fallback_recipient: Address::repeat_byte(51),
                        token: Address::repeat_byte(52),
                        amount: 53,
                    },
                ),
                ObservedDepositOutcome::WithdrawalBounceBackPending(
                    ObservedWithdrawalBounceBackPending {
                        position: position(54, 55, 56, 57),
                        zone_fallback_recipient: Address::repeat_byte(58),
                        token: Address::repeat_byte(59),
                        amount: 60,
                    },
                ),
            ],
            tempo_advanced: ObservedTempoAdvanced {
                position: position(61, 62, 63, 64),
                tempo_block_hash: B256::repeat_byte(65),
                tempo_block_number: 66,
                deposits_processed: U256::from(67),
                new_processed_deposit_queue_hash: B256::repeat_byte(68),
                last_processed_deposit_number: 69,
            },
            operations: vec![
                ObservedZoneOperation::WithdrawalRequested(ObservedWithdrawalRequested {
                    position: position(70, 71, 72, 73),
                    withdrawal_index: 74,
                    sender: Address::repeat_byte(75),
                    token: Address::repeat_byte(76),
                    to: Address::repeat_byte(77),
                    amount: 78,
                    fee: 79,
                    memo: B256::repeat_byte(80),
                    gas_limit: 81,
                    fallback_nonce: 82,
                    data: Bytes::from_static(&[83]),
                    reveal_to: Bytes::from_static(&[84, 85]),
                }),
                ObservedZoneOperation::RefundClaimed(ObservedRefundClaimed {
                    position: position(86, 87, 88, 89),
                    recipient: Address::repeat_byte(90),
                    token: Address::repeat_byte(91),
                    amount: 92,
                }),
            ],
            batch_finalized: Some(ObservedBatchFinalized {
                position: position(93, 94, 95, 96),
                withdrawal_queue_hash: B256::repeat_byte(97),
                withdrawal_batch_index: 98,
            }),
        }
    }
}
