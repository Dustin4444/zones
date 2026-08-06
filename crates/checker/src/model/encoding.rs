//! Checker-owned ABI preimages and queue folds.

use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256, Bytes, FixedBytes, U256, keccak256};
use alloy_sol_types::SolValue;

use super::constants::{
    AUTHENTICATED_WITHDRAWAL_SIZE, COMPRESSED_PUBLIC_KEY_SIZE, EMPTY_WITHDRAWAL_QUEUE_SENTINEL,
    ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE, MAX_CALLBACK_DATA_SIZE, MAX_WITHDRAWAL_GAS_LIMIT,
    ORDINARY_DEPOSIT_KIND, WITHDRAWAL_BOUNCE_BACK_DEPOSIT_KIND,
};

mod abi {
    alloy_sol_types::sol! {
        enum DepositType {
            WithdrawalBounceBack,
            Deposit
        }

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

        struct WithdrawalBounceBackDeposit {
            address token;
            address to;
            uint128 amount;
        }

        struct Withdrawal {
            address token;
            bytes32 senderTag;
            address to;
            uint128 amount;
            bytes32 memo;
            uint64 gasLimit;
            uint64 fallbackNonce;
            bytes callbackData;
            bytes encryptedSender;
        }
    }
}

/// Compressed secp256k1 public-key prefix accepted by the Portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum CompressedYParity {
    Even = 0x02,
    Odd = 0x03,
}

impl CompressedYParity {
    const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Encrypted payload embedded in an ordinary Portal deposit. Its fixed-size
/// ciphertext and compressed-key prefix cannot represent malformed values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DepositPayload {
    ephemeral_pubkey_x: B256,
    ephemeral_pubkey_y_parity: CompressedYParity,
    ciphertext: FixedBytes<ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE>,
    nonce: FixedBytes<12>,
    tag: FixedBytes<16>,
}

impl DepositPayload {
    pub(crate) const fn new(
        ephemeral_pubkey_x: B256,
        ephemeral_pubkey_y_parity: CompressedYParity,
        ciphertext: FixedBytes<ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE>,
        nonce: FixedBytes<12>,
        tag: FixedBytes<16>,
    ) -> Self {
        Self {
            ephemeral_pubkey_x,
            ephemeral_pubkey_y_parity,
            ciphertext,
            nonce,
            tag,
        }
    }
}

/// Full ordinary-deposit queue preimage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrdinaryDeposit {
    token: Address,
    sender: Address,
    amount: u128,
    tempo_refund_recipient: Address,
    key_index: U256,
    encrypted: DepositPayload,
}

impl OrdinaryDeposit {
    pub(crate) const fn new(
        token: Address,
        sender: Address,
        amount: u128,
        tempo_refund_recipient: Address,
        key_index: U256,
        encrypted: DepositPayload,
    ) -> Self {
        Self {
            token,
            sender,
            amount,
            tempo_refund_recipient,
            key_index,
            encrypted,
        }
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn sender(&self) -> Address {
        self.sender
    }

    pub(crate) const fn amount(&self) -> u128 {
        self.amount
    }

    pub(crate) const fn tempo_refund_recipient(&self) -> Address {
        self.tempo_refund_recipient
    }
}

/// Full withdrawal-bounce-back deposit queue preimage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WithdrawalBounceBackDeposit {
    token: Address,
    fallback_nonce: NonZeroU64,
    amount: NonZeroU128,
}

impl WithdrawalBounceBackDeposit {
    pub(crate) const fn new(
        token: Address,
        fallback_nonce: NonZeroU64,
        amount: NonZeroU128,
    ) -> Self {
        Self {
            token,
            fallback_nonce,
            amount,
        }
    }

    pub(crate) const fn fallback_nonce(&self) -> NonZeroU64 {
        self.fallback_nonce
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn amount(&self) -> NonZeroU128 {
        self.amount
    }

    /// Portal queue preimages encode the nonzero fallback nonce as an address.
    pub(crate) fn recipient(&self) -> Address {
        let mut bytes = [0_u8; 20];
        bytes[12..].copy_from_slice(&self.fallback_nonce.get().to_be_bytes());
        Address::from(bytes)
    }
}

/// A typed member of the unified Portal deposit queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DepositQueueMember {
    Ordinary(OrdinaryDeposit),
    WithdrawalBounceBack(WithdrawalBounceBackDeposit),
}

