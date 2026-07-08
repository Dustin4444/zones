//! Tempo Zone Node configuration.
//!
//! This is a lightweight L2 node built on reth's node builder infrastructure.
//! It reuses Tempo's EVM, primitives, and pool, but with noop consensus/network/payload.

use crate::{
    ZoneEngine,
    rpc::{ZoneRpc, ZoneRpcApi, rpc_connection_config, start_private_rpc},
};
use alloy_consensus::{
    BlockHeader as _, Transaction as _, constants::KECCAK_EMPTY,
    transaction::SignerRecoverable as _,
};
use alloy_eips::eip2718::Encodable2718 as _;
use alloy_evm::{
    Evm as _, EvmEnv,
    block::BlockExecutor as _,
    eth::EthBlockExecutionCtx,
    precompiles::{DynPrecompile, PrecompilesMap},
};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_provider::Provider as _;
use alloy_rlp::{Decodable as _, Encodable as _};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall as _, SolError as _};
use eyre::WrapErr;
use futures::future::BoxFuture;
use k256::SecretKey;
use reth_eth_wire_types::primitives::BasicNetworkPrimitives;
use reth_node_api::{
    AddOnsContext, FullNodeComponents, FullNodeTypes, NodeAddOns, NodeTypes,
    PayloadAttributesBuilder, PayloadTypes,
};
use reth_node_builder::{
    BuilderContext, DebugNode, Node, NodeAdapter,
    components::{
        BasicPayloadServiceBuilder, ComponentsBuilder, ExecutorBuilder, NoopConsensusBuilder,
        NoopNetworkBuilder, PoolBuilder, spawn_maintenance_tasks,
    },
    rpc::{
        BasicEngineValidatorBuilder, EngineValidatorAddOn, EthApiBuilder, NoopEngineApiBuilder,
        PayloadValidatorBuilder, RethRpcAddOns, RpcAddOns,
    },
};
use reth_primitives_traits::{SealedHeader, transaction::error::InvalidTransactionError};
use reth_provider::ChainSpecProvider;
use reth_rpc_builder::Identity;
use reth_rpc_eth_api::EthApiTypes;
use reth_storage_api::{
    BlockNumReader, BlockReader, BlockSource, EmptyBodyStorage, HeaderProvider, StateProvider,
    StateProviderFactory, StateReader,
};
use reth_transaction_pool::{
    Pool, TransactionValidationTaskExecutor, blobstore::InMemoryBlobStore,
    error::InvalidPoolTransactionError,
};
use reth_trie_common::{
    AccountProof as RethAccountProof, EMPTY_ROOT_HASH, HashedPostState, KeccakKeyHasher,
    KeyHasher as _, TrieInput,
};
use revm::{
    database::{StateBuilder, states::BundleState, states::bundle_state::BundleRetention},
    database_interface::{
        DBErrorMarker, Database,
        primitives::{StorageKey, StorageValue},
        state::{AccountInfo, Bytecode as RevmBytecode},
    },
    precompile::{PrecompileError, PrecompileId, PrecompileOutput, PrecompileResult},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};
use tempo_alloy::TempoNetwork;
use tempo_chainspec::hardfork::{TempoHardfork, TempoHardforks as _};
use tempo_chainspec::spec::TempoChainSpec;
use tempo_contracts::precompiles::ITIP403Registry::PolicyType;
use tempo_evm::{TempoBlockEnv, TempoEvmConfig, evm::TempoEvm};
use tempo_node::{
    DEFAULT_AA_VALID_AFTER_MAX_SECS, engine::TempoEngineValidator, rpc::TempoEthApiBuilder,
};
use tempo_precompiles::{
    TIP_FEE_MANAGER_ADDRESS,
    storage::{StorageAction, StorageActions, packing::extract_from_word},
    tip20::is_tip20_prefix,
    tip403_registry::{ALLOW_ALL_POLICY_ID, REJECT_ALL_POLICY_ID},
};
use tempo_primitives::{
    self as primitives, TempoHeader, TempoPrimitives, TempoTxEnvelope, TempoTxType,
};
use tempo_transaction_pool::{
    AA2dPool, AA2dPoolConfig, TempoTransactionPool,
    amm::AmmLiquidityCache,
    ordering::TempoTipOrdering,
    transaction::TempoPooledTransaction,
    validator::{DEFAULT_MAX_TEMPO_AUTHORIZATIONS, TempoTransactionValidator},
};
use tempo_zone_contracts::{
    TEMPO_STATE_ADDRESS, TEMPO_STATE_READER_ADDRESS, TempoStateReader as TempoStateReaderAbi,
    ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS, ZonePortal,
};
use tracing::{debug, info};
use zone_evm::ZoneEvmConfig;
use zone_l1::{
    DepositQueue, L1Subscriber, L1SubscriberConfig, PolicyCache, TempoStateExt,
    state::{
        L1StateCache, L1StateProvider, L1StateProviderConfig, PolicyProvider,
        spawn_policy_resolution_task, spawn_pool_prefetch_task,
    },
};
use zone_payload::{ZonePayloadAttributes, ZonePayloadFactory, ZonePayloadTypes};
use zone_precompiles::{
    AES_GCM_DECRYPT_ADDRESS, AesGcmDecrypt, CHAUM_PEDERSEN_VERIFY_ADDRESS, ChaumPedersenVerify,
    SequencerExt, ZONE_TIP20_FACTORY_ADDRESS, ZONE_TIP403_PROXY_ADDRESS, ZoneFeeManager,
    ZonePortalReader, ZoneTip20Token, ZoneTip403ProxyRegistry, ZoneTokenFactory, ZoneTxContext,
    policy::PolicyCheck,
};
use zone_primitives::{
    ZoneHeader,
    constants::{
        TEMPO_BENEFICIARY_SLOT, TEMPO_BLOCK_HASH_SLOT, TEMPO_PACKED_SLOT, TEMPO_PARENT_HASH_SLOT,
        TEMPO_PREV_RANDAO_SLOT, TEMPO_RECEIPTS_ROOT_SLOT, TEMPO_STATE_ROOT_SLOT,
        TEMPO_TIMESTAMP_MILLIS_SLOT, TEMPO_TRANSACTIONS_ROOT_SLOT, TEMPO_WRAPPER_GAS_LIMITS_SLOT,
        ZONE_BLOCK_PROTOCOL_VERSION, ZONE_INBOX_PROCESSED_HASH_SLOT,
        ZONE_INBOX_PROCESSED_NUMBER_SLOT, ZONE_OUTBOX_LAST_BATCH_HASH_SLOT,
        ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
    },
    policy::AuthRole,
};
use zone_prover::types::{
    AlloyZoneBlockExecutor, BatchStateProof, L1StateRead, TEMPO_STATE_READER_BASE_GAS,
    TEMPO_STATE_READER_PER_SLOT_GAS, TIP20_TRANSFER_POLICY_ID_SLOT,
    WitnessZoneBlockExecutorProvider, ZoneAccountCode, ZoneAccountRead, ZoneAlloyBlockExecutor,
    ZoneBlock, ZoneBlockEnvWitness, ZoneBlockExecutionContextWitness, ZoneBlockExecutionInput,
    ZoneCfgEnvWitness, ZoneEthExecutorSpec, ZoneStateWitness, ZoneStorageRead, ZoneTempoImport,
    ZoneWithdrawalFinalization, ZoneWitnessEvmFactory, batch_output_from_execution,
    execute_prepared_blocks, prepare_stateless_execution, zone_witness_precompiles,
};
use zone_sequencer::{
    BatchAnchorConfig, BatchWitness, ProverWitnessRequest, ProverWitnessSource,
    ZoneSequencerConfig, spawn_zone_sequencer,
};

/// EVM `BLOCKHASH` can access the 256 most recent ancestor blocks. Keep this
/// mirrored with prover-core's ancestry verifier until it is shared from a
/// common protocol crate.
const ZONE_BLOCKHASH_ANCESTOR_LIMIT: usize = 256;
const MAX_DYNAMIC_WITNESS_READ_CLOSURE_ITERS: usize = 64;
const ZONE_OUTBOX_PACKED_SLOT: U256 = U256::ZERO;
const ZONE_OUTBOX_PENDING_WITHDRAWALS_SLOT: U256 = U256::from_limbs([3, 0, 0, 0]);
const ZONE_OUTBOX_PENDING_WITHDRAWALS_HEAD_SLOT: U256 = U256::from_limbs([4, 0, 0, 0]);
const PENDING_WITHDRAWAL_STORAGE_SLOTS: u64 = 9;
const PENDING_WITHDRAWAL_CALLBACK_DATA_OFFSET: u64 = 7;
const PENDING_WITHDRAWAL_REVEAL_TO_OFFSET: u64 = 8;
const TIP20_TRANSFER_POLICY_ID_OFFSET: usize = 20;
const TIP20_TRANSFER_POLICY_ID_BYTES: usize = 8;
const TEMPO_STATE_HEADER_SLOTS: [B256; 10] = [
    TEMPO_BLOCK_HASH_SLOT,
    TEMPO_WRAPPER_GAS_LIMITS_SLOT,
    TEMPO_PARENT_HASH_SLOT,
    TEMPO_BENEFICIARY_SLOT,
    TEMPO_STATE_ROOT_SLOT,
    TEMPO_TRANSACTIONS_ROOT_SLOT,
    TEMPO_RECEIPTS_ROOT_SLOT,
    TEMPO_PACKED_SLOT,
    TEMPO_TIMESTAMP_MILLIS_SLOT,
    TEMPO_PREV_RANDAO_SLOT,
];

/// Network primitives for Zone Nodes
type ZoneNetworkPrimitives = BasicNetworkPrimitives<TempoPrimitives, TempoTxEnvelope>;

/// Prover witness source backed by a local Zone node provider.
///
/// This is the production sequencer witness path: it derives the witness from
/// canonical node state for the requested batch instead of accepting externally
/// supplied storage data.
#[derive(Clone)]
pub struct LocalNodeProverWitnessSource<P> {
    provider: P,
    l1_state_provider: Option<L1StateProvider>,
}

#[derive(Debug, Clone)]
struct ValidatedZoneHeaderRange {
    from_zone_block: u64,
    to_zone_block: u64,
    parent_number: u64,
    parent_header: SealedHeader<TempoHeader>,
    headers: Vec<SealedHeader<TempoHeader>>,
}

impl ValidatedZoneHeaderRange {
    fn parent_hash(&self) -> B256 {
        self.parent_header.hash()
    }

    fn header_count(&self) -> usize {
        self.headers.len()
    }
}

#[derive(Debug, Clone)]
struct ValidatedZoneBlockData {
    raw_transactions: Vec<Bytes>,
    decoded_transactions: DecodedZoneBlockTransactions,
    witness_block: ZoneBlock,
    receipt_count: usize,
}

#[derive(Debug, Clone)]
struct ZoneHeaderContext {
    prev_block_header: ZoneHeader,
    batch_headers: Vec<ZoneHeader>,
    ancestry_headers: Vec<ZoneHeader>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DecodedZoneBlockTransactions {
    tempo_import: ZoneTempoImport,
    user_transactions: Vec<Bytes>,
    withdrawal_finalization: ZoneWithdrawalFinalization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZoneSystemTransactionKind {
    AdvanceTempo,
    FinalizeWithdrawalBatch,
}

#[derive(Debug, Clone)]
struct ValidatedZoneBatchData {
    range: ValidatedZoneHeaderRange,
    prev_block_header: ZoneHeader,
    batch_headers: Vec<ZoneHeader>,
    first_block_number: u64,
    final_canonical_block_hash: B256,
    final_zone_header_hash: B256,
    zone_ancestry_headers: Vec<Bytes>,
    blocks: Vec<ValidatedZoneBlockData>,
}

impl ValidatedZoneBatchData {
    fn block_count(&self) -> usize {
        self.blocks.len()
    }

    fn first_block_number(&self) -> u64 {
        self.first_block_number
    }

    fn last_zone_header_hash(&self) -> B256 {
        self.final_zone_header_hash
    }

    fn last_canonical_block_hash(&self) -> B256 {
        self.final_canonical_block_hash
    }

    fn raw_transaction_count(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| block.raw_transactions.len())
            .sum()
    }

    fn user_transaction_count(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| block.witness_block.transactions.len())
            .sum()
    }

    fn system_transaction_count(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| {
                usize::from(block.decoded_transactions.tempo_import.is_advance())
                    + usize::from(
                        block
                            .decoded_transactions
                            .withdrawal_finalization
                            .is_finalize(),
                    )
            })
            .sum()
    }

    fn receipt_count(&self) -> usize {
        self.blocks.iter().map(|block| block.receipt_count).sum()
    }

    fn ancestry_header_count(&self) -> usize {
        self.zone_ancestry_headers.len()
    }
}

impl<P> LocalNodeProverWitnessSource<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            l1_state_provider: None,
        }
    }

    pub fn with_l1_state_provider(mut self, l1_state_provider: L1StateProvider) -> Self {
        self.l1_state_provider = Some(l1_state_provider);
        self
    }
}

impl<P> fmt::Debug for LocalNodeProverWitnessSource<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalNodeProverWitnessSource")
            .field("has_l1_state_provider", &self.l1_state_provider.is_some())
            .finish_non_exhaustive()
    }
}

