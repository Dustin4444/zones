//! Tempo Zone CLI.

mod checker;

pub use checker::{CheckerArgs, CheckerCommand};

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use clap::{Args, CommandFactory, FromArgMatches};
use reth_chainspec::EthChainSpec;
use reth_consensus::noop::NoopConsensus;
use reth_ethereum::cli::Cli;
use reth_tracing::tracing::info;
use zeroize::Zeroizing;
use zone_chainspec::{ZoneChainSpec, ZoneChainSpecParser};
use zone_evm::ZoneEvmConfig;
use zone_p2p::{P2pConfig, Role};
use zone_payload::DEFAULT_WITHDRAWAL_BATCH_INTERVAL_BLOCKS;

use crate::{
    ZoneNode, ZoneRedactedRpcConfig, ZoneSequencerAddOnsConfig, dev::DevCommand,
    rpc::auth::DEFAULT_MAX_AUTH_TOKEN_VALIDITY_SECS,
};
use zone_checker::CheckerExEx;
use zone_sequencer::{
    BatchAnchorConfig, DEFAULT_MAX_IN_FLIGHT_WITHDRAWAL_BATCHES, DEFAULT_MAX_WITHDRAWAL_BATCH_GAS,
    MAX_WITHDRAWAL_BATCH_GAS, WithdrawalBatchLimits,
};

const MAX_LOGS_PER_RESPONSE: u64 = 1_000_000;
const MAX_BLOCKS_PER_FILTER: u64 = 1_000_000;

const ZONE_LOG_FILTER_DIRECTIVES: &str = concat!(
    "tungstenite=warn,",
    "alloy_pubsub=warn,",
    "alloy_transport_ws=warn,",
    "rustls::client=warn"
);

/// Tempo Zone CLI entry point.
pub enum ZoneCli {
    Node(Box<Cli<ZoneChainSpecParser, ZoneArgs>>),
    Dev(Box<DevCommand>),
    Checker(Box<CheckerCommand>),
}

impl ZoneCli {
    fn command() -> clap::Command {
        Cli::<ZoneChainSpecParser, ZoneArgs>::command()
            .about("Tempo Zone")
            .subcommand(DevCommand::command())
            .subcommand(CheckerCommand::command())
    }

    /// Parse CLI arguments from the environment.
    pub fn parse() -> Self {
        Self::parse_from(std::env::args_os())
    }

    /// Parse CLI arguments from an iterator. The first item is the binary name.
    pub fn parse_from<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        Self::try_parse_from(args).unwrap_or_else(|err| err.exit())
    }

    /// Try to parse CLI arguments from an iterator.
    pub fn try_parse_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let matches = Self::command().try_get_matches_from(args)?;
        if let Some(("dev", dev_matches)) = matches.subcommand() {
            return DevCommand::from_arg_matches(dev_matches)
                .map(Box::new)
                .map(Self::Dev);
        }
        if let Some(("checker", checker_matches)) = matches.subcommand() {
            return CheckerCommand::from_arg_matches(checker_matches)
                .map(Box::new)
                .map(Self::Checker);
        }
        Cli::from_arg_matches(&matches)
            .map(Box::new)
            .map(Self::Node)
    }

    /// Run the Tempo Zone node.
    ///
    /// Configures the node builder, launches the zone node with all sequencer
    /// background tasks, and blocks until exit.
    pub fn run(self) -> eyre::Result<()> {
        match self {
            Self::Node(cli) => run_node(*cli, NodeAction::Run),
            Self::Dev(command) => (*command).run(),
            Self::Checker(command) => (*command).run(),
        }
    }
}

/// Main entry point for the `node` command.
#[derive(Debug)]
enum NodeAction {
    Run,
    BuildCheckpoint {
        portal_creation_block_hash: alloy_primitives::B256,
        database_path: PathBuf,
    },
}

