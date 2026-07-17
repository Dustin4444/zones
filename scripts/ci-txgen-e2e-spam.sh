#!/usr/bin/env bash
set -Eeuo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

lowercase() {
  tr '[:upper:]' '[:lower:]'
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

for name in RUNNER_TEMP TXGEN_DIR L1_HTTP_URL; do
  [ -n "${!name:-}" ] || die "$name is required"
done

ZONE_DATADIR="${ZONE_DATADIR:-$RUNNER_TEMP/tempo-zone-dev}"
ARTIFACT_DIR="${TXGEN_E2E_ARTIFACT_DIR:-$RUNNER_TEMP/txgen-e2e}"
REPORT_DIR="${TXGEN_REPORT_DIR:-$ARTIFACT_DIR/reports}"
SUMMARY="$ARTIFACT_DIR/summary.md"
ZONE_RPC_URL="${ZONE_RPC_URL:-http://127.0.0.1:9545}"
L1_WS_URL="${L1_WS_URL:-ws://127.0.0.1:8545}"
ANVIL_PID=""
ZONE_PID=""

mkdir -p "$ARTIFACT_DIR" "$REPORT_DIR"
export TXGEN_REPORT_DIR="$REPORT_DIR" ZONE_DATADIR

stop_process() {
  local pid="$1"
  [ -n "$pid" ] || return 0
  kill -0 "$pid" 2>/dev/null || return 0
  kill -INT "$pid" 2>/dev/null || true
  for _ in $(seq 1 50); do
    if ! kill -0 "$pid" 2>/dev/null; then
      wait "$pid" 2>/dev/null || true
      return 0
    fi
    sleep 0.2
  done
  kill -TERM "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

install_anvil_zone_factory() {
  local specs_root="$REPO_ROOT/specs/ref-impls"
  local dev_key="${DEV_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
  local factory="0x5aF2000000000000000000000000000000000000"
  local client_version owner factory_code installed_owner verifier messenger
  local verifier_deployment messenger_deployment runtime one three valid_verifier_slot
  local observed_verifier observed_messenger observed_owner valid_verifier

  client_version="$(cast rpc web3_clientVersion --rpc-url "$L1_HTTP_URL")"
  case "$(printf '%s' "$client_version" | lowercase)" in
    *anvil*) ;;
    *) die "refusing to modify non-Anvil endpoint $L1_HTTP_URL (client: $client_version)" ;;
  esac

  owner="$(cast wallet address "$dev_key")"
  factory_code="$(cast code "$factory" --rpc-url "$L1_HTTP_URL")"
  if [ "$factory_code" != "0x" ]; then
    installed_owner="$(cast call "$factory" 'owner()(address)' --rpc-url "$L1_HTTP_URL")"
    verifier="$(cast call "$factory" 'verifier()(address)' --rpc-url "$L1_HTTP_URL")"
    messenger="$(cast call "$factory" 'messenger()(address)' --rpc-url "$L1_HTTP_URL")"
    [ "$(printf '%s' "$installed_owner" | lowercase)" = "$(printf '%s' "$owner" | lowercase)" ] || \
      die "ZoneFactory owner is $installed_owner; expected $owner"
    [ "$verifier" != "0x0000000000000000000000000000000000000000" ] || \
      die "ZoneFactory verifier is unset"
    [ "$messenger" != "0x0000000000000000000000000000000000000000" ] || \
      die "ZoneFactory messenger is unset"
    return
  fi

  forge build --root "$specs_root" --skip test --no-lint >/dev/null
  verifier_deployment="$(
    forge create --root "$specs_root" src/tempo/Verifier.sol:Verifier \
      --broadcast --json --rpc-url "$L1_HTTP_URL" --private-key "$dev_key"
  )"
  verifier="$(printf '%s' "$verifier_deployment" | jq -er '.deployedTo')"
  messenger_deployment="$(
    forge create --root "$specs_root" src/tempo/ZoneMessenger.sol:ZoneMessenger \
      --broadcast --json --rpc-url "$L1_HTTP_URL" --private-key "$dev_key" \
      --constructor-args "$factory"
  )"
  messenger="$(printf '%s' "$messenger_deployment" | jq -er '.deployedTo')"

  runtime="$(jq -er '.deployedBytecode.object' "$specs_root/out/ZoneFactory.sol/ZoneFactory.json")"
  case "$runtime" in 0x*) ;; *) runtime="0x$runtime" ;; esac
  one="$(cast pad 0x01)"
  three="$(cast pad 0x03)"
  valid_verifier_slot="$(cast index address "$verifier" 3)"

  cast rpc anvil_setCode "$factory" "$runtime" --rpc-url "$L1_HTTP_URL" >/dev/null
  cast rpc anvil_setNonce "$factory" 0x3 --rpc-url "$L1_HTTP_URL" >/dev/null
  cast rpc anvil_setStorageAt "$factory" "$(cast pad 0x00)" "$one" --rpc-url "$L1_HTTP_URL" >/dev/null
  cast rpc anvil_setStorageAt "$factory" "$(cast pad "$valid_verifier_slot")" "$one" --rpc-url "$L1_HTTP_URL" >/dev/null
  cast rpc anvil_setStorageAt "$factory" "$(cast pad 0x04)" "$(cast pad "$verifier")" --rpc-url "$L1_HTTP_URL" >/dev/null
  cast rpc anvil_setStorageAt "$factory" "$(cast pad 0x05)" "$(cast pad "$messenger")" --rpc-url "$L1_HTTP_URL" >/dev/null
  cast rpc anvil_setStorageAt "$factory" "$(cast pad 0x06)" "$(cast pad "$owner")" --rpc-url "$L1_HTTP_URL" >/dev/null
  cast rpc anvil_setStorageAt "$factory" "$(cast pad 0x07)" "$three" --rpc-url "$L1_HTTP_URL" >/dev/null

  observed_verifier="$(cast call "$factory" 'verifier()(address)' --rpc-url "$L1_HTTP_URL")"
  observed_messenger="$(cast call "$factory" 'messenger()(address)' --rpc-url "$L1_HTTP_URL")"
  observed_owner="$(cast call "$factory" 'owner()(address)' --rpc-url "$L1_HTTP_URL")"
  valid_verifier="$(cast call "$factory" 'isValidVerifier(address)(bool)' "$verifier" --rpc-url "$L1_HTTP_URL")"
  [ "$(printf '%s' "$observed_verifier" | lowercase)" = "$(printf '%s' "$verifier" | lowercase)" ] || \
    die "installed ZoneFactory verifier mismatch"
  [ "$(printf '%s' "$observed_messenger" | lowercase)" = "$(printf '%s' "$messenger" | lowercase)" ] || \
    die "installed ZoneFactory messenger mismatch"
  [ "$(printf '%s' "$observed_owner" | lowercase)" = "$(printf '%s' "$owner" | lowercase)" ] || \
    die "installed ZoneFactory owner mismatch"
  [ "$valid_verifier" = "true" ] || die "installed ZoneFactory does not recognize its verifier"
}