impl DepositQueueMember {
    /// Literal `abi.encode(deposit_type, deposit, previous_hash)` bytes.
    pub(crate) fn abi_preimage(&self, previous_hash: B256) -> Vec<u8> {
        match self {
            Self::Ordinary(deposit) => (
                abi::DepositType::try_from(ORDINARY_DEPOSIT_KIND)
                    .expect("literal ordinary-deposit discriminator"),
                abi::OrdinaryDeposit {
                    token: deposit.token,
                    sender: deposit.sender,
                    amount: deposit.amount,
                    tempoRefundRecipient: deposit.tempo_refund_recipient,
                    keyIndex: deposit.key_index,
                    encrypted: abi::DepositPayload {
                        ephemeralPubkeyX: deposit.encrypted.ephemeral_pubkey_x,
                        ephemeralPubkeyYParity: deposit.encrypted.ephemeral_pubkey_y_parity.as_u8(),
                        ciphertext: Bytes::copy_from_slice(deposit.encrypted.ciphertext.as_slice()),
                        nonce: deposit.encrypted.nonce,
                        tag: deposit.encrypted.tag,
                    },
                },
                previous_hash,
            )
                .abi_encode_params(),
            Self::WithdrawalBounceBack(deposit) => (
                abi::DepositType::try_from(WITHDRAWAL_BOUNCE_BACK_DEPOSIT_KIND)
                    .expect("literal withdrawal-bounce-back discriminator"),
                abi::WithdrawalBounceBackDeposit {
                    token: deposit.token,
                    to: deposit.recipient(),
                    amount: deposit.amount.get(),
                },
                previous_hash,
            )
                .abi_encode_params(),
        }
    }

    /// Append this member to a deposit hash chain.
    pub(crate) fn hash_after(&self, previous_hash: B256) -> B256 {
        keccak256(self.abi_preimage(previous_hash))
    }
}

/// Fold an ordered oldest-to-newest deposit prefix from `previous_hash`.
pub(crate) fn fold_deposit_prefix<'a>(
    previous_hash: B256,
    members: impl IntoIterator<Item = &'a DepositQueueMember>,
) -> B256 {
    members
        .into_iter()
        .fold(previous_hash, |tail, member| member.hash_after(tail))
}

/// Callback gas admitted by the pinned Outbox policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WithdrawalGasLimit(u64);

impl WithdrawalGasLimit {
    pub(crate) const fn new(gas_limit: u64) -> Result<Self, WithdrawalDataError> {
        if gas_limit > MAX_WITHDRAWAL_GAS_LIMIT {
            return Err(WithdrawalDataError::GasLimitTooHigh {
                actual: gas_limit,
                maximum: MAX_WITHDRAWAL_GAS_LIMIT,
            });
        }
        Ok(Self(gas_limit))
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Validated user callback payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallbackData(Bytes);

impl CallbackData {
    pub(crate) fn new(bytes: Bytes) -> Result<Self, WithdrawalDataError> {
        if bytes.len() > MAX_CALLBACK_DATA_SIZE {
            return Err(WithdrawalDataError::CallbackDataTooLong {
                actual: bytes.len(),
                maximum: MAX_CALLBACK_DATA_SIZE,
            });
        }
        Ok(Self(bytes))
    }

    pub(crate) fn empty() -> Self {
        Self(Bytes::new())
    }

    pub(crate) fn as_bytes(&self) -> &Bytes {
        &self.0
    }
}

/// Validated sender-reveal mode retained after request admission.
///
/// The checker needs only the mode after validating the supplied compressed
/// key shape; cryptographic key validity is an authenticated implementation
/// outcome and the raw key is not part of the withdrawal queue preimage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SenderReveal {
    None,
    Encrypted,
}

impl SenderReveal {
    pub(crate) fn from_reveal_to(bytes: &[u8]) -> Result<Self, WithdrawalDataError> {
        if bytes.is_empty() {
            return Ok(Self::None);
        }
        if bytes.len() != COMPRESSED_PUBLIC_KEY_SIZE {
            return Err(WithdrawalDataError::InvalidRevealToLength {
                actual: bytes.len(),
                expected: COMPRESSED_PUBLIC_KEY_SIZE,
            });
        }
        if !matches!(bytes[0], 0x02 | 0x03) {
            return Err(WithdrawalDataError::InvalidRevealToPrefix { actual: bytes[0] });
        }
        Ok(Self::Encrypted)
    }

    pub(crate) fn none() -> Self {
        Self::None
    }

    pub(crate) fn is_enabled(&self) -> bool {
        matches!(self, Self::Encrypted)
    }
}

/// Validated finalization bytes whose presence must match `revealTo`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EncryptedSender(Bytes);

impl EncryptedSender {
    fn for_reveal(bytes: Bytes, sender_reveal: SenderReveal) -> Result<Self, WithdrawalDataError> {
        let expected = if sender_reveal.is_enabled() {
            AUTHENTICATED_WITHDRAWAL_SIZE
        } else {
            0
        };
        if bytes.len() != expected {
            return Err(WithdrawalDataError::InvalidEncryptedSenderLength {
                actual: bytes.len(),
                expected,
            });
        }
        Ok(Self(bytes))
    }

