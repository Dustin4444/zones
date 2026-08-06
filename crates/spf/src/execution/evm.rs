//! Tempo EVM setup and Zone-block execution.

use alloy_consensus::{
    Signed, TxLegacy,
    transaction::{Recovered, SignerRecoverable as _},
};
use alloy_eips::{eip2718::Decodable2718 as _, eip4895::Withdrawals};
use alloy_evm::{
    Evm as _,
    block::{BlockExecutor, TxResult as _},
};
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_rlp::Decodable as _;
use alloy_sol_types::SolCall as _;
use reth_chainspec::EthereumHardforks as _;
use reth_evm::{
    ConfigureEvm as _, NextBlockEnvAttributes,
    execute::{BlockBuilder, BlockBuilderOutcome},
};
use revm::{
    database::{State, states::bundle_state::BundleRetention},
    database_interface::bal::EvmDatabaseError,
};
use tempo_evm::TempoNextBlockEnvAttributes;
use tempo_primitives::{
    TempoHeader, TempoTxEnvelope,
    transaction::envelope::{TEMPO_SYSTEM_TX_SENDER, TEMPO_SYSTEM_TX_SIGNATURE},
};
use tempo_zone_contracts::{IZoneInbox, IZoneOutbox, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS};
use zone_evm::ZoneEvmConfig;
use zone_primitives::constants::zone_chain_id;

use crate::{
    Error, SpfConfig, ZoneBlock,
    execution::database::{TempoWitnessDatabase, WitnessDatabase},
};

type ZoneState = State<WitnessDatabase>;

pub(crate) struct BlockReplayContext<'a> {
    pub(crate) parent: &'a TempoHeader,
    pub(crate) block_index: usize,
    pub(crate) zone_id: u32,
    pub(crate) portal: Address,
}

/// Execute a complete Zone block in system-then-user order.
///
/// When a Tempo header is present, `ZoneInbox.advanceTempo` executes first.
/// That call invokes `TempoState.finalizeTempo`, then processes deposits and
/// enabled tokens. User transactions run only after that system transition.
pub(crate) fn execute_zone_block(
    config: &SpfConfig,
    zone_state: &mut ZoneState,
    tempo_database: &TempoWitnessDatabase,
    replay: BlockReplayContext<'_>,
    block: &ZoneBlock,
) -> Result<BlockBuilderOutcome<tempo_primitives::TempoPrimitives>, Error> {
    let BlockReplayContext {
        parent,
        block_index: zone_block_index,
        zone_id,
        portal,
    } = replay;
    let user_transactions = decode_user_transactions(zone_block_index, &block.transactions)?;
    let parent_number = block
        .number
        .checked_sub(1)
        .ok_or(Error::BlockNumberOverflow)?;
    if let Some(existing) = zone_state.block_hashes.get(parent_number)
        && existing != block.parent_hash
    {
        return Err(crate::WitnessDatabaseError::ConflictingBlockHash {
            number: parent_number,
            expected: existing,
            actual: block.parent_hash,
        }
        .into());
    }
    zone_state
        .block_hashes
        .insert(parent_number, block.parent_hash);

    let evm_config = ZoneEvmConfig::new(
        config.zone_chain_spec.clone(),
        config.zone_chain_spec.inner.clone(),
        tempo_database.clone(),
        portal,
    );
    let attributes = next_block_env_attributes(evm_config.chain_spec(), parent, block)?;
    let sealed_parent =
        reth_primitives_traits::SealedHeader::new(parent.clone(), block.parent_hash);
    let mut evm_env = evm_config
        .next_evm_env(parent, &attributes)
        .map_err(|_| Error::EvmEnvironment)?;
    // The Zone ID is verifier-bound independently of the parent Tempo chain specification.
    evm_env.cfg_env.chain_id = zone_chain_id(zone_id);
    let evm = evm_config.evm_with_env(&mut *zone_state, evm_env);
    let execution_context = evm_config
        .context_for_next_block(&sealed_parent, attributes)
        .map_err(|_| Error::EvmEnvironment)?;
    let mut builder = evm_config.create_block_builder(evm, &sealed_parent, execution_context);

    builder.apply_pre_execution_changes().map_err(|error| {
        map_block_execution_error(
            error,
            Error::BlockPreExecution {
                block_index: zone_block_index,
            },
        )
    })?;

    execute_advance_tempo(
        &mut builder,
        &block.tempo_header_rlp,
        block,
        zone_block_index,
    )?;
    execute_user_transactions(&mut builder, zone_block_index, user_transactions)?;
    if let Some(count) = block.finalize_withdrawal_batch_count {
        execute_finalize_withdrawal_batch(
            &mut builder,
            count,
            block.number,
            block.finalize_withdrawal_batch_encrypted_senders.clone(),
            zone_block_index,
        )?;
    }

    // BasicBlockBuilder normally computes the root through a full StateProvider. SPF has only a
    // witness-backed sparse trie, so preserve its root calculation and provide it to finish.
    let state_root = {
        let state = builder.evm_mut().components_mut().0;
        state.merge_transitions(BundleRetention::Reverts);
        state.database.state_root(&state.bundle_state)?
    };
    let state_provider = reth_storage_api::noop::NoopProvider::<
        tempo_chainspec::TempoChainSpec,
        tempo_primitives::TempoPrimitives,
    >::new(config.zone_chain_spec.inner.clone());
    builder
        .finish(&state_provider, Some((state_root, Default::default())))
        .map_err(|error| {
            map_block_execution_error(
                error,
                Error::BlockPostExecution {
                    block_index: zone_block_index,
                },
            )
        })
}

