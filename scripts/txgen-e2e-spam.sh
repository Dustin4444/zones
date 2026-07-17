#!/usr/bin/env bash
set -euo pipefail

export FOUNDRY_DISABLE_NIGHTLY_WARNING="${FOUNDRY_DISABLE_NIGHTLY_WARNING:-1}"

usage() {
  cat <<'EOF'
Usage: scripts/txgen-e2e-spam.sh

Sends one randomized, multi-sender L2 TIP-20 transfer workload through txgen.
The target Zone must already be running and have written zone.json. Correctness
properties for bridge, policy, transfer, withdrawal, and bounceback behavior live
in the Rust E2E test suite; this script measures sustained submission/execution.

The main controls are COUNT, TPS, TXGEN_TRANSFER_ACCOUNTS, and TXGEN_NONCE_LANES.
See docs/txgen-e2e-spam.md.
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

first_word() {
  awk 'NR == 1 { print $1; exit }'
}

lowercase() {
  tr '[:upper:]' '[:lower:]'
}

require_uint() {
  local name="$1"
  local value="$2"
  case "$value" in
    ''|*[!0-9]*) die "$name must be an unsigned integer, got '$value'" ;;
  esac
}

uint_ge() {
  local lhs="$1"
  local rhs="$2"

  while [ "${lhs#0}" != "$lhs" ]; do lhs="${lhs#0}"; done
  while [ "${rhs#0}" != "$rhs" ]; do rhs="${rhs#0}"; done
  lhs="${lhs:-0}"
  rhs="${rhs:-0}"
  if [ "${#lhs}" -ne "${#rhs}" ]; then
    [ "${#lhs}" -gt "${#rhs}" ]
  else
    [ "$lhs" = "$rhs" ] || [[ "$lhs" > "$rhs" ]]
  fi
}

if [ "$#" -gt 0 ]; then
  case "$1" in
    -h|--help) usage; exit 0 ;;
    spam|throughput) ;;
    *) usage >&2; die "this runner only supports the random L2 transfer spam primitive" ;;
  esac
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SYSTEM_TMPDIR="${TMPDIR:-/tmp}"
SYSTEM_TMPDIR="${SYSTEM_TMPDIR%/}"
ZONE_DATADIR="${ZONE_DATADIR:-$SYSTEM_TMPDIR/tempo-zone-dev}"
ZONE_JSON="${ZONE_JSON:-$ZONE_DATADIR/zone.json}"

L1_HTTP_URL="${L1_HTTP_URL:-${L1_RPC_URL:-http://127.0.0.1:8545}}"
case "$L1_HTTP_URL" in
  ws://*) L1_HTTP_URL="http://${L1_HTTP_URL#ws://}" ;;
  wss://*) L1_HTTP_URL="https://${L1_HTTP_URL#wss://}" ;;
esac

TXGEN_DIR="${TXGEN_DIR:-$(cd "$REPO_ROOT/.." && pwd)/txgen}"
TXGEN_TEMPO_BIN="${TXGEN_TEMPO_BIN:-$TXGEN_DIR/target/release/txgen-tempo}"
BENCH_BIN="${BENCH_BIN:-$TXGEN_DIR/target/release/bench}"
TXGEN_REPORT_DIR="${TXGEN_REPORT_DIR:-$ZONE_DATADIR/txgen-reports}"

COUNT="${COUNT:-5000}"
TPS="${TPS:-1000}"
MAX_CONCURRENT="${MAX_CONCURRENT:-2000}"
DRAIN_TIMEOUT="${DRAIN_TIMEOUT:-240}"
SYNC_TIMEOUT="${SYNC_TIMEOUT:-300}"
TXGEN_NONCE_LANES="${TXGEN_NONCE_LANES:-1000000}"
TXGEN_TRANSFER_ACCOUNTS="${TXGEN_TRANSFER_ACCOUNTS:-8}"
TXGEN_TRANSFER_AMOUNT="${TXGEN_TRANSFER_AMOUNT:-1000}"
TXGEN_TRANSFER_FEE_BUFFER="${TXGEN_TRANSFER_FEE_BUFFER:-100000}"
TXGEN_DEPOSIT_AMOUNT="${TXGEN_DEPOSIT_AMOUNT:-5000000000}"
TXGEN_MAX_FEE_PER_GAS="${TXGEN_MAX_FEE_PER_GAS:-100000000000}"
TXGEN_MAX_PRIORITY_FEE_PER_GAS="${TXGEN_MAX_PRIORITY_FEE_PER_GAS:-100000000000}"
TXGEN_MNEMONIC="${TXGEN_MNEMONIC:-test test test test test test test test test test test junk}"
TXGEN_FEE_TOKEN="${TXGEN_FEE_TOKEN:-0x20C0000000000000000000000000000000000000}"

