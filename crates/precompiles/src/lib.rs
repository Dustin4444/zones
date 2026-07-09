//! Zone-native precompiles and zone execution for upstream Tempo precompiles.
//!
//! Zone-native precompiles execute with ordinary local EVM storage. Zone-adapted Tempo
//! precompiles first apply narrow zone call rules, then forward accepted calls to
//! the upstream implementation. L1-backed execution pins each call to the finalized
//! Tempo anchor and overlays selected policy reads without reimplementing Tempo logic.
//!
//! This crate is `no_std` compatible so the precompiles can run inside the SP1 prover
//! guest (RISC-V) as well as in the zone node.
//!
//! ## Crypto precompiles
//!
//! - **Chaum-Pedersen Verify** ([`chaum_pedersen`]) — verifies DLOG equality proofs
//!   for ECDH shared secret derivation.
//! - **AES-256-GCM Decrypt** ([`aes_gcm`]) — decrypts ECIES ciphertext and verifies
//!   the GCM authentication tag.
//! - **ECIES** ([`ecies`]) — sequencer-side ECIES decryption logic.
//!
//! ## Policy/token precompiles
//!
//! - **TIP-20 Factory** ([`tip20_factory`]) — zone-side TIP-20 token factory.
//! - **TIP403 Registry** ([`tip403_proxy`]) — upstream registry over finalized L1 state.
//! - **Zone TIP20** ([`ztip20`]) — upstream TIP20 with zone rules and L1 policy reads.

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::too_many_arguments)]

extern crate alloc;

#[cfg(test)]
mod test_utils;

// Required by the `#[contract]` proc macro expansion (references `crate::error`).
pub(crate) use tempo_precompiles::error;

pub mod aes_gcm;
pub mod chaum_pedersen;
pub mod ecies;
mod execution;
pub mod storage;
pub mod tempo_state;
pub mod tip20_factory;
pub mod tip403_proxy;
pub mod ztip20;

pub use aes_gcm::{AES_GCM_DECRYPT_ADDRESS, AesGcmDecrypt};
pub use chaum_pedersen::{CHAUM_PEDERSEN_VERIFY_ADDRESS, ChaumPedersenVerify};
pub(crate) use execution::{
    L1BackedPrecompileEnv, create_l1_backed_precompile, create_local_precompile,
};
pub use storage::L1StorageReader;
pub use tempo_state::TempoState;
pub use tip20_factory::{ZONE_TIP20_FACTORY_ADDRESS, ZoneTokenFactory};
pub use tip403_proxy::ZONE_TIP403_ADDRESS;
pub use ztip20::SequencerExt;

use alloc::{rc::Rc, sync::Arc};
use core::cell::RefCell;

use alloy_evm::precompiles::{DynPrecompile, PrecompilesMap};
use revm::{context::CfgEnv, precompile::PrecompileError};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_precompiles::{
    ACCOUNT_KEYCHAIN_ADDRESS, NONCE_PRECOMPILE_ADDRESS, Precompile as TempoPrecompile,
    PrecompileEnv, RECEIVE_POLICY_GUARD_ADDRESS, STABLECOIN_DEX_ADDRESS, TIP_FEE_MANAGER_ADDRESS,
    account_keychain::AccountKeychain,
    nonce::NonceManager,
    receive_policy_guard::ReceivePolicyGuard,
    storage::actions::StorageActions,
    storage_credits::NonCreditableSlots,
    tip_fee_manager::TipFeeManager,
    tip20::{TIP20Token, is_tip20_prefix},
    tip403_registry::TIP403Registry,
};
use zone_primitives::constants::TEMPO_STATE_ADDRESS;