/// Construct the same next-block attributes supplied by the production Zone
/// payload builder.
pub(crate) fn next_block_env_attributes(
    chain_spec: &zone_chainspec::ZoneChainSpec,
    parent: &TempoHeader,
    block: &ZoneBlock,
) -> Result<TempoNextBlockEnvAttributes, Error> {
    let block_gas_limit = parent.inner.gas_limit;

    let mut encoded = block.tempo_header_rlp.as_ref();
    let header = TempoHeader::decode(&mut encoded)
        .map_err(|_| crate::WitnessDatabaseError::InvalidTempoHeader)?;
    if !encoded.is_empty() {
        return Err(crate::WitnessDatabaseError::InvalidTempoHeader.into());
    }

    Ok(zone_evm::next_block_env_attributes(
        NextBlockEnvAttributes {
            timestamp: block.timestamp,
            suggested_fee_recipient: block.beneficiary,
            prev_randao: B256::ZERO,
            gas_limit: block_gas_limit,
            parent_beacon_block_root: chain_spec
                .is_cancun_active_at_timestamp(block.timestamp)
                .then_some(B256::ZERO),
            withdrawals: chain_spec
                .is_shanghai_active_at_timestamp(block.timestamp)
                .then_some(Withdrawals::default()),
            extra_data: Bytes::new(),
            slot_number: None,
        },
        header.timestamp_millis_part,
    ))
}

fn execute_advance_tempo<B>(
    builder: &mut B,
    header: &Bytes,
    block: &ZoneBlock,
    block_index: usize,
) -> Result<(), Error>
where
    B: BlockBuilder,
    B::Executor: BlockExecutor<Transaction = TempoTxEnvelope>,
{
    let calldata = IZoneInbox::advanceTempoCall {
        header: header.clone(),
        deposits: block.deposits.clone(),
        decryptions: block.decryptions.clone(),
        enabledTokens: block.enabled_tokens.clone(),
    }
    .abi_encode();
    let transaction = TxLegacy {
        chain_id: None,
        nonce: 0,
        gas_price: 0,
        gas_limit: 0,
        to: ZONE_INBOX_ADDRESS.into(),
        value: U256::ZERO,
        input: calldata.into(),
    };
    let transaction =
        TempoTxEnvelope::Legacy(Signed::new_unhashed(transaction, TEMPO_SYSTEM_TX_SIGNATURE));
    let recovered = Recovered::new_unchecked(transaction.clone(), TEMPO_SYSTEM_TX_SENDER);

    execute_recovered_transaction(
        builder,
        recovered,
        Error::AdvanceTempoExecution { block_index },
        true,
    )
}