for pair in \
  "COUNT:$COUNT" \
  "TPS:$TPS" \
  "MAX_CONCURRENT:$MAX_CONCURRENT" \
  "DRAIN_TIMEOUT:$DRAIN_TIMEOUT" \
  "SYNC_TIMEOUT:$SYNC_TIMEOUT" \
  "TXGEN_NONCE_LANES:$TXGEN_NONCE_LANES" \
  "TXGEN_TRANSFER_ACCOUNTS:$TXGEN_TRANSFER_ACCOUNTS" \
  "TXGEN_TRANSFER_AMOUNT:$TXGEN_TRANSFER_AMOUNT" \
  "TXGEN_TRANSFER_FEE_BUFFER:$TXGEN_TRANSFER_FEE_BUFFER" \
  "TXGEN_DEPOSIT_AMOUNT:$TXGEN_DEPOSIT_AMOUNT"; do
  require_uint "${pair%%:*}" "${pair#*:}"
done

[ "$COUNT" -gt 0 ] || die "COUNT must be at least 1"
[ "$MAX_CONCURRENT" -gt 0 ] || die "MAX_CONCURRENT must be at least 1"
[ "$TXGEN_NONCE_LANES" -gt 1 ] || die "TXGEN_NONCE_LANES must be greater than 1"
[ "$TXGEN_TRANSFER_ACCOUNTS" -gt 0 ] || die "TXGEN_TRANSFER_ACCOUNTS must be at least 1"
[ "$TXGEN_TRANSFER_AMOUNT" -gt 0 ] || die "TXGEN_TRANSFER_AMOUNT must be at least 1"

for command in cast jq awk tr date sleep mkdir; do
  command -v "$command" >/dev/null || die "required command '$command' is not installed"
done
[ -f "$ZONE_JSON" ] || die "zone metadata not found at $ZONE_JSON; start tempo-zone dev first or set ZONE_DATADIR/ZONE_JSON"
[ -x "$TXGEN_TEMPO_BIN" ] || die "txgen-tempo not found at $TXGEN_TEMPO_BIN; build the sibling txgen checkout first"
[ -x "$BENCH_BIN" ] || die "bench not found at $BENCH_BIN; build bench-cli in the sibling txgen checkout first"

TXGEN_PORTAL="$(jq -er '.portal' "$ZONE_JSON")"
ZONE_RPC_URL="${ZONE_RPC_URL:-$(jq -er '.rpcUrl' "$ZONE_JSON")}"
TXGEN_TOKEN="${TXGEN_TOKEN:-$(jq -er '.initialToken' "$ZONE_JSON")}"
TXGEN_ACCOUNT="$(cast wallet address --mnemonic "$TXGEN_MNEMONIC" --mnemonic-index 0)"
TXGEN_TRANSFER_RECIPIENT="${TXGEN_TRANSFER_RECIPIENT:-$(cast wallet address --mnemonic "$TXGEN_MNEMONIC" --mnemonic-index "$TXGEN_TRANSFER_ACCOUNTS")}"

cast block-number --rpc-url "$L1_HTTP_URL" >/dev/null
cast block-number --rpc-url "$ZONE_RPC_URL" >/dev/null
[ "$(cast code "$TXGEN_PORTAL" --rpc-url "$L1_HTTP_URL")" != "0x" ] || \
  die "no ZonePortal code at $TXGEN_PORTAL on $L1_HTTP_URL"