impl<P> LocalNodeProverWitnessSource<P>
where
    P: BlockReader<
            Header = TempoHeader,
            Block = primitives::Block,
            Transaction = TempoTxEnvelope,
            Receipt = primitives::TempoReceipt,
        > + ChainSpecProvider<ChainSpec = TempoChainSpec>
        + StateProviderFactory
        + StateReader<Receipt = primitives::TempoReceipt>
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn build_witness_sync(&self, request: ProverWitnessRequest) -> eyre::Result<BatchWitness> {
        let batch_data = self.load_validated_batch_data(&request)?;
        ensure_local_witness_source_coverage(&batch_data.blocks)?;
        let pre_state = self
            .provider
            .state_by_block_hash(batch_data.range.parent_hash())
            .map_err(|err| {
                eyre::Report::from(err).wrap_err(format!(
                    "failed to open local pre-state provider at zone parent block {}",
                    batch_data.range.parent_number
                ))
            })?;
        let initial_reads = initial_zone_read_slots(pre_state.as_ref(), &batch_data.blocks)
            .wrap_err("failed to derive initial zone state witness read set")?;
        let zone_blocks = batch_data
            .blocks
            .iter()
            .map(|block| block.witness_block.clone())
            .collect::<Vec<_>>();
        let execution_reads = collect_zone_execution_read_slots(
            pre_state.as_ref(),
            batch_data.prev_block_header.state_root,
            initial_reads,
            &request,
            &batch_data.prev_block_header,
            &batch_data.zone_ancestry_headers,
            &zone_blocks,
            self.provider.chain_spec().as_ref(),
            self.l1_state_provider.as_ref(),
        )
        .wrap_err("failed to collect zone execution witness reads")?;
        let mut initial_reads = execution_reads.zone_reads;
        let zone_from = batch_data.range.from_zone_block;
        let zone_to = batch_data.range.to_zone_block;
        let header_count = batch_data.range.header_count();
        let block_count = batch_data.block_count();
        let ancestry_header_count = batch_data.ancestry_header_count();
        let raw_transaction_count = batch_data.raw_transaction_count();
        let system_transaction_count = batch_data.system_transaction_count();
        let user_transaction_count = batch_data.user_transaction_count();
        let receipt_count = batch_data.receipt_count();
        let first_block_number = batch_data.first_block_number();
        let parent_zone_header_hash = batch_data.prev_block_header.hash();
        let final_canonical_block_hash = batch_data.last_canonical_block_hash();
        let final_zone_header_hash = batch_data.last_zone_header_hash();
        let canonical_execution_bundle =
            self.canonical_execution_bundle_state(zone_from, zone_to)?;
        let portal_address = self
            .l1_state_provider
            .as_ref()
            .map(ZonePortalReader::portal_address)
            .unwrap_or_default();

        for replay_attempt in 0..=MAX_DYNAMIC_WITNESS_READ_CLOSURE_ITERS {
            let initial_zone_state = initial_zone_state_witness(
                pre_state.as_ref(),
                batch_data.prev_block_header.state_root,
                &initial_reads,
            )
            .wrap_err("failed to build initial zone state proof witness")?;
            let initial_tempo_binding = initial_tempo_binding_from_state(&initial_zone_state)?;
            let final_tempo_binding =
                final_tempo_binding_from_blocks(initial_tempo_binding, &batch_data.blocks)?;
            eyre::ensure!(
                final_tempo_binding.block_number == request.public_inputs.tempo_block_number,
                "zone witness final Tempo block mismatch: witness {}, batch {}",
                final_tempo_binding.block_number,
                request.public_inputs.tempo_block_number
            );
            if request.public_inputs.anchor_block_number == final_tempo_binding.block_number {
                eyre::ensure!(
                    request.public_inputs.anchor_block_hash == final_tempo_binding.block_hash,
                    "zone witness direct Tempo anchor hash mismatch: witness {}, public {}",
                    final_tempo_binding.block_hash,
                    request.public_inputs.anchor_block_hash
                );
            }
            let expected_withdrawal_batch_index =
                previous_withdrawal_batch_index(&initial_zone_state)?
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("withdrawal batch index overflows u64"))?;
            eyre::ensure!(
                expected_withdrawal_batch_index
                    == request.public_inputs.expected_withdrawal_batch_index,
                "zone witness withdrawal batch index mismatch: local {}, public {}",
                expected_withdrawal_batch_index,
                request.public_inputs.expected_withdrawal_batch_index
            );

            let witness = BatchWitness {
                public_inputs: request.public_inputs.clone(),
                prev_block_header: batch_data.prev_block_header.clone(),
                zone_ancestry_headers: batch_data.zone_ancestry_headers.clone(),
                zone_blocks: zone_blocks.clone(),
                initial_zone_state,
                tempo_state_proofs: execution_reads.tempo_state_proofs.clone(),
                tempo_ancestry_headers: request.tempo_ancestry_headers.clone(),
            };

            let validation = validate_generated_witness_replays_to_canonical_headers(
                &witness,
                &batch_data.batch_headers,
                pre_state.as_ref(),
                Some(&canonical_execution_bundle),
                portal_address,
            );

            match validation {
                Ok(()) => {
                    prepare_stateless_execution(&witness).wrap_err(
                        "local prover witness failed stateless pre-execution validation",
                    )?;
                    debug!(
                        zone_from,
                        zone_to,
                        headers = header_count,
                        bodies = block_count,
                        ancestry_headers = ancestry_header_count,
                        raw_txs = raw_transaction_count,
                        system_txs = system_transaction_count,
                        user_txs = user_transaction_count,
                        receipts = receipt_count,
                        first_body = first_block_number,
                        parent_zone_header_hash = %parent_zone_header_hash,
                        final_canonical_body_hash = %final_canonical_block_hash,
                        final_zone_header_hash = %final_zone_header_hash,
                        dynamic_read_closure_attempts = replay_attempt,
                        "Built local prover witness with initial zone state proofs"
                    );
                    return Ok(witness);
                }
                Err(err) => {
                    if replay_attempt < MAX_DYNAMIC_WITNESS_READ_CLOSURE_ITERS
                        && let Some(account) = missing_account_read_from_error(&err)
                        && initial_reads.entry(account).or_default().is_empty()
                    {
                        debug!(
                            zone_from,
                            zone_to,
                            replay_attempt,
                            %account,
                            "Added dynamic witness account read from stateless replay"
                        );
                        continue;
                    }

                    if replay_attempt < MAX_DYNAMIC_WITNESS_READ_CLOSURE_ITERS
                        && let Some((account, slot)) = missing_storage_read_from_error(&err)
                        && initial_reads.entry(account).or_default().insert(slot)
                    {
                        debug!(
                            zone_from,
                            zone_to,
                            replay_attempt,
                            %account,
                            %slot,
                            "Added dynamic witness storage read from stateless replay"
                        );
                        continue;
                    }

                    return Err(err).wrap_err(
                        "local prover witness replay did not match canonical zone headers",
                    );
                }
            }
        }

        unreachable!("bounded dynamic witness read closure loop always returns")
    }

    fn canonical_execution_bundle_state(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> eyre::Result<BundleState> {
        let mut execution = self.provider.get_state(from_block)?.ok_or_else(|| {
            eyre::eyre!("canonical execution outcome for zone block {from_block} not found")
        })?;

        for block_number in from_block.saturating_add(1)..=to_block {
            let next = self.provider.get_state(block_number)?.ok_or_else(|| {
                eyre::eyre!("canonical execution outcome for zone block {block_number} not found")
            })?;
            execution.extend(next);
        }

        Ok(execution.bundle)
    }

    fn load_validated_batch_data(
        &self,
        request: &ProverWitnessRequest,
    ) -> eyre::Result<ValidatedZoneBatchData> {
        let range = self.load_canonical_header_range(request)?;
        let chain_spec = self.provider.chain_spec();
        let chain_id = chain_spec.inner.genesis().config.chain_id;
        let zone_headers = self.load_zone_header_context(&range)?;
        let final_zone_header = zone_headers
            .batch_headers
            .last()
            .ok_or_else(|| eyre::eyre!("validated zone header range is empty"))?;
        validate_zone_public_hashes(request, &zone_headers.prev_block_header, final_zone_header)?;
        let final_canonical_block_hash = range
            .headers
            .last()
            .map(SealedHeader::hash)
            .ok_or_else(|| eyre::eyre!("validated zone header range is empty"))?;
        let final_zone_header_hash = final_zone_header.hash();
        let zone_ancestry_headers = zone_headers
            .ancestry_headers
            .iter()
            .map(encode_zone_header)
            .collect();
        let mut blocks = Vec::with_capacity(range.header_count());

        for (header, zone_header) in range.headers.iter().zip(&zone_headers.batch_headers) {
            let hash = header.hash();
            let spec = chain_spec.tempo_hardfork_at(header.header().timestamp());
            let block = self
                .provider
                .find_block_by_hash(hash, BlockSource::Canonical)?
                .ok_or_else(|| eyre::eyre!("canonical zone block {hash} not found"))?;
            let receipts = self
                .provider
                .receipts_by_block(hash.into())?
                .ok_or_else(|| eyre::eyre!("zone receipts for canonical block {hash} not found"))?;
            blocks.push(validated_zone_block_data(
                chain_id,
                spec,
                zone_header.parent_hash,
                header,
                &block,
                &receipts,
            )?);
        }
        Ok(ValidatedZoneBatchData {
            range,
            prev_block_header: zone_headers.prev_block_header,
            batch_headers: zone_headers.batch_headers,
            first_block_number: request.from_zone_block,
            final_canonical_block_hash,
            final_zone_header_hash,
            zone_ancestry_headers,
            blocks,
        })
    }

    fn load_canonical_header_range(
        &self,
        request: &ProverWitnessRequest,
    ) -> eyre::Result<ValidatedZoneHeaderRange> {
        let (parent_number, block_capacity) = validate_requested_zone_header_range(request)?;
        let parent_header = self
            .provider
            .sealed_header(parent_number)?
            .ok_or_else(|| eyre::eyre!("zone parent header {parent_number} not found"))?;

        let mut headers = Vec::with_capacity(block_capacity);
        for number in request.from_zone_block..=request.to_zone_block {
            let header = self
                .provider
                .sealed_header(number)?
                .ok_or_else(|| eyre::eyre!("zone header {number} not found"))?;
            headers.push(header);
        }

        validate_canonical_zone_header_range(request, parent_header, headers)
    }

    fn load_zone_header_context(
        &self,
        range: &ValidatedZoneHeaderRange,
    ) -> eyre::Result<ZoneHeaderContext> {
        let mut zone_headers_by_number = BTreeMap::new();
        let mut previous_zone_hash = B256::ZERO;

        for number in 0..=range.to_zone_block {
            let header = self
                .provider
                .sealed_header(number)?
                .ok_or_else(|| eyre::eyre!("zone header {number} not found"))?;
            let parent_hash = if number == 0 {
                header.header().parent_hash()
            } else {
                previous_zone_hash
            };
            let zone_header = zone_header_from_tempo_header(header.header(), parent_hash);
            previous_zone_hash = zone_header.hash();
            zone_headers_by_number.insert(number, zone_header);
        }

        let prev_block_header = zone_headers_by_number
            .remove(&range.parent_number)
            .ok_or_else(|| eyre::eyre!("zone parent header {} not found", range.parent_number))?;

        let mut batch_headers = Vec::with_capacity(range.header_count());
        for number in range.from_zone_block..=range.to_zone_block {
            let header = zone_headers_by_number
                .remove(&number)
                .ok_or_else(|| eyre::eyre!("zone batch header {number} not found"))?;
            batch_headers.push(header);
        }

        let mut ancestry_headers = Vec::new();
        let mut number = range.parent_number;
        for _ in 1..ZONE_BLOCKHASH_ANCESTOR_LIMIT {
            let Some(parent_number) = number.checked_sub(1) else {
                break;
            };
            let header = zone_headers_by_number
                .remove(&parent_number)
                .ok_or_else(|| eyre::eyre!("zone ancestry header {parent_number} not found"))?;
            ancestry_headers.push(header);
            number = parent_number;
        }

        Ok(ZoneHeaderContext {
            prev_block_header,
            batch_headers,
            ancestry_headers,
        })
    }
}

fn ensure_local_witness_source_coverage(blocks: &[ValidatedZoneBlockData]) -> eyre::Result<()> {
    for (index, block) in blocks.iter().enumerate() {
        ensure_local_witness_block_coverage(index, &block.witness_block)?;
    }
    Ok(())
}

fn ensure_local_witness_block_coverage(_index: usize, _block: &ZoneBlock) -> eyre::Result<()> {
    Ok(())
}

fn validate_requested_zone_header_range(
    request: &ProverWitnessRequest,
) -> eyre::Result<(u64, usize)> {
    eyre::ensure!(
        request.from_zone_block <= request.to_zone_block,
        "invalid prover witness range: from_zone_block {} is after to_zone_block {}",
        request.from_zone_block,
        request.to_zone_block
    );
    eyre::ensure!(
        request.from_zone_block > 0,
        "cannot build prover witness for a batch starting at genesis block"
    );

    let parent_number = request
        .from_zone_block
        .checked_sub(1)
        .ok_or_else(|| eyre::eyre!("zone witness parent block number underflowed"))?;
    let block_count = request
        .to_zone_block
        .checked_sub(request.from_zone_block)
        .and_then(|span| span.checked_add(1))
        .ok_or_else(|| eyre::eyre!("zone witness block range length overflowed"))?;
    let block_capacity = usize::try_from(block_count)
        .map_err(|_| eyre::eyre!("zone witness block range too large: {block_count}"))?;

    Ok((parent_number, block_capacity))
}

fn validate_canonical_zone_header_range(
    request: &ProverWitnessRequest,
    parent_header: SealedHeader<TempoHeader>,
    headers: Vec<SealedHeader<TempoHeader>>,
) -> eyre::Result<ValidatedZoneHeaderRange> {
    let (parent_number, expected_header_count) = validate_requested_zone_header_range(request)?;
    eyre::ensure!(
        parent_header.header().number() == parent_number,
        "zone parent header number mismatch: expected {parent_number}, got {}",
        parent_header.header().number()
    );
    let parent_hash = parent_header.hash();
    eyre::ensure!(
        headers.len() == expected_header_count,
        "zone witness header count mismatch for range {}..={}: expected {expected_header_count}, got {}",
        request.from_zone_block,
        request.to_zone_block,
        headers.len()
    );
    let mut expected_parent_hash = parent_hash;

    for (offset, header) in headers.iter().enumerate() {
        let offset = u64::try_from(offset)
            .map_err(|_| eyre::eyre!("zone witness header offset overflowed"))?;
        let number = request
            .from_zone_block
            .checked_add(offset)
            .ok_or_else(|| eyre::eyre!("zone witness header number overflowed"))?;
        eyre::ensure!(
            header.header().number() == number,
            "canonical zone header number mismatch at offset {offset}: expected {number}, got {}",
            header.header().number()
        );
        eyre::ensure!(
            header.header().parent_hash() == expected_parent_hash,
            "canonical zone header parent mismatch at block {number}: expected {}, got {}",
            expected_parent_hash,
            header.header().parent_hash()
        );
        expected_parent_hash = header.hash();
    }

    Ok(ValidatedZoneHeaderRange {
        from_zone_block: request.from_zone_block,
        to_zone_block: request.to_zone_block,
        parent_number,
        parent_header,
        headers,
    })
}

fn validate_zone_public_hashes(
    request: &ProverWitnessRequest,
    prev_block_header: &ZoneHeader,
    final_block_header: &ZoneHeader,
) -> eyre::Result<()> {
    let prev_block_hash = prev_block_header.hash();
    eyre::ensure!(
        prev_block_hash == request.batch.prev_block_hash,
        "zone parent header hash mismatch: derived {}, batch {}",
        prev_block_hash,
        request.batch.prev_block_hash
    );

    let final_block_hash = final_block_header.hash();
    eyre::ensure!(
        final_block_hash == request.batch.next_block_hash,
        "zone final header hash mismatch at block {}: derived {}, batch {}",
        request.to_zone_block,
        final_block_hash,
        request.batch.next_block_hash
    );

    Ok(())
}

fn validated_zone_block_data(
    chain_id: u64,
    spec: TempoHardfork,
    parent_zone_hash: B256,
    header: &SealedHeader<TempoHeader>,
    block: &primitives::Block,
    receipts: &[primitives::TempoReceipt],
) -> eyre::Result<ValidatedZoneBlockData> {
    let canonical_hash = header.hash();
    eyre::ensure!(
        &block.header == header.header(),
        "canonical zone block {canonical_hash} header does not match validated header"
    );

    let raw_transactions = block
        .body
        .transactions
        .iter()
        .map(|tx| Bytes::from(tx.encoded_2718()))
        .collect::<Vec<_>>();
    eyre::ensure!(
        raw_transactions.len() == receipts.len(),
        "canonical zone block {} transaction/receipt count mismatch: {} transactions, {} receipts",
        header.header().number(),
        raw_transactions.len(),
        receipts.len()
    );
    let decoded_transactions = decode_zone_block_transactions(
        header.header().number(),
        &block.body.transactions,
        &raw_transactions,
    )?;
    let witness_block = zone_block_witness_from_header(
        chain_id,
        spec,
        parent_zone_hash,
        header.header(),
        decoded_transactions.clone(),
    )?;

    Ok(ValidatedZoneBlockData {
        raw_transactions,
        decoded_transactions,
        witness_block,
        receipt_count: receipts.len(),
    })
}

fn zone_block_witness_from_header(
    chain_id: u64,
    spec: TempoHardfork,
    parent_zone_hash: B256,
    header: &TempoHeader,
    decoded_transactions: DecodedZoneBlockTransactions,
) -> eyre::Result<ZoneBlock> {
    let basefee = header
        .base_fee_per_gas()
        .ok_or_else(|| eyre::eyre!("zone block {} is missing base_fee_per_gas", header.number()))?;
    let prevrandao = header.mix_hash().ok_or_else(|| {
        eyre::eyre!(
            "zone block {} is missing prevrandao/mix_hash",
            header.number()
        )
    })?;
    let parent_beacon_block_root = header.parent_beacon_block_root().ok_or_else(|| {
        eyre::eyre!(
            "zone block {} is missing parent_beacon_block_root",
            header.number()
        )
    })?;
    let slot_num = header
        .slot_number()
        .ok_or_else(|| eyre::eyre!("zone block {} is missing slot_number", header.number()))?;

    Ok(ZoneBlock {
        number: header.number(),
        parent_hash: parent_zone_hash,
        timestamp: header.timestamp(),
        beneficiary: header.beneficiary(),
        protocol_version: ZONE_BLOCK_PROTOCOL_VERSION,
        cfg_env: ZoneCfgEnvWitness {
            chain_id,
            spec,
            enable_amsterdam_eip8037: false,
        },
        execution_context: ZoneBlockExecutionContextWitness {
            parent_beacon_block_root,
            extra_data: header.extra_data().clone(),
        },
        block_env: ZoneBlockEnvWitness {
            gas_limit: header.gas_limit(),
            basefee,
            difficulty: header.difficulty(),
            prevrandao,
            slot_num,
            timestamp_millis_part: header.timestamp_millis_part,
        },
        tempo_import: decoded_transactions.tempo_import,
        withdrawal_finalization: decoded_transactions.withdrawal_finalization,
        transactions: decoded_transactions.user_transactions,
    })
}

fn decode_zone_block_transactions(
    block_number: u64,
    transactions: &[TempoTxEnvelope],
    raw_transactions: &[Bytes],
) -> eyre::Result<DecodedZoneBlockTransactions> {
    eyre::ensure!(
        transactions.len() == raw_transactions.len(),
        "canonical zone block {block_number} raw transaction count mismatch: {} decoded, {} raw",
        transactions.len(),
        raw_transactions.len()
    );

    let mut user_start = 0;
    let mut user_end = transactions.len();
    let mut withdrawal_finalization = ZoneWithdrawalFinalization::none();

    if user_end > 0 {
        match classify_zone_system_transaction(&transactions[user_end - 1])? {
            Some(ZoneSystemTransactionKind::FinalizeWithdrawalBatch) => {
                let call = decode_finalize_withdrawal_batch_transaction(
                    block_number,
                    &transactions[user_end - 1],
                )?;
                withdrawal_finalization =
                    ZoneWithdrawalFinalization::finalize(call.count, call.encryptedSenders);
                user_end = user_end.checked_sub(1).ok_or_else(|| {
                    eyre::eyre!("zone block {block_number} transaction index underflow")
                })?;
            }
            Some(ZoneSystemTransactionKind::AdvanceTempo) if user_end > 1 => {
                eyre::bail!(
                    "zone block {block_number} advanceTempo system transaction must be first"
                );
            }
            Some(ZoneSystemTransactionKind::AdvanceTempo) | None => {}
        }
    }

    let mut tempo_import = ZoneTempoImport::none();
    if user_start < user_end {
        match classify_zone_system_transaction(&transactions[user_start])? {
            Some(ZoneSystemTransactionKind::AdvanceTempo) => {
                let call =
                    decode_advance_tempo_transaction(block_number, &transactions[user_start])?;
                tempo_import = ZoneTempoImport::advance(
                    call.header,
                    call.deposits,
                    call.decryptions,
                    call.enabledTokens,
                );
                user_start = user_start.checked_add(1).ok_or_else(|| {
                    eyre::eyre!("zone block {block_number} transaction index overflow")
                })?;
            }
            Some(ZoneSystemTransactionKind::FinalizeWithdrawalBatch) => {
                eyre::bail!(
                    "zone block {block_number} finalizeWithdrawalBatch system transaction must be last"
                );
            }
            None => {}
        }
    }

    for (relative_index, tx) in transactions[user_start..user_end].iter().enumerate() {
        if classify_zone_system_transaction(tx)?.is_some() {
            let transaction_index = user_start.checked_add(relative_index).ok_or_else(|| {
                eyre::eyre!("zone block {block_number} transaction index overflow")
            })?;
            eyre::bail!(
                "zone block {block_number} unexpected system transaction at user transaction index {transaction_index}"
            );
        }
    }

    Ok(DecodedZoneBlockTransactions {
        tempo_import,
        user_transactions: raw_transactions[user_start..user_end].to_vec(),
        withdrawal_finalization,
    })
}

