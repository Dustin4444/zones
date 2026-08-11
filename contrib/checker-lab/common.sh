# Shared checker-lab process and build helpers.

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
