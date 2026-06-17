//! Pool transaction prefetch task for TIP-403 policy cache warming.
//!
//! Subscribes to new pending transactions from the transaction pool and
//! extracts sender/recipient addresses from TIP-20 transfer calls. For each
//! address, a [`ResolveAuthorization`](super::PolicyTaskMessage::ResolveAuthorization)
//! request is sent to the [`PolicyResolutionTask`](super::PolicyResolutionTask)
//! via the [`PolicyTaskHandle`](super::PolicyTaskHandle), warming the policy cache
//! before block building.

use std::collections::HashSet;

use alloy_primitives::{Address, TxKind};
use alloy_sol_types::SolCall;
use reth_transaction_pool::TransactionPool;
use tempo_contracts::precompiles::{DEFAULT_FEE_TOKEN, ITIP20};
use tempo_precompiles::tip20::is_tip20_prefix;
use tempo_revm::TempoTx;
use tempo_transaction_pool::transaction::TempoPooledTransaction;
use tracing::debug;

use super::{AuthRole, task::PolicyTaskHandle};

/// Spawns a background task that warms TIP-403 policy cache entries
/// for transactions entering the pool.
///
/// For each new transaction, this prefetches authorization for:
/// - the fee payer against the fee token;
/// - TIP-20 transfer senders;
/// - TIP-20 transfer recipients.
///
/// This is only an optimization. If the task exits or misses a transaction,
/// block building still resolves policy data synchronously on cache misses.
pub fn spawn_pool_prefetch_task<Pool>(
    pool: Pool,
    handle: PolicyTaskHandle,
    task_executor: reth_tasks::Runtime,
) where
    Pool: TransactionPool<Transaction = TempoPooledTransaction> + 'static,
{
    task_executor.spawn_task(Box::pin(async move {
        run_pool_prefetch(pool, handle).await;
    }));
}

async fn run_pool_prefetch<Pool>(pool: Pool, handle: PolicyTaskHandle)
where
    Pool: TransactionPool<Transaction = TempoPooledTransaction>,
{
    let mut new_txs = pool.new_transactions_listener();

    while let Some(tx_event) = new_txs.recv().await {
        let tx = &tx_event.transaction;
        let inner = tx.transaction.inner();
        let sender = tx.sender();

        let mut prefetched = HashSet::new();

        let fee_token = inner.fee_token().unwrap_or(DEFAULT_FEE_TOKEN);
        let fee_payer = tx.transaction.inner().fee_payer(sender).unwrap_or(sender);

        prefetch_policy(
            &handle,
            &mut prefetched,
            fee_token,
            fee_payer,
            AuthRole::Sender,
        );

        for (kind, input) in inner.calls() {
            let TxKind::Call(token) = kind else {
                continue;
            };

            if !is_tip20_prefix(token) {
                continue;
            }

            let Some((transfer_sender, recipient)) = decode_tip20_transfer(input, sender) else {
                continue;
            };

            prefetch_policy(
                &handle,
                &mut prefetched,
                token,
                transfer_sender,
                AuthRole::Sender,
            );

            prefetch_policy(
                &handle,
                &mut prefetched,
                token,
                recipient,
                AuthRole::Recipient,
            );
        }
    }

    debug!("Pool prefetch task shutting down");
}

#[inline]
fn prefetch_policy(
    handle: &PolicyTaskHandle,
    prefetched: &mut HashSet<(Address, Address, AuthRole)>,
    token: Address,
    account: Address,
    role: AuthRole,
) {
    if !prefetched.insert((token, account, role)) {
        return;
    }

    debug!(
        %token,
        %account,
        ?role,
        "Pre-fetching TIP-403 authorization"
    );

    // NOTE: handle this error?
    let _ = handle.send_resolve_policy(token, account, role);
}

#[inline]
fn decode_tip20_transfer(input: &[u8], sender: Address) -> Option<(Address, Address)> {
    let selector = input.first_chunk::<4>()?;
    let args = &input[4..];

    if *selector == ITIP20::transferCall::SELECTOR {
        let call = ITIP20::transferCall::abi_decode_raw(args).ok()?;
        Some((sender, call.to))
    } else if *selector == ITIP20::transferWithMemoCall::SELECTOR {
        let call = ITIP20::transferWithMemoCall::abi_decode_raw(args).ok()?;
        Some((sender, call.to))
    } else if *selector == ITIP20::transferFromCall::SELECTOR {
        let call = ITIP20::transferFromCall::abi_decode_raw(args).ok()?;
        Some((call.from, call.to))
    } else if *selector == ITIP20::transferFromWithMemoCall::SELECTOR {
        let call = ITIP20::transferFromWithMemoCall::abi_decode_raw(args).ok()?;
        Some((call.from, call.to))
    } else {
        None
    }
}
