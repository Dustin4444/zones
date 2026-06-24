//! Zone-specific TIP-20 token precompile with PolicyCheck-backed authorization.
//!
//! On L1, the vanilla [`TIP20Token`] checks transfer/mint authorization by
//! instantiating a `TIP403Registry` in Rust which reads EVM storage at
//! `0x403C…0000`. On the zone, that storage is empty (defaults to policy 1 =
//! allow-all), so all transfers pass regardless of L1 blacklists.
//!
//! This wrapper intercepts transfer and mint calls, checks authorization
//! against the zone's [`ZoneTip403ProxyRegistry`] (which delegates to
//! [`PolicyCheck`] — cache-first, L1 RPC fallback), and only then delegates
//! to the vanilla `TIP20Token` implementation.

use alloc::{string::String, sync::Arc};

use alloy_evm::precompiles::DynPrecompile;
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_sol_types::{SolCall, SolError, SolInterface, SolValue};
use revm::precompile::{
    PrecompileError, PrecompileHalt, PrecompileId, PrecompileOutput, PrecompileResult,
};
use tempo_contracts::precompiles::{
    IReceivePolicyGuard, ITIP403Registry::BlockedReason, ReceivePolicyGuardError,
};
use tempo_precompiles::{
    DelegateCallNotAllowed, Precompile as TempoPrecompile, RECEIVE_POLICY_GUARD_ADDRESS,
    StaticCallNotAllowed,
    address_registry::AddressRegistry,
    storage::{ContractStorage, Handler, Mapping, StorageCtx, evm::EvmPrecompileStorageProvider},
    tip20::{
        IRolesAuth, ISSUER_ROLE, ITIP20, RolesAuthError, TIP20Error, TIP20Event, TIP20Token,
        is_tip20_prefix, rewards::UserRewardInfo,
    },
};
use tempo_precompiles_macros::contract;
use tempo_zone_contracts::Unauthorized;
use tracing::{trace, warn};
use zone_primitives::{
    constants::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS},
    policy::AuthRole,
};

use crate::{
    policy::{PolicyCheck, ReceivePolicyDecision},
    tip403_proxy::{AUTH_CHECK_GAS, ZoneTip403ProxyRegistry},
};

const FIXED_TRANSFER_GAS: u64 = 100_000;

/// Decode ABI args or return a reverted precompile output.
///
/// Unlike `.ok()?` (which silently skips the policy check on decode failure),
/// this macro returns a definitive revert so malformed calldata cannot bypass
/// the zone policy layer.
macro_rules! decode_or_revert {
    ($call_ty:ty, $args:expr) => {
        match <$call_ty>::abi_decode_raw($args) {
            Ok(c) => c,
            Err(_) => {
                return Some(Ok(StorageCtx::default().revert_output(Bytes::new())));
            }
        }
    };
}

/// Convert token/precompile business errors into normal ABI reverts.
macro_rules! token_or_revert {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(err) => return StorageCtx::default().error_result(err),
        }
    };
}

/// Recipient resolved through the Tempo address registry.
#[derive(Debug, Clone, Copy)]
struct ZoneRecipient {
    target: Address,
    virtual_addr: Option<Address>,
}

impl ZoneRecipient {
    fn direct(addr: Address) -> Self {
        Self {
            target: addr,
            virtual_addr: None,
        }
    }

    fn resolve(addr: Address) -> tempo_precompiles::error::Result<Self> {
        let effective = AddressRegistry::new().resolve_recipient(addr)?;
        Ok(if effective == addr {
            Self::direct(addr)
        } else {
            Self {
                target: effective,
                virtual_addr: Some(addr),
            }
        })
    }

    fn validate(&self) -> tempo_precompiles::error::Result<()> {
        if self.target.is_zero() || is_tip20_prefix(self.target) {
            return Err(tempo_contracts::precompiles::TIP20Error::invalid_recipient().into());
        }
        Ok(())
    }

    fn addressed_recipient(&self) -> Address {
        self.virtual_addr.unwrap_or(self.target)
    }
}

/// Zone-local writer for the upstream `ReceivePolicyGuard` storage layout.
#[contract(addr = RECEIVE_POLICY_GUARD_ADDRESS)]
struct ZoneReceivePolicyGuard {
    nonce: u64,
    balances: Mapping<B256, U256>,
}

impl ZoneReceivePolicyGuard {
    #[allow(clippy::too_many_arguments)]
    fn store_blocked(
        &mut self,
        token: Address,
        originator: Address,
        recipient: &ZoneRecipient,
        recovery_authority: Address,
        amount: U256,
        blocked_reason: BlockedReason,
        kind: IReceivePolicyGuard::InboundKind,
        memo: B256,
    ) -> tempo_precompiles::error::Result<()> {
        if matches!(
            blocked_reason,
            BlockedReason::NONE | BlockedReason::__Invalid
        ) || matches!(kind, IReceivePolicyGuard::InboundKind::__Invalid)
        {
            return Err(ReceivePolicyGuardError::invalid_receipt().into());
        }

        let blocked_nonce = self.next_receipt_nonce()?;
        let blocked_at = self.storage.timestamp().saturating_to::<u64>();
        let receipt = IReceivePolicyGuard::ClaimReceiptV1::new(
            token,
            recovery_authority,
            originator,
            recipient.addressed_recipient(),
            blocked_at,
            blocked_nonce,
            blocked_reason as u8,
            kind,
            memo,
        );
        let key = self.storage.keccak256(receipt.abi_encode().as_ref())?;
        self.balances[key].write(amount)?;
        self.emit_event(receipt.blocked_event(recipient.target, amount))
    }

    fn next_receipt_nonce(&mut self) -> tempo_precompiles::error::Result<u64> {
        let nonce = self.nonce.read()?.max(1);
        self.nonce.write(
            nonce
                .checked_add(1)
                .ok_or_else(tempo_precompiles::error::TempoPrecompileError::under_overflow)?,
        )?;
        Ok(nonce)
    }
}

mod zone_tip20_ledger {
    use super::*;

    /// Zone-local writer for the upstream `TIP20Token` storage layout.
    ///
    /// This deliberately mirrors the upstream storage fields so a blocked inbound
    /// transfer can move funds to the receive-policy guard without going through
    /// the public TIP20 entrypoints that reject the guard as a direct recipient.
    #[contract]
    pub(super) struct ZoneTip20Ledger {
        roles: Mapping<Address, Mapping<B256, bool>>,
        role_admins: Mapping<B256, B256>,

        name: String,
        symbol: String,
        currency: String,
        logo_uri: String,
        quote_token: Address,
        next_quote_token: Address,
        transfer_policy_id: u64,

        total_supply: U256,
        balances: Mapping<Address, U256>,
        allowances: Mapping<Address, Mapping<Address, U256>>,
        permit_nonces: Mapping<Address, U256>,
        paused: bool,
        supply_cap: U256,
        _salts: Mapping<B256, bool>,

        global_reward_per_token: U256,
        opted_in_supply: u128,
        user_reward_info: Mapping<Address, UserRewardInfo>,
    }

    impl ZoneTip20Ledger {
        pub(super) fn from_valid_tip20_address(address: Address) -> Self {
            Self::__new(address)
        }

