use alloy_primitives::{address, b256, bytes, fixed_bytes};

use super::*;
use crate::model::test_vectors::{
    BOUNCE_BACK_DEPOSIT_PREIMAGE, ORDINARY_DEPOSIT_ONE_PREIMAGE, WITHDRAWAL_FAILED_PREIMAGE,
    WITHDRAWAL_ONE_PREIMAGE, literal_bytes,
};

fn ordinary_one() -> DepositQueueMember {
    DepositQueueMember::Ordinary(OrdinaryDeposit::new(
        address!("1111111111111111111111111111111111111111"),
        address!("2222222222222222222222222222222222222222"),
        1_000,
        address!("3333333333333333333333333333333333333333"),
        U256::from(7),
        DepositPayload::new(
            b256!("4444444444444444444444444444444444444444444444444444444444444444"),
            CompressedYParity::Even,
            fixed_bytes!(
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f"
            ),
            fixed_bytes!("555555555555555555555555"),
            fixed_bytes!("66666666666666666666666666666666"),
        ),
    ))
}

fn ordinary_two() -> DepositQueueMember {
    DepositQueueMember::Ordinary(OrdinaryDeposit::new(
        address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        7,
        address!("cccccccccccccccccccccccccccccccccccccccc"),
        U256::from(8),
        DepositPayload::new(
            b256!("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"),
            CompressedYParity::Odd,
            FixedBytes::repeat_byte(0xff),
            fixed_bytes!("eeeeeeeeeeeeeeeeeeeeeeee"),
            fixed_bytes!("99999999999999999999999999999999"),
        ),
    ))
}

fn bounce_back_deposit() -> DepositQueueMember {
    DepositQueueMember::WithdrawalBounceBack(WithdrawalBounceBackDeposit::new(
        address!("1234567890123456789012345678901234567890"),
        NonZeroU64::new(7).unwrap(),
        NonZeroU128::new(500).unwrap(),
    ))
}

fn withdrawals() -> [Withdrawal; 3] {
    [
        Withdrawal::for_user(
            UserWithdrawalIdentity::new(
                address!("7777777777777777777777777777777777777777"),
                b256!("8888888888888888888888888888888888888888888888888888888888888888"),
                NonZeroU64::new(1).unwrap(),
            )
            .unwrap(),
            UserWithdrawalRequest::new(
                address!("1111111111111111111111111111111111111111"),
                address!("2222222222222222222222222222222222222222"),
                100,
                b256!("3333333333333333333333333333333333333333333333333333333333333333"),
                0,
                Bytes::new(),
            )
            .unwrap(),
            SenderReveal::none(),
            Bytes::new(),
        )
        .unwrap(),
        Withdrawal::for_user(
            UserWithdrawalIdentity::new(
                address!("5555555555555555555555555555555555555555"),
                b256!("6666666666666666666666666666666666666666666666666666666666666666"),
                NonZeroU64::new(2).unwrap(),
            )
            .unwrap(),
            UserWithdrawalRequest::new(
                address!("4444444444444444444444444444444444444444"),
                address!("7777777777777777777777777777777777777777"),
                200,
                b256!("8888888888888888888888888888888888888888888888888888888888888888"),
                1_234,
                bytes!("deadbeef"),
            )
            .unwrap(),
            SenderReveal::none(),
            Bytes::new(),
        )
        .unwrap(),
        Withdrawal::for_failed_deposit(
            address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            9,
        ),
    ]
}

// Fixed literal hashes below are populated from independent `cast abi-encode`
// and `cast keccak` invocations documented in MODEL_VECTORS.md.

#[test]
fn model_ordinary_deposit_payload_has_fixed_release_one_shape() {
    let DepositQueueMember::Ordinary(deposit) = ordinary_one() else {
        unreachable!()
    };
    assert_eq!(
        deposit.encrypted.ciphertext.len(),
        ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE
    );
    assert_eq!(
        deposit.encrypted.ephemeral_pubkey_y_parity,
        CompressedYParity::Even
    );
}

#[test]
fn model_ordinary_deposit_append_and_multi_item_prefix_vectors() {
    let members = [ordinary_one(), ordinary_two()];
    let after_one = members[0].hash_after(B256::ZERO);
    let after_two = fold_deposit_prefix(B256::ZERO, &members);

    assert_eq!(
        members[0].abi_preimage(B256::ZERO),
        literal_bytes(ORDINARY_DEPOSIT_ONE_PREIMAGE)
    );
    assert_eq!(
        after_one,
        b256!("89982eeee3ca64954daa0322b331f17efd85a433564bfdb4938c0ab087663a5d")
    );
    assert_eq!(
        after_two,
        b256!("1c5c0c09978e9f50b319cbb91fe92fafa252fe6375cd0689ed1dd31ce7880fee")
    );
}