    fn none() -> Self {
        Self(Bytes::new())
    }

    pub(crate) fn as_bytes(&self) -> &Bytes {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum WithdrawalDataError {
    #[error("withdrawal amount must be nonzero")]
    ZeroAmount,
    #[error("containing transaction hash must be nonzero")]
    ZeroTransactionHash,
    #[error("withdrawal gas limit {actual} exceeds {maximum}")]
    GasLimitTooHigh { actual: u64, maximum: u64 },
    #[error("callback data length {actual} exceeds {maximum}")]
    CallbackDataTooLong { actual: usize, maximum: usize },
    #[error("reveal-to key length {actual} is neither zero nor {expected}")]
    InvalidRevealToLength { actual: usize, expected: usize },
    #[error("reveal-to key prefix {actual:#04x} is not 0x02 or 0x03")]
    InvalidRevealToPrefix { actual: u8 },
    #[error("encrypted sender length {actual} does not match expected length {expected}")]
    InvalidEncryptedSenderLength { actual: usize, expected: usize },
}

/// Public identity inputs needed to derive a user sender tag and nonzero nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UserWithdrawalIdentity {
    sender: Address,
    tx_hash: B256,
    fallback_nonce: NonZeroU64,
}

impl UserWithdrawalIdentity {
    pub(crate) fn new(
        sender: Address,
        tx_hash: B256,
        fallback_nonce: NonZeroU64,
    ) -> Result<Self, WithdrawalDataError> {
        if tx_hash.is_zero() {
            return Err(WithdrawalDataError::ZeroTransactionHash);
        }
        Ok(Self {
            sender,
            tx_hash,
            fallback_nonce,
        })
    }

    pub(crate) const fn sender(&self) -> Address {
        self.sender
    }

    pub(crate) const fn tx_hash(&self) -> B256 {
        self.tx_hash
    }

    pub(crate) const fn fallback_nonce(&self) -> NonZeroU64 {
        self.fallback_nonce
    }
}

/// Validated fields shared by a pending and finalized user withdrawal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserWithdrawalRequest {
    token: Address,
    to: Address,
    amount: NonZeroU128,
    memo: B256,
    gas_limit: WithdrawalGasLimit,
    callback_data: CallbackData,
}

impl UserWithdrawalRequest {
    pub(crate) fn new(
        token: Address,
        to: Address,
        amount: u128,
        memo: B256,
        gas_limit: u64,
        callback_data: Bytes,
    ) -> Result<Self, WithdrawalDataError> {
        let amount = NonZeroU128::new(amount).ok_or(WithdrawalDataError::ZeroAmount)?;
        Ok(Self {
            token,
            to,
            amount,
            memo,
            gas_limit: WithdrawalGasLimit::new(gas_limit)?,
            callback_data: CallbackData::new(callback_data)?,
        })
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn principal(&self) -> NonZeroU128 {
        self.amount
    }

    pub(crate) const fn gas_limit(&self) -> WithdrawalGasLimit {
        self.gas_limit
    }
}

/// Full public withdrawal member committed by the Zone and Portal queues.
/// Fields are private so only the two release-one origins can construct one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Withdrawal {
    token: Address,
    sender_tag: B256,
    to: Address,
    amount: u128,
    memo: B256,
    gas_limit: u64,
    fallback_nonce: u64,
    callback_data: CallbackData,
    encrypted_sender: EncryptedSender,
}

impl Withdrawal {
    pub(crate) fn for_user(
        identity: UserWithdrawalIdentity,
        request: UserWithdrawalRequest,
        sender_reveal: SenderReveal,
        encrypted_sender: Bytes,
    ) -> Result<Self, WithdrawalDataError> {
        let encrypted_sender = EncryptedSender::for_reveal(encrypted_sender, sender_reveal)?;
        Ok(Self {
            token: request.token,
            sender_tag: sender_tag(identity.sender, identity.tx_hash),
            to: request.to,
            amount: request.amount.get(),
            memo: request.memo,
            gas_limit: request.gas_limit.get(),
            fallback_nonce: identity.fallback_nonce.get(),
            callback_data: request.callback_data,
            encrypted_sender,
        })
    }

