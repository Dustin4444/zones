#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR
ZONES_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
readonly ZONES_ROOT
readonly TEMPO_ROOT="${TEMPO_ROOT:-$(cd -- "$ZONES_ROOT/.." && pwd)/tempo}"
readonly STATE_DIR="${CHECKER_LAB_STATE_DIR:-$ZONES_ROOT/target/checker-lab}"

readonly TEMPO_BIN_OVERRIDE="${TEMPO_BIN:-}"
readonly ZONE_BIN_OVERRIDE="${ZONE_BIN:-}"
readonly TEMPO_BIN="${TEMPO_BIN:-$TEMPO_ROOT/target/debug/tempo}"
readonly ZONE_BIN="${ZONE_BIN:-$ZONES_ROOT/target/debug/tempo-zone}"
readonly L1_HTTP_PORT="${L1_HTTP_PORT:-8545}"
readonly L1_WS_PORT="${L1_WS_PORT:-8546}"
readonly L1_AUTH_PORT="${L1_AUTH_PORT:-8551}"
readonly L1_P2P_PORT="${L1_P2P_PORT:-30303}"
readonly ZONE_HTTP_PORT="${ZONE_HTTP_PORT:-9545}"
readonly ZONE_REDACTED_PORT="${ZONE_REDACTED_PORT:-9555}"
readonly L1_BLOCK_TIME="${L1_BLOCK_TIME:-500ms}"

readonly L1_HTTP_URL="http://127.0.0.1:$L1_HTTP_PORT"
readonly L1_WS_URL="ws://127.0.0.1:$L1_WS_PORT"
readonly ZONE_HTTP_URL="http://127.0.0.1:$ZONE_HTTP_PORT"

# Standard first Anvil development account. Never use this lab configuration on a public network.
readonly DEV_KEY="${DEV_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
readonly DEV_ADDRESS="${DEV_ADDRESS:-0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266}"
readonly PATH_USD="0x20C0000000000000000000000000000000000000"
readonly ALPHA_USD="0x20c0000000000000000000000000000000000001"
readonly TIP403_REGISTRY="0x403C000000000000000000000000000000000000"
readonly ZONE_FACTORY="0x5AF2000000000000000000000000000000000000"

readonly GENESIS_DIR="$STATE_DIR/genesis"
readonly L1_DATADIR="$STATE_DIR/l1"
readonly ZONE_DIR="$STATE_DIR/zone"
readonly ZONE_DATADIR="$ZONE_DIR/node"
readonly CHECKER_DB="$STATE_DIR/checker"
readonly LOG_DIR="$STATE_DIR/logs"
readonly PID_DIR="$STATE_DIR/pids"
readonly L1_LOG="$LOG_DIR/l1.log"
readonly ZONE_LOG="$LOG_DIR/zone.log"
readonly L1_PID_FILE="$PID_DIR/l1.pid"
readonly ZONE_PID_FILE="$PID_DIR/zone.pid"

