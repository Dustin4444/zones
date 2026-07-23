#!/usr/bin/env bash
set -euo pipefail

readonly FOUNDRY_ROOT="specs/ref-impls"
readonly CREATE2_FACTORY="0x4e59b44847b379578588920cA78FbF26c0B4956C"
readonly SALT_NAMESPACE="tempo-zones-runtime-source-v1"

readonly -a CONTRACTS=("ZonePortal" "ZoneMessenger" "Verifier")
readonly -a ARTIFACTS=(
    "src/tempo/ZonePortal.sol:ZonePortal"
    "src/tempo/ZoneMessenger.sol:ZoneMessenger"
    "src/tempo/Verifier.sol:Verifier"
)

usage() {
    cat <<'EOF'
Build and deploy the canonical Zone runtime sources to one Tempo chain.

Usage:
  RPC_URL=<tempo-rpc-url> PRIVATE_KEY=<deployer-private-key> \
    scripts/deploy-zone-runtime-sources.sh

The script derives the signer and chain ID, computes deterministic CREATE2
addresses, skips matching deployments, deploys missing runtimes, rejects
conflicting code, and prints a secret-free JSON manifest.
EOF
}

log() {
    echo "$*" >&2
}

fail() {
    log "error: $*"
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command '$1' was not found"
}

normalize_hex() {
    tr '[:upper:]' '[:lower:]' <<< "$1"
}

runtime_initcode() {
    local runtime="${1#0x}"
    local runtime_size
    local runtime_size_hex

    runtime_size=$(( ${#runtime} / 2 ))
    (( runtime_size > 0 )) || fail "cannot deploy empty runtime"
    (( runtime_size <= 65535 )) || fail "runtime is too large to wrap in PUSH2 initcode"

    printf -v runtime_size_hex '%04x' "$runtime_size"
    # PUSH2 size; PUSH2 0x000f; PUSH1 0; CODECOPY; PUSH2 size; PUSH1 0; RETURN.
    echo "0x61${runtime_size_hex}61000f60003961${runtime_size_hex}6000f3${runtime}"
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi
(( $# == 0 )) || fail "this script takes no arguments; set RPC_URL and PRIVATE_KEY"

rpc_url="${RPC_URL:-}"
private_key="${PRIVATE_KEY:-}"
[[ -n "$rpc_url" ]] || fail "RPC_URL is required"
[[ -n "$private_key" ]] || fail "PRIVATE_KEY is required"

require_command cast
require_command forge
require_command jq

sender="$(normalize_hex "$(cast wallet address "$private_key")")"
chain_id="$(cast chain-id --rpc-url "$rpc_url")"
[[ "$chain_id" =~ ^[0-9]+$ ]] || fail "RPC returned invalid chain ID: $chain_id"

factory="$(normalize_hex "$CREATE2_FACTORY")"
factory_runtime="$(normalize_hex "$(cast code "$factory" --rpc-url "$rpc_url")")"
[[ "$factory_runtime" != "0x" && -n "$factory_runtime" ]] ||
    fail "chain $chain_id has no canonical CREATE2 factory at $factory"
factory_hash="$(normalize_hex "$(cast keccak "$factory_runtime")")"

log "Building Zone runtime artifacts..."
forge build --root "$FOUNDRY_ROOT" --skip test --no-lint >/dev/null

source_addresses=()
salts=()
runtime_hashes=()
deployment_states=()
transaction_hashes=()

for index in "${!CONTRACTS[@]}"; do
    contract="${CONTRACTS[$index]}"
    runtime="$(normalize_hex "$(forge inspect \
        --root "$FOUNDRY_ROOT" \
        "${ARTIFACTS[$index]}" \
        deployedBytecode)")"
    [[ "$runtime" != "0x" && -n "$runtime" ]] ||
        fail "built $contract runtime is empty"

    initcode="$(runtime_initcode "$runtime")"
    salt="$(normalize_hex "$(cast keccak "${SALT_NAMESPACE}:${contract}")")"
    create2_output="$(cast create2 \
        --deployer "$factory" \
        --salt "$salt" \
        --init-code "$initcode")"
    source_address="$(normalize_hex "${create2_output%%[[:space:]]*}")"
    [[ "$source_address" =~ ^0x[0-9a-f]{40}$ ]] ||
        fail "could not derive the $contract source address"

    deployed_runtime="$(normalize_hex "$(cast code "$source_address" --rpc-url "$rpc_url")")"
    transaction_hash=""

    if [[ "$deployed_runtime" == "$runtime" ]]; then
        state="already_deployed"
        log "$contract already deployed at $source_address"
    elif [[ "$deployed_runtime" == "0x" || -z "$deployed_runtime" ]]; then
        log "Deploying $contract to $source_address..."
        calldata="0x${salt#0x}${initcode#0x}"
        receipt="$(cast send \
            "$factory" \
            --data "$calldata" \
            --rpc-url "$rpc_url" \
            --private-key "$private_key" \
            --confirmations 1 \
            --force \
            --json)"
        status="$(jq -r '.status // empty' <<< "$receipt")"
        [[ "$status" == "0x1" || "$status" == "1" ]] ||
            fail "failed to deploy $contract"

        transaction_hash="$(jq -r '.transactionHash // empty' <<< "$receipt")"
        [[ -n "$transaction_hash" && "$transaction_hash" != "null" ]] ||
            fail "$contract deployment receipt has no transaction hash"

        deployed_runtime="$(normalize_hex "$(cast code "$source_address" --rpc-url "$rpc_url")")"
        [[ "$deployed_runtime" == "$runtime" ]] ||
            fail "deployed unexpected $contract bytecode at $source_address"
        state="deployed"
    else
        fail "unexpected code at deterministic $contract address $source_address"
    fi

    source_addresses[$index]="$source_address"
    salts[$index]="$salt"
    runtime_hashes[$index]="$(normalize_hex "$(cast keccak "$runtime")")"
    deployment_states[$index]="$state"
    transaction_hashes[$index]="$transaction_hash"
done

contracts_json='[]'
for index in "${!CONTRACTS[@]}"; do
    contracts_json="$(jq -c \
        --arg contract "${CONTRACTS[$index]}" \
        --arg address "${source_addresses[$index]}" \
        --arg salt "${salts[$index]}" \
        --arg runtime_hash "${runtime_hashes[$index]}" \
        --arg state "${deployment_states[$index]}" \
        --arg transaction_hash "${transaction_hashes[$index]}" \
        '. + [{
            contract: $contract,
            sourceAddress: $address,
            salt: $salt,
            runtimeHash: $runtime_hash,
            state: $state,
            transactionHash: (
                if $transaction_hash == "" then null else $transaction_hash end
            )
        }]' \
        <<< "$contracts_json")"
done

jq -n \
    --arg chain_id "$chain_id" \
    --arg sender "$sender" \
    --arg factory "$factory" \
    --arg factory_hash "$factory_hash" \
    --arg salt_namespace "$SALT_NAMESPACE" \
    --argjson contracts "$contracts_json" \
    '{
        chainId: ($chain_id | tonumber),
        sender: $sender,
        create2Factory: $factory,
        create2FactoryRuntimeHash: $factory_hash,
        saltNamespace: $salt_namespace,
        contracts: $contracts
    }'
