use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256};

use super::*;

impl ExpectedWithdrawalProcessing {
    pub(crate) fn semantic_fixture_for_test() -> Self {
        let delivered = ExpectedWithdrawalProcessed {
            withdrawal: WithdrawalId {
                zone_id: 14,
                withdrawal_index: 15,
            },
            to: Address::repeat_byte(16),
            sender_tag: B256::repeat_byte(17),
            token: Address::repeat_byte(18),
            amount: 19,
            callback_success: true,
        };
        let bounced = ExpectedWithdrawalProcessed {
            withdrawal: WithdrawalId {
                zone_id: 26,
                withdrawal_index: 27,
            },
            to: Address::repeat_byte(28),
            sender_tag: B256::repeat_byte(29),
            token: Address::repeat_byte(30),
            amount: 31,
            callback_success: false,
        };
        Self::new(vec![
            ExpectedProcessedWithdrawal::UserDelivered(Box::new(
                ExpectedUserWithdrawalDelivery::new(
                    vec![ExpectedDepositAppend::new(
                        DepositId {
                            portal: Address::repeat_byte(11),
                            deposit_number: NonZeroU64::new(12).unwrap(),
                        },
                        B256::repeat_byte(13),
                    )],
                    delivered,
                ),
            )),
            ExpectedProcessedWithdrawal::UserBounced(Box::new(ExpectedUserWithdrawalBounce::new(
                ExpectedWithdrawalBounceBackAppend::new(
                    WithdrawalBounceBackDeposit::new(
                        Address::repeat_byte(20),
                        NonZeroU64::new(21).unwrap(),
                        NonZeroU128::new(22).unwrap(),
                    ),
                    ExpectedDepositAppend::new(
                        DepositId {
                            portal: Address::repeat_byte(23),
                            deposit_number: NonZeroU64::new(24).unwrap(),
                        },
                        B256::repeat_byte(25),
                    ),
                ),
                bounced,
            ))),
            ExpectedProcessedWithdrawal::FailedDepositPaid(ExpectedDepositRefund::new(
                DepositId {
                    portal: Address::repeat_byte(32),
                    deposit_number: NonZeroU64::new(33).unwrap(),
                },
                Address::repeat_byte(34),
                Address::repeat_byte(35),
                36,
                37,
            )),
            ExpectedProcessedWithdrawal::FailedDepositPending(ExpectedDepositRefund::new(
                DepositId {
                    portal: Address::repeat_byte(38),
                    deposit_number: NonZeroU64::new(39).unwrap(),
                },
                Address::repeat_byte(40),
                Address::repeat_byte(41),
                42,
                43,
            )),
        ])
    }
}
