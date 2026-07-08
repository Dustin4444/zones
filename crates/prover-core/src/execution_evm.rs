use alloc::{borrow::Cow, string::ToString};
use core::ops::{Deref, DerefMut};

use alloy_evm::{
    Database, Evm, EvmEnv, EvmFactory,
    eth::{EthBlockExecutionCtx, spec::EthExecutorSpec},
    precompiles::PrecompilesMap,
};
use alloy_hardforks::{EthereumHardfork, EthereumHardforks, ForkCondition};
use alloy_primitives::{Address, Bytes, TxKind};
use revm::{
    Context, ExecuteEvm, InspectEvm, InspectSystemCallEvm, Inspector, MainBuilder, MainContext,
    SystemCallEvm,
    context::{CfgEnv, DBErrorMarker, Evm as RevmEvm},
    context_interface::result::{
        EVMError, ExecutionResult, HaltReason, InvalidTransaction, ResultAndState, ResultGas,
    },
    handler::{EthFrame, PrecompileProvider, instructions::EthInstructions},
    inspector::NoOpInspector,
    interpreter::{InterpreterResult, interpreter::EthInterpreter},
    precompile::{PrecompileSpecId, Precompiles},
};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_evm::{TempoBlockEnv, evm::TempoEvm};
use tempo_primitives::TempoReceipt;

use crate::{
    AlloyZoneBlockExecutorProvider, OwnedWitnessTempoStateReader, WitnessTempoStateReader,
    ZoneAlloyBlockExecutor, ZoneBlockEnv, ZoneBlockExecutionInput, ZoneEvmEnv, ZoneExecutionState,
    ZoneTxEnv,
    execution_precompiles::{
        WitnessZonePortalReader, register_witness_zone_precompiles_with_fee_manager,
    },
    register_witness_zone_precompiles,
};

pub type ZoneEvmContext<DB> = Context<ZoneBlockEnv, ZoneTxEnv, CfgEnv<TempoHardfork>, DB>;

pub type ZoneRevm<DB, I> = RevmEvm<
    ZoneEvmContext<DB>,
    I,
    EthInstructions<EthInterpreter, ZoneEvmContext<DB>>,
    PrecompilesMap,
    EthFrame<EthInterpreter>,
>;

#[derive(Debug)]
pub struct ZoneWitnessEvm<DB: Database, I = NoOpInspector> {
    inner: ZoneRevm<DB, I>,
    inspect: bool,
}

impl<DB: Database> ZoneWitnessEvm<DB, NoOpInspector> {
    pub fn new(
        db: DB,
        input: EvmEnv<TempoHardfork, ZoneBlockEnv>,
        precompiles: PrecompilesMap,
    ) -> Self {
        Self::new_with_inspector(db, input, NoOpInspector {}, false, precompiles)
    }
}

impl<DB: Database, I> ZoneWitnessEvm<DB, I> {
    pub fn new_with_inspector(
        db: DB,
        input: EvmEnv<TempoHardfork, ZoneBlockEnv>,
        inspector: I,
        inspect: bool,
        precompiles: PrecompilesMap,
    ) -> Self
    where
        I: Inspector<ZoneEvmContext<DB>>,
    {
        let inner = Context::mainnet()
            .with_db(db)
            .with_block(input.block_env)
            .with_cfg(input.cfg_env)
            .with_tx(Default::default())
            .build_mainnet_with_inspector(inspector)
            .with_precompiles(precompiles);

        Self { inner, inspect }
    }

    pub const fn ctx(&self) -> &ZoneEvmContext<DB> {
        &self.inner.ctx
    }

    pub const fn ctx_mut(&mut self) -> &mut ZoneEvmContext<DB> {
        &mut self.inner.ctx
    }

    pub fn into_inner(self) -> ZoneRevm<DB, I> {
        self.inner
    }
}

impl<DB, I> Deref for ZoneWitnessEvm<DB, I>
where
    DB: Database,
    I: Inspector<ZoneEvmContext<DB>>,
{
    type Target = ZoneEvmContext<DB>;

    fn deref(&self) -> &Self::Target {
        self.ctx()
    }
}