        pub(super) fn consume_allowance(
            &mut self,
            owner: Address,
            spender: Address,
            amount: U256,
        ) -> tempo_precompiles::error::Result<()> {
            let allowed = self.get_allowance(owner, spender)?;
            if amount > allowed {
                return Err(TIP20Error::insufficient_allowance().into());
            }

            if allowed != U256::MAX {
                let new_allowance = allowed
                    .checked_sub(amount)
                    .ok_or_else(TIP20Error::insufficient_allowance)?;
                self.set_allowance(owner, spender, new_allowance)?;
            }
            Ok(())
        }

        pub(super) fn checked_from_balance_for_transfer(
            &self,
            from: Address,
            amount: U256,
        ) -> tempo_precompiles::error::Result<Option<U256>> {
            if self.storage.spec().is_t8() {
                return Ok(None);
            }

            let from_balance = self.get_balance(from)?;
            if amount > from_balance {
                return Err(
                    TIP20Error::insufficient_balance(from_balance, amount, self.address).into(),
                );
            }
            Ok(Some(from_balance))
        }

        pub(super) fn transfer_to_guard(
            &mut self,
            from: Address,
            amount: U256,
            from_balance: Option<U256>,
        ) -> tempo_precompiles::error::Result<()> {
            if let Some(from_balance) = from_balance {
                let new_from_balance = from_balance
                    .checked_sub(amount)
                    .ok_or_else(tempo_precompiles::error::TempoPrecompileError::under_overflow)?;
                self.set_balance(from, new_from_balance)?;
            } else {
                self.decrement_balance(from, amount)?;
            }

            self.increment_balance(RECEIVE_POLICY_GUARD_ADDRESS, amount)?;
            self.emit_event(TIP20Event::transfer(
                from,
                RECEIVE_POLICY_GUARD_ADDRESS,
                amount,
            ))
        }

        pub(super) fn checked_mint_supply(
            &self,
            total_supply: U256,
            amount: U256,
        ) -> tempo_precompiles::error::Result<U256> {
            let new_supply = total_supply
                .checked_add(amount)
                .ok_or_else(tempo_precompiles::error::TempoPrecompileError::under_overflow)?;

            let supply_cap = self.supply_cap.read()?;
            if new_supply > supply_cap {
                return Err(TIP20Error::supply_cap_exceeded().into());
            }

            Ok(new_supply)
        }

        pub(super) fn mint_to_guard(
            &mut self,
            new_supply: U256,
            amount: U256,
        ) -> tempo_precompiles::error::Result<()> {
            self.total_supply.write(new_supply)?;
            self.increment_balance(RECEIVE_POLICY_GUARD_ADDRESS, amount)?;
            self.emit_event(TIP20Event::transfer(
                Address::ZERO,
                RECEIVE_POLICY_GUARD_ADDRESS,
                amount,
            ))?;
            self.emit_event(TIP20Event::mint(RECEIVE_POLICY_GUARD_ADDRESS, amount))
        }

        fn get_balance(&self, account: Address) -> tempo_precompiles::error::Result<U256> {
            self.balances[account].read()
        }

        fn set_balance(
            &mut self,
            account: Address,
            amount: U256,
        ) -> tempo_precompiles::error::Result<()> {
            self.balances[account].write(amount)
        }

        fn increment_balance(
            &mut self,
            account: Address,
            amount: U256,
        ) -> tempo_precompiles::error::Result<()> {
            self.balances[account].sinc(amount).map_err(|err| {
                if err == tempo_precompiles::error::TempoPrecompileError::under_overflow() {
                    TIP20Error::supply_cap_exceeded().into()
                } else {
                    err
                }
            })
        }

        fn decrement_balance(
            &mut self,
            account: Address,
            amount: U256,
        ) -> tempo_precompiles::error::Result<()> {
            self.balances[account]
                .sdec(amount)
                .map_err(|err| match err {
                    tempo_precompiles::error::TempoPrecompileError::StorageDeltaUnderflow(
                        current,
                    ) => TIP20Error::insufficient_balance(current, amount, self.address).into(),
                    err => err,
                })
        }

        fn get_allowance(
            &self,
            owner: Address,
            spender: Address,
        ) -> tempo_precompiles::error::Result<U256> {
            self.allowances[owner][spender].read()
        }

        fn set_allowance(
            &mut self,
            owner: Address,
            spender: Address,
            amount: U256,
        ) -> tempo_precompiles::error::Result<()> {
            self.allowances[owner][spender].write(amount)
        }
    }
}

use zone_tip20_ledger::ZoneTip20Ledger;

/// Capability trait for resolving the active zone sequencer.
///
/// The zone runtime implements this for its L1-backed state provider so the
/// precompile can enforce sequencer-visible reads without knowing about the
/// concrete provider type.
pub trait SequencerExt: Send + Sync {
    /// Return the latest known active sequencer.
    fn latest_sequencer(&self) -> Option<Address>;
}

/// Zone-specific TIP-20 token precompile.
///
/// Wraps the vanilla [`TIP20Token`] and the [`ZoneTip403ProxyRegistry`] to add
/// optional PolicyCheck-backed authorization for transfers and mints, privacy-gated
/// `balanceOf`/`allowance`, fixed gas for transfer-family calls and `approve`,
/// and operation-specific bridge auth for mint/burn selectors.
pub struct ZoneTip20Token<P> {
    /// Optional TIP-403 registry wrapper used for transfer and mint-recipient policy checks.
    registry: Option<ZoneTip403ProxyRegistry<P>>,
    /// Sequencer-capable backend used to authorize private reads for the active sequencer.
    sequencer: Arc<dyn SequencerExt>,
}

impl<P: PolicyCheck> ZoneTip20Token<P> {
    /// Create a new wrapper with the given registry.
    pub fn new(
        registry: Option<ZoneTip403ProxyRegistry<P>>,
        sequencer: Arc<dyn SequencerExt>,
    ) -> Self {
        Self {
            registry,
            sequencer,
        }
    }

    fn selector(data: &[u8]) -> Option<[u8; 4]> {
        data.get(..4)?.try_into().ok()
    }

    fn is_fixed_gas_selector(selector: [u8; 4]) -> bool {
        matches!(
            selector,
            ITIP20::transferCall::SELECTOR
                | ITIP20::transferFromCall::SELECTOR
                | ITIP20::transferWithMemoCall::SELECTOR
                | ITIP20::transferFromWithMemoCall::SELECTOR
                | ITIP20::approveCall::SELECTOR
        )
    }

    fn apply_fixed_gas(result: PrecompileResult) -> PrecompileResult {
        match result {
            Ok(mut output) => {
                output.gas_used = FIXED_TRANSFER_GAS;
                Ok(output)
            }
            Err(err) => Err(err),
        }
    }

