//! ABI compatibility checker between Zone Rust bindings and Solidity interfaces.

use std::path::PathBuf;

use eyre::bail;
use tempo_precompiles_conformance::test_util::{
    abi_conformance::{AbiSurface, compare_abi},
    foundry_artifact_path,
};

struct InterfaceSpec {
    name: &'static str,
    artifact_name: &'static str,
    source: &'static str,
    rust: fn() -> AbiSurface,
    ignored_rust_functions: &'static [&'static str],
    ignored_functions: &'static [&'static str],
}

macro_rules! interface {
    ($name:ident, $artifact:literal) => {
        InterfaceSpec {
            name: stringify!($name),
            artifact_name: $artifact,
            source: "IZone.sol",
            rust: || AbiSurface::from_abi(&tempo_zone_contracts::$name::abi::contract()),
            ignored_rust_functions: &[],
            ignored_functions: &[],
        }
    };
}

const INTERFACES: &[InterfaceSpec] = &[
    InterfaceSpec {
        name: "TempoState",
        artifact_name: "ITempoState",
        source: "IZone.sol",
        rust: || AbiSurface::from_abi(&tempo_zone_contracts::TempoState::abi::contract()),
        ignored_rust_functions: &[],
        ignored_functions: &[
            "function readTempoStorageSlot(address account, bytes32 slot) view returns (bytes32)",
            "function readTempoStorageSlots(address account, bytes32[] slots) view returns (bytes32[])",
        ],
    },
    InterfaceSpec {
        name: "IZoneInbox",
        artifact_name: "IZoneInbox",
        source: "IZone.sol",
        rust: || AbiSurface::from_abi(&tempo_zone_contracts::IZoneInbox::abi::contract()),
        // Alloy's generated JSON ABI currently omits names from a nested struct's components.
        ignored_rust_functions: &[concat!(
            "function advanceTempo(bytes header, ",
            "tuple(uint8 depositType, bytes depositData, bool rejected)[] deposits, ",
            "tuple(bytes32 sharedSecret, uint8 sharedSecretYParity, ",
            "tuple(bytes32, bytes32) cpProof)[] decryptions, ",
            "tuple(address token, string name, string symbol, string currency)[] enabledTokens)"
        )],
        ignored_functions: &[concat!(
            "function advanceTempo(bytes header, ",
            "tuple(uint8 depositType, bytes depositData, bool rejected)[] deposits, ",
            "tuple(bytes32 sharedSecret, uint8 sharedSecretYParity, ",
            "tuple(bytes32 s, bytes32 c) cpProof)[] decryptions, ",
            "tuple(address token, string name, string symbol, string currency)[] enabledTokens)"
        )],
    },
    interface!(IZoneOutbox, "IZoneOutbox"),
    interface!(ZoneFactory, "IZoneFactory"),
    interface!(ZonePortal, "IZonePortal"),
];

#[derive(Debug, clap::Args)]
pub(crate) struct CheckAbi {
    /// Foundry output directory produced from `specs/ref-impls`.
    #[arg(long, default_value = "specs/ref-impls/out")]
    artifacts: PathBuf,
}

impl CheckAbi {
    pub(crate) fn run(self) -> eyre::Result<()> {
        let mut failed = false;
        for spec in INTERFACES {
            let path = foundry_artifact_path(&self.artifacts, spec.source, spec.artifact_name);
            let mut rust = (spec.rust)();
            rust.remove_functions(spec.ignored_rust_functions);
            let errors = compare_abi(&path, &rust, spec.ignored_functions)
                .err()
                .unwrap_or_default();
            if errors.is_empty() {
                eprintln!("  ✓  {}", spec.name);
                continue;
            }
            failed = true;
            eprintln!("  ✗  {}", spec.name);
            for error in errors {
                eprintln!("    {error}");
            }
        }
        if failed {
            bail!("Zone ABI compatibility check found differences");
        }
        Ok(())
    }
}