fn run_node(mut cli: Cli<ZoneChainSpecParser, ZoneArgs>, action: NodeAction) -> eyre::Result<()> {
    prepend_log_filter(&mut cli.logs.log_stdout_filter, ZONE_LOG_FILTER_DIRECTIVES);
    prepend_log_filter(&mut cli.logs.log_file_filter, ZONE_LOG_FILTER_DIRECTIVES);

    let components = |spec: Arc<ZoneChainSpec>| {
        (
            ZoneEvmConfig::new_without_l1(spec),
            NoopConsensus::default(),
        )
    };

    cli.run_with_components::<ZoneNode>(components, async move |mut builder, args| {
        info!(target: "reth::cli", "Launching Tempo Zone node");

        validate_l1_rpc_url(&args.l1_rpc_url)?;
        validate_portal_address(args.portal_address)?;
        let zone_chain_id = builder.config().chain.genesis().config.chain_id;
        args.validate_zone_id(zone_chain_id)?;
        if matches!(action, NodeAction::BuildCheckpoint { .. }) {
            eyre::ensure!(
                !args.enable_sequencer
                    && args.sequencer_key.is_none()
                    && args.sequencer_key_file.is_none()
                    && args.sequencer_manifest.is_none()
                    && args.p2p_key.is_none()
                    && args.secp256k1_key.is_none()
                    && args.sequencer_role.is_none()
                    && !args.p2p_bypass_ip_check
                    && args.deposit_decryption_keys_file.is_none(),
                "checker build-checkpoint rejects sequencer, P2P, and decryption key options"
            );
        }
        let checker_config = if matches!(action, NodeAction::BuildCheckpoint { .. }) {
            None
        } else {
            args.checker.config(
                &args.l1_rpc_url,
                args.portal_address,
                args.zone_id,
            )?
        };

        let p2p_config = args
            .sequencer_manifest
            .as_ref()
            .map(|manifest_path| {
                let ed25519_key_path = args.p2p_key.as_ref().ok_or_else(|| {
                    eyre::eyre!("--p2p.key is required with --sequencer.manifest")
                })?;
                // Required for a quorum member and rejected for an `rpc_only` node, which never
                // signs a settlement attestation; the manifest decides which this node is.
                P2pConfig::load(
                    manifest_path,
                    ed25519_key_path,
                    args.secp256k1_key.as_ref(),
                    args.p2p_listen,
                    args.p2p_bypass_ip_check,
                    args.zone_id,
                    args.sequencer_role,
                )
            })
            .transpose()?;
        if let Some(config) = p2p_config.as_ref() {
            info!(
                target: "reth::cli",
                ed25519_public_key = %config.ed25519_public_key(),
                secp256k1_address = ?config.secp256k1_address(),
                listen = %config.listen(),
                "Validated multi-sequencer manifest and local identity"
            );
        }

        let manifest_mode = p2p_config.is_some();
        if manifest_mode {
            // Replicate only durable blocks. Persist every block immediately so followers can
            // acknowledge each block without waiting for Reth's in-memory buffer to fill.
            builder.config_mut().engine.persistence_threshold = 0;
            builder.config_mut().engine.memory_block_buffer_target = Some(0);
        }
        // Every promotable node constructs all the sequencer resources: activation is gated at
        // runtime by the leadership schedule, so a quorum follower must be able to become a
        // leader without a restart. An rpc-only standby is not promotable at runtime — that
        // needs a new individual key registered with `ZonePortal` and a manifest change — so it
        // is deliberately left without the shared sequencer key, which is also the zone's ECIES
        // private key for encrypted deposits and must not sit on an internet-facing host.
        let rpc_only = p2p_config.as_ref().is_some_and(P2pConfig::is_rpc_only);
        let should_sequence_blocks = sequencer_enabled(args.enable_sequencer, p2p_config.as_ref());
        if rpc_only && (args.sequencer_key.is_some() || args.sequencer_key_file.is_some()) {
            return Err(eyre::eyre!(
                "this node is `rpc_only` in the manifest, so --sequencer-key/--sequencer-key-file must not be provided: the shared key is never used here and is also the zone ECIES private key for encrypted deposits"
            ));
        }
        let sequencer_signer = if should_sequence_blocks {
            Some(
                load_sequencer_signer(args.sequencer_key, args.sequencer_key_file.as_deref())
                    .await?,
            )
        } else {
            None
        };
        let additional_decryption_keys =
            load_decryption_keys(args.deposit_decryption_keys_file.as_deref()).await?;

        builder.config_mut().network.discovery.disable_discovery = true;
        builder.config_mut().rpc.disable_auth_server = true;
        builder.config_mut().rpc.rpc_max_logs_per_response = MAX_LOGS_PER_RESPONSE.into();
        builder.config_mut().rpc.rpc_max_blocks_per_filter = MAX_BLOCKS_PER_FILTER.into();

        let mut node = ZoneNode::new(
            args.l1_rpc_url.clone(),
            args.portal_address,
            args.l1_fetch_concurrency,
            Duration::from_millis(args.l1_retry_connection_interval_ms),
        )
        .with_withdrawal_batch_interval_blocks(args.zone_batch_interval_blocks)
        .with_redacted_rpc(ZoneRedactedRpcConfig {
            redacted_rpc_port: args.redacted_rpc_port,
            zone_id: args.zone_id,
            max_auth_token_validity: Duration::from_secs(
                args.redacted_rpc_max_auth_token_validity_secs,
            ),
        });
        if !additional_decryption_keys.is_empty() {
            node = node.with_deposit_decryption_keys(additional_decryption_keys);
        }

        if should_sequence_blocks {
            let sequencer_signer = sequencer_signer
                .expect("sequencer signer is parsed whenever sequencing is enabled");
            // `None` on an rpc-only node: it holds no individual key, and it is never the
            // scheduled leader, so it never submits an L1 settlement transaction.
            let l1_transaction_signer = p2p_config
                .as_ref()
                .and_then(P2pConfig::block_attestation_signer);
            node = node.with_sequencer(ZoneSequencerAddOnsConfig {
                sequencer_signer,
                l1_transaction_signer,
                zone_id: args.zone_id,
                zone_poll_interval: Duration::from_secs(args.zone_poll_interval_secs),
                batch_anchor_config: BatchAnchorConfig::default(),
                withdrawal_poll_interval: Duration::from_secs(args.withdrawal_poll_interval_secs),
                withdrawal_batch_limits: WithdrawalBatchLimits {
                    max_batch_gas: args.withdrawal_max_batch_gas,
                    max_in_flight_batches: args.withdrawal_max_in_flight_batches,
                },
            });
        }
        if let Some(config) = p2p_config {
            node = node.with_p2p(config);
        }

        // Install the checker ExEx only when observe mode produced a runtime config.
        if let NodeAction::BuildCheckpoint {
            portal_creation_block_hash,
            database_path,
        } = action
        {
            let config = zone_checker::CheckerConfig {
                l1_rpc_url: args.l1_rpc_url.clone(),
                portal_address: args.portal_address,
                portal_creation_block_hash,
                zone_id: args.zone_id,
                database_path: Some(database_path.clone()),
                acquisition_timeout: Duration::from_secs(30),
            };
            let node_handle = builder
                .node(node)
                .launch_with_debug_capabilities()
                .await?;
            zone_checker::build_checkpoint(
                config,
                zone_chain_id,
                node_handle.node.provider(),
                &database_path,
            )
            .await?;
            drop(node_handle);
            return Ok(());
        }

        match checker_config {
            None => {
                let handle = builder
                    .node(node)
                    .launch_with_debug_capabilities()
                    .await?;
                handle.wait_for_node_exit().await
            }
            Some(config) => {
                info!(target: "reth::cli", "Checker ExEx enabled (observe mode)");
                let checker = CheckerExEx::new(config);
                builder
                    .node(node)
                    .install_exex("zone-checker", async move |ctx| checker.launch(ctx))
                    .launch_with_debug_capabilities()
                    .await?
                    .wait_for_node_exit()
                    .await
            }
        }
    })
}

