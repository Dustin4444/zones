// The `sol!`generated `ZoneFactory` event/contract bindings expand to functions
// with more than 7 parameters, which trips `clippy::too_many_arguments`.
#![allow(clippy::too_many_arguments)]

use alloy::{
    network::{EthereumWallet, primitives::ReceiptResponse},
    primitives::{Address, address, keccak256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
    sol_types::SolEvent,
};
use alloy_rlp::Encodable;
use eyre::{WrapErr as _, ensure, eyre};
use std::path::PathBuf;
use tempo_alloy::{TempoNetwork, rpc::TempoCallBuilderExt as _};
use tempo_chainspec::spec::TEMPO_T0_BASE_FEE;
use tempo_contracts::precompiles::ITIP403Registry;
use tempo_precompiles::TIP403_REGISTRY_ADDRESS;
use tempo_zone_contracts::{
    ZONE_MESSENGER_ADDRESS, ZONE_VERIFIER_ADDRESS, ZoneFactory, ZonePortal,
};
use zone_primitives::constants::zone_chain_id;

use crate::zone_utils::{MODERATO_ZONE_FACTORY, check};

alloy::sol! {
    #[sol(rpc)]
    interface LegacyZoneFactory {
        struct CreateZoneParams {
            address initialToken;
            address admin;
            address[] sequencers;
            uint8 threshold;
            string rpcUrl;
        }

        function createZone(CreateZoneParams calldata params)
            external
            returns (uint32 zoneId, address portal);
    }
}

#[derive(Debug, clap::Parser)]
pub(crate) struct CreateZone {
    /// Output directory where genesis.json will be written.
    #[arg(short, long)]
    output: PathBuf,

    /// Tempo L1 HTTP RPC URL used to fetch headers and send the createZone transaction.
    #[arg(long, default_value = "https://rpc.moderato.tempo.xyz")]
    l1_rpc_url: String,

    /// ZoneFactory contract address on Tempo L1.
    #[arg(long, env = "ZONE_FACTORY", default_value_t = MODERATO_ZONE_FACTORY)]
    zone_factory: Address,

    /// Use the legacy ZoneFactory selector and configure roles through the created portal.
    #[arg(long)]
    legacy_factory: bool,

    /// Initial TIP-20 token address for the zone (additional tokens can be enabled later).
    #[arg(long, default_value_t = address!("0x20C0000000000000000000000000000000000000"))]
    initial_token: Address,

    /// Initial callback-only ZoneGateway implementation. Repeat to support multiple gateways.
    /// Gateways may also be registered after creation through the ZonePortal admin API.
    #[arg(long = "zone-gateway")]
    zone_gateways: Vec<Address>,

    /// Allowed plain-withdrawal/deposit account. Repeat for each member.
    /// Zone gateways are configured separately and must not be included.
    #[arg(long = "allowed-account", required = true)]
    allowed_accounts: Vec<Address>,

    /// Sequencer address that will operate the zone.
    #[arg(long)]
    sequencer: Address,

    /// Admin address that controls token enablement and deposit pause/resume.
    /// Pass the sequencer address explicitly when both roles should use the same key.
    #[arg(long)]
    admin: Address,

    /// Public RPC endpoint for the zone, published on-chain in the portal.
    /// Can be left empty and set later via `ZonePortal.setRpcUrl`.
    #[arg(long, default_value = "")]
    rpc_url: String,

    /// ZoneFactory owner private key (hex) for signing the createZone transaction on L1.
    /// Prefer the ZONE_FACTORY_OWNER_KEY environment variable so the key is not exposed in the
    /// process argument list.
    #[arg(long, env = "ZONE_FACTORY_OWNER_KEY", hide_env_values = true)]
    private_key: String,

    /// Base fee per gas for the zone L2.
    #[arg(long, default_value_t = TEMPO_T0_BASE_FEE.into())]
    base_fee_per_gas: u128,

    /// Genesis block gas limit for the zone L2.
    #[arg(long, default_value_t = 30_000_000)]
    gas_limit: u64,

    /// Path to the Foundry compiled output directory containing zone contract artifacts.
    #[arg(long, default_value = "specs/ref-impls/out")]
    specs_out: PathBuf,
}

impl CreateZone {
    pub(crate) async fn run(self) -> eyre::Result<()> {
        let key_str = self
            .private_key
            .strip_prefix("0x")
            .unwrap_or(&self.private_key);
        let signer: PrivateKeySigner = key_str.parse()?;
        let wallet = EthereumWallet::from(signer);
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .wallet(wallet)
            .connect(&self.l1_rpc_url)
            .await?;

        if self.legacy_factory {
            let registry = ITIP403Registry::new(TIP403_REGISTRY_ADDRESS, &provider);
            let binding = registry
                .tokenTransferPolicyId(self.initial_token)
                .call()
                .await
                .wrap_err("failed reading the initial token TIP-403 policy binding")?;
            if !binding.isSet {
                let receipt = registry
                    .migrateTransferPolicyIds(vec![self.initial_token])
                    .send_sync()
                    .await
                    .wrap_err("failed migrating the initial token TIP-403 policy binding")?;
                check(&receipt, "migrate initial token TIP-403 policy")?;

                let binding = registry
                    .tokenTransferPolicyId(self.initial_token)
                    .call()
                    .await
                    .wrap_err("failed verifying the initial token TIP-403 policy binding")?;
                ensure!(
                    binding.isSet,
                    "initial token TIP-403 policy binding is still unset after migration"
                );
            }
        }

        println!("Verifier: {ZONE_VERIFIER_ADDRESS}");
        println!("Messenger: {ZONE_MESSENGER_ADDRESS}");

        // Anchor before createZone so the zone replays the creation block and its
        // initial TokenEnabled event during L1 backfill.
        let anchor_block_number = provider.get_block_number().await?;
        let anchor_header = provider
            .get_header_by_number(anchor_block_number.into())
            .await?
            .ok_or_else(|| eyre!("anchor header {anchor_block_number} not found"))?
            .inner
            .inner;
        let mut genesis_header_rlp = Vec::new();
        anchor_header.encode(&mut genesis_header_rlp);
        let anchor_hash = keccak256(&genesis_header_rlp);

        println!("Admin: {}", self.admin);
        println!("Sequencer: {}", self.sequencer);

        println!(
            "Creating zone on L1 via ZoneFactory at {}...",
            self.zone_factory
        );
        let receipt = if self.legacy_factory {
            LegacyZoneFactory::new(self.zone_factory, &provider)
                .createZone(LegacyZoneFactory::CreateZoneParams {
                    initialToken: self.initial_token,
                    admin: self.admin,
                    sequencers: vec![self.sequencer],
                    threshold: 1,
                    rpcUrl: self.rpc_url.clone(),
                })
                .send_sync()
                .await?
        } else {
            ZoneFactory::new(self.zone_factory, &provider)
                .createZone(ZoneFactory::CreateZoneParams {
                    initialToken: self.initial_token,
                    admin: self.admin,
                    sequencers: vec![self.sequencer],
                    threshold: 1,
                    rpcUrl: self.rpc_url.clone(),
                    allowedAccounts: self.allowed_accounts.clone(),
                    zoneGateways: self.zone_gateways.clone(),
                })
                .send_sync()
                .await?
        };
        println!("Transaction confirmed in block {:?}", receipt.block_number);
        println!("Status: {}", receipt.status());
        println!("Gas used: {:?}", receipt.gas_used);

        if !receipt.status() {
            return Err(eyre!(
                "createZone transaction reverted (tx: {:?})",
                receipt.transaction_hash
            ));
        }

        let event = receipt
            .inner
            .logs()
            .iter()
            .find_map(|log| ZoneFactory::ZoneCreated::decode_log(&log.inner).ok())
            .ok_or_else(|| eyre!("no ZoneCreated event in receipt"))?;

        let zone_id = event.zoneId;
        let portal = event.portal;
        let chain_id = zone_chain_id(zone_id);

        if self.legacy_factory {
            self.configure_legacy_roles(portal).await?;
        }

        println!(
            "Using pre-creation block {} (hash: {anchor_hash}) as genesis anchor",
            anchor_header.inner.number
        );

        let header_rlp_hex = const_hex::encode(&genesis_header_rlp);

        let genesis_cmd = crate::generate_zone_genesis::GenerateZoneGenesis {
            output: self.output.clone(),
            chain_id,
            base_fee_per_gas: self.base_fee_per_gas,
            gas_limit: self.gas_limit,
            tempo_portal: portal,
            tempo_genesis_header_rlp: Some(header_rlp_hex),
            admin: self.admin,
            sequencer: Some(self.sequencer),
            specs_out: self.specs_out.clone(),
            with_createx: true,
            with_safe_deployer: true,
            with_create2_factory: true,
        };
        genesis_cmd.run().await?;

        // Write zone.json with deployment metadata for downstream tooling (e.g. `just zone-up`).
        let zone_json = serde_json::json!({
            "zoneId": zone_id,
            "chainId": chain_id,
            "portal": format!("{portal}"),
            "messenger": format!("{ZONE_MESSENGER_ADDRESS}"),
            "initialToken": format!("{}", self.initial_token),
            "zoneGateways": self.zone_gateways.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "allowedAccounts": self.allowed_accounts.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "admin": format!("{}", self.admin),
            "sequencer": format!("{}", self.sequencer),
            "tempoAnchorBlock": anchor_header.inner.number,
            "zoneFactory": format!("{}", self.zone_factory),
            "rpcUrl": self.rpc_url,
        });
        let zone_json_path = self.output.join("zone.json");
        std::fs::write(
            &zone_json_path,
            serde_json::to_string_pretty(&zone_json).wrap_err("failed encoding zone.json")?,
        )
        .wrap_err("failed writing zone.json")?;

        println!("Zone created successfully!");
        println!("  Zone ID: {zone_id}");
        println!("  Chain ID: {chain_id}");
        println!("  Portal: {portal}");
        println!("  Messenger: {ZONE_MESSENGER_ADDRESS}");
        println!("  Initial Token: {}", self.initial_token);
        println!("  Admin: {}", self.admin);
        println!("  Sequencer: {}", self.sequencer);
        println!("  ZoneFactory: {}", self.zone_factory);
        if !self.rpc_url.is_empty() {
            println!("  RPC URL: {}", self.rpc_url);
        }
        println!("  Tempo anchor block: {}", anchor_header.inner.number);
        println!(
            "  Genesis written to: {}",
            self.output.join("genesis.json").display()
        );
        println!("  Zone metadata written to: {}", zone_json_path.display());

        Ok(())
    }

    async fn configure_legacy_roles(&self, portal_address: Address) -> eyre::Result<()> {
        let key = std::env::var("PORTAL_ADMIN_KEY")
            .wrap_err("PORTAL_ADMIN_KEY must be set when --legacy-factory is used")?;
        let signer: PrivateKeySigner = key
            .strip_prefix("0x")
            .unwrap_or(&key)
            .parse()
            .wrap_err("PORTAL_ADMIN_KEY is not a valid private key")?;
        ensure!(
            signer.address() == self.admin,
            "PORTAL_ADMIN_KEY resolves to {}, not configured admin {}",
            signer.address(),
            self.admin
        );
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .wallet(EthereumWallet::from(signer))
            .connect(&self.l1_rpc_url)
            .await
            .wrap_err("failed connecting portal admin to Tempo L1")?;
        let portal = ZonePortal::new(portal_address, &provider);

        for gateway in &self.zone_gateways {
            let receipt = portal
                .setGateway(*gateway, true)
                .fee_token(self.initial_token)
                .send()
                .await
                .wrap_err_with(|| format!("failed registering ZoneGateway {gateway}"))?
                .get_receipt()
                .await
                .wrap_err_with(|| {
                    format!("failed waiting for ZoneGateway {gateway} registration")
                })?;
            check(&receipt, &format!("register ZoneGateway {gateway}"))?;
        }
        for account in &self.allowed_accounts {
            let receipt = portal
                .setAllowedAccount(*account, true)
                .fee_token(self.initial_token)
                .send()
                .await
                .wrap_err_with(|| format!("failed allowing Zone account {account}"))?
                .get_receipt()
                .await
                .wrap_err_with(|| format!("failed waiting for Zone account {account} allowlist"))?;
            check(&receipt, &format!("allow Zone account {account}"))?;
        }

        println!(
            "Configured {} gateway(s) and {} allowed account(s) through the legacy ZonePortal",
            self.zone_gateways.len(),
            self.allowed_accounts.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::LegacyZoneFactory;
    use alloy::sol_types::SolCall;

    #[test]
    fn legacy_factory_selector_matches_pinned_tempo() {
        assert_eq!(
            LegacyZoneFactory::createZoneCall::SELECTOR,
            [0xf2, 0xc5, 0x8f, 0x2b]
        );
    }
}
