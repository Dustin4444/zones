use alloy_primitives::{B256, Bytes, keccak256};
use alloy_sol_types::SolValue as _;

use crate::facts::OrdinaryDeposit;

mod abi {
    alloy_sol_types::sol! {
        enum DepositType { WithdrawalBounceBack, Deposit }
        struct DepositPayload {
            bytes32 ephemeralPubkeyX;
            uint8 ephemeralPubkeyYParity;
            bytes ciphertext;
            bytes12 nonce;
            bytes16 tag;
        }
        struct OrdinaryDeposit {
            address token;
            address sender;
            uint128 amount;
            address tempoRefundRecipient;
            uint256 keyIndex;
            DepositPayload encrypted;
        }
    }
}

pub(crate) fn ordinary_deposit_hash(deposit: &OrdinaryDeposit, previous: B256) -> B256 {
    let wire = abi::OrdinaryDeposit {
        token: deposit.token,
        sender: deposit.sender,
        amount: deposit.amount,
        tempoRefundRecipient: deposit.tempo_refund_recipient,
        keyIndex: deposit.key_index,
        encrypted: abi::DepositPayload {
            ephemeralPubkeyX: deposit.encrypted.ephemeral_pubkey_x,
            ephemeralPubkeyYParity: deposit.encrypted.ephemeral_pubkey_y_parity,
            ciphertext: Bytes::copy_from_slice(deposit.encrypted.ciphertext.as_slice()),
            nonce: deposit.encrypted.nonce,
            tag: deposit.encrypted.tag,
        },
    };
    keccak256((abi::DepositType::Deposit, wire, previous).abi_encode_params())
}

pub(crate) fn portal_address(zone_id: u32) -> alloy_primitives::Address {
    const PREFIX: [u8; 12] = [
        0x5a, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let mut bytes = [0_u8; 20];
    bytes[..12].copy_from_slice(&PREFIX);
    bytes[12..].copy_from_slice(&u64::from(zone_id).to_be_bytes());
    alloy_primitives::Address::from(bytes)
}
