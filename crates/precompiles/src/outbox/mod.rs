//! Native `ZoneOutbox` precompile.
//!
//! Mirrors the Solidity ZoneOutbox predeploy at `0x1c00...0002` while keeping
//! the proof-facing storage slots compatible with the Solidity layout.

mod dispatch;

use alloc::vec::Vec;

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_sol_types::{SolCall, SolError, SolValue};
use revm::precompile::{PrecompileError, PrecompileResult};
use tempo_precompiles::{
    Result as TempoResult,
    error::TempoPrecompileError,
    storage::Handler,
    tip20::{ITIP20, TIP20Error, TIP20Token},
};
use tempo_precompiles_macros::{Storable, contract};
use tempo_zone_contracts::ZoneOutbox as ZoneOutboxAbi;
use zone_primitives::constants::{
    EMPTY_SENTINEL, MAX_WITHDRAWAL_GAS_LIMIT, PORTAL_SEQUENCER_SLOT, ZONE_INBOX_ADDRESS,
    ZONE_OUTBOX_ADDRESS,
};

use crate::{
    chaum_pedersen::recover_point,
    ecies::AUTHENTICATED_WITHDRAWAL_ENCRYPTED_SIZE,
    policy::PolicyCheck,
    tempo_state::{L1StorageReader, TempoState},
    tip403_proxy::ZoneTip403ProxyRegistry,
};

const MAX_CALLBACK_DATA_SIZE: usize = 1024;
const MAX_GAS_FEE_RATE: u128 = 1_000_000_000_000_000_000;
const WITHDRAWAL_BASE_GAS: u64 = 50_000;
const REVEAL_TO_KEY_LENGTH: usize = 33;

const PORTAL_TOKEN_CONFIGS_SLOT: B256 = {
    let mut bytes = [0u8; 32];
    bytes[31] = 8;
    B256::new(bytes)
};

alloy_sol_types::sol! {
    error StaticCallNotAllowed();
}

/// L1 portal state needed by the native outbox.
pub trait ZonePortalReader: L1StorageReader {
    /// Zone portal address on Tempo L1.
    fn portal_address(&self) -> Address;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Storable)]
struct LastBatchStorage {
    withdrawal_queue_hash: B256,
    withdrawal_batch_index: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Storable)]
struct PendingWithdrawalStorage {
    token: Address,
    sender: Address,
    tx_hash: B256,
    to: Address,
    amount: u128,
    fee: u128,
    memo: B256,
    gas_limit: u64,
    fallback_recipient: Address,
    callback_data: Bytes,
    reveal_to: Bytes,
}

#[contract(addr = ZONE_OUTBOX_ADDRESS)]
pub struct ZoneOutbox {
    // Slot 0: packed to match Solidity `(uint128,uint64,uint64)`.
    tempo_gas_rate: u128,
    next_withdrawal_index: u64,
    withdrawal_batch_index: u64,
    // Slots 1-2: proof code reads these directly.
    last_batch: LastBatchStorage,
    // Slot 3 onward: same dynamic-array layout as Solidity.
    pending_withdrawals: Vec<PendingWithdrawalStorage>,
    pending_withdrawals_head: U256,
    max_withdrawals_per_block: U256,
    withdrawals_this_block: U256,
    current_block_number: U256,
    last_finalized_timestamp: u64,
}

macro_rules! try_or_error {
    ($self:expr, $expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(err) => return $self.storage.error_result(err),
        }
    };
}

impl ZoneOutbox {
    /// Initializes the precompile account code.
    pub fn initialize(&mut self) -> TempoResult<()> {
        self.__initialize()
    }

    fn revert_error<E: SolError>(&self, error: E) -> PrecompileResult {
        Ok(self.storage.revert_output(error.abi_encode().into()))
    }

    fn success_empty(&self) -> PrecompileResult {
        Ok(self.storage.success_output(Bytes::new()))
    }

    fn static_revert(&self) -> Option<PrecompileResult> {
        if self.storage.is_static() {
            Some(self.revert_error(StaticCallNotAllowed {}))
        } else {
            None
        }
    }

    fn current_tempo_block_number(&self) -> TempoResult<u64> {
        TempoState::new().current_tempo_block_number()
    }

