use alloy_primitives::Address;

use crate::model::{
    TokenEnable,
    state::PortalIdentity,
    transition::{DepositKind, DepositOutcomeKind, ModelError},
};

use super::{Case, expected};

pub(super) fn cases() -> Vec<Case> {
    let first = Address::repeat_byte(1);
    let second = Address::repeat_byte(2);
    let third = Address::repeat_byte(3);
    vec![
        (ModelError::PortalNotCreated, expected(0x01, |_| {})),
        (ModelError::PortalAlreadyCreated, expected(0x02, |_| {})),
        (
            ModelError::PortalIdentityMismatch {
                expected: PortalIdentity::new(first, 4, second),
                actual: PortalIdentity::new(second, 5, third),
            },
            expected(0x03, |bytes| {
                bytes.address(first);
                bytes.u32(4);
                bytes.address(second);
                bytes.address(second);
                bytes.u32(5);
                bytes.address(third);
            }),
        ),
        (
            ModelError::PortalAddressMismatch {
                expected: first,
                actual: second,
            },
            expected(0x04, |bytes| {
                bytes.address(first);
                bytes.address(second);
            }),
        ),
        (
            ModelError::InitialTokenMismatch {
                expected: second,
                actual: third,
            },
            expected(0x05, |bytes| {
                bytes.address(second);
                bytes.address(third);
            }),
        ),
        (
            ModelError::TokenAlreadyEnabled { token: first },
            expected(0x06, |bytes| bytes.address(first)),
        ),
        (
            ModelError::TokenNotPortalEnabled { token: second },
            expected(0x07, |bytes| bytes.address(second)),
        ),
        (
            ModelError::TokenNotZoneEnabled { token: third },
            expected(0x08, |bytes| bytes.address(third)),
        ),
        (ModelError::ZeroTempoRefundRecipient, expected(0x09, |_| {})),
        (
            ModelError::ZoneTokenEnableCountMismatch {
                expected: 10,
                actual: 11,
            },
            expected(0x0a, |bytes| {
                bytes.usize(10);
                bytes.usize(11);
            }),
        ),
        (
            ModelError::ZoneTokenEnableMismatch {
                index: 12,
                expected: Box::new(TokenEnable::for_test(first, "first", "F", "ONE")),
                actual: Box::new(TokenEnable::for_test(second, "second", "S", "TWO")),
            },
            expected(0x0b, |bytes| {
                bytes.usize(12);
                bytes.address(first);
                bytes.bytes(b"first");
                bytes.bytes(b"F");
                bytes.bytes(b"ONE");
                bytes.address(second);
                bytes.bytes(b"second");
                bytes.bytes(b"S");
                bytes.bytes(b"TWO");
            }),
        ),
        (
            ModelError::PortalDepositNumberOverflow,
            expected(0x0c, |_| {}),
        ),
        (
            ModelError::DepositOwnerCollision { number: 13 },
            expected(0x0d, |bytes| bytes.u64(13)),
        ),
        (
            ModelError::FallbackOwnerMissing { fallback_nonce: 14 },
            expected(0x0e, |bytes| bytes.u64(14)),
        ),
        (
            ModelError::FallbackOwnerMismatch { fallback_nonce: 15 },
            expected(0x0f, |bytes| bytes.u64(15)),
        ),
        (
            ModelError::WithdrawalBounceBackAlreadyPending {
                withdrawal_index: 16,
            },
            expected(0x10, |bytes| bytes.u64(16)),
        ),
        (
            ModelError::DepositOutcomeCountMismatch {
                deposits: 17,
                outcomes: 18,
            },
            expected(0x11, |bytes| {
                bytes.usize(17);
                bytes.usize(18);
            }),
        ),
        (
            ModelError::ProcessedDepositNumberOverflow,
            expected(0x12, |_| {}),
        ),
        (
            ModelError::PendingDepositMissing { number: 19 },
            expected(0x13, |bytes| bytes.u64(19)),
        ),
        (
            ModelError::DepositPrefixMismatch { number: 20 },
            expected(0x14, |bytes| bytes.u64(20)),
        ),
        (
            ModelError::DepositOutcomeKindMismatch {
                number: 21,
                expected: DepositKind::Ordinary,
                actual: DepositOutcomeKind::OrdinaryMinted,
            },
            expected(0x15, |bytes| {
                bytes.u64(21);
                bytes.u8(1);
                bytes.u8(1);
            }),
        ),
        (ModelError::WithdrawalIndexOverflow, expected(0x16, |_| {})),
        (
            ModelError::WithdrawalOwnerCollision {
                withdrawal_index: 23,
            },
            expected(0x17, |bytes| bytes.u64(23)),
        ),
        (
            ModelError::WithdrawalBlockCapExceeded { limit: 24 },
            expected(0x18, |bytes| bytes.u32(24)),
        ),
        (ModelError::FallbackNonceOverflow, expected(0x19, |_| {})),
        (
            ModelError::FallbackOwnerCollision { fallback_nonce: 26 },
            expected(0x1a, |bytes| bytes.u64(26)),
        ),
        (
            ModelError::FinalizationBlockNumberMismatch {
                expected: 27,
                actual: 28,
            },
            expected(0x1b, |bytes| {
                bytes.u64(27);
                bytes.u64(28);
            }),
        ),
        (
            ModelError::FinalizationCountMismatch {
                expected: 29,
                actual: 30,
            },
            expected(0x1c, |bytes| {
                bytes.u64(29);
                bytes.usize(30);
            }),
        ),
        (
            ModelError::FinalizationSenderCountMismatch {
                declared: 31,
                actual: 32,
            },
            expected(0x1d, |bytes| {
                bytes.usize(31);
                bytes.usize(32);
            }),
        ),
        (
            ModelError::InvalidBatchWithdrawalRange {
                first: 33,
                next: 34,
            },
            expected(0x1e, |bytes| {
                bytes.u64(33);
                bytes.u64(34);
            }),
        ),
        (
            ModelError::WithdrawalOwnerMissing {
                withdrawal_index: 35,
            },
            expected(0x1f, |bytes| bytes.u64(35)),
        ),
        (
            ModelError::WithdrawalAlreadyFinalized {
                withdrawal_index: 36,
            },
            expected(0x20, |bytes| bytes.u64(36)),
        ),
    ]
}
