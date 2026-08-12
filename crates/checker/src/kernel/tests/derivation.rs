//! Derivation tests.

use super::*;

#[test]
fn ordinary_deposit_commitment_matches_literal_vector() {
    assert_eq!(
        ordinary_deposit_hash(&deposit(), B256::ZERO),
        b256!("89982eeee3ca64954daa0322b331f17efd85a433564bfdb4938c0ab087663a5d")
    );
}

#[test]
fn sender_tag_matches_literal_vector_and_includes_fallback_nonce() {
    let sender = Address::repeat_byte(0x11);
    let transaction = B256::repeat_byte(0x22);
    assert_eq!(
        crate::kernel::derivation::sender_tag(sender, transaction, 0x0102_0304_0506_0708),
        b256!("09e5aae3d74dbb09f2046a3a15c5504ce844113049b83c2884ca41a43124acbf")
    );
    assert_ne!(
        crate::kernel::derivation::sender_tag(sender, transaction, 1),
        crate::kernel::derivation::sender_tag(sender, transaction, 2)
    );
    assert_eq!(
        crate::kernel::derivation::failed_deposit_sender_tag(),
        alloy_primitives::keccak256([0u8; 52])
    );
}
