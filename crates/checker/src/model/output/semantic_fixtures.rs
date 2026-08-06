use std::num::NonZeroU64;

use alloy_primitives::{Address, B256, Bytes, U256};

use super::*;

impl ExpectedOutputs {
    pub(crate) fn semantic_fixture_for_test() -> Self {
        let mut imported = ExpectedImportedTempoBlock::default();
        imported.push_deposit_append(ExpectedDepositAppend::new(
            DepositId {
                portal: Address::repeat_byte(1),
                deposit_number: NonZeroU64::new(2).unwrap(),
            },
            B256::repeat_byte(3),
        ));
        imported.push_batch_submission(ExpectedBatchSubmission::new(
            BatchId {
                zone_id: 4,
                withdrawal_batch_index: NonZeroU64::new(5).unwrap(),
            },
            U256::from(6),
            B256::repeat_byte(7),
            B256::repeat_byte(8),
            B256::repeat_byte(9),
            10,
        ));
        imported
            .push_withdrawal_processing(ExpectedWithdrawalProcessing::semantic_fixture_for_test());
        imported.push_refund_claim(ExpectedRefundClaim::new(
            Address::repeat_byte(44),
            Address::repeat_byte(45),
            46,
        ));

        let failed_withdrawal = ExpectedWithdrawalRequested {
            withdrawal: WithdrawalId {
                zone_id: 56,
                withdrawal_index: 57,
            },
            sender: Address::repeat_byte(58),
            token: Address::repeat_byte(59),
            to: Address::repeat_byte(60),
            amount: 61,
            fee: 62,
            memo: B256::repeat_byte(63),
            gas_limit: 64,
            fallback_nonce: 65,
            data: Bytes::from_static(&[66, 67]),
            reveal_to: Bytes::from_static(&[68]),
        };
        let requested = ExpectedWithdrawalRequested {
            withdrawal: WithdrawalId {
                zone_id: 83,
                withdrawal_index: 84,
            },
            sender: Address::repeat_byte(85),
            token: Address::repeat_byte(86),
            to: Address::repeat_byte(87),
            amount: 88,
            fee: 89,
            memo: B256::repeat_byte(90),
            gas_limit: 91,
            fallback_nonce: 92,
            data: Bytes::from_static(&[93]),
            reveal_to: Bytes::from_static(&[94, 95]),
        };
        Self::new(
            imported,
            ExpectedZoneDepositPrefix::new(
                vec![ExpectedTokenEnable::new(
                    Address::repeat_byte(51),
                    "name",
                    "SYM",
                    "CUR",
                )],
                vec![
                    ExpectedDepositOutcome::OrdinaryMinted(ExpectedDepositProcessed::new(
                        B256::repeat_byte(52),
                        Address::repeat_byte(53),
                        Address::repeat_byte(54),
                        55,
                    )),
                    ExpectedDepositOutcome::OrdinaryFailed(Box::new(
                        ExpectedOrdinaryDepositFailure::new(
                            failed_withdrawal,
                            ExpectedDepositFailed {
                                deposit_hash: B256::repeat_byte(69),
                                sender: Address::repeat_byte(70),
                                token: Address::repeat_byte(71),
                                amount: 72,
                            },
                        ),
                    )),
                    ExpectedDepositOutcome::WithdrawalBounceBackMinted(
                        ExpectedWithdrawalBounceBack::new(Address::repeat_byte(73), 74),
                    ),
                    ExpectedDepositOutcome::WithdrawalBounceBackPending(
                        ExpectedWithdrawalBounceBack::new(Address::repeat_byte(75), 76),
                    ),
                ],
                4,
                ZoneProcessedDepositCursor::ZERO,
            ),
            ExpectedZoneBlock::new(
                vec![
                    ExpectedZoneOperation::WithdrawalRequested(Box::new(requested)),
                    ExpectedZoneOperation::RefundClaimed(ExpectedRefundClaim::new(
                        Address::repeat_byte(77),
                        Address::repeat_byte(78),
                        79,
                    )),
                ],
                Some(ExpectedBatchFinalized::new(
                    BatchId {
                        zone_id: 80,
                        withdrawal_batch_index: NonZeroU64::new(81).unwrap(),
                    },
                    B256::repeat_byte(82),
                )),
            ),
        )
    }
}