    /// Check selector-specific privacy/auth rules before delegating.
    ///
    /// Returns `Some(Ok(reverted_output))` if the call is forbidden.
    /// Returns `None` if the call may delegate to vanilla TIP20.
    fn precheck(
        &self,
        selector: [u8; 4],
        address: Address,
        data: &[u8],
        caller: Address,
    ) -> Option<PrecompileResult> {
        let args = &data[4..];

        match selector {
            ITIP20::balanceOfCall::SELECTOR => {
                let call = decode_or_revert!(ITIP20::balanceOfCall, args);
                self.enforce_balance_of(call.account, caller)
            }
            ITIP20::allowanceCall::SELECTOR => {
                let call = decode_or_revert!(ITIP20::allowanceCall, args);
                self.enforce_allowance(call.owner, call.spender, caller)
            }
            ITIP20::transferCall::SELECTOR => {
                let call = decode_or_revert!(ITIP20::transferCall, args);
                if let Some(revert) = self.enforce_transfer(address, caller, call.to) {
                    return Some(revert);
                }
                self.enforce_receive_transfer(
                    address,
                    None,
                    caller,
                    call.to,
                    call.amount,
                    B256::ZERO,
                    true,
                )
            }
            ITIP20::transferFromCall::SELECTOR => {
                let call = decode_or_revert!(ITIP20::transferFromCall, args);
                if let Some(revert) = self.enforce_transfer(address, call.from, call.to) {
                    return Some(revert);
                }
                self.enforce_receive_transfer(
                    address,
                    Some(caller),
                    call.from,
                    call.to,
                    call.amount,
                    B256::ZERO,
                    true,
                )
            }
            ITIP20::transferWithMemoCall::SELECTOR => {
                let call = decode_or_revert!(ITIP20::transferWithMemoCall, args);
                if let Some(revert) = self.enforce_transfer(address, caller, call.to) {
                    return Some(revert);
                }
                self.enforce_receive_transfer(
                    address,
                    None,
                    caller,
                    call.to,
                    call.amount,
                    call.memo,
                    false,
                )
            }
            ITIP20::transferFromWithMemoCall::SELECTOR => {
                let call = decode_or_revert!(ITIP20::transferFromWithMemoCall, args);
                if let Some(revert) = self.enforce_transfer(address, call.from, call.to) {
                    return Some(revert);
                }
                self.enforce_receive_transfer(
                    address,
                    Some(caller),
                    call.from,
                    call.to,
                    call.amount,
                    call.memo,
                    true,
                )
            }
            ITIP20::mintCall::SELECTOR => {
                if let Some(revert) = self.reject_crossed_mint_caller(caller) {
                    return Some(revert);
                }
                let call = decode_or_revert!(ITIP20::mintCall, args);
                if let Some(revert) = self.enforce_mint(address, call.to) {
                    return Some(revert);
                }
                self.enforce_receive_mint(address, caller, call.to, call.amount, B256::ZERO)
            }
            ITIP20::mintWithMemoCall::SELECTOR => {
                if let Some(revert) = self.reject_crossed_mint_caller(caller) {
                    return Some(revert);
                }
                let call = decode_or_revert!(ITIP20::mintWithMemoCall, args);
                if let Some(revert) = self.enforce_mint(address, call.to) {
                    return Some(revert);
                }
                self.enforce_receive_mint(address, caller, call.to, call.amount, call.memo)
            }
            ITIP20::burnCall::SELECTOR | ITIP20::burnWithMemoCall::SELECTOR => {
                self.reject_crossed_burn_caller(caller)
            }
            ITIP20::userRewardInfoCall::SELECTOR => {
                let call = decode_or_revert!(ITIP20::userRewardInfoCall, args);
                self.enforce_balance_of(call.account, caller)
            }
            ITIP20::getPendingRewardsCall::SELECTOR => {
                let call = decode_or_revert!(ITIP20::getPendingRewardsCall, args);
                self.enforce_balance_of(call.account, caller)
            }
            IRolesAuth::hasRoleCall::SELECTOR => {
                let call = decode_or_revert!(IRolesAuth::hasRoleCall, args);
                self.enforce_balance_of(call.account, caller)
            }
            _ => None,
        }
    }

    fn enforce_balance_of(&self, account: Address, caller: Address) -> Option<PrecompileResult> {
        if caller == account || self.is_sequencer(caller) {
            None
        } else {
            Some(Ok(Self::unauthorized_output()))
        }
    }

    fn enforce_allowance(
        &self,
        owner: Address,
        spender: Address,
        caller: Address,
    ) -> Option<PrecompileResult> {
        if caller == owner || caller == spender || self.is_sequencer(caller) {
            None
        } else {
            Some(Ok(Self::unauthorized_output()))
        }
    }

    /// Check sender + recipient authorization for a transfer.
    ///
    /// Returns `Some(revert)` if forbidden, `None` if allowed.
    fn enforce_transfer(
        &self,
        token: Address,
        from: Address,
        to: Address,
    ) -> Option<PrecompileResult> {
        let registry = self.registry.as_ref()?;
        let policy_id = match Self::resolve_transfer_policy_id(registry, token) {
            Ok(id) => id,
            Err(e) => {
                warn!(
                    target: "zone::precompile",
                    %token, error = %e,
                    "failed to resolve transfer_policy_id, rejecting transfer"
                );
                return Some(Err(e));
            }
        };

        trace!(
            target: "zone::precompile",
            %token, %from, %to, policy_id,
            "ZoneTip20Token: checking transfer authorization"
        );

        match registry.is_transfer_authorized(policy_id, from, to) {
            Ok(true) => None,
            Ok(false) => {
                trace!(
                    target: "zone::precompile",
                    %from, %to, policy_id, "transfer not authorized"
                );
                Some(Ok(Self::policy_forbids_output()))
            }
            Err(e) => Some(Err(e)),
        }
    }

    /// Check mint recipient authorization.
    ///
    /// Returns `Some(revert)` if forbidden, `None` if allowed.
    /// Resolution errors are treated as allow because mints are triggered by
    /// deposit system transactions whose policy is already enforced on L1.
    fn enforce_mint(&self, token: Address, to: Address) -> Option<PrecompileResult> {
        let registry = self.registry.as_ref()?;
        let policy_id = match Self::resolve_transfer_policy_id(registry, token) {
            Ok(id) => id,
            Err(e) => {
                warn!(
                    target: "zone::precompile",
                    %token, error = %e,
                    "failed to resolve transfer_policy_id for mint, deferring to L1 enforcement"
                );
                return None;
            }
        };

        trace!(
            target: "zone::precompile",
            %token, %to, policy_id,
            "ZoneTip20Token: checking mint recipient authorization"
        );

        match registry.is_authorized(policy_id, to, AuthRole::MintRecipient) {
            Ok(true) => None,
            Ok(false) => {
                trace!(target: "zone::precompile", %to, policy_id, "mint recipient not authorized");
                Some(Ok(Self::policy_forbids_output()))
            }
            Err(e) => Some(Err(e)),
        }
    }

