use alloy_primitives::{Address, Bytes};
use tempo_zone_contracts::{
    ZONE_MESSENGER_ADDRESS, ZONE_PORTAL_IMPL_ADDRESS, ZONE_VERIFIER_ADDRESS,
};

#[derive(Clone, Copy, Debug)]
pub(super) enum RuntimeKind {
    Portal,
    Messenger,
    Verifier,
}

#[derive(Debug)]
pub(super) struct NativeRuntime {
    pub(super) kind: RuntimeKind,
    pub(super) name: &'static str,
    pub(super) target: Address,
    pub(super) code: Bytes,
}

pub(super) fn bundled() -> eyre::Result<[NativeRuntime; 3]> {
    Ok([
        decode(
            RuntimeKind::Portal,
            "ZonePortal",
            ZONE_PORTAL_IMPL_ADDRESS,
            include_str!("../../assets/zone-portal-runtime.hex"),
        )?,
        decode(
            RuntimeKind::Messenger,
            "ZoneMessenger",
            ZONE_MESSENGER_ADDRESS,
            include_str!("../../assets/zone-messenger-runtime.hex"),
        )?,
        decode(
            RuntimeKind::Verifier,
            "Verifier",
            ZONE_VERIFIER_ADDRESS,
            include_str!("../../assets/verifier-runtime.hex"),
        )?,
    ])
}

fn decode(
    kind: RuntimeKind,
    name: &'static str,
    target: Address,
    encoded: &str,
) -> eyre::Result<NativeRuntime> {
    Ok(NativeRuntime {
        kind,
        name,
        target,
        code: alloy_primitives::hex::decode(encoded.trim())?.into(),
    })
}

/// Wraps a deployed runtime in initcode that returns it unchanged.
pub(super) fn deployment_bytecode(runtime: &[u8]) -> eyre::Result<Bytes> {
    const PREFIX_LENGTH: u16 = 15;

    let length = u16::try_from(runtime.len())
        .map_err(|_| eyre::eyre!("native zone runtime is too large to deploy"))?;
    let [length_hi, length_lo] = length.to_be_bytes();
    let [offset_hi, offset_lo] = PREFIX_LENGTH.to_be_bytes();
    let mut bytecode = Vec::with_capacity(usize::from(PREFIX_LENGTH) + runtime.len());
    bytecode.extend_from_slice(&[
        0x61, length_hi, length_lo, // PUSH2 runtime length
        0x61, offset_hi, offset_lo, // PUSH2 runtime offset
        0x60, 0x00, // PUSH1 destination offset
        0x39, // CODECOPY
        0x61, length_hi, length_lo, // PUSH2 runtime length
        0x60, 0x00, // PUSH1 return offset
        0xf3, // RETURN
    ]);
    bytecode.extend_from_slice(runtime);
    Ok(bytecode.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_bytecode_wraps_runtime() {
        let bytecode = deployment_bytecode(&[0xfe, 0xed]).unwrap();
        assert_eq!(
            bytecode.as_ref(),
            [
                0x61, 0x00, 0x02, 0x61, 0x00, 0x0f, 0x60, 0x00, 0x39, 0x61, 0x00, 0x02, 0x60, 0x00,
                0xf3, 0xfe, 0xed,
            ]
        );
    }

    #[test]
    fn bundled_runtimes_match_foundry_artifacts() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        for runtime in bundled().unwrap() {
            let path = manifest
                .join("../../specs/ref-impls/out")
                .join(format!("{}.sol/{}.json", runtime.name, runtime.name));
            let artifact: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&path)
                    .unwrap_or_else(|err| panic!("failed reading {}: {err}", path.display())),
            )
            .unwrap_or_else(|err| panic!("failed parsing {}: {err}", path.display()));
            let encoded = artifact["deployedBytecode"]["object"]
                .as_str()
                .unwrap_or_else(|| panic!("{} has no deployed bytecode", path.display()));
            let expected = alloy_primitives::hex::decode(encoded).unwrap();

            assert_eq!(
                runtime.code.as_ref(),
                expected,
                "{} runtime asset is stale; run `just regen-native-zone-runtimes`",
                runtime.name
            );
        }
    }
}
