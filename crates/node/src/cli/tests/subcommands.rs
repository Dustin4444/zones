use super::super::ZoneCli;

#[test]
fn top_level_help_lists_operator_subcommands() {
    let result = ZoneCli::try_parse_from(["tempo-zone", "--help"]);
    let error = result.err().expect("--help exits through clap");
    assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    assert!(error.to_string().contains("  dev"));
    assert!(error.to_string().contains("  checker"));
}

#[test]
fn dev_is_parsed_by_the_top_level_cli() {
    let parsed = ZoneCli::try_parse_from(["tempo-zone", "dev"]).unwrap();
    assert!(matches!(parsed, ZoneCli::Dev(_)));
}

#[test]
fn checker_is_parsed_by_the_top_level_cli() {
    let parsed = ZoneCli::try_parse_from([
        "tempo-zone",
        "checker",
        "diagnose",
        "--database-path",
        "checker-db",
        "--zone-height",
        "1",
        "--key",
        "portal-config",
    ])
    .unwrap();
    assert!(matches!(parsed, ZoneCli::Checker(_)));
}
