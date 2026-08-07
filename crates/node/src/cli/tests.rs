use std::{io::Write as _, process::Command, thread, time::Duration};

use clap::Parser as _;

use super::{
    Role, ZoneArgs, load_decryption_keys, load_sequencer_signer, sequencer_enabled,
    validate_l1_rpc_url, validate_p2p_transaction_size_limit, validate_portal_address,
};
use zone_sequencer::MAX_WITHDRAWAL_BATCH_GAS;

mod subcommands;

#[derive(Debug, clap::Parser)]
struct ZoneArgsParser {
    #[command(flatten)]
    zone: ZoneArgs,
}

#[test]
fn portal_address_must_be_nonzero() {
    assert!(validate_portal_address(alloy_primitives::Address::ZERO).is_err());
    assert!(validate_portal_address(alloy_primitives::Address::repeat_byte(0x11)).is_ok());
}

#[test]
fn zone_id_must_match_genesis_chain_id() {
    let args = ZoneArgsParser::try_parse_from([
        "tempo-zone",
        "--l1.rpc-url",
        "ws://localhost:8546",
        "--l1.portal-address",
        "0x0000000000000000000000000000000000000001",
        "--zone.id",
        "7",
    ])
    .unwrap()
    .zone;
    let expected = zone_primitives::constants::zone_chain_id(args.zone_id);

    assert!(args.validate_zone_id(expected).is_ok());
    assert!(args.validate_zone_id(expected + 1).is_err());
}

#[test]
fn manifest_mode_rejects_a_txpool_limit_above_the_p2p_wire_limit() {
    assert!(
        validate_p2p_transaction_size_limit(true, zone_p2p::MAX_TRANSACTION_MESSAGE_SIZE).is_ok()
    );
    assert!(
        validate_p2p_transaction_size_limit(true, zone_p2p::MAX_TRANSACTION_MESSAGE_SIZE + 1)
            .is_err()
    );
    assert!(
        validate_p2p_transaction_size_limit(false, zone_p2p::MAX_TRANSACTION_MESSAGE_SIZE + 1)
            .is_ok()
    );
}

