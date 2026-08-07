//! Checker CLI arguments and mode-dependent configuration.

use std::{ffi::OsString, path::PathBuf};

use alloy_primitives::{Address, B256};
use clap::{Args, Parser, Subcommand};
use eyre::WrapErr as _;
use reth_ethereum::cli::Cli;
use zone_chainspec::ZoneChainSpecParser;
use zone_checker::{
    CheckerConfig, CheckerMode,
    diagnostic::{DiagnosticModelKey, diagnose_retained_model_change},
};

use super::{NodeAction, ZoneArgs, run_node};

/// Offline, read-only checker operator commands.
#[derive(Debug, Parser)]
#[command(name = "checker", about = "Inspect a durable checker database")]
pub struct CheckerCommand {
    #[command(subcommand)]
    command: CheckerSubcommand,
}

#[derive(Debug, Subcommand)]
enum CheckerSubcommand {
    /// Build and atomically publish a compact checkpoint using a local Zone node.
    BuildCheckpoint(CheckerBuildCheckpointArgs),
    /// Reconstruct one model key across a retained canonical Zone block.
    Diagnose(CheckerDiagnoseArgs),
}

/// Checker checkpoint destination and ordinary node arguments.
#[derive(Debug, Args)]
struct CheckerBuildCheckpointArgs {
    /// L1 block hash containing the authenticated `ZoneCreated` event.
    #[arg(long = "checker.portal-creation-block-hash", value_name = "HASH")]
    portal_creation_block_hash: B256,

    /// Destination path for the atomically published compact checkpoint.
    #[arg(long = "checker.database-path", value_name = "PATH")]
    database_path: PathBuf,

    /// Standard node arguments after `--`, including `--chain`, `--datadir`,
    /// `--l1.rpc-url`, `--l1.portal-address`, and `--zone.id`.
    #[arg(last = true, required = true, value_name = "NODE_ARGS")]
    node_args: Vec<OsString>,
}

/// Exact retained boundary and selected model key to inspect.
#[derive(Debug, Args)]
struct CheckerDiagnoseArgs {
    /// Exact checker MDBX path. Stop the Zone node first, or use a consistent copied database.
    #[arg(long, value_name = "PATH")]
    database_path: PathBuf,

    /// Retained canonical Zone height whose parent/child boundary is shown.
    #[arg(long, value_name = "HEIGHT")]
    zone_height: u64,

    /// Typed model key, for example `token:0x...` or `withdrawal:7`.
    #[arg(long, value_name = "MODEL_KEY")]
    key: DiagnosticModelKey,
}

impl CheckerCommand {
    pub(super) fn run(self) -> eyre::Result<()> {
        match self.command {
            CheckerSubcommand::BuildCheckpoint(args) => {
                let cli = Cli::<ZoneChainSpecParser, ZoneArgs>::try_parse_from(
                    std::iter::once(OsString::from("zone-node")).chain(args.node_args),
                )?;
                run_node(
                    cli,
                    NodeAction::BuildCheckpoint {
                        portal_creation_block_hash: args.portal_creation_block_hash,
                        database_path: args.database_path,
                    },
                )
            }
            CheckerSubcommand::Diagnose(args) => {
                let report = diagnose_retained_model_change(
                    args.database_path,
                    args.zone_height,
                    args.key,
                )
                .wrap_err(
                    "offline checker diagnosis failed; stop the Zone node before inspecting its \
                     MDBX database, or diagnose a consistent copy",
                )?;
                print!("{report}");
                Ok(())
            }
        }
    }
}

