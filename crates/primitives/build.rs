use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use alloy_primitives::{B256, U256};
use serde_json::Value;

const TEMPO_STATE_ARTIFACT: &str = "../../specs/ref-impls/out/TempoState.sol/TempoState.json";
const ZONE_INBOX_ARTIFACT: &str = "../../specs/ref-impls/out/ZoneInbox.sol/ZoneInbox.json";
const ZONE_OUTBOX_ARTIFACT: &str = "../../specs/ref-impls/out/ZoneOutbox.sol/ZoneOutbox.json";
const ZONE_PORTAL_ARTIFACT: &str = "../../specs/ref-impls/out/ZonePortal.sol/ZonePortal.json";

fn main() -> io::Result<()> {
    let manifest_dir = path_from_env("CARGO_MANIFEST_DIR")?;
    let out_dir = path_from_env("OUT_DIR")?;
    let tempo_state_artifact_path = manifest_dir.join(TEMPO_STATE_ARTIFACT);
    let zone_inbox_artifact_path = manifest_dir.join(ZONE_INBOX_ARTIFACT);
    let zone_outbox_artifact_path = manifest_dir.join(ZONE_OUTBOX_ARTIFACT);
    let zone_portal_artifact_path = manifest_dir.join(ZONE_PORTAL_ARTIFACT);

    println!(
        "cargo:rerun-if-changed={}",
        tempo_state_artifact_path.display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        zone_inbox_artifact_path.display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        zone_outbox_artifact_path.display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        zone_portal_artifact_path.display()
    );

    let tempo_state_artifact = load_json(&tempo_state_artifact_path)?;
    let zone_inbox_artifact = load_json(&zone_inbox_artifact_path)?;
    let zone_outbox_artifact = load_json(&zone_outbox_artifact_path)?;
    let zone_portal_artifact = load_json(&zone_portal_artifact_path)?;
    let tempo_block_hash_slot =
        storage_key(&tempo_state_artifact, "tempoBlockHash", 0, "bytes32", "32")?;
    let tempo_wrapper_gas_limits_slot =
        storage_key(&tempo_state_artifact, "generalGasLimit", 0, "uint64", "8")?;
    let tempo_parent_hash_slot =
        storage_key(&tempo_state_artifact, "tempoParentHash", 0, "bytes32", "32")?;
    let tempo_beneficiary_slot = storage_key(
        &tempo_state_artifact,
        "tempoBeneficiary",
        0,
        "address",
        "20",
    )?;
    let tempo_state_root_slot =
        storage_key(&tempo_state_artifact, "tempoStateRoot", 0, "bytes32", "32")?;
    let tempo_transactions_root_slot = storage_key(
        &tempo_state_artifact,
        "tempoTransactionsRoot",
        0,
        "bytes32",
        "32",
    )?;
    let tempo_receipts_root_slot = storage_key(
        &tempo_state_artifact,
        "tempoReceiptsRoot",
        0,
        "bytes32",
        "32",
    )?;
    let tempo_packed_slot =
        storage_key(&tempo_state_artifact, "tempoBlockNumber", 0, "uint64", "8")?;
    let tempo_timestamp_millis_slot = storage_key(
        &tempo_state_artifact,
        "tempoTimestampMillis",
        0,
        "uint64",
        "8",
    )?;
    let tempo_prev_randao_slot =
        storage_key(&tempo_state_artifact, "tempoPrevRandao", 0, "bytes32", "32")?;
    let inbox_processed_hash_slot = storage_slot_u256(
        &zone_inbox_artifact,
        "processedDepositQueueHash",
        0,
        "bytes32",
        "32",
    )?;
    let inbox_processed_number_slot = storage_slot_u256(
        &zone_inbox_artifact,
        "processedDepositNumber",
        0,
        "uint64",
        "8",
    )?;
    let outbox_last_batch_hash_slot = struct_member_slot_u256(
        &zone_outbox_artifact,
        "_lastBatch",
        "withdrawalQueueHash",
        0,
        "bytes32",
        "32",
    )?;
    let outbox_last_batch_index_slot = struct_member_slot_u256(
        &zone_outbox_artifact,
        "_lastBatch",
        "withdrawalBatchIndex",
        0,
        "uint64",
        "8",
    )?;
    let portal_current_deposit_queue_hash_slot = storage_key(
        &zone_portal_artifact,
        "currentDepositQueueHash",
        0,
        "bytes32",
        "32",
    )?;

    let generated = format!(
        "\
/// TempoState storage slot for `tempoBlockHash`, generated from the Foundry storage layout.
pub const TEMPO_BLOCK_HASH_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});

/// TempoState storage slot containing `generalGasLimit` and `sharedGasLimit`, generated from the Foundry storage layout.
pub const TEMPO_WRAPPER_GAS_LIMITS_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});

