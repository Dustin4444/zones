#!/usr/bin/env bash
set -euo pipefail

readonly FOUNDRY_ROOT="specs/ref-impls"
readonly DEFAULT_CREATE2_FACTORY="0x4e59b44847b379578588920cA78FbF26c0B4956C"
readonly DEFAULT_SALT_NAMESPACE="tempo-zones-runtime-source-v1"

readonly -a CONTRACTS=("ZonePortal" "ZoneMessenger" "Verifier")
readonly -a ARTIFACTS=(
    "src/tempo/ZonePortal.sol:ZonePortal"
    "src/tempo/ZoneMessenger.sol:ZoneMessenger"
    "src/tempo/Verifier.sol:Verifier"
)

usage() {
    cat <<'EOF'
Deploy the canonical Zone runtime source contracts to one or more Tempo chains.

Each runtime is wrapped in initcode and deployed through the canonical Arachnid
CREATE2 factory. The CREATE2 address depends on the runtime, but not the
transaction signer, so the same runtime is deployed at the same address on every
chain. Existing matching deployments are skipped.

Usage:
  scripts/deploy-zone-runtime-sources.sh \
    --chain NAME,RPC_URL,SENDER_ADDRESS,PRIVATE_KEY_ENV \
    [--chain NAME,RPC_URL,SENDER_ADDRESS,PRIVATE_KEY_ENV ...] \
    [--address CONTRACT=EXPECTED_ADDRESS ...] \
    [--factory ADDRESS] \
    [--salt-namespace STRING] \
    [--manifest PATH] \
    [--dry-run] \
    [--no-build]

Options:
  --chain SPEC
      Add an environment. SPEC contains:
        NAME             Environment label such as devnet, testnet, or mainnet.
        RPC_URL          Tempo RPC URL.
        SENDER_ADDRESS   Expected address of the deployment signer.
        PRIVATE_KEY_ENV  Name of the environment variable containing its key.

      The private key is only required when that chain is missing a deployment.

  --address CONTRACT=ADDRESS
      Assert the computed deterministic address for ZonePortal, ZoneMessenger,
      or Verifier. This is useful for pinning addresses selected by a hardfork.

  --factory ADDRESS
      CREATE2 factory shared by every chain.
      Default: 0x4e59b44847b379578588920cA78FbF26c0B4956C

  --salt-namespace STRING
      Namespace used to derive one stable salt per contract. Runtime bytecode is
      part of the CREATE2 address, so changed bytecode produces a new address.

  --manifest PATH
      Write the deployment manifest to PATH. The manifest never includes RPC
      URLs or private keys. Without this option, it is printed to stdout.

  --dry-run
      Inspect every chain and report missing deployments without sending.

  --no-build
      Reuse existing Foundry artifacts instead of building them first.

Examples:
  export DEVNET_DEPLOYER_PRIVATE_KEY=...
  export TESTNET_DEPLOYER_PRIVATE_KEY=...
  export MAINNET_DEPLOYER_PRIVATE_KEY=...

  scripts/deploy-zone-runtime-sources.sh \
    --chain "devnet,$DEVNET_RPC_URL,$DEVNET_DEPLOYER,DEVNET_DEPLOYER_PRIVATE_KEY" \
    --chain "testnet,$TESTNET_RPC_URL,$TESTNET_DEPLOYER,TESTNET_DEPLOYER_PRIVATE_KEY" \
    --chain "mainnet,$MAINNET_RPC_URL,$MAINNET_DEPLOYER,MAINNET_DEPLOYER_PRIVATE_KEY" \
    --manifest zone-runtime-sources.json
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

validate_address() {
    [[ "$1" =~ ^0x[0-9a-fA-F]{40}$ ]] || fail "invalid address: $1"
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

chain_names=()
chain_rpc_urls=()
chain_senders=()
chain_key_envs=()
expected_contracts=()
expected_addresses=()

factory="$DEFAULT_CREATE2_FACTORY"
salt_namespace="$DEFAULT_SALT_NAMESPACE"
manifest_path=""
dry_run=false
build_artifacts=true

while (( $# > 0 )); do
    case "$1" in
        --chain)
            (( $# >= 2 )) || fail "--chain requires a value"
            IFS=',' read -r chain_name rpc_url sender key_env extra <<< "$2"
            [[ -n "$chain_name" && -n "$rpc_url" && -n "$sender" && -n "$key_env" && -z "${extra:-}" ]] ||
                fail "--chain must be NAME,RPC_URL,SENDER_ADDRESS,PRIVATE_KEY_ENV"
            [[ "$chain_name" =~ ^[a-zA-Z0-9_-]+$ ]] ||
                fail "invalid environment name: $chain_name"
            validate_address "$sender"
            [[ "$key_env" =~ ^[a-zA-Z_][a-zA-Z0-9_]*$ ]] ||
                fail "invalid private-key environment variable name: $key_env"
            chain_names+=("$chain_name")
            chain_rpc_urls+=("$rpc_url")
            chain_senders+=("$(normalize_hex "$sender")")
            chain_key_envs+=("$key_env")
            shift 2
            ;;
        --address)
            (( $# >= 2 )) || fail "--address requires a value"
            contract="${2%%=*}"
            address="${2#*=}"
            [[ "$contract" != "$2" && -n "$address" ]] ||
                fail "--address must be CONTRACT=ADDRESS"
            case "$contract" in
                ZonePortal|ZoneMessenger|Verifier) ;;
                *) fail "unknown contract in --address: $contract" ;;
            esac
            validate_address "$address"
            expected_contracts+=("$contract")
            expected_addresses+=("$(normalize_hex "$address")")
            shift 2
            ;;
        --factory)
            (( $# >= 2 )) || fail "--factory requires a value"
            validate_address "$2"
            factory="$2"
            shift 2
            ;;
        --salt-namespace)
            (( $# >= 2 )) || fail "--salt-namespace requires a value"
            [[ -n "$2" ]] || fail "--salt-namespace cannot be empty"
            salt_namespace="$2"
            shift 2
            ;;
        --manifest)
            (( $# >= 2 )) || fail "--manifest requires a value"
            [[ -n "$2" ]] || fail "--manifest cannot be empty"
            manifest_path="$2"
            shift 2
            ;;
        --dry-run)
            dry_run=true
            shift
            ;;
        --no-build)
            build_artifacts=false
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
done

(( ${#chain_names[@]} > 0 )) || fail "at least one --chain is required"

require_command cast
require_command forge
require_command jq

factory="$(normalize_hex "$factory")"

for (( i = 0; i < ${#chain_names[@]}; i++ )); do
    for (( j = i + 1; j < ${#chain_names[@]}; j++ )); do
        [[ "${chain_names[$i]}" != "${chain_names[$j]}" ]] ||
            fail "duplicate environment name: ${chain_names[$i]}"
    done
done

if [[ "$build_artifacts" == true ]]; then
    log "Building Zone runtime artifacts..."
    forge build --root "$FOUNDRY_ROOT" --skip test --no-lint >/dev/null
fi

desired_runtimes=()
desired_hashes=()
initcodes=()
salts=()
source_addresses=()

for index in "${!CONTRACTS[@]}"; do
    contract="${CONTRACTS[$index]}"
    runtime=$(forge inspect \
        --root "$FOUNDRY_ROOT" \
        "${ARTIFACTS[$index]}" \
        deployedBytecode)
    [[ "$runtime" != "0x" && "$runtime" != "0X" && -n "$runtime" ]] ||
        fail "built $contract runtime is empty"

    initcode="$(runtime_initcode "$runtime")"
    salt="$(cast keccak "${salt_namespace}:${contract}")"
    create2_output=$(cast create2 \
        --deployer "$factory" \
        --salt "$salt" \
        --init-code "$initcode")
    source_address="${create2_output%%[[:space:]]*}"
    validate_address "$source_address"

    desired_runtimes[$index]="$(normalize_hex "$runtime")"
    desired_hashes[$index]="$(normalize_hex "$(cast keccak "$runtime")")"
    initcodes[$index]="$initcode"
    salts[$index]="$(normalize_hex "$salt")"
    source_addresses[$index]="$(normalize_hex "$source_address")"
done

for expected_index in "${!expected_contracts[@]}"; do
    expected_contract="${expected_contracts[$expected_index]}"
    expected_address="${expected_addresses[$expected_index]}"
    found=false
    for contract_index in "${!CONTRACTS[@]}"; do
        if [[ "${CONTRACTS[$contract_index]}" == "$expected_contract" ]]; then
            found=true
            [[ "${source_addresses[$contract_index]}" == "$expected_address" ]] ||
                fail "$expected_contract computes to ${source_addresses[$contract_index]}, not $expected_address"
        fi
    done
    [[ "$found" == true ]] || fail "could not validate expected address for $expected_contract"
done

chain_ids=()
factory_hash=""
deployment_states=()
transaction_hashes=()

log "Preflighting ${#chain_names[@]} environment(s)..."
for chain_index in "${!chain_names[@]}"; do
    chain_name="${chain_names[$chain_index]}"
    rpc_url="${chain_rpc_urls[$chain_index]}"
    chain_id=$(cast chain-id --rpc-url "$rpc_url")
    [[ "$chain_id" =~ ^[0-9]+$ ]] || fail "$chain_name returned invalid chain ID: $chain_id"
    chain_ids[$chain_index]="$chain_id"

    deployed_factory=$(cast code "$factory" --rpc-url "$rpc_url")
    [[ "$deployed_factory" != "0x" && "$deployed_factory" != "0X" && -n "$deployed_factory" ]] ||
        fail "$chain_name has no CREATE2 factory at $factory"
    deployed_factory_hash="$(normalize_hex "$(cast keccak "$deployed_factory")")"
    if [[ -z "$factory_hash" ]]; then
        factory_hash="$deployed_factory_hash"
    elif [[ "$deployed_factory_hash" != "$factory_hash" ]]; then
        fail "$chain_name has a different CREATE2 factory runtime at $factory"
    fi

    for contract_index in "${!CONTRACTS[@]}"; do
        flat_index=$(( contract_index * ${#chain_names[@]} + chain_index ))
        source_address="${source_addresses[$contract_index]}"
        deployed_runtime="$(normalize_hex "$(cast code "$source_address" --rpc-url "$rpc_url")")"

        if [[ "$deployed_runtime" == "${desired_runtimes[$contract_index]}" ]]; then
            deployment_states[$flat_index]="already_deployed"
            transaction_hashes[$flat_index]=""
            log "$chain_name: ${CONTRACTS[$contract_index]} already deployed at $source_address"
        elif [[ "$deployed_runtime" == "0x" || "$deployed_runtime" == "0X" || -z "$deployed_runtime" ]]; then
            deployment_states[$flat_index]="missing"
            transaction_hashes[$flat_index]=""
            log "$chain_name: ${CONTRACTS[$contract_index]} is missing at $source_address"
        else
            fail "$chain_name has unexpected code at deterministic address $source_address"
        fi
    done
done

if [[ "$dry_run" == false ]]; then
    for chain_index in "${!chain_names[@]}"; do
        chain_name="${chain_names[$chain_index]}"
        rpc_url="${chain_rpc_urls[$chain_index]}"
        sender="${chain_senders[$chain_index]}"
        key_env="${chain_key_envs[$chain_index]}"
        private_key="${!key_env:-}"
        needs_deployment=false

        for contract_index in "${!CONTRACTS[@]}"; do
            flat_index=$(( contract_index * ${#chain_names[@]} + chain_index ))
            if [[ "${deployment_states[$flat_index]}" == "missing" ]]; then
                needs_deployment=true
            fi
        done

        if [[ "$needs_deployment" == false ]]; then
            continue
        fi

        [[ -n "$private_key" ]] ||
            fail "$chain_name needs deployments but $key_env is not set"
        actual_sender="$(normalize_hex "$(cast wallet address "$private_key")")"
        [[ "$actual_sender" == "$sender" ]] ||
            fail "$chain_name signer is $actual_sender, not configured sender $sender"

        for contract_index in "${!CONTRACTS[@]}"; do
            flat_index=$(( contract_index * ${#chain_names[@]} + chain_index ))
            [[ "${deployment_states[$flat_index]}" == "missing" ]] || continue

            contract="${CONTRACTS[$contract_index]}"
            source_address="${source_addresses[$contract_index]}"
            calldata="0x${salts[$contract_index]#0x}${initcodes[$contract_index]#0x}"

            log "$chain_name: deploying $contract to $source_address..."
            receipt=$(cast send \
                "$factory" \
                --data "$calldata" \
                --rpc-url "$rpc_url" \
                --private-key "$private_key" \
                --confirmations 1 \
                --force \
                --json)
            status=$(jq -r '.status // empty' <<< "$receipt")
            [[ "$status" == "0x1" || "$status" == "1" ]] ||
                fail "$chain_name failed to deploy $contract"

            transaction_hash=$(jq -r '.transactionHash // empty' <<< "$receipt")
            [[ -n "$transaction_hash" && "$transaction_hash" != "null" ]] ||
                fail "$chain_name deployment receipt for $contract has no transaction hash"

            deployed_runtime="$(normalize_hex "$(cast code "$source_address" --rpc-url "$rpc_url")")"
            [[ "$deployed_runtime" == "${desired_runtimes[$contract_index]}" ]] ||
                fail "$chain_name deployed unexpected $contract bytecode at $source_address"

            deployment_states[$flat_index]="deployed"
            transaction_hashes[$flat_index]="$transaction_hash"
        done
    done
else
    for state_index in "${!deployment_states[@]}"; do
        if [[ "${deployment_states[$state_index]}" == "missing" ]]; then
            deployment_states[$state_index]="would_deploy"
        fi
    done
fi

chains_json='[]'
for chain_index in "${!chain_names[@]}"; do
    chains_json=$(jq -c \
        --arg name "${chain_names[$chain_index]}" \
        --arg chain_id "${chain_ids[$chain_index]}" \
        --arg sender "${chain_senders[$chain_index]}" \
        '. + [{name: $name, chainId: ($chain_id | tonumber), sender: $sender}]' \
        <<< "$chains_json")
done

contracts_json='[]'
for contract_index in "${!CONTRACTS[@]}"; do
    environments_json='[]'
    for chain_index in "${!chain_names[@]}"; do
        flat_index=$(( contract_index * ${#chain_names[@]} + chain_index ))
        environments_json=$(jq -c \
            --arg name "${chain_names[$chain_index]}" \
            --arg state "${deployment_states[$flat_index]}" \
            --arg transaction_hash "${transaction_hashes[$flat_index]}" \
            '. + [{
                name: $name,
                state: $state,
                transactionHash: (if $transaction_hash == "" then null else $transaction_hash end)
            }]' \
            <<< "$environments_json")
    done

    contracts_json=$(jq -c \
        --arg name "${CONTRACTS[$contract_index]}" \
        --arg address "${source_addresses[$contract_index]}" \
        --arg salt "${salts[$contract_index]}" \
        --arg runtime_hash "${desired_hashes[$contract_index]}" \
        --argjson environments "$environments_json" \
        '. + [{
            contract: $name,
            sourceAddress: $address,
            salt: $salt,
            runtimeHash: $runtime_hash,
            environments: $environments
        }]' \
        <<< "$contracts_json")
done

manifest=$(jq -n \
    --arg factory "$factory" \
    --arg factory_hash "$factory_hash" \
    --arg salt_namespace "$salt_namespace" \
    --argjson dry_run "$dry_run" \
    --argjson chains "$chains_json" \
    --argjson contracts "$contracts_json" \
    '{
        create2Factory: $factory,
        create2FactoryRuntimeHash: $factory_hash,
        saltNamespace: $salt_namespace,
        dryRun: $dry_run,
        chains: $chains,
        contracts: $contracts
    }')

if [[ -n "$manifest_path" ]]; then
    printf '%s\n' "$manifest" > "$manifest_path"
    log "Wrote deployment manifest to $manifest_path"
else
    printf '%s\n' "$manifest"
fi

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
        echo "### Zone runtime sources"
        for contract_index in "${!CONTRACTS[@]}"; do
            echo "- ${CONTRACTS[$contract_index]}: \`${source_addresses[$contract_index]}\` (\`${desired_hashes[$contract_index]}\`)"
        done
    } >> "$GITHUB_STEP_SUMMARY"
fi