#[test]
fn model_withdrawal_bounce_back_deposit_vector() {
    assert_eq!(
        bounce_back_deposit().abi_preimage(B256::ZERO),
        literal_bytes(BOUNCE_BACK_DEPOSIT_PREIMAGE)
    );
    assert_eq!(
        bounce_back_deposit().hash_after(B256::ZERO),
        b256!("737e7e554cef04e8a45184e9162cde4450971ee8b09fbccb2658a22a58611808")
    );
}

#[test]
fn model_sender_tag_vectors_include_special_zero_identity() {
    assert_eq!(
        sender_tag(
            address!("7777777777777777777777777777777777777777"),
            b256!("8888888888888888888888888888888888888888888888888888888888888888")
        ),
        b256!("977ca7d7170498bf6675510cf2e40c11a6e5683f702bb46e206064af26a505a3")
    );
    assert_eq!(
        sender_tag(Address::ZERO, B256::ZERO),
        b256!("a86d54e9aab41ae5e520ff0062ff1b4cbd0b2192bb01080a058bb170d84e6457")
    );
}

#[test]
fn model_withdrawal_queue_empty_partial_and_full_vectors() {
    let withdrawals = withdrawals();
    let full = withdrawal_queue_hash(&withdrawals);
    let after_one = withdrawal_queue_hash(&withdrawals[1..]);
    let after_two = withdrawal_queue_hash(&withdrawals[2..]);

    assert_eq!(withdrawal_queue_hash(&[]), B256::ZERO);
    assert_eq!(
        withdrawals[0].abi_preimage(after_one),
        literal_bytes(WITHDRAWAL_ONE_PREIMAGE)
    );
    assert_eq!(
        full,
        b256!("ea645b2419e3da96758eb0dbacc08e41d1c744b0cd8adb09405d6056e91f1753")
    );
    assert_eq!(
        after_one,
        b256!("9749a08b6e690a932830e1d29974cb9b7b1f7145cc006bbb2c42c426fe335c81")
    );
    assert_eq!(
        after_two,
        b256!("ac67cdf55db79608ba1e80bcf7ee9f623774def8252331e67102ad4dc683f910")
    );

    assert_eq!(
        process_nonempty_withdrawal_prefix(full, &withdrawals[..1], after_one),
        Ok(ProcessedWithdrawalQueue::Partial(after_one))
    );
    assert_eq!(
        process_nonempty_withdrawal_prefix(after_one, &withdrawals[1..2], after_two),
        Ok(ProcessedWithdrawalQueue::Partial(after_two))
    );
    assert_eq!(
        process_nonempty_withdrawal_prefix(after_two, &withdrawals[2..], B256::ZERO),
        Ok(ProcessedWithdrawalQueue::Exhausted)
    );
    assert_eq!(
        process_nonempty_withdrawal_prefix(full, &withdrawals, B256::ZERO),
        Ok(ProcessedWithdrawalQueue::Exhausted)
    );
}

#[test]
fn model_empty_process_withdrawals_is_noop_with_arbitrary_suffix() {
    let current = b256!("abababababababababababababababababababababababababababababababab");
    for ignored_suffix in [
        B256::ZERO,
        B256::repeat_byte(0xcd),
        EMPTY_WITHDRAWAL_QUEUE_SENTINEL,
    ] {
        assert_eq!(
            process_empty_withdrawals(ignored_suffix),
            ProcessedWithdrawalQueue::Noop
        );
    }
    assert_eq!(
        process_nonempty_withdrawal_prefix(current, &[], B256::ZERO),
        Err(WithdrawalQueueError::EmptyPrefixUsesNoQueueState)
    );
    assert_eq!(
        process_nonempty_withdrawal_prefix(
            EMPTY_WITHDRAWAL_QUEUE_SENTINEL,
            &withdrawals()[..1],
            B256::ZERO,
        ),
        Err(WithdrawalQueueError::SentinelCannotBeCurrentQueue)
    );
}

#[test]
fn model_failed_deposit_withdrawal_has_zero_public_identity_vector() {
    let failed = &withdrawals()[2];
    assert_eq!(failed.sender_tag(), sender_tag(Address::ZERO, B256::ZERO));
    assert_eq!(failed.gas_limit(), 0);
    assert_eq!(failed.fallback_nonce(), 0);
    assert!(failed.callback_data().is_empty());
    assert!(failed.encrypted_sender().is_empty());
    assert_eq!(
        failed.abi_preimage(EMPTY_WITHDRAWAL_QUEUE_SENTINEL),
        literal_bytes(WITHDRAWAL_FAILED_PREIMAGE)
    );
    assert_eq!(
        failed.hash_with_tail(EMPTY_WITHDRAWAL_QUEUE_SENTINEL),
        b256!("ac67cdf55db79608ba1e80bcf7ee9f623774def8252331e67102ad4dc683f910")
    );
}