fn classify_zone_system_transaction(
    tx: &TempoTxEnvelope,
) -> eyre::Result<Option<ZoneSystemTransactionKind>> {
    let Some(target) = tx.to() else {
        if tx.is_system_tx() {
            eyre::bail!("Tempo system transaction must call a zone system contract");
        }
        return Ok(None);
    };

    if !tx.is_system_tx() {
        return Ok(None);
    };

    let kind = if target == ZONE_INBOX_ADDRESS {
        ZoneSystemTransactionKind::AdvanceTempo
    } else if target == ZONE_OUTBOX_ADDRESS {
        ZoneSystemTransactionKind::FinalizeWithdrawalBatch
    } else {
        eyre::bail!("Tempo system transaction targets unexpected address {target}");
    };

    let signer = tx
        .recover_signer()
        .map_err(|err| eyre::eyre!("failed to recover zone system transaction signer: {err}"))?;
    eyre::ensure!(
        signer == tempo_primitives::transaction::envelope::TEMPO_SYSTEM_TX_SENDER,
        "zone system contract transaction signer mismatch: expected {}, got {signer}",
        tempo_primitives::transaction::envelope::TEMPO_SYSTEM_TX_SENDER
    );
    eyre::ensure!(
        tx.value().is_zero() && tx.nonce() == 0 && tx.gas_limit() == 0 && tx.max_fee_per_gas() == 0,
        "zone system transaction to {target} must have zero value, nonce, and gas fields"
    );

    Ok(Some(kind))
}

fn decode_advance_tempo_transaction(
    block_number: u64,
    tx: &TempoTxEnvelope,
) -> eyre::Result<tempo_zone_contracts::ZoneInbox::advanceTempoCall> {
    tempo_zone_contracts::ZoneInbox::advanceTempoCall::abi_decode(tx.input().as_ref()).map_err(
        |err| {
            eyre::eyre!("zone block {block_number} failed to decode advanceTempo calldata: {err}")
        },
    )
}

fn decode_finalize_withdrawal_batch_transaction(
    block_number: u64,
    tx: &TempoTxEnvelope,
) -> eyre::Result<tempo_zone_contracts::ZoneOutbox::finalizeWithdrawalBatchCall> {
    let call = tempo_zone_contracts::ZoneOutbox::finalizeWithdrawalBatchCall::abi_decode(
        tx.input().as_ref(),
    )
    .map_err(|err| {
        eyre::eyre!(
            "zone block {block_number} failed to decode finalizeWithdrawalBatch calldata: {err}"
        )
    })?;
    eyre::ensure!(
        call.blockNumber == block_number,
        "zone block {block_number} finalizeWithdrawalBatch blockNumber mismatch: calldata {}",
        call.blockNumber
    );
    Ok(call)
}

#[derive(Debug, Clone, Copy)]
struct LocalTempoBinding {
    block_number: u64,
    block_hash: B256,
}

fn initial_zone_read_slots(
    state: &dyn StateProvider,
    blocks: &[ValidatedZoneBlockData],
) -> eyre::Result<BTreeMap<Address, BTreeSet<B256>>> {
    let mut reads = required_initial_zone_reads();
    add_withdrawal_finalization_reads(state, blocks, &mut reads)?;
    Ok(reads)
}

fn collect_zone_execution_read_slots(
    state: &dyn StateProvider,
    state_root: B256,
    initial_reads: BTreeMap<Address, BTreeSet<B256>>,
    request: &ProverWitnessRequest,
    prev_block_header: &ZoneHeader,
    zone_ancestry_headers: &[Bytes],
    zone_blocks: &[ZoneBlock],
    chain_spec: &TempoChainSpec,
    l1_state_provider: Option<&L1StateProvider>,
) -> eyre::Result<ExecutionReadCollection> {
    let initial_zone_state = initial_zone_state_witness(state, state_root, &initial_reads)
        .wrap_err("failed to build static seed witness for execution read collection")?;
    let witness = BatchWitness {
        public_inputs: request.public_inputs.clone(),
        prev_block_header: prev_block_header.clone(),
        zone_ancestry_headers: zone_ancestry_headers.to_vec(),
        zone_blocks: zone_blocks.to_vec(),
        initial_zone_state,
        tempo_state_proofs: BatchStateProof {
            node_pool: BTreeMap::new(),
            reads: Vec::new(),
        },
        tempo_ancestry_headers: request.tempo_ancestry_headers.clone(),
    };
    let prepared = prepare_stateless_execution(&witness)
        .wrap_err("static seed witness failed stateless preparation")?;
    let recording_db = RecordingStateProviderDb::new(state, initial_reads);
    let mut execution_state = StateBuilder::new_with_database(recording_db)
        .with_bundle_update()
        .build();
    let tempo_reads =
        l1_state_provider.map(|_| Arc::new(Mutex::new(TempoExecutionReadSet::default())));
    let tempo_collection =
        l1_state_provider
            .zip(tempo_reads.as_ref())
            .map(|(l1_state_provider, reads)| TempoReadCollectionContext {
                l1_state_provider,
                reads: reads.clone(),
            });
    let mut post_state_targets = BTreeMap::new();

    for block_index in 0..zone_blocks.len() {
        let input = prepared
            .block_execution_input(block_index)
            .wrap_err_with(|| {
                format!("failed to prepare block {block_index} for read collection")
            })?;
        execute_zone_block_for_read_collection(
            &mut execution_state,
            &input,
            chain_spec,
            tempo_collection.as_ref(),
        )
        .wrap_err_with(|| {
            format!(
                "zone block {} failed during execution read collection",
                input.block.number
            )
        })?;
        add_post_state_proof_targets(
            execution_state.transition_state.as_ref(),
            &mut post_state_targets,
        );
        execution_state.merge_transitions(BundleRetention::PlainState);
    }

    let tempo_state_proofs =
        if let Some((l1_state_provider, reads)) = l1_state_provider.zip(tempo_reads.as_ref()) {
            let reads = reads
                .lock()
                .map_err(|_| eyre::eyre!("Tempo read collection mutex was poisoned"))?
                .clone();
            tempo_state_proof_from_reads(l1_state_provider, reads)?
        } else {
            BatchStateProof {
                node_pool: BTreeMap::new(),
                reads: Vec::new(),
            }
        };

    let mut zone_reads = execution_state.database.into_reads();
    merge_read_sets(&mut zone_reads, post_state_targets);

    Ok(ExecutionReadCollection {
        zone_reads,
        tempo_state_proofs,
    })
}

fn merge_read_sets(
    target: &mut BTreeMap<Address, BTreeSet<B256>>,
    source: BTreeMap<Address, BTreeSet<B256>>,
) {
    for (account, slots) in source {
        target.entry(account).or_default().extend(slots);
    }
}

fn add_post_state_proof_targets(
    transition_state: Option<&revm::database::TransitionState>,
    reads: &mut BTreeMap<Address, BTreeSet<B256>>,
) {
    let Some(transition_state) = transition_state else {
        return;
    };

    for (address, account) in &transition_state.transitions {
        let slots = reads.entry(*address).or_default();
        for slot in account.storage.keys() {
            slots.insert(B256::from(*slot));
        }
    }
}

fn missing_account_read_from_error(err: &eyre::Report) -> Option<Address> {
    err.chain().find_map(|cause| {
        let message = cause.to_string();
        parse_missing_account_read(&message)
    })
}

fn parse_missing_account_read(message: &str) -> Option<Address> {
    let (_, rest) = message.split_once("witness is missing account read for ")?;
    rest.split_whitespace().next()?.parse().ok()
}

fn missing_storage_read_from_error(err: &eyre::Report) -> Option<(Address, B256)> {
    err.chain().find_map(|cause| {
        let message = cause.to_string();
        parse_missing_storage_read(&message)
    })
}

fn parse_missing_storage_read(message: &str) -> Option<(Address, B256)> {
    let (_, rest) = message.split_once("witness is missing storage read for account ")?;
    let (account, rest) = rest.split_once(" slot ")?;
    let account = account.split_whitespace().next()?.parse().ok()?;
    let slot = rest.split_whitespace().next()?.parse::<U256>().ok()?;
    Some((account, B256::from(slot)))
}

fn validate_generated_witness_replays_to_canonical_headers(
    witness: &BatchWitness,
    expected_headers: &[ZoneHeader],
    pre_state: &dyn StateProvider,
    canonical_execution_bundle: Option<&BundleState>,
    portal_address: Address,
) -> eyre::Result<()> {
    let prepared = prepare_stateless_execution(witness)
        .wrap_err("generated witness failed stateless preparation")?;
    eyre::ensure!(
        prepared.zone_blocks.len() == expected_headers.len(),
        "generated witness block count {} does not match canonical header count {}",
        prepared.zone_blocks.len(),
        expected_headers.len()
    );

    let mut executor = AlloyZoneBlockExecutor::new(
        WitnessZoneBlockExecutorProvider::new().with_portal_address(portal_address),
    );
    let execution = execute_prepared_blocks(&prepared, &mut executor)
        .map_err(|err| eyre::eyre!(err.to_string()))
        .wrap_err("generated witness failed stateless execution")?;
    let provider_final_state_root = pre_state
        .state_root(execution.post_state.hashed().clone())
        .map_err(|err| {
            eyre::Report::from(err)
                .wrap_err("failed to calculate provider state root for replay post-state")
        })?;
    let expected_final_state_root = expected_headers
        .last()
        .map(|header| header.state_root)
        .ok_or_else(|| eyre::eyre!("generated witness has no expected final zone header"))?;

    let mut previous_hash = witness.public_inputs.prev_block_hash;
    for (index, ((expected, executed), block)) in expected_headers
        .iter()
        .zip(&execution.blocks)
        .zip(&prepared.zone_blocks)
        .enumerate()
    {
        let actual = executed.header(previous_hash, block);
        if &actual != expected {
            let post_state_diff = canonical_replay_post_state_diff(
                canonical_execution_bundle,
                execution.post_state.hashed(),
            );
            eyre::bail!(
                "zone block {} replay header mismatch: expected_hash={}, actual_hash={}, expected_parent={}, actual_parent={}, expected_state_root={}, actual_state_root={}, expected_tx_root={}, actual_tx_root={}, expected_receipts_root={}, actual_receipts_root={}, expected_beneficiary={}, actual_beneficiary={}, expected_timestamp={}, actual_timestamp={}, expected_protocol_version={}, actual_protocol_version={}, provider_final_state_root={}, expected_final_state_root={}, {}",
                block.number,
                expected.hash(),
                actual.hash(),
                expected.parent_hash,
                actual.parent_hash,
                expected.state_root,
                actual.state_root,
                expected.transactions_root,
                actual.transactions_root,
                expected.receipts_root,
                actual.receipts_root,
                expected.beneficiary,
                actual.beneficiary,
                expected.timestamp,
                actual.timestamp,
                expected.protocol_version,
                actual.protocol_version,
                provider_final_state_root,
                expected_final_state_root,
                post_state_diff
            );
        }
        previous_hash = actual.hash();
        if index + 1 == expected_headers.len() {
            let output = batch_output_from_execution(&prepared, &execution)
                .map_err(|err| eyre::eyre!(err.to_string()))
                .wrap_err("generated witness failed output derivation")?;
            eyre::ensure!(
                output.block_transition.nextBlockHash == expected.hash(),
                "generated witness output nextBlockHash {} does not match final canonical Zone hash {}",
                output.block_transition.nextBlockHash,
                expected.hash()
            );
        }
    }

    Ok(())
}

const POST_STATE_DIFF_SAMPLE_LIMIT: usize = 12;

fn canonical_replay_post_state_diff(
    canonical: Option<&BundleState>,
    replay: &HashedPostState,
) -> String {
    let Some(canonical) = canonical else {
        return "canonical_post_state_diff=unavailable".to_string();
    };

    let canonical_hashed = HashedPostState::from_bundle_state::<KeccakKeyHasher>(canonical.state());
    let mut canonical_address_by_hash = BTreeMap::new();
    let mut canonical_slot_by_hash = BTreeMap::new();
    for (address, account) in canonical.state() {
        let hashed_address = KeccakKeyHasher::hash_key(*address);
        canonical_address_by_hash.insert(hashed_address, *address);
        for slot in account.storage.keys() {
            let raw_slot = B256::from(*slot);
            canonical_slot_by_hash.insert(
                (hashed_address, KeccakKeyHasher::hash_key(raw_slot)),
                raw_slot,
            );
        }
    }

    let mut missing_accounts = DiffSamples::default();
    let mut account_value_mismatches = DiffSamples::default();
    let mut extra_accounts = DiffSamples::default();
    let mut missing_storage = DiffSamples::default();
    let mut storage_value_mismatches = DiffSamples::default();
    let mut storage_wipe_mismatches = DiffSamples::default();
    let mut extra_storage = DiffSamples::default();

    for (hashed_address, canonical_account) in &canonical_hashed.accounts {
        match replay.accounts.get(hashed_address) {
            Some(replay_account) if replay_account == canonical_account => {}
            Some(replay_account) => account_value_mismatches.push(format!(
                "{} canonical={canonical_account:?} replay={replay_account:?}",
                display_hashed_address(*hashed_address, &canonical_address_by_hash)
            )),
            None => missing_accounts.push(format!(
                "{} canonical={canonical_account:?}",
                display_hashed_address(*hashed_address, &canonical_address_by_hash)
            )),
        }
    }

    for (hashed_address, replay_account) in &replay.accounts {
        if !canonical_hashed.accounts.contains_key(hashed_address) {
            extra_accounts.push(format!("{hashed_address} replay={replay_account:?}"));
        }
    }

    for (hashed_address, canonical_storage) in &canonical_hashed.storages {
        let replay_storage = replay.storages.get(hashed_address);
        if replay_storage.map(|storage| storage.wiped).unwrap_or(false) != canonical_storage.wiped {
            storage_wipe_mismatches.push(format!(
                "{} canonical_wiped={} replay_wiped={}",
                display_hashed_address(*hashed_address, &canonical_address_by_hash),
                canonical_storage.wiped,
                replay_storage.map(|storage| storage.wiped).unwrap_or(false)
            ));
        }

        for (hashed_slot, canonical_value) in &canonical_storage.storage {
            match replay_storage.and_then(|storage| storage.storage.get(hashed_slot)) {
                Some(replay_value) if replay_value == canonical_value => {}
                Some(replay_value) => storage_value_mismatches.push(format!(
                    "{}[{}] canonical={} replay={}",
                    display_hashed_address(*hashed_address, &canonical_address_by_hash),
                    display_hashed_slot(*hashed_address, *hashed_slot, &canonical_slot_by_hash),
                    canonical_value,
                    replay_value
                )),
                None => missing_storage.push(format!(
                    "{}[{}] canonical={}",
                    display_hashed_address(*hashed_address, &canonical_address_by_hash),
                    display_hashed_slot(*hashed_address, *hashed_slot, &canonical_slot_by_hash),
                    canonical_value
                )),
            }
        }
    }

    for (hashed_address, replay_storage) in &replay.storages {
        let canonical_storage = canonical_hashed.storages.get(hashed_address);
        if replay_storage.wiped
            && !canonical_storage
                .map(|storage| storage.wiped)
                .unwrap_or(false)
        {
            extra_storage.push(format!("{hashed_address} replay_wiped=true"));
        }
        for (hashed_slot, replay_value) in &replay_storage.storage {
            if canonical_storage
                .and_then(|storage| storage.storage.get(hashed_slot))
                .is_none()
            {
                extra_storage.push(format!(
                    "{hashed_address}[{hashed_slot}] replay={replay_value}"
                ));
            }
        }
    }

    let mut out = format!(
        "canonical_post_state_diff=canonical_accounts:{}, replay_accounts:{}, canonical_storages:{}, replay_storages:{}",
        canonical_hashed.accounts.len(),
        replay.accounts.len(),
        canonical_hashed
            .storages
            .values()
            .map(|storage| storage.storage.len())
            .sum::<usize>(),
        replay
            .storages
            .values()
            .map(|storage| storage.storage.len())
            .sum::<usize>(),
    );
    missing_accounts.append("missing_accounts", &mut out);
    account_value_mismatches.append("account_value_mismatches", &mut out);
    extra_accounts.append("extra_accounts", &mut out);
    missing_storage.append("missing_storage", &mut out);
    storage_value_mismatches.append("storage_value_mismatches", &mut out);
    storage_wipe_mismatches.append("storage_wipe_mismatches", &mut out);
    extra_storage.append("extra_storage", &mut out);
    out
}

