//! Zone transaction pool construction and admission policy.

use crate::node::ZoneNode;
use alloy_primitives::TxKind;
use alloy_sol_types::SolCall;
use reth_chainspec::EthChainSpec;
use reth_node_api::FullNodeTypes;
use reth_node_builder::{
    BuilderContext,
    components::{PoolBuilder, spawn_maintenance_tasks},
};
use reth_transaction_pool::{
    Pool, PoolTransaction, StatelessValidationFn, TransactionOrigin,
    TransactionValidationTaskExecutor, blobstore::InMemoryBlobStore,
    error::InvalidPoolTransactionError,
};
use std::sync::Arc;
use tempo_contracts::precompiles::ITIP20;
use tempo_node::DEFAULT_AA_VALID_AFTER_MAX_SECS;
use tempo_primitives::{TempoTxType, is_tip20_prefix};
use tempo_transaction_pool::{
    AA2dPool, AA2dPoolConfig, TempoTransactionPool,
    amm::AmmLiquidityCache,
    ordering::TempoTipOrdering,
    transaction::{TempoPoolTransactionError, TempoPooledTransaction},
    validator::{DEFAULT_MAX_TEMPO_AUTHORIZATIONS, TempoTransactionValidator},
};
use tempo_zone_contracts::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS, ZoneInbox, ZoneOutbox};
use tracing::{debug, info};
use zone_evm::ZoneEvmConfig;

/// Transaction pool builder for Zone - uses Tempo pool with defaults.
#[derive(Default, Clone)]
#[non_exhaustive]
pub struct ZonePoolBuilder {
    additional_stateless_validation: Option<StatelessValidationFn<TempoPooledTransaction>>,
}

impl std::fmt::Debug for ZonePoolBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZonePoolBuilder")
            .field(
                "additional_stateless_validation",
                &self.additional_stateless_validation.as_ref().map(|_| "..."),
            )
            .finish()
    }
}

impl ZonePoolBuilder {
    /// Sets an additional stateless validation check for Zone pool admission.
    pub fn with_additional_stateless_validation<F>(mut self, f: F) -> Self
    where
        F: Fn(
                TransactionOrigin,
                &TempoPooledTransaction,
            ) -> Result<(), InvalidPoolTransactionError>
            + Send
            + Sync
            + 'static,
    {
        self.additional_stateless_validation = Some(Arc::new(f));
        self
    }
}

