//! Native `ZoneInbox` precompile.
//!
//! Replaces the Solidity ZoneInbox predeploy at `0x1c00...0001` while
//! preserving the zone-facing ABI, storage layout, and event surface.

use alloc::vec::Vec;

use alloy_evm::precompiles::DynPrecompile;
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_sol_types::{SolCall, SolError, SolValue};
use revm::precompile::{PrecompileError, PrecompileId, PrecompileOutput, PrecompileResult};
use tempo_contracts::precompiles::TIP20Error;
use tempo_precompiles::{
    DelegateCallNotAllowed, charge_input_cost, dispatch,
    error::TempoPrecompileError,
    storage::{ContractStorage, Handler, Mapping, StorageCtx, evm::EvmPrecompileStorageProvider},
    tip20::{ITIP20, TIP20Token},
    view,
};
use tempo_precompiles_macros::{Storable, contract};
use tempo_zone_contracts::{
    TempoState as TempoStateAbi, ZoneInbox as ZoneInboxAbi, ZoneOutbox as ZoneOutboxAbi,
    ZonePortal as ZonePortalAbi,
};
use tracing::warn;
use zone_primitives::{
    constants::{
        PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT, PORTAL_ENCRYPTION_KEYS_SLOT, PORTAL_SEQUENCER_SLOT,
        TEMPO_STATE_ADDRESS, ZONE_CONFIG_ADDRESS, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS,
    },
    policy::AuthRole,
};

use crate::{
    aes_gcm::decrypt_aes_gcm,
    chaum_pedersen::verify_chaum_pedersen,
    ecies::{ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE, hkdf_info, hkdf_sha256},
    policy::PolicyCheck,
    tempo_state::{L1StorageReader, TempoState},
    tip20_factory::{ZoneTokenFactory, enableTokenCall},
    tip403_proxy::ZoneTip403ProxyRegistry,
};

alloy_sol_types::sol! {
    error StaticCallNotAllowed();
}

mod outbox_storage {
    use super::*;

    #[derive(Debug, Clone, Storable)]
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

    #[derive(Debug, Clone, Copy, Storable)]
    struct LastBatchStorage {
        withdrawal_queue_hash: B256,
        withdrawal_batch_index: u64,
    }

    #[contract(addr = ZONE_OUTBOX_ADDRESS)]
    struct ZoneOutboxStorage {
        tempo_gas_rate: u128,
        next_withdrawal_index: u64,
        withdrawal_batch_index: u64,
        last_batch: LastBatchStorage,
        pending_withdrawals: Vec<PendingWithdrawalStorage>,
        pending_withdrawals_head: U256,
        max_withdrawals_per_block: U256,
        withdrawals_this_block: U256,
        current_block_number: U256,
        last_finalized_timestamp: u64,
    }

    pub(super) fn enqueue_deposit_bounce_back(
        token: Address,
        amount: u128,
        bounceback_recipient: Address,
    ) -> Result<(), TempoPrecompileError> {
        let mut outbox = ZoneOutboxStorage::new();
        let withdrawal_index = outbox.next_withdrawal_index.read()?;
        outbox.pending_withdrawals.push(PendingWithdrawalStorage {
            token,
            sender: Address::ZERO,
            tx_hash: B256::ZERO,
            to: bounceback_recipient,
            amount,
            fee: 0,
            memo: B256::ZERO,
            gas_limit: 0,
            fallback_recipient: Address::ZERO,
            callback_data: Bytes::new(),
            reveal_to: Bytes::new(),
        })?;
        let next_index = withdrawal_index
            .checked_add(1)
            .ok_or_else(TempoPrecompileError::under_overflow)?;
        outbox.next_withdrawal_index.write(next_index)?;
        outbox.emit_event(ZoneOutboxAbi::WithdrawalRequested {
            withdrawalIndex: withdrawal_index,
            sender: Address::ZERO,
            token,
            to: bounceback_recipient,
            amount,
            fee: 0,
            memo: B256::ZERO,
            gasLimit: 0,
            fallbackRecipient: Address::ZERO,
            data: Bytes::new(),
            revealTo: Bytes::new(),
        })
    }
}

enum MintOutcome {
    Success,
    Reverted,
    SystemError(TempoPrecompileError),
}

enum DirectMintError {
    Tempo(TempoPrecompileError),
    Precompile(PrecompileError),
}

#[contract(addr = ZONE_INBOX_ADDRESS)]
pub struct ZoneInbox {
    processed_deposit_queue_hash: B256,
    processed_deposit_number: u64,
    refunds: Mapping<Address, Mapping<Address, u128>>,
}

impl ZoneInbox {
    /// Sets the contract bytecode marker so the precompile account is non-empty.
    pub fn initialize(&mut self) -> tempo_precompiles::Result<()> {
        self.__initialize()
    }