#[derive(Default)]
struct DiffSamples {
    count: usize,
    samples: Vec<String>,
}

impl DiffSamples {
    fn push(&mut self, sample: String) {
        self.count += 1;
        if self.samples.len() < POST_STATE_DIFF_SAMPLE_LIMIT {
            self.samples.push(sample);
        }
    }

    fn append(&self, label: &str, out: &mut String) {
        use std::fmt::Write as _;

        let _ = write!(out, ", {label}:{}", self.count);
        if !self.samples.is_empty() {
            let _ = write!(out, " [{}]", self.samples.join("; "));
        }
    }
}

fn display_hashed_address(address: B256, raw_addresses: &BTreeMap<B256, Address>) -> String {
    raw_addresses
        .get(&address)
        .map(ToString::to_string)
        .unwrap_or_else(|| address.to_string())
}

fn display_hashed_slot(
    address: B256,
    slot: B256,
    raw_slots: &BTreeMap<(B256, B256), B256>,
) -> String {
    raw_slots
        .get(&(address, slot))
        .map(ToString::to_string)
        .unwrap_or_else(|| slot.to_string())
}

fn execute_zone_block_for_read_collection(
    state: &mut revm::database::State<RecordingStateProviderDb<'_>>,
    input: &ZoneBlockExecutionInput<'_>,
    chain_spec: &TempoChainSpec,
    tempo_collection: Option<&TempoReadCollectionContext<'_>>,
) -> eyre::Result<()> {
    if let Some(tempo_collection) = tempo_collection {
        return execute_zone_block_for_read_collection_with_tempo_fee_path(
            state,
            input,
            chain_spec,
            tempo_collection,
        );
    }

    let evm_factory = ZoneWitnessEvmFactory;
    let precompiles = zone_witness_precompiles(
        &input.evm_env,
        input.tempo_state_reader,
        input.block.beneficiary,
        input.block.tempo_block_number,
    );
    let evm =
        evm_factory.create_evm_with_precompiles(&mut *state, input.evm_env.clone(), precompiles);
    let ctx: EthBlockExecutionCtx<'_> = input.execution_context.inner.clone();
    let executor = ZoneAlloyBlockExecutor::new(evm, ctx, ZoneEthExecutorSpec);
    executor
        .execute_block(input.recovered_transactions())
        .map(|_| ())
        .map_err(|err| eyre::eyre!(err.to_string()))
}

fn execute_zone_block_for_read_collection_with_tempo_fee_path(
    state: &mut revm::database::State<RecordingStateProviderDb<'_>>,
    input: &ZoneBlockExecutionInput<'_>,
    chain_spec: &TempoChainSpec,
    tempo_collection: &TempoReadCollectionContext<'_>,
) -> eyre::Result<()> {
    let mut evm = TempoEvm::new(&mut *state, tempo_evm_env(input)).with_actions();
    let storage_actions = evm.inner_mut().actions().clone();
    let fee_provider = RecordingZonePortalReader::new(
        tempo_collection.l1_state_provider.clone(),
        tempo_collection.reads.clone(),
        u64::try_from(input.block_index).unwrap_or(u64::MAX),
    );
    evm = evm.with_fee_manager(ZoneFeeManager::new(fee_provider.clone()));
    let (_, _, precompiles) = evm.components_mut();
    register_zone_read_collection_precompiles(
        input,
        tempo_collection,
        precompiles,
        storage_actions.clone(),
    );

    let ctx: EthBlockExecutionCtx<'_> = input.execution_context.inner.clone();
    let executor = ZoneAlloyBlockExecutor::new(evm, ctx, chain_spec);
    let result = executor
        .execute_block(input.recovered_transactions())
        .map(|_| ())
        .map_err(|err| eyre::eyre!(err.to_string()));
    if result.is_ok() {
        let actions = storage_actions.take().unwrap_or_default();
        record_storage_action_vec(state, actions)?;
    }
    result.map(|_| ())
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

fn record_storage_action_vec(
    state: &mut revm::database::State<RecordingStateProviderDb<'_>>,
    actions: Vec<StorageAction>,
) -> eyre::Result<()> {
    for action in actions {
        let (account, slot) = match action {
            StorageAction::Sload(account, slot, _)
            | StorageAction::Sstore(account, slot, _)
            | StorageAction::Sinc(account, slot, _)
            | StorageAction::Sdec(account, slot, _)
            | StorageAction::FeeAmmSwap(account, slot, _) => (account, slot),
        };
        state.database.record_storage(account, B256::from(slot));
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct ExecutionReadCollection {
    zone_reads: BTreeMap<Address, BTreeSet<B256>>,
    tempo_state_proofs: BatchStateProof,
}

#[derive(Clone)]
struct TempoReadCollectionContext<'a> {
    l1_state_provider: &'a L1StateProvider,
    reads: Arc<Mutex<TempoExecutionReadSet>>,
}

fn register_zone_read_collection_precompiles(
    input: &ZoneBlockExecutionInput<'_>,
    context: &TempoReadCollectionContext<'_>,
    precompiles: &mut PrecompilesMap,
    storage_actions: StorageActions,
) {
    let reader = RecordingTempoStateReader::new(
        context.l1_state_provider.clone(),
        context.reads.clone(),
        u64::try_from(input.block_index).unwrap_or(u64::MAX),
    );
    precompiles.apply_precompile(&TEMPO_STATE_READER_ADDRESS, |_| {
        Some(recording_tempo_state_reader_precompile(reader.clone()))
    });
    precompiles.apply_precompile(&tempo_zone_contracts::ZONE_TX_CONTEXT_ADDRESS, |_| {
        Some(ZoneTxContext::create())
    });
    precompiles.apply_precompile(&CHAUM_PEDERSEN_VERIFY_ADDRESS, |_| {
        Some(ChaumPedersenVerify.into())
    });
    precompiles.apply_precompile(&AES_GCM_DECRYPT_ADDRESS, |_| Some(AesGcmDecrypt.into()));
    precompiles.apply_precompile(&ZONE_TIP20_FACTORY_ADDRESS, |_| {
        Some(ZoneTokenFactory::create_with_actions(
            &input.evm_env.cfg_env,
            storage_actions.clone(),
        ))
    });

    let policy_provider =
        RecordingPolicyProvider::new(reader.clone(), input.block.tempo_block_number);
    let registry = Some(ZoneTip403ProxyRegistry::new(policy_provider.clone()));
    let sequencer: Arc<dyn SequencerExt> = Arc::new(context.l1_state_provider.clone());
    let zone_cfg = input.evm_env.cfg_env.clone();
    precompiles.apply_precompile(&ZONE_TIP403_PROXY_ADDRESS, |_| {
        Some(ZoneTip403ProxyRegistry::create(policy_provider.clone()))
    });
    let fee_provider = RecordingZonePortalReader::new(
        context.l1_state_provider.clone(),
        context.reads.clone(),
        u64::try_from(input.block_index).unwrap_or(u64::MAX),
    );
    let lookup_storage_actions = storage_actions.clone();
    precompiles.set_precompile_lookup(move |address: &Address| {
        if is_tip20_prefix(*address) {
            Some(ZoneTip20Token::create_with_actions(
                *address,
                &zone_cfg,
                registry.clone(),
                sequencer.clone(),
                lookup_storage_actions.clone(),
            ))
        } else if *address == TIP_FEE_MANAGER_ADDRESS {
            Some(ZoneFeeManager::create(fee_provider.clone(), &zone_cfg))
        } else {
            None
        }
    });
}

fn recording_tempo_state_reader_precompile(reader: RecordingTempoStateReader) -> DynPrecompile {
    DynPrecompile::new_stateful(
        PrecompileId::Custom("RecordingTempoStateReader".into()),
        move |input| {
            if !input.is_direct_call() {
                return Ok(PrecompileOutput::revert(
                    0,
                    TempoStateReaderAbi::DelegateCallNotAllowed {}
                        .abi_encode()
                        .into(),
                    input.reservoir,
                ));
            }

            if input.caller != TEMPO_STATE_ADDRESS {
                return Ok(PrecompileOutput::revert(
                    0,
                    TempoStateReaderAbi::Unauthorized {}.abi_encode().into(),
                    input.reservoir,
                ));
            }

            if input.data.len() < 4 {
                return Ok(PrecompileOutput::revert(0, Bytes::new(), input.reservoir));
            }

            let selector: [u8; 4] = input.data[..4].try_into().expect("selector length checked");
            if selector == TempoStateReaderAbi::readStorageAtCall::SELECTOR {
                recording_tempo_read_storage_at(&reader, input.data, input.reservoir)
            } else if selector == TempoStateReaderAbi::readStorageBatchAtCall::SELECTOR {
                recording_tempo_read_storage_batch_at(&reader, input.data, input.reservoir)
            } else {
                Ok(PrecompileOutput::revert(0, Bytes::new(), input.reservoir))
            }
        },
    )
}

fn recording_tempo_read_storage_at(
    reader: &RecordingTempoStateReader,
    data: &[u8],
    reservoir: u64,
) -> PrecompileResult {
    let call = match TempoStateReaderAbi::readStorageAtCall::abi_decode(data) {
        Ok(call) => call,
        Err(_) => return Ok(PrecompileOutput::revert(0, Bytes::new(), reservoir)),
    };
    let value = reader.read_storage_word(call.blockNumber, call.account, call.slot)?;
    let output = TempoStateReaderAbi::readStorageAtCall::abi_encode_returns(&value);
    Ok(PrecompileOutput::new(
        TEMPO_STATE_READER_BASE_GAS + TEMPO_STATE_READER_PER_SLOT_GAS,
        output.into(),
        reservoir,
    ))
}

fn recording_tempo_read_storage_batch_at(
    reader: &RecordingTempoStateReader,
    data: &[u8],
    reservoir: u64,
) -> PrecompileResult {
    let call = match TempoStateReaderAbi::readStorageBatchAtCall::abi_decode(data) {
        Ok(call) => call,
        Err(_) => return Ok(PrecompileOutput::revert(0, Bytes::new(), reservoir)),
    };
    let slot_count = u64::try_from(call.slots.len()).map_err(|_| {
        PrecompileError::Fatal("TempoStateReader slot count overflows u64".to_string())
    })?;
    let gas = TEMPO_STATE_READER_PER_SLOT_GAS
        .checked_mul(slot_count)
        .and_then(|slot_gas| TEMPO_STATE_READER_BASE_GAS.checked_add(slot_gas))
        .ok_or_else(|| PrecompileError::Fatal("TempoStateReader gas overflow".to_string()))?;
    let mut values = Vec::with_capacity(call.slots.len());
    for slot in call.slots {
        values.push(reader.read_storage_word(call.blockNumber, call.account, slot)?);
    }
    let output = TempoStateReaderAbi::readStorageBatchAtCall::abi_encode_returns(&values);
    Ok(PrecompileOutput::new(gas, output.into(), reservoir))
}

#[derive(Debug, Clone)]
struct RecordingTempoStateReader {
    l1_state_provider: L1StateProvider,
    reads: Arc<Mutex<TempoExecutionReadSet>>,
    zone_block_index: u64,
}

impl RecordingTempoStateReader {
    fn new(
        l1_state_provider: L1StateProvider,
        reads: Arc<Mutex<TempoExecutionReadSet>>,
        zone_block_index: u64,
    ) -> Self {
        Self {
            l1_state_provider,
            reads,
            zone_block_index,
        }
    }

    fn read_storage_word(
        &self,
        tempo_block_number: u64,
        account: Address,
        slot: B256,
    ) -> Result<B256, PrecompileError> {
        self.reads
            .lock()
            .map_err(|_| PrecompileError::Fatal("Tempo read collection mutex poisoned".into()))?
            .record(self.zone_block_index, tempo_block_number, account, slot);
        self.l1_state_provider
            .get_storage(account, slot, tempo_block_number)
            .map_err(|err| {
                zone_precompiles::zone_rpc_error(format!(
                    "L1 storage unavailable for account={account} slot={slot} block={tempo_block_number}: {err}"
                ))
            })
    }
}

#[derive(Debug, Clone)]
struct RecordingZonePortalReader {
    l1_state_provider: L1StateProvider,
    reads: Arc<Mutex<TempoExecutionReadSet>>,
    zone_block_index: u64,
}

impl RecordingZonePortalReader {
    fn new(
        l1_state_provider: L1StateProvider,
        reads: Arc<Mutex<TempoExecutionReadSet>>,
        zone_block_index: u64,
    ) -> Self {
        Self {
            l1_state_provider,
            reads,
            zone_block_index,
        }
    }
}

impl zone_precompiles::L1StorageReader for RecordingZonePortalReader {
    fn read_l1_storage(
        &self,
        account: Address,
        slot: B256,
        block_number: u64,
    ) -> Result<B256, PrecompileError> {
        self.reads
            .lock()
            .map_err(|_| PrecompileError::Fatal("Tempo read collection mutex poisoned".into()))?
            .record(self.zone_block_index, block_number, account, slot);
        self.l1_state_provider
            .get_storage(account, slot, block_number)
            .map_err(|err| {
                zone_precompiles::zone_rpc_error(format!(
                    "L1 storage unavailable for account={account} slot={slot} block={block_number}: {err}"
                ))
            })
    }
}

impl zone_precompiles::ZonePortalReader for RecordingZonePortalReader {
    fn portal_address(&self) -> Address {
        self.l1_state_provider.portal_address()
    }
}

#[derive(Debug, Clone)]
struct RecordingPolicyProvider {
    tempo_state_reader: RecordingTempoStateReader,
    tempo_block_number: u64,
}

impl RecordingPolicyProvider {
    fn new(tempo_state_reader: RecordingTempoStateReader, tempo_block_number: u64) -> Self {
        Self {
            tempo_state_reader,
            tempo_block_number,
        }
    }

    fn missing_policy(policy_id: u64) -> PrecompileError {
        PrecompileError::Fatal(format!(
            "missing witness-backed TIP-403 policy data for policy {policy_id}"
        ))
    }
}

impl PolicyCheck for RecordingPolicyProvider {
    fn is_authorized(
        &self,
        policy_id: u64,
        _user: Address,
        _role: AuthRole,
    ) -> Result<bool, PrecompileError> {
        match policy_id {
            REJECT_ALL_POLICY_ID => Ok(false),
            ALLOW_ALL_POLICY_ID => Ok(true),
            _ => Err(Self::missing_policy(policy_id)),
        }
    }

    fn resolve_transfer_policy_id(&self, token: Address) -> Result<u64, PrecompileError> {
        let word = self.tempo_state_reader.read_storage_word(
            self.tempo_block_number,
            token,
            B256::from(TIP20_TRANSFER_POLICY_ID_SLOT),
        )?;
        extract_from_word::<u64>(
            U256::from_be_bytes(word.0),
            TIP20_TRANSFER_POLICY_ID_OFFSET,
            TIP20_TRANSFER_POLICY_ID_BYTES,
        )
        .map_err(|err| {
            PrecompileError::Fatal(format!(
                "invalid packed TIP-20 transfer policy id for token {token}: {err}"
            ))
        })
    }

    fn policy_type_sync(&self, policy_id: u64) -> Result<PolicyType, PrecompileError> {
        match policy_id {
            REJECT_ALL_POLICY_ID => Ok(PolicyType::WHITELIST),
            ALLOW_ALL_POLICY_ID => Ok(PolicyType::BLACKLIST),
            _ => Err(Self::missing_policy(policy_id)),
        }
    }

    fn compound_policy_data(&self, policy_id: u64) -> Result<(u64, u64, u64), PrecompileError> {
        Err(Self::missing_policy(policy_id))
    }

    fn policy_exists(&self, policy_id: u64) -> Result<bool, PrecompileError> {
        match policy_id {
            REJECT_ALL_POLICY_ID | ALLOW_ALL_POLICY_ID => Ok(true),
            _ => Err(Self::missing_policy(policy_id)),
        }
    }

    fn policy_id_counter(&self) -> u64 {
        2
    }
}

#[derive(Debug, Clone, Default)]
struct TempoExecutionReadSet {
    reads: BTreeMap<TempoExecutionReadKey, BTreeSet<B256>>,
}

impl TempoExecutionReadSet {
    fn record(
        &mut self,
        zone_block_index: u64,
        tempo_block_number: u64,
        account: Address,
        slot: B256,
    ) {
        self.reads
            .entry(TempoExecutionReadKey {
                zone_block_index,
                tempo_block_number,
                account,
            })
            .or_default()
            .insert(slot);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TempoExecutionReadKey {
    zone_block_index: u64,
    tempo_block_number: u64,
    account: Address,
}

fn tempo_state_proof_from_reads(
    l1_state_provider: &L1StateProvider,
    reads: TempoExecutionReadSet,
) -> eyre::Result<BatchStateProof> {
    let mut node_pool = BTreeMap::new();
    let mut state_reads = Vec::new();

    for (key, slots) in reads.reads {
        let slots = slots.into_iter().collect::<Vec<_>>();
        let proof = l1_state_provider
            .get_proof(key.account, slots.clone(), key.tempo_block_number)
            .wrap_err_with(|| {
                format!(
                    "failed to fetch Tempo proof for account {} at block {}",
                    key.account, key.tempo_block_number
                )
            })?;
        eyre::ensure!(
            proof.address == key.account,
            "Tempo proof address mismatch: requested {}, got {}",
            key.account,
            proof.address
        );
        let account_proof_node_hashes = insert_witness_nodes(&mut node_pool, &proof.account_proof);
        let mut storage_proofs = proof
            .storage_proof
            .into_iter()
            .map(|proof| (proof.key.as_b256(), proof))
            .collect::<BTreeMap<_, _>>();

        for slot in slots {
            let storage_proof = storage_proofs.remove(&slot).ok_or_else(|| {
                eyre::eyre!(
                    "Tempo proof for account {} at block {} omitted requested slot {}",
                    key.account,
                    key.tempo_block_number,
                    slot
                )
            })?;
            let storage_proof_node_hashes =
                insert_witness_nodes(&mut node_pool, &storage_proof.proof);
            state_reads.push(L1StateRead {
                zone_block_index: key.zone_block_index,
                tempo_block_number: key.tempo_block_number,
                account: key.account,
                account_nonce: proof.nonce,
                account_balance: proof.balance,
                account_storage_root: proof.storage_hash,
                account_code_hash: proof.code_hash,
                account_proof_node_hashes: account_proof_node_hashes.clone(),
                slot: storage_word_slot(slot),
                value: storage_proof.value,
                storage_proof_node_hashes,
            });
        }
    }

    Ok(BatchStateProof {
        node_pool,
        reads: state_reads,
    })
}

#[derive(Debug, Clone)]
struct ZoneExecutionReadSet {
    reads: BTreeMap<Address, BTreeSet<B256>>,
}

impl ZoneExecutionReadSet {
    fn new(reads: BTreeMap<Address, BTreeSet<B256>>) -> Self {
        Self { reads }
    }

    fn record_account(&mut self, account: Address) {
        self.reads.entry(account).or_default();
    }

    fn record_storage(&mut self, account: Address, slot: B256) {
        self.reads.entry(account).or_default().insert(slot);
    }

    fn into_reads(self) -> BTreeMap<Address, BTreeSet<B256>> {
        self.reads
    }
}

struct RecordingStateProviderDb<'a> {
    state: &'a dyn StateProvider,
    reads: ZoneExecutionReadSet,
}

impl fmt::Debug for RecordingStateProviderDb<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordingStateProviderDb")
            .field("reads", &self.reads)
            .finish_non_exhaustive()
    }
}

impl<'a> RecordingStateProviderDb<'a> {
    fn new(state: &'a dyn StateProvider, reads: BTreeMap<Address, BTreeSet<B256>>) -> Self {
        Self {
            state,
            reads: ZoneExecutionReadSet::new(reads),
        }
    }

    fn into_reads(self) -> BTreeMap<Address, BTreeSet<B256>> {
        self.reads.into_reads()
    }

    fn record_storage(&mut self, account: Address, slot: B256) {
        self.reads.record_storage(account, slot);
    }
}

impl Database for RecordingStateProviderDb<'_> {
    type Error = RecordingStateProviderDbError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.reads.record_account(address);
        self.state
            .basic_account(&address)
            .map(|account| account.map(AccountInfo::from))
            .map_err(|err| RecordingStateProviderDbError::provider("basic_account", err))
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<RevmBytecode, Self::Error> {
        let bytecode = self
            .state
            .bytecode_by_hash(&code_hash)
            .map_err(|err| RecordingStateProviderDbError::provider("bytecode_by_hash", err))?
            .ok_or(RecordingStateProviderDbError::MissingCode(code_hash))?;
        Ok(bytecode.0)
    }

    fn storage(
        &mut self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        let slot = B256::from(index);
        self.reads.record_storage(address, slot);
        self.state
            .storage(address, slot)
            .map(|value| value.unwrap_or(U256::ZERO))
            .map_err(|err| RecordingStateProviderDbError::provider("storage", err))
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.state
            .block_hash(number)
            .map_err(|err| RecordingStateProviderDbError::provider("block_hash", err))?
            .ok_or(RecordingStateProviderDbError::MissingBlockHash(number))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordingStateProviderDbError {
    Provider {
        operation: &'static str,
        error: String,
    },
    MissingCode(B256),
    MissingBlockHash(u64),
}

impl RecordingStateProviderDbError {
    fn provider(operation: &'static str, err: impl fmt::Display) -> Self {
        Self::Provider {
            operation,
            error: err.to_string(),
        }
    }
}

impl fmt::Display for RecordingStateProviderDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { operation, error } => {
                write!(f, "local state provider {operation} failed: {error}")
            }
            Self::MissingCode(code_hash) => {
                write!(f, "local state provider is missing bytecode {code_hash}")
            }
            Self::MissingBlockHash(number) => {
                write!(f, "local state provider is missing block hash for {number}")
            }
        }
    }
}