async fn load_sequencer_signer(
    inline_key: Option<String>,
    key_file: Option<&std::path::Path>,
) -> eyre::Result<PrivateKeySigner> {
    let (key, source) = match (inline_key, key_file) {
        (Some(key), None) => (Zeroizing::new(key), "--sequencer-key".to_owned()),
        (None, Some(path)) => {
            let path = path.to_path_buf();
            let source = format!("--sequencer-key-file {}", path.display());
            let display_path = path.display().to_string();
            let key = tokio::task::spawn_blocking(move || std::fs::read_to_string(path))
                .await
                .map_err(|err| {
                    eyre::eyre!("sequencer key reader task failed for {display_path}: {err}")
                })?
                .map_err(|err| {
                    eyre::eyre!("failed to read sequencer key from {display_path}: {err}")
                })?;
            (Zeroizing::new(key), source)
        }
        (Some(_), Some(_)) => {
            return Err(eyre::eyre!(
                "--sequencer-key and --sequencer-key-file are mutually exclusive"
            ));
        }
        (None, None) => {
            return Err(eyre::eyre!(
                "one of --sequencer-key or --sequencer-key-file is required"
            ));
        }
    };

    key.trim()
        .parse::<PrivateKeySigner>()
        .map_err(|_| eyre::eyre!("invalid sequencer key from {source}"))
}

