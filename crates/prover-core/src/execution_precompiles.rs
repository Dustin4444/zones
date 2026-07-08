use alloc::{format, rc::Rc, sync::Arc};
use core::cell::RefCell;

use alloy_evm::precompiles::PrecompilesMap;
use alloy_primitives::{Address, B256, U256};
use tempo_precompiles::{
    ACCOUNT_KEYCHAIN_ADDRESS, NONCE_PRECOMPILE_ADDRESS, PrecompileEnv, STABLECOIN_DEX_ADDRESS,
    TIP_FEE_MANAGER_ADDRESS, account_keychain::AccountKeychain, nonce::NonceManager,
    storage::actions::StorageActions, storage_credits::NonCreditableSlots, tip20::is_tip20_prefix,
};
use tempo_zone_contracts::{TEMPO_STATE_READER_ADDRESS, ZONE_TX_CONTEXT_ADDRESS};
use zone_precompiles::{
    AES_GCM_DECRYPT_ADDRESS, AesGcmDecrypt, CHAUM_PEDERSEN_VERIFY_ADDRESS, ChaumPedersenVerify,
    L1StorageReader, ZONE_TIP20_FACTORY_ADDRESS, ZONE_TIP403_PROXY_ADDRESS, ZoneFeeManager,
    ZonePortalReader, ZoneTip20Token, ZoneTip403ProxyRegistry, ZoneTokenFactory, ZoneTxContext,
};

use crate::{
    OwnedWitnessTempoStateReader, ProverError, WitnessPolicyProvider, WitnessSequencer, ZoneCfgEnv,
};

/// Register Zone precompiles that can execute entirely inside prover-core.
pub fn register_witness_zone_precompiles(
    precompiles: &mut PrecompilesMap,
    cfg: &ZoneCfgEnv,
    tempo_state_reader: OwnedWitnessTempoStateReader,
    sequencer: Address,
    tempo_block_number: u64,
) {
    let policy_provider =
        WitnessPolicyProvider::new(tempo_state_reader.clone(), tempo_block_number);
    precompiles.apply_precompile(&TEMPO_STATE_READER_ADDRESS, |_| {
        Some(tempo_state_reader.into_dyn())
    });
    precompiles.apply_precompile(&ZONE_TX_CONTEXT_ADDRESS, |_| Some(ZoneTxContext::create()));
    precompiles.apply_precompile(&CHAUM_PEDERSEN_VERIFY_ADDRESS, |_| {
        Some(ChaumPedersenVerify.into())
    });
    precompiles.apply_precompile(&AES_GCM_DECRYPT_ADDRESS, |_| Some(AesGcmDecrypt.into()));
    precompiles.apply_precompile(&ZONE_TIP20_FACTORY_ADDRESS, |_| {
        Some(ZoneTokenFactory::create(cfg))
    });
    precompiles.apply_precompile(&ZONE_TIP403_PROXY_ADDRESS, |_| {
        Some(ZoneTip403ProxyRegistry::create(policy_provider.clone()))
    });

    let registry = Some(ZoneTip403ProxyRegistry::new(policy_provider));
    let sequencer = Arc::new(WitnessSequencer::new(sequencer));
    let zone_cfg = cfg.clone();
    precompiles.set_precompile_lookup(move |address: &Address| {
        if is_tip20_prefix(*address) {
            Some(ZoneTip20Token::create(
                *address,
                &zone_cfg,
                registry.clone(),
                sequencer.clone(),
            ))
        } else {
            None
        }
    });
}

/// Register Zone precompiles for the TempoEVM-backed witness executor.
///
/// `TempoEvm` installs Tempo's dynamic precompile lookup by default. Zone
/// execution replaces the TIP-20, fee-manager, and selected protocol entries,
/// so the witness executor must install the same lookup shape instead of using
/// the smaller legacy witness lookup.
pub fn register_witness_zone_precompiles_with_fee_manager<P>(
    precompiles: &mut PrecompilesMap,
    cfg: &ZoneCfgEnv,
    tempo_state_reader: OwnedWitnessTempoStateReader,
    sequencer: Address,
    tempo_block_number: u64,
    fee_provider: P,
) where
    P: ZonePortalReader + Clone + Send + Sync + 'static,
{
    let policy_provider =
        WitnessPolicyProvider::new(tempo_state_reader.clone(), tempo_block_number);
    precompiles.apply_precompile(&TEMPO_STATE_READER_ADDRESS, |_| {
        Some(tempo_state_reader.into_dyn())
    });
    precompiles.apply_precompile(&ZONE_TX_CONTEXT_ADDRESS, |_| Some(ZoneTxContext::create()));
    precompiles.apply_precompile(&CHAUM_PEDERSEN_VERIFY_ADDRESS, |_| {
        Some(ChaumPedersenVerify.into())
    });
    precompiles.apply_precompile(&AES_GCM_DECRYPT_ADDRESS, |_| Some(AesGcmDecrypt.into()));
    precompiles.apply_precompile(&ZONE_TIP20_FACTORY_ADDRESS, |_| {
        Some(ZoneTokenFactory::create(cfg))
    });
    precompiles.apply_precompile(&ZONE_TIP403_PROXY_ADDRESS, |_| {
        Some(ZoneTip403ProxyRegistry::create(policy_provider.clone()))
    });

    let registry = Some(ZoneTip403ProxyRegistry::new(policy_provider));
    let sequencer: Arc<dyn zone_precompiles::SequencerExt> =
        Arc::new(WitnessSequencer::new(sequencer));
    let zone_cfg = cfg.clone();
    let zone_env = PrecompileEnv::new(
        cfg,
        StorageActions::disabled(),
        Rc::new(RefCell::new(NonCreditableSlots::empty())),
    );
    precompiles.set_precompile_lookup(move |address: &Address| {
        if is_tip20_prefix(*address) {
            Some(ZoneTip20Token::create(
                *address,
                &zone_cfg,
                registry.clone(),
                sequencer.clone(),
            ))
        } else if *address == TIP_FEE_MANAGER_ADDRESS {
            Some(ZoneFeeManager::create(fee_provider.clone(), &zone_cfg))
        } else if *address == STABLECOIN_DEX_ADDRESS {
            None
        } else if *address == NONCE_PRECOMPILE_ADDRESS {
            Some(NonceManager::create_precompile(&zone_env))
        } else if *address == ACCOUNT_KEYCHAIN_ADDRESS {
            Some(AccountKeychain::create_precompile(&zone_env))
        } else {
            None
        }
    });
}

