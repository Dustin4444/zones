//! Multi-sequencer P2P cluster harness and sequencer task spawning.

use super::*;

use alloy::genesis::Genesis;
use alloy_eips::NumHash;
use alloy_primitives::{Address, B256};
use alloy_provider::Provider;
use alloy_rpc_types_eth::BlockNumberOrTag;
use alloy_signer_local::PrivateKeySigner;
use commonware_codec::Encode as _;
use commonware_cryptography::{Signer as _, ed25519::PrivateKey as Ed25519PrivateKey};
use reth_primitives_traits::SealedHeader;
use std::{
    net::{SocketAddr, TcpListener},
    sync::atomic::Ordering,
    time::Duration,
};
use tempo_zone_contracts::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS};
use zone_l1::{Deposit, L1Deposit, L1PortalEvents};
use zone_p2p::{LeadershipState, P2pConfig, P2pPeerId, Role};

/// Spawn the zone sequencer background tasks (batch submitter + withdrawal processor).
pub(crate) async fn spawn_sequencer(
    l1: &L1TestNode,
    zone: &ZoneTestNode,
    portal_address: Address,
    sequencer_signer: alloy_signer_local::PrivateKeySigner,
) -> zone_sequencer::ZoneSequencerHandle {
    spawn_sequencer_with_config(
        l1,
        zone,
        portal_address,
        sequencer_signer,
        zone_sequencer::BatchAnchorConfig::default(),
        zone_sequencer::WithdrawalBatchLimits::default(),
    )
    .await
}

/// Spawn the zone sequencer background tasks with custom limits.
pub(crate) async fn spawn_sequencer_with_config(
    l1: &L1TestNode,
    zone: &ZoneTestNode,
    portal_address: Address,
    sequencer_signer: alloy_signer_local::PrivateKeySigner,
    batch_anchor_config: zone_sequencer::BatchAnchorConfig,
    withdrawal_batch_limits: zone_sequencer::WithdrawalBatchLimits,
) -> zone_sequencer::ZoneSequencerHandle {
    let config = zone_sequencer::ZoneSequencerConfig {
        portal_address,
        l1_rpc_url: l1.http_url().to_string(),
        retry_connection_interval: Duration::from_millis(100),
        zone_poll_interval: Duration::from_secs(1),
        withdrawal_poll_interval: Duration::from_millis(500),
        withdrawal_batch_limits,
        outbox_address: ZONE_OUTBOX_ADDRESS,
        inbox_address: ZONE_INBOX_ADDRESS,
        batch_anchor_config,
        attestation_store: None,
    };

    zone.spawn_sequencer(config, sequencer_signer).await
}

/// A three-node multi-sequencer cluster driven by the real role controller.
///
/// Node 0 is the manifest bootstrap leader. Each node runs the complete dynamic role
/// machinery: the leader generation (engine with the per-anchor production permit,
/// broadcast, settlement, sequencer background tasks) and the follower generation (import,
/// transaction forwarding), switched by finalized leadership observations that tests publish
/// directly into each node's [`LeadershipSchedule`].
///
/// Every node uses a distinct sequencer signer, so a block's beneficiary identifies its
/// producer.
pub(crate) struct P2pCluster {
    pub(crate) nodes: Vec<ZoneTestNode>,
    pub(crate) p2p_public_keys: Vec<P2pPeerId>,
    pub(crate) sequencer_signers: Vec<PrivateKeySigner>,
    pub(crate) fixture: L1Fixture,
}

impl P2pCluster {
    /// The next Tempo anchor number the fixture will inject.
    pub(crate) fn next_anchor_number(&self) -> u64 {
        self.fixture.next_anchor_number()
    }

    /// Inject one L1 block into every node, simulating each node's finalized subscriber:
    /// the anchor is recorded in every tracker and the block enqueued in every deposit
    /// queue. Returns the anchor.
    pub(crate) fn inject_block(&mut self, deposits: Vec<Deposit>) -> eyre::Result<NumHash> {
        let all: Vec<usize> = (0..self.nodes.len()).collect();
        self.inject_block_observed_by(deposits, &all)
    }

    /// Inject one L1 block into every deposit queue, but record the anchor observation only
    /// on the given nodes. A node without the observation cannot import the corresponding
    /// zone block (or produce it) until [`Self::record_anchor`] delivers it.
    pub(crate) fn inject_block_observed_by(
        &mut self,
        deposits: Vec<Deposit>,
        observers: &[usize],
    ) -> eyre::Result<NumHash> {
        let block = self.fixture.next_block();
        let anchor = SealedHeader::seal_slow(block.header.clone()).num_hash();
        let events = L1PortalEvents::from_deposits(
            deposits.iter().cloned().map(L1Deposit::Regular).collect(),
        );
        for index in observers {
            self.nodes[*index]
                .l1_block_tracker()
                .record_with_portal_events(anchor, events.clone())?;
        }
        for node in &self.nodes {
            self.fixture
                .enqueue(&block, node.deposit_queue(), deposits.clone());
        }
        Ok(anchor)
    }