fn execute_finalize_withdrawal_batch<B>(
    builder: &mut B,
    count: U256,
    block_number: u64,
    encrypted_senders: Vec<Bytes>,
    block_index: usize,
) -> Result<(), Error>
where
    B: BlockBuilder,
    B::Executor: BlockExecutor<Transaction = TempoTxEnvelope>,
{
    let calldata = IZoneOutbox::finalizeWithdrawalBatchCall {
        count,
        blockNumber: block_number,
        encryptedSenders: encrypted_senders,
    }
    .abi_encode();
    let transaction = TxLegacy {
        chain_id: None,
        nonce: 0,
        gas_price: 0,
        gas_limit: 0,
        to: ZONE_OUTBOX_ADDRESS.into(),
        value: U256::ZERO,
        input: calldata.into(),
    };
    let transaction =
        TempoTxEnvelope::Legacy(Signed::new_unhashed(transaction, TEMPO_SYSTEM_TX_SIGNATURE));
    let recovered = Recovered::new_unchecked(transaction.clone(), TEMPO_SYSTEM_TX_SENDER);

    execute_recovered_transaction(
        builder,
        recovered,
        Error::FinalizeWithdrawalBatchExecution { block_index },
        true,
    )
}

fn decode_user_transactions(
    block_index: usize,
    transactions: &[Bytes],
) -> Result<Vec<Recovered<TempoTxEnvelope>>, Error> {
    let mut decoded = Vec::with_capacity(transactions.len());
    for (transaction_index, encoded_transaction) in transactions.iter().enumerate() {
        let transaction =
            TempoTxEnvelope::decode_2718_exact(encoded_transaction).map_err(|_| {
                Error::TransactionDecoding {
                    block_index,
                    transaction_index,
                }
            })?;
        if transaction.is_system_tx() {
            return Err(Error::SystemTransactionInUserList {
                block_index,
                transaction_index,
            });
        }
        let signer = transaction
            .recover_signer()
            .map_err(|_| Error::TransactionSignature {
                block_index,
                transaction_index,
            })?;
        decoded.push(Recovered::new_unchecked(transaction, signer));
    }
    Ok(decoded)
}

fn execute_user_transactions<B>(
    builder: &mut B,
    block_index: usize,
    transactions: Vec<Recovered<TempoTxEnvelope>>,
) -> Result<(), Error>
where
    B: BlockBuilder,
    B::Executor: BlockExecutor<Transaction = TempoTxEnvelope>,
{
    for (transaction_index, transaction) in transactions.into_iter().enumerate() {
        execute_recovered_transaction(
            builder,
            transaction,
            Error::TransactionExecution {
                block_index,
                transaction_index,
            },
            false,
        )?;
    }

    Ok(())
}

fn execute_recovered_transaction<B>(
    builder: &mut B,
    transaction: Recovered<TempoTxEnvelope>,
    execution_error: Error,
    require_success: bool,
) -> Result<(), Error>
where
    B: BlockBuilder,
    B::Executor: BlockExecutor<Transaction = TempoTxEnvelope>,
{
    let mut success = false;
    builder
        .execute_transaction_with_result_closure(transaction, |result| {
            success = result.result().result.is_success();
        })
        .map_err(|error| map_block_execution_error(error, execution_error))?;
    if require_success && !success {
        return Err(execution_error);
    }
    Ok(())
}

fn map_block_execution_error(
    error: alloy_evm::block::BlockExecutionError,
    execution_error: Error,
) -> Error {
    type WitnessEvmError = revm::context::result::EVMError<
        EvmDatabaseError<crate::WitnessDatabaseError>,
        tempo_evm::TempoInvalidTransaction,
    >;

    if let Some(revm::context::result::EVMError::Database(EvmDatabaseError::Database(error))) =
        error
            .as_internal()
            .and_then(|error| error.downcast_evm::<WitnessEvmError>())
    {
        return (*error).into();
    }

    execution_error
}