    fn revert_error<E: SolError>(&self, error: E) -> PrecompileResult {
        Ok(self.storage.revert_output(error.abi_encode().into()))
    }

    fn revert_empty(&self) -> PrecompileResult {
        Ok(self.storage.revert_output(Bytes::new()))
    }

    fn static_call_revert(&self) -> PrecompileResult {
        self.revert_error(StaticCallNotAllowed {})
    }

    fn success_empty(&self) -> PrecompileResult {
        Ok(self.storage.success_output(Bytes::new()))
    }

    fn current_tempo_block_number(&self) -> Result<u64, TempoPrecompileError> {
        TempoState::new().tempo_block_number()
    }

    fn read_l1_at_current_tempo<P: L1StorageReader>(
        &self,
        provider: &P,
        account: Address,
        slot: B256,
    ) -> Result<B256, PrecompileError> {
        let block_number = match self.current_tempo_block_number() {
            Ok(number) => number,
            Err(err) => return Err(PrecompileError::Fatal(err.to_string())),
        };
        provider.read_l1_storage(account, slot, block_number)
    }

    fn sequencer<P: L1StorageReader>(
        &self,
        provider: &P,
        tempo_portal: Address,
    ) -> Result<Address, PrecompileError> {
        let value = self.read_l1_at_current_tempo(provider, tempo_portal, PORTAL_SEQUENCER_SLOT)?;
        Ok(Address::from_slice(&value.as_slice()[12..]))
    }

    fn require_sequencer<P: L1StorageReader>(
        &self,
        provider: &P,
        tempo_portal: Address,
        msg_sender: Address,
    ) -> PrecompileResult {
        if msg_sender == Address::ZERO {
            return self.success_empty();
        }

        let sequencer = self.sequencer(provider, tempo_portal)?;
        if msg_sender != sequencer {
            return self.revert_error(ZoneInboxAbi::OnlySequencer {});
        }
        self.success_empty()
    }

    fn enable_token(
        &mut self,
        token: ZoneInboxAbi::EnabledToken,
    ) -> Result<(), TempoPrecompileError> {
        ZoneTokenFactory::new().enable_token(enableTokenCall {
            token: token.token,
            name: token.name.clone(),
            symbol: token.symbol.clone(),
            currency: token.currency.clone(),
        })?;
        self.emit_event(ZoneInboxAbi::TokenEnabled {
            token: token.token,
            name: token.name,
            symbol: token.symbol,
            currency: token.currency,
        })
    }

    fn enqueue_deposit_bounce_back(
        &mut self,
        token: Address,
        amount: u128,
        bounceback_recipient: Address,
    ) -> Result<(), TempoPrecompileError> {
        outbox_storage::enqueue_deposit_bounce_back(token, amount, bounceback_recipient)
    }

    fn reject_deposit(
        &mut self,
        current_hash: B256,
        deposit_type: ZoneInboxAbi::DepositType,
        sender: Address,
        token: Address,
        amount: u128,
        bounceback_recipient: Address,
    ) -> Result<(), TempoPrecompileError> {
        self.enqueue_deposit_bounce_back(token, amount, bounceback_recipient)?;
        self.emit_event(ZoneInboxAbi::DepositRejected {
            depositHash: current_hash,
            sender,
            depositType: deposit_type,
            token,
            amount,
            bouncebackRecipient: bounceback_recipient,
        })
    }

    fn fail_encrypted_deposit(
        &mut self,
        current_hash: B256,
        deposit: &ZonePortalAbi::EncryptedDeposit,
    ) -> Result<(), TempoPrecompileError> {
        self.enqueue_deposit_bounce_back(
            deposit.token,
            deposit.amount,
            deposit.bouncebackRecipient,
        )?;
        self.emit_event(ZoneInboxAbi::EncryptedDepositFailed {
            depositHash: current_hash,
            sender: deposit.sender,
            token: deposit.token,
            amount: deposit.amount,
        })
    }