    fn portal_mapping_slot(token: Address) -> B256 {
        keccak256((token, PORTAL_TOKEN_CONFIGS_SLOT).abi_encode())
    }

    fn portal_storage<P: ZonePortalReader>(
        &self,
        provider: &P,
        slot: B256,
        tempo_block_number: u64,
    ) -> Result<B256, PrecompileError> {
        provider.read_l1_storage(provider.portal_address(), slot, tempo_block_number)
    }

    fn sequencer<P: ZonePortalReader>(
        &self,
        provider: &P,
        tempo_block_number: u64,
    ) -> Result<Address, PrecompileError> {
        let value = self.portal_storage(provider, PORTAL_SEQUENCER_SLOT, tempo_block_number)?;
        Ok(Address::from_slice(&value.as_slice()[12..]))
    }

    fn token_enabled<P: ZonePortalReader>(
        &self,
        provider: &P,
        tempo_block_number: u64,
        token: Address,
    ) -> Result<bool, PrecompileError> {
        let slot = Self::portal_mapping_slot(token);
        let value = self.portal_storage(provider, slot, tempo_block_number)?;
        Ok(value.as_slice()[31] != 0)
    }

    fn ensure_sequencer<P: ZonePortalReader>(
        &self,
        provider: &P,
        caller: Address,
        tempo_block_number: u64,
    ) -> PrecompileResult {
        if caller == Address::ZERO || caller == self.sequencer(provider, tempo_block_number)? {
            return self.success_empty();
        }
        self.revert_error(ZoneOutboxAbi::OnlySequencer {})
    }

    fn validate_gas_limit(&self, gas_limit: u64) -> Option<PrecompileResult> {
        if gas_limit > MAX_WITHDRAWAL_GAS_LIMIT {
            Some(self.revert_error(ZoneOutboxAbi::GasLimitTooHigh {}))
        } else {
            None
        }
    }

    fn calculate_fee_unchecked(&self, gas_limit: u64) -> TempoResult<u128> {
        let gas = u128::from(WITHDRAWAL_BASE_GAS)
            .checked_add(u128::from(gas_limit))
            .ok_or_else(TempoPrecompileError::under_overflow)?;
        gas.checked_mul(self.tempo_gas_rate.read()?)
            .ok_or_else(TempoPrecompileError::under_overflow)
    }

    fn calculate_withdrawal_fee(&self, gas_limit: u64) -> PrecompileResult {
        if let Some(revert) = self.validate_gas_limit(gas_limit) {
            return revert;
        }
        let fee = try_or_error!(self, self.calculate_fee_unchecked(gas_limit));
        Ok(self.storage.success_output(
            ZoneOutboxAbi::calculateWithdrawalFeeCall::abi_encode_returns(&fee).into(),
        ))
    }

    fn validate_reveal_to(&self, reveal_to: &[u8]) -> Option<PrecompileResult> {
        if reveal_to.is_empty() {
            return None;
        }
        if reveal_to.len() != REVEAL_TO_KEY_LENGTH {
            return Some(self.revert_error(ZoneOutboxAbi::InvalidRevealTo {}));
        }
        let y_parity = reveal_to[0];
        if !matches!(y_parity, 0x02 | 0x03) {
            return Some(self.revert_error(ZoneOutboxAbi::InvalidRevealTo {}));
        }
        let mut x = [0u8; 32];
        x.copy_from_slice(&reveal_to[1..]);
        if recover_point(&x, y_parity).is_none() {
            return Some(self.revert_error(ZoneOutboxAbi::InvalidRevealTo {}));
        }
        None
    }

    fn validate_encrypted_sender(
        &self,
        reveal_to: &[u8],
        encrypted_sender: &[u8],
    ) -> Option<PrecompileResult> {
        let expected = if reveal_to.is_empty() {
            0
        } else {
            AUTHENTICATED_WITHDRAWAL_ENCRYPTED_SIZE
        };
        if encrypted_sender.len() != expected {
            return Some(
                self.revert_error(ZoneOutboxAbi::InvalidEncryptedSenderLength {
                    actual: U256::from(encrypted_sender.len()),
                    expected: U256::from(expected),
                }),
            );
        }
        None
    }

