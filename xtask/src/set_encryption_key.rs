//! Registers an encryption public key on the ZonePortal from its corresponding private key.
//!
//! Calls the shared sequencer registration helper, which derives the secp256k1
//! public key, constructs the proof-of-possession signature, and submits it to
//! the portal contract.

use alloy::{
    network::EthereumWallet,
    primitives::Address,
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use eyre::WrapErr as _;
use tempo_alloy::TempoNetwork;
use tempo_chainspec::constants::{mainnet::MAINNET_CHAIN_ID, moderato::MODERATO_CHAIN_ID};
use zone_sequencer::register_encryption_key;

#[derive(Debug, clap::Parser)]
pub(crate) struct SetEncryptionKey {
    /// Tempo L1 RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc_url: String,

    /// ZonePortal contract address on Tempo L1.
    #[arg(long, env = "L1_PORTAL_ADDRESS")]
    portal: Address,

    /// Admin or active sequencer private key (hex) used to sign the transaction.
    #[arg(long, env = "PRIVATE_KEY", hide_env_values = true)]
    private_key: String,

    /// Encryption private key (hex) whose public key is registered.
    #[arg(long, hide_env_values = true)]
    encryption_private_key: String,
}

impl SetEncryptionKey {
    pub(crate) async fn run(self) -> eyre::Result<()> {
        let transaction_signer =
            parse_private_key(&self.private_key, "--private-key / PRIVATE_KEY")?;
        let encryption_signer =
            parse_private_key(&self.encryption_private_key, "--encryption-private-key")?;
        let wallet = EthereumWallet::from(transaction_signer);
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .wallet(wallet)
            .connect(&self.l1_rpc_url)
            .await?;
        let chain_id = provider.get_chain_id().await?;

        println!(
            "Sending setSequencerEncryptionKey to portal {}...",
            self.portal
        );
        let tx_hash = register_encryption_key(&provider, self.portal, &encryption_signer)
            .await
            .wrap_err("failed to send setSequencerEncryptionKey")?;

        println!("Encryption public key registered!");
        match chain_id {
            MAINNET_CHAIN_ID => println!("Explorer: https://explore.tempo.xyz/tx/{tx_hash}"),
            MODERATO_CHAIN_ID => {
                println!("Explorer: https://explore.moderato.tempo.xyz/tx/{tx_hash}")
            }
            _ => println!("Transaction: {tx_hash}"),
        }

        Ok(())
    }
}

fn parse_private_key(private_key: &str, source: &str) -> eyre::Result<PrivateKeySigner> {
    private_key
        .trim()
        .strip_prefix("0x")
        .unwrap_or(private_key.trim())
        .parse()
        .wrap_err_with(|| format!("invalid {source}"))
}

#[cfg(test)]
mod tests {
    use super::parse_private_key;

    #[test]
    fn parses_prefixed_and_unprefixed_private_keys() {
        let key = "1111111111111111111111111111111111111111111111111111111111111111";

        assert_eq!(
            parse_private_key(key, "test key").unwrap().to_bytes(),
            parse_private_key(&format!("0x{key}"), "test key")
                .unwrap()
                .to_bytes()
        );
    }
}