/// Durable observe-only checker arguments.
#[derive(Debug, Clone, Args)]
pub struct CheckerArgs {
    /// Checker ExEx mode: `off` (default) or `observe`.
    #[arg(
        long = "checker.mode",
        env = "CHECKER_MODE",
        default_value = "off",
        value_parser = CheckerMode::parse,
    )]
    pub mode: CheckerMode,

    /// L1 block hash containing the authenticated `ZoneCreated` event for this portal.
    #[arg(
        long = "checker.portal-creation-block-hash",
        env = "CHECKER_PORTAL_CREATION_BLOCK_HASH",
        value_name = "HASH"
    )]
    pub portal_creation_block_hash: Option<B256>,

    /// Override the path to the checker's dedicated database.
    #[arg(
        long = "checker.database-path",
        env = "CHECKER_DATABASE_PATH",
        value_name = "PATH"
    )]
    pub database_path: Option<PathBuf>,
}

impl CheckerArgs {
    /// Validate the selected mode and construct its complete runtime config.
    pub(super) fn config(
        &self,
        l1_rpc_url: &str,
        portal_address: Address,
        zone_id: u32,
    ) -> eyre::Result<Option<CheckerConfig>> {
        match self.mode {
            CheckerMode::Off => {
                eyre::ensure!(
                    self.database_path.is_none(),
                    "--checker.database-path requires --checker.mode observe"
                );
                eyre::ensure!(
                    self.portal_creation_block_hash.is_none(),
                    "--checker.portal-creation-block-hash requires --checker.mode observe"
                );
                Ok(None)
            }
            CheckerMode::Observe => {
                eyre::ensure!(
                    zone_id != 0,
                    "--zone.id must be nonzero with --checker.mode observe"
                );
                let portal_creation_block_hash =
                    self.portal_creation_block_hash.ok_or_else(|| {
                        eyre::eyre!(
                            "--checker.portal-creation-block-hash is required with --checker.mode observe"
                        )
                    })?;
                eyre::ensure!(
                    !portal_creation_block_hash.is_zero(),
                    "--checker.portal-creation-block-hash must be nonzero"
                );
                Ok(Some(CheckerConfig {
                    l1_rpc_url: l1_rpc_url.to_owned(),
                    portal_address,
                    portal_creation_block_hash,
                    zone_id,
                    database_path: self.database_path.clone(),
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use alloy_primitives::{Address, B256};
    use clap::Parser as _;
    use zone_checker::CheckerMode;

    use super::{CheckerArgs, CheckerCommand, CheckerSubcommand};

    const CREATION_HASH: &str =
        "0x1111111111111111111111111111111111111111111111111111111111111111";
    const PORTAL: Address = Address::repeat_byte(0x22);

    #[derive(Debug, clap::Parser)]
    struct Parser {
        #[command(flatten)]
        checker: CheckerArgs,
    }

    fn parse(arguments: impl IntoIterator<Item = &'static str>) -> CheckerArgs {
        Parser::try_parse_from(arguments).unwrap().checker
    }

    #[test]
    fn defaults_to_off_without_a_runtime_config() {
        let args = parse(["tempo-zone"]);

        assert_eq!(args.mode, CheckerMode::Off);
        assert!(
            args.config("ws://localhost:8546", PORTAL, 0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn observe_builds_the_complete_runtime_config() {
        let args = parse([
            "tempo-zone",
            "--checker.mode",
            "observe",
            "--checker.portal-creation-block-hash",
            CREATION_HASH,
            "--checker.database-path",
            "checker-test-db",
        ]);

        let config = args
            .config("ws://localhost:8546", PORTAL, 7)
            .unwrap()
            .unwrap();
        assert_eq!(config.l1_rpc_url, "ws://localhost:8546");
        assert_eq!(config.portal_address, PORTAL);
        assert_eq!(
            config.portal_creation_block_hash,
            CREATION_HASH.parse::<B256>().unwrap()
        );
        assert_eq!(config.zone_id, 7);
        assert_eq!(config.database_path, Some(PathBuf::from("checker-test-db")));
    }

    #[test]
    fn observe_requires_a_creation_hash() {
        let args = parse(["tempo-zone", "--checker.mode", "observe"]);

        let error = args.config("ws://localhost:8546", PORTAL, 7).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("--checker.portal-creation-block-hash is required")
        );
    }

    #[test]
    fn observe_requires_a_nonzero_creation_hash() {
        let args = parse([
            "tempo-zone",
            "--checker.mode",
            "observe",
            "--checker.portal-creation-block-hash",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
        ]);

        let error = args.config("ws://localhost:8546", PORTAL, 7).unwrap_err();
        assert!(error.to_string().contains("must be nonzero"));
    }

    #[test]
    fn observe_requires_a_nonzero_zone_id() {
        let args = parse([
            "tempo-zone",
            "--checker.mode",
            "observe",
            "--checker.portal-creation-block-hash",
            CREATION_HASH,
        ]);

        let error = args.config("ws://localhost:8546", PORTAL, 0).unwrap_err();
        assert!(error.to_string().contains("--zone.id must be nonzero"));
    }

    #[test]
    fn checker_only_options_require_observe_mode() {
        for (option, value) in [
            ("--checker.database-path", "checker-test-db"),
            ("--checker.portal-creation-block-hash", CREATION_HASH),
        ] {
            let args = parse(["tempo-zone", option, value]);
            let error = args.config("ws://localhost:8546", PORTAL, 7).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("requires --checker.mode observe")
            );
        }
    }

    #[test]
    fn malformed_creation_hash_is_rejected_by_clap() {
        assert!(
            Parser::try_parse_from([
                "tempo-zone",
                "--checker.mode",
                "observe",
                "--checker.portal-creation-block-hash",
                "not-a-hash",
            ])
            .is_err()
        );
    }

    #[test]
    fn unknown_mode_is_rejected_by_clap() {
        assert!(Parser::try_parse_from(["tempo-zone", "--checker.mode", "enforce"]).is_err());
    }

    #[test]
    fn diagnostic_command_requires_a_typed_key_and_retained_height() {
        assert!(
            CheckerCommand::try_parse_from([
                "checker",
                "diagnose",
                "--database-path",
                "checker-db",
                "--zone-height",
                "7",
                "--key",
                "withdrawal:4",
            ])
            .is_ok()
        );
        assert!(
            CheckerCommand::try_parse_from([
                "checker",
                "diagnose",
                "--database-path",
                "checker-db",
                "--zone-height",
                "7",
                "--key",
                "withdrawal:not-a-number",
            ])
            .is_err()
        );
    }

    #[test]
    fn build_checkpoint_requires_checker_fields_and_trailing_node_arguments() {
        let valid = CheckerCommand::try_parse_from([
            "checker",
            "build-checkpoint",
            "--checker.portal-creation-block-hash",
            CREATION_HASH,
            "--checker.database-path",
            "checkpoint",
            "--",
            "--chain",
            "dev",
            "--l1.rpc-url",
            "ws://localhost:8546",
            "--l1.portal-address",
            "0x0000000000000000000000000000000000000001",
            "--zone.id",
            "1",
        ])
        .unwrap();
        assert!(matches!(
            valid.command,
            CheckerSubcommand::BuildCheckpoint(_)
        ));

        for omitted in [
            "--checker.portal-creation-block-hash",
            "--checker.database-path",
        ] {
            let mut args = vec![
                "checker",
                "build-checkpoint",
                "--checker.portal-creation-block-hash",
                CREATION_HASH,
                "--checker.database-path",
                "checkpoint",
                "--",
                "--chain",
                "dev",
            ];
            let index = args.iter().position(|arg| *arg == omitted).unwrap();
            args.drain(index..=index + 1);
            assert_eq!(
                CheckerCommand::try_parse_from(args).unwrap_err().kind(),
                clap::error::ErrorKind::MissingRequiredArgument
            );
        }
    }

    #[test]
    fn diagnostic_help_requires_an_offline_or_copied_database() {
        let error = CheckerCommand::try_parse_from(["checker", "diagnose", "--help"])
            .expect_err("--help exits through clap");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        let help = error.to_string();
        assert!(help.contains("Stop the Zone node first"));
        assert!(help.contains("consistent copied database"));
    }
}