async fn load_decryption_keys(
    key_file: Option<&std::path::Path>,
) -> eyre::Result<Vec<k256::SecretKey>> {
    let Some(path) = key_file else {
        return Ok(Vec::new());
    };
    let path = path.to_path_buf();
    let display_path = path.display().to_string();
    let contents = tokio::task::spawn_blocking(move || std::fs::read_to_string(path))
        .await
        .map_err(|err| eyre::eyre!("decryption key reader task failed for {display_path}: {err}"))?
        .map_err(|err| eyre::eyre!("failed to read decryption keys from {display_path}: {err}"))?;
    let contents = Zeroizing::new(contents);
    let mut keys = Vec::new();
    for (line_index, value) in contents.lines().enumerate() {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let signer = value.parse::<PrivateKeySigner>().map_err(|_| {
            eyre::eyre!(
                "invalid decryption key on line {} of {display_path}",
                line_index + 1
            )
        })?;
        keys.push(k256::SecretKey::from(signer.credential()));
    }
    eyre::ensure!(
        !keys.is_empty(),
        "decryption key file {display_path} contains no keys"
    );
    Ok(keys)
}

/// Tempo Zone CLI arguments.
#[derive(Debug, Clone, Args)]
pub struct ZoneArgs {
    /// Certified Tempo follower WebSocket RPC URL for finalized L1 state, deposit events, and chain notifications.
    #[arg(long = "l1.rpc-url", env = "L1_RPC_URL")]
    pub l1_rpc_url: String,

    /// ZonePortal contract address on L1.
    #[arg(long = "l1.portal-address", env = "L1_PORTAL_ADDRESS")]
    pub portal_address: Address,

