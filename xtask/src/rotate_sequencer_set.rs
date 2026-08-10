//! Rotates a ZonePortal sequencer set without removing the active leader prematurely.

use alloy::{
    network::{EthereumWallet, ReceiptResponse as _},
    primitives::{Address, U256},
    providers::ProviderBuilder,
    signers::local::PrivateKeySigner,
};
use eyre::{WrapErr as _, ensure};
use std::collections::HashSet;
use tempo_alloy::TempoNetwork;
use tempo_zone_contracts::ZonePortal;

#[derive(Debug, clap::Parser)]
pub(crate) struct RotateSequencerSet {
    /// Tempo L1 HTTP RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc_url: String,

    /// ZonePortal address on Tempo L1.
    #[arg(long, env = "L1_PORTAL_ADDRESS")]
    portal: Address,

    /// Final sequencer set. Pass once per sequencer, in the desired order.
    #[arg(long = "sequencer", required = true)]
    sequencers: Vec<Address>,

    /// Number of signatures required for settlement.
    #[arg(long)]
    threshold: u8,

    /// Sequencer that should lead after the rotation.
    #[arg(long)]
    leader: Address,

    /// Portal admin key used for setSequencerSet and setLeader transactions.
    #[arg(long, env = "ADMIN_KEY", hide_env_values = true)]
    admin_private_key: String,
}

impl RotateSequencerSet {
    pub(crate) async fn run(self) -> eyre::Result<()> {
        validate_target(&self.sequencers, self.threshold, self.leader)?;

        let admin_signer = parse_key(&self.admin_private_key, "ADMIN_KEY")?;
        let admin_address = admin_signer.address();

        let admin_provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .wallet(EthereumWallet::from(admin_signer))
            .connect(&self.l1_rpc_url)
            .await
            .wrap_err("failed connecting admin provider to Tempo L1 RPC")?;
        let admin_portal = ZonePortal::new(self.portal, &admin_provider);

        let onchain_admin = admin_portal
            .admin()
            .call()
            .await
            .wrap_err("failed reading portal admin")?;
        ensure!(
            admin_address == onchain_admin,
            "ADMIN_KEY resolves to {admin_address}, but portal admin is {onchain_admin}"
        );

        let current_leader = admin_portal
            .leader()
            .call()
            .await
            .wrap_err("failed reading current leader")?;
        let current_set = read_sequencer_set(&admin_portal).await?;
        let mut installed_set = current_set.clone();

        println!("Portal:         {}", self.portal);
        println!("Current leader: {current_leader}");
        println!("New leader:     {}", self.leader);
        println!("Final set:      {:?}", self.sequencers);
        println!("Threshold:      {}", self.threshold);

        if current_leader != self.leader {
            let joint_set = joint_set(&self.sequencers, current_leader);

            if installed_set != joint_set
                || admin_portal.sequencerThreshold().call().await? != self.threshold
            {
                send_set(
                    &admin_portal,
                    joint_set.clone(),
                    self.threshold,
                    "joint set",
                )
                .await?;
                installed_set = joint_set;
            }

            let expected_epoch = admin_portal
                .leaderEpoch()
                .call()
                .await
                .wrap_err("failed reading leader epoch")?;
            let receipt = admin_portal
                .setLeader(self.leader, expected_epoch)
                .send()
                .await
                .wrap_err("failed sending setLeader")?
                .get_receipt()
                .await
                .wrap_err("failed waiting for setLeader receipt")?;
            ensure!(receipt.status(), "setLeader reverted");
            println!("Installed new leader in tx {:?}", receipt.transaction_hash);
        }

        let installed_threshold = admin_portal.sequencerThreshold().call().await?;
        if installed_set != self.sequencers || installed_threshold != self.threshold {
            send_set(
                &admin_portal,
                self.sequencers.clone(),
                self.threshold,
                "final set",
            )
            .await?;
        }

        ensure!(
            admin_portal.leader().call().await? == self.leader,
            "leader verification failed"
        );
        ensure!(
            read_sequencer_set(&admin_portal).await? == self.sequencers,
            "sequencer set verification failed"
        );
        ensure!(
            admin_portal.sequencerThreshold().call().await? == self.threshold,
            "sequencer threshold verification failed"
        );
        println!("Rotation complete and verified");
        Ok(())
    }
}