impl<DB, I> DerefMut for ZoneWitnessEvm<DB, I>
where
    DB: Database,
    I: Inspector<ZoneEvmContext<DB>>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_mut()
    }
}

impl<DB, I> Evm for ZoneWitnessEvm<DB, I>
where
    DB: Database,
    I: Inspector<ZoneEvmContext<DB>>,
    PrecompilesMap: PrecompileProvider<ZoneEvmContext<DB>, Output = InterpreterResult>,
{
    type DB = DB;
    type Tx = ZoneTxEnv;
    type Error = EVMError<DB::Error>;
    type HaltReason = HaltReason;
    type Spec = TempoHardfork;
    type BlockEnv = ZoneBlockEnv;
    type Precompiles = PrecompilesMap;
    type Inspector = I;

    fn block(&self) -> &Self::BlockEnv {
        &self.block
    }

    fn cfg_env(&self) -> &CfgEnv<Self::Spec> {
        &self.cfg
    }

    fn chain_id(&self) -> u64 {
        self.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        if tx.is_system_tx {
            let TxKind::Call(to) = tx.inner.kind else {
                return Err(InvalidTransaction::Str(Cow::Borrowed(
                    "system transaction must be a call",
                ))
                .into());
            };

            let mut result = if self.inspect {
                self.inner
                    .inspect_system_call_with_caller(tx.inner.caller, to, tx.inner.data)?
            } else {
                self.inner
                    .system_call_with_caller(tx.inner.caller, to, tx.inner.data)?
            };

            match &mut result.result {
                ExecutionResult::Success { gas, .. } => {
                    *gas = ResultGas::default();
                }
                _ => {
                    return Err(EVMError::Custom(
                        "system transaction execution failed".to_string(),
                    ));
                }
            }

            Ok(result)
        } else if self.inspect {
            self.inner.inspect_tx(tx)
        } else {
            self.inner.transact(tx)
        }
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.inner.system_call_with_caller(caller, contract, data)
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec, Self::BlockEnv>) {
        let Context {
            block: block_env,
            cfg: cfg_env,
            journaled_state,
            ..
        } = self.inner.ctx;

        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        (
            &self.inner.ctx.journaled_state.database,
            &self.inner.inspector,
            &self.inner.precompiles,
        )
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        (
            &mut self.inner.ctx.journaled_state.database,
            &mut self.inner.inspector,
            &mut self.inner.precompiles,
        )
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ZoneWitnessEvmFactory;

impl ZoneWitnessEvmFactory {
    pub fn default_precompiles(spec: TempoHardfork) -> PrecompilesMap {
        PrecompilesMap::from_static(Precompiles::new(PrecompileSpecId::from_spec_id(
            spec.into(),
        )))
    }

    pub fn create_evm_with_precompiles<DB: Database>(
        &self,
        db: DB,
        input: EvmEnv<TempoHardfork, ZoneBlockEnv>,
        precompiles: PrecompilesMap,
    ) -> ZoneWitnessEvm<DB, NoOpInspector> {
        ZoneWitnessEvm::new(db, input, precompiles)
    }

    pub fn create_evm_with_inspector_and_precompiles<
        DB: Database,
        I: Inspector<ZoneEvmContext<DB>>,
    >(
        &self,
        db: DB,
        input: EvmEnv<TempoHardfork, ZoneBlockEnv>,
        inspector: I,
        precompiles: PrecompilesMap,
    ) -> ZoneWitnessEvm<DB, I> {
        ZoneWitnessEvm::new_with_inspector(db, input, inspector, true, precompiles)
    }
}

impl EvmFactory for ZoneWitnessEvmFactory {
    type Evm<DB: Database, I: Inspector<Self::Context<DB>>> = ZoneWitnessEvm<DB, I>;
    type Context<DB: Database> = ZoneEvmContext<DB>;
    type Tx = ZoneTxEnv;
    type Error<DBError: DBErrorMarker> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Spec = TempoHardfork;
    type BlockEnv = ZoneBlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(
        &self,
        db: DB,
        input: EvmEnv<Self::Spec, Self::BlockEnv>,
    ) -> Self::Evm<DB, NoOpInspector> {
        let precompiles = Self::default_precompiles(input.cfg_env.spec);
        self.create_evm_with_precompiles(db, input, precompiles)
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv<Self::Spec, Self::BlockEnv>,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let precompiles = Self::default_precompiles(input.cfg_env.spec);
        self.create_evm_with_inspector_and_precompiles(db, input, inspector, precompiles)
    }
}

/// Zone block executor spec for Alloy's Ethereum block driver.
///
/// Zone system transactions are explicit witness transactions, so this disables
/// Ethereum-only block-boundary state changes such as EIP-2935, EIP-4788,
/// EIP-6110 deposit requests, Shanghai withdrawals, and DAO balance changes.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZoneEthExecutorSpec;

impl EthereumHardforks for ZoneEthExecutorSpec {
    fn ethereum_fork_activation(&self, fork: EthereumHardfork) -> ForkCondition {
        match fork {
            EthereumHardfork::Dao
            | EthereumHardfork::Bpo1
            | EthereumHardfork::Bpo2
            | EthereumHardfork::Bpo3
            | EthereumHardfork::Bpo4
            | EthereumHardfork::Bpo5
            | EthereumHardfork::Amsterdam => ForkCondition::Never,
            EthereumHardfork::Shanghai
            | EthereumHardfork::Cancun
            | EthereumHardfork::Prague
            | EthereumHardfork::Osaka => ForkCondition::Timestamp(0),
            _ => ForkCondition::Block(0),
        }
    }
}

impl EthExecutorSpec for ZoneEthExecutorSpec {
    fn deposit_contract_address(&self) -> Option<Address> {
        None
    }
}

pub fn zone_witness_precompiles(
    evm_env: &ZoneEvmEnv,
    tempo_state_reader: WitnessTempoStateReader<'_>,
    sequencer: Address,
    tempo_block_number: u64,
) -> PrecompilesMap {
    let mut precompiles = ZoneWitnessEvmFactory::default_precompiles(evm_env.cfg_env.spec);
    register_witness_zone_precompiles(
        &mut precompiles,
        &evm_env.cfg_env,
        OwnedWitnessTempoStateReader::from_reader(tempo_state_reader),
        sequencer,
        tempo_block_number,
    );
    precompiles
}

#[derive(Debug, Clone, Default)]
pub struct WitnessZoneBlockExecutorProvider {
    executor_spec: ZoneEthExecutorSpec,
    portal_address: Address,
}

impl WitnessZoneBlockExecutorProvider {
    pub const fn new() -> Self {
        Self {
            executor_spec: ZoneEthExecutorSpec,
            portal_address: Address::ZERO,
        }
    }

    pub const fn with_portal_address(mut self, portal_address: Address) -> Self {
        self.portal_address = portal_address;
        self
    }
}

impl AlloyZoneBlockExecutorProvider for WitnessZoneBlockExecutorProvider {
    type Receipt = TempoReceipt;
    type Executor<'a> =
        ZoneAlloyBlockExecutor<'a, TempoEvm<&'a mut ZoneExecutionState>, ZoneEthExecutorSpec>;

    fn create_executor<'a>(
        &'a mut self,
        state: &'a mut ZoneExecutionState,
        input: &ZoneBlockExecutionInput<'_>,
    ) -> Result<Self::Executor<'a>, crate::ProverError> {
        let tempo_state_reader =
            OwnedWitnessTempoStateReader::from_reader(input.tempo_state_reader);
        let fee_provider =
            WitnessZonePortalReader::new(tempo_state_reader.clone(), self.portal_address);
        let mut evm = TempoEvm::new(&mut *state, tempo_evm_env(input))
            .with_fee_manager(zone_precompiles::ZoneFeeManager::new(fee_provider.clone()));
        let (_, _, precompiles) = evm.components_mut();
        register_witness_zone_precompiles_with_fee_manager(
            precompiles,
            &input.evm_env.cfg_env,
            tempo_state_reader,
            input.block.beneficiary,
            input.block.tempo_block_number,
            fee_provider,
        );
        let ctx: EthBlockExecutionCtx<'a> = input.execution_context.inner.clone();

        Ok(ZoneAlloyBlockExecutor::new(evm, ctx, self.executor_spec))
    }
}