    /// Block building interval in milliseconds.
    #[arg(
        long = "block.interval-ms",
        env = "BLOCK_INTERVAL_MS",
        default_value_t = 250
    )]
    pub block_interval_ms: u64,

    /// Shared sequencer private key (hex, with or without 0x prefix).
    ///
    /// Required by every node that can produce blocks. Requiredness is checked after the
    /// manifest is read rather than by `clap`, because an `rpc_only` node must not hold this
    /// key: it is also the zone's ECIES private key for encrypted deposits.
    #[arg(
        long = "sequencer-key",
        env = "SEQUENCER_KEY",
        value_name = "HEX",
        conflicts_with = "sequencer_key_file"
    )]
    pub sequencer_key: Option<String>,

    /// Path to a file or FIFO containing the shared sequencer private key.
    #[arg(
        long = "sequencer-key-file",
        env = "SEQUENCER_KEY_FILE",
        value_name = "PATH",
        conflicts_with = "sequencer_key"
    )]
    pub sequencer_key_file: Option<PathBuf>,

    /// File containing additional deposit decryption keys, one hex key per line.
    #[arg(
        long = "deposit-decryption-keys-file",
        env = "DEPOSIT_DECRYPTION_KEYS_FILE",
        value_name = "PATH"
    )]
    pub deposit_decryption_keys_file: Option<PathBuf>,

    /// Path to the static multi-sequencer manifest. Its presence activates
    /// multi-sequencer mode and makes the manifest authoritative for role selection.
    #[arg(
        long = "sequencer.manifest",
        env = "SEQUENCER_MANIFEST",
        value_name = "PATH",
        requires = "p2p_key",
        conflicts_with = "enable_sequencer"
    )]
    pub sequencer_manifest: Option<PathBuf>,

    /// Path to this node's hex-encoded Ed25519 P2P identity key.
    #[arg(
        long = "p2p.key",
        env = "P2P_KEY",
        value_name = "PATH",
        requires = "sequencer_manifest"
    )]
    pub p2p_key: Option<PathBuf>,

    /// Path to this node's hex-encoded individual secp256k1 private key.
    #[arg(
        long = "secp256k1.key",
        env = "SECP256K1_KEY",
        value_name = "PATH",
        requires = "sequencer_manifest"
    )]
    pub secp256k1_key: Option<PathBuf>,

    /// Socket address bound for multi-sequencer Commonware traffic.
    #[arg(
        long = "p2p.listen",
        env = "P2P_LISTEN",
        default_value = "0.0.0.0:9200"
    )]
    pub p2p_listen: SocketAddr,

    /// Disable Commonware's pre-authentication source-IP filter.
    ///
    /// Required for DNS peer addresses whose egress IPs are not known in advance.
    /// Only enable this when network-level policy restricts access to the P2P port.
    #[arg(
        long = "p2p.bypass-ip-check",
        env = "P2P_BYPASS_IP_CHECK",
        requires = "sequencer_manifest"
    )]
    pub p2p_bypass_ip_check: bool,

    /// (Optional) Checked against the role derived from the manifest.
    ///
    /// One of `leader`, `follower`, or `rpc-follower`.
    #[arg(
        long = "sequencer.role",
        env = "SEQUENCER_ROLE",
        value_name = "ROLE",
        requires = "sequencer_manifest"
    )]
    pub sequencer_role: Option<Role>,

    /// How often (in seconds) the zone monitor reconciles with the canonical head if no
    /// canonical-state notification triggers it first.
    #[arg(
        long = "zone.poll-interval-secs",
        env = "ZONE_POLL_INTERVAL_SECS",
        default_value_t = 1
    )]
    pub zone_poll_interval_secs: u64,

    /// Number of zone blocks between withdrawal batch boundaries.
    ///
    /// Default 120 is ~1 minute at Tempo's expected 500 ms block time.
    #[arg(
        long = "zone.batch-interval-blocks",
        env = "ZONE_BATCH_INTERVAL_BLOCKS",
        default_value_t = DEFAULT_WITHDRAWAL_BATCH_INTERVAL_BLOCKS
    )]
    pub zone_batch_interval_blocks: u64,

    /// How often (in seconds) the withdrawal processor polls the L1 queue.
    #[arg(
        long = "withdrawal-poll-interval-secs",
        env = "WITHDRAWAL_POLL_INTERVAL_SECS",
        default_value_t = 5
    )]
    pub withdrawal_poll_interval_secs: u64,

    /// Maximum gas reserved by one processWithdrawals transaction, up to 20,000,000. An oversized
    /// withdrawal is submitted alone.
    #[arg(
        long = "withdrawal-max-batch-gas",
        env = "WITHDRAWAL_MAX_BATCH_GAS",
        default_value_t = DEFAULT_MAX_WITHDRAWAL_BATCH_GAS,
        value_parser = clap::builder::RangedU64ValueParser::<u64>::new()
            .range(1..=MAX_WITHDRAWAL_BATCH_GAS)
    )]
    pub withdrawal_max_batch_gas: u64,

    /// Maximum number of ordered processWithdrawals transactions kept in flight.
    #[arg(
        long = "withdrawal-max-in-flight-batches",
        env = "WITHDRAWAL_MAX_IN_FLIGHT_BATCHES",
        default_value_t = DEFAULT_MAX_IN_FLIGHT_WITHDRAWAL_BATCHES,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..)
    )]
    pub withdrawal_max_in_flight_batches: usize,

    /// Maximum number of concurrent L1 receipt fetches.
    #[arg(
        long = "l1.fetch-concurrency",
        env = "L1_FETCH_CONCURRENCY",
        default_value_t = 4
    )]
    pub l1_fetch_concurrency: usize,

    /// Interval in milliseconds between WebSocket reconnection attempts to L1.
    #[arg(
        long = "l1.retry-connection-interval",
        env = "L1_RETRY_CONNECTION_INTERVAL_MS",
        default_value_t = 100
    )]
    pub l1_retry_connection_interval_ms: u64,

    /// Zone ID used for chain identity and redacted RPC authentication.
    #[arg(long = "zone.id", env = "ZONE_ID", default_value_t = 0)]
    pub zone_id: u32,

    /// Port for the redacted zone RPC server (0 for OS-assigned).
    #[arg(
        long = "redacted-rpc.port",
        alias = "private-rpc.port",
        env = "REDACTED_RPC_PORT",
        default_value_t = 8544
    )]
    pub redacted_rpc_port: u16,

    /// Maximum auth token validity window the redacted RPC accepts, in seconds.
    #[arg(
        long = "redacted-rpc.max-auth-token-validity-secs",
        env = "REDACTED_RPC_MAX_AUTH_TOKEN_VALIDITY_SECS",
        default_value_t = DEFAULT_MAX_AUTH_TOKEN_VALIDITY_SECS
    )]
    pub redacted_rpc_max_auth_token_validity_secs: u64,

    /// Enable the Zone node in sequencer mode. This advances block production and submits
    /// withdrawal batches.
    #[arg(
        long = "sequencer",
        env = "SEQUENCER",
        conflicts_with = "sequencer_manifest"
    )]
    pub enable_sequencer: bool,

    /// Durable observe-only checker configuration.
    #[command(flatten)]
    pub checker: CheckerArgs,
}

