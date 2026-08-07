use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_sol_types::SolValue as _;

use crate::kernel::facts::{BounceBackDeposit, OrdinaryDeposit};
use crate::kernel::state::Withdrawal;

pub(crate) const WITHDRAWAL_SENTINEL: B256 = B256::repeat_byte(0xff);
pub(crate) const RING_CAPACITY: u64 = 100;
pub(crate) const NO_QUEUE_INDEX: U256 = U256::MAX;

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
        struct WithdrawalBounceBackDeposit { address token; address to; uint128 amount; }
        struct Withdrawal { address token; bytes32 senderTag; address to; uint128 amount; bytes32 memo;
            uint64 gasLimit; uint64 fallbackNonce; bytes callbackData; bytes encryptedSender; }
    }
}

pub(crate) fn sender_tag(sender: Address, transaction_hash: B256, fallback_nonce: u64) -> B256 {
    let mut value = [0u8; 60];
    value[..20].copy_from_slice(sender.as_slice());
    value[20..52].copy_from_slice(transaction_hash.as_slice());
    value[52..].copy_from_slice(&fallback_nonce.to_be_bytes());
    keccak256(value)
}

/// Failed-deposit withdrawals use the Portal's 52-byte zero preimage.
pub(crate) fn failed_deposit_sender_tag() -> B256 {
    keccak256([0u8; 52])
}

pub(crate) fn withdrawal_hash(value: &Withdrawal, tail: B256) -> B256 {
    let value = abi::Withdrawal {
        token: value.token,
        senderTag: value.sender_tag,
        to: value.to,
        amount: value.amount,
        memo: value.memo,
        gasLimit: value.gas_limit,
        fallbackNonce: value.fallback_nonce,
        callbackData: value.callback_data.clone(),
        encryptedSender: value.encrypted_sender.clone(),
    };
    keccak256((value, tail).abi_encode_params())
}

pub(crate) fn withdrawal_queue_hash(values: &[Withdrawal]) -> B256 {
    if values.is_empty() {
        return B256::ZERO;
    }
    values
        .iter()
        .rev()
        .fold(WITHDRAWAL_SENTINEL, |tail, value| {
            withdrawal_hash(value, tail)
        })
}

pub(crate) fn withdrawal_fee(gas_limit: u64, rate: u128) -> Option<u128> {
    u128::from(50_000u64)
        .checked_add(u128::from(gas_limit))?
        .checked_mul(rate)
}

pub(crate) fn bounceback_fee(gas: u64, base_fee: U256, amount: u128) -> Option<u128> {
    let scale = U256::from(1_000_000_000_000u64);
    let fee = U256::from(gas)
        .checked_mul(base_fee)?
        .checked_add(scale - U256::ONE)?
        / scale;
    Some(fee.min(U256::from(amount)).to::<u128>())
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

pub(crate) fn bounceback_deposit_hash(deposit: BounceBackDeposit, previous: B256) -> B256 {
    let mut recipient = [0_u8; 20];
    recipient[12..].copy_from_slice(&deposit.fallback_nonce.get().to_be_bytes());
    let wire = abi::WithdrawalBounceBackDeposit {
        token: deposit.token,
        to: Address::from(recipient),
        amount: deposit.amount,
    };
    keccak256((abi::DepositType::WithdrawalBounceBack, wire, previous).abi_encode_params())
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