    fn enforce_transfer_policy<P: PolicyCheck>(
        &self,
        registry: Option<&ZoneTip403ProxyRegistry<P>>,
        token: Address,
        from: Address,
        to: Address,
    ) -> TempoResult<()> {
        let Some(registry) = registry else {
            return Ok(());
        };
        let policy_id = registry.resolve_transfer_policy_id(token).map_err(|err| {
            TempoPrecompileError::Fatal(alloc::format!(
                "failed to resolve transfer policy for {token}: {err:?}"
            ))
        })?;
        let authorized = registry
            .is_transfer_authorized(policy_id, from, to)
            .map_err(|err| {
                TempoPrecompileError::Fatal(alloc::format!(
                    "failed to check transfer policy {policy_id}: {err:?}"
                ))
            })?;
        if authorized {
            Ok(())
        } else {
            Err(TIP20Error::policy_forbids().into())
        }
    }

    fn enforce_withdrawal_block_cap(&mut self) -> PrecompileResult {
        let max = try_or_error!(self, self.max_withdrawals_per_block.read());
        if max.is_zero() {
            return self.success_empty();
        }

        let block_number = U256::from(self.storage.block_number());
        let current_block = try_or_error!(self, self.current_block_number.read());
        if block_number != current_block {
            try_or_error!(self, self.current_block_number.write(block_number));
            try_or_error!(self, self.withdrawals_this_block.write(U256::ZERO));
        }

        let withdrawals = try_or_error!(self, self.withdrawals_this_block.read());
        if withdrawals >= max {
            return self.revert_error(ZoneOutboxAbi::TooManyWithdrawalsThisBlock {});
        }
        let next = withdrawals
            .checked_add(U256::ONE)
            .ok_or_else(TempoPrecompileError::under_overflow);
        try_or_error!(
            self,
            next.and_then(|value| self.withdrawals_this_block.write(value))
        );
        self.success_empty()
    }

    fn request_withdrawal<P: ZonePortalReader, Q: PolicyCheck>(
        &mut self,
        provider: &P,
        registry: Option<&ZoneTip403ProxyRegistry<Q>>,
        caller: Address,
        current_tx_hash: B256,
        args: RequestWithdrawalArgs,
    ) -> PrecompileResult {
        if let Some(revert) = self.static_revert() {
            return revert;
        }
        if args.fallback_recipient == Address::ZERO {
            return self.revert_error(ZoneOutboxAbi::InvalidFallbackRecipient {});
        }

        let tempo_block_number = try_or_error!(self, self.current_tempo_block_number());
        if !self.token_enabled(provider, tempo_block_number, args.token)? {
            return self.revert_error(ZoneOutboxAbi::TokenNotEnabled {});
        }
        if let Some(revert) = self.validate_gas_limit(args.gas_limit) {
            return revert;
        }
        if args.callback_data.len() > MAX_CALLBACK_DATA_SIZE {
            return self.revert_error(ZoneOutboxAbi::CallbackDataTooLarge {});
        }
        if let Some(revert) = self.validate_reveal_to(&args.reveal_to) {
            return revert;
        }
        let cap_check = self.enforce_withdrawal_block_cap()?;
        if cap_check.is_revert() {
            return Ok(cap_check);
        }

        let fee = try_or_error!(self, self.calculate_fee_unchecked(args.gas_limit));
        let total_burn = try_or_error!(
            self,
            args.amount
                .checked_add(fee)
                .ok_or_else(TempoPrecompileError::under_overflow)
        );
        if current_tx_hash.is_zero() {
            return self.revert_error(ZoneOutboxAbi::InvalidCurrentTxHash {});
        }

        try_or_error!(
            self,
            self.enforce_transfer_policy(registry, args.token, caller, ZONE_OUTBOX_ADDRESS)
        );

        let mut zone_token = try_or_error!(self, TIP20Token::from_address(args.token));
        let amount = U256::from(total_burn);
        let transferred = try_or_error!(
            self,
            zone_token.transfer_from(
                ZONE_OUTBOX_ADDRESS,
                ITIP20::transferFromCall {
                    from: caller,
                    to: ZONE_OUTBOX_ADDRESS,
                    amount,
                },
            )
        );
        if !transferred {
            return self.revert_error(ZoneOutboxAbi::TransferFailed {});
        }
        try_or_error!(
            self,
            zone_token.burn(ZONE_OUTBOX_ADDRESS, ITIP20::burnCall { amount })
        );

        try_or_error!(
            self,
            self.pending_withdrawals.push(PendingWithdrawalStorage {
                token: args.token,
                sender: caller,
                tx_hash: current_tx_hash,
                to: args.to,
                amount: args.amount,
                fee,
                memo: args.memo,
                gas_limit: args.gas_limit,
                fallback_recipient: args.fallback_recipient,
                callback_data: args.callback_data.clone(),
                reveal_to: args.reveal_to.clone(),
            })
        );

        let index = try_or_error!(self, self.next_withdrawal_index.read());
        let next_index = try_or_error!(
            self,
            index
                .checked_add(1)
                .ok_or_else(TempoPrecompileError::under_overflow)
        );
        try_or_error!(self, self.next_withdrawal_index.write(next_index));

        try_or_error!(
            self,
            self.emit_event(ZoneOutboxAbi::WithdrawalRequested {
                withdrawalIndex: index,
                sender: caller,
                token: args.token,
                to: args.to,
                amount: args.amount,
                fee,
                memo: args.memo,
                gasLimit: args.gas_limit,
                fallbackRecipient: args.fallback_recipient,
                data: args.callback_data,
                revealTo: args.reveal_to,
            })
        );

        self.success_empty()
    }