impl std::error::Error for RecordingStateProviderDbError {}
impl DBErrorMarker for RecordingStateProviderDbError {}

fn required_initial_zone_reads() -> BTreeMap<Address, BTreeSet<B256>> {
    let mut reads = BTreeMap::new();
    add_slots(&mut reads, TEMPO_STATE_ADDRESS, TEMPO_STATE_HEADER_SLOTS);
    add_slots(
        &mut reads,
        ZONE_INBOX_ADDRESS,
        [
            proof_slot(ZONE_INBOX_PROCESSED_HASH_SLOT),
            proof_slot(ZONE_INBOX_PROCESSED_NUMBER_SLOT),
        ],
    );
    add_slots(
        &mut reads,
        ZONE_OUTBOX_ADDRESS,
        [
            proof_slot(ZONE_OUTBOX_LAST_BATCH_HASH_SLOT),
            proof_slot(ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT),
        ],
    );
    reads
}

fn add_withdrawal_finalization_reads(
    state: &dyn StateProvider,
    blocks: &[ValidatedZoneBlockData],
    reads: &mut BTreeMap<Address, BTreeSet<B256>>,
) -> eyre::Result<()> {
    let mut requested = U256::ZERO;
    for block in blocks {
        if let ZoneWithdrawalFinalization::Finalize(finalization) =
            &block.witness_block.withdrawal_finalization
        {
            requested = requested.checked_add(finalization.count).ok_or_else(|| {
                eyre::eyre!("local witness withdrawal finalization count overflowed")
            })?;
        }
    }
    if requested.is_zero() {
        return Ok(());
    }

    add_slots(
        reads,
        ZONE_OUTBOX_ADDRESS,
        [
            proof_slot(ZONE_OUTBOX_PACKED_SLOT),
            proof_slot(ZONE_OUTBOX_PENDING_WITHDRAWALS_SLOT),
            proof_slot(ZONE_OUTBOX_PENDING_WITHDRAWALS_HEAD_SLOT),
        ],
    );

    let length = read_storage_u256(
        state,
        ZONE_OUTBOX_ADDRESS,
        ZONE_OUTBOX_PENDING_WITHDRAWALS_SLOT,
    )?;
    let head = read_storage_u256(
        state,
        ZONE_OUTBOX_ADDRESS,
        ZONE_OUTBOX_PENDING_WITHDRAWALS_HEAD_SLOT,
    )?;
    if head >= length {
        return Ok(());
    }

    let pending = length - head;
    let effective_count = if requested > pending {
        pending
    } else {
        requested
    };
    let count = u64_from_u256(effective_count, "withdrawal finalization count")?;
    let start = u64_from_u256(head, "pending withdrawal head")?;

    for offset in 0..count {
        let index = start
            .checked_add(offset)
            .ok_or_else(|| eyre::eyre!("pending withdrawal index overflowed"))?;
        add_pending_withdrawal_reads(state, reads, index)?;
    }

    Ok(())
}

fn add_pending_withdrawal_reads(
    state: &dyn StateProvider,
    reads: &mut BTreeMap<Address, BTreeSet<B256>>,
    index: u64,
) -> eyre::Result<()> {
    for offset in 0..PENDING_WITHDRAWAL_STORAGE_SLOTS {
        add_slot(
            reads,
            ZONE_OUTBOX_ADDRESS,
            proof_slot(pending_withdrawal_slot(index, offset)?),
        );
    }

    add_dynamic_bytes_reads(
        state,
        reads,
        pending_withdrawal_slot(index, PENDING_WITHDRAWAL_CALLBACK_DATA_OFFSET)?,
    )?;
    add_dynamic_bytes_reads(
        state,
        reads,
        pending_withdrawal_slot(index, PENDING_WITHDRAWAL_REVEAL_TO_OFFSET)?,
    )?;

    Ok(())
}

fn add_dynamic_bytes_reads(
    state: &dyn StateProvider,
    reads: &mut BTreeMap<Address, BTreeSet<B256>>,
    slot: U256,
) -> eyre::Result<()> {
    let marker = read_storage_u256(state, ZONE_OUTBOX_ADDRESS, slot)?;
    if (marker & U256::ONE).is_zero() {
        return Ok(());
    }

    let byte_len = u64_from_u256((marker - U256::ONE) >> 1, "dynamic bytes length")?;
    let slot_count = byte_len.saturating_add(31) / 32;
    let base = storage_data_base_slot(slot);
    for offset in 0..slot_count {
        let data_slot = base
            .checked_add(U256::from(offset))
            .ok_or_else(|| eyre::eyre!("dynamic bytes storage slot overflowed"))?;
        add_slot(reads, ZONE_OUTBOX_ADDRESS, proof_slot(data_slot));
    }

    Ok(())
}

fn add_slots(
    reads: &mut BTreeMap<Address, BTreeSet<B256>>,
    account: Address,
    slots: impl IntoIterator<Item = B256>,
) {
    for slot in slots {
        add_slot(reads, account, slot);
    }
}

fn add_slot(reads: &mut BTreeMap<Address, BTreeSet<B256>>, account: Address, slot: B256) {
    reads.entry(account).or_default().insert(slot);
}

fn read_storage_u256(
    state: &dyn StateProvider,
    account: Address,
    slot: U256,
) -> eyre::Result<U256> {
    Ok(state
        .storage(account, proof_slot(slot))?
        .unwrap_or_default())
}

fn u64_from_u256(value: U256, label: &str) -> eyre::Result<u64> {
    eyre::ensure!(
        value <= U256::from(u64::MAX),
        "{label} does not fit in u64: {value}"
    );
    Ok(value.to::<u64>())
}

fn pending_withdrawal_slot(index: u64, offset: u64) -> eyre::Result<U256> {
    let element_offset = U256::from(index)
        .checked_mul(U256::from(PENDING_WITHDRAWAL_STORAGE_SLOTS))
        .and_then(|value| value.checked_add(U256::from(offset)))
        .ok_or_else(|| eyre::eyre!("pending withdrawal slot overflowed"))?;
    storage_data_base_slot(ZONE_OUTBOX_PENDING_WITHDRAWALS_SLOT)
        .checked_add(element_offset)
        .ok_or_else(|| eyre::eyre!("pending withdrawal slot overflowed"))
}

fn storage_data_base_slot(slot: U256) -> U256 {
    U256::from_be_bytes(keccak256(slot.to_be_bytes::<32>()).0)
}

fn initial_zone_state_witness(
    state: &dyn StateProvider,
    state_root: B256,
    reads: &BTreeMap<Address, BTreeSet<B256>>,
) -> eyre::Result<ZoneStateWitness> {
    let mut node_pool = BTreeMap::new();
    let mut account_reads = Vec::new();
    let mut storage_reads = Vec::new();

    for (account, slots) in reads {
        let slots = slots.iter().copied().collect::<Vec<_>>();
        let proof = state.proof(TrieInput::default(), *account, &slots)?;
        proof.verify(state_root).map_err(|err| {
            eyre::eyre!("reth state proof for account {account} is invalid: {err}")
        })?;
        account_reads.push(zone_account_read_from_reth_proof(
            state,
            &mut node_pool,
            &proof,
        )?);
        storage_reads.extend(zone_storage_reads_from_reth_proof(
            &mut node_pool,
            *account,
            &proof,
        ));
    }

    Ok(ZoneStateWitness {
        state_root,
        node_pool,
        account_reads,
        storage_reads,
    })
}

fn zone_account_read_from_reth_proof(
    state: &dyn StateProvider,
    node_pool: &mut BTreeMap<B256, Bytes>,
    proof: &RethAccountProof,
) -> eyre::Result<ZoneAccountRead> {
    let (nonce, balance, code_hash) = match proof.info {
        Some(account) => (account.nonce, account.balance, account.get_bytecode_hash()),
        None => {
            eyre::ensure!(
                proof.storage_root == EMPTY_ROOT_HASH,
                "absent account {} proof has non-empty storage root {}",
                proof.address,
                proof.storage_root
            );
            (0, U256::ZERO, KECCAK_EMPTY)
        }
    };
    let code = if code_hash == KECCAK_EMPTY {
        ZoneAccountCode::Empty
    } else {
        let bytecode = state.bytecode_by_hash(&code_hash)?.ok_or_else(|| {
            eyre::eyre!("missing bytecode preimage for account {}", proof.address)
        })?;
        let bytes = Bytes::copy_from_slice(bytecode.original_byte_slice());
        eyre::ensure!(
            keccak256(bytes.as_ref()) == code_hash,
            "bytecode preimage hash mismatch for account {}",
            proof.address
        );
        ZoneAccountCode::Bytecode(bytes)
    };

    Ok(ZoneAccountRead {
        account: proof.address,
        nonce,
        balance,
        storage_root: proof.storage_root,
        code_hash,
        code,
        proof_node_hashes: insert_witness_nodes(node_pool, &proof.proof),
    })
}

fn zone_storage_reads_from_reth_proof(
    node_pool: &mut BTreeMap<B256, Bytes>,
    account: Address,
    proof: &RethAccountProof,
) -> Vec<ZoneStorageRead> {
    proof
        .storage_proofs
        .iter()
        .map(|storage_proof| ZoneStorageRead {
            account,
            slot: storage_word_slot(storage_proof.key),
            value: storage_proof.value,
            proof_node_hashes: insert_witness_nodes(node_pool, &storage_proof.proof),
        })
        .collect()
}

fn insert_witness_nodes(node_pool: &mut BTreeMap<B256, Bytes>, nodes: &[Bytes]) -> Vec<B256> {
    nodes
        .iter()
        .map(|node| {
            let hash = keccak256(node.as_ref());
            node_pool.entry(hash).or_insert_with(|| node.clone());
            hash
        })
        .collect()
}

fn initial_tempo_binding_from_state(state: &ZoneStateWitness) -> eyre::Result<LocalTempoBinding> {
    let block_hash = required_storage_word(
        state,
        TEMPO_STATE_ADDRESS,
        storage_word_slot(TEMPO_BLOCK_HASH_SLOT),
    )?;
    let packed = required_storage_word(
        state,
        TEMPO_STATE_ADDRESS,
        storage_word_slot(TEMPO_PACKED_SLOT),
    )?;
    Ok(LocalTempoBinding {
        block_number: low_u64(packed),
        block_hash: B256::from(block_hash),
    })
}

fn final_tempo_binding_from_blocks(
    initial: LocalTempoBinding,
    blocks: &[ValidatedZoneBlockData],
) -> eyre::Result<LocalTempoBinding> {
    let mut current = initial;
    for (index, block) in blocks.iter().enumerate() {
        let ZoneTempoImport::Advance(import) = &block.witness_block.tempo_import else {
            continue;
        };
        let mut cursor = import.header_rlp.as_ref();
        let header = TempoHeader::decode(&mut cursor)
            .map_err(|err| eyre::eyre!("zone block {index} has invalid Tempo import RLP: {err}"))?;
        eyre::ensure!(
            cursor.is_empty(),
            "zone block {index} Tempo import has trailing RLP bytes"
        );
        current = LocalTempoBinding {
            block_number: header.inner.number,
            block_hash: keccak256(import.header_rlp.as_ref()),
        };
    }
    Ok(current)
}

fn previous_withdrawal_batch_index(state: &ZoneStateWitness) -> eyre::Result<u64> {
    required_storage_word(
        state,
        ZONE_OUTBOX_ADDRESS,
        ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
    )
    .map(low_u64)
}

fn required_storage_word(
    state: &ZoneStateWitness,
    account: Address,
    slot: U256,
) -> eyre::Result<U256> {
    state
        .storage_reads
        .iter()
        .find(|read| read.account == account && read.slot == slot)
        .map(|read| read.value)
        .ok_or_else(|| eyre::eyre!("initial zone witness missing storage read {account}[{slot}]"))
}

fn low_u64(value: U256) -> u64 {
    (value & U256::from(u64::MAX)).to::<u64>()
}

fn proof_slot(slot: U256) -> B256 {
    B256::from(slot)
}