    /// Apply TIP-1028 receive policy to a transfer-family call.
    fn enforce_receive_transfer(
        &self,
        token: Address,
        spender: Option<Address>,
        from: Address,
        to: Address,
        amount: U256,
        memo: B256,
        returns_bool: bool,
    ) -> Option<PrecompileResult> {
        if !StorageCtx::default().spec().is_t6() {
            return None;
        }

        let registry = self.registry.as_ref()?;
        let recipient = match Self::resolve_and_validate_recipient(to) {
            Ok(recipient) => recipient,
            Err(e) => return Some(StorageCtx::default().error_result(e)),
        };

        if recipient.target == RECEIVE_POLICY_GUARD_ADDRESS {
            return Some(Ok(Self::receive_policy_guard_address_reserved_output()));
        }

        match registry
            .validate_receive_policy(token, from, recipient.target)
            .map_err(|e| {
                warn!(
                    target: "zone::precompile",
                    %token, %from, receiver = %recipient.target, error = %e,
                    "failed to validate receive policy, rejecting transfer"
                );
                e
            }) {
            Ok(ReceivePolicyDecision::Authorized) => None,
            Ok(ReceivePolicyDecision::Blocked {
                reason,
                recovery_authority,
            }) => Some(self.block_transfer(
                token,
                spender,
                from,
                &recipient,
                amount,
                reason,
                recovery_authority,
                memo,
                returns_bool,
            )),
            Err(e) => Some(Err(e)),
        }
    }