/// TempoState storage slot for `tempoParentHash`, generated from the Foundry storage layout.
pub const TEMPO_PARENT_HASH_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});

/// TempoState storage slot for `tempoBeneficiary`, generated from the Foundry storage layout.
pub const TEMPO_BENEFICIARY_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});

/// TempoState storage slot for `tempoStateRoot`, generated from the Foundry storage layout.
pub const TEMPO_STATE_ROOT_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});

/// TempoState storage slot for `tempoTransactionsRoot`, generated from the Foundry storage layout.
pub const TEMPO_TRANSACTIONS_ROOT_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});

/// TempoState storage slot for `tempoReceiptsRoot`, generated from the Foundry storage layout.
pub const TEMPO_RECEIPTS_ROOT_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});

/// TempoState storage slot containing `tempoBlockNumber`, generated from the Foundry storage layout.
pub const TEMPO_PACKED_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});

/// TempoState storage slot containing `tempoTimestampMillis`, generated from the Foundry storage layout.
pub const TEMPO_TIMESTAMP_MILLIS_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});

/// TempoState storage slot for `tempoPrevRandao`, generated from the Foundry storage layout.
pub const TEMPO_PREV_RANDAO_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});

/// ZoneInbox storage slot for `processedDepositQueueHash`, generated from the Foundry storage layout.
pub const ZONE_INBOX_PROCESSED_HASH_SLOT: ::alloy_primitives::U256 =
    {};

/// ZoneInbox storage slot for `processedDepositNumber`, generated from the Foundry storage layout.
pub const ZONE_INBOX_PROCESSED_NUMBER_SLOT: ::alloy_primitives::U256 =
    {};

/// ZoneOutbox storage slot for `_lastBatch.withdrawalQueueHash`, generated from the Foundry storage layout.
pub const ZONE_OUTBOX_LAST_BATCH_HASH_SLOT: ::alloy_primitives::U256 =
    {};

/// ZoneOutbox storage slot for `_lastBatch.withdrawalBatchIndex`, generated from the Foundry storage layout.
pub const ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT: ::alloy_primitives::U256 =
    {};

/// ZonePortal storage slot for `currentDepositQueueHash`, generated from the Foundry storage layout.
pub const PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT: ::alloy_primitives::StorageKey =
    ::alloy_primitives::b256!({});
",
        b256_literal(&tempo_block_hash_slot),
        b256_literal(&tempo_wrapper_gas_limits_slot),
        b256_literal(&tempo_parent_hash_slot),
        b256_literal(&tempo_beneficiary_slot),
        b256_literal(&tempo_state_root_slot),
        b256_literal(&tempo_transactions_root_slot),
        b256_literal(&tempo_receipts_root_slot),
        b256_literal(&tempo_packed_slot),
        b256_literal(&tempo_timestamp_millis_slot),
        b256_literal(&tempo_prev_randao_slot),
        u256_literal(inbox_processed_hash_slot),
        u256_literal(inbox_processed_number_slot),
        u256_literal(outbox_last_batch_hash_slot),
        u256_literal(outbox_last_batch_index_slot),
        b256_literal(&portal_current_deposit_queue_hash_slot),
    );

    fs::write(out_dir.join("tempo_state_slots.rs"), generated)
}

fn path_from_env(name: &str) -> io::Result<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| invalid_data(format!("missing {name} environment variable")))
}

fn load_json(path: &Path) -> io::Result<Value> {
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(|err| {
        invalid_data(format!(
            "failed to parse Foundry artifact {}: {err}",
            path.display()
        ))
    })
}

fn storage_key(
    artifact: &Value,
    label: &str,
    expected_offset: u64,
    expected_type_label: &str,
    expected_type_bytes: &str,
) -> io::Result<B256> {
    Ok(B256::from(storage_slot_u256(
        artifact,
        label,
        expected_offset,
        expected_type_label,
        expected_type_bytes,
    )?))
}

fn storage_slot_u256(
    artifact: &Value,
    label: &str,
    expected_offset: u64,
    expected_type_label: &str,
    expected_type_bytes: &str,
) -> io::Result<U256> {
    let entry = storage_entry(artifact, label)?;
    let offset = value_u64(entry, "offset")?;
    if offset != expected_offset {
        return Err(invalid_data(format!(
            "`{label}` has storage offset {offset}, expected {expected_offset}"
        )));
    }

    let type_id = value_str(entry, "type")?;
    let ty = artifact
        .pointer("/storageLayout/types")
        .and_then(|types| types.get(type_id))
        .ok_or_else(|| invalid_data(format!("missing storage type `{type_id}` for `{label}`")))?;
    let type_label = value_str(ty, "label")?;
    if type_label != expected_type_label {
        return Err(invalid_data(format!(
            "`{label}` has storage type `{type_label}`, expected `{expected_type_label}`"
        )));
    }
    let type_bytes = value_str(ty, "numberOfBytes")?;
    if type_bytes != expected_type_bytes {
        return Err(invalid_data(format!(
            "`{label}` has {type_bytes} storage bytes, expected {expected_type_bytes}"
        )));
    }

    parse_u256(value_str(entry, "slot")?)
}