say() {
    printf '\n==> %s\n' "$*"
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

prepare() {
    require_command cargo
    require_command cast
    require_command jq
    require_command just
    [[ -d "$TEMPO_ROOT" ]] || die "Tempo checkout not found: $TEMPO_ROOT (set TEMPO_ROOT)"
    mkdir -p "$GENESIS_DIR" "$L1_DATADIR" "$ZONE_DIR" "$LOG_DIR" "$PID_DIR"
}

validate_tempo_checkout() {
    if [[ -z "$TEMPO_BIN_OVERRIDE" ]]; then
        require_command git
        local expected_revision actual_revision
        expected_revision="$(sed -nE 's/.*tempo-alloy.*rev = "([0-9a-f]+)".*/\1/p' \
            "$ZONES_ROOT/Cargo.toml" | head -n 1)"
        [[ -n "$expected_revision" ]] || die "could not determine the pinned Tempo revision"
        actual_revision="$(git -C "$TEMPO_ROOT" rev-parse HEAD)"
        [[ "$actual_revision" == "$expected_revision" ]] || die \
            "Tempo checkout is at $actual_revision, but Zones is pinned to $expected_revision; set TEMPO_ROOT to a compatible checkout"
    fi
}

pid_is_running() {
    local pid_file="$1"
    [[ -f "$pid_file" ]] || return 1
    local pid
    pid="$(cat "$pid_file")"
    [[ "$pid" =~ ^[0-9]+$ ]] && kill -0 "$pid" 2>/dev/null
}

assert_running() {
    local pid_file="$1"
    local name="$2"
    local log_file="$3"
    if ! pid_is_running "$pid_file"; then
        printf '%s is not running. Last log lines:\n' "$name" >&2
        tail -n 40 "$log_file" >&2 2>/dev/null || true
        exit 1
    fi
}

wait_for_rpc() {
    local url="$1"
    local pid_file="$2"
    local name="$3"
    local log_file="$4"
    local i
    for ((i = 0; i < 120; i++)); do
        assert_running "$pid_file" "$name" "$log_file"
        if cast block-number --rpc-url "$url" >/dev/null 2>&1; then
            return
        fi
        sleep 1
    done
    die "timed out waiting for $name RPC at $url; inspect $log_file"
}

build_tempo() {
    validate_tempo_checkout
    if [[ -z "$TEMPO_BIN_OVERRIDE" ]]; then
        say "Building Tempo"
        cargo build --manifest-path "$TEMPO_ROOT/Cargo.toml" --bin tempo
    fi
    [[ -x "$TEMPO_BIN" ]] || die "Tempo binary not found: $TEMPO_BIN"
}

build_zone() {
    if [[ -z "$ZONE_BIN_OVERRIDE" ]]; then
        say "Building tempo-zone"
        cargo build --manifest-path "$ZONES_ROOT/Cargo.toml" --bin tempo-zone
    fi
    [[ -x "$ZONE_BIN" ]] || die "Zone binary not found: $ZONE_BIN"
}

generate_genesis() {
    if [[ ! -f "$GENESIS_DIR/genesis.json" ]]; then
        say "Generating Tempo development genesis"
        cargo run --manifest-path "$TEMPO_ROOT/Cargo.toml" -p tempo-xtask -- \
            generate-genesis \
            --output "$GENESIS_DIR" \
            --accounts 1000 \
            --no-dkg-in-genesis
    fi

    # The generated genesis uses the production ZoneFactory owner. This disposable lab needs the
    # standard development account so `tempo-zone dev` can provision a Zone.
    local factory slot current owner desired temporary
    factory="$(printf '%s' "$ZONE_FACTORY" | tr '[:upper:]' '[:lower:]')"
    slot="0x$(printf '%064d' 0)"
    current="$(jq -er --arg factory "$factory" --arg slot "$slot" \
        '.alloc[$factory].storage[$slot]' "$GENESIS_DIR/genesis.json")" \
        || die "Tempo genesis is missing the ZoneFactory configuration slot"
    [[ "$current" =~ ^0x[0-9a-fA-F]{64}$ && "${current: -8}" == "00000001" ]] \
        || die "unexpected ZoneFactory configuration in Tempo genesis"
    owner="$(printf '%s' "${DEV_ADDRESS#0x}" | tr '[:upper:]' '[:lower:]')"
    desired="0x0000000000000000${owner}00000001"
    temporary="$GENESIS_DIR/genesis.json.tmp"
    jq --arg factory "$factory" --arg slot "$slot" --arg value "$desired" \
        '.alloc[$factory].storage[$slot] = $value' \
        "$GENESIS_DIR/genesis.json" >"$temporary"
    mv "$temporary" "$GENESIS_DIR/genesis.json"
}

start_l1() {
    if pid_is_running "$L1_PID_FILE"; then
        return
    fi
    if cast block-number --rpc-url "$L1_HTTP_URL" >/dev/null 2>&1; then
        die "$L1_HTTP_URL is already serving RPC but is not owned by the checker lab"
    fi
    build_tempo
    generate_genesis
    say "Starting Tempo L1; log: $L1_LOG"
    (
        cd "$TEMPO_ROOT"
        exec "$TEMPO_BIN" node \
            --chain "$GENESIS_DIR/genesis.json" \
            --datadir "$L1_DATADIR" \
            --dev \
            --dev.block-time "$L1_BLOCK_TIME" \
            --http --http.addr 127.0.0.1 --http.port "$L1_HTTP_PORT" --http.api all \
            --ws --ws.addr 127.0.0.1 --ws.port "$L1_WS_PORT" --ws.api all \
            --authrpc.port "$L1_AUTH_PORT" \
            --port "$L1_P2P_PORT" \
            --engine.disable-precompile-cache \
            --engine.legacy-state-root \
            --builder.gaslimit 3000000000 \
            --builder.max-tasks 1 \
            --builder.deadline 3 \
            --faucet.enabled \
            --faucet.private-key "$DEV_KEY" \
            --faucet.amount 1000000000000000 \
            --faucet.address "$PATH_USD" "$ALPHA_USD"
    ) >"$L1_LOG" 2>&1 &
    echo "$!" >"$L1_PID_FILE"
    wait_for_rpc "$L1_HTTP_URL" "$L1_PID_FILE" "Tempo L1" "$L1_LOG"
}

prepare_l1_protocol() {
    local owner expected_owner balance token configured
    owner="$(cast call "$ZONE_FACTORY" 'owner()(address)' --rpc-url "$L1_HTTP_URL")"
    owner="$(printf '%s' "$owner" | tr '[:upper:]' '[:lower:]')"
    expected_owner="$(printf '%s' "$DEV_ADDRESS" | tr '[:upper:]' '[:lower:]')"
    [[ "$owner" == "$expected_owner" ]] \
        || die "local ZoneFactory is not owned by the development account"

    balance="$(cast call "$PATH_USD" 'balanceOf(address)(uint256)' "$DEV_ADDRESS" \
        --rpc-url "$L1_HTTP_URL" | awk '{print $1}')"
    if [[ "$balance" == "0" ]]; then
        cast rpc tempo_fundAddress "$DEV_ADDRESS" --rpc-url "$L1_HTTP_URL" >/dev/null
    fi

    for token in "$PATH_USD" "$ALPHA_USD"; do
        configured="$(cast call "$TIP403_REGISTRY" \
            'tokenTransferPolicyId(address)(bool,uint64)' "$token" \
            --rpc-url "$L1_HTTP_URL" | head -n 1)"
        if [[ "$configured" != "true" ]]; then
            cast send "$TIP403_REGISTRY" \
                'migrateTransferPolicyIds(address[])' "[$token]" \
                --private-key "$DEV_KEY" \
                --rpc-url "$L1_HTTP_URL" >/dev/null
        fi
    done
}

stop_one() {
    local pid_file="$1"
    local name="$2"
    if ! pid_is_running "$pid_file"; then
        rm -f "$pid_file"
        return
    fi
    local pid i
    pid="$(cat "$pid_file")"
    printf 'Stopping %s (PID %s)\n' "$name" "$pid"
    kill -INT "$pid" 2>/dev/null || true
    for ((i = 0; i < 40; i++)); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.25
    done
    if kill -0 "$pid" 2>/dev/null; then
        kill -TERM "$pid" 2>/dev/null || true
        sleep 1
    fi
    if kill -0 "$pid" 2>/dev/null; then
        kill -KILL "$pid" 2>/dev/null || true
    fi
    rm -f "$pid_file"
}

stop_zone() {
    stop_one "$ZONE_PID_FILE" "Zone"
}

zone_metadata() {
    local field="$1"
    jq -er ".$field" "$ZONE_DIR/zone.json" \
        || die "Zone metadata field '$field' is missing from $ZONE_DIR/zone.json"
}

provision_zone() {
    [[ ! -f "$ZONE_DIR/zone.json" ]] || return 0
    say "Provisioning the development Zone"
    env DEV_KEY="$DEV_KEY" RUST_LOG_STYLE=never "$ZONE_BIN" dev \
        --l1.rpc-url "$L1_WS_URL" \
        --datadir "$ZONE_DIR" \
        --http.addr 127.0.0.1 \
        --http.port "$ZONE_HTTP_PORT" \
        --redacted-rpc.port "$ZONE_REDACTED_PORT" \
        >"$ZONE_LOG" 2>&1 &
    echo "$!" >"$ZONE_PID_FILE"
    wait_for_rpc "$ZONE_HTTP_URL" "$ZONE_PID_FILE" "Zone provisioning node" "$ZONE_LOG"
    stop_zone
}

build_checkpoint() {
    [[ ! -d "$CHECKER_DB" ]] || return 0
    local portal zone_id creation_hash
    portal="$(zone_metadata portal)"
    zone_id="$(zone_metadata zoneId)"
    creation_hash="$(zone_metadata portalCreationBlockHash)"
    say "Building checker checkpoint"
    "$ZONE_BIN" checker build-checkpoint \
        --checker.database-path "$CHECKER_DB" \
        --checker.portal-creation-block-hash "$creation_hash" \
        -- \
        node \
        --chain "$ZONE_DIR/genesis.json" \
        --datadir "$ZONE_DATADIR" \
        --l1.rpc-url "$L1_WS_URL" \
        --l1.portal-address "$portal" \
        --zone.id "$zone_id" \
        --http \
        --http.port "$ZONE_HTTP_PORT" \
        --redacted-rpc.port "$ZONE_REDACTED_PORT"
}

start_zone() {
    if pid_is_running "$ZONE_PID_FILE"; then
        return
    fi
    [[ -f "$ZONE_DIR/zone.json" ]] || die "Zone is not provisioned; run 'up'"
    [[ -d "$CHECKER_DB" ]] || die "checker checkpoint is missing; run 'up'"
    if cast block-number --rpc-url "$ZONE_HTTP_URL" >/dev/null 2>&1; then
        die "$ZONE_HTTP_URL is already serving RPC but is not owned by the checker lab"
    fi
    local portal zone_id creation_hash sequencer_key_file
    portal="$(zone_metadata portal)"
    zone_id="$(zone_metadata zoneId)"
    creation_hash="$(zone_metadata portalCreationBlockHash)"
    sequencer_key_file="$ZONE_DIR/sequencer.key"
    [[ -f "$sequencer_key_file" ]] || die "Zone sequencer key file is missing; run 'up'"
    say "Starting Zone with checker observe mode; log: $ZONE_LOG"
    (
        cd "$ZONES_ROOT"
        export RUST_LOG="${RUST_LOG:-info,zone::checker=debug}"
        export RUST_LOG_STYLE=never
        exec "$ZONE_BIN" node \
            --chain "$ZONE_DIR/genesis.json" \
            --datadir "$ZONE_DATADIR" \
            --l1.rpc-url "$L1_WS_URL" \
            --l1.portal-address "$portal" \
            --zone.id "$zone_id" \
            --http --http.addr 127.0.0.1 --http.port "$ZONE_HTTP_PORT" --http.api all \
            --redacted-rpc.port "$ZONE_REDACTED_PORT" \
            --log.file.directory "$ZONE_DIR/logs" \
            --sequencer \
            --sequencer-key-file "$sequencer_key_file" \
            --checker.mode observe \
            --checker.database-path "$CHECKER_DB" \
            --checker.portal-creation-block-hash "$creation_hash"
    ) >"$ZONE_LOG" 2>&1 &
    echo "$!" >"$ZONE_PID_FILE"
    wait_for_rpc "$ZONE_HTTP_URL" "$ZONE_PID_FILE" "Zone" "$ZONE_LOG"
}

checker_json() {
    "$ZONE_BIN" checker inspect --checker.database-path "$CHECKER_DB" --json
}

wait_for_checker() {
    local l1_target="$1"
    local l2_target="$2"
    local i state imported verified finding
    for ((i = 0; i < 120; i++)); do
        assert_running "$ZONE_PID_FILE" "Zone" "$ZONE_LOG"
        if state="$(checker_json 2>/dev/null)"; then
            imported="$(jq -r '.importedTempoTip.number' <<<"$state")"
            verified="$(jq -r '.verifiedZoneTip.number' <<<"$state")"
            finding="$(jq -r '.activeFinding' <<<"$state")"
            if [[ "$finding" == "true" ]]; then
                say "Checker recorded a finding"
                printf '%s\n' "$state"
                return
            fi
            if ((imported >= l1_target && verified >= l2_target)); then
                say "Checker verified the triggered transition"
                printf '%s\n' "$state"
                return
            fi
        fi
        sleep 1
    done
    die "timed out waiting for checker progress; inspect $ZONE_LOG"
}

export_trigger_environment() {
    export L1_RPC_URL="$L1_HTTP_URL"
    export L1_PORTAL_ADDRESS
    L1_PORTAL_ADDRESS="$(zone_metadata portal)"
    export ZONE_RPC_URL="$ZONE_HTTP_URL"
    export PRIVATE_KEY="$DEV_KEY"
    export ADMIN_KEY="$DEV_KEY"
}

trigger() {
    local scenario="${1:-}"
    assert_running "$L1_PID_FILE" "Tempo L1" "$L1_LOG"
    assert_running "$ZONE_PID_FILE" "Zone" "$ZONE_LOG"
    export_trigger_environment
    case "$scenario" in
        token)
            local salt suffix output token
            suffix="$(date +%s)"
            salt="$(cast keccak "checker-lab-$suffix-$$")"
            output="$(just --justfile "$ZONES_ROOT/Justfile" create-token \
                "Checker $suffix" "CHK" "$salt")"
            printf '%s\n' "$output"
            token="$(awk '/Address:/ {print $2; exit}' <<<"$output")"
            [[ "$token" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "could not parse token address"
            just --justfile "$ZONES_ROOT/Justfile" enable-token "$token"
            ;;
        deposit)
            just --justfile "$ZONES_ROOT/Justfile" max-approve-portal "$PATH_USD"
            just --justfile "$ZONES_ROOT/Justfile" send-deposit \
                1000000 "$DEV_ADDRESS" \
                0x0000000000000000000000000000000000000000000000000000000000000000 \
                "$PATH_USD" "$ZONE_HTTP_URL"
            ;;
        withdrawal)
            just --justfile "$ZONES_ROOT/Justfile" max-approve-portal "$PATH_USD"
            just --justfile "$ZONES_ROOT/Justfile" send-deposit \
                1000000 "$DEV_ADDRESS" \
                0x0000000000000000000000000000000000000000000000000000000000000000 \
                "$PATH_USD" "$ZONE_HTTP_URL"
            just --justfile "$ZONES_ROOT/Justfile" max-approve-outbox "$PATH_USD" "$ZONE_HTTP_URL"
            just --justfile "$ZONES_ROOT/Justfile" send-withdrawal \
                100000 "$DEV_ADDRESS" "$PATH_USD" \
                0x0000000000000000000000000000000000000000000000000000000000000000 \
                0 "$DEV_ADDRESS" 0x 0x "$ZONE_HTTP_URL"
            ;;
        *) die "trigger expects one of: token, deposit, withdrawal" ;;
    esac
    wait_for_checker \
        "$(cast block-number --rpc-url "$L1_HTTP_URL")" \
        "$(cast block-number --rpc-url "$ZONE_HTTP_URL")"
}