#[test]
fn sequencer_key_file_is_accepted_and_conflicts_with_inline_key() {
    let common = [
        "tempo-zone",
        "--l1.rpc-url",
        "ws://localhost:8546",
        "--l1.portal-address",
        "0x0000000000000000000000000000000000000001",
    ];

    let parsed = ZoneArgsParser::try_parse_from(
        common
            .into_iter()
            .chain(["--sequencer-key-file", "/run/secrets/sequencer-key"]),
    )
    .unwrap();
    assert_eq!(
        parsed.zone.sequencer_key_file.as_deref(),
        Some(std::path::Path::new("/run/secrets/sequencer-key"))
    );
    assert!(parsed.zone.sequencer_key.is_none());

    let error = ZoneArgsParser::try_parse_from(common.into_iter().chain([
        "--sequencer-key",
        "0x01",
        "--sequencer-key-file",
        "/run/secrets/sequencer-key",
    ]))
    .unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[tokio::test(flavor = "current_thread")]
async fn loads_sequencer_key_from_file_with_trailing_newline() {
    let path =
        std::env::temp_dir().join(format!("tempo-zone-sequencer-key-{}", std::process::id()));
    std::fs::write(
        &path,
        "0000000000000000000000000000000000000000000000000000000000000001\n",
    )
    .unwrap();

    let signer = load_sequencer_signer(None, Some(&path)).await.unwrap();
    std::fs::remove_file(path).unwrap();

    assert_eq!(
        signer.address(),
        "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"
            .parse::<alloy_primitives::Address>()
            .unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn loads_additional_decryption_keys_one_per_line() {
    let path =
        std::env::temp_dir().join(format!("tempo-zone-decryption-keys-{}", std::process::id()));
    std::fs::write(
        &path,
        concat!(
            "0000000000000000000000000000000000000000000000000000000000000001\n",
            "\n",
            "0000000000000000000000000000000000000000000000000000000000000002\n"
        ),
    )
    .unwrap();

    let keys = load_decryption_keys(Some(&path)).await.unwrap();
    std::fs::remove_file(path).unwrap();

    assert_eq!(keys.len(), 2);
    assert_eq!(
        keys[0].to_bytes().as_slice(),
        alloy_primitives::B256::with_last_byte(1).as_slice()
    );
    assert_eq!(
        keys[1].to_bytes().as_slice(),
        alloy_primitives::B256::with_last_byte(2).as_slice()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn loads_sequencer_key_from_fifo() {
    let path = std::env::temp_dir().join(format!(
        "tempo-zone-sequencer-key-{}.fifo",
        std::process::id()
    ));
    let status = Command::new("mkfifo")
        .args(["-m", "600"])
        .arg(&path)
        .status()
        .expect("mkfifo must be available");
    assert!(status.success(), "mkfifo failed: {status}");

    let writer_path = path.clone();
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        let mut fifo = std::fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .unwrap();
        writeln!(
            fifo,
            "0000000000000000000000000000000000000000000000000000000000000001"
        )
        .unwrap();
    });

    let signer = tokio::time::timeout(
        Duration::from_secs(2),
        load_sequencer_signer(None, Some(&path)),
    )
    .await
    .expect("FIFO read timed out")
    .unwrap();
    writer.join().unwrap();
    std::fs::remove_file(path).unwrap();

    assert_eq!(
        signer.address(),
        "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"
            .parse::<alloy_primitives::Address>()
            .unwrap()
    );
}

#[test]
fn manifest_mode_requires_the_p2p_key_and_conflicts_with_legacy_sequencer() {
    let common = [
        "tempo-zone",
        "--l1.rpc-url",
        "ws://localhost:8546",
        "--l1.portal-address",
        "0x0000000000000000000000000000000000000001",
        "--sequencer-key",
        "0x01",
    ];

    let missing_key = ZoneArgsParser::try_parse_from(
        common
            .into_iter()
            .chain(["--sequencer.manifest", "zone.toml"]),
    )
    .unwrap_err();
    assert_eq!(
        missing_key.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );

    // `--secp256k1.key` is deliberately not a parse-time requirement: an `rpc_only` node
    // holds no individual key. Whether one is expected is decided against the manifest in
    // `ZoneManifest::validate_node`.
    let without_secp256k1_key = ZoneArgsParser::try_parse_from(common.into_iter().chain([
        "--sequencer.manifest",
        "zone.toml",
        "--p2p.key",
        "node.key",
    ]))
    .unwrap();
    assert_eq!(without_secp256k1_key.zone.secp256k1_key, None);

    let conflict = ZoneArgsParser::try_parse_from(common.into_iter().chain([
        "--sequencer.manifest",
        "zone.toml",
        "--p2p.key",
        "node.key",
        "--secp256k1.key",
        "node-secp256k1.key",
        "--sequencer",
    ]))
    .unwrap_err();
    assert_eq!(conflict.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn legacy_mode_still_accepts_the_sequencer_flag_without_a_manifest() {
    let parsed = ZoneArgsParser::try_parse_from([
        "tempo-zone",
        "--l1.rpc-url",
        "ws://localhost:8546",
        "--l1.portal-address",
        "0x0000000000000000000000000000000000000001",
        "--sequencer-key",
        "0x01",
        "--sequencer",
    ])
    .unwrap();
    assert!(parsed.zone.enable_sequencer);
    assert!(parsed.zone.sequencer_manifest.is_none());
}

#[test]
fn zone_poll_interval_keeps_one_second_default_and_accepts_override() {
    let common = [
        "tempo-zone",
        "--l1.rpc-url",
        "ws://localhost:8546",
        "--l1.portal-address",
        "0x0000000000000000000000000000000000000001",
        "--sequencer-key",
        "0x01",
    ];

    let default = ZoneArgsParser::try_parse_from(common).unwrap();
    assert_eq!(default.zone.zone_poll_interval_secs, 1);

    let overridden = ZoneArgsParser::try_parse_from(
        common.into_iter().chain(["--zone.poll-interval-secs", "3"]),
    )
    .unwrap();
    assert_eq!(overridden.zone.zone_poll_interval_secs, 3);
}

#[test]
fn private_rpc_port_alias_is_accepted() {
    let common = [
        "tempo-zone",
        "--l1.rpc-url",
        "ws://localhost:8546",
        "--l1.portal-address",
        "0x0000000000000000000000000000000000000001",
    ];

    let redacted =
        ZoneArgsParser::try_parse_from(common.into_iter().chain(["--redacted-rpc.port", "9544"]))
            .unwrap();
    let private =
        ZoneArgsParser::try_parse_from(common.into_iter().chain(["--private-rpc.port", "9544"]))
            .unwrap();

    assert_eq!(redacted.zone.redacted_rpc_port, 9544);
    assert_eq!(private.zone.redacted_rpc_port, 9544);
}

#[test]
fn withdrawal_batch_gas_rejects_values_above_the_safe_limit() {
    let above_limit = (MAX_WITHDRAWAL_BATCH_GAS + 1).to_string();
    let error = ZoneArgsParser::try_parse_from([
        "tempo-zone",
        "--l1.rpc-url",
        "ws://localhost:8546",
        "--l1.portal-address",
        "0x0000000000000000000000000000000000000001",
        "--sequencer-key",
        "0x01",
        "--withdrawal-max-batch-gas",
        &above_limit,
    ])
    .unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn p2p_ip_check_bypass_is_explicit_and_requires_manifest_mode() {
    let common = [
        "tempo-zone",
        "--l1.rpc-url",
        "ws://localhost:8546",
        "--l1.portal-address",
        "0x0000000000000000000000000000000000000001",
        "--sequencer-key",
        "0x01",
    ];

    let without_manifest =
        ZoneArgsParser::try_parse_from(common.into_iter().chain(["--p2p.bypass-ip-check"]))
            .unwrap_err();
    assert_eq!(
        without_manifest.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );

    let default = ZoneArgsParser::try_parse_from(common).unwrap();
    assert!(!default.zone.p2p_bypass_ip_check);

    let enabled = ZoneArgsParser::try_parse_from(common.into_iter().chain([
        "--sequencer.manifest",
        "zone.toml",
        "--p2p.key",
        "node.key",
        "--secp256k1.key",
        "node-secp256k1.key",
        "--p2p.bypass-ip-check",
    ]))
    .unwrap();
    assert!(enabled.zone.p2p_bypass_ip_check);
}

#[test]
fn sequencer_resources_follow_the_cli_flag_without_a_manifest() {
    // Manifest mode is covered by `sequencer_enabled`'s other arm, which needs a real
    // `P2pConfig`; see `manifest_mode_configures_sequencer_resources_except_on_standbys`
    // in the integration tests.
    assert!(sequencer_enabled(true, None));
    assert!(!sequencer_enabled(false, None));
}

#[test]
fn sequencer_role_argument_accepts_the_rpc_follower_spelling() {
    let parsed = ZoneArgsParser::try_parse_from([
        "tempo-zone",
        "--l1.rpc-url",
        "ws://localhost:8546",
        "--l1.portal-address",
        "0x0000000000000000000000000000000000000001",
        "--sequencer.manifest",
        "zone.toml",
        "--p2p.key",
        "node.key",
        "--sequencer.role",
        "rpc-follower",
    ])
    .unwrap();
    assert_eq!(parsed.zone.sequencer_role, Some(Role::RpcFollower));
    // A standby holds neither key. Requiredness is checked against the manifest after it is
    // read, so neither flag is a parse-time requirement.
    assert_eq!(parsed.zone.secp256k1_key, None);
    assert_eq!(parsed.zone.sequencer_key, None);
    assert_eq!(parsed.zone.sequencer_key_file, None);
}

#[test]
fn l1_rpc_url_accepts_websocket_schemes() {
    validate_l1_rpc_url("ws://localhost:8546").unwrap();
    validate_l1_rpc_url("wss://rpc.moderato.tempo.xyz").unwrap();
}

#[test]
fn l1_rpc_url_rejects_non_websocket_schemes() {
    assert!(validate_l1_rpc_url("http://localhost:8545").is_err());
    assert!(validate_l1_rpc_url("https://rpc.moderato.tempo.xyz").is_err());
}