    /// Apply TIP-1028 receive policy to a mint-family call.
    fn enforce_receive_mint(
        &self,
        token: Address,
        originator: Address,
        to: Address,
        amount: U256,
        memo: B256,
    ) -> Option<PrecompileResult> {
        if !StorageCtx::default().spec().is_t6() {
            return None;
        }

        let registry = self.registry.as_ref()?;
        let recipient = match Self::resolve_and_validate_recipient(to) {
            Ok(recipient) => recipient,
            Err(e) => return Some(StorageCtx::default().error_result(e)),
        };

        if recipient.target == RECEIVE_POLICY_GUARD_ADDRESS {
            return Some(Ok(Self::receive_policy_guard_address_reserved_output()));
        }

        match registry.validate_receive_policy(token, originator, recipient.target) {
            Ok(ReceivePolicyDecision::Authorized) => None,
            Ok(ReceivePolicyDecision::Blocked {
                reason,
                recovery_authority,
            }) => Some(self.block_mint(
                token,
                originator,
                &recipient,
                amount,
                reason,
                recovery_authority,
                memo,
            )),
            Err(e) => {
                warn!(
                    target: "zone::precompile",
                    %token, %originator, receiver = %recipient.target, error = %e,
                    "failed to validate receive policy for mint, deferring to L1 enforcement"
                );
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn block_transfer(
        &self,
        token: Address,
        spender: Option<Address>,
        from: Address,
        recipient: &ZoneRecipient,
        amount: U256,
        reason: BlockedReason,
        recovery_authority: Address,
        memo: B256,
        returns_bool: bool,
    ) -> PrecompileResult {
        trace!(
            target: "zone::precompile",
            %token, %from, receiver = %recipient.target, ?reason,
            "receive policy blocked transfer, moving funds to guard"
        );

        if StorageCtx::default().is_static() {
            return Ok(Self::static_call_not_allowed_output());
        }

        let mut tip20 = token_or_revert!(TIP20Token::from_address(token));
        if !token_or_revert!(tip20.is_initialized()) {
            return StorageCtx::default().error_result(TIP20Error::uninitialized());
        }

        token_or_revert!(tip20.check_not_paused());
        let mut ledger = ZoneTip20Ledger::from_valid_tip20_address(token);
        if let Some(spender) = spender {
            token_or_revert!(ledger.consume_allowance(from, spender, amount));
        } else {
            token_or_revert!(tip20.check_and_update_spending_limit(from, amount));
        }
        let from_balance = token_or_revert!(ledger.checked_from_balance_for_transfer(from, amount));
        token_or_revert!(tip20.handle_rewards_on_transfer(
            from,
            RECEIVE_POLICY_GUARD_ADDRESS,
            amount
        ));
        token_or_revert!(ledger.transfer_to_guard(from, amount, from_balance));

        token_or_revert!(Self::store_blocked_receipt(
            token,
            from,
            recipient,
            recovery_authority,
            amount,
            reason,
            IReceivePolicyGuard::InboundKind::TRANSFER,
            memo,
        ));
        Ok(Self::transfer_success_output(returns_bool))
    }

    #[allow(clippy::too_many_arguments)]
    fn block_mint(
        &self,
        token: Address,
        originator: Address,
        recipient: &ZoneRecipient,
        amount: U256,
        reason: BlockedReason,
        recovery_authority: Address,
        memo: B256,
    ) -> PrecompileResult {
        trace!(
            target: "zone::precompile",
            %token, %originator, receiver = %recipient.target, ?reason,
            "receive policy blocked mint, moving funds to guard"
        );

        if StorageCtx::default().is_static() {
            return Ok(Self::static_call_not_allowed_output());
        }

        let mut tip20 = token_or_revert!(TIP20Token::from_address(token));
        if !token_or_revert!(tip20.is_initialized()) {
            return StorageCtx::default().error_result(TIP20Error::uninitialized());
        }

        token_or_revert!(tip20.check_role(originator, *ISSUER_ROLE));
        let total_supply = token_or_revert!(tip20.total_supply());
        if StorageCtx::default().spec().is_t3() {
            token_or_revert!(tip20.check_not_paused());
        }

        let mut ledger = ZoneTip20Ledger::from_valid_tip20_address(token);
        let new_supply = token_or_revert!(ledger.checked_mint_supply(total_supply, amount));
        token_or_revert!(tip20.handle_rewards_on_mint(RECEIVE_POLICY_GUARD_ADDRESS, amount));
        token_or_revert!(ledger.mint_to_guard(new_supply, amount));

        token_or_revert!(Self::store_blocked_receipt(
            token,
            originator,
            recipient,
            recovery_authority,
            amount,
            reason,
            IReceivePolicyGuard::InboundKind::MINT,
            memo,
        ));
        Ok(StorageCtx::default().success_output(Bytes::new()))
    }

    fn store_blocked_receipt(
        token: Address,
        originator: Address,
        recipient: &ZoneRecipient,
        recovery_authority: Address,
        amount: U256,
        reason: BlockedReason,
        kind: IReceivePolicyGuard::InboundKind,
        memo: B256,
    ) -> tempo_precompiles::error::Result<()> {
        ZoneReceivePolicyGuard::new().store_blocked(
            token,
            originator,
            recipient,
            recovery_authority,
            amount,
            reason,
            kind,
            memo,
        )
    }

    fn resolve_and_validate_recipient(
        to: Address,
    ) -> tempo_precompiles::error::Result<ZoneRecipient> {
        let recipient = ZoneRecipient::resolve(to)?;
        recipient.validate()?;
        Ok(recipient)
    }

    /// Reject the system caller that is only allowed on the opposite bridge path.
    fn reject_crossed_mint_caller(&self, caller: Address) -> Option<PrecompileResult> {
        if caller == ZONE_OUTBOX_ADDRESS {
            Some(Ok(Self::roles_unauthorized_output()))
        } else {
            None
        }
    }

    /// Reject the system caller that is only allowed on the opposite bridge path.
    fn reject_crossed_burn_caller(&self, caller: Address) -> Option<PrecompileResult> {
        if caller == ZONE_INBOX_ADDRESS {
            Some(Ok(Self::roles_unauthorized_output()))
        } else {
            None
        }
    }

    /// Resolve the `transfer_policy_id` for a token.
    fn resolve_transfer_policy_id(
        registry: &ZoneTip403ProxyRegistry<P>,
        token: Address,
    ) -> Result<u64, PrecompileError> {
        registry.resolve_transfer_policy_id(token)
    }

    fn is_sequencer(&self, caller: Address) -> bool {
        self.sequencer
            .latest_sequencer()
            .is_some_and(|sequencer| caller == sequencer)
    }

    fn unauthorized_output() -> PrecompileOutput {
        StorageCtx::default().revert_output(Unauthorized {}.abi_encode().into())
    }

    fn roles_unauthorized_output() -> PrecompileOutput {
        StorageCtx::default().revert_output(RolesAuthError::unauthorized().selector().into())
    }

    fn receive_policy_guard_address_reserved_output() -> PrecompileOutput {
        StorageCtx::default().revert_output(
            ReceivePolicyGuardError::address_reserved()
                .selector()
                .into(),
        )
    }

    fn static_call_not_allowed_output() -> PrecompileOutput {
        let storage = StorageCtx::default();
        PrecompileOutput::revert(
            0,
            StaticCallNotAllowed {}.abi_encode().into(),
            storage.reservoir(),
        )
    }

    fn transfer_success_output(returns_bool: bool) -> PrecompileOutput {
        let bytes = if returns_bool {
            ITIP20::transferCall::abi_encode_returns(&true).into()
        } else {
            Bytes::new()
        };
        StorageCtx::default().success_output(bytes)
    }

    /// Build a reverted output with the `policyForbids()` error selector.
    fn policy_forbids_output() -> PrecompileOutput {
        PrecompileOutput::revert(
            AUTH_CHECK_GAS,
            tempo_contracts::precompiles::TIP20Error::policy_forbids()
                .selector()
                .into(),
            StorageCtx::default().reservoir(),
        )
    }
}

impl<P> ZoneTip20Token<P>
where
    P: PolicyCheck + Clone + Send + Sync + 'static,
{
    /// Create a [`DynPrecompile`] for a zone-side TIP-20 token at `address`.
    ///
    /// The returned precompile:
    /// 1. Checks the 4-byte selector for transfer/mint calls.
    /// 2. When a TIP-403 registry is configured, reads `transfer_policy_id`
    ///    from EVM storage and checks authorization via the
    ///    [`ZoneTip403ProxyRegistry`].
    /// 3. Delegates to the vanilla `TIP20Token::call()` for execution.
    pub fn create(
        address: Address,
        cfg: &revm::context::CfgEnv<tempo_chainspec::hardfork::TempoHardfork>,
        registry: Option<ZoneTip403ProxyRegistry<P>>,
        sequencer: Arc<dyn SequencerExt>,
    ) -> DynPrecompile {
        let spec = cfg.spec;
        let amsterdam_eip8037_enabled = cfg.enable_amsterdam_eip8037;
        let gas_params = cfg.gas_params.clone();
        let token = Self::new(registry, sequencer);

        DynPrecompile::new_stateful(
            PrecompileId::Custom("ZoneTip20Token".into()),
            move |input| {
                if !input.is_direct_call() {
                    return Ok(PrecompileOutput::revert(
                        0,
                        SolError::abi_encode(&DelegateCallNotAllowed {}).into(),
                        input.reservoir,
                    ));
                }

                let selector = Self::selector(input.data);
                let is_fixed_gas = selector.is_some_and(Self::is_fixed_gas_selector);
                if is_fixed_gas && input.gas < FIXED_TRANSFER_GAS {
                    return Ok(PrecompileOutput::halt(
                        PrecompileHalt::OutOfGas,
                        input.reservoir,
                    ));
                }

                let mut storage = EvmPrecompileStorageProvider::new(
                    input.internals,
                    if is_fixed_gas { u64::MAX } else { input.gas },
                    input.reservoir,
                    spec,
                    amsterdam_eip8037_enabled,
                    input.is_static,
                    gas_params.clone(),
                );

                StorageCtx::enter(&mut storage, || {
                    if let Some(selector) = selector
                        && let Some(revert) =
                            token.precheck(selector, address, input.data, input.caller)
                    {
                        return if is_fixed_gas {
                            Self::apply_fixed_gas(revert)
                        } else {
                            revert
                        };
                    }

                    let mut tip20 =
                        TIP20Token::from_address(address).expect("TIP20 prefix already verified");
                    let result = tip20.call(input.data, input.caller);
                    if is_fixed_gas {
                        Self::apply_fixed_gas(result)
                    } else {
                        result
                    }
                })
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, U256, address};
    use alloy_evm::{
        EvmInternals,
        precompiles::{Precompile as AlloyEvmPrecompile, PrecompileInput},
    };
    use alloy_sol_types::SolCall;
    use revm::{
        Context,
        database::{CacheDB, EmptyDB},
        precompile::{PrecompileHalt, PrecompileResult},
    };
    use tempo_chainspec::hardfork::TempoHardfork;
    use tempo_precompiles::{
        PATH_USD_ADDRESS, RECEIVE_POLICY_GUARD_ADDRESS,
        tip20::{ISSUER_ROLE, ITIP20, TIP20Token},
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
    type TestContext = Context<
        revm::context::BlockEnv,
        revm::context::TxEnv,
        revm::context::CfgEnv<TempoHardfork>,
        CacheDB<EmptyDB>,
    >;

    #[derive(Clone, Default)]
    struct MockPolicyProvider {
        transfer_authorized: bool,
        mint_authorized: bool,
        policy_id: u64,
        fail_policy_id_resolution: bool,
        receive_policy_decision: Option<ReceivePolicyDecision>,
    }

    impl MockPolicyProvider {
        fn allow_all() -> Self {
            Self {
                transfer_authorized: true,
                mint_authorized: true,
                policy_id: 1,
                fail_policy_id_resolution: false,
                receive_policy_decision: None,
            }
        }

        fn failing() -> Self {
            Self {
                fail_policy_id_resolution: true,
                ..Default::default()
            }
        }
    }

    impl PolicyCheck for MockPolicyProvider {
        fn is_authorized(
            &self,
            _policy_id: u64,
            _user: Address,
            role: AuthRole,
        ) -> Result<bool, PrecompileError> {
            let authorized = match role {
                AuthRole::MintRecipient => self.mint_authorized,
                _ => self.transfer_authorized,
            };
            Ok(authorized)
        }

        fn resolve_transfer_policy_id(&self, _token: Address) -> Result<u64, PrecompileError> {
            if self.fail_policy_id_resolution {
                return Err(PrecompileError::Fatal("RPC unavailable".into()));
            }
            Ok(self.policy_id)
        }

        fn policy_type_sync(
            &self,
            _policy_id: u64,
        ) -> Result<tempo_contracts::precompiles::ITIP403Registry::PolicyType, PrecompileError>
        {
            Ok(tempo_contracts::precompiles::ITIP403Registry::PolicyType::BLACKLIST)
        }

        fn compound_policy_data(
            &self,
            _policy_id: u64,
        ) -> Result<(u64, u64, u64), PrecompileError> {
            Ok((self.policy_id, self.policy_id, self.policy_id))
        }

        fn policy_exists(&self, _policy_id: u64) -> Result<bool, PrecompileError> {
            Ok(true)
        }

        fn policy_id_counter(&self) -> u64 {
            self.policy_id
        }

        fn receive_policy(
            &self,
            _account: Address,
        ) -> Result<crate::policy::ReceivePolicy, PrecompileError> {
            Ok(crate::policy::ReceivePolicy::none())
        }

        fn validate_receive_policy(
            &self,
            _token: Address,
            _sender: Address,
            _receiver: Address,
        ) -> Result<ReceivePolicyDecision, PrecompileError> {
            Ok(self
                .receive_policy_decision
                .unwrap_or(ReceivePolicyDecision::Authorized))
        }
    }

    #[derive(Clone, Copy)]
    struct MockSequencer {
        address: Option<Address>,
    }

    impl SequencerExt for MockSequencer {
        fn latest_sequencer(&self) -> Option<Address> {
            self.address
        }
    }

    struct PrecompileHarness {
        ctx: TestContext,
        token: Address,
        alice: Address,
        bob: Address,
        spender: Address,
        sequencer: Address,
        issuer: Address,
        precompile: DynPrecompile,
    }

    impl PrecompileHarness {
        fn new(policy: MockPolicyProvider) -> TestResult<Self> {
            Self::new_with_registry(Some(policy))
        }

        fn new_without_registry() -> TestResult<Self> {
            Self::new_with_registry(None)
        }

        fn new_with_registry(policy: Option<MockPolicyProvider>) -> TestResult<Self> {
            Self::new_with_registry_and_spec(policy, TempoHardfork::default())
        }

        fn new_with_registry_and_spec(
            policy: Option<MockPolicyProvider>,
            spec: TempoHardfork,
        ) -> TestResult<Self> {
            let token = PATH_USD_ADDRESS;
            let admin = address!("0x00000000000000000000000000000000000000a1");
            let alice = address!("0x00000000000000000000000000000000000000a2");
            let bob = address!("0x00000000000000000000000000000000000000a3");
            let spender = address!("0x00000000000000000000000000000000000000a4");
            let issuer = address!("0x00000000000000000000000000000000000000a5");
            let sequencer = address!("0x00000000000000000000000000000000000000a6");
            let mut ctx = Context::new(CacheDB::new(EmptyDB::new()), spec);

            Self::with_storage(&mut ctx, u64::MAX, |storage| {
                StorageCtx::enter(storage, || -> TestResult {
                    let mut token_contract =
                        TIP20Token::from_address(token).expect("PATH_USD must be valid");
                    token_contract.initialize(
                        admin,
                        "Zone USD",
                        "zUSD",
                        "USD",
                        Address::ZERO,
                        admin,
                    )?;
                    token_contract.grant_role_internal(admin, *ISSUER_ROLE)?;
                    token_contract.grant_role_internal(issuer, *ISSUER_ROLE)?;
                    token_contract.grant_role_internal(ZONE_INBOX_ADDRESS, *ISSUER_ROLE)?;
                    token_contract.grant_role_internal(ZONE_OUTBOX_ADDRESS, *ISSUER_ROLE)?;
                    token_contract.mint(
                        admin,
                        ITIP20::mintCall {
                            to: alice,
                            amount: U256::from(1_000_000u64),
                        },
                    )?;
                    token_contract.mint(
                        admin,
                        ITIP20::mintCall {
                            to: ZONE_OUTBOX_ADDRESS,
                            amount: U256::from(10_000u64),
                        },
                    )?;
                    token_contract.approve(
                        alice,
                        ITIP20::approveCall {
                            spender,
                            amount: U256::from(300_000u64),
                        },
                    )?;
                    Ok(())
                })
            })?;

            let precompile = ZoneTip20Token::create(
                token,
                &ctx.cfg,
                policy.map(ZoneTip403ProxyRegistry::new),
                Arc::new(MockSequencer {
                    address: Some(sequencer),
                }),
            );

            Ok(Self {
                ctx,
                token,
                alice,
                bob,
                spender,
                sequencer,
                issuer,
                precompile,
            })
        }

        fn with_storage<T>(
            ctx: &mut TestContext,
            gas_limit: u64,
            f: impl FnOnce(&mut EvmPrecompileStorageProvider<'_>) -> TestResult<T>,
        ) -> TestResult<T> {
            let spec = ctx.cfg.spec;
            let amsterdam_eip8037_enabled = ctx.cfg.enable_amsterdam_eip8037;
            let gas_params = ctx.cfg.gas_params.clone();
            let internals = EvmInternals::from_context(ctx);
            let mut storage = EvmPrecompileStorageProvider::new(
                internals,
                gas_limit,
                0,
                spec,
                amsterdam_eip8037_enabled,
                false,
                gas_params,
            );
            f(&mut storage)
        }

        fn call(
            &mut self,
            caller: Address,
            calldata: Bytes,
            gas: u64,
            is_static: bool,
        ) -> PrecompileResult {
            AlloyEvmPrecompile::call(
                &self.precompile,
                PrecompileInput {
                    data: &calldata,
                    caller,
                    internals: EvmInternals::from_context(&mut self.ctx),
                    gas,
                    reservoir: 0,
                    value: U256::ZERO,
                    is_static,
                    target_address: self.token,
                    bytecode_address: self.token,
                },
            )
        }

        fn balance_of(&mut self, account: Address) -> TestResult<U256> {
            Self::with_storage(&mut self.ctx, u64::MAX, |storage| {
                StorageCtx::enter(storage, || {
                    let token = TIP20Token::from_address(self.token).expect("token must exist");
                    Ok(token.balance_of(ITIP20::balanceOfCall { account })?)
                })
            })
        }

        fn allowance(&mut self, owner: Address, spender: Address) -> TestResult<U256> {
            Self::with_storage(&mut self.ctx, u64::MAX, |storage| {
                StorageCtx::enter(storage, || {
                    let token = TIP20Token::from_address(self.token).expect("token must exist");
                    Ok(token.allowance(ITIP20::allowanceCall { owner, spender })?)
                })
            })
        }
    }

    #[test]
    fn balance_of_enforces_account_or_sequencer_access() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::allow_all())?;
        let calldata: Bytes = ITIP20::balanceOfCall {
            account: harness.alice,
        }
        .abi_encode()
        .into();

        let owner = harness.call(harness.alice, calldata.clone(), 100_000, true)?;
        assert_eq!(
            ITIP20::balanceOfCall::abi_decode_returns(&owner.bytes)?,
            U256::from(1_000_000u64)
        );

        let sequencer = harness.call(harness.sequencer, calldata.clone(), 100_000, true)?;
        assert_eq!(
            ITIP20::balanceOfCall::abi_decode_returns(&sequencer.bytes)?,
            U256::from(1_000_000u64)
        );

        let outsider = harness.call(harness.bob, calldata, 100_000, true)?;
        assert!(outsider.is_revert());
        assert_eq!(outsider.bytes, Bytes::from(Unauthorized {}.abi_encode()));

        Ok(())
    }

    #[test]
    fn allowance_enforces_owner_spender_or_sequencer_access() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::allow_all())?;
        let calldata: Bytes = ITIP20::allowanceCall {
            owner: harness.alice,
            spender: harness.spender,
        }
        .abi_encode()
        .into();

        let owner = harness.call(harness.alice, calldata.clone(), 100_000, true)?;
        assert_eq!(
            ITIP20::allowanceCall::abi_decode_returns(&owner.bytes)?,
            U256::from(300_000u64)
        );

        let spender = harness.call(harness.spender, calldata.clone(), 100_000, true)?;
        assert_eq!(
            ITIP20::allowanceCall::abi_decode_returns(&spender.bytes)?,
            U256::from(300_000u64)
        );

        let sequencer = harness.call(harness.sequencer, calldata.clone(), 100_000, true)?;
        assert_eq!(
            ITIP20::allowanceCall::abi_decode_returns(&sequencer.bytes)?,
            U256::from(300_000u64)
        );

        let outsider = harness.call(harness.bob, calldata, 100_000, true)?;
        assert!(outsider.is_revert());
        assert_eq!(outsider.bytes, Bytes::from(Unauthorized {}.abi_encode()));

        Ok(())
    }

    #[test]
    fn wrapper_without_policy_registry_still_enforces_privacy_and_fixed_gas() -> TestResult {
        let mut harness = PrecompileHarness::new_without_registry()?;

        let private_balance = harness.call(
            harness.bob,
            ITIP20::balanceOfCall {
                account: harness.alice,
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            true,
        )?;
        assert!(private_balance.is_revert());
        assert_eq!(
            private_balance.bytes,
            Bytes::from(Unauthorized {}.abi_encode())
        );

        let transfer = harness.call(
            harness.alice,
            ITIP20::transferCall {
                to: harness.bob,
                amount: U256::from(12_345u64),
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;
        assert!(transfer.is_success());
        assert_eq!(transfer.gas_used, FIXED_TRANSFER_GAS);
        assert_eq!(harness.balance_of(harness.bob)?, U256::from(12_345u64));

        Ok(())
    }

    #[test]
    fn receive_policy_blocked_transfer_moves_funds_to_guard() -> TestResult {
        let mut policy = MockPolicyProvider::allow_all();
        policy.receive_policy_decision = Some(ReceivePolicyDecision::Blocked {
            reason: BlockedReason::RECEIVE_POLICY,
            recovery_authority: Address::ZERO,
        });
        let mut harness =
            PrecompileHarness::new_with_registry_and_spec(Some(policy), TempoHardfork::T6)?;

        let amount = U256::from(42_000u64);
        let transfer = harness.call(
            harness.alice,
            ITIP20::transferCall {
                to: harness.bob,
                amount,
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;

        assert!(transfer.is_success());
        assert_eq!(
            ITIP20::transferCall::abi_decode_returns(&transfer.bytes)?,
            true
        );
        assert_eq!(harness.balance_of(harness.bob)?, U256::ZERO);
        assert_eq!(harness.balance_of(RECEIVE_POLICY_GUARD_ADDRESS)?, amount);
        assert_eq!(
            harness.balance_of(harness.alice)?,
            U256::from(1_000_000u64) - amount
        );

        Ok(())
    }

    #[test]
    fn receive_policy_blocked_mint_moves_funds_to_guard() -> TestResult {
        let mut policy = MockPolicyProvider::allow_all();
        policy.receive_policy_decision = Some(ReceivePolicyDecision::Blocked {
            reason: BlockedReason::TOKEN_FILTER,
            recovery_authority: Address::ZERO,
        });
        let mut harness =
            PrecompileHarness::new_with_registry_and_spec(Some(policy), TempoHardfork::T6)?;

        let amount = U256::from(17_000u64);
        let mint = harness.call(
            harness.issuer,
            ITIP20::mintCall {
                to: harness.bob,
                amount,
            }
            .abi_encode()
            .into(),
            200_000,
            false,
        )?;

        assert!(mint.is_success());
        assert_eq!(harness.balance_of(harness.bob)?, U256::ZERO);
        assert_eq!(harness.balance_of(RECEIVE_POLICY_GUARD_ADDRESS)?, amount);

        Ok(())
    }

    #[test]
    fn bridge_auth_rejects_crossed_system_calls_and_keeps_allowed_paths() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::allow_all())?;

        let inbox_mint = harness.call(
            ZONE_INBOX_ADDRESS,
            ITIP20::mintCall {
                to: harness.bob,
                amount: U256::from(50_000u64),
            }
            .abi_encode()
            .into(),
            100_000,
            false,
        )?;
        assert!(inbox_mint.is_success());
        assert_eq!(harness.balance_of(harness.bob)?, U256::from(50_000u64));

        let outbox_burn = harness.call(
            ZONE_OUTBOX_ADDRESS,
            ITIP20::burnCall {
                amount: U256::from(10_000u64),
            }
            .abi_encode()
            .into(),
            100_000,
            false,
        )?;
        assert!(outbox_burn.is_success());
        assert_eq!(harness.balance_of(ZONE_OUTBOX_ADDRESS)?, U256::ZERO);

        let crossed_mint = harness.call(
            ZONE_OUTBOX_ADDRESS,
            ITIP20::mintCall {
                to: harness.bob,
                amount: U256::from(1u64),
            }
            .abi_encode()
            .into(),
            100_000,
            false,
        )?;
        assert!(crossed_mint.is_revert());
        assert_eq!(
            crossed_mint.bytes,
            Bytes::from(RolesAuthError::unauthorized().selector().to_vec())
        );

        let crossed_burn = harness.call(
            ZONE_INBOX_ADDRESS,
            ITIP20::burnCall {
                amount: U256::from(1u64),
            }
            .abi_encode()
            .into(),
            100_000,
            false,
        )?;
        assert!(crossed_burn.is_revert());
        assert_eq!(
            crossed_burn.bytes,
            Bytes::from(RolesAuthError::unauthorized().selector().to_vec())
        );

        let issuer_mint = harness.call(
            harness.issuer,
            ITIP20::mintCall {
                to: harness.issuer,
                amount: U256::from(25_000u64),
            }
            .abi_encode()
            .into(),
            100_000,
            false,
        )?;
        assert!(issuer_mint.is_success());

        let issuer_burn = harness.call(
            harness.issuer,
            ITIP20::burnCall {
                amount: U256::from(5_000u64),
            }
            .abi_encode()
            .into(),
            100_000,
            false,
        )?;
        assert!(issuer_burn.is_success());

        Ok(())
    }

    #[test]
    fn fixed_gas_selectors_charge_exactly_one_hundred_thousand_gas() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::allow_all())?;

        let approve = harness.call(
            harness.alice,
            ITIP20::approveCall {
                spender: harness.spender,
                amount: U256::from(111_111u64),
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;
        assert_eq!(approve.gas_used, FIXED_TRANSFER_GAS);
        assert_eq!(approve.state_gas_used, 0);

        let approve_update = harness.call(
            harness.alice,
            ITIP20::approveCall {
                spender: harness.spender,
                amount: U256::from(222_222u64),
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;
        assert_eq!(approve_update.gas_used, FIXED_TRANSFER_GAS);
        assert_eq!(approve_update.state_gas_used, 0);

        let transfer_new = harness.call(
            harness.alice,
            ITIP20::transferCall {
                to: harness.bob,
                amount: U256::from(10_000u64),
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;
        assert_eq!(transfer_new.gas_used, FIXED_TRANSFER_GAS);
        assert_eq!(transfer_new.state_gas_used, 0);

        let transfer_existing = harness.call(
            harness.alice,
            ITIP20::transferCall {
                to: harness.bob,
                amount: U256::from(10_000u64),
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;
        assert_eq!(transfer_existing.gas_used, FIXED_TRANSFER_GAS);
        assert_eq!(transfer_existing.state_gas_used, 0);

        let transfer_with_memo = harness.call(
            harness.alice,
            ITIP20::transferWithMemoCall {
                to: harness.bob,
                amount: U256::from(10_000u64),
                memo: Default::default(),
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;
        assert_eq!(transfer_with_memo.gas_used, FIXED_TRANSFER_GAS);
        assert_eq!(transfer_with_memo.state_gas_used, 0);

        let transfer_from = harness.call(
            harness.spender,
            ITIP20::transferFromCall {
                from: harness.alice,
                to: harness.bob,
                amount: U256::from(10_000u64),
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;
        assert_eq!(transfer_from.gas_used, FIXED_TRANSFER_GAS);
        assert_eq!(transfer_from.state_gas_used, 0);

        let transfer_from_with_memo = harness.call(
            harness.spender,
            ITIP20::transferFromWithMemoCall {
                from: harness.alice,
                to: harness.bob,
                amount: U256::from(10_000u64),
                memo: Default::default(),
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;
        assert_eq!(transfer_from_with_memo.gas_used, FIXED_TRANSFER_GAS);
        assert_eq!(transfer_from_with_memo.state_gas_used, 0);

        Ok(())
    }

    #[test]
    fn fixed_gas_selectors_fail_out_of_gas_below_threshold() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::allow_all())?;

        for calldata in [
            ITIP20::transferCall {
                to: harness.bob,
                amount: U256::from(1u64),
            }
            .abi_encode()
            .into(),
            ITIP20::transferFromCall {
                from: harness.alice,
                to: harness.bob,
                amount: U256::from(1u64),
            }
            .abi_encode()
            .into(),
            ITIP20::transferWithMemoCall {
                to: harness.bob,
                amount: U256::from(1u64),
                memo: Default::default(),
            }
            .abi_encode()
            .into(),
            ITIP20::transferFromWithMemoCall {
                from: harness.alice,
                to: harness.bob,
                amount: U256::from(1u64),
                memo: Default::default(),
            }
            .abi_encode()
            .into(),
            ITIP20::approveCall {
                spender: harness.spender,
                amount: U256::from(1u64),
            }
            .abi_encode()
            .into(),
        ] {
            let output = harness
                .call(harness.alice, calldata, FIXED_TRANSFER_GAS - 1, false)
                .expect("out of gas is returned as a halted precompile output");
            assert!(output.is_halt());
            assert_eq!(output.halt_reason(), Some(&PrecompileHalt::OutOfGas));
        }

        Ok(())
    }

    #[test]
    fn fixed_gas_keeps_allowance_and_balance_state_changes_intact() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::allow_all())?;

        let approve = harness.call(
            harness.alice,
            ITIP20::approveCall {
                spender: harness.spender,
                amount: U256::from(123_456u64),
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;
        assert!(approve.is_success());
        assert_eq!(
            harness.allowance(harness.alice, harness.spender)?,
            U256::from(123_456u64)
        );

        let transfer = harness.call(
            harness.alice,
            ITIP20::transferCall {
                to: harness.bob,
                amount: U256::from(7_654u64),
            }
            .abi_encode()
            .into(),
            FIXED_TRANSFER_GAS,
            false,
        )?;
        assert!(transfer.is_success());
        assert_eq!(harness.balance_of(harness.bob)?, U256::from(7_654u64));

        Ok(())
    }

    #[test]
    fn user_reward_info_enforces_account_or_sequencer_access() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::allow_all())?;
        let calldata: Bytes = ITIP20::userRewardInfoCall {
            account: harness.alice,
        }
        .abi_encode()
        .into();

        // Owner can query their own reward info
        let owner = harness.call(harness.alice, calldata.clone(), 100_000, true)?;
        assert!(owner.is_success());

        // Sequencer can query anyone's reward info
        let sequencer = harness.call(harness.sequencer, calldata.clone(), 100_000, true)?;
        assert!(sequencer.is_success());

        // Outsider is rejected
        let outsider = harness.call(harness.bob, calldata, 100_000, true)?;
        assert!(outsider.is_revert());
        assert_eq!(outsider.bytes, Bytes::from(Unauthorized {}.abi_encode()));

        Ok(())
    }

    #[test]
    fn get_pending_rewards_enforces_account_or_sequencer_access() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::allow_all())?;
        let calldata: Bytes = ITIP20::getPendingRewardsCall {
            account: harness.alice,
        }
        .abi_encode()
        .into();

        // Owner can query their own pending rewards
        let owner = harness.call(harness.alice, calldata.clone(), 100_000, true)?;
        assert!(owner.is_success());

        // Sequencer can query anyone's pending rewards
        let sequencer = harness.call(harness.sequencer, calldata.clone(), 100_000, true)?;
        assert!(sequencer.is_success());

        // Outsider is rejected
        let outsider = harness.call(harness.bob, calldata, 100_000, true)?;
        assert!(outsider.is_revert());
        assert_eq!(outsider.bytes, Bytes::from(Unauthorized {}.abi_encode()));

        Ok(())
    }

    #[test]
    fn transfer_fails_closed_on_policy_resolution_error() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::failing())?;

        let calldata: Bytes = ITIP20::transferCall {
            to: harness.bob,
            amount: U256::from(100u64),
        }
        .abi_encode()
        .into();

        let result = harness.call(harness.alice, calldata, 100_000, false);
        assert!(
            result.is_err(),
            "transfer must fail when policy resolution errors"
        );

        Ok(())
    }

    #[test]
    fn mint_defers_to_l1_on_policy_resolution_error() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::failing())?;

        let calldata: Bytes = ITIP20::mintCall {
            to: harness.alice,
            amount: U256::from(100u64),
        }
        .abi_encode()
        .into();

        let result = harness.call(harness.issuer, calldata, 100_000, false);
        assert!(
            result.is_ok(),
            "mint must proceed when policy resolution errors (L1 enforces policy at deposit time)"
        );

        Ok(())
    }

    #[test]
    fn has_role_enforces_account_or_sequencer_access() -> TestResult {
        let mut harness = PrecompileHarness::new(MockPolicyProvider::allow_all())?;
        let calldata: Bytes = IRolesAuth::hasRoleCall {
            account: harness.alice,
            role: *ISSUER_ROLE,
        }
        .abi_encode()
        .into();

        // Owner can query their own roles
        let owner = harness.call(harness.alice, calldata.clone(), 100_000, true)?;
        assert!(owner.is_success());

        // Sequencer can query anyone's roles
        let sequencer = harness.call(harness.sequencer, calldata.clone(), 100_000, true)?;
        assert!(sequencer.is_success());

        // Outsider is rejected
        let outsider = harness.call(harness.bob, calldata, 100_000, true)?;
        assert!(outsider.is_revert());
        assert_eq!(outsider.bytes, Bytes::from(Unauthorized {}.abi_encode()));

        Ok(())
    }
}