    /// Deliver a previously withheld anchor observation to one node.
    pub(crate) fn record_anchor(
        &self,
        index: usize,
        anchor: NumHash,
        deposits: Vec<Deposit>,
    ) -> eyre::Result<()> {
        let events =
            L1PortalEvents::from_deposits(deposits.into_iter().map(L1Deposit::Regular).collect());
        self.nodes[index]
            .l1_block_tracker()
            .record_with_portal_events(anchor, events)?;
        Ok(())
    }

    /// Publish a finalized leadership transition into every node's schedule, standing in
    /// for each node's receipt-authenticated `LeaderUpdated` observation.
    pub(crate) fn publish_transition(
        &self,
        epoch: u64,
        leader_index: usize,
        activation_tempo_block: u64,
    ) -> eyre::Result<()> {
        for node in &self.nodes {
            node.leadership().publish(LeadershipState::new(
                epoch,
                self.p2p_public_keys[leader_index].clone(),
                activation_tempo_block,
            ))?;
        }
        Ok(())
    }

    /// Wait until every node's canonical head reaches `height`.
    pub(crate) async fn wait_all_at(&self, height: u64, timeout: Duration) -> eyre::Result<()> {
        for node in &self.nodes {
            node.wait_for_block_number(height, timeout).await?;
        }
        Ok(())
    }

    /// Assert every node holds the same block at `height` and return its header.
    pub(crate) async fn assert_same_block(
        &self,
        height: u64,
    ) -> eyre::Result<alloy_rpc_types_eth::Header> {
        let mut reference: Option<alloy_rpc_types_eth::Header> = None;
        for (index, node) in self.nodes.iter().enumerate() {
            let block = node
                .provider()
                .get_block_by_number(BlockNumberOrTag::Number(height))
                .await?
                .ok_or_else(|| eyre::eyre!("node {index} is missing block {height}"))?;
            match &reference {
                None => reference = Some(block.header),
                Some(reference) => eyre::ensure!(
                    block.header.hash == reference.hash,
                    "node {index} diverges at height {height}: {} != {}",
                    block.header.hash,
                    reference.hash,
                ),
            }
        }
        reference.ok_or_else(|| eyre::eyre!("cluster is empty"))
    }
}

/// Returns a free localhost address to bind a P2P listener on.
fn available_address() -> eyre::Result<SocketAddr> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?)
}