    fn direct_mint<P: PolicyCheck>(
        registry: &Option<ZoneTip403ProxyRegistry<P>>,
        token: Address,
        to: Address,
        amount: u128,
    ) -> Result<(), DirectMintError> {
        if let Some(registry) = registry {
            let policy_id = match registry.resolve_transfer_policy_id(token) {
                Ok(policy_id) => Some(policy_id),
                Err(err) => {
                    warn!(
                        target: "zone::precompile",
                        %token,
                        error = %err,
                        "failed to resolve transfer_policy_id for ZoneInbox mint, deferring to L1 enforcement"
                    );
                    None
                }
            };
            if let Some(policy_id) = policy_id {
                match registry.is_authorized(policy_id, to, AuthRole::MintRecipient) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(DirectMintError::Tempo(TIP20Error::policy_forbids().into()));
                    }
                    Err(err) => return Err(DirectMintError::Precompile(err)),
                }
            }
        }

        let mut tip20 = TIP20Token::from_address(token).map_err(DirectMintError::Tempo)?;
        if !tip20.is_initialized().map_err(DirectMintError::Tempo)? {
            return Err(DirectMintError::Tempo(TIP20Error::uninitialized().into()));
        }
        tip20
            .mint(
                ZONE_INBOX_ADDRESS,
                ITIP20::mintCall {
                    to,
                    amount: U256::from(amount),
                },
            )
            .map_err(DirectMintError::Tempo)
    }

    fn try_mint<P: PolicyCheck>(
        &mut self,
        registry: &Option<ZoneTip403ProxyRegistry<P>>,
        token: Address,
        to: Address,
        amount: u128,
    ) -> Result<MintOutcome, PrecompileError> {
        let guard = self.storage.checkpoint();
        match Self::direct_mint(registry, token, to, amount) {
            Ok(()) => {
                guard.commit();
                Ok(MintOutcome::Success)
            }
            Err(DirectMintError::Tempo(err)) if err.is_system_error() => {
                Ok(MintOutcome::SystemError(err))
            }
            Err(DirectMintError::Tempo(_)) => Ok(MintOutcome::Reverted),
            Err(DirectMintError::Precompile(err)) => Err(err),
        }
    }

    fn process_withdrawal_bounce_back<P: PolicyCheck>(
        &mut self,
        registry: &Option<ZoneTip403ProxyRegistry<P>>,
        deposit: &ZoneInboxAbi::Deposit,
    ) -> PrecompileResult {
        match self.try_mint(registry, deposit.token, deposit.to, deposit.amount)? {
            MintOutcome::Success => {
                if let Err(err) = self.emit_event(ZoneInboxAbi::WithdrawalBounceBackProcessed {
                    fallbackRecipient: deposit.to,
                    token: deposit.token,
                    amount: deposit.amount,
                }) {
                    return self.storage.error_result(err);
                }
            }
            MintOutcome::Reverted => {
                let current_refund = match self.refunds[deposit.token][deposit.to].read() {
                    Ok(amount) => amount,
                    Err(err) => return self.storage.error_result(err),
                };
                let new_refund = match current_refund.checked_add(deposit.amount) {
                    Some(amount) => amount,
                    None => {
                        return self
                            .storage
                            .error_result(TempoPrecompileError::under_overflow());
                    }
                };
                if let Err(err) = self.refunds[deposit.token][deposit.to].write(new_refund) {
                    return self.storage.error_result(err);
                }
                if let Err(err) = self.emit_event(ZoneInboxAbi::WithdrawalBounceBackPending {
                    fallbackRecipient: deposit.to,
                    token: deposit.token,
                    amount: deposit.amount,
                }) {
                    return self.storage.error_result(err);
                }
            }
            MintOutcome::SystemError(err) => return self.storage.error_result(err),
        }
        self.success_empty()
    }

    fn encryption_key_slot(key_index: U256) -> Result<(B256, B256), TempoPrecompileError> {
        let base = U256::from_be_bytes(
            keccak256(U256::from_be_bytes(PORTAL_ENCRYPTION_KEYS_SLOT.0).to_be_bytes::<32>()).0,
        );
        let offset = key_index
            .checked_mul(U256::from(2))
            .ok_or_else(TempoPrecompileError::under_overflow)?;
        let slot_x = base
            .checked_add(offset)
            .ok_or_else(TempoPrecompileError::under_overflow)?;
        let slot_meta = slot_x
            .checked_add(U256::ONE)
            .ok_or_else(TempoPrecompileError::under_overflow)?;
        Ok((
            B256::from(slot_x.to_be_bytes::<32>()),
            B256::from(slot_meta.to_be_bytes::<32>()),
        ))
    }

    fn read_encryption_key<P: L1StorageReader>(
        &self,
        provider: &P,
        tempo_portal: Address,
        key_index: U256,
    ) -> Result<Option<(B256, u8)>, PrecompileError> {
        let (slot_x, slot_meta) = match Self::encryption_key_slot(key_index) {
            Ok(slots) => slots,
            Err(err) => return Err(PrecompileError::Fatal(err.to_string())),
        };
        let x = self.read_l1_at_current_tempo(provider, tempo_portal, slot_x)?;
        if x == B256::ZERO {
            return Ok(None);
        }
        let meta = self.read_l1_at_current_tempo(provider, tempo_portal, slot_meta)?;
        Ok(Some((x, meta.as_slice()[31])))
    }

    fn process_encrypted_deposit<
        P: L1StorageReader + Clone + Send + Sync + 'static,
        R: PolicyCheck,
    >(
        &mut self,
        provider: &P,
        registry: &Option<ZoneTip403ProxyRegistry<R>>,
        tempo_portal: Address,
        current_hash: B256,
        deposit: ZonePortalAbi::EncryptedDeposit,
        decryption: &ZoneInboxAbi::DecryptionData,
    ) -> PrecompileResult {
        let Some((sequencer_pub_x, sequencer_pub_y_parity)) =
            self.read_encryption_key(provider, tempo_portal, deposit.keyIndex)?
        else {
            return self.revert_error(ZoneInboxAbi::InvalidSharedSecretProof {});
        };

        let proof_valid = verify_chaum_pedersen(
            &deposit.encrypted.ephemeralPubkeyX.0,
            deposit.encrypted.ephemeralPubkeyYParity,
            &decryption.sharedSecret.0,
            decryption.sharedSecretYParity,
            &sequencer_pub_x.0,
            sequencer_pub_y_parity,
            &decryption.cpProof.s.0,
            &decryption.cpProof.c.0,
        );

        let mut valid = proof_valid;
        let mut plaintext = Vec::new();
        if valid {
            let info = hkdf_info(
                &tempo_portal,
                &deposit.keyIndex,
                &deposit.encrypted.ephemeralPubkeyX,
            );
            let aes_key = hkdf_sha256(&decryption.sharedSecret.0, b"ecies-aes-key", &info);
            let (decrypted, decrypted_valid) = decrypt_aes_gcm(
                &aes_key,
                &deposit.encrypted.nonce.0,
                deposit.encrypted.ciphertext.as_ref(),
                &[],
                &deposit.encrypted.tag.0,
            );
            plaintext = decrypted;
            valid = decrypted_valid;
        }

        if !valid || plaintext.len() != ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE {
            if let Err(err) = self.fail_encrypted_deposit(current_hash, &deposit) {
                return self.storage.error_result(err);
            }
            return self.success_empty();
        }

        let decrypted_to = Address::from_slice(&plaintext[..20]);
        let decrypted_memo = B256::from_slice(&plaintext[20..52]);
        match self.try_mint(registry, deposit.token, decrypted_to, deposit.amount)? {
            MintOutcome::Success => {
                if let Err(err) = self.emit_event(ZoneInboxAbi::EncryptedDepositProcessed {
                    depositHash: current_hash,
                    sender: deposit.sender,
                    to: decrypted_to,
                    token: deposit.token,
                    amount: deposit.amount,
                    memo: decrypted_memo,
                }) {
                    return self.storage.error_result(err);
                }
            }
            MintOutcome::Reverted => {
                if let Err(err) = self.fail_encrypted_deposit(current_hash, &deposit) {
                    return self.storage.error_result(err);
                }
            }
            MintOutcome::SystemError(err) => return self.storage.error_result(err),
        }
        self.success_empty()
    }

    fn advance_tempo<P, R>(
        &mut self,
        provider: &P,
        registry: &Option<ZoneTip403ProxyRegistry<R>>,
        tempo_portal: Address,
        msg_sender: Address,
        call: ZoneInboxAbi::advanceTempoCall,
    ) -> PrecompileResult
    where
        P: L1StorageReader + Clone + Send + Sync + 'static,
        R: PolicyCheck,
    {
        if self.storage.is_static() {
            return self.static_call_revert();
        }

        let output = self.require_sequencer(provider, tempo_portal, msg_sender)?;
        if !output.status.is_success() {
            return Ok(output);
        }

        let checkpoint = TempoState::new().apply_checkpoint(
            ZONE_INBOX_ADDRESS,
            TempoStateAbi::finalizeTempoCall {
                header: call.header,
            },
        )?;
        if !checkpoint.status.is_success() {
            return Ok(checkpoint);
        }

        for token in call.enabledTokens {
            if let Err(err) = self.enable_token(token) {
                return self.storage.error_result(err);
            }
        }

        let mut current_hash = match self.processed_deposit_queue_hash.read() {
            Ok(hash) => hash,
            Err(err) => return self.storage.error_result(err),
        };
        let mut decryption_index = 0usize;
        let deposits_processed = call.deposits.len();

        for queued in call.deposits {
            if queued.depositType == ZoneInboxAbi::DepositType::Regular {
                let deposit = match ZoneInboxAbi::Deposit::abi_decode(&queued.depositData) {
                    Ok(deposit) => deposit,
                    Err(_) => return self.revert_empty(),
                };
                current_hash = keccak256(
                    (
                        ZoneInboxAbi::DepositType::Regular,
                        deposit.clone(),
                        current_hash,
                    )
                        .abi_encode(),
                );

                if deposit.bouncebackRecipient == Address::ZERO {
                    let output = self.process_withdrawal_bounce_back(registry, &deposit)?;
                    if !output.status.is_success() {
                        return Ok(output);
                    }
                } else if queued.rejected {
                    if let Err(err) = self.reject_deposit(
                        current_hash,
                        ZoneInboxAbi::DepositType::Regular,
                        deposit.sender,
                        deposit.token,
                        deposit.amount,
                        deposit.bouncebackRecipient,
                    ) {
                        return self.storage.error_result(err);
                    }
                } else {
                    match self.try_mint(registry, deposit.token, deposit.to, deposit.amount)? {
                        MintOutcome::Success => {
                            if let Err(err) = self.emit_event(ZoneInboxAbi::DepositProcessed {
                                depositHash: current_hash,
                                sender: deposit.sender,
                                to: deposit.to,
                                token: deposit.token,
                                amount: deposit.amount,
                                memo: deposit.memo,
                            }) {
                                return self.storage.error_result(err);
                            }
                        }
                        MintOutcome::Reverted => {
                            if let Err(err) = self.enqueue_deposit_bounce_back(
                                deposit.token,
                                deposit.amount,
                                deposit.bouncebackRecipient,
                            ) {
                                return self.storage.error_result(err);
                            }
                            if let Err(err) = self.emit_event(ZoneInboxAbi::DepositFailed {
                                depositHash: current_hash,
                                sender: deposit.sender,
                                to: deposit.to,
                                token: deposit.token,
                                amount: deposit.amount,
                                bouncebackRecipient: deposit.bouncebackRecipient,
                            }) {
                                return self.storage.error_result(err);
                            }
                        }
                        MintOutcome::SystemError(err) => return self.storage.error_result(err),
                    }
                }
            } else {
                let deposit = match ZonePortalAbi::EncryptedDeposit::abi_decode(&queued.depositData)
                {
                    Ok(deposit) => deposit,
                    Err(_) => return self.revert_empty(),
                };
                current_hash = keccak256(
                    (
                        ZoneInboxAbi::DepositType::Encrypted,
                        deposit.clone(),
                        current_hash,
                    )
                        .abi_encode(),
                );

                if queued.rejected {
                    if let Err(err) = self.reject_deposit(
                        current_hash,
                        ZoneInboxAbi::DepositType::Encrypted,
                        deposit.sender,
                        deposit.token,
                        deposit.amount,
                        deposit.bouncebackRecipient,
                    ) {
                        return self.storage.error_result(err);
                    }
                    continue;
                }

                let Some(decryption) = call.decryptions.get(decryption_index) else {
                    return self.revert_error(ZoneInboxAbi::MissingDecryptionData {});
                };
                decryption_index += 1;

                let output = self.process_encrypted_deposit(
                    provider,
                    registry,
                    tempo_portal,
                    current_hash,
                    deposit,
                    decryption,
                )?;
                if !output.status.is_success() {
                    return Ok(output);
                }
            }
        }

        if decryption_index != call.decryptions.len() {
            return self.revert_error(ZoneInboxAbi::ExtraDecryptionData {});
        }

        self.read_l1_at_current_tempo(
            provider,
            tempo_portal,
            PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT,
        )?;

        if let Err(err) = self.processed_deposit_queue_hash.write(current_hash) {
            return self.storage.error_result(err);
        }
        let processed_delta = match u64::try_from(deposits_processed) {
            Ok(delta) => delta,
            Err(_) => {
                return self
                    .storage
                    .error_result(TempoPrecompileError::under_overflow());
            }
        };
        let processed_number = match self.processed_deposit_number.read() {
            Ok(number) => number,
            Err(err) => return self.storage.error_result(err),
        };
        let new_processed_number = match processed_number.checked_add(processed_delta) {
            Some(number) => number,
            None => {
                return self
                    .storage
                    .error_result(TempoPrecompileError::under_overflow());
            }
        };
        if let Err(err) = self.processed_deposit_number.write(new_processed_number) {
            return self.storage.error_result(err);
        }

        let tempo_state = TempoState::new();
        let tempo_block_hash = match tempo_state.tempo_block_hash() {
            Ok(hash) => hash,
            Err(err) => return self.storage.error_result(err),
        };
        let tempo_block_number = match tempo_state.tempo_block_number() {
            Ok(number) => number,
            Err(err) => return self.storage.error_result(err),
        };

        if let Err(err) = self.emit_event(ZoneInboxAbi::TempoAdvanced {
            tempoBlockHash: tempo_block_hash,
            tempoBlockNumber: tempo_block_number,
            depositsProcessed: U256::from(deposits_processed),
            newProcessedDepositQueueHash: current_hash,
            lastProcessedDepositNumber: new_processed_number,
        }) {
            return self.storage.error_result(err);
        }

        self.success_empty()
    }

    fn claim_refund<R: PolicyCheck>(
        &mut self,
        registry: &Option<ZoneTip403ProxyRegistry<R>>,
        msg_sender: Address,
        call: ZoneInboxAbi::claimRefundCall,
    ) -> PrecompileResult {
        if self.storage.is_static() {
            return self.static_call_revert();
        }

        let guard = self.storage.checkpoint();
        let amount = match self.refunds[call.token][msg_sender].read() {
            Ok(amount) => amount,
            Err(err) => return self.storage.error_result(err),
        };
        if let Err(err) = self.refunds[call.token][msg_sender].write(0) {
            return self.storage.error_result(err);
        }

        match Self::direct_mint(registry, call.token, msg_sender, amount) {
            Ok(()) => {}
            Err(DirectMintError::Tempo(err)) => return self.storage.error_result(err),
            Err(DirectMintError::Precompile(err)) => return Err(err),
        }

        if let Err(err) = self.emit_event(ZoneInboxAbi::RefundClaimed {
            recipient: msg_sender,
            token: call.token,
            amount,
        }) {
            return self.storage.error_result(err);
        }
        guard.commit();

        Ok(self
            .storage
            .success_output(ZoneInboxAbi::claimRefundCall::abi_encode_returns(&amount).into()))
    }

    /// Wraps this precompile for registration in the zone EVM.
    pub fn create<P, R>(
        tempo_portal: Address,
        provider: P,
        registry: Option<ZoneTip403ProxyRegistry<R>>,
        cfg: &revm::context::CfgEnv<tempo_chainspec::hardfork::TempoHardfork>,
    ) -> DynPrecompile
    where
        P: L1StorageReader + Clone + Send + Sync + 'static,
        R: PolicyCheck + Clone + Send + Sync + 'static,
    {
        let spec = cfg.spec;
        let amsterdam_eip8037_enabled = cfg.enable_amsterdam_eip8037;
        let gas_params = cfg.gas_params.clone();

        DynPrecompile::new_stateful(PrecompileId::Custom("ZoneInbox".into()), move |input| {
            if !input.is_direct_call() {
                return Ok(PrecompileOutput::revert(
                    0,
                    SolError::abi_encode(&DelegateCallNotAllowed {}).into(),
                    input.reservoir,
                ));
            }

            let mut storage = EvmPrecompileStorageProvider::new(
                input.internals,
                input.gas,
                input.reservoir,
                spec,
                amsterdam_eip8037_enabled,
                input.is_static,
                gas_params.clone(),
            );

            StorageCtx::enter(&mut storage, || {
                Self::new().call_with_provider(
                    &provider,
                    &registry,
                    tempo_portal,
                    input.data,
                    input.caller,
                )
            })
        })
    }

    fn call_with_provider<P, R>(
        &mut self,
        provider: &P,
        registry: &Option<ZoneTip403ProxyRegistry<R>>,
        tempo_portal: Address,
        calldata: &[u8],
        msg_sender: Address,
    ) -> PrecompileResult
    where
        P: L1StorageReader + Clone + Send + Sync + 'static,
        R: PolicyCheck,
    {
        if let Some(err) = charge_input_cost(&mut self.storage, calldata) {
            return err;
        }

        dispatch!(
            calldata,
            |call| match call {
                ZoneInboxAbi::ZoneInboxCalls {
                    processedDepositQueueHash(call) => {
                        view(call, |_| self.processed_deposit_queue_hash.read())
                    },
                    processedDepositNumber(call) => {
                        view(call, |_| self.processed_deposit_number.read())
                    },
                    tempoPortal(call) => view(call, |_| Ok(tempo_portal)),
                    tempoState(call) => view(call, |_| Ok(TEMPO_STATE_ADDRESS)),
                    config(call) => view(call, |_| Ok(ZONE_CONFIG_ADDRESS)),
                    refunds(call) => {
                        view(call, |call| self.refunds[call.token][call.owner].read())
                    },
                    claimRefund(call) => self.claim_refund(registry, msg_sender, call),
                    advanceTempo(call) => {
                        self.advance_tempo(provider, registry, tempo_portal, msg_sender, call)
                    },
                }
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::address;
    use alloy_rlp::Encodable as _;
    use alloy_sol_types::SolCall;
    use revm::precompile::PrecompileResult;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };
    use tempo_contracts::precompiles::ITIP403Registry;
    use tempo_precompiles::{
        storage::{StorageCtx, hashmap::HashMapStorageProvider},
        tip20::{ITIP20, TIP20Token},
    };
    use tempo_primitives::TempoHeader;
    use zone_primitives::policy::AuthRole;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[derive(Clone, Default)]
    struct MockL1Reader {
        values: Arc<Mutex<HashMap<(Address, B256, u64), B256>>>,
        reads: Arc<Mutex<Vec<(Address, B256, u64)>>>,
    }

    impl MockL1Reader {
        fn set(&self, account: Address, slot: B256, block_number: u64, value: B256) {
            self.values
                .lock()
                .expect("values lock")
                .insert((account, slot, block_number), value);
        }

        fn reads(&self) -> Vec<(Address, B256, u64)> {
            self.reads.lock().expect("reads lock").clone()
        }
    }

    impl L1StorageReader for MockL1Reader {
        fn read_l1_storage(
            &self,
            account: Address,
            slot: B256,
            block_number: u64,
        ) -> Result<B256, PrecompileError> {
            self.reads
                .lock()
                .expect("reads lock")
                .push((account, slot, block_number));
            Ok(self
                .values
                .lock()
                .expect("values lock")
                .get(&(account, slot, block_number))
                .copied()
                .unwrap_or(B256::ZERO))
        }
    }

    #[derive(Clone, Default)]
    struct MockPolicy;

    impl PolicyCheck for MockPolicy {
        fn is_authorized(
            &self,
            _policy_id: u64,
            _user: Address,
            _role: AuthRole,
        ) -> Result<bool, PrecompileError> {
            Ok(true)
        }

        fn resolve_transfer_policy_id(&self, _token: Address) -> Result<u64, PrecompileError> {
            Ok(1)
        }

        fn policy_type_sync(
            &self,
            _policy_id: u64,
        ) -> Result<ITIP403Registry::PolicyType, PrecompileError> {
            Ok(ITIP403Registry::PolicyType::BLACKLIST)
        }

        fn compound_policy_data(
            &self,
            _policy_id: u64,
        ) -> Result<(u64, u64, u64), PrecompileError> {
            Ok((1, 1, 1))
        }

        fn policy_exists(&self, _policy_id: u64) -> Result<bool, PrecompileError> {
            Ok(true)
        }

        fn policy_id_counter(&self) -> u64 {
            1
        }
    }

    fn encode_header(header: &TempoHeader) -> Bytes {
        let mut encoded = Vec::new();
        header.encode(&mut encoded);
        encoded.into()
    }

    fn child_header(parent_hash: B256, number: u64) -> TempoHeader {
        TempoHeader {
            inner: alloy_consensus::Header {
                parent_hash,
                number,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn test_storage() -> HashMapStorageProvider {
        HashMapStorageProvider::new(1)
    }

    fn initialize(storage: &mut HashMapStorageProvider, header_rlp: &[u8]) -> TestResult {
        StorageCtx::enter(storage, || -> tempo_precompiles::Result<()> {
            TempoState::new().initialize(header_rlp)?;
            ZoneInbox::new().initialize()?;
            Ok(())
        })?;
        Ok(())
    }

    fn call(
        storage: &mut HashMapStorageProvider,
        l1: &MockL1Reader,
        tempo_portal: Address,
        msg_sender: Address,
        calldata: Bytes,
    ) -> PrecompileResult {
        let registry: Option<ZoneTip403ProxyRegistry<MockPolicy>> = None;
        StorageCtx::enter(storage, || {
            ZoneInbox::new().call_with_provider(l1, &registry, tempo_portal, &calldata, msg_sender)
        })
    }

    fn child_advance_calldata(
        genesis_rlp: &[u8],
        deposits: Vec<ZoneInboxAbi::QueuedDeposit>,
        enabled_tokens: Vec<ZoneInboxAbi::EnabledToken>,
    ) -> Bytes {
        let child_rlp = encode_header(&child_header(keccak256(genesis_rlp), 1));
        ZoneInboxAbi::advanceTempoCall {
            header: child_rlp,
            deposits,
            decryptions: Vec::new(),
            enabledTokens: enabled_tokens,
        }
        .abi_encode()
        .into()
    }

    fn processed_number(storage: &mut HashMapStorageProvider) -> TestResult<u64> {
        Ok(StorageCtx::enter(storage, || {
            ZoneInbox::new().processed_deposit_number.read()
        })?)
    }

    fn processed_hash(storage: &mut HashMapStorageProvider) -> TestResult<B256> {
        Ok(StorageCtx::enter(storage, || {
            ZoneInbox::new().processed_deposit_queue_hash.read()
        })?)
    }

    fn refund(
        storage: &mut HashMapStorageProvider,
        token: Address,
        owner: Address,
    ) -> TestResult<u128> {
        Ok(StorageCtx::enter(storage, || {
            ZoneInbox::new().refunds[token][owner].read()
        })?)
    }

    fn balance_of(
        storage: &mut HashMapStorageProvider,
        token: Address,
        account: Address,
    ) -> TestResult<U256> {
        Ok(StorageCtx::enter(storage, || {
            TIP20Token::from_address(token)?.balance_of(ITIP20::balanceOfCall { account })
        })?)
    }

    #[test]
    fn view_accessors_return_native_constants() -> TestResult {
        let genesis_rlp = encode_header(&TempoHeader::default());
        let mut storage = test_storage();
        initialize(&mut storage, &genesis_rlp)?;

        let l1 = MockL1Reader::default();
        let tempo_portal = Address::repeat_byte(0x11);

        let portal = call(
            &mut storage,
            &l1,
            tempo_portal,
            Address::ZERO,
            ZoneInboxAbi::tempoPortalCall {}.abi_encode().into(),
        )?;
        assert!(portal.is_success());
        assert_eq!(
            ZoneInboxAbi::tempoPortalCall::abi_decode_returns(&portal.bytes)?,
            tempo_portal
        );

        let tempo_state = call(
            &mut storage,
            &l1,
            tempo_portal,
            Address::ZERO,
            ZoneInboxAbi::tempoStateCall {}.abi_encode().into(),
        )?;
        assert_eq!(
            ZoneInboxAbi::tempoStateCall::abi_decode_returns(&tempo_state.bytes)?,
            TEMPO_STATE_ADDRESS
        );

        let config = call(
            &mut storage,
            &l1,
            tempo_portal,
            Address::ZERO,
            ZoneInboxAbi::configCall {}.abi_encode().into(),
        )?;
        assert_eq!(
            ZoneInboxAbi::configCall::abi_decode_returns(&config.bytes)?,
            ZONE_CONFIG_ADDRESS
        );

        Ok(())
    }

    #[test]
    fn advance_tempo_enables_token_and_processes_regular_deposit() -> TestResult {
        let genesis_rlp = encode_header(&TempoHeader::default());
        let mut storage = test_storage();
        initialize(&mut storage, &genesis_rlp)?;

        let l1 = MockL1Reader::default();
        let tempo_portal = Address::repeat_byte(0x22);
        l1.set(
            tempo_portal,
            PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT,
            1,
            B256::ZERO,
        );

        let token = address!("0x20C0000000000000000000000000000000001234");
        let sender = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);
        let bounceback_recipient = Address::repeat_byte(0x55);
        let deposit = ZoneInboxAbi::Deposit {
            token,
            sender,
            to: recipient,
            amount: 123,
            bouncebackRecipient: bounceback_recipient,
            memo: B256::repeat_byte(0x66),
        };
        let queued = ZoneInboxAbi::QueuedDeposit {
            depositType: ZoneInboxAbi::DepositType::Regular,
            depositData: deposit.abi_encode().into(),
            rejected: false,
        };
        let enabled_token = ZoneInboxAbi::EnabledToken {
            token,
            name: "Zone Test".into(),
            symbol: "ZTST".into(),
            currency: "USD".into(),
        };

        let output = call(
            &mut storage,
            &l1,
            tempo_portal,
            Address::ZERO,
            child_advance_calldata(&genesis_rlp, vec![queued], vec![enabled_token]),
        )?;
        assert!(output.is_success());

        let expected_hash =
            keccak256((ZoneInboxAbi::DepositType::Regular, deposit, B256::ZERO).abi_encode());
        assert_eq!(processed_hash(&mut storage)?, expected_hash);
        assert_eq!(processed_number(&mut storage)?, 1);
        assert_eq!(
            balance_of(&mut storage, token, recipient)?,
            U256::from(123u64)
        );
        assert!(
            l1.reads()
                .contains(&(tempo_portal, PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT, 1))
        );

        Ok(())
    }

    #[test]
    fn withdrawal_bounceback_parks_refund_when_mint_reverts() -> TestResult {
        let genesis_rlp = encode_header(&TempoHeader::default());
        let mut storage = test_storage();
        initialize(&mut storage, &genesis_rlp)?;

        let l1 = MockL1Reader::default();
        let tempo_portal = Address::repeat_byte(0x77);
        l1.set(
            tempo_portal,
            PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT,
            1,
            B256::ZERO,
        );

        let token = address!("0x20C0000000000000000000000000000000005678");
        let fallback_recipient = Address::repeat_byte(0x88);
        let deposit = ZoneInboxAbi::Deposit {
            token,
            sender: Address::repeat_byte(0x99),
            to: fallback_recipient,
            amount: 456,
            bouncebackRecipient: Address::ZERO,
            memo: B256::ZERO,
        };
        let queued = ZoneInboxAbi::QueuedDeposit {
            depositType: ZoneInboxAbi::DepositType::Regular,
            depositData: deposit.abi_encode().into(),
            rejected: true,
        };

        let output = call(
            &mut storage,
            &l1,
            tempo_portal,
            Address::ZERO,
            child_advance_calldata(&genesis_rlp, vec![queued], Vec::new()),
        )?;
        assert!(output.is_success());

        assert_eq!(processed_number(&mut storage)?, 1);
        assert_eq!(refund(&mut storage, token, fallback_recipient)?, 456);

        Ok(())
    }
}