fn storage_word_slot(slot: B256) -> U256 {
    slot.into()
}

fn zone_header_from_tempo_header(header: &TempoHeader, parent_hash: B256) -> ZoneHeader {
    ZoneHeader {
        parent_hash,
        beneficiary: header.beneficiary(),
        state_root: header.state_root(),
        transactions_root: header.transactions_root(),
        receipts_root: header.receipts_root(),
        number: header.number(),
        timestamp: header.timestamp(),
        protocol_version: ZONE_BLOCK_PROTOCOL_VERSION,
    }
}

fn encode_zone_header(header: &ZoneHeader) -> Bytes {
    let mut encoded = Vec::new();
    header.encode(&mut encoded);
    Bytes::from(encoded)
}

impl<P> ProverWitnessSource for LocalNodeProverWitnessSource<P>
where
    P: BlockReader<
            Header = TempoHeader,
            Block = primitives::Block,
            Transaction = TempoTxEnvelope,
            Receipt = primitives::TempoReceipt,
        > + ChainSpecProvider<ChainSpec = TempoChainSpec>
        + StateProviderFactory
        + StateReader<Receipt = primitives::TempoReceipt>
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn build_witness<'a>(
        &'a self,
        request: ProverWitnessRequest,
    ) -> BoxFuture<'a, eyre::Result<BatchWitness>> {
        Box::pin(async move { self.build_witness_sync(request) })
    }
}

#[cfg(test)]
mod tests {
    use alloy_consensus::{BlockBody, Header, Signed, TxLegacy};
    use alloy_eips::eip2718::Encodable2718 as _;
    use alloy_primitives::{Signature, TxKind, U256};
    use alloy_sol_types::SolCall as _;
    use tempo_primitives::transaction::envelope::TEMPO_SYSTEM_TX_SIGNATURE;

    use super::*;

    const TEST_CHAIN_ID: u64 = 421_700_001;
    const TEST_SPEC: TempoHardfork = TempoHardfork::T1;