up() {
    prepare
    start_l1
    prepare_l1_protocol
    build_zone
    provision_zone
    build_checkpoint
    start_zone
    status
}

restart_zone() {
    prepare
    assert_running "$L1_PID_FILE" "Tempo L1" "$L1_LOG"
    stop_zone
    build_zone
    start_zone
    status
}

status() {
    prepare
    local l1_running=false zone_running=false
    if pid_is_running "$L1_PID_FILE"; then
        l1_running=true
        printf 'Tempo L1: running (PID %s, HTTP %s, WS %s)\n' \
            "$(cat "$L1_PID_FILE")" "$L1_HTTP_URL" "$L1_WS_URL"
    else
        printf 'Tempo L1: stopped\n'
    fi
    if pid_is_running "$ZONE_PID_FILE"; then
        zone_running=true
        printf 'Zone:     running (PID %s, HTTP %s)\n' \
            "$(cat "$ZONE_PID_FILE")" "$ZONE_HTTP_URL"
    else
        printf 'Zone:     stopped\n'
    fi
    printf 'State:    %s\n' "$STATE_DIR"

    if [[ -d "$CHECKER_DB" && -x "$ZONE_BIN" ]]; then
        local checker_state
        if ! checker_state="$(checker_json 2>/dev/null)"; then
            printf 'Checker:  database currently unavailable for inspection\n'
            return
        fi

        local imported_tip verified_tip active_finding coverage_gap
        imported_tip="$(jq -r '.importedTempoTip.number' <<<"$checker_state")"
        verified_tip="$(jq -r '.verifiedZoneTip.number' <<<"$checker_state")"
        active_finding="$(jq -r '.activeFinding' <<<"$checker_state")"
        coverage_gap="$(jq -r '.hasCoverageGap' <<<"$checker_state")"

        printf '\nLive tips:\n'
        if [[ "$l1_running" == true ]]; then
            local l1_head l1_finalized
            l1_head="$(cast block-number --rpc-url "$L1_HTTP_URL")"
            l1_finalized="$(( $(cast block finalized --rpc-url "$L1_HTTP_URL" --json | jq -r '.number') ))"
            printf '  Tempo L1 head:       %s\n' "$l1_head"
            printf '  Tempo L1 finalized:  %s (head distance: %s blocks)\n' \
                "$l1_finalized" "$((l1_head - l1_finalized))"
            printf '  Imported Tempo tip:  %s (finalized lag: %s blocks)\n' \
                "$imported_tip" "$((l1_finalized - imported_tip))"
        else
            printf '  Imported Tempo tip:  %s\n' "$imported_tip"
        fi
        if [[ "$zone_running" == true ]]; then
            local zone_head
            zone_head="$(cast block-number --rpc-url "$ZONE_HTTP_URL")"
            printf '  Zone head:           %s\n' "$zone_head"
            printf '  Verified Zone tip:   %s (lag: %s blocks)\n' \
                "$verified_tip" "$((zone_head - verified_tip))"
        else
            printf '  Verified Zone tip:   %s\n' "$verified_tip"
        fi
        printf '\nChecker:\n'
        printf '  Active finding:      %s\n' "$active_finding"
        printf '  Coverage gap:        %s\n' "$coverage_gap"
        printf '\nDurable checker state:\n%s\n' "$checker_state"
    fi
}