/// Start a three-node multi-sequencer cluster with identical genesis state and authenticated
/// P2P identities. Node 0 bootstraps as the leader.
pub(crate) async fn start_local_p2p_cluster(seed_blocks: u64) -> eyre::Result<P2pCluster> {
    let addresses = [
        available_address()?,
        available_address()?,
        available_address()?,
    ];
    let identities = [
        Ed25519PrivateKey::from_seed(101),
        Ed25519PrivateKey::from_seed(102),
        Ed25519PrivateKey::from_seed(103),
    ];
    let public_keys = identities.each_ref().map(|key| key.public_key());
    let secp256k1_keys = [101_u64, 102, 103].map(|key| format!("0x{key:064x}"));
    let secp256k1_signers = secp256k1_keys
        .each_ref()
        .map(|key| key.parse::<PrivateKeySigner>().unwrap());
    // Distinct shared-sequencer signers per node: the block beneficiary then identifies the
    // producer, which handoff tests assert on.
    let sequencer_signers: Vec<PrivateKeySigner> = (0x51u8..0x54)
        .map(|byte| {
            PrivateKeySigner::from_bytes(&B256::with_last_byte(byte))
                .expect("valid test sequencer key")
        })
        .collect();

    let unique = NEXT_CHAIN_ID.fetch_add(1, Ordering::Relaxed);
    let config_dir = std::env::temp_dir().join(format!(
        "tempo-zone-p2p-test-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&config_dir)?;
    let manifest_path = config_dir.join("manifest.toml");
    let mut manifest = format!(
        "zone_id = 0\nleader_ed25519_public_key = \"{}\"\n",
        const_hex::encode_prefixed(public_keys[0].as_ref())
    );
    for (index, ((public_key, secp256k1_signer), address)) in public_keys
        .iter()
        .zip(&secp256k1_signers)
        .zip(addresses)
        .enumerate()
    {
        manifest.push_str(&format!(
            "\n[[nodes]]\nname = \"node-{index}\"\ned25519_public_key = \"{}\"\nsecp256k1_address = \"{}\"\naddress = \"{address}\"\n",
            const_hex::encode_prefixed(public_key.as_ref()),
            secp256k1_signer.address(),
        ));
    }
    std::fs::write(&manifest_path, manifest)?;
    let mut configs = Vec::with_capacity(3);
    for (index, role) in [(0, Role::Leader), (1, Role::Follower), (2, Role::Follower)] {
        let key_path = config_dir.join(format!("node-{index}.key"));
        std::fs::write(
            &key_path,
            const_hex::encode_prefixed(identities[index].encode().as_ref()),
        )?;
        let secp256k1_key_path = config_dir.join(format!("node-{index}-secp256k1.key"));
        std::fs::write(&secp256k1_key_path, &secp256k1_keys[index])?;
        configs.push(P2pConfig::load(
            &manifest_path,
            &key_path,
            Some(&secp256k1_key_path),
            addresses[index],
            false,
            0,
            Some(role),
        )?);
    }
    let _ = std::fs::remove_dir_all(&config_dir);

    let chain_id = next_unique_chain_id();
    let l1_rpc_url = spawn_test_l1_rpc(1337).await?;
    let genesis: Genesis = serde_json::from_str(zone_node::genesis::GENESIS_TEMPLATE_JSON)?;
    let mut nodes = Vec::with_capacity(3);
    for (index, config) in configs.into_iter().enumerate() {
        nodes.push(
            ZoneTestNode::launch_with_genesis_and_withdrawal_batch_interval(
                l1_rpc_url.clone(),
                Address::ZERO,
                chain_id,
                Some(genesis.clone()),
                sequencer_signers[index].clone(),
                8,
                Some(config),
                false,
            )
            .await?,
        );
    }

    let fixture = L1Fixture::new();
    for zone in &nodes {
        fixture.seed_l1_cache(
            zone.l1_state_cache(),
            zone.enabled_tokens(),
            Address::ZERO,
            Address::ZERO,
            seed_blocks,
        );
    }
    Ok(P2pCluster {
        nodes,
        p2p_public_keys: public_keys.to_vec(),
        sequencer_signers,
        fixture,
    })
}

pub(crate) fn leader_p2p_config(listen: SocketAddr) -> eyre::Result<P2pConfig> {
    let identities = [
        Ed25519PrivateKey::from_seed(201),
        Ed25519PrivateKey::from_seed(202),
        Ed25519PrivateKey::from_seed(203),
    ];
    let public_keys = identities.each_ref().map(|key| key.public_key());
    let secp256k1_keys = [201_u64, 202, 203].map(|key| format!("0x{key:064x}"));
    let secp256k1_signers = secp256k1_keys
        .each_ref()
        .map(|key| key.parse::<PrivateKeySigner>().unwrap());
    let addresses = [listen, available_address()?, available_address()?];
    let config_dir = std::env::temp_dir().join(format!(
        "tempo-zone-p2p-config-{}-{}",
        std::process::id(),
        next_unique_chain_id()
    ));
    std::fs::create_dir_all(&config_dir)?;
    let manifest_path = config_dir.join("manifest.toml");
    let key_path = config_dir.join("leader.key");
    let mut manifest = format!(
        "zone_id = 0\nleader_ed25519_public_key = \"{}\"\n",
        const_hex::encode_prefixed(public_keys[0].as_ref())
    );
    for (index, ((public_key, secp256k1_signer), address)) in public_keys
        .iter()
        .zip(&secp256k1_signers)
        .zip(addresses)
        .enumerate()
    {
        manifest.push_str(&format!(
            "\n[[nodes]]\nname = \"node-{index}\"\ned25519_public_key = \"{}\"\nsecp256k1_address = \"{}\"\naddress = \"{address}\"\n",
            const_hex::encode_prefixed(public_key.as_ref()),
            secp256k1_signer.address(),
        ));
    }
    std::fs::write(&manifest_path, manifest)?;
    std::fs::write(
        &key_path,
        const_hex::encode_prefixed(identities[0].encode().as_ref()),
    )?;
    let secp256k1_key_path = config_dir.join("leader-secp256k1.key");
    std::fs::write(&secp256k1_key_path, &secp256k1_keys[0])?;
    let config = P2pConfig::load(
        &manifest_path,
        &key_path,
        Some(&secp256k1_key_path),
        listen,
        false,
        0,
        Some(Role::Leader),
    )?;
    let _ = std::fs::remove_dir_all(config_dir);
    Ok(config)
}

pub(crate) async fn start_chain_id_rpc(chain_id: u64) -> eyre::Result<url::Url> {
    Ok(spawn_test_l1_rpc(chain_id).await?.parse()?)
}