[ "$(cast call "$TXGEN_PORTAL" 'isTokenEnabled(address)(bool)' "$TXGEN_TOKEN" --rpc-url "$L1_HTTP_URL")" = "true" ] || \
  die "TXGEN_TOKEN $TXGEN_TOKEN is not enabled on portal $TXGEN_PORTAL"
[ "$(printf '%s' "$TXGEN_TOKEN" | lowercase)" = "$(printf '%s' "$TXGEN_FEE_TOKEN" | lowercase)" ] || \
  die "the standalone spammer currently requires TXGEN_TOKEN and TXGEN_FEE_TOKEN to match"
[ "$(cast call "$TXGEN_TOKEN" 'transferPolicyId()(uint64)' --rpc-url "$L1_HTTP_URL" | first_word)" = "1" ] || \
  die "TXGEN_TOKEN must use allow-all policy 1 before throughput spam"

L1_CHAIN_ID="$(cast chain-id --rpc-url "$L1_HTTP_URL")"
L2_CHAIN_ID="$(cast chain-id --rpc-url "$ZONE_RPC_URL")"
TXGEN_ACTIVE_ACCOUNTS=1
ACTIVE_TRANSFER_ACCOUNTS=()
LAST_REPORT=""
SETTLED_L2_BLOCK=""

mkdir -p "$TXGEN_REPORT_DIR"
export TXGEN_MNEMONIC TXGEN_NONCE_LANES TXGEN_MAX_FEE_PER_GAS
export TXGEN_MAX_PRIORITY_FEE_PER_GAS TXGEN_PORTAL TXGEN_TRANSFER_AMOUNT
export TXGEN_TOKEN TXGEN_FEE_TOKEN TXGEN_ACTIVE_ACCOUNTS TXGEN_TRANSFER_RECIPIENT
export TXGEN_DEPOSIT_AMOUNT

run_workload() {
  local spec="$1"
  local rpc="$2"
  local chain_id="$3"
  local count="$4"
  local report="$5"
  local label="$6"
  local sent accepted failed

  export TXGEN_CHAIN_ID="$chain_id"
  LAST_REPORT="$report"
  echo "==> $label: count=$count tps=$TPS rpc=$rpc"
  "$TXGEN_TEMPO_BIN" generate \
    --spec "$SCRIPT_DIR/txgen/$spec" \
    --count "$count" \
    --rpc "$rpc" \
    | "$BENCH_BIN" send \
      --rpc-url "$rpc" \
      --tps "$TPS" \
      --max-concurrent "$MAX_CONCURRENT" \
      --retries 0 \
      --drain-timeout "$DRAIN_TIMEOUT" \
      --report "json:$report" \
      --metadata "zone-workload=$label"

  sent="$(jq -er '.sent' "$report")"
  accepted="$(jq -er '.success' "$report")"
  failed="$(jq -er '.failed' "$report")"
  [ "$sent" -eq "$count" ] || die "$label generated $sent/$count transactions"
  [ "$accepted" -eq "$count" ] && [ "$failed" -eq 0 ] || \
    die "$label accepted=$accepted failed=$failed expected=$count (report: $report)"
}

zone_balance() {
  local token="$1"
  local account="$2"
  cast call "$token" 'balanceOf(address)(uint256)' "$account" \
    --from "$account" --rpc-url "$ZONE_RPC_URL" | first_word
}