write_summary() {
  local status="$1"
  local result="❌ Failed"
  local report metrics sent accepted transfer_tps zone_tps
  local run_link=""
  local -a reports

  shopt -s nullglob
  reports=("$REPORT_DIR"/l2-tip20-throughput-*.json)
  if [ "$status" -eq 0 ]; then
    result="✅ Passed"
  fi

  if [ "${#reports[@]}" -eq 1 ] && \
    metrics="$(jq -er '
      [
        .sent,
        .success,
        ((.sent * 100000 / .run_stats.duration_ms) | round / 100),
        ((.run_stats.avg_tps * 100) | round / 100)
      ] | @tsv
    ' "${reports[0]}" 2>/dev/null)"; then
    IFS=$'\t' read -r sent accepted transfer_tps zone_tps <<<"$metrics"
    printf '%s\n\n' '### Txgen E2E spam' >"$SUMMARY"
    if [ "$status" -eq 0 ]; then
      printf '✅ Passed — **%s/%s accepted at a %s TPS target**; verified TIP-20 execution: **%s TPS**; total Zone throughput: **%s TPS**.\n\n' \
        "$accepted" "$sent" "${TXGEN_THROUGHPUT_TPS:-${TPS:-unknown}}" "$transfer_tps" "$zone_tps" >>"$SUMMARY"
      printf 'Receipts, the balance delta, and L1 batch settlement were verified.\n' >>"$SUMMARY"
    else
      printf '❌ Failed — the report recorded **%s/%s accepted** and **%s TPS**, but the run did not pass verification.\n\n' \
        "$accepted" "$sent" "$transfer_tps" >>"$SUMMARY"
      printf 'The throughput report completed, but a later verification failed; see the attached logs.\n' >>"$SUMMARY"
    fi
  else
    printf '%s\n\n%s — the run did not produce a complete throughput report.\n' \
      '### Txgen E2E spam' "$result" >"$SUMMARY"
  fi

  if [ -n "${GITHUB_SERVER_URL:-}" ] && [ -n "${GITHUB_REPOSITORY:-}" ] && [ -n "${GITHUB_RUN_ID:-}" ]; then
    run_link="$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID"
    printf '\n[Workflow run](%s)\n' "$run_link" >>"$SUMMARY"
  fi

  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    cat "$SUMMARY" >>"$GITHUB_STEP_SUMMARY"
  fi
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
      echo 'comment_body<<TXGEN_EOF'
      cat "$SUMMARY"
      echo 'TXGEN_EOF'
    } >>"$GITHUB_OUTPUT"
  fi
}

