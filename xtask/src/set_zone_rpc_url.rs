//! Updates the public RPC URL metadata stored on a ZonePortal.

use alloy::{
    network::{EthereumWallet, primitives::ReceiptResponse},
    primitives::Address,
    providers::ProviderBuilder,
    signers::local::PrivateKeySigner,
};
use eyre::{WrapErr as _, eyre};
use tempo_alloy::TempoNetwork;
use zone::abi::ZonePortal;

#[derive(Debug, clap::Parser)]
pub(crate) struct SetZoneRpcUrl {
    /// Tempo L1 RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc_url: String,

    /// ZonePortal contract address on Tempo L1.
    #[arg(long, env = "L1_PORTAL_ADDRESS")]
    portal: Address,

    /// Sequencer private key (hex) for the ZonePortal update transaction.
    #[arg(long, env = "SEQUENCER_KEY")]
    private_key: String,

    /// New public zone RPC URL. Empty string clears/unsets the URL.
    #[arg(long, default_value = "")]
    zone_rpc_url: String,
}

impl SetZoneRpcUrl {
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

        println!("Sending setZoneRpcUrl to portal {}...", self.portal);
        let portal = ZonePortal::new(self.portal, &provider);
        let receipt = portal
            .setZoneRpcUrl(self.zone_rpc_url.clone())
            .send_sync()
            .await
            .wrap_err("failed to send setZoneRpcUrl")?;

        let tx_hash = receipt.transaction_hash;
        if !receipt.status() {
            return Err(eyre!("setZoneRpcUrl reverted (tx: {tx_hash})"));
        }

        println!("Zone RPC URL updated: {}", self.zone_rpc_url);
        println!("Explorer: https://explore.moderato.tempo.xyz/tx/{tx_hash}");

        Ok(())
    }
}