fn struct_member_slot_u256(
    artifact: &Value,
    struct_label: &str,
    member_label: &str,
    expected_offset: u64,
    expected_type_label: &str,
    expected_type_bytes: &str,
) -> io::Result<U256> {
    let entry = storage_entry(artifact, struct_label)?;
    let base_slot = parse_u256(value_str(entry, "slot")?)?;
    let struct_type_id = value_str(entry, "type")?;
    let struct_type = artifact
        .pointer("/storageLayout/types")
        .and_then(|types| types.get(struct_type_id))
        .ok_or_else(|| {
            invalid_data(format!(
                "missing storage type `{struct_type_id}` for `{struct_label}`"
            ))
        })?;
    let members = struct_type
        .get("members")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_data(format!("`{struct_label}` storage type has no members")))?;

    let mut found = None;
    for member in members {
        if value_str(member, "label")? == member_label {
            if found.is_some() {
                return Err(invalid_data(format!(
                    "duplicate storageLayout member `{struct_label}.{member_label}`"
                )));
            }
            found = Some(member);
        }
    }
    let member = found.ok_or_else(|| {
        invalid_data(format!(
            "missing storageLayout member `{struct_label}.{member_label}`"
        ))
    })?;

    let offset = value_u64(member, "offset")?;
    if offset != expected_offset {
        return Err(invalid_data(format!(
            "`{struct_label}.{member_label}` has storage offset {offset}, expected {expected_offset}"
        )));
    }
    let type_id = value_str(member, "type")?;
    let ty = artifact
        .pointer("/storageLayout/types")
        .and_then(|types| types.get(type_id))
        .ok_or_else(|| {
            invalid_data(format!(
                "missing storage type `{type_id}` for `{struct_label}.{member_label}`"
            ))
        })?;
    let type_label = value_str(ty, "label")?;
    if type_label != expected_type_label {
        return Err(invalid_data(format!(
            "`{struct_label}.{member_label}` has storage type `{type_label}`, expected `{expected_type_label}`"
        )));
    }
    let type_bytes = value_str(ty, "numberOfBytes")?;
    if type_bytes != expected_type_bytes {
        return Err(invalid_data(format!(
            "`{struct_label}.{member_label}` has {type_bytes} storage bytes, expected {expected_type_bytes}"
        )));
    }

    let member_slot = parse_u256(value_str(member, "slot")?)?;
    base_slot.checked_add(member_slot).ok_or_else(|| {
        invalid_data(format!(
            "`{struct_label}.{member_label}` absolute storage slot overflowed"
        ))
    })
}

fn storage_entry<'a>(artifact: &'a Value, label: &str) -> io::Result<&'a Value> {
    let storage = artifact
        .pointer("/storageLayout/storage")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_data("missing storageLayout.storage array"))?;

    let mut found = None;
    for entry in storage {
        if value_str(entry, "label")? == label {
            if found.is_some() {
                return Err(invalid_data(format!(
                    "duplicate storageLayout entry for `{label}`"
                )));
            }
            found = Some(entry);
        }
    }

    found.ok_or_else(|| invalid_data(format!("missing storageLayout entry for `{label}`")))
}

fn value_str<'a>(value: &'a Value, field: &str) -> io::Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data(format!("missing string field `{field}`")))
}

fn value_u64(value: &Value, field: &str) -> io::Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data(format!("missing integer field `{field}`")))
}

fn parse_u256(slot: &str) -> io::Result<U256> {
    let (digits, radix) = if let Some(hex) = slot.strip_prefix("0x") {
        (hex, 16)
    } else {
        (slot, 10)
    };
    let parsed = U256::from_str_radix(digits, radix)
        .map_err(|err| invalid_data(format!("invalid storage slot `{slot}`: {err}")))?;
    Ok(parsed)
}

fn b256_literal(value: &B256) -> String {
    let bytes: &[u8; 32] = value.as_ref();
    let mut hex = String::with_capacity(66);
    hex.push_str("\"0x");
    for byte in bytes {
        hex.push_str(&format!("{:02x}", *byte));
    }
    hex.push('"');
    hex
}

fn u256_literal(value: U256) -> String {
    let [limb0, limb1, limb2, limb3] = value.into_limbs();
    format!("::alloy_primitives::U256::from_limbs([{limb0}, {limb1}, {limb2}, {limb3}])")
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