fn tempo_evm_env(input: &ZoneBlockExecutionInput<'_>) -> EvmEnv<TempoHardfork, TempoBlockEnv> {
    EvmEnv {
        cfg_env: input.evm_env.cfg_env.clone(),
        block_env: TempoBlockEnv {
            inner: input.evm_env.block_env.inner.clone(),
            timestamp_millis_part: input.evm_env.block_env.timestamp_millis_part,
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;

    use alloy_evm::Evm as _;
    use revm::database::EmptyDB;
    use tempo_zone_contracts::TEMPO_STATE_READER_ADDRESS;
    use zone_precompiles::{
        AES_GCM_DECRYPT_ADDRESS, CHAUM_PEDERSEN_VERIFY_ADDRESS, ZONE_TIP20_FACTORY_ADDRESS,
        ZONE_TIP403_PROXY_ADDRESS,
    };

    use super::*;
    use crate::{BatchStateProof, TempoWitnessProvider, ZONE_NO_BLOB_GAS, ZoneCfgEnv};

    fn evm_env() -> ZoneEvmEnv {
        ZoneEvmEnv {
            cfg_env: ZoneCfgEnv::new_with_spec_and_gas_params(
                TempoHardfork::T1,
                crate::tempo_gas_params_with_amsterdam(TempoHardfork::T1, false),
            ),
            block_env: ZoneBlockEnv {
                inner: revm::context::BlockEnv {
                    gas_limit: 30_000_000,
                    basefee: 0,
                    blob_excess_gas_and_price: Some(ZONE_NO_BLOB_GAS),
                    ..Default::default()
                },
                timestamp_millis_part: 0,
            },
        }
    }

    fn empty_tempo_provider() -> TempoWitnessProvider {
        let proof = BatchStateProof {
            node_pool: BTreeMap::new(),
            reads: Vec::new(),
        };
        TempoWitnessProvider::new(&proof, 1, &[]).unwrap()
    }

    #[test]
    fn factory_constructs_zone_typed_evm() {
        let input = evm_env();
        let evm = ZoneWitnessEvmFactory.create_evm(EmptyDB::default(), input.clone());

        assert_eq!(evm.chain_id(), input.cfg_env.chain_id);
        assert_eq!(evm.block().inner.gas_limit, 30_000_000);
    }

    #[test]
    fn witness_precompiles_include_zone_entries() {
        let provider = empty_tempo_provider();
        let reader = WitnessTempoStateReader::new(&provider, 0);
        let precompiles = zone_witness_precompiles(&evm_env(), reader, Address::ZERO, 0);

        assert!(precompiles.get(&TEMPO_STATE_READER_ADDRESS).is_some());
        assert!(precompiles.get(&CHAUM_PEDERSEN_VERIFY_ADDRESS).is_some());
        assert!(precompiles.get(&AES_GCM_DECRYPT_ADDRESS).is_some());
        assert!(precompiles.get(&ZONE_TIP20_FACTORY_ADDRESS).is_some());
        assert!(precompiles.get(&ZONE_TIP403_PROXY_ADDRESS).is_some());
    }

    #[test]
    fn zone_executor_spec_disables_ethereum_only_state_changes() {
        let spec = ZoneEthExecutorSpec;

        assert!(spec.is_prague_active_at_timestamp(0));
        assert!(spec.is_shanghai_active_at_timestamp(0));
        assert_eq!(spec.deposit_contract_address(), None);
    }
}