    fn enqueue_deposit_bounce_back(
        &mut self,
        caller: Address,
        call: ZoneOutboxAbi::enqueueDepositBounceBackCall,
    ) -> PrecompileResult {
        if let Some(revert) = self.static_revert() {
            return revert;
        }
        if caller != ZONE_INBOX_ADDRESS {
            return self.revert_error(ZoneOutboxAbi::OnlyZoneInbox {});
        }

        try_or_error!(
            self,
            self.pending_withdrawals.push(PendingWithdrawalStorage {
                token: call.token,
                sender: Address::ZERO,
                tx_hash: B256::ZERO,
                to: call.bouncebackRecipient,
                amount: call.amount,
                fee: 0,
                memo: B256::ZERO,
                gas_limit: 0,
                fallback_recipient: Address::ZERO,
                callback_data: Bytes::new(),
                reveal_to: Bytes::new(),
            })
        );

        let index = try_or_error!(self, self.next_withdrawal_index.read());
        let next_index = try_or_error!(
            self,
            index
                .checked_add(1)
                .ok_or_else(TempoPrecompileError::under_overflow)
        );
        try_or_error!(self, self.next_withdrawal_index.write(next_index));

        try_or_error!(
            self,
            self.emit_event(ZoneOutboxAbi::WithdrawalRequested {
                withdrawalIndex: index,
                sender: Address::ZERO,
                token: call.token,
                to: call.bouncebackRecipient,
                amount: call.amount,
                fee: 0,
                memo: B256::ZERO,
                gasLimit: 0,
                fallbackRecipient: Address::ZERO,
                data: Bytes::new(),
                revealTo: Bytes::new(),
            })
        );

        self.success_empty()
    }

