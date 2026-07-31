//! Shared harness for the `it` integration-test suite.

mod accounts;
mod fixture;
mod l1;
mod node;
mod p2p;
mod private_rpc;

pub(crate) use accounts::*;
pub(crate) use fixture::*;
pub(crate) use l1::*;
pub(crate) use node::*;
pub(crate) use p2p::*;
pub(crate) use private_rpc::*;

use alloy_primitives::{Address, address};
use alloy_signer_local::{MnemonicBuilder, coins_bip39::English};
use eyre::WrapErr;
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "../../../../rpc/test-utils/auth_tokens.rs"]
mod auth_tokens;

pub(crate) use auth_tokens::{
    build_signed_token_blob, now_secs, sign_keychain_signature, sign_p256_signature,
    sign_webauthn_signature,
};

/// Atomic counter for unique chain IDs across concurrent tests.
static NEXT_CHAIN_ID: AtomicU64 = AtomicU64::new(71_000);

fn next_unique_chain_id() -> u64 {
    NEXT_CHAIN_ID.fetch_add(1, Ordering::Relaxed)
}

fn l1_dev_signer() -> alloy_signer_local::PrivateKeySigner {
    MnemonicBuilder::<English>::default()
        .phrase(TEST_MNEMONIC)
        .build()
        .expect("valid test mnemonic")
}

/// Default timeout for polling loops in e2e tests.
pub(crate) const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Default poll interval for e2e tests.
pub(crate) const DEFAULT_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// Gas limit for ordinary TIP-20 calls under the current Tempo fork schedule.
pub(crate) const TIP20_TX_GAS: u64 = 500_000;

/// Gas limit for `ZoneOutbox.requestWithdrawal` test transactions.
///
/// The current Tempo fork schedule needs enough headroom for `transferFrom`, the subsequent
/// `burn`, and storage writes for the encrypted callback payloads exercised by router-based
/// withdrawals.
pub(crate) const WITHDRAWAL_TX_GAS: u64 = 10_000_000;

/// Local test nodes finalize a withdrawal batch every this many zone blocks.
pub(crate) const WITHDRAWAL_BATCH_INTERVAL_BLOCKS: u64 = 8;

/// Timeout for operations against a real in-process L1 — its dev node produces
/// blocks every 500ms and the L1Subscriber needs to connect, backfill, and
/// subscribe.
pub(crate) const L1_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Timeout for a withdrawal to be batched, submitted, and processed on L1.
pub(crate) const WITHDRAWAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub(crate) const TEST_MNEMONIC: &str =
    "test test test test test test test test test test test junk";

pub(crate) const STABLECOIN_DEX_ADDRESS: Address =
    address!("0xDEc0000000000000000000000000000000000000");

/// Poll an async condition until it returns `Some(T)` or the timeout expires.
pub(crate) async fn poll_until<T, Fut, F>(
    timeout: std::time::Duration,
    interval: std::time::Duration,
    description: &str,
    mut f: F,
) -> eyre::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = eyre::Result<Option<T>>>,
{
    let start = std::time::Instant::now();
    loop {
        if let Some(v) = f().await.wrap_err("poll iteration failed")? {
            return Ok(v);
        }
        if start.elapsed() > timeout {
            eyre::bail!("timed out after {timeout:?}: {description}");
        }
        tokio::time::sleep(interval).await;
    }
}