#[derive(Debug, Clone)]
pub struct WitnessZonePortalReader {
    tempo_state_reader: OwnedWitnessTempoStateReader,
    portal_address: Address,
}

impl WitnessZonePortalReader {
    pub const fn new(
        tempo_state_reader: OwnedWitnessTempoStateReader,
        portal_address: Address,
    ) -> Self {
        Self {
            tempo_state_reader,
            portal_address,
        }
    }

    fn prover_error(err: ProverError) -> revm::precompile::PrecompileError {
        revm::precompile::PrecompileError::Fatal(format!("{err}"))
    }
}

impl L1StorageReader for WitnessZonePortalReader {
    fn read_l1_storage(
        &self,
        account: Address,
        slot: B256,
        block_number: u64,
    ) -> Result<B256, revm::precompile::PrecompileError> {
        self.tempo_state_reader
            .read_storage_word(block_number, account, U256::from_be_bytes(slot.0))
            .map(|value| B256::from(value.to_be_bytes::<32>()))
            .map_err(Self::prover_error)
    }
}

impl ZonePortalReader for WitnessZonePortalReader {
    fn portal_address(&self) -> Address {
        self.portal_address
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;

    use alloy_evm::{
        EvmInternals,
        eth::EthEvmContext,
        precompiles::{Precompile, PrecompileInput, PrecompilesMap},
    };
    use alloy_primitives::{Address, U256, address};
    use revm::{database::EmptyDB, precompile::Precompiles};
    use tempo_zone_contracts::TEMPO_STATE_READER_ADDRESS;
    use zone_precompiles::{
        AES_GCM_DECRYPT_ADDRESS, CHAUM_PEDERSEN_VERIFY_ADDRESS, ZONE_TIP20_FACTORY_ADDRESS,
        ZONE_TIP403_PROXY_ADDRESS,
    };

    use super::*;
    use crate::{TempoHardfork, TempoWitnessProvider, ZoneCfgEnv};

    fn empty_tempo_provider() -> TempoWitnessProvider {
        let proof = crate::BatchStateProof {
            node_pool: BTreeMap::new(),
            reads: Vec::new(),
        };
        TempoWitnessProvider::new(&proof, 1, &[]).unwrap()
    }

    fn cfg_env() -> ZoneCfgEnv {
        ZoneCfgEnv::new_with_spec_and_gas_params(
            TempoHardfork::T1,
            crate::tempo_gas_params_with_amsterdam(TempoHardfork::T1, false),
        )
    }

    fn precompile_input<'a>(
        ctx: &'a mut EthEvmContext<EmptyDB>,
        data: &'a [u8],
        caller: Address,
    ) -> PrecompileInput<'a> {
        PrecompileInput {
            data,
            gas: 1_000_000,
            reservoir: 0,
            caller,
            value: U256::ZERO,
            is_static: false,
            internals: EvmInternals::from_context(ctx),
            target_address: TEMPO_STATE_READER_ADDRESS,
            bytecode_address: TEMPO_STATE_READER_ADDRESS,
        }
    }

    #[test]
    fn registers_witness_zone_precompiles() {
        let mut precompiles = PrecompilesMap::from_static(Precompiles::prague());
        register_witness_zone_precompiles(
            &mut precompiles,
            &cfg_env(),
            OwnedWitnessTempoStateReader::new(empty_tempo_provider(), 0),
            address!("0x00000000000000000000000000000000000000a1"),
            0,
        );

        assert!(precompiles.get(&TEMPO_STATE_READER_ADDRESS).is_some());
        assert!(precompiles.get(&CHAUM_PEDERSEN_VERIFY_ADDRESS).is_some());
        assert!(precompiles.get(&AES_GCM_DECRYPT_ADDRESS).is_some());
        assert!(precompiles.get(&ZONE_TIP20_FACTORY_ADDRESS).is_some());
        assert!(precompiles.get(&ZONE_TIP403_PROXY_ADDRESS).is_some());
    }

    #[test]
    fn registered_tempo_reader_uses_witness_provider() {
        let mut precompiles = PrecompilesMap::from_static(Precompiles::prague());
        register_witness_zone_precompiles(
            &mut precompiles,
            &cfg_env(),
            OwnedWitnessTempoStateReader::new(empty_tempo_provider(), 0),
            address!("0x00000000000000000000000000000000000000a1"),
            0,
        );

        let mut ctx = EthEvmContext::new(EmptyDB::default(), Default::default());
        let precompile = precompiles
            .get(&TEMPO_STATE_READER_ADDRESS)
            .expect("registered Tempo state reader");
        let output = Precompile::call(
            &precompile,
            precompile_input(
                &mut ctx,
                &[],
                address!("0x0000000000000000000000000000000000000001"),
            ),
        )
        .unwrap();

        assert!(output.is_revert());
    }
}