/// Register zone-native and zone-adapted Tempo precompiles.
///
/// Native precompiles use zone-local execution. TIP20, TIP403, and other policy-aware
/// Tempo precompiles use zone pre-execution rules and finalized L1-backed reads.
pub fn extend_zone_precompiles<P: L1StorageReader>(
    precompiles: &mut PrecompilesMap,
    cfg: &CfgEnv<TempoHardfork>,
    l1_reader: P,
    sequencer: Arc<dyn SequencerExt>,
    actions: StorageActions,
    non_creditable_slots: Rc<RefCell<NonCreditableSlots>>,
) {
    let tempo_state_reader = l1_reader.clone();
    let zone_env = L1BackedPrecompileEnv::new(
        cfg,
        l1_reader,
        actions.clone(),
        non_creditable_slots.clone(),
    );
    let tempo_env = PrecompileEnv::new(cfg, actions, non_creditable_slots);

    precompiles.apply_precompile(&TEMPO_STATE_ADDRESS, |_| {
        Some(TempoState::create(tempo_state_reader.clone(), cfg))
    });

    precompiles.apply_precompile(&CHAUM_PEDERSEN_VERIFY_ADDRESS, |_| {
        Some(ChaumPedersenVerify::create_precompile(cfg))
    });
    precompiles.apply_precompile(&AES_GCM_DECRYPT_ADDRESS, |_| {
        Some(AesGcmDecrypt::create(cfg))
    });
    precompiles.apply_precompile(&ZONE_TIP20_FACTORY_ADDRESS, |_| {
        Some(ZoneTokenFactory::create(cfg))
    });

    let registry_env = zone_env.clone();
    precompiles.apply_precompile(&ZONE_TIP403_ADDRESS, move |_| {
        Some(create_tip403_precompile(&registry_env))
    });

    // Replace upstream lookup only where the zone needs call rules or L1-backed state.
    // Static zone-native and TIP403 entries above take priority over this lookup.
    let spec = cfg.spec;
    precompiles.set_precompile_lookup(move |address: &alloy_primitives::Address| {
        if is_tip20_prefix(*address) {
            Some(create_tip20_precompile(
                *address,
                &zone_env,
                sequencer.clone(),
            ))
        } else if *address == TIP_FEE_MANAGER_ADDRESS {
            Some(create_l1_backed_precompile(
                "TipFeeManager",
                zone_env.clone(),
                execution::NoCallRules,
                |data, caller| TipFeeManager::new().call(data, caller),
            ))
        } else if *address == RECEIVE_POLICY_GUARD_ADDRESS && spec.is_t6() {
            Some(create_l1_backed_precompile(
                "ReceivePolicyGuard",
                zone_env.clone(),
                execution::NoCallRules,
                |data, caller| ReceivePolicyGuard::new().call(data, caller),
            ))
        } else if *address == STABLECOIN_DEX_ADDRESS {
            None
        } else if *address == NONCE_PRECOMPILE_ADDRESS {
            Some(NonceManager::create_precompile(&tempo_env))
        } else if *address == ACCOUNT_KEYCHAIN_ADDRESS {
            Some(AccountKeychain::create_precompile(&tempo_env))
        } else {
            None
        }
    });
}

const ZONE_RPC_ERROR_PREFIX: &str = "[zone rpc]";

/// Create a [`PrecompileError::Fatal`] for transient L1 RPC errors.
///
/// Fatal errors propagate out of the EVM as `Err` (instead of a revert),
/// allowing the builder to skip the pool transaction rather than charging gas.
pub fn zone_rpc_error(msg: impl core::fmt::Display) -> PrecompileError {
    PrecompileError::Fatal(alloc::format!("{ZONE_RPC_ERROR_PREFIX} {msg}"))
}

/// Returns `true` if the error string was produced by [`zone_rpc_error`].
pub fn is_zone_rpc_error(err: &str) -> bool {
    err.starts_with(ZONE_RPC_ERROR_PREFIX)
}

impl AesGcmDecrypt {
    /// Create this precompile with ordinary zone-local execution.
    pub fn create(cfg: &CfgEnv<TempoHardfork>) -> DynPrecompile {
        create_local_precompile(
            "AesGcmDecrypt",
            cfg,
            execution::NoCallRules,
            |data, caller| Self.call(data, caller),
        )
    }
}

impl ChaumPedersenVerify {
    /// Create this precompile with ordinary zone-local execution.
    pub fn create_precompile(cfg: &CfgEnv<TempoHardfork>) -> DynPrecompile {
        create_local_precompile(
            "ChaumPedersenVerify",
            cfg,
            execution::NoCallRules,
            |data, caller| Self.call(data, caller),
        )
    }
}

impl TempoState {
    /// Create this precompile with local storage and a direct-call rule.
    pub fn create<P: L1StorageReader>(reader: P, cfg: &CfgEnv<TempoHardfork>) -> DynPrecompile {
        create_local_precompile(
            "TempoState",
            cfg,
            execution::DirectCallOnly,
            move |data, caller| Self::new().call_with_provider(&reader, data, caller),
        )
    }
}

impl ZoneTokenFactory {
    /// Create this precompile with local storage and a direct-call rule.
    pub fn create(cfg: &CfgEnv<TempoHardfork>) -> DynPrecompile {
        create_local_precompile(
            "ZoneTokenFactory",
            cfg,
            execution::DirectCallOnly,
            |data, caller| Self::new().call(data, caller),
        )
    }
}

/// Create upstream TIP403 execution with read-only rules and finalized L1 state.
pub(crate) fn create_tip403_precompile<P: L1StorageReader>(
    env: &L1BackedPrecompileEnv<P>,
) -> DynPrecompile {
    create_l1_backed_precompile(
        "ZoneTip403Registry",
        env.clone(),
        tip403_proxy::TIP403Rules,
        |data, caller| TIP403Registry::new().call(data, caller),
    )
}

/// Create upstream TIP20 execution with zone rules and finalized L1 policy reads.
pub(crate) fn create_tip20_precompile<P: L1StorageReader>(
    address: alloy_primitives::Address,
    env: &L1BackedPrecompileEnv<P>,
    sequencer: Arc<dyn SequencerExt>,
) -> DynPrecompile {
    let checks = ztip20::TIP20Rules::new(sequencer);
    create_l1_backed_precompile(
        "ZoneTip20Token",
        env.clone(),
        checks,
        move |data, caller| TIP20Token::from_address_unchecked(address).call(data, caller),
    )
}