    fn finalize_withdrawal_batch<P: ZonePortalReader>(
        &mut self,
        provider: &P,
        caller: Address,
        call: ZoneOutboxAbi::finalizeWithdrawalBatchCall,
    ) -> PrecompileResult {
        if let Some(revert) = self.static_revert() {
            return revert;
        }

        let tempo_block_number = try_or_error!(self, self.current_tempo_block_number());
        let sequencer_check = self.ensure_sequencer(provider, caller, tempo_block_number)?;
        if sequencer_check.is_revert() {
            return Ok(sequencer_check);
        }

        if call.blockNumber != self.storage.block_number() {
            return self.revert_error(ZoneOutboxAbi::InvalidBlockNumber {});
        }

        let len = try_or_error!(self, self.pending_withdrawals.len());
        let head_u256 = try_or_error!(self, self.pending_withdrawals_head.read());
        let head = try_or_error!(self, checked_usize(head_u256));
        let pending = len.saturating_sub(head);
        let count = try_or_error!(self, checked_usize(call.count));

        if count != pending {
            return self.revert_error(ZoneOutboxAbi::InvalidWithdrawalCount {
                actual: call.count,
                expected: U256::from(pending),
            });
        }
        if call.encryptedSenders.len() != count {
            return self.revert_error(ZoneOutboxAbi::InvalidEncryptedSenderCount {
                actual: U256::from(call.encryptedSenders.len()),
                expected: U256::from(count),
            });
        }

        let mut withdrawal_queue_hash = B256::ZERO;
        if count > 0 {
            withdrawal_queue_hash = EMPTY_SENTINEL;
            let start = head;
            let end = start + count;

            for i in (start..end).rev() {
                let pending_withdrawal = try_or_error!(self, self.pending_withdrawals[i].read());
                let encrypted_sender = call.encryptedSenders[i - start].clone();
                if let Some(revert) = self.validate_encrypted_sender(
                    &pending_withdrawal.reveal_to,
                    encrypted_sender.as_ref(),
                ) {
                    return revert;
                }

                let sender_tag = sender_tag(pending_withdrawal.sender, pending_withdrawal.tx_hash);
                let withdrawal = ZoneOutboxAbi::Withdrawal {
                    token: pending_withdrawal.token,
                    senderTag: sender_tag,
                    to: pending_withdrawal.to,
                    amount: pending_withdrawal.amount,
                    fee: pending_withdrawal.fee,
                    memo: pending_withdrawal.memo,
                    gasLimit: pending_withdrawal.gas_limit,
                    fallbackRecipient: pending_withdrawal.fallback_recipient,
                    callbackData: pending_withdrawal.callback_data,
                    encryptedSender: encrypted_sender,
                };
                withdrawal_queue_hash = keccak256((withdrawal, withdrawal_queue_hash).abi_encode());
                try_or_error!(self, self.pending_withdrawals[i].delete());
            }

            try_or_error!(self, self.pending_withdrawals_head.write(U256::from(end)));
            if end == len {
                try_or_error!(self, self.pending_withdrawals.delete());
                try_or_error!(self, self.pending_withdrawals_head.write(U256::ZERO));
            }
        }

        let current_batch_index = try_or_error!(self, self.withdrawal_batch_index.read());
        let next_batch_index = try_or_error!(
            self,
            current_batch_index
                .checked_add(1)
                .ok_or_else(TempoPrecompileError::under_overflow)
        );
        try_or_error!(self, self.withdrawal_batch_index.write(next_batch_index));
        try_or_error!(
            self,
            self.last_batch.write(LastBatchStorage {
                withdrawal_queue_hash,
                withdrawal_batch_index: next_batch_index,
            })
        );
        try_or_error!(
            self,
            self.last_finalized_timestamp
                .write(self.storage.timestamp().to::<u64>())
        );
        try_or_error!(
            self,
            self.emit_event(ZoneOutboxAbi::BatchFinalized {
                withdrawalQueueHash: withdrawal_queue_hash,
                withdrawalBatchIndex: next_batch_index,
            })
        );

        Ok(self.storage.success_output(
            ZoneOutboxAbi::finalizeWithdrawalBatchCall::abi_encode_returns(&withdrawal_queue_hash)
                .into(),
        ))
    }

    fn set_tempo_gas_rate<P: ZonePortalReader>(
        &mut self,
        provider: &P,
        caller: Address,
        call: ZoneOutboxAbi::setTempoGasRateCall,
    ) -> PrecompileResult {
        if let Some(revert) = self.static_revert() {
            return revert;
        }
        let tempo_block_number = try_or_error!(self, self.current_tempo_block_number());
        let sequencer_check = self.ensure_sequencer(provider, caller, tempo_block_number)?;
        if sequencer_check.is_revert() {
            return Ok(sequencer_check);
        }

        if call._tempoGasRate > MAX_GAS_FEE_RATE {
            return self.revert_error(ZoneOutboxAbi::GasFeeRateTooHigh {});
        }
        try_or_error!(self, self.tempo_gas_rate.write(call._tempoGasRate));
        try_or_error!(
            self,
            self.emit_event(ZoneOutboxAbi::TempoGasRateUpdated {
                tempoGasRate: call._tempoGasRate,
            })
        );
        self.success_empty()
    }

