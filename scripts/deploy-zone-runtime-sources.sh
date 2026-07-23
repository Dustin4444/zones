#!/usr/bin/env bash
set -euo pipefail

readonly FOUNDRY_ROOT="specs/ref-impls"
readonly CREATE2_FACTORY="0x4e59b44847b379578588920cA78FbF26c0B4956C"
readonly SALT_NAMESPACE="tempo-zones-runtime-source-v1"
readonly DEFAULT_RPC_URL="http://zone-factory-hf-val-rpc-service.tail388b2e.ts.net:8545"

readonly -a CONTRACTS=("ZonePortal" "ZoneMessenger" "Verifier")
readonly -a ARTIFACTS=(
    "src/tempo/ZonePortal.sol:ZonePortal"
    "src/tempo/ZoneMessenger.sol:ZoneMessenger"
    "src/tempo/Verifier.sol:Verifier"
)

usage() {
    cat <<'EOF'
Build and deploy the canonical Zone runtime sources to Tempo chains.

Usage:
  PRIVATE_KEY=<deployer-private-key> \
    scripts/deploy-zone-runtime-sources.sh [RPC_URL ...]

When no RPC URL is supplied, the script deploys to the Tempo L1 backing the
zone-unstable devnet. It derives everything else, skips matching deployments,
rejects conflicting code, and prints a secret-free JSON manifest.
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

private_key="${PRIVATE_KEY:-}"
[[ -n "$private_key" ]] || fail "PRIVATE_KEY is required"

rpc_urls=("$@")
if (( ${#rpc_urls[@]} == 0 )); then
    rpc_urls=("$DEFAULT_RPC_URL")
fi

require_command cast
require_command forge
require_command jq

sender="$(normalize_hex "$(cast wallet address "$private_key")")"
factory="$(normalize_hex "$CREATE2_FACTORY")"

log "Building Zone runtime artifacts..."
forge build --root "$FOUNDRY_ROOT" --skip test --no-lint >/dev/null

runtimes=()
runtime_hashes=()
initcodes=()
salts=()
source_addresses=()

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

    runtimes[$index]="$runtime"
    runtime_hashes[$index]="$(normalize_hex "$(cast keccak "$runtime")")"
    initcodes[$index]="$initcode"
    salts[$index]="$salt"
    source_addresses[$index]="$source_address"
done

chain_ids=()
deployment_states=()
transaction_hashes=()
factory_hash=""

log "Preflighting ${#rpc_urls[@]} chain(s)..."
for chain_index in "${!rpc_urls[@]}"; do
    rpc_url="${rpc_urls[$chain_index]}"
    chain_id="$(cast chain-id --rpc-url "$rpc_url")"
    [[ "$chain_id" =~ ^[0-9]+$ ]] || fail "RPC returned invalid chain ID: $chain_id"

    if (( ${#chain_ids[@]} > 0 )); then
        for existing_chain_id in "${chain_ids[@]}"; do
            [[ "$chain_id" != "$existing_chain_id" ]] ||
                fail "chain ID $chain_id was supplied more than once"
        done
    fi
    chain_ids[$chain_index]="$chain_id"

    factory_runtime="$(normalize_hex "$(cast code "$factory" --rpc-url "$rpc_url")")"
    [[ "$factory_runtime" != "0x" && -n "$factory_runtime" ]] ||
        fail "chain $chain_id has no canonical CREATE2 factory at $factory"
    deployed_factory_hash="$(normalize_hex "$(cast keccak "$factory_runtime")")"
    if [[ -z "$factory_hash" ]]; then
        factory_hash="$deployed_factory_hash"
    elif [[ "$deployed_factory_hash" != "$factory_hash" ]]; then
        fail "chain $chain_id has different code at the canonical CREATE2 factory"
    fi

    for contract_index in "${!CONTRACTS[@]}"; do
        flat_index=$(( contract_index * ${#rpc_urls[@]} + chain_index ))
        source_address="${source_addresses[$contract_index]}"
        deployed_runtime="$(normalize_hex "$(cast code "$source_address" --rpc-url "$rpc_url")")"

        if [[ "$deployed_runtime" == "${runtimes[$contract_index]}" ]]; then
            deployment_states[$flat_index]="already_deployed"
            log "Chain $chain_id: ${CONTRACTS[$contract_index]} already deployed at $source_address"
        elif [[ "$deployed_runtime" == "0x" || -z "$deployed_runtime" ]]; then
            deployment_states[$flat_index]="missing"
            log "Chain $chain_id: ${CONTRACTS[$contract_index]} is missing at $source_address"
        else
            fail "chain $chain_id has unexpected code at $source_address"
        fi
        transaction_hashes[$flat_index]=""
    done
done

for chain_index in "${!rpc_urls[@]}"; do
    rpc_url="${rpc_urls[$chain_index]}"
    chain_id="${chain_ids[$chain_index]}"

    for contract_index in "${!CONTRACTS[@]}"; do
        flat_index=$(( contract_index * ${#rpc_urls[@]} + chain_index ))
        [[ "${deployment_states[$flat_index]}" == "missing" ]] || continue

        contract="${CONTRACTS[$contract_index]}"
        source_address="${source_addresses[$contract_index]}"
        calldata="0x${salts[$contract_index]#0x}${initcodes[$contract_index]#0x}"

        log "Chain $chain_id: deploying $contract to $source_address..."
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
            fail "chain $chain_id failed to deploy $contract"

        transaction_hash="$(jq -r '.transactionHash // empty' <<< "$receipt")"
        [[ -n "$transaction_hash" && "$transaction_hash" != "null" ]] ||
            fail "$contract deployment receipt has no transaction hash"

        deployed_runtime="$(normalize_hex "$(cast code "$source_address" --rpc-url "$rpc_url")")"
        [[ "$deployed_runtime" == "${runtimes[$contract_index]}" ]] ||
            fail "chain $chain_id deployed unexpected $contract bytecode"

        deployment_states[$flat_index]="deployed"
        transaction_hashes[$flat_index]="$transaction_hash"
    done
done

chains_json='[]'
for chain_id in "${chain_ids[@]}"; do
    chains_json="$(jq -c \
        --arg chain_id "$chain_id" \
        '. + [{chainId: ($chain_id | tonumber)}]' \
        <<< "$chains_json")"
done

contracts_json='[]'
for contract_index in "${!CONTRACTS[@]}"; do
    deployments_json='[]'
    for chain_index in "${!rpc_urls[@]}"; do
        flat_index=$(( contract_index * ${#rpc_urls[@]} + chain_index ))
        deployments_json="$(jq -c \
            --arg chain_id "${chain_ids[$chain_index]}" \
            --arg state "${deployment_states[$flat_index]}" \
            --arg transaction_hash "${transaction_hashes[$flat_index]}" \
            '. + [{
                chainId: ($chain_id | tonumber),
                state: $state,
                transactionHash: (
                    if $transaction_hash == "" then null else $transaction_hash end
                )
            }]' \
            <<< "$deployments_json")"
    done

    contracts_json="$(jq -c \
        --arg contract "${CONTRACTS[$contract_index]}" \
        --arg address "${source_addresses[$contract_index]}" \
        --arg salt "${salts[$contract_index]}" \
        --arg runtime_hash "${runtime_hashes[$contract_index]}" \
        --argjson deployments "$deployments_json" \
        '. + [{
            contract: $contract,
            sourceAddress: $address,
            salt: $salt,
            runtimeHash: $runtime_hash,
            deployments: $deployments
        }]' \
        <<< "$contracts_json")"
done

jq -n \
    --arg sender "$sender" \
    --arg factory "$factory" \
    --arg factory_hash "$factory_hash" \
    --arg salt_namespace "$SALT_NAMESPACE" \
    --argjson chains "$chains_json" \
    --argjson contracts "$contracts_json" \
    '{
        sender: $sender,
        create2Factory: $factory,
        create2FactoryRuntimeHash: $factory_hash,
        saltNamespace: $salt_namespace,
        chains: $chains,
        contracts: $contracts
    }'