    pub(crate) fn for_failed_deposit(token: Address, to: Address, amount: u128) -> Self {
        Self {
            token,
            sender_tag: sender_tag(Address::ZERO, B256::ZERO),
            to,
            amount,
            memo: B256::ZERO,
            gas_limit: 0,
            fallback_nonce: 0,
            callback_data: CallbackData::empty(),
            encrypted_sender: EncryptedSender::none(),
        }
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn to(&self) -> Address {
        self.to
    }

    pub(crate) const fn amount(&self) -> u128 {
        self.amount
    }

    pub(crate) const fn sender_tag(&self) -> B256 {
        self.sender_tag
    }

    pub(crate) const fn memo(&self) -> B256 {
        self.memo
    }

    pub(crate) const fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    pub(crate) const fn fallback_nonce(&self) -> u64 {
        self.fallback_nonce
    }

    pub(crate) fn callback_data(&self) -> &Bytes {
        self.callback_data.as_bytes()
    }

    pub(crate) fn encrypted_sender(&self) -> &Bytes {
        self.encrypted_sender.as_bytes()
    }

    /// Literal `abi.encode(withdrawal, tail)` bytes.
    pub(crate) fn abi_preimage(&self, tail: B256) -> Vec<u8> {
        (
            abi::Withdrawal {
                token: self.token,
                senderTag: self.sender_tag,
                to: self.to,
                amount: self.amount,
                memo: self.memo,
                gasLimit: self.gas_limit,
                fallbackNonce: self.fallback_nonce,
                callbackData: self.callback_data.0.clone(),
                encryptedSender: self.encrypted_sender.0.clone(),
            },
            tail,
        )
            .abi_encode_params()
    }

    pub(crate) fn hash_with_tail(&self, tail: B256) -> B256 {
        keccak256(self.abi_preimage(tail))
    }
}

/// Literal `keccak256(sender || containing_transaction_hash)` sender tag.
pub(crate) fn sender_tag(sender: Address, containing_transaction_hash: B256) -> B256 {
    let mut bytes = [0_u8; 52];
    bytes[..20].copy_from_slice(sender.as_slice());
    bytes[20..].copy_from_slice(containing_transaction_hash.as_slice());
    keccak256(bytes)
}

/// Fold newest-to-oldest so the oldest member is the outermost FIFO link.
/// Empty batches use zero, while non-empty folds start at the all-ones
/// sentinel.
pub(crate) fn withdrawal_queue_hash(withdrawals: &[Withdrawal]) -> B256 {
    if withdrawals.is_empty() {
        return B256::ZERO;
    }
    fold_withdrawals(EMPTY_WITHDRAWAL_QUEUE_SENTINEL, withdrawals)
}

fn fold_withdrawals(tail: B256, withdrawals: &[Withdrawal]) -> B256 {
    withdrawals
        .iter()
        .rev()
        .fold(tail, |hash, withdrawal| withdrawal.hash_with_tail(hash))
}

/// Pure result of checking one `processWithdrawals` queue prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessedWithdrawalQueue {
    /// Empty arrays are exact no-ops. No queue head is acquired or returned.
    Noop,
    /// A non-empty prefix was consumed and this suffix remains in the slot.
    Partial(B256),
    /// The complete slot was consumed; the caller advances the logical head.
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum WithdrawalQueueError {
    #[error("an empty withdrawal prefix must not acquire queue state")]
    EmptyPrefixUsesNoQueueState,
    #[error("the all-ones sentinel is not a valid caller-supplied suffix")]
    SentinelCannotBeSuppliedAsSuffix,
    #[error("withdrawal queue commitment mismatch: expected {expected}, got {actual}")]
    CommitmentMismatch { expected: B256, actual: B256 },
}

/// Model an empty `processWithdrawals` call without acquiring the queue head.
/// The caller-supplied suffix is deliberately ignored.
pub(crate) const fn process_empty_withdrawals(
    _arbitrary_remaining_queue: B256,
) -> ProcessedWithdrawalQueue {
    ProcessedWithdrawalQueue::Noop
}

/// Verify a non-empty FIFO prefix against the current slot commitment.
/// Empty calls must use [`process_empty_withdrawals`] before queue state is read.
pub(crate) fn process_nonempty_withdrawal_prefix(
    current_queue: B256,
    withdrawals: &[Withdrawal],
    remaining_queue: B256,
) -> Result<ProcessedWithdrawalQueue, WithdrawalQueueError> {
    if withdrawals.is_empty() {
        return Err(WithdrawalQueueError::EmptyPrefixUsesNoQueueState);
    }
    if remaining_queue == EMPTY_WITHDRAWAL_QUEUE_SENTINEL {
        return Err(WithdrawalQueueError::SentinelCannotBeSuppliedAsSuffix);
    }

    let tail = if remaining_queue.is_zero() {
        EMPTY_WITHDRAWAL_QUEUE_SENTINEL
    } else {
        remaining_queue
    };
    let expected = fold_withdrawals(tail, withdrawals);
    if expected != current_queue {
        return Err(WithdrawalQueueError::CommitmentMismatch {
            expected,
            actual: current_queue,
        });
    }

    if remaining_queue.is_zero() {
        Ok(ProcessedWithdrawalQueue::Exhausted)
    } else {
        Ok(ProcessedWithdrawalQueue::Partial(remaining_queue))
    }
}

#[cfg(test)]
mod tests {
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
}