finish() {
  local status="$?"
  trap - EXIT
  set +e
  stop_process "$ZONE_PID"
  stop_process "$ANVIL_PID"
  if [ -f "$ZONE_DATADIR/zone.json" ]; then
    cp "$ZONE_DATADIR/zone.json" "$ARTIFACT_DIR/zone.json"
  fi
  write_summary "$status"
  exit "$status"
}
trap finish EXIT

cargo build --release --bin tempo-zone
cargo build --release \
  --manifest-path "$TXGEN_DIR/Cargo.toml" \
  -p txgen-tempo \
  -p bench-cli

anvil --network tempo --block-time 1 --host 127.0.0.1 --port 8545 \
  >"$ARTIFACT_DIR/anvil.log" 2>&1 &
ANVIL_PID=$!

for _ in $(seq 1 60); do
  if cast rpc web3_clientVersion --rpc-url "$L1_HTTP_URL" >/dev/null 2>&1; then
    break
  fi
  kill -0 "$ANVIL_PID" 2>/dev/null || die "Anvil exited before becoming ready"
  sleep 1
done
cast rpc web3_clientVersion --rpc-url "$L1_HTTP_URL" >/dev/null 2>&1 || \
  die "Anvil did not become ready"

install_anvil_zone_factory

target/release/tempo-zone dev \
  --l1.rpc-url "$L1_WS_URL" \
  --datadir "$ZONE_DATADIR" \
  --http.port 9545 \
  --private-rpc.port 8544 \
  -- \
  --zone.batch-interval-blocks 10 \
  --txpool.max-account-slots 1024 \
  >"$ARTIFACT_DIR/zone.log" 2>&1 &
ZONE_PID=$!

for _ in $(seq 1 180); do
  if [ -f "$ZONE_DATADIR/zone.json" ] && \
    cast block-number --rpc-url "$ZONE_RPC_URL" >/dev/null 2>&1; then
    break
  fi
  kill -0 "$ZONE_PID" 2>/dev/null || die "tempo-zone dev exited before becoming ready"
  sleep 1
done
[ -f "$ZONE_DATADIR/zone.json" ] && \
  cast block-number --rpc-url "$ZONE_RPC_URL" >/dev/null 2>&1 || \
  die "tempo-zone dev did not become ready"

scripts/txgen-e2e-spam.sh all

shopt -s nullglob
reports=("$REPORT_DIR"/l2-tip20-throughput-*.json)
[ "${#reports[@]}" -eq 1 ] || die "expected one throughput report, found ${#reports[@]}"
