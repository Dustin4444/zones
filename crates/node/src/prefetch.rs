//! Node adapter for canonical Tempo deposit policy reads.
//!
//! The L1 crate owns event planning, dependency waves, and concurrency. This module supplies the
//! node-only boundary: opening parent Zone state, applying the real child checkpoint to an
//! ephemeral overlay, and evaluating one policy operation against that speculative child state.

use std::fmt::Debug;

use alloy_consensus::BlockHeader as _;
use alloy_eips::eip4895::Withdrawals;
use alloy_primitives::{Address, B256, Bytes};
use alloy_sol_types::SolCall as _;
use reth_evm::{ConfigureEvm as _, Evm as _, EvmFactory as _, NextBlockEnvAttributes};
use reth_provider::StateProviderFactory;
use reth_revm::{DatabaseCommit as _, State, database::StateProviderDatabase};
use tempo_evm::TempoNextBlockEnvAttributes;
use tempo_precompiles::{
    storage::{StorageActions, StorageCtx},
    tip20::TIP20Token,
    tip403_registry::{AuthRole, TIP403Registry},
};
use tempo_zone_contracts::{TEMPO_STATE_ADDRESS, TempoState, ZONE_INBOX_ADDRESS};
use zone_evm::ZoneEvmConfig;
use zone_l1::state::{PolicyCheckExecutor, PrefetchCtx};

/// Executes canonical Tempo L1 policy reads over independent throwaway Zone EVMs.
///
/// The executor is intentionally stateless per block. [`PrefetchCtx`] is supplied with each call so
/// one shared instance can safely serve concurrent checks without mutable anchor or block state.
#[derive(Debug, Clone)]
pub struct L1PolicyExecutor<P> {
    pub provider: P,
    pub evm_config: ZoneEvmConfig,
}

impl<P> L1PolicyExecutor<P>
where
    P: StateProviderFactory + Clone + Send + Sync + 'static,
{
    /// Execute read-only operations at the next L1 anchor to prefetch L1-backed state.
    ///
    /// The anchor update is committed to an ephemeral [`State`], leaving the provider untouched.
    /// A new EVM discovers the anchor from `TempoState` storage through the production read path.
    fn with_next_l1_anchor<T, E>(
        &self,
        ctx: &PrefetchCtx,
        read_only_call: impl FnOnce() -> Result<T, E>,
    ) -> eyre::Result<T>
    where
        E: std::fmt::Display,
    {
        let provider = self.provider.state_by_block_hash(ctx.parent.hash())?;
        let db = StateProviderDatabase::new(provider.as_ref());
        let mut state = State::builder().with_database(db).build();
        let attributes = TempoNextBlockEnvAttributes {
            inner: NextBlockEnvAttributes {
                timestamp: ctx.child.timestamp(),
                suggested_fee_recipient: Address::ZERO,
                prev_randao: B256::ZERO,
                gas_limit: ctx.parent.gas_limit(),
                parent_beacon_block_root: None,
                withdrawals: Some(Withdrawals::default()),
                extra_data: Default::default(),
                slot_number: None,
            },
            general_gas_limit: 0,
            shared_gas_limit: ctx.parent.gas_limit(),
            timestamp_millis_part: ctx.child.timestamp_millis_part,
            consensus_context: None,
            subblock_fee_recipients: Default::default(),
        };
        let env = self.evm_config.next_evm_env(&ctx.parent, &attributes)?;

        // Execute the canonical anchor update against the Zone parent state.
        let anchored_state = {
            let mut parent_evm = self
                .evm_config
                .evm_factory()
                .create_evm(&mut state, env.clone());
            let call = TempoState::finalizeTempoCall {
                header: Bytes::from(alloy_rlp::encode(ctx.child.header())),
            };
            let anchor_update = parent_evm.transact_system_call(
                ZONE_INBOX_ADDRESS,
                TEMPO_STATE_ADDRESS,
                call.abi_encode().into(),
            )?;
            if !anchor_update.result.is_success() {
                eyre::bail!(
                    "simulated Tempo anchor update failed: {:?}",
                    anchor_update.result
                );
            }
            anchor_update.state
        };

        // Commit only to the ephemeral overlay so policy reads observe the simulated child state.
        state.commit(anchored_state);

        let mut child_evm = self.evm_config.evm_factory().create_evm(&mut state, env);
        StorageCtx::enter_ctx(child_evm.ctx_mut(), StorageActions::disabled(), || {
            read_only_call()
        })
        .map_err(|error| eyre::eyre!(error.to_string()))
    }
}

impl<P> PolicyCheckExecutor for L1PolicyExecutor<P>
where
    P: StateProviderFactory + Clone + Debug + Send + Sync + 'static,
{
    fn transfer_policy(&self, ctx: &PrefetchCtx, token: Address) -> eyre::Result<u64> {
        self.with_next_l1_anchor(ctx, || {
            TIP20Token::from_address(token).and_then(|token| token.transfer_policy_id())
        })
    }

    fn is_mint_authorized(
        &self,
        ctx: &PrefetchCtx,
        policy_id: u64,
        recipient: Address,
    ) -> eyre::Result<bool> {
        self.with_next_l1_anchor(ctx, || {
            TIP403Registry::new().is_authorized_as(policy_id, recipient, AuthRole::mint_recipient())
        })
    }

    fn validate_receive_policy(
        &self,
        ctx: &PrefetchCtx,
        token: Address,
        recipient: Address,
    ) -> eyre::Result<()> {
        self.with_next_l1_anchor(ctx, || {
            TIP403Registry::new()
                .validate_receive_policy(token, ZONE_INBOX_ADDRESS, recipient)
                .map(|_| ())
        })
    }
}