deposit_to_account0() {
  local required="$1"
  local balance deposit_fee net_per_deposit missing deposit_count gross l1_balance

  balance="$(zone_balance "$TXGEN_TOKEN" "$TXGEN_ACCOUNT")"
  if [ "$balance" -ge "$required" ]; then
    return
  fi
  deposit_fee="$(cast call "$TXGEN_PORTAL" 'calculateDepositFee()(uint128)' --rpc-url "$L1_HTTP_URL" | first_word)"
  [ "$TXGEN_DEPOSIT_AMOUNT" -gt "$deposit_fee" ] || \
    die "TXGEN_DEPOSIT_AMOUNT must exceed portal fee $deposit_fee"
  net_per_deposit=$((TXGEN_DEPOSIT_AMOUNT - deposit_fee))
  missing=$((required - balance))
  deposit_count=$(((missing + net_per_deposit - 1) / net_per_deposit))
  gross=$((deposit_count * TXGEN_DEPOSIT_AMOUNT))
  l1_balance="$(cast call "$TXGEN_TOKEN" 'balanceOf(address)(uint256)' "$TXGEN_ACCOUNT" --rpc-url "$L1_HTTP_URL" | first_word)"
  uint_ge "$l1_balance" "$gross" || \
    die "insufficient L1 token balance: have=$l1_balance need=$gross"

  run_workload l1-deposits.yaml "$L1_HTTP_URL" "$L1_CHAIN_ID" "$deposit_count" \
    "$TXGEN_REPORT_DIR/setup-deposit.json" setup-deposit

  local deadline=$(( $(date +%s) + SYNC_TIMEOUT ))
  while :; do
    balance="$(zone_balance "$TXGEN_TOKEN" "$TXGEN_ACCOUNT")"
    [ "$balance" -ge "$required" ] && break
    [ "$(date +%s)" -lt "$deadline" ] || \
      die "timed out waiting for L2 setup deposit: balance=$balance required=$required"
    sleep 1
  done
}