fn parse_key(key: &str, name: &str) -> eyre::Result<PrivateKeySigner> {
    key.strip_prefix("0x")
        .unwrap_or(key)
        .parse()
        .wrap_err_with(|| format!("{name} is not a valid private key"))
}

fn validate_target(sequencers: &[Address], threshold: u8, leader: Address) -> eyre::Result<()> {
    ensure!(
        !sequencers.is_empty(),
        "at least one --sequencer is required"
    );
    ensure!(
        threshold > 0 && usize::from(threshold) <= sequencers.len(),
        "--threshold must be between 1 and the number of sequencers"
    );
    ensure!(
        sequencers.iter().all(|address| !address.is_zero()),
        "sequencer addresses cannot be zero"
    );
    ensure!(
        sequencers.iter().copied().collect::<HashSet<_>>().len() == sequencers.len(),
        "sequencer addresses must be unique"
    );
    ensure!(
        sequencers.contains(&leader),
        "--leader must be included in the final sequencer set"
    );
    Ok(())
}

fn joint_set(final_set: &[Address], current_leader: Address) -> Vec<Address> {
    let mut joint = final_set.to_vec();
    if !joint.contains(&current_leader) {
        joint.push(current_leader);
    }
    joint
}

async fn read_sequencer_set<P, N>(
    portal: &ZonePortal::ZonePortalInstance<P, N>,
) -> eyre::Result<Vec<Address>>
where
    P: alloy::providers::Provider<N>,
    N: alloy::network::Network,
{
    let count = portal
        .sequencerCount()
        .call()
        .await
        .wrap_err("failed reading sequencer count")?;
    let count = count.to::<usize>();
    let mut sequencers = Vec::with_capacity(count);
    for index in 0..count {
        sequencers.push(
            portal
                .sequencerAt(U256::from(index))
                .call()
                .await
                .wrap_err_with(|| format!("failed reading sequencer at index {index}"))?,
        );
    }
    Ok(sequencers)
}

async fn send_set<P, N>(
    portal: &ZonePortal::ZonePortalInstance<P, N>,
    sequencers: Vec<Address>,
    threshold: u8,
    label: &str,
) -> eyre::Result<()>
where
    P: alloy::providers::Provider<N>,
    N: alloy::network::Network,
{
    let receipt = portal
        .setSequencerSet(sequencers, threshold)
        .send()
        .await
        .wrap_err_with(|| format!("failed sending {label}"))?
        .get_receipt()
        .await
        .wrap_err_with(|| format!("failed waiting for {label} receipt"))?;
    ensure!(receipt.status(), "{label} transaction reverted");
    println!("Installed {label} in tx {:?}", receipt.transaction_hash());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    const OLD: Address = address!("0x1111111111111111111111111111111111111111");
    const NEW: Address = address!("0x2222222222222222222222222222222222222222");
    const FOLLOWER: Address = address!("0x3333333333333333333333333333333333333333");

    #[test]
    fn joint_set_retains_old_leader_until_handoff() {
        assert_eq!(joint_set(&[NEW, FOLLOWER], OLD), vec![NEW, FOLLOWER, OLD]);
    }

    #[test]
    fn joint_set_does_not_duplicate_retained_leader() {
        assert_eq!(joint_set(&[NEW, OLD], OLD), vec![NEW, OLD]);
    }

    #[test]
    fn target_requires_leader_membership() {
        assert!(validate_target(&[FOLLOWER], 1, NEW).is_err());
    }

    #[test]
    fn target_rejects_duplicate_members() {
        assert!(validate_target(&[NEW, NEW], 1, NEW).is_err());
    }
}
