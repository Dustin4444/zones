use alloy::{
    network::TransactionBuilder,
    primitives::{Address, Bytes, TxKind, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    sol_types::SolCall,
};
use eyre::eyre;
use tempo_alloy::TempoNetwork;
use tempo_zone_contracts::{ZONE_MESSENGER_ADDRESS, ZoneFactory, ZonePortal};

use crate::zone_utils::MODERATO_ZONE_FACTORY;

#[derive(Debug, clap::Parser)]
pub(crate) struct ZoneInfoCmd {
    /// Zone ID (integer) or portal address (0x...) to look up.
    identifier: String,

    /// Tempo L1 HTTP RPC URL.
    #[arg(long, default_value = "https://rpc.moderato.tempo.xyz")]
    l1_rpc_url: String,

    /// ZoneFactory contract address on Tempo L1.
    #[arg(long, env = "ZONE_FACTORY", default_value_t = MODERATO_ZONE_FACTORY)]
    zone_factory: Address,
}

impl ZoneInfoCmd {
    pub(crate) async fn run(self) -> eyre::Result<()> {
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect(&self.l1_rpc_url)
            .await?;

        let zone_id = if self.identifier.starts_with("0x") {
            // Look up by portal address — scan all zones
            let portal: Address = self.identifier.parse()?;
            let call = ZoneFactory::nextZoneIdCall {};
            let output = provider
                .call(
                    TransactionRequest::default()
                        .with_kind(TxKind::Call(self.zone_factory))
                        .input(Bytes::from(call.abi_encode()).into())
                        .into(),
                )
                .await?;
            let next_zone_id = ZoneFactory::nextZoneIdCall::abi_decode_returns(&output)?;

            let mut found = None;
            for id in 1..next_zone_id {
                let call = ZoneFactory::zonesCall { id };
                let output = provider
                    .call(
                        TransactionRequest::default()
                            .with_kind(TxKind::Call(self.zone_factory))
                            .input(Bytes::from(call.abi_encode()).into())
                            .into(),
                    )
                    .await?;
                let info = ZoneFactory::zonesCall::abi_decode_returns(&output)?;
                if info.portal == portal {
                    found = Some(id);
                    break;
                }
            }
            found.ok_or_else(|| eyre!("no zone found with portal address {portal}"))?
        } else {
            self.identifier
                .parse::<u32>()
                .map_err(|_| eyre!("expected a zone ID (integer) or portal address (0x...)"))?
        };

        let call = ZoneFactory::zonesCall { id: zone_id };
        let output = provider
            .call(
                TransactionRequest::default()
                    .with_kind(TxKind::Call(self.zone_factory))
                    .input(Bytes::from(call.abi_encode()).into())
                    .into(),
            )
            .await?;
        let info = ZoneFactory::zonesCall::abi_decode_returns(&output)?;
        if info.portal == Address::ZERO {
            return Err(eyre!("zone {zone_id} does not exist"));
        }
        println!("Zone {}", info.zoneId);
        println!("  Portal:                {}", info.portal);
        println!("  Messenger:             {ZONE_MESSENGER_ADDRESS}");
        println!("  Admin:                 {}", info.admin);
        println!("  Sequencers:            {:?}", info.sequencers);
        println!("  Threshold:             {}", info.threshold);
        println!("  Verifier:              {}", info.verifier);
        println!("  RPC URL:               {}", info.rpcUrl);

        // Query live portal state
        let portal = ZonePortal::new(info.portal, &provider);

        let sequencer_count = portal.sequencerCount().call().await?.to::<usize>();
        let mut sequencers = Vec::with_capacity(sequencer_count);
        for index in 0..sequencer_count {
            sequencers.push(portal.sequencerAt(U256::from(index)).call().await?);
        }
        let gas_rate = portal.zoneGasRate().call().await?;
        let batch_index = portal.withdrawalBatchIndex().call().await?;
        let block_hash = portal.blockHash().call().await?;
        let deposit_queue = portal.currentDepositQueueHash().call().await?;
        let last_synced = portal.lastSyncedTempoBlockNumber().call().await?;

        println!("\nPortal State");
        println!("  Active Sequencers:     {sequencers:?}");
        println!("  Zone Gas Rate:         {gas_rate}");
        println!("  Withdrawal Batch:      {batch_index}");
        println!("  Block Hash:            {block_hash}");
        println!("  Deposit Queue Hash:    {deposit_queue}");
        println!("  Last Synced Block:     {last_synced}");

        // Encryption key
        match portal.sequencerEncryptionKey().call().await {
            Ok(key) => {
                println!("\nEncryption Key");
                println!("  X:                     {}", key.x);
                println!("  Y Parity:              0x{:02x}", key.yParity);
            }
            Err(_) => println!("\nEncryption Key:          (not set)"),
        }

        // Enabled tokens
        let tokens = portal.enabled_tokens().await?;
        println!("\nEnabled Tokens ({})", tokens.len());
        for (i, token) in tokens.iter().enumerate() {
            println!("  [{i}] {token}");
        }

        Ok(())
    }
}