down() {
    prepare
    stop_zone
    stop_one "$L1_PID_FILE" "Tempo L1"
}

reset() {
    down
    say "Removing checker-lab state: $STATE_DIR"
    rm -rf "$STATE_DIR"
}

logs() {
    prepare
    case "${1:-zone}" in
        l1) tail -f "$L1_LOG" ;;
        zone) tail -f "$ZONE_LOG" ;;
        *) die "logs expects 'l1' or 'zone'" ;;
    esac
}

usage() {
    cat <<EOF
Usage: $(basename "$0") <command> [argument]

Commands:
  up                         Build and start Tempo L1 and the Zone checker
  restart-zone               Rebuild and restart only the Zone checker
  trigger token|deposit|withdrawal
                             Submit bridge activity and await checker progress
  status                     Show processes and durable checker state
  logs [zone|l1]             Follow a managed log
  down                       Stop managed processes and preserve state
  reset                      Stop processes and remove $STATE_DIR

Environment overrides:
  TEMPO_ROOT, TEMPO_BIN, ZONE_BIN, CHECKER_LAB_STATE_DIR,
  L1_HTTP_PORT, L1_WS_PORT, L1_AUTH_PORT, L1_P2P_PORT,
  ZONE_HTTP_PORT, ZONE_REDACTED_PORT, L1_BLOCK_TIME
EOF
}

case "${1:-}" in
    up) up ;;
    restart-zone) restart_zone ;;
    trigger) trigger "${2:-}" ;;
    status) status ;;
    logs) logs "${2:-zone}" ;;
    down) down ;;
    reset) reset ;;
    *) usage; exit 1 ;;
esac