impl<Node> PoolBuilder<Node, ZoneEvmConfig> for ZonePoolBuilder
where
    Node: FullNodeTypes<Types = ZoneNode>,
{
    type Pool = TempoTransactionPool<Node::Provider, ZoneEvmConfig>;

    async fn build_pool(
        self,
        ctx: &BuilderContext<Node>,
        evm_config: ZoneEvmConfig,
    ) -> eyre::Result<Self::Pool> {
        // Zone blocks have no protocol base fee, so allow zero-fee transactions into the pool.
        let mut pool_config = ctx.pool_config().with_disabled_protocol_base_fee();
        pool_config.max_inflight_delegated_slot_limit = pool_config.max_account_slots;

        // this store is effectively a noop
        let blob_store = InMemoryBlobStore::default();
        let additional_tasks = ctx.config().txpool.additional_validation_tasks;
        let task_executor = ctx.task_executor().clone();
        let validator =
            TransactionValidationTaskExecutor::eth_builder(ctx.provider().clone(), evm_config)
                .with_max_tx_input_bytes(ctx.config().txpool.max_tx_input_bytes)
                .with_local_transactions_config(pool_config.local_transactions_config.clone())
                .set_tx_fee_cap(ctx.config().rpc.rpc_tx_fee_cap)
                .with_max_tx_gas_limit(ctx.config().txpool.max_tx_gas_limit)
                .set_block_gas_limit(ctx.chain_spec().genesis().gas_limit)
                .disable_balance_check()
                .with_minimum_priority_fee(ctx.config().txpool.minimum_priority_fee)
                .with_custom_tx_type(TempoTxType::AA as u8)
                .no_eip4844()
                .build::<TempoPooledTransaction, _>(blob_store.clone());

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

        let additional_stateless_validation = self.additional_stateless_validation;
        let validator = validator.map(move |mut v| {
            v.set_additional_stateless_validation_fn_opt(additional_stateless_validation.clone());
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

/// Additional stateless validation hook for Zone transaction-pool admission.
///
/// # Scope
///
/// This policy runs only when the pool admits a transaction. A transaction whose recovered
/// `PoolTransaction::sender()` is in `CONTRACT_DEPLOYER_ALLOWLIST` bypasses the hook entirely. For
/// every other sender, the hook does not affect consensus validation, EVM execution, payload
/// conversion, or the validity of transactions in imported blocks. `TransactionOrigin` is
/// intentionally ignored, so origin metadata cannot grant or bypass the sender allowlist. Legitimate
/// Zone system transactions are synthesized and handled outside pool admission. The production
/// allowlist is currently empty, so no transaction takes the bypass until a protocol deployer is
/// configured.
///
/// # Policy
///
/// 1. Return successfully for a sender in
///    `zone_primitives::constants::CONTRACT_DEPLOYER_ALLOWLIST`, before any other validation.
/// 2. For a non-allowlisted sender, delegate base call validation to
///    `zone_evm::validate_transaction` with
///    `zone_primitives::constants::CONTRACT_DEPLOYER_ALLOWLIST`.
/// 3. Inspect `tx.calls()`, which yields the single direct call or every call in an AA batch.
/// 4. Unconditionally reject `advanceTempo` calls to `ZoneInbox` and `finalizeWithdrawalBatch`
///    calls to `ZoneOutbox` from pool admission.
/// 5. After base validation, apply no further call-level restriction to creates or other
///    non-TIP-20 targets.
/// 6. For TIP-20-prefixed targets, allow only fully ABI-decodable `ITIP20::transferFromCall` and
///    `ITIP20::approveCall` inputs; reject malformed inputs and every other operation.
///
/// # Errors
///
/// Allowlisted senders cannot fail this hook. Every failure for a non-allowlisted sender is returned
/// as `InvalidPoolTransactionError` backed by `TempoPoolTransactionError::Evm`, so the pool treats
/// it as a deterministic bad transaction.
pub(crate) fn validate_zone_pool_transaction(
    _origin: TransactionOrigin,
    tx: &TempoPooledTransaction,
) -> Result<(), InvalidPoolTransactionError> {
    let allowlist = zone_primitives::constants::CONTRACT_DEPLOYER_ALLOWLIST;
    if allowlist.contains(&tx.sender()) {
        return Ok(());
    }

    let tx = tx.tx_env();
    zone_evm::validate_transaction(tx, allowlist)
        .map_err(|err| InvalidPoolTransactionError::other(TempoPoolTransactionError::Evm(err)))?;

    let validation_error = |message: &'static str| {
        InvalidPoolTransactionError::other(TempoPoolTransactionError::Evm(message.into()))
    };

    for (target, input) in tx.calls() {
        let target = *target;
        let is_zone_system_operation = match (target, input.get(..4)) {
            (TxKind::Call(ZONE_INBOX_ADDRESS), Some(selector)) => {
                selector == ZoneInbox::advanceTempoCall::SELECTOR
            }
            (TxKind::Call(ZONE_OUTBOX_ADDRESS), Some(selector)) => {
                selector == ZoneOutbox::finalizeWithdrawalBatchCall::SELECTOR
            }
            _ => false,
        };

        if is_zone_system_operation {
            return Err(validation_error(
                "zone system operations require a system transaction",
            ));
        }

        let TxKind::Call(address) = target else {
            continue;
        };
        if !is_tip20_prefix(address) {
            continue;
        }

        if input.starts_with(&ITIP20::transferFromCall::SELECTOR) {
            ITIP20::transferFromCall::abi_decode(input)
                .map_err(|_| validation_error("malformed TIP-20 transferFrom call"))?;
        } else if input.starts_with(&ITIP20::approveCall::SELECTOR) {
            ITIP20::approveCall::abi_decode(input)
                .map_err(|_| validation_error("malformed TIP-20 approve call"))?;
        } else {
            return Err(validation_error("TIP-20 operation is not allowed on zones"));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{Signed, TxEip1559};
    use alloy_primitives::{Address, Bytes, Signature, U256, address};
    use reth_primitives_traits::Recovered;
    use tempo_primitives::{
        TempoTxEnvelope,
        transaction::{AASigned, Call, PrimitiveSignature, TempoSignature, TempoTransaction},
    };

    const TOKEN: Address = address!("0x20C0000000000000000000000000000000000001");

    fn pooled_transaction(envelope: TempoTxEnvelope, sender: Address) -> TempoPooledTransaction {
        TempoPooledTransaction::new(Recovered::new_unchecked(envelope, sender))
    }

    fn aa_transaction(sender: Address, calls: Vec<Call>) -> TempoPooledTransaction {
        let transaction = TempoTransaction {
            calls,
            ..Default::default()
        };
        let signature =
            TempoSignature::Primitive(PrimitiveSignature::Secp256k1(Signature::test_signature()));
        pooled_transaction(
            AASigned::new_unhashed(transaction, signature).into(),
            sender,
        )
    }

    fn call_transaction(sender: Address, target: Address, input: Bytes) -> TempoPooledTransaction {
        pooled_transaction(
            TempoTxEnvelope::Eip1559(Signed::new_unhashed(
                TxEip1559 {
                    to: TxKind::Call(target),
                    input,
                    ..Default::default()
                },
                Signature::test_signature(),
            )),
            sender,
        )
    }

    #[test]
    fn pool_policy_rejects_create_in_non_first_aa_call() {
        let transaction = aa_transaction(
            Address::repeat_byte(0x11),
            vec![
                Call {
                    to: TxKind::Call(Address::repeat_byte(0x22)),
                    value: U256::ZERO,
                    input: Bytes::new(),
                },
                Call {
                    to: TxKind::Create,
                    value: U256::ZERO,
                    input: Bytes::new(),
                },
            ],
        );

        assert!(validate_zone_pool_transaction(TransactionOrigin::External, &transaction).is_err());
    }

    #[test]
    fn pool_policy_restricts_tip20_operations() {
        let sender = Address::repeat_byte(0x11);
        let transfer_from = ITIP20::transferFromCall {
            from: sender,
            to: Address::repeat_byte(0x22),
            amount: U256::from(7),
        };
        let approve = ITIP20::approveCall {
            spender: Address::repeat_byte(0x33),
            amount: U256::from(9),
        };

        for input in [transfer_from.abi_encode(), approve.abi_encode()] {
            let transaction = call_transaction(sender, TOKEN, input.into());
            assert!(
                validate_zone_pool_transaction(TransactionOrigin::External, &transaction).is_ok()
            );
        }

        let transfer = ITIP20::transferCall {
            to: Address::repeat_byte(0x22),
            amount: U256::from(7),
        };
        let transaction = call_transaction(sender, TOKEN, transfer.abi_encode().into());
        let error =
            validate_zone_pool_transaction(TransactionOrigin::External, &transaction).unwrap_err();
        assert_eq!(
            error.to_string(),
            "TIP-20 operation is not allowed on zones"
        );

        let transaction =
            call_transaction(sender, TOKEN, ITIP20::approveCall::SELECTOR.to_vec().into());
        let error =
            validate_zone_pool_transaction(TransactionOrigin::External, &transaction).unwrap_err();
        assert_eq!(error.to_string(), "malformed TIP-20 approve call");
    }

    #[test]
    fn pool_policy_validates_every_tip20_call_in_aa_batch() {
        let sender = Address::repeat_byte(0x11);
        let transaction = aa_transaction(
            sender,
            vec![
                Call {
                    to: TxKind::Call(TOKEN),
                    value: U256::ZERO,
                    input: ITIP20::approveCall {
                        spender: Address::repeat_byte(0x33),
                        amount: U256::from(9),
                    }
                    .abi_encode()
                    .into(),
                },
                Call {
                    to: TxKind::Call(TOKEN),
                    value: U256::ZERO,
                    input: ITIP20::mintCall {
                        to: Address::repeat_byte(0x44),
                        amount: U256::from(1),
                    }
                    .abi_encode()
                    .into(),
                },
            ],
        );

        let error =
            validate_zone_pool_transaction(TransactionOrigin::External, &transaction).unwrap_err();
        assert_eq!(
            error.to_string(),
            "TIP-20 operation is not allowed on zones"
        );
    }

    #[test]
    fn pool_policy_preserves_non_tip20_calls_and_rejects_system_operations() {
        let sender = Address::repeat_byte(0x11);
        let non_tip20 = call_transaction(sender, Address::repeat_byte(0x44), Bytes::new());
        for origin in [
            TransactionOrigin::Local,
            TransactionOrigin::External,
            TransactionOrigin::Private,
        ] {
            assert!(validate_zone_pool_transaction(origin, &non_tip20).is_ok());
        }

        for (target, selector) in [
            (ZONE_INBOX_ADDRESS, ZoneInbox::advanceTempoCall::SELECTOR),
            (
                ZONE_OUTBOX_ADDRESS,
                ZoneOutbox::finalizeWithdrawalBatchCall::SELECTOR,
            ),
        ] {
            let system_operation = call_transaction(sender, target, selector.to_vec().into());
            for origin in [
                TransactionOrigin::Local,
                TransactionOrigin::External,
                TransactionOrigin::Private,
            ] {
                let error = validate_zone_pool_transaction(origin, &system_operation).unwrap_err();
                assert_eq!(
                    error.to_string(),
                    "zone system operations require a system transaction"
                );
            }
        }
    }
}
