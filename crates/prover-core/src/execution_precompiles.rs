use alloc::sync::Arc;

use alloy_evm::precompiles::PrecompilesMap;
use alloy_primitives::Address;
use tempo_precompiles::tip20::is_tip20_prefix;
use tempo_zone_contracts::TEMPO_STATE_READER_ADDRESS;
use zone_precompiles::{
    AES_GCM_DECRYPT_ADDRESS, AesGcmDecrypt, CHAUM_PEDERSEN_VERIFY_ADDRESS, ChaumPedersenVerify,
    ZONE_TIP20_FACTORY_ADDRESS, ZONE_TIP403_PROXY_ADDRESS, ZoneTip20Token, ZoneTip403ProxyRegistry,
    ZoneTokenFactory,
};

use crate::{OwnedWitnessTempoStateReader, WitnessPolicyProvider, WitnessSequencer, ZoneCfgEnv};

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
