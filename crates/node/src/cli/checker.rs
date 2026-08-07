//! Checker CLI arguments and mode-dependent configuration.

use std::path::PathBuf;

use alloy_primitives::{Address, B256};
use clap::Args;
use zone_checker::{CheckerConfig, CheckerMode};

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

    use super::CheckerArgs;

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
}