prepare_senders() {
  local index=0 account
  while [ "$index" -lt "$TXGEN_TRANSFER_ACCOUNTS" ]; do
    account="$(cast wallet address --mnemonic "$TXGEN_MNEMONIC" --mnemonic-index "$index")"
    [ "$(printf '%s' "$account" | lowercase)" != "$(printf '%s' "$TXGEN_TRANSFER_RECIPIENT" | lowercase)" ] || \
      die "TXGEN_TRANSFER_RECIPIENT must not be a throughput sender"
    ACTIVE_TRANSFER_ACCOUNTS[${#ACTIVE_TRANSFER_ACCOUNTS[@]}]="$account"
    index=$((index + 1))
  done
}

fund_senders() {
  local per_sender source_required total_missing=0 index account balance missing deadline
  local original_amount="$TXGEN_TRANSFER_AMOUNT"
  local original_recipient="$TXGEN_TRANSFER_RECIPIENT"
  local -a missing_by_index

  # Random selection may choose one sender for the entire run. Fund every
  # sender for that worst case so generator skew cannot invalidate the result.
  per_sender=$((COUNT * (TXGEN_TRANSFER_AMOUNT + TXGEN_TRANSFER_FEE_BUFFER)))
  index=1
  while [ "$index" -lt "${#ACTIVE_TRANSFER_ACCOUNTS[@]}" ]; do
    account="${ACTIVE_TRANSFER_ACCOUNTS[$index]}"
    balance="$(zone_balance "$TXGEN_TOKEN" "$account")"
    if [ "$balance" -lt "$per_sender" ]; then missing=$((per_sender - balance)); else missing=0; fi
    missing_by_index[$index]="$missing"
    total_missing=$((total_missing + missing))
    index=$((index + 1))
  done

  source_required=$((per_sender + total_missing + TXGEN_TRANSFER_ACCOUNTS * TXGEN_TRANSFER_FEE_BUFFER))
  deposit_to_account0 "$source_required"

  TXGEN_ACTIVE_ACCOUNTS=1
  export TXGEN_ACTIVE_ACCOUNTS
  index=1
  while [ "$index" -lt "${#ACTIVE_TRANSFER_ACCOUNTS[@]}" ]; do
    missing="${missing_by_index[$index]}"
    if [ "$missing" -gt 0 ]; then
      account="${ACTIVE_TRANSFER_ACCOUNTS[$index]}"
      balance="$(zone_balance "$TXGEN_TOKEN" "$account")"
      TXGEN_TRANSFER_AMOUNT="$missing"
      TXGEN_TRANSFER_RECIPIENT="$account"
      export TXGEN_TRANSFER_AMOUNT TXGEN_TRANSFER_RECIPIENT
      run_workload l2-tip20-transfers.yaml "$ZONE_RPC_URL" "$L2_CHAIN_ID" 1 \
        "$TXGEN_REPORT_DIR/setup-sender-$index.json" "setup-sender-$index"
      deadline=$(( $(date +%s) + SYNC_TIMEOUT ))
      while :; do
        [ "$(zone_balance "$TXGEN_TOKEN" "$account")" -eq $((balance + missing)) ] && break
        [ "$(date +%s)" -lt "$deadline" ] || die "timed out funding sender $account"
        sleep 1
      done
    fi
    index=$((index + 1))
  done

  TXGEN_TRANSFER_AMOUNT="$original_amount"
  TXGEN_TRANSFER_RECIPIENT="$original_recipient"
  TXGEN_ACTIVE_ACCOUNTS="$TXGEN_TRANSFER_ACCOUNTS"
  export TXGEN_TRANSFER_AMOUNT TXGEN_TRANSFER_RECIPIENT TXGEN_ACTIVE_ACCOUNTS
}

wait_for_l1_settlement() {
  local batch_before="$1"
  local end_block deadline batch_after portal_hash l2_head block_number block_hash

  end_block="$(jq -er '.run_stats.end_block' "$LAST_REPORT")"
  deadline=$(( $(date +%s) + SYNC_TIMEOUT ))
  while :; do
    batch_after="$(cast call "$TXGEN_PORTAL" 'withdrawalBatchIndex()(uint64)' --rpc-url "$L1_HTTP_URL" | first_word)"
    portal_hash="$(cast call "$TXGEN_PORTAL" 'blockHash()(bytes32)' --rpc-url "$L1_HTTP_URL" | first_word)"
    l2_head="$(cast block-number --rpc-url "$ZONE_RPC_URL")"
    if [ "$batch_after" -gt "$batch_before" ] && [ "$l2_head" -ge "$end_block" ]; then
      block_number="$end_block"
      while [ "$block_number" -le "$l2_head" ]; do
        block_hash="$(cast block "$block_number" --field hash --rpc-url "$ZONE_RPC_URL")"
        if [ "$(printf '%s' "$block_hash" | lowercase)" = "$(printf '%s' "$portal_hash" | lowercase)" ]; then
          SETTLED_L2_BLOCK="$block_number"
          return
        fi
        block_number=$((block_number + 1))
      done
    fi
    [ "$(date +%s)" -lt "$deadline" ] || \
      die "timed out waiting for L1 settlement of spam ending at L2 block $end_block"
    sleep 1
  done
}

prepare_senders
fund_senders

echo "Zone RPC:      $ZONE_RPC_URL (chain $L2_CHAIN_ID)"
echo "Token:         $TXGEN_TOKEN"
echo "Senders:       ${#ACTIVE_TRANSFER_ACCOUNTS[@]} (${ACTIVE_TRANSFER_ACCOUNTS[*]})"
echo "Recipient:     $TXGEN_TRANSFER_RECIPIENT"
echo "Target:        $COUNT transactions at $TPS TPS"

recipient_before="$(zone_balance "$TXGEN_TOKEN" "$TXGEN_TRANSFER_RECIPIENT")"
batch_before="$(cast call "$TXGEN_PORTAL" 'withdrawalBatchIndex()(uint64)' --rpc-url "$L1_HTTP_URL" | first_word)"
main_report="$TXGEN_REPORT_DIR/l2-tip20-spam.json"
run_workload l2-tip20-transfers.yaml "$ZONE_RPC_URL" "$L2_CHAIN_ID" "$COUNT" "$main_report" l2-tip20-spam

expected_recipient=$((recipient_before + COUNT * TXGEN_TRANSFER_AMOUNT))
deadline=$(( $(date +%s) + SYNC_TIMEOUT ))
while :; do
  recipient_after="$(zone_balance "$TXGEN_TOKEN" "$TXGEN_TRANSFER_RECIPIENT")"
  [ "$recipient_after" -eq "$expected_recipient" ] && break
  [ "$(date +%s)" -lt "$deadline" ] || \
    die "recipient balance=$recipient_after expected=$expected_recipient after spam"
  sleep 1
done

wait_for_l1_settlement "$batch_before"
accepted="$(jq -er '.success' "$main_report")"
submission_tps="$(jq -er '.tps' "$main_report")"
zone_tps="$(jq -er '.run_stats.avg_tps' "$main_report")"
echo "txgen spam passed: accepted=$accepted/$COUNT target_tps=$TPS submission_tps=$submission_tps zone_tps=$zone_tps settled_l2_block=$SETTLED_L2_BLOCK report=$main_report"
