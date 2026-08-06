use alloy_primitives::{Address, B256, U256};

use crate::model::{
    ownership::DepositCursor,
    transition::{
        BlockTransitionMismatch, DepositTransitionMismatch, ModelError, WithdrawalOriginKind,
        WithdrawalProcessingOutcomeKind,
    },
};

use super::{Case, expected};

pub(super) fn cases() -> Vec<Case> {
    let first_address = Address::repeat_byte(1);
    let second_address = Address::repeat_byte(2);
    let first_hash = B256::repeat_byte(3);
    let second_hash = B256::repeat_byte(4);
    vec![
        (
            ModelError::WithdrawalBatchIndexOverflow,
            expected(0x21, |_| {}),
        ),
        (
            ModelError::BatchOwnerCollision {
                withdrawal_batch_index: 34,
            },
            expected(0x22, |bytes| bytes.u64(34)),
        ),
        (ModelError::PortalBatchIndexOverflow, expected(0x23, |_| {})),
        (
            ModelError::BatchOwnerMissing {
                withdrawal_batch_index: 36,
            },
            expected(0x24, |bytes| bytes.u64(36)),
        ),
        (
            ModelError::BatchAlreadySubmitted {
                withdrawal_batch_index: 37,
            },
            expected(0x25, |bytes| bytes.u64(37)),
        ),
        (
            ModelError::BatchTempoBlockMismatch {
                withdrawal_batch_index: 38,
                expected: 39,
                actual: 40,
            },
            expected(0x26, |bytes| {
                bytes.u64(38);
                bytes.u64(39);
                bytes.u64(40);
            }),
        ),
        (
            ModelError::BatchZoneHeightMismatch {
                withdrawal_batch_index: 41,
                expected: U256::from(42),
                actual: U256::from(43),
            },
            expected(0x27, |bytes| {
                bytes.u64(41);
                bytes.u256(U256::from(42));
                bytes.u256(U256::from(43));
            }),
        ),
        (
            ModelError::BatchBlockTransitionMismatch {
                withdrawal_batch_index: 44,
                details: Box::new(BlockTransitionMismatch {
                    expected_previous: B256::repeat_byte(45),
                    actual_previous: B256::repeat_byte(46),
                    expected_next: B256::repeat_byte(47),
                    actual_next: B256::repeat_byte(48),
                }),
            },
            expected(0x28, |bytes| {
                bytes.u64(44);
                bytes.hash(B256::repeat_byte(45));
                bytes.hash(B256::repeat_byte(46));
                bytes.hash(B256::repeat_byte(47));
                bytes.hash(B256::repeat_byte(48));
            }),
        ),
        (
            ModelError::BatchDepositTransitionMismatch {
                withdrawal_batch_index: 49,
                details: Box::new(DepositTransitionMismatch {
                    expected_previous: DepositCursor {
                        hash: B256::repeat_byte(50),
                        number: 51,
                    },
                    actual_previous: DepositCursor {
                        hash: B256::repeat_byte(52),
                        number: 53,
                    },
                    expected_next: DepositCursor {
                        hash: B256::repeat_byte(54),
                        number: 55,
                    },
                    actual_next: DepositCursor {
                        hash: B256::repeat_byte(56),
                        number: 57,
                    },
                }),
            },
            expected(0x29, |bytes| {
                bytes.u64(49);
                for (hash, number) in [(50, 51), (52, 53), (54, 55), (56, 57)] {
                    bytes.hash(B256::repeat_byte(hash));
                    bytes.u64(number);
                }
            }),
        ),
        (
            ModelError::BatchWithdrawalQueueHashMismatch {
                withdrawal_batch_index: 58,
                expected: first_hash,
                actual: second_hash,
            },
            expected(0x2a, |bytes| {
                bytes.u64(58);
                bytes.hash(first_hash);
                bytes.hash(second_hash);
            }),
        ),
        (
            ModelError::PortalBlockContinuityMismatch {
                withdrawal_batch_index: 59,
                expected: first_hash,
                actual: second_hash,
            },
            expected(0x2b, |bytes| {
                bytes.u64(59);
                bytes.hash(first_hash);
                bytes.hash(second_hash);
            }),
        ),
        (
            ModelError::PortalDepositContinuityMismatch {
                withdrawal_batch_index: 60,
                expected: DepositCursor {
                    hash: B256::repeat_byte(61),
                    number: 62,
                },
                actual: DepositCursor {
                    hash: B256::repeat_byte(63),
                    number: 64,
                },
            },
            expected(0x2c, |bytes| {
                bytes.u64(60);
                bytes.hash(B256::repeat_byte(61));
                bytes.u64(62);
                bytes.hash(B256::repeat_byte(63));
                bytes.u64(64);
            }),
        ),
        (
            ModelError::PortalZoneHeightNotIncreasing {
                withdrawal_batch_index: 65,
                previous: U256::from(66),
                next: U256::from(67),
            },
            expected(0x2d, |bytes| {
                bytes.u64(65);
                bytes.u256(U256::from(66));
                bytes.u256(U256::from(67));
            }),
        ),
        (
            ModelError::PortalDepositCursorBeyondQueue {
                withdrawal_batch_index: 68,
                submitted: 69,
                deposited: 70,
            },
            expected(0x2e, |bytes| {
                bytes.u64(68);
                bytes.u64(69);
                bytes.u64(70);
            }),
        ),
        (
            ModelError::InvalidPortalWithdrawalQueueProgress {
                head: U256::from(71),
                tail: U256::from(72),
            },
            expected(0x2f, |bytes| {
                bytes.u256(U256::from(71));
                bytes.u256(U256::from(72));
            }),
        ),
        (
            ModelError::PortalWithdrawalQueueFull,
            expected(0x30, |_| {}),
        ),
        (
            ModelError::PortalWithdrawalQueueCounterOverflow,
            expected(0x31, |_| {}),
        ),
        (
            ModelError::WithdrawalProcessingOutcomeCountMismatch {
                withdrawals: 75,
                outcomes: 76,
            },
            expected(0x32, |bytes| {
                bytes.usize(75);
                bytes.usize(76);
            }),
        ),
        (
            ModelError::PortalWithdrawalQueueEmpty,
            expected(0x33, |_| {}),
        ),
        (
            ModelError::PortalWithdrawalQueueHeadMissing,
            expected(0x34, |_| {}),
        ),
        (
            ModelError::PortalWithdrawalQueueHeadNotSubmitted,
            expected(0x35, |_| {}),
        ),
        (
            ModelError::PortalWithdrawalQueuePortalMismatch {
                expected: first_address,
                actual: second_address,
            },
            expected(0x36, |bytes| {
                bytes.address(first_address);
                bytes.address(second_address);
            }),
        ),
        (
            ModelError::PortalWithdrawalQueueHeadMismatch {
                expected: U256::from(79),
                actual: U256::from(80),
            },
            expected(0x37, |bytes| {
                bytes.u256(U256::from(79));
                bytes.u256(U256::from(80));
            }),
        ),
        (
            ModelError::WithdrawalProcessingLengthOverflow { actual: 81 },
            expected(0x38, |bytes| bytes.usize(81)),
        ),
        (
            ModelError::WithdrawalProcessingBeyondBatch {
                remaining: 82,
                actual: 83,
            },
            expected(0x39, |bytes| {
                bytes.u64(82);
                bytes.u64(83);
            }),
        ),
        (
            ModelError::WithdrawalProcessingExhaustedEarly,
            expected(0x3a, |_| {}),
        ),
        (
            ModelError::WithdrawalProcessingLeftSuffixAfterBatch,
            expected(0x3b, |_| {}),
        ),
        (
            ModelError::WithdrawalNotFinalizedForProcessing {
                withdrawal_index: 86,
            },
            expected(0x3c, |bytes| bytes.u64(86)),
        ),
        (
            ModelError::WithdrawalProcessingPreimageMismatch {
                withdrawal_index: 87,
            },
            expected(0x3d, |bytes| bytes.u64(87)),
        ),
        (
            ModelError::WithdrawalProcessingOutcomeMismatch {
                withdrawal_index: 88,
                expected: WithdrawalOriginKind::User,
                actual: WithdrawalProcessingOutcomeKind::UserDelivered,
            },
            expected(0x3e, |bytes| {
                bytes.u64(88);
                bytes.u8(1);
                bytes.u8(1);
            }),
        ),
        (
            ModelError::CallbackDepositsWithoutCallback {
                withdrawal_index: 89,
            },
            expected(0x3f, |bytes| bytes.u64(89)),
        ),
        (
            ModelError::PortalRefundCollision { deposit_number: 90 },
            expected(0x40, |bytes| bytes.u64(90)),
        ),
        (
            ModelError::RefundAggregateOverflow {
                token: first_address,
                recipient: second_address,
            },
            expected(0x41, |bytes| {
                bytes.address(first_address);
                bytes.address(second_address);
            }),
        ),
        (
            ModelError::RefundClaimAmountMismatch {
                token: first_address,
                recipient: second_address,
                expected: 92,
                actual: 93,
            },
            expected(0x44, |bytes| {
                bytes.address(first_address);
                bytes.address(second_address);
                bytes.u128(92);
                bytes.u128(93);
            }),
        ),
        (
            ModelError::InboxRefundCollision {
                withdrawal_index: 94,
            },
            expected(0x45, |bytes| bytes.u64(94)),
        ),
        (
            ModelError::ZeroBounceBackRecipient {
                withdrawal_index: 95,
            },
            expected(0x46, |bytes| bytes.u64(95)),
        ),
    ]
}