impl ZoneArgs {
    /// Assert the genesis chain ID is the one `zone.id` requires.
    ///
    /// `zone_id == 0` means "unset", which imposes no constraint.
    fn validate_zone_id(&self, chain_id: u64) -> eyre::Result<()> {
        if self.zone_id == 0 {
            return Ok(());
        }
        let expected = zone_primitives::constants::zone_chain_id(self.zone_id);
        eyre::ensure!(
            chain_id == expected,
            "chain ID mismatch: zone.id={} requires chain_id={}, but genesis has {}",
            self.zone_id,
            expected,
            chain_id,
        );
        Ok(())
    }
}

fn prepend_log_filter(filter: &mut String, directives: &str) {
    if filter.is_empty() {
        *filter = directives.to_owned();
    } else {
        *filter = format!("{directives},{filter}");
    }
}

/// Whether the sequencer add-on is configured at boot.
///
/// `rpc_only` nodes are excluded: they never produce blocks and cannot be promoted without a
/// restart, so they must not be given the shared sequencer key.
fn sequencer_enabled(cli_flag: bool, p2p_config: Option<&P2pConfig>) -> bool {
    cli_flag || p2p_config.is_some_and(|config| !config.is_rpc_only())
}

fn validate_l1_rpc_url(l1_rpc_url: &str) -> eyre::Result<()> {
    let url: url::Url = l1_rpc_url
        .parse()
        .map_err(|err| eyre::eyre!("failed parsing --l1.rpc-url as URL: {err}"))?;
    eyre::ensure!(
        matches!(url.scheme(), "ws" | "wss"),
        "--l1.rpc-url must use ws:// or wss://, got `{}`",
        url.scheme()
    );
    Ok(())
}

fn validate_portal_address(portal_address: Address) -> eyre::Result<()> {
    eyre::ensure!(
        !portal_address.is_zero(),
        "--l1.portal-address must be nonzero"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