    fn sealed_header(number: u64, parent_hash: B256) -> SealedHeader<TempoHeader> {
        SealedHeader::seal_slow(TempoHeader {
            inner: Header {
                parent_hash,
                beneficiary: Address::repeat_byte(0x42),
                number,
                gas_limit: 30_000_000,
                timestamp: number,
                mix_hash: B256::repeat_byte(0x22),
                base_fee_per_gas: Some(7),
                parent_beacon_block_root: Some(B256::repeat_byte(0x33)),
                slot_number: Some(number),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    fn block_for_header(
        header: &SealedHeader<TempoHeader>,
        transactions: Vec<TempoTxEnvelope>,
    ) -> primitives::Block {
        primitives::Block::new(
            header.header().clone(),
            BlockBody {
                transactions,
                ommers: Vec::new(),
                withdrawals: None,
            },
        )
    }

    fn legacy_transaction() -> TempoTxEnvelope {
        TempoTxEnvelope::Legacy(Signed::new_unhashed(
            TxLegacy {
                chain_id: None,
                nonce: 0,
                gas_price: 0,
                gas_limit: 21_000,
                to: TxKind::Call(Address::ZERO),
                value: U256::ZERO,
                input: Bytes::new(),
            },
            Signature::test_signature(),
        ))
    }

    fn test_validated_zone_block_data(
        header: &SealedHeader<TempoHeader>,
        block: &primitives::Block,
        receipts: &[primitives::TempoReceipt],
    ) -> eyre::Result<ValidatedZoneBlockData> {
        validated_zone_block_data(
            TEST_CHAIN_ID,
            TEST_SPEC,
            header.header().parent_hash(),
            header,
            block,
            receipts,
        )
    }

    fn system_legacy_transaction(to: Address, input: Bytes) -> TempoTxEnvelope {
        TempoTxEnvelope::Legacy(Signed::new_unhashed(
            TxLegacy {
                chain_id: None,
                nonce: 0,
                gas_price: 0,
                gas_limit: 0,
                to: TxKind::Call(to),
                value: U256::ZERO,
                input,
            },
            TEMPO_SYSTEM_TX_SIGNATURE,
        ))
    }

    fn advance_tempo_transaction(header_rlp: Bytes) -> TempoTxEnvelope {
        system_legacy_transaction(
            ZONE_INBOX_ADDRESS,
            tempo_zone_contracts::ZoneInbox::advanceTempoCall {
                header: header_rlp,
                deposits: Vec::new(),
                decryptions: Vec::new(),
                enabledTokens: Vec::new(),
            }
            .abi_encode()
            .into(),
        )
    }

    fn finalize_withdrawal_transaction(
        block_number: u64,
        encrypted_senders: Vec<Bytes>,
    ) -> TempoTxEnvelope {
        system_legacy_transaction(
            ZONE_OUTBOX_ADDRESS,
            tempo_zone_contracts::ZoneOutbox::finalizeWithdrawalBatchCall {
                count: U256::from(3),
                blockNumber: block_number,
                encryptedSenders: encrypted_senders,
            }
            .abi_encode()
            .into(),
        )
    }

    fn raw_transactions(transactions: &[TempoTxEnvelope]) -> Vec<Bytes> {
        transactions
            .iter()
            .map(|tx| Bytes::from(tx.encoded_2718()))
            .collect()
    }

    fn witness_request(
        from_zone_block: u64,
        to_zone_block: u64,
        prev_block_hash: B256,
        next_block_hash: B256,
    ) -> ProverWitnessRequest {
        ProverWitnessRequest {
            from_zone_block,
            to_zone_block,
            batch: zone_sequencer::UnprovenBatchData {
                tempo_block_number: 1,
                prev_block_hash,
                next_block_hash,
                prev_processed_deposit_hash: B256::ZERO,
                next_processed_deposit_hash: B256::ZERO,
                prev_deposit_number: 0,
                next_deposit_number: 0,
                withdrawal_queue_hash: B256::ZERO,
            },
            public_inputs: zone_prover::types::PublicInputs {
                prev_block_hash,
                tempo_block_number: 1,
                anchor_block_number: 1,
                anchor_block_hash: B256::ZERO,
                expected_withdrawal_batch_index: 1,
                sequencer: Address::repeat_byte(0x42),
            },
            tempo_ancestry_headers: Vec::new(),
        }
    }

    #[test]
    fn validates_and_preserves_canonical_header_range() {
        let parent = sealed_header(9, B256::repeat_byte(0x09));
        let parent_hash = parent.hash();
        let child = sealed_header(10, parent_hash);
        let child_hash = child.hash();
        let grandchild = sealed_header(11, child_hash);
        let grandchild_hash = grandchild.hash();
        let prev_zone_header =
            zone_header_from_tempo_header(parent.header(), B256::repeat_byte(0xaa));
        let child_zone_header =
            zone_header_from_tempo_header(child.header(), prev_zone_header.hash());
        let final_zone_header =
            zone_header_from_tempo_header(grandchild.header(), child_zone_header.hash());
        let request = witness_request(10, 11, prev_zone_header.hash(), final_zone_header.hash());

        validate_zone_public_hashes(&request, &prev_zone_header, &final_zone_header).unwrap();
        let range = validate_canonical_zone_header_range(&request, parent, vec![child, grandchild])
            .unwrap();

        assert_eq!(range.from_zone_block, 10);
        assert_eq!(range.to_zone_block, 11);
        assert_eq!(range.parent_number, 9);
        assert_eq!(range.parent_hash(), parent_hash);
        assert_eq!(range.header_count(), 2);
        assert_eq!(range.headers[1].hash(), grandchild_hash);
    }

    #[test]
    fn rejects_invalid_requested_header_ranges() {
        let request = witness_request(11, 10, B256::ZERO, B256::ZERO);
        let err = validate_requested_zone_header_range(&request).unwrap_err();
        assert!(
            err.to_string().contains("from_zone_block 11 is after"),
            "unexpected error: {err}"
        );

        let request = witness_request(0, 0, B256::ZERO, B256::ZERO);
        let err = validate_requested_zone_header_range(&request).unwrap_err();
        assert!(
            err.to_string().contains("starting at genesis"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn initial_zone_reads_use_raw_storage_keys_for_reth_proofs() {
        let reads = required_initial_zone_reads();

        assert!(reads[&TEMPO_STATE_ADDRESS].contains(&TEMPO_BLOCK_HASH_SLOT));
        assert!(reads[&TEMPO_STATE_ADDRESS].contains(&TEMPO_WRAPPER_GAS_LIMITS_SLOT));
        assert!(reads[&TEMPO_STATE_ADDRESS].contains(&TEMPO_PARENT_HASH_SLOT));
        assert!(reads[&TEMPO_STATE_ADDRESS].contains(&TEMPO_BENEFICIARY_SLOT));
        assert!(reads[&TEMPO_STATE_ADDRESS].contains(&TEMPO_STATE_ROOT_SLOT));
        assert!(reads[&TEMPO_STATE_ADDRESS].contains(&TEMPO_TRANSACTIONS_ROOT_SLOT));
        assert!(reads[&TEMPO_STATE_ADDRESS].contains(&TEMPO_RECEIPTS_ROOT_SLOT));
        assert!(reads[&TEMPO_STATE_ADDRESS].contains(&TEMPO_PACKED_SLOT));
        assert!(reads[&TEMPO_STATE_ADDRESS].contains(&TEMPO_TIMESTAMP_MILLIS_SLOT));
        assert!(reads[&TEMPO_STATE_ADDRESS].contains(&TEMPO_PREV_RANDAO_SLOT));
        assert!(reads[&ZONE_INBOX_ADDRESS].contains(&B256::from(ZONE_INBOX_PROCESSED_HASH_SLOT)));
        assert!(reads[&ZONE_INBOX_ADDRESS].contains(&B256::from(ZONE_INBOX_PROCESSED_NUMBER_SLOT)));
        assert!(
            reads[&ZONE_OUTBOX_ADDRESS].contains(&B256::from(ZONE_OUTBOX_LAST_BATCH_HASH_SLOT))
        );
        assert!(
            reads[&ZONE_OUTBOX_ADDRESS].contains(&B256::from(ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT))
        );
        assert_eq!(storage_word_slot(TEMPO_STATE_ROOT_SLOT), U256::from(4));
        assert_eq!(storage_word_slot(TEMPO_PREV_RANDAO_SLOT), U256::from(9));
        assert_eq!(
            proof_slot(ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT),
            B256::from(U256::from(2))
        );
    }

    #[test]
    fn pending_withdrawal_slots_match_solidity_dynamic_array_layout() {
        let base = storage_data_base_slot(ZONE_OUTBOX_PENDING_WITHDRAWALS_SLOT);
        assert_eq!(pending_withdrawal_slot(0, 0).unwrap(), base);
        assert_eq!(
            pending_withdrawal_slot(1, 0).unwrap(),
            base + U256::from(PENDING_WITHDRAWAL_STORAGE_SLOTS)
        );
        assert_eq!(
            pending_withdrawal_slot(2, PENDING_WITHDRAWAL_REVEAL_TO_OFFSET).unwrap(),
            base + U256::from(2 * PENDING_WITHDRAWAL_STORAGE_SLOTS)
                + U256::from(PENDING_WITHDRAWAL_REVEAL_TO_OFFSET)
        );
    }

    #[test]
    fn rejects_header_count_mismatch() {
        let parent = sealed_header(9, B256::repeat_byte(0x09));
        let parent_hash = parent.hash();
        let child = sealed_header(10, parent_hash);
        let child_hash = child.hash();
        let request = witness_request(10, 11, parent_hash, child_hash);

        let err = validate_canonical_zone_header_range(&request, parent, vec![child]).unwrap_err();

        assert!(
            err.to_string().contains("header count mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_noncanonical_child_parent_hash() {
        let parent = sealed_header(9, B256::repeat_byte(0x09));
        let parent_hash = parent.hash();
        let child = sealed_header(10, B256::repeat_byte(0xee));
        let child_hash = child.hash();
        let request = witness_request(10, 10, parent_hash, child_hash);

        let err = validate_canonical_zone_header_range(&request, parent, vec![child]).unwrap_err();

        assert!(
            err.to_string().contains("parent mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_final_hash_mismatch() {
        let parent = sealed_header(9, B256::repeat_byte(0x09));
        let child = sealed_header(10, parent.hash());
        let request = witness_request(
            10,
            10,
            zone_header_from_tempo_header(parent.header(), B256::repeat_byte(0xaa)).hash(),
            B256::repeat_byte(0xee),
        );

        let prev_zone_header =
            zone_header_from_tempo_header(parent.header(), B256::repeat_byte(0xaa));
        let err = validate_zone_public_hashes(
            &request,
            &prev_zone_header,
            &zone_header_from_tempo_header(child.header(), prev_zone_header.hash()),
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("zone final header hash mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn extracts_empty_block_data() {
        let header = sealed_header(10, B256::repeat_byte(0x09));
        let block = block_for_header(&header, Vec::new());

        let data = test_validated_zone_block_data(&header, &block, &[]).unwrap();

        assert!(data.raw_transactions.is_empty());
        assert_eq!(data.witness_block.number, 10);
        assert_eq!(
            data.witness_block.parent_hash,
            header.header().parent_hash()
        );
        assert_eq!(data.witness_block.timestamp, header.header().timestamp());
        assert_eq!(
            data.witness_block.beneficiary,
            header.header().beneficiary()
        );
        assert_eq!(
            data.witness_block.protocol_version,
            ZONE_BLOCK_PROTOCOL_VERSION
        );
        assert_eq!(data.witness_block.cfg_env.chain_id, TEST_CHAIN_ID);
        assert_eq!(data.witness_block.cfg_env.spec, TEST_SPEC);
        assert_eq!(
            data.witness_block
                .execution_context
                .parent_beacon_block_root,
            header.header().parent_beacon_block_root().unwrap()
        );
        assert_eq!(
            data.witness_block.block_env.prevrandao,
            header.header().mix_hash().unwrap()
        );
        assert_eq!(data.witness_block.block_env.slot_num, 10);
        assert_eq!(
            data.decoded_transactions.tempo_import,
            ZoneTempoImport::none()
        );
        assert!(data.decoded_transactions.user_transactions.is_empty());
        assert_eq!(
            data.decoded_transactions.withdrawal_finalization,
            ZoneWithdrawalFinalization::none()
        );
        assert_eq!(data.receipt_count, 0);
    }

    #[test]
    fn local_witness_source_allows_dynamic_batches_for_execution_collection() {
        let header = sealed_header(10, B256::repeat_byte(0x09));
        let mut block = zone_block_witness_from_header(
            TEST_CHAIN_ID,
            TEST_SPEC,
            header.header().parent_hash(),
            header.header(),
            DecodedZoneBlockTransactions {
                tempo_import: ZoneTempoImport::none(),
                user_transactions: vec![Bytes::from_static(b"user transaction")],
                withdrawal_finalization: ZoneWithdrawalFinalization::none(),
            },
        )
        .unwrap();

        ensure_local_witness_block_coverage(0, &block).unwrap();

        block.transactions.clear();
        block.tempo_import = ZoneTempoImport::advance(
            Bytes::new(),
            Vec::new(),
            Vec::new(),
            vec![tempo_zone_contracts::EnabledToken {
                token: Address::repeat_byte(0x20),
                name: "Token".into(),
                symbol: "TOK".into(),
                currency: "TOK".into(),
            }],
        );
        ensure_local_witness_block_coverage(0, &block).unwrap();

        block.tempo_import = ZoneTempoImport::none();
        block.withdrawal_finalization =
            ZoneWithdrawalFinalization::finalize(U256::from(1), Vec::new());
        ensure_local_witness_block_coverage(0, &block).unwrap();
    }

    #[test]
    fn local_witness_source_allows_static_header_only_paths() {
        let header = sealed_header(10, B256::repeat_byte(0x09));
        let block = zone_block_witness_from_header(
            TEST_CHAIN_ID,
            TEST_SPEC,
            header.header().parent_hash(),
            header.header(),
            DecodedZoneBlockTransactions {
                tempo_import: ZoneTempoImport::advance(
                    Bytes::from_static(&[0xc0]),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                ),
                user_transactions: Vec::new(),
                withdrawal_finalization: ZoneWithdrawalFinalization::finalize(
                    U256::ZERO,
                    Vec::new(),
                ),
            },
        )
        .unwrap();

        ensure_local_witness_block_coverage(0, &block).unwrap();
    }

    #[test]
    fn rejects_block_witness_missing_required_execution_fields() {
        let header = sealed_header(10, B256::repeat_byte(0x09));
        let mut missing_basefee = header.header().clone();
        missing_basefee.inner.base_fee_per_gas = None;

        let err = zone_block_witness_from_header(
            TEST_CHAIN_ID,
            TEST_SPEC,
            header.header().parent_hash(),
            &missing_basefee,
            DecodedZoneBlockTransactions {
                tempo_import: ZoneTempoImport::none(),
                user_transactions: Vec::new(),
                withdrawal_finalization: ZoneWithdrawalFinalization::none(),
            },
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("missing base_fee_per_gas"),
            "unexpected error: {err}"
        );

        let mut missing_parent_beacon = header.header().clone();
        missing_parent_beacon.inner.parent_beacon_block_root = None;

        let err = zone_block_witness_from_header(
            TEST_CHAIN_ID,
            TEST_SPEC,
            header.header().parent_hash(),
            &missing_parent_beacon,
            DecodedZoneBlockTransactions {
                tempo_import: ZoneTempoImport::none(),
                user_transactions: Vec::new(),
                withdrawal_finalization: ZoneWithdrawalFinalization::none(),
            },
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("missing parent_beacon_block_root"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn decodes_zone_system_transactions_into_witness_fields() {
        let header_rlp = Bytes::from_static(&[0xc0]);
        let encrypted_sender = Bytes::from_static(b"sender");
        let transactions = vec![
            advance_tempo_transaction(header_rlp.clone()),
            legacy_transaction(),
            finalize_withdrawal_transaction(10, vec![encrypted_sender.clone()]),
        ];
        let raw_transactions = raw_transactions(&transactions);

        let decoded = decode_zone_block_transactions(10, &transactions, &raw_transactions).unwrap();

        match &decoded.tempo_import {
            ZoneTempoImport::Advance(import) => {
                assert_eq!(import.header_rlp, header_rlp);
                assert!(import.deposits.is_empty());
                assert!(import.decryptions.is_empty());
                assert!(import.enabled_tokens.is_empty());
            }
            ZoneTempoImport::None => panic!("expected advanceTempo import"),
        }
        assert_eq!(decoded.user_transactions, vec![raw_transactions[1].clone()]);
        match &decoded.withdrawal_finalization {
            ZoneWithdrawalFinalization::Finalize(finalization) => {
                assert_eq!(finalization.count, U256::from(3));
                assert_eq!(finalization.encrypted_senders, vec![encrypted_sender]);
            }
            ZoneWithdrawalFinalization::None => {
                panic!("expected finalizeWithdrawalBatch finalization")
            }
        }
    }

    #[test]
    fn rejects_misordered_zone_system_transactions() {
        let transactions = vec![
            legacy_transaction(),
            advance_tempo_transaction(Bytes::new()),
        ];
        let raw = raw_transactions(&transactions);

        let err = decode_zone_block_transactions(10, &transactions, &raw).unwrap_err();

        assert!(
            err.to_string()
                .contains("advanceTempo system transaction must be first"),
            "unexpected error: {err}"
        );

        let transactions = vec![
            finalize_withdrawal_transaction(10, Vec::new()),
            legacy_transaction(),
        ];
        let raw = raw_transactions(&transactions);

        let err = decode_zone_block_transactions(10, &transactions, &raw).unwrap_err();

        assert!(
            err.to_string()
                .contains("finalizeWithdrawalBatch system transaction must be last"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn preserves_user_signed_system_contract_transaction_as_user_transaction() {
        let transactions = vec![TempoTxEnvelope::Legacy(Signed::new_unhashed(
            TxLegacy {
                chain_id: None,
                nonce: 0,
                gas_price: 0,
                gas_limit: 0,
                to: TxKind::Call(ZONE_INBOX_ADDRESS),
                value: U256::ZERO,
                input: tempo_zone_contracts::ZoneInbox::advanceTempoCall {
                    header: Bytes::new(),
                    deposits: Vec::new(),
                    decryptions: Vec::new(),
                    enabledTokens: Vec::new(),
                }
                .abi_encode()
                .into(),
            },
            Signature::test_signature(),
        ))];
        let raw_transactions = raw_transactions(&transactions);

        let decoded = decode_zone_block_transactions(10, &transactions, &raw_transactions).unwrap();

        assert_eq!(decoded.tempo_import, ZoneTempoImport::none());
        assert_eq!(decoded.user_transactions, raw_transactions);
        assert_eq!(
            decoded.withdrawal_finalization,
            ZoneWithdrawalFinalization::none()
        );
    }

    #[test]
    fn rejects_finalize_withdrawal_block_number_mismatch() {
        let transactions = vec![finalize_withdrawal_transaction(11, Vec::new())];
        let raw_transactions = raw_transactions(&transactions);

        let err = decode_zone_block_transactions(10, &transactions, &raw_transactions).unwrap_err();

        assert!(
            err.to_string().contains("blockNumber mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_block_transaction_receipt_count_mismatch() {
        let header = sealed_header(10, B256::repeat_byte(0x09));
        let block = block_for_header(&header, vec![legacy_transaction()]);

        let err = test_validated_zone_block_data(&header, &block, &[]).unwrap_err();

        assert!(
            err.to_string()
                .contains("transaction/receipt count mismatch"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_block_header_mismatch() {
        let header = sealed_header(10, B256::repeat_byte(0x09));
        let mut block = block_for_header(&header, Vec::new());
        block.header.inner.timestamp = 12345;

        let err = test_validated_zone_block_data(&header, &block, &[]).unwrap_err();

        assert!(
            err.to_string().contains("header does not match"),
            "unexpected error: {err}"
        );
    }
}

/// Configuration for the sequencer background tasks
#[derive(Debug, Clone)]
pub struct ZoneSequencerAddOnsConfig {
    /// Sequencer private key signer for signing L1 transactions.
    pub sequencer_signer: PrivateKeySigner,
    /// Zone ID for chain ID validation.
    pub zone_id: u32,
    /// How often the zone monitor polls for new L2 blocks.
    pub zone_poll_interval: Duration,
    /// Maximum time to accumulate zone blocks before batch submission.
    pub batch_interval: Duration,
    /// EIP-2935 history and safety-margin limits used by the batch submitter.
    pub batch_anchor_config: BatchAnchorConfig,
    /// How often the withdrawal processor polls the L1 queue.
    pub withdrawal_poll_interval: Duration,
}

/// Configuration for the Zone private RPC server extension.
#[derive(Debug, Clone, Default)]
pub struct ZonePrivateRpcConfig {
    /// Port for RPC traffic.
    pub private_rpc_port: u16,
    /// Zone ID for chain ID validation and private RPC auth.
    pub zone_id: u32,
    /// Max duration for private RPC auth.
    pub max_auth_token_validity: Duration,
}

/// Tempo Zone node type configuration.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ZoneNode {
    /// Queue of L1 deposit messages to be included in the next zone block.
    deposit_queue: DepositQueue,
    /// Configuration for the L1 event subscriber (RPC endpoint, poll interval, etc.).
    l1_config: L1SubscriberConfig,
    /// Configuration for the L1 state provider (contract addresses, query parameters).
    l1_state_provider_config: L1StateProviderConfig,
    /// Shared L1 state cache (enabled tokens, zone metadata, etc.).
    l1_state_cache: L1StateCache,
    /// Shared TIP-403 policy cache, populated by the unified [`L1Subscriber`](zone_l1::L1Subscriber)
    /// and read by the precompile during block building.
    policy_cache: PolicyCache,
    /// Address of the L1 deposit portal contract.
    portal_address: Address,
    /// Optional pre-configured list of enabled token addresses. When set, the
    /// startup L1 RPC query for `enabledTokenCount`/`enabledTokens` is skipped.
    initial_tokens: Option<Vec<Address>>,
    /// Private RPC config.
    private_rpc_config: ZonePrivateRpcConfig,
    /// Optional sequencer config. When set, sequencer tasks are spawned.
    sequencer_config: Option<ZoneSequencerAddOnsConfig>,
}

impl ZoneNode {
    // Creates a new ZoneNode
    pub fn new(
        l1_rpc_url: String,
        portal_address: Address,
        genesis_tempo_block_number: Option<u64>,
        l1_fetch_concurrency: usize,
        retry_connection_interval: Duration,
    ) -> Self {
        let deposit_queue = DepositQueue::default();

        let policy_cache = PolicyCache::default();
        let l1_state_cache = L1StateCache::new(HashSet::from([portal_address]));
        let l1_config = L1SubscriberConfig {
            l1_rpc_url: l1_rpc_url.clone(),
            portal_address,
            genesis_tempo_block_number,
            policy_cache: policy_cache.clone(),
            l1_state_cache: l1_state_cache.clone(),
            l1_fetch_concurrency,
            retry_connection_interval,
        };

        let l1_state_provider_config = L1StateProviderConfig {
            l1_rpc_url,
            portal_address,
            retry_connection_interval,
            ..Default::default()
        };

        Self {
            deposit_queue,
            l1_config,
            l1_state_provider_config,
            l1_state_cache,
            policy_cache,
            portal_address,
            initial_tokens: None,
            private_rpc_config: ZonePrivateRpcConfig::default(),
            sequencer_config: None,
        }
    }

    /// Set the private RPC configuration.
    pub fn with_private_rpc(mut self, config: ZonePrivateRpcConfig) -> Self {
        self.private_rpc_config = config;
        self
    }

    /// Set the sequencer configuration. When set, batch submission and
    /// withdrawal processing tasks are spawned during node launch.
    pub fn with_sequencer(mut self, config: ZoneSequencerAddOnsConfig) -> Self {
        self.sequencer_config = Some(config);
        self
    }

    /// Set the initial list of enabled token addresses.
    /// When set, the startup L1 RPC query for enabled tokens is skipped.
    pub fn with_initial_tokens(mut self, tokens: Vec<Address>) -> Self {
        self.initial_tokens = Some(tokens);
        self
    }

    /// Returns the current deposit queue
    pub fn deposit_queue(&self) -> DepositQueue {
        self.deposit_queue.clone()
    }

    /// Returns the current l1 state cache
    pub fn l1_state_cache(&self) -> L1StateCache {
        self.l1_state_cache.clone()
    }

    /// Returns the current TIP-403 policy cache
    pub fn policy_cache(&self) -> PolicyCache {
        self.policy_cache.clone()
    }

    /// Returns a [`ComponentsBuilder`] configured for a Zone node.
    pub fn components<N>(
        executor_builder: ZoneExecutorBuilder,
    ) -> ComponentsBuilder<
        N,
        ZonePoolBuilder,
        BasicPayloadServiceBuilder<ZonePayloadFactory>,
        NoopNetworkBuilder<ZoneNetworkPrimitives>,
        ZoneExecutorBuilder,
        NoopConsensusBuilder,
    >
    where
        N: FullNodeTypes<Types = Self>,
    {
        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(ZonePoolBuilder)
            .executor(executor_builder)
            .payload(BasicPayloadServiceBuilder::new(
                ZonePayloadFactory::default(),
            ))
            .network(NoopNetworkBuilder::<ZoneNetworkPrimitives>::default())
            .noop_consensus()
    }
}

impl NodeTypes for ZoneNode {
    type Primitives = TempoPrimitives;
    type ChainSpec = TempoChainSpec;
    type Storage = EmptyBodyStorage<TempoTxEnvelope, TempoHeader>;
    type Payload = ZonePayloadTypes;
}

/// Addons for Tempo Zone nodes.
pub struct ZoneAddOns<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
    N::Pool: reth_transaction_pool::TransactionPool<Transaction = TempoPooledTransaction>,
{
    inner: RpcAddOns<
        N,
        TempoEthApiBuilder<N>,
        ZoneEngineValidatorBuilder,
        NoopEngineApiBuilder,
        BasicEngineValidatorBuilder<ZoneEngineValidatorBuilder>,
        Identity,
    >,
    /// Queue of L1 deposit messages to be included in the next zone block.
    deposit_queue: DepositQueue,
    /// Configuration for the L1 event subscriber
    l1_config: L1SubscriberConfig,
    /// TIP-403 policy cache
    policy_cache: PolicyCache,
    /// ZonePortal address on L1.
    portal_address: Address,
    /// Pre-configured list of initial tokens.
    initial_tokens: Option<Vec<Address>>,
    /// Private RPC configuration.
    private_rpc_config: ZonePrivateRpcConfig,
    /// Sequencer configuration.
    sequencer_config: Option<ZoneSequencerAddOnsConfig>,
}

impl<N> std::fmt::Debug for ZoneAddOns<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
    N::Pool: reth_transaction_pool::TransactionPool<Transaction = TempoPooledTransaction>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZoneAddOns").finish_non_exhaustive()
    }
}

impl<N> ZoneAddOns<NodeAdapter<N>>
where
    N: FullNodeTypes<Types = ZoneNode>,
    N::Provider: StateReader<Receipt = primitives::TempoReceipt>,
{
    /// Creates a new ZoneAddOns instance.
    pub fn new(
        deposit_queue: DepositQueue,
        l1_config: L1SubscriberConfig,
        policy_cache: PolicyCache,
        portal_address: Address,
        initial_tokens: Option<Vec<Address>>,
        private_rpc_config: ZonePrivateRpcConfig,
        sequencer_config: Option<ZoneSequencerAddOnsConfig>,
    ) -> Self {
        Self {
            inner: RpcAddOns::new(
                TempoEthApiBuilder::default(),
                ZoneEngineValidatorBuilder,
                NoopEngineApiBuilder::default(),
                BasicEngineValidatorBuilder::default(),
                Identity::default(),
                Default::default(),
            ),
            deposit_queue,
            l1_config,
            policy_cache,
            portal_address,
            initial_tokens,
            private_rpc_config,
            sequencer_config,
        }
    }
}

impl<N> NodeAddOns<N> for ZoneAddOns<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
    N::Pool: reth_transaction_pool::TransactionPool<
            Transaction = tempo_transaction_pool::transaction::TempoPooledTransaction,
        >,
    N::Provider: StateReader<Receipt = primitives::TempoReceipt>,
    TempoEthApiBuilder<N>: EthApiBuilder<N, EthApi: EthApiTypes<NetworkTypes = TempoNetwork>>,
{
    type Handle = <RpcAddOns<
        N,
        TempoEthApiBuilder<N>,
        ZoneEngineValidatorBuilder,
        NoopEngineApiBuilder,
        BasicEngineValidatorBuilder<ZoneEngineValidatorBuilder>,
        Identity,
    > as NodeAddOns<N>>::Handle;

    async fn launch_add_ons(mut self, ctx: AddOnsContext<'_, N>) -> eyre::Result<Self::Handle> {
        let sp = ctx.node.provider().latest()?;
        let tempo_block_number = sp.tempo_block_number()?;
        self.policy_cache.set_last_l1_block(tempo_block_number);
        info!(target: "reth::cli", tempo_block_number, "Read local tempoBlockNumber for L1 subscriber");

        let l1_provider = alloy_provider::ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_with_config(
                &self.l1_config.l1_rpc_url,
                rpc_connection_config(self.l1_config.retry_connection_interval),
            )
            .await?
            .erased();

        self.resolve_and_seed_tokens(&l1_provider).await?;
        self.spawn_l1_subscriber(&ctx);
        self.spawn_policy_tasks(&l1_provider, &ctx);

        if let Some(ref config) = self.sequencer_config {
            let sequencer_addr = config.sequencer_signer.address();
            let sequencer_key = SecretKey::from(config.sequencer_signer.credential());
            self.spawn_zone_engine(l1_provider, &ctx, sequencer_addr, sequencer_key)?;
        }

        let task_executor = ctx.node.task_executor().clone();
        let local_provider = ctx.node.provider().clone();

        let chain_id = ctx
            .node
            .provider()
            .chain_spec()
            .inner
            .genesis()
            .config
            .chain_id;
        let handle = self.inner.launch_add_ons(ctx).await?;

        Self::launch_private_rpc(
            self.private_rpc_config,
            &handle,
            self.l1_config.l1_rpc_url.clone(),
            self.l1_config.retry_connection_interval,
            self.l1_config.portal_address,
            chain_id,
        )
        .await?;

        if let Some(config) = self.sequencer_config.take() {
            let sequencer_addr = config.sequencer_signer.address();

            Self::launch_sequencer_tasks(
                config,
                &handle,
                &task_executor,
                self.l1_config.l1_rpc_url,
                self.l1_config.portal_address,
                self.l1_config.retry_connection_interval,
                sequencer_addr,
                chain_id,
                local_provider,
            )
            .await?;
        }

        Ok(handle)
    }
}

impl<N> ZoneAddOns<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
    N::Pool: reth_transaction_pool::TransactionPool<
            Transaction = tempo_transaction_pool::transaction::TempoPooledTransaction,
        >,
    N::Provider: StateReader<Receipt = primitives::TempoReceipt>,
    TempoEthApiBuilder<N>: EthApiBuilder<N, EthApi: EthApiTypes<NetworkTypes = TempoNetwork>>,
{
    /// Resolve enabled tokens and seed the policy cache.
    async fn resolve_and_seed_tokens(
        &mut self,
        l1_provider: &alloy_provider::DynProvider<TempoNetwork>,
    ) -> eyre::Result<()> {
        let portal = self.portal_address;
        let tracked_tokens = if let Some(tokens) = self.initial_tokens.take() {
            info!(target: "reth::cli", count = tokens.len(), ?tokens, "Using pre-configured initial tokens");
            tokens
        } else {
            let tokens = ZonePortal::new(portal, l1_provider)
                .enabled_tokens()
                .await?;
            info!(target: "reth::cli", count = tokens.len(), ?tokens, "Discovered enabled tokens from L1");
            tokens
        };

        self.policy_cache
            .seed_token_policies(portal, &tracked_tokens, l1_provider)
            .await?;
        info!(target: "reth::cli", "Seeded token policies from L1");
        Ok(())
    }

    /// Spawn the L1 subscriber. Listens for new blocks and deposit events.
    fn spawn_l1_subscriber(&mut self, ctx: &AddOnsContext<'_, N>) {
        L1Subscriber::spawn(
            self.l1_config.clone(),
            ctx.node.provider().clone(),
            self.deposit_queue.clone(),
            ctx.node.task_executor().clone(),
        );
        info!(target: "reth::cli", "Unified L1 subscriber started");
    }

    /// Spawn TIP-403 policy resolution and pool prefetch tasks.
    fn spawn_policy_tasks(
        &self,
        l1_provider: &alloy_provider::DynProvider<TempoNetwork>,
        ctx: &AddOnsContext<'_, N>,
    ) {
        let policy_task_handle = spawn_policy_resolution_task(
            self.policy_cache.clone(),
            l1_provider.clone(),
            16,
            256,
            ctx.node.task_executor().clone(),
        );
        spawn_pool_prefetch_task(
            ctx.node.pool().clone(),
            policy_task_handle,
            ctx.node.task_executor().clone(),
        );
        info!(target: "reth::cli", "TIP-403 policy prefetch tasks started");
    }

    /// Spawn the [`ZoneEngine`] for L1-event-driven block production.
    fn spawn_zone_engine(
        &self,
        l1_provider: alloy_provider::DynProvider<TempoNetwork>,
        ctx: &AddOnsContext<'_, N>,
        fee_recipient: Address,
        sequencer_key: SecretKey,
    ) -> eyre::Result<()> {
        let policy_provider = PolicyProvider::new(
            self.policy_cache.clone(),
            l1_provider,
            tokio::runtime::Handle::current(),
        );
        let provider = ctx.node.provider();
        let last_header = provider
            .sealed_header(provider.best_block_number()?)?
            .ok_or_else(|| eyre::eyre!("no latest block header"))?;
        let engine = ZoneEngine::new(
            provider.chain_spec(),
            ctx.beacon_engine_handle.clone(),
            ctx.node.payload_builder_handle().clone(),
            self.deposit_queue.clone(),
            last_header,
            fee_recipient,
            sequencer_key,
            self.portal_address,
            policy_provider,
        );
        ctx.node
            .task_executor()
            .spawn_critical_task("zone-engine", engine.run());
        info!(target: "reth::cli", "ZoneEngine spawned");
        Ok(())
    }

    /// Launch the private RPC server.
    async fn launch_private_rpc(
        config: ZonePrivateRpcConfig,
        handle: &<Self as NodeAddOns<N>>::Handle,
        l1_rpc_url: String,
        retry_connection_interval: Duration,
        portal_address: Address,
        chain_id: u64,
    ) -> eyre::Result<()> {
        if config.zone_id != 0 {
            let expected = zone_primitives::constants::zone_chain_id(config.zone_id);
            if chain_id != expected {
                eyre::bail!(
                    "chain ID mismatch: zone.id={} requires chain_id={}, but genesis has {}",
                    config.zone_id,
                    expected,
                    chain_id,
                );
            }
        }

        let eth_handlers = handle.eth_handlers().clone();
        let zone_rpc_url = handle
            .rpc_server_handles
            .rpc
            .http_url()
            .expect("HTTP RPC server must be enabled for private RPC");
        let private_rpc_config = zone_rpc::PrivateRpcConfig {
            listen_addr: ([0, 0, 0, 0], config.private_rpc_port).into(),
            l1_rpc_url,
            zone_rpc_url,
            retry_connection_interval,
            zone_id: config.zone_id,
            chain_id,
            max_auth_token_validity: config.max_auth_token_validity,
            zone_portal: portal_address,
        };
        let api: Arc<dyn ZoneRpcApi> =
            Arc::new(ZoneRpc::new(eth_handlers, private_rpc_config.clone()).await?);
        let local_addr = start_private_rpc(private_rpc_config, api).await?;
        info!(target: "reth::cli", %local_addr, "Private zone RPC server started");

        Ok(())
    }

    /// Launch sequencer background tasks: batch submission, withdrawal processing,
    /// and engine shutdown hook.
    async fn launch_sequencer_tasks(
        config: ZoneSequencerAddOnsConfig,
        handle: &<Self as NodeAddOns<N>>::Handle,
        task_executor: &reth_tasks::TaskExecutor,
        l1_rpc_url: String,
        portal_address: Address,
        retry_connection_interval: Duration,
        sequencer_addr: Address,
        chain_id: u64,
        local_provider: N::Provider,
    ) -> eyre::Result<()> {
        if config.zone_id != 0 {
            let expected = zone_primitives::constants::zone_chain_id(config.zone_id);
            if chain_id != expected {
                eyre::bail!(
                    "chain ID mismatch: zone.id={} requires chain_id={}, but genesis has {}",
                    config.zone_id,
                    expected,
                    chain_id,
                );
            }
        }

        let zone_rpc_url = handle
            .rpc_server_handles
            .rpc
            .http_url()
            .expect("HTTP RPC server must be enabled for sequencer mode");

        info!(target: "reth::cli", %sequencer_addr, "Starting sequencer background tasks");
        let l1_state_provider = L1StateProvider::new(
            L1StateProviderConfig {
                l1_rpc_url: l1_rpc_url.clone(),
                portal_address,
                retry_connection_interval,
                ..Default::default()
            },
            L1StateCache::new(HashSet::from([portal_address])),
            tokio::runtime::Handle::current(),
        )
        .await
        .wrap_err("failed to initialize prover witness L1 state provider")?;
        let prover_witness_source = LocalNodeProverWitnessSource::new(local_provider)
            .with_l1_state_provider(l1_state_provider);

        let sequencer_config = ZoneSequencerConfig {
            portal_address,
            l1_rpc_url,
            retry_connection_interval,
            withdrawal_poll_interval: config.withdrawal_poll_interval,
            outbox_address: ZONE_OUTBOX_ADDRESS,
            inbox_address: ZONE_INBOX_ADDRESS,
            tempo_state_address: TEMPO_STATE_ADDRESS,
            zone_rpc_url,
            zone_poll_interval: config.zone_poll_interval,
            batch_interval: config.batch_interval,
            batch_anchor_config: config.batch_anchor_config,
            prover_witness_source: Arc::new(prover_witness_source),
        };
        let seq_handle = spawn_zone_sequencer(sequencer_config, config.sequencer_signer).await;
        info!(target: "reth::cli", "Sequencer tasks spawned");

        // Critical task — node shuts down if either exits.
        task_executor.spawn_critical_task("zone-monitor", async move {
            tokio::select! {
                res = seq_handle.withdrawal_handle => {
                    tracing::error!(target: "reth::cli", ?res, "Withdrawal processor task exited");
                }
                res = seq_handle.monitor_handle => {
                    tracing::error!(target: "reth::cli", ?res, "Zone monitor task exited");
                }
            }
        });

        // Flush unpersisted blocks on shutdown.
        let engine_shutdown = handle.engine_shutdown.clone();
        task_executor.spawn_critical_with_graceful_shutdown_signal(
            "zone-engine-shutdown",
            |shutdown| async move {
                let _guard = shutdown.await;
                info!(target: "reth::cli", "Shutdown signal received — flushing engine state");
                if let Some(done) = engine_shutdown.shutdown() {
                    let _ = done.await;
                }
            },
        );

        Ok(())
    }
}

impl<N> RethRpcAddOns<N> for ZoneAddOns<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
    N::Pool: reth_transaction_pool::TransactionPool<
            Transaction = tempo_transaction_pool::transaction::TempoPooledTransaction,
        >,
    N::Provider: StateReader<Receipt = primitives::TempoReceipt>,
    TempoEthApiBuilder<N>:
        EthApiBuilder<N, EthApi: reth_rpc_eth_api::EthApiTypes<NetworkTypes = TempoNetwork>>,
{
    type EthApi = <TempoEthApiBuilder<N> as EthApiBuilder<N>>::EthApi;

    fn hooks_mut(&mut self) -> &mut reth_node_builder::rpc::RpcHooks<N, Self::EthApi> {
        self.inner.hooks_mut()
    }
}

impl<N> EngineValidatorAddOn<N> for ZoneAddOns<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
    N::Pool: reth_transaction_pool::TransactionPool<
            Transaction = tempo_transaction_pool::transaction::TempoPooledTransaction,
        >,
    TempoEthApiBuilder<N>: EthApiBuilder<N, EthApi: EthApiTypes<NetworkTypes = TempoNetwork>>,
{
    type ValidatorBuilder = BasicEngineValidatorBuilder<ZoneEngineValidatorBuilder>;

    fn engine_validator_builder(&self) -> Self::ValidatorBuilder {
        self.inner.engine_validator_builder()
    }
}

impl<N> Node<N> for ZoneNode
where
    N: FullNodeTypes<Types = Self>,
    N::Provider: StateReader<Receipt = primitives::TempoReceipt>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        ZonePoolBuilder,
        BasicPayloadServiceBuilder<ZonePayloadFactory>,
        NoopNetworkBuilder<ZoneNetworkPrimitives>,
        ZoneExecutorBuilder,
        NoopConsensusBuilder,
    >;
    type AddOns = ZoneAddOns<NodeAdapter<N>>;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        let executor_builder = ZoneExecutorBuilder::new(
            self.l1_state_provider_config.clone(),
            self.l1_state_cache.clone(),
            self.policy_cache.clone(),
        );
        Self::components(executor_builder)
    }

    fn add_ons(&self) -> Self::AddOns {
        ZoneAddOns::new(
            self.deposit_queue.clone(),
            self.l1_config.clone(),
            self.policy_cache.clone(),
            self.portal_address,
            self.initial_tokens.clone(),
            self.private_rpc_config.clone(),
            self.sequencer_config.clone(),
        )
    }
}

impl<N> DebugNode<N> for ZoneNode
where
    N: FullNodeComponents<Types = Self>,
    N::Provider: StateReader<Receipt = primitives::TempoReceipt>,
{
    type RpcBlock =
        alloy_rpc_types_eth::Block<alloy_rpc_types_eth::Transaction<TempoTxEnvelope>, TempoHeader>;

    fn rpc_to_primitive_block(rpc_block: Self::RpcBlock) -> primitives::Block {
        rpc_block
            .into_consensus_block()
            .map_transactions(|tx| tx.into_inner())
    }

    fn local_payload_attributes_builder(
        chain_spec: &Self::ChainSpec,
    ) -> impl PayloadAttributesBuilder<<Self::Payload as PayloadTypes>::PayloadAttributes, TempoHeader>
    {
        ZonePayloadAttributesBuilder::new(Arc::new(chain_spec.clone()))
    }
}

/// Builds [`ZonePayloadAttributes`] with `l1_block: None` — suitable for
/// debug/test scenarios where no L1 data is available.
#[derive(Debug)]
pub(crate) struct ZonePayloadAttributesBuilder;

impl ZonePayloadAttributesBuilder {
    pub(crate) fn new(_chain_spec: Arc<TempoChainSpec>) -> Self {
        Self
    }
}

impl PayloadAttributesBuilder<ZonePayloadAttributes, TempoHeader> for ZonePayloadAttributesBuilder {
    fn build(&self, _parent: &SealedHeader<TempoHeader>) -> ZonePayloadAttributes {
        unimplemented!("zone blocks require L1 data — use ZoneEngine instead")
    }
}

/// Builder that constructs the [`ZoneEvmConfig`] used during block execution.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ZoneExecutorBuilder {
    l1_state_provider_config: L1StateProviderConfig,
    l1_state_cache: L1StateCache,
    policy_cache: PolicyCache,
}

impl ZoneExecutorBuilder {
    /// Create a zone executor builder with the shared L1 state/policy caches.
    pub fn new(
        l1_state_provider_config: L1StateProviderConfig,
        l1_state_cache: L1StateCache,
        policy_cache: PolicyCache,
    ) -> Self {
        Self {
            l1_state_provider_config,
            l1_state_cache,
            policy_cache,
        }
    }
}

impl<Node> ExecutorBuilder<Node> for ZoneExecutorBuilder
where
    Node: FullNodeTypes<Types = ZoneNode>,
{
    type EVM = ZoneEvmConfig;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        let runtime_handle = tokio::runtime::Handle::current();
        let l1_provider = L1StateProvider::new(
            self.l1_state_provider_config.clone(),
            self.l1_state_cache,
            runtime_handle.clone(),
        )
        .await?;

        let mut evm_config = ZoneEvmConfig::new(ctx.chain_spec(), l1_provider);

        // Create PolicyProvider for the TIP-403 proxy precompile.
        let policy_l1 = alloy_provider::ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_with_config(
                &self.l1_state_provider_config.l1_rpc_url,
                rpc_connection_config(self.l1_state_provider_config.retry_connection_interval),
            )
            .await?
            .erased();

        let policy_provider = PolicyProvider::new(self.policy_cache, policy_l1, runtime_handle);
        evm_config = evm_config.with_policy_provider(policy_provider);
        info!(target: "reth::cli", "Zone EVM initialized with TempoStateReader + TIP-403 proxy precompiles");

        Ok(evm_config)
    }
}

/// Engine validator builder for Zone.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct ZoneEngineValidatorBuilder;

impl<Node> PayloadValidatorBuilder<Node> for ZoneEngineValidatorBuilder
where
    Node: FullNodeComponents<Types = ZoneNode>,
{
    type Validator = TempoEngineValidator;

    async fn build(self, _ctx: &AddOnsContext<'_, Node>) -> eyre::Result<Self::Validator> {
        Ok(TempoEngineValidator::new())
    }
}

/// Transaction pool builder for Zone - uses Tempo pool with defaults.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct ZonePoolBuilder;

impl<Node> PoolBuilder<Node, ZoneEvmConfig> for ZonePoolBuilder
where
    Node: FullNodeTypes<Types = ZoneNode>,
{
    type Pool = TempoTransactionPool<Node::Provider>;

    async fn build_pool(
        self,
        ctx: &BuilderContext<Node>,
        _evm_config: ZoneEvmConfig,
    ) -> eyre::Result<Self::Pool> {
        let mut pool_config = ctx.pool_config();
        pool_config.max_inflight_delegated_slot_limit = pool_config.max_account_slots;

        // this store is effectively a noop
        let blob_store = InMemoryBlobStore::default();
        let tempo_evm_config = TempoEvmConfig::new(ctx.chain_spec());
        let additional_tasks = ctx.config().txpool.additional_validation_tasks;
        let task_executor = ctx.task_executor().clone();
        let mut validator = TransactionValidationTaskExecutor::eth_builder(
            ctx.provider().clone(),
            tempo_evm_config,
        )
        .with_max_tx_input_bytes(ctx.config().txpool.max_tx_input_bytes)
        .with_local_transactions_config(pool_config.local_transactions_config.clone())
        .set_tx_fee_cap(ctx.config().rpc.rpc_tx_fee_cap)
        .with_max_tx_gas_limit(ctx.config().txpool.max_tx_gas_limit)
        .set_block_gas_limit(ctx.chain_spec().inner.genesis().gas_limit)
        .disable_balance_check()
        .with_minimum_priority_fee(ctx.config().txpool.minimum_priority_fee)
        .with_custom_tx_type(TempoTxType::AA as u8)
        .no_eip4844()
        .build::<TempoPooledTransaction, _>(blob_store.clone());

        validator.set_additional_stateless_validation(|_origin, tx| {
            use alloy_consensus::Transaction;
            if tx.is_create() {
                return Err(InvalidPoolTransactionError::Consensus(
                    InvalidTransactionError::TxTypeNotSupported,
                ));
            }
            Ok(())
        });

        let validator =
            TransactionValidationTaskExecutor::spawn(validator, &task_executor, additional_tasks);

        let aa_2d_config = AA2dPoolConfig {
            price_bump_config: pool_config.price_bumps,
            pending_limit: pool_config.pending_limit,
            queued_limit: pool_config.queued_limit,
            max_txs_per_sender: pool_config.max_account_slots,
        };
        let aa_2d_pool = AA2dPool::new(aa_2d_config);
        let amm_liquidity_cache = AmmLiquidityCache::new(ctx.provider())?;

        let validator = validator.map(|v| {
            TempoTransactionValidator::new(
                v,
                DEFAULT_AA_VALID_AFTER_MAX_SECS,
                DEFAULT_MAX_TEMPO_AUTHORIZATIONS,
                amm_liquidity_cache.clone(),
            )
        });
        let protocol_pool = Pool::new(
            validator,
            TempoTipOrdering::default(),
            blob_store,
            pool_config.clone(),
        );

        let transaction_pool = TempoTransactionPool::new(protocol_pool, aa_2d_pool);

        spawn_maintenance_tasks(ctx, transaction_pool.clone(), &pool_config)?;

        // Spawn unified Tempo pool maintenance task
        // This consolidates: expired AA txs, 2D nonce updates, AMM cache, and keychain revocations
        ctx.task_executor().spawn_critical_task(
            "txpool maintenance - tempo pool",
            tempo_transaction_pool::maintain::maintain_tempo_pool(transaction_pool.clone()),
        );

        info!(target: "reth::cli", "Transaction pool initialized");
        debug!(target: "reth::cli", "Spawned txpool maintenance task");

        Ok(transaction_pool)
    }
}
