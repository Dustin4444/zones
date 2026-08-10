//! Closes a ZonePortal's account access and revokes every discovered account role.
//!
//! ZonePortal stores roles in a mapping, so account membership cannot be enumerated from contract
//! state.  This command rebuilds the candidate set from `RoleUpdated` events (including the events
//! emitted during portal initialization), then reads each candidate's current role before revoking
//! only `Account` roles.

use std::collections::BTreeSet;

use alloy::{
    network::{EthereumWallet, ReceiptResponse as _},
    primitives::Address,
    providers::{Provider, ProviderBuilder},
    rpc::types::Filter,
    signers::local::PrivateKeySigner,
    sol_types::SolEvent,
};
use eyre::{WrapErr as _, ensure};
use futures::{StreamExt as _, TryStreamExt as _};
use tempo_alloy::TempoNetwork;
use tempo_zone_contracts::{ZonePortal, ZonePortal::Role as PortalRole};

/// Close account access and clear the portal account allowlist during an incident.
#[derive(Debug, clap::Parser)]
pub(crate) struct LockDownAccess {
    /// Tempo L1 HTTP RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc_url: String,

    /// ZonePortal contract address on Tempo L1.
    #[arg(long, env = "L1_PORTAL_ADDRESS")]
    portal: Address,

    /// Portal admin private key (hex) used to sign the transactions.
    #[arg(long, env = "PRIVATE_KEY", hide_env_values = true)]
    private_key: String,

    /// First L1 block to replay for `RoleUpdated` events. This must be at or before portal
    /// initialization, otherwise pre-existing allowed accounts cannot be discovered.
    #[arg(long, default_value_t = 0)]
    from_block: u64,

    /// Maximum simultaneous RPC requests for role reads, transaction submission, and receipt
    /// polling. Transactions are assigned explicit sequential nonces before submission.
    #[arg(long, default_value_t = 16)]
    max_concurrent_requests: usize,
}

impl LockDownAccess {
    pub(crate) async fn run(self) -> eyre::Result<()> {
        ensure!(
            self.max_concurrent_requests > 0,
            "--max-concurrent-requests must be greater than zero"
        );
        let signer: PrivateKeySigner = self
            .private_key
            .strip_prefix("0x")
            .unwrap_or(&self.private_key)
            .parse()
            .wrap_err("PRIVATE_KEY is not a valid private key")?;
        let admin = signer.address();
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .wallet(EthereumWallet::from(signer))
            .connect(&self.l1_rpc_url)
            .await
            .wrap_err("failed connecting to Tempo L1 RPC")?;
        let portal = ZonePortal::new(self.portal, &provider);

        let onchain_admin = portal
            .admin()
            .call()
            .await
            .wrap_err("failed querying ZonePortal admin")?;
        ensure!(
            admin == onchain_admin,
            "PRIVATE_KEY resolves to {admin}, but portal {} is administered by {onchain_admin}",
            self.portal
        );

        // Close the portal before taking the replay snapshot. This prevents an open portal from
        // admitting accounts while the mapping backfill and revocations are in progress.
        let snapshot_block = if portal
            .isAccessEnforced()
            .call()
            .await
            .wrap_err("failed querying ZonePortal access mode")?
        {
            provider
                .get_block_number()
                .await
                .wrap_err("failed querying Tempo L1 block number")?
        } else {
            println!(
                "Closing account access enforcement on portal {}...",
                self.portal
            );
            let receipt = portal
                .setAccessMode(true)
                .send()
                .await
                .wrap_err("failed sending ZonePortal.setAccessMode(true)")?
                .get_receipt()
                .await
                .wrap_err("failed waiting for ZonePortal.setAccessMode(true) receipt")?;
            ensure!(receipt.status(), "ZonePortal.setAccessMode(true) reverted");
            receipt.block_number.ok_or_else(|| {
                eyre::eyre!("setAccessMode receipt did not include a block number")
            })?
        };

        println!(
            "Replaying RoleUpdated events from block {} through {}...",
            self.from_block, snapshot_block
        );
        let filter = Filter::new()
            .address(self.portal)
            .event_signature(ZonePortal::RoleUpdated::SIGNATURE_HASH)
            .from_block(self.from_block)
            .to_block(snapshot_block);
        let logs = provider
            .get_logs(&filter)
            .await
            .wrap_err("failed fetching ZonePortal RoleUpdated logs")?;

        let mut candidates = BTreeSet::new();
        for log in logs {
            let event = ZonePortal::RoleUpdated::decode_log(&log.inner)
                .wrap_err("failed decoding ZonePortal RoleUpdated log")?;
            candidates.insert(event.account);
        }

        let accounts = futures::stream::iter(candidates)
            .map(|account| {
                let portal = portal.clone();
                async move {
                    let role = portal
                        .role(account)
                        .call()
                        .await
                        .wrap_err_with(|| format!("failed querying role for {account}"))?;
                    Ok::<_, eyre::Report>((account, role))
                }
            })
            .buffer_unordered(self.max_concurrent_requests)
            .try_filter_map(|(account, role)| async move {
                Ok((role == PortalRole::Account).then_some(account))
            })
            .try_collect::<Vec<_>>()
            .await?;

        println!("Revoking {} allowed account(s)...", accounts.len());
        let account_count = accounts.len();
        let next_nonce = provider
            .get_transaction_count(admin)
            .await
            .wrap_err("failed querying portal admin transaction count")?;
        let pending = futures::stream::iter(accounts.iter().copied().enumerate())
            .map(|(index, account)| {
                let portal = portal.clone();
                async move {
                    let pending = portal
                        .setAllowedAccount(account, false)
                        .nonce(next_nonce + index as u64)
                        .send()
                        .await
                        .wrap_err_with(|| {
                            format!("failed submitting revoke transaction for {account}")
                        })?;
                    Ok::<_, eyre::Report>((index, account, pending))
                }
            })
            .buffer_unordered(self.max_concurrent_requests)
            .try_collect::<Vec<_>>()
            .await?;

        futures::stream::iter(pending)
            .map(|(index, account, pending)| async move {
                let receipt = pending
                    .get_receipt()
                    .await
                    .wrap_err_with(|| format!("failed waiting for revoke receipt for {account}"))?;
                ensure!(
                    receipt.status(),
                    "ZonePortal.setAllowedAccount({account}, false) reverted"
                );
                Ok::<_, eyre::Report>((index, account))
            })
            .buffer_unordered(self.max_concurrent_requests)
            .try_for_each(|(index, account)| async move {
                println!("  [{}/{}] revoked {account}", index + 1, account_count);
                Ok(())
            })
            .await?;

        ensure!(
            portal.isAccessEnforced().call().await?,
            "ZonePortal access enforcement is not enabled after lockdown"
        );
        for account in &accounts {
            ensure!(
                portal.role(*account).call().await? != PortalRole::Account,
                "account {account} remained allowed after lockdown"
            );
        }

        println!(
            "Portal {} is locked down; access enforcement is enabled and {} allowed account(s) were revoked.",
            self.portal,
            accounts.len()
        );
        Ok(())
    }
}
