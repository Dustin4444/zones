//! Registration helpers for the full zone precompile set.

use alloc::sync::Arc;

use alloy_evm::precompiles::PrecompilesMap;
use alloy_primitives::Address;
use revm::context::CfgEnv;
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_precompiles::{
    ACCOUNT_KEYCHAIN_ADDRESS, NONCE_PRECOMPILE_ADDRESS, STABLECOIN_DEX_ADDRESS,
    TIP_FEE_MANAGER_ADDRESS, account_keychain::AccountKeychain, nonce::NonceManager,
    tip_fee_manager::TipFeeManager, tip20::is_tip20_prefix,
};
use zone_primitives::constants::{TEMPO_STATE_READER_ADDRESS, ZONE_TX_CONTEXT_ADDRESS};

use crate::{
    AES_GCM_DECRYPT_ADDRESS, AesGcmDecrypt, CHAUM_PEDERSEN_VERIFY_ADDRESS, ChaumPedersenVerify,
    SequencerExt, TempoStateReader, TempoStateReaderProvider, ZONE_TIP20_FACTORY_ADDRESS,
    ZONE_TIP403_PROXY_ADDRESS, ZoneTip20Token, ZoneTip403ProxyRegistry, ZoneTokenFactory,
    ZoneTxContext, policy::PolicyCheck,
};

/// Registers zone-specific precompiles into an existing [`PrecompilesMap`].
///
/// This is the zone counterpart to `tempo_precompiles::extend_tempo_precompiles`: callers provide
/// the already-created map and hardfork config, and this helper installs the zone precompile set
/// in-place. It registers static zone addresses for TempoStateReader, ZoneTxContext,
/// Chaum-Pedersen verification, AES-GCM decryption, the zone TIP-20 factory, and the optional
/// TIP-403 proxy.
///
/// The helper also replaces the dynamic lookup with zone-aware behavior:
///
/// - TIP-20 token addresses are routed to [`ZoneTip20Token`] so zone policy, privacy,
///   fixed-gas, and bridge-auth rules are enforced before delegating to vanilla TIP-20 logic.
/// - Tempo precompiles that remain valid on zones (`TipFeeManager`, `NonceManager`, and
///   `AccountKeychain`) are preserved.
/// - `StablecoinDEX` is intentionally disabled because zones should not expose the L1 DEX
///   precompile.
///
/// Static map entries installed by `apply_precompile` take priority over the dynamic lookup, so
/// zone overrides such as [`ZoneTokenFactory`] and [`ZoneTip403ProxyRegistry`] win over generic
/// TIP-20 prefix matching.
pub fn extend_zone_precompiles<L1, P>(
    precompiles: &mut PrecompilesMap,
    cfg: &CfgEnv<TempoHardfork>,
    l1_provider: L1,
    policy_provider: Option<P>,
) where
    L1: TempoStateReaderProvider + SequencerExt + Clone + Send + Sync + 'static,
    P: PolicyCheck + Clone + Send + Sync + 'static,
{
    precompiles.apply_precompile(&TEMPO_STATE_READER_ADDRESS, |_| {
        Some(TempoStateReader::create(l1_provider.clone()))
    });
    precompiles.apply_precompile(&ZONE_TX_CONTEXT_ADDRESS, |_| Some(ZoneTxContext::create()));
    precompiles.apply_precompile(&CHAUM_PEDERSEN_VERIFY_ADDRESS, |_| {
        Some(ChaumPedersenVerify.into())
    });
    precompiles.apply_precompile(&AES_GCM_DECRYPT_ADDRESS, |_| Some(AesGcmDecrypt.into()));
    precompiles.apply_precompile(&ZONE_TIP20_FACTORY_ADDRESS, |_| {
        Some(ZoneTokenFactory::create(cfg))
    });

    let registry = policy_provider.clone().map(ZoneTip403ProxyRegistry::new);
    let sequencer: Arc<dyn SequencerExt> = Arc::new(l1_provider);

    if let Some(provider) = policy_provider {
        precompiles.apply_precompile(&ZONE_TIP403_PROXY_ADDRESS, |_| {
            Some(ZoneTip403ProxyRegistry::create(provider))
        });
    }

    let cfg = cfg.clone();
    precompiles.set_precompile_lookup(move |address: &Address| {
        if is_tip20_prefix(*address) {
            Some(ZoneTip20Token::create(
                *address,
                &cfg,
                registry.clone(),
                sequencer.clone(),
            ))
        } else if *address == TIP_FEE_MANAGER_ADDRESS {
            Some(TipFeeManager::create_precompile(&cfg))
        } else if *address == STABLECOIN_DEX_ADDRESS {
            None
        } else if *address == NONCE_PRECOMPILE_ADDRESS {
            Some(NonceManager::create_precompile(&cfg))
        } else if *address == ACCOUNT_KEYCHAIN_ADDRESS {
            Some(AccountKeychain::create_precompile(&cfg))
        } else {
            None
        }
    });
}