    fn set_max_withdrawals_per_block<P: ZonePortalReader>(
        &mut self,
        provider: &P,
        caller: Address,
        call: ZoneOutboxAbi::setMaxWithdrawalsPerBlockCall,
    ) -> PrecompileResult {
        if let Some(revert) = self.static_revert() {
            return revert;
        }
        let tempo_block_number = try_or_error!(self, self.current_tempo_block_number());
        let sequencer_check = self.ensure_sequencer(provider, caller, tempo_block_number)?;
        if sequencer_check.is_revert() {
            return Ok(sequencer_check);
        }

        try_or_error!(
            self,
            self.max_withdrawals_per_block
                .write(call._maxWithdrawalsPerBlock)
        );
        try_or_error!(
            self,
            self.emit_event(ZoneOutboxAbi::MaxWithdrawalsPerBlockUpdated {
                maxWithdrawalsPerBlock: call._maxWithdrawalsPerBlock,
            })
        );
        self.success_empty()
    }

    fn pending_withdrawals_count(&self) -> TempoResult<U256> {
        let len = self.pending_withdrawals.len()?;
        let head = checked_usize(self.pending_withdrawals_head.read()?)?;
        if head >= len {
            Ok(U256::ZERO)
        } else {
            Ok(U256::from(len - head))
        }
    }

    fn get_pending_withdrawals(&self) -> TempoResult<Vec<ZoneOutboxAbi::PendingWithdrawal>> {
        let len = self.pending_withdrawals.len()?;
        let head = checked_usize(self.pending_withdrawals_head.read()?)?;
        if head >= len {
            return Ok(Vec::new());
        }

        let mut pending = Vec::with_capacity(len - head);
        for index in head..len {
            pending.push(self.pending_withdrawals[index].read()?.into_abi());
        }
        Ok(pending)
    }

    fn last_batch(&self) -> TempoResult<ZoneOutboxAbi::LastBatch> {
        Ok(self.last_batch.read()?.into_abi())
    }
}

struct RequestWithdrawalArgs {
    token: Address,
    to: Address,
    amount: u128,
    memo: B256,
    gas_limit: u64,
    fallback_recipient: Address,
    callback_data: Bytes,
    reveal_to: Bytes,
}

impl LastBatchStorage {
    fn into_abi(self) -> ZoneOutboxAbi::LastBatch {
        ZoneOutboxAbi::LastBatch {
            withdrawalQueueHash: self.withdrawal_queue_hash,
            withdrawalBatchIndex: self.withdrawal_batch_index,
        }
    }
}

impl PendingWithdrawalStorage {
    fn into_abi(self) -> ZoneOutboxAbi::PendingWithdrawal {
        ZoneOutboxAbi::PendingWithdrawal {
            token: self.token,
            sender: self.sender,
            txHash: self.tx_hash,
            to: self.to,
            amount: self.amount,
            fee: self.fee,
            memo: self.memo,
            gasLimit: self.gas_limit,
            fallbackRecipient: self.fallback_recipient,
            callbackData: self.callback_data,
            revealTo: self.reveal_to,
        }
    }
}

fn checked_usize(value: U256) -> TempoResult<usize> {
    if value > U256::from(u32::MAX) {
        return Err(TempoPrecompileError::under_overflow());
    }
    Ok(value.to::<usize>())
}

fn sender_tag(sender: Address, tx_hash: B256) -> B256 {
    let mut preimage = [0u8; 52];
    preimage[..20].copy_from_slice(sender.as_slice());
    preimage[20..].copy_from_slice(tx_hash.as_slice());
    keccak256(preimage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_mapping_slot_matches_solidity_helper() {
        let token = Address::repeat_byte(0x20);
        assert_eq!(
            ZoneOutbox::portal_mapping_slot(token),
            keccak256((token, PORTAL_TOKEN_CONFIGS_SLOT).abi_encode())
        );
    }
}
