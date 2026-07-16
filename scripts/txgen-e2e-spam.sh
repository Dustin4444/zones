#!/usr/bin/env bash
set -euo pipefail

# Keep nightly Foundry's per-invocation warning from drowning out stress results.
export FOUNDRY_DISABLE_NIGHTLY_WARNING="${FOUNDRY_DISABLE_NIGHTLY_WARNING:-1}"

usage() {
  cat <<'EOF'
Usage: scripts/txgen-e2e-spam.sh [all|deposits|withdrawals|policies|throughput]

Runs txgen workloads against an Anvil-backed `tempo-zone dev` deployment.
The policies action covers allow-all, reject-all, whitelist, blacklist, and
compound TIP-403 behavior with real L2 TIP-20 transfers.
The throughput action runs one allow-all L2 transfer workload at the requested TPS.

Configuration is supplied through environment variables; see
docs/txgen-e2e-spam.md for the complete list.
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

parse_policy_modes() {
  local raw_mode mode
  local -a requested_modes

  IFS=',' read -r -a requested_modes <<<"$TXGEN_POLICY_MODES"
  [ "${#requested_modes[@]}" -gt 0 ] || die "TXGEN_POLICY_MODES must not be empty"

  POLICY_MODES=()
  for raw_mode in "${requested_modes[@]}"; do
    mode="$(printf '%s' "$raw_mode" | tr '[:upper:]_' '[:lower:]-' | tr -d '[:space:]')"
    case "$mode" in
      all)
        [ "${#requested_modes[@]}" -eq 1 ] || die "'all' cannot be combined with other TXGEN_POLICY_MODES"
        POLICY_MODES=(allow-all reject-all whitelist blacklist compound)
        return
        ;;
      allow|allow-all) mode="allow-all" ;;
      reject|reject-all) mode="reject-all" ;;
      whitelist|blacklist|compound) ;;
      *) die "unknown TIP-403 policy mode '$raw_mode'" ;;
    esac

    case " ${POLICY_MODES[*]-} " in
      *" $mode "*) die "duplicate TIP-403 policy mode '$mode'" ;;
    esac
    POLICY_MODES[${#POLICY_MODES[@]}]="$mode"
  done
}

parse_anvil_admin_bootstrap() {
  local mode
  mode="$(printf '%s' "$TXGEN_ANVIL_BOOTSTRAP_ADMIN" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  case "$mode" in
    auto|true|false) ANVIL_ADMIN_BOOTSTRAP_MODE="$mode" ;;
    *) die "TXGEN_ANVIL_BOOTSTRAP_ADMIN must be auto, true, or false; got '$TXGEN_ANVIL_BOOTSTRAP_ADMIN'" ;;
  esac
}

has_policy_mode() {
  local wanted="$1"
  local mode
  for mode in "${POLICY_MODES[@]}"; do
    [ "$mode" = "$wanted" ] && return 0
  done
  return 1
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ACTION="${1:-all}"

case "$ACTION" in
  all|deposits|withdrawals|policies|throughput) ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; die "unknown action '$ACTION'" ;;
esac

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

COUNT="${COUNT:-20}"
TPS="${TPS:-10}"
MAX_CONCURRENT="${MAX_CONCURRENT:-100}"
DRAIN_TIMEOUT="${DRAIN_TIMEOUT:-120}"
SYNC_TIMEOUT="${SYNC_TIMEOUT:-180}"
TXGEN_NONCE_LANES="${TXGEN_NONCE_LANES:-100}"
TXGEN_DEPOSIT_AMOUNT="${TXGEN_DEPOSIT_AMOUNT:-1000000}"
TXGEN_WITHDRAWAL_AMOUNT="${TXGEN_WITHDRAWAL_AMOUNT:-1000}"
TXGEN_TRANSFER_AMOUNT="${TXGEN_TRANSFER_AMOUNT:-1000}"
TXGEN_TRANSFER_FEE_BUFFER="${TXGEN_TRANSFER_FEE_BUFFER:-100000}"
TXGEN_TRANSFER_ACCOUNTS="${TXGEN_TRANSFER_ACCOUNTS:-1}"
TXGEN_POLICY_MODES="${TXGEN_POLICY_MODES:-all}"
TXGEN_ANVIL_BOOTSTRAP_ADMIN="${TXGEN_ANVIL_BOOTSTRAP_ADMIN:-auto}"
TXGEN_MAX_FEE_PER_GAS="${TXGEN_MAX_FEE_PER_GAS:-100000000000}"
TXGEN_MAX_PRIORITY_FEE_PER_GAS="${TXGEN_MAX_PRIORITY_FEE_PER_GAS:-100000000000}"
TXGEN_MNEMONIC="${TXGEN_MNEMONIC:-test test test test test test test test test test test junk}"
TXGEN_OUTBOX="${TXGEN_OUTBOX:-0x1c00000000000000000000000000000000000002}"
TIP403_REGISTRY="${TIP403_REGISTRY:-0x403c000000000000000000000000000000000000}"
TXGEN_REPORT_DIR="${TXGEN_REPORT_DIR:-$ZONE_DATADIR/txgen-reports}"

for pair in \
  "COUNT:$COUNT" \
  "TPS:$TPS" \
  "MAX_CONCURRENT:$MAX_CONCURRENT" \
  "DRAIN_TIMEOUT:$DRAIN_TIMEOUT" \
  "SYNC_TIMEOUT:$SYNC_TIMEOUT" \
  "TXGEN_NONCE_LANES:$TXGEN_NONCE_LANES" \
  "TXGEN_DEPOSIT_AMOUNT:$TXGEN_DEPOSIT_AMOUNT" \
  "TXGEN_WITHDRAWAL_AMOUNT:$TXGEN_WITHDRAWAL_AMOUNT" \
  "TXGEN_TRANSFER_AMOUNT:$TXGEN_TRANSFER_AMOUNT" \
  "TXGEN_TRANSFER_FEE_BUFFER:$TXGEN_TRANSFER_FEE_BUFFER" \
  "TXGEN_TRANSFER_ACCOUNTS:$TXGEN_TRANSFER_ACCOUNTS"; do
  require_uint "${pair%%:*}" "${pair#*:}"
done

[ "$COUNT" -gt 0 ] || die "COUNT must be at least 1"
[ "$TXGEN_NONCE_LANES" -gt 1 ] || die "TXGEN_NONCE_LANES must be greater than 1"
[ "$TXGEN_TRANSFER_AMOUNT" -gt 0 ] || die "TXGEN_TRANSFER_AMOUNT must be at least 1"
[ "$TXGEN_TRANSFER_ACCOUNTS" -gt 0 ] || die "TXGEN_TRANSFER_ACCOUNTS must be at least 1"
parse_policy_modes
parse_anvil_admin_bootstrap

for command in cast jq awk tr date sleep mkdir; do
  command -v "$command" >/dev/null || die "required command '$command' is not installed"
done
[ -f "$ZONE_JSON" ] || die "zone metadata not found at $ZONE_JSON; start tempo-zone dev first or set ZONE_DATADIR/ZONE_JSON to its datadir"
[ -x "$TXGEN_TEMPO_BIN" ] || die "txgen-tempo not found at $TXGEN_TEMPO_BIN; run: cargo build --release --manifest-path '$TXGEN_DIR/Cargo.toml'"
[ -x "$BENCH_BIN" ] || die "bench not found at $BENCH_BIN; run: cargo build --release --manifest-path '$TXGEN_DIR/Cargo.toml'"

TXGEN_PORTAL="$(jq -er '.portal' "$ZONE_JSON")"
ZONE_RPC_URL="${ZONE_RPC_URL:-$(jq -er '.rpcUrl' "$ZONE_JSON")}"
TXGEN_TOKEN="${TXGEN_TOKEN:-$(jq -er '.initialToken' "$ZONE_JSON")}"
ZONE_ADMIN="$(jq -er '.admin' "$ZONE_JSON")"
TXGEN_ACCOUNT="$(cast wallet address --mnemonic "$TXGEN_MNEMONIC" --mnemonic-index 0)"
TXGEN_ALLOWED_RECIPIENT="${TXGEN_ALLOWED_RECIPIENT:-$(cast wallet address --mnemonic "$TXGEN_MNEMONIC" --mnemonic-index 1)}"
TXGEN_DENIED_RECIPIENT="${TXGEN_DENIED_RECIPIENT:-$(cast wallet address --mnemonic "$TXGEN_MNEMONIC" --mnemonic-index 2)}"
TXGEN_THROUGHPUT_RECIPIENT="${TXGEN_THROUGHPUT_RECIPIENT:-$(cast wallet address --mnemonic "$TXGEN_MNEMONIC" --mnemonic-index "$TXGEN_TRANSFER_ACCOUNTS")}"
TXGEN_ACTIVE_ACCOUNTS=1
ACTIVE_TRANSFER_ACCOUNTS=("$TXGEN_ACCOUNT")

if [ "$(printf '%s' "$TXGEN_ACCOUNT" | lowercase)" != "$(printf '%s' "$ZONE_ADMIN" | lowercase)" ]; then
  die "txgen mnemonic account $TXGEN_ACCOUNT is not zone dev/admin account $ZONE_ADMIN; use the default tempo-zone dev key or set TXGEN_MNEMONIC to the matching mnemonic"
fi
[ "$(printf '%s' "$TXGEN_ALLOWED_RECIPIENT" | lowercase)" != "$(printf '%s' "$TXGEN_DENIED_RECIPIENT" | lowercase)" ] || \
  die "TXGEN_ALLOWED_RECIPIENT and TXGEN_DENIED_RECIPIENT must differ"

cast block-number --rpc-url "$L1_HTTP_URL" >/dev/null
cast block-number --rpc-url "$ZONE_RPC_URL" >/dev/null
[ "$(cast code "$TXGEN_PORTAL" --rpc-url "$L1_HTTP_URL")" != "0x" ] || die "no ZonePortal code at $TXGEN_PORTAL on $L1_HTTP_URL"

L1_CHAIN_ID="$(cast chain-id --rpc-url "$L1_HTTP_URL")"
L2_CHAIN_ID="$(cast chain-id --rpc-url "$ZONE_RPC_URL")"
ENABLED_TOKEN_COUNT="$(cast call "$TXGEN_PORTAL" 'enabledTokenCount()(uint256)' --rpc-url "$L1_HTTP_URL" | first_word)"
require_uint "enabledTokenCount" "$ENABLED_TOKEN_COUNT"
[ "$ENABLED_TOKEN_COUNT" -gt 0 ] || die "portal $TXGEN_PORTAL has no enabled tokens"

ENABLED_TOKENS=()
token_index=0
while [ "$token_index" -lt "$ENABLED_TOKEN_COUNT" ]; do
  token="$(cast call "$TXGEN_PORTAL" 'enabledTokenAt(uint256)(address)' "$token_index" --rpc-url "$L1_HTTP_URL")"
  ENABLED_TOKENS[${#ENABLED_TOKENS[@]}]="$token"
  token_index=$((token_index + 1))
done

token_is_enabled="$(cast call "$TXGEN_PORTAL" 'isTokenEnabled(address)(bool)' "$TXGEN_TOKEN" --rpc-url "$L1_HTTP_URL")"
[ "$token_is_enabled" = "true" ] || die "TXGEN_TOKEN $TXGEN_TOKEN is not enabled on portal $TXGEN_PORTAL"

mkdir -p "$TXGEN_REPORT_DIR"

export TXGEN_MNEMONIC TXGEN_NONCE_LANES TXGEN_MAX_FEE_PER_GAS
export TXGEN_MAX_PRIORITY_FEE_PER_GAS TXGEN_PORTAL TXGEN_OUTBOX
export TXGEN_DEPOSIT_AMOUNT TXGEN_WITHDRAWAL_AMOUNT TXGEN_TRANSFER_AMOUNT TXGEN_TOKEN
export TXGEN_ACTIVE_ACCOUNTS

run_workload() {
  local spec="$1"
  local rpc="$2"
  local label="$3"
  local chain_id="$4"
  local workload_count="${5:-$COUNT}"
  local expected="${6:-success}"
  local report_name report sent success failed

  export TXGEN_CHAIN_ID="$chain_id"
  report_name="$(printf '%s' "$label" | tr -c '[:alnum:]_.-' '_')"
  report="$TXGEN_REPORT_DIR/$report_name.json"
  LAST_REPORT="$report"

  echo
  echo "==> $label: count=$workload_count expected=$expected tps=$TPS rpc=$rpc"
  "$TXGEN_TEMPO_BIN" generate \
    --spec "$SCRIPT_DIR/txgen/$spec" \
    --count "$workload_count" \
    --rpc "$rpc" \
    | "$BENCH_BIN" send \
      --rpc-url "$rpc" \
      --tps "$TPS" \
      --max-concurrent "$MAX_CONCURRENT" \
      --retries 0 \
      --drain-timeout "$DRAIN_TIMEOUT" \
      --report "json:$report" \
      --metadata "zone-action=$label"

  sent="$(jq -er '.sent' "$report")"
  success="$(jq -er '.success' "$report")"
  failed="$(jq -er '.failed' "$report")"
  require_uint "$label sent count" "$sent"
  require_uint "$label success count" "$success"
  require_uint "$label failure count" "$failed"
  [ "$sent" -eq "$workload_count" ] || die "$label sent $sent transactions; expected $workload_count (report: $report)"

  case "$expected" in
    success)
      [ "$success" -eq "$workload_count" ] && [ "$failed" -eq 0 ] || \
        die "$label had success=$success failed=$failed; expected every transaction to succeed (report: $report)"
      ;;
    failure)
      # Bench always records successful RPC submission in `success`. It only
      # adds receipt failures when a generated transaction has an inclusion
      # scheduling key, so normal stress templates usually leave `failed` at
      # zero even for reverted receipts. Callers must verify receipt status or
      # state after this submission check.
      [ "$success" -eq "$workload_count" ] || \
        die "$label had accepted=$success failed=$failed; expected $workload_count accepted submissions (report: $report)"
      { [ "$failed" -eq 0 ] || [ "$failed" -eq "$workload_count" ]; } || \
        die "$label had partial bench failure accounting: accepted=$success failed=$failed (report: $report)"
      ;;
    *) die "internal error: unknown workload expectation '$expected'" ;;
  esac

  echo "$label verified: sent=$sent accepted=$success reverted-or-rejected=$failed report=$report"
}

verify_transfer_receipts() {
  local token="$1"
  local recipient="$2"
  local expected="$3"
  local calldata start_block end_block block_number block_hex block_json receipts_json counts
  local block_total block_succeeded block_reverted sender_json total=0 succeeded=0 reverted=0

  calldata="$(cast calldata 'transfer(address,uint256)' "$recipient" "$TXGEN_TRANSFER_AMOUNT")"
  sender_json="$(printf '%s\n' "${ACTIVE_TRANSFER_ACCOUNTS[@]}" | \
    jq -Rsc 'split("\n") | map(select(length > 0) | ascii_downcase)')"
  start_block="$(jq -er '.run_stats.start_block' "$LAST_REPORT")"
  end_block="$(jq -er '.run_stats.end_block' "$LAST_REPORT")"
  require_uint "transfer report start block" "$start_block"
  require_uint "transfer report end block" "$end_block"

  block_number="$start_block"
  while [ "$block_number" -le "$end_block" ]; do
    block_hex="$(printf '0x%x' "$block_number")"
    block_json="$(cast rpc --raw --rpc-url "$ZONE_RPC_URL" \
      eth_getBlockByNumber "[\"$block_hex\",true]")"
    receipts_json="$(cast rpc --raw --rpc-url "$ZONE_RPC_URL" \
      eth_getBlockReceipts "[\"$block_hex\"]")"
    counts="$(jq -cn \
      --argjson block "$block_json" \
      --argjson receipts "$receipts_json" \
      --argjson senders "$sender_json" \
      --arg to "$(printf '%s' "$token" | lowercase)" \
      --arg calldata "$(printf '%s' "$calldata" | lowercase)" '
        [
          $block.transactions[]
          | select((.from // "" | ascii_downcase) as $sender | $senders | index($sender))
          | select(any(.calls[]?;
              (.to // "" | ascii_downcase) == $to and
              (.input // "" | ascii_downcase) == $calldata))
          | .hash
        ] as $hashes
        | [
            $receipts[]
            | select(.transactionHash as $hash | $hashes | index($hash))
            | .status
          ]
        | {
            total: length,
            succeeded: map(select(. == "0x1")) | length,
            reverted: map(select(. == "0x0")) | length
          }
      ')"
    block_total="$(printf '%s' "$counts" | jq -er '.total')"
    block_succeeded="$(printf '%s' "$counts" | jq -er '.succeeded')"
    block_reverted="$(printf '%s' "$counts" | jq -er '.reverted')"
    total=$((total + block_total))
    succeeded=$((succeeded + block_succeeded))
    reverted=$((reverted + block_reverted))
    block_number=$((block_number + 1))
  done

  [ "$total" -eq "$COUNT" ] || \
    die "found $total matching TIP-20 transfer receipts; expected $COUNT in blocks $start_block..$end_block (report: $LAST_REPORT)"
  case "$expected" in
    success)
      [ "$succeeded" -eq "$COUNT" ] && [ "$reverted" -eq 0 ] || \
        die "TIP-20 receipts had succeeded=$succeeded reverted=$reverted; expected all $COUNT to succeed"
      ;;
    failure)
      [ "$succeeded" -eq 0 ] && [ "$reverted" -eq "$COUNT" ] || \
        die "TIP-20 receipts had succeeded=$succeeded reverted=$reverted; expected all $COUNT to revert"
      ;;
    *) die "internal error: unknown receipt expectation '$expected'" ;;
  esac
  echo "TIP-20 receipts verified: token=$token recipient=$recipient succeeded=$succeeded reverted=$reverted blocks=$start_block..$end_block"
}

zone_balance() {
  local token="$1"
  local account="${2:-$TXGEN_ACCOUNT}"
  local rpc="${3:-$ZONE_RPC_URL}"
  cast call "$token" 'balanceOf(address)(uint256)' "$account" \
    --from "$account" \
    --rpc-url "$rpc" \
    | first_word
}

verify_l1_batch_settlement() {
  local batch_before="$1"
  local started="$2"
  local end_block deadline batch_after portal_hash l2_head block_number block_hash
  local elapsed batch_delta bench_elapsed bench_tps zone_tps zone_txs

  end_block="$(jq -er '.run_stats.end_block' "$LAST_REPORT")"
  require_uint "transfer report end block" "$end_block"
  deadline=$(( $(date +%s) + SYNC_TIMEOUT ))

  while :; do
    batch_after="$(cast call "$TXGEN_PORTAL" 'withdrawalBatchIndex()(uint64)' --rpc-url "$L1_HTTP_URL" | first_word)"
    require_uint "portal withdrawal batch index" "$batch_after"
    portal_hash="$(cast call "$TXGEN_PORTAL" 'blockHash()(bytes32)' --rpc-url "$L1_HTTP_URL" | first_word)"
    l2_head="$(cast block-number --rpc-url "$ZONE_RPC_URL")"

    if [ "$batch_after" -gt "$batch_before" ] && [ "$l2_head" -ge "$end_block" ]; then
      block_number="$end_block"
      while [ "$block_number" -le "$l2_head" ]; do
        block_hash="$(cast block "$block_number" --field hash --rpc-url "$ZONE_RPC_URL")"
        if [ "$(printf '%s' "$block_hash" | lowercase)" = "$(printf '%s' "$portal_hash" | lowercase)" ]; then
          elapsed=$(( $(date +%s) - started ))
          batch_delta=$((batch_after - batch_before))
          bench_elapsed="$(jq -er '.elapsed_secs' "$LAST_REPORT")"
          bench_tps="$(jq -er '.tps' "$LAST_REPORT")"
          zone_tps="$(jq -er '.run_stats.avg_tps' "$LAST_REPORT")"
          zone_txs="$(jq -er '.run_stats.total_txs' "$LAST_REPORT")"
          echo "L1 batch settlement verified: target-tps=$TPS count=$COUNT txgen-tps=$bench_tps txgen-seconds=$bench_elapsed zone-observed-tps=$zone_tps zone-observed-txs=$zone_txs transfer-end-block=$end_block settled-through-block=$block_number l1-batches=$batch_delta settlement-seconds=$elapsed batch-index=$batch_after"
          return
        fi
        block_number=$((block_number + 1))
      done
    fi

    [ "$(date +%s)" -lt "$deadline" ] || \
      die "timed out waiting for L1 batch settlement through L2 block $end_block (batch before=$batch_before after=$batch_after portal hash=$portal_hash L2 head=$l2_head)"
    sleep 1
  done
}

run_deposits_for_token() {
  local token="$1"
  local deposit_count="${2:-$COUNT}"
  local deposit_fee balance_before expected_balance deadline balance_now

  deposit_fee="$(cast call "$TXGEN_PORTAL" 'calculateDepositFee()(uint128)' --rpc-url "$L1_HTTP_URL" | first_word)"
  require_uint "deposit fee" "$deposit_fee"
  [ "$TXGEN_DEPOSIT_AMOUNT" -gt "$deposit_fee" ] || die "TXGEN_DEPOSIT_AMOUNT must exceed portal fee $deposit_fee"

  balance_before="$(zone_balance "$token")"
  require_uint "zone balance" "$balance_before"
  export TXGEN_TOKEN="$token"
  run_workload l1-deposits.yaml "$L1_HTTP_URL" "l1-deposits:$token" "$L1_CHAIN_ID" "$deposit_count"

  expected_balance=$((balance_before + deposit_count * (TXGEN_DEPOSIT_AMOUNT - deposit_fee)))
  deadline=$(( $(date +%s) + SYNC_TIMEOUT ))
  while :; do
    balance_now="$(zone_balance "$token")"
    if [ "$balance_now" -ge "$expected_balance" ]; then
      echo "L1 deposits reached L2: token=$token balance=$balance_now expected-at-least=$expected_balance"
      break
    fi
    [ "$(date +%s)" -lt "$deadline" ] || die "timed out waiting for deposits on L2 (token=$token balance=$balance_now expected=$expected_balance)"
    sleep 1
  done
}

run_deposits() {
  run_deposits_for_token "$TXGEN_TOKEN" "$COUNT"
}

run_withdrawals() {
  local head_before l1_block_before deadline head tail logs expected_data processed_count
  export TXGEN_TOKEN="$TXGEN_TOKEN_OVERRIDE"
  head_before="$(cast call "$TXGEN_PORTAL" 'withdrawalQueueHead()(uint256)' --rpc-url "$L1_HTTP_URL" | first_word)"
  require_uint "withdrawal queue head" "$head_before"
  l1_block_before="$(cast block-number --rpc-url "$L1_HTTP_URL")"
  require_uint "L1 block before withdrawals" "$l1_block_before"

  run_workload l2-withdrawals.yaml "$ZONE_RPC_URL" l2-withdrawals "$L2_CHAIN_ID"

  expected_data="$(cast abi-encode 'f(address,uint128,bool)' "$TXGEN_TOKEN_OVERRIDE" "$TXGEN_WITHDRAWAL_AMOUNT" true | lowercase)"
  deadline=$(( $(date +%s) + SYNC_TIMEOUT ))
  while :; do
    head="$(cast call "$TXGEN_PORTAL" 'withdrawalQueueHead()(uint256)' --rpc-url "$L1_HTTP_URL" | first_word)"
    tail="$(cast call "$TXGEN_PORTAL" 'withdrawalQueueTail()(uint256)' --rpc-url "$L1_HTTP_URL" | first_word)"
    logs="$(cast logs --json \
      --rpc-url "$L1_HTTP_URL" \
      --address "$TXGEN_PORTAL" \
      --from-block "$l1_block_before" \
      'WithdrawalProcessed(address,bytes32,address,uint128,bool)' \
      "$TXGEN_ACCOUNT")"
    processed_count="$(printf '%s' "$logs" | jq -er --arg data "$expected_data" \
      '[.[] | select((.data | ascii_downcase) == $data)] | length')"
    require_uint "matching WithdrawalProcessed event count" "$processed_count"
    if [ "$processed_count" -eq "$COUNT" ] && [ "$head" -gt "$head_before" ] && [ "$head" -eq "$tail" ]; then
      echo "L2 withdrawals processed on L1: events=$processed_count head-before=$head_before head=$head tail=$tail"
      break
    fi
    [ "$(date +%s)" -lt "$deadline" ] || \
      die "timed out waiting for L1 withdrawal processing (events=$processed_count expected=$COUNT head-before=$head_before head=$head tail=$tail)"
    sleep 1
  done
}

transfer_probe_allows() {
  local token="$1"
  local recipient="$2"
  local output

  if output="$(cast call "$token" 'transfer(address,uint256)(bool)' "$recipient" 0 \
    --from "$TXGEN_ACCOUNT" \
    --rpc-url "$ZONE_RPC_URL" 2>/dev/null)"; then
    [ "$(printf '%s\n' "$output" | first_word)" = "true" ]
  else
    return 1
  fi
}

wait_for_transfer_outcome() {
  local token="$1"
  local recipient="$2"
  local expected="$3"
  local label="$4"
  local deadline observed

  deadline=$(( $(date +%s) + SYNC_TIMEOUT ))
  while :; do
    if transfer_probe_allows "$token" "$recipient"; then
      observed="allowed"
    else
      observed="denied"
    fi
    if [ "$observed" = "$expected" ]; then
      echo "TIP-20 policy effect synchronized to L2: token=$token probe=$label outcome=$observed"
      return
    fi
    [ "$(date +%s)" -lt "$deadline" ] || \
      die "timed out waiting for $label transfer to be $expected on L2 token $token (observed=$observed)"
    sleep 1
  done
}

wait_for_policy_effect() {
  local token="$1"
  local policy_id="$2"
  local mode="$3"
  local l1_policy

  l1_policy="$(cast call "$token" 'transferPolicyId()(uint64)' \
    --rpc-url "$L1_HTTP_URL" \
    | first_word)"
  [ "$l1_policy" -eq "$policy_id" ] || \
    die "L1 token $token exposes policy $l1_policy after assigning $policy_id"

  case "$mode" in
    allow-all)
      wait_for_transfer_outcome "$token" "$TXGEN_ALLOWED_RECIPIENT" allowed allowed-target
      ;;
    reject-all)
      wait_for_transfer_outcome "$token" "$TXGEN_ALLOWED_RECIPIENT" denied allowed-target
      ;;
    whitelist|blacklist|compound)
      wait_for_transfer_outcome "$token" "$TXGEN_ALLOWED_RECIPIENT" allowed allowed-target
      wait_for_transfer_outcome "$token" "$TXGEN_DENIED_RECIPIENT" denied denied-target
      ;;
    *) die "internal error: unknown policy effect mode '$mode'" ;;
  esac
  echo "TIP-403 assignment verified through L2 transfer enforcement: token=$token policy=$policy_id mode=$mode"
}

set_token_policy() {
  local token="$1"
  local policy_id="$2"
  local update_count="$3"
  local label="$4"
  local mode="$5"

  export TXGEN_TOKEN="$token"
  export TXGEN_POLICY_ID="$policy_id"
  run_workload l1-policy-updates.yaml "$L1_HTTP_URL" "$label:$token" "$L1_CHAIN_ID" "$update_count"
  wait_for_policy_effect "$token" "$policy_id" "$mode"
}

create_simple_policy() {
  local policy_type="$1"
  local label="$2"
  local next_policy_id policy_exists observed_type

  next_policy_id="$(cast call "$TIP403_REGISTRY" 'policyIdCounter()(uint64)' --rpc-url "$L1_HTTP_URL" | first_word)"
  require_uint "next TIP-403 policy ID" "$next_policy_id"
  export TXGEN_POLICY_TYPE="$policy_type"
  run_workload l1-create-policy.yaml "$L1_HTTP_URL" "l1-create-$label" "$L1_CHAIN_ID" 1

  policy_exists="$(cast call "$TIP403_REGISTRY" 'policyExists(uint64)(bool)' "$next_policy_id" --rpc-url "$L1_HTTP_URL")"
  [ "$policy_exists" = "true" ] || die "TIP-403 policy $next_policy_id was not created"
  observed_type="$(cast call "$TIP403_REGISTRY" 'policyData(uint64)(uint8,address)' "$next_policy_id" --rpc-url "$L1_HTTP_URL" | first_word)"
  [ "$observed_type" -eq "$policy_type" ] || die "TIP-403 policy $next_policy_id has type $observed_type; expected $policy_type"
  CREATED_POLICY_ID="$next_policy_id"
}

modify_policy_member() {
  local spec="$1"
  local label="$2"
  local policy_id="$3"
  local account="$4"
  local member="$5"

  export TXGEN_POLICY_ID="$policy_id"
  export TXGEN_POLICY_ACCOUNT="$account"
  export TXGEN_POLICY_MEMBER="$member"
  run_workload "$spec" "$L1_HTTP_URL" "$label:$policy_id:$account" "$L1_CHAIN_ID" 1
}

create_policy_matrix() {
  local next_policy_id policy_exists compound_output
  local compound_sender compound_recipient compound_mint_recipient

  WHITELIST_POLICY_ID=""
  BLACKLIST_POLICY_ID=""
  COMPOUND_POLICY_ID=""

  if has_policy_mode whitelist || has_policy_mode compound; then
    create_simple_policy 0 whitelist
    WHITELIST_POLICY_ID="$CREATED_POLICY_ID"
    modify_policy_member l1-modify-whitelist.yaml l1-whitelist-sender "$WHITELIST_POLICY_ID" "$TXGEN_ACCOUNT" true
    modify_policy_member l1-modify-whitelist.yaml l1-whitelist-recipient "$WHITELIST_POLICY_ID" "$TXGEN_ALLOWED_RECIPIENT" true
  fi

  if has_policy_mode blacklist || has_policy_mode compound; then
    create_simple_policy 1 blacklist
    BLACKLIST_POLICY_ID="$CREATED_POLICY_ID"
    modify_policy_member l1-modify-blacklist.yaml l1-blacklist-recipient "$BLACKLIST_POLICY_ID" "$TXGEN_DENIED_RECIPIENT" true
  fi

  if has_policy_mode compound; then
    next_policy_id="$(cast call "$TIP403_REGISTRY" 'policyIdCounter()(uint64)' --rpc-url "$L1_HTTP_URL" | first_word)"
    require_uint "next TIP-403 compound policy ID" "$next_policy_id"
    export TXGEN_SENDER_POLICY_ID="$WHITELIST_POLICY_ID"
    export TXGEN_RECIPIENT_POLICY_ID="$BLACKLIST_POLICY_ID"
    export TXGEN_MINT_RECIPIENT_POLICY_ID=1
    run_workload l1-create-compound-policy.yaml "$L1_HTTP_URL" l1-create-compound "$L1_CHAIN_ID" 1

    policy_exists="$(cast call "$TIP403_REGISTRY" 'policyExists(uint64)(bool)' "$next_policy_id" --rpc-url "$L1_HTTP_URL")"
    [ "$policy_exists" = "true" ] || die "TIP-403 compound policy $next_policy_id was not created"
    compound_output="$(cast call "$TIP403_REGISTRY" \
      'compoundPolicyData(uint64)(uint64,uint64,uint64)' "$next_policy_id" \
      --rpc-url "$L1_HTTP_URL")"
    compound_sender="$(printf '%s\n' "$compound_output" | awk 'NR == 1 {print $1}')"
    compound_recipient="$(printf '%s\n' "$compound_output" | awk 'NR == 2 {print $1}')"
    compound_mint_recipient="$(printf '%s\n' "$compound_output" | awk 'NR == 3 {print $1}')"
    [ "$compound_sender" = "$WHITELIST_POLICY_ID" ] && \
      [ "$compound_recipient" = "$BLACKLIST_POLICY_ID" ] && \
      [ "$compound_mint_recipient" = "1" ] || \
      die "TIP-403 compound policy $next_policy_id has unexpected sub-policies: $compound_output"
    COMPOUND_POLICY_ID="$next_policy_id"
  fi
}

assert_authorized() {
  local function_signature="$1"
  local policy_id="$2"
  local account="$3"
  local expected="$4"
  local observed

  observed="$(cast call "$TIP403_REGISTRY" "$function_signature" "$policy_id" "$account" --rpc-url "$ZONE_RPC_URL")"
  [ "$observed" = "$expected" ] || die "$function_signature policy=$policy_id account=$account returned $observed; expected $expected"
}

run_allowed_transfers() {
  local token="$1"
  local mode="$2"
  local balance_before balance_after expected_balance

  balance_before="$(zone_balance "$token" "$TXGEN_ALLOWED_RECIPIENT")"
  require_uint "allowed recipient balance" "$balance_before"
  export TXGEN_TOKEN="$token"
  export TXGEN_TRANSFER_RECIPIENT="$TXGEN_ALLOWED_RECIPIENT"
  run_workload l2-tip20-transfers.yaml "$ZONE_RPC_URL" "l2-tip20-$mode-allowed:$token" "$L2_CHAIN_ID" "$COUNT" success
  verify_transfer_receipts "$token" "$TXGEN_ALLOWED_RECIPIENT" success

  balance_after="$(zone_balance "$token" "$TXGEN_ALLOWED_RECIPIENT")"
  expected_balance=$((balance_before + COUNT * TXGEN_TRANSFER_AMOUNT))
  [ "$balance_after" -eq "$expected_balance" ] || \
    die "$mode allowed transfer balance mismatch for $token: got $balance_after expected $expected_balance"
  echo "TIP-20 allowed transfers verified: token=$token mode=$mode recipient-balance=$balance_after"
}

run_denied_transfers() {
  local token="$1"
  local mode="$2"
  local balance_before balance_after

  balance_before="$(zone_balance "$token" "$TXGEN_DENIED_RECIPIENT")"
  require_uint "denied recipient balance" "$balance_before"
  export TXGEN_TOKEN="$token"
  export TXGEN_TRANSFER_RECIPIENT="$TXGEN_DENIED_RECIPIENT"
  run_workload l2-tip20-transfers.yaml "$ZONE_RPC_URL" "l2-tip20-$mode-denied:$token" "$L2_CHAIN_ID" "$COUNT" failure
  verify_transfer_receipts "$token" "$TXGEN_DENIED_RECIPIENT" failure

  balance_after="$(zone_balance "$token" "$TXGEN_DENIED_RECIPIENT")"
  [ "$balance_after" -eq "$balance_before" ] || \
    die "$mode denied transfer changed recipient balance for $token: before=$balance_before after=$balance_after"
  echo "TIP-20 denied transfers verified: token=$token mode=$mode recipient-balance=$balance_after"
}

policy_transfer_workload_count() {
  local workloads=0 mode
  for mode in "${POLICY_MODES[@]}"; do
    case "$mode" in
      allow-all|reject-all) workloads=$((workloads + 1)) ;;
      whitelist|blacklist|compound) workloads=$((workloads + 2)) ;;
    esac
  done
  echo "$workloads"
}

policy_success_workload_count() {
  local workloads=0 mode
  for mode in "${POLICY_MODES[@]}"; do
    case "$mode" in
      allow-all|whitelist|blacklist|compound) workloads=$((workloads + 1)) ;;
    esac
  done
  echo "$workloads"
}

ensure_account0_transfer_funds() {
  local token="$1"
  local required="$2"
  local balance deposit_fee net_per_deposit missing deposit_count l1_balance gross

  balance="$(zone_balance "$token")"
  require_uint "zone sender balance" "$balance"
  if [ "$balance" -ge "$required" ]; then
    echo "TIP-20 transfer funding ready: token=$token balance=$balance required=$required"
    return
  fi

  deposit_fee="$(cast call "$TXGEN_PORTAL" 'calculateDepositFee()(uint128)' --rpc-url "$L1_HTTP_URL" | first_word)"
  require_uint "deposit fee" "$deposit_fee"
  [ "$TXGEN_DEPOSIT_AMOUNT" -gt "$deposit_fee" ] || die "TXGEN_DEPOSIT_AMOUNT must exceed portal fee $deposit_fee"
  net_per_deposit=$((TXGEN_DEPOSIT_AMOUNT - deposit_fee))
  missing=$((required - balance))
  deposit_count=$(((missing + net_per_deposit - 1) / net_per_deposit))
  gross=$((deposit_count * TXGEN_DEPOSIT_AMOUNT))
  l1_balance="$(cast call "$token" 'balanceOf(address)(uint256)' "$TXGEN_ACCOUNT" --rpc-url "$L1_HTTP_URL" | first_word)"
  require_uint "L1 sender balance" "$l1_balance"
  uint_ge "$l1_balance" "$gross" || \
    die "insufficient L1 $token balance to fund policy transfers: have=$l1_balance need=$gross ($deposit_count deposits)"

  echo "Funding transfer source on L2: token=$token deposits=$deposit_count required=$required"
  run_deposits_for_token "$token" "$deposit_count"
}

ensure_transfer_funds() {
  local token="$1"
  local transfer_workloads="$2"
  local success_workloads="$3"
  local required

  required=$((COUNT * (success_workloads * TXGEN_TRANSFER_AMOUNT + transfer_workloads * TXGEN_TRANSFER_FEE_BUFFER)))
  ensure_account0_transfer_funds "$token" "$required"
}

prepare_throughput_accounts() {
  local index account

  ACTIVE_TRANSFER_ACCOUNTS=()
  index=0
  while [ "$index" -lt "$TXGEN_TRANSFER_ACCOUNTS" ]; do
    account="$(cast wallet address --mnemonic "$TXGEN_MNEMONIC" --mnemonic-index "$index")"
    [ "$(printf '%s' "$account" | lowercase)" != "$(printf '%s' "$TXGEN_THROUGHPUT_RECIPIENT" | lowercase)" ] || \
      die "TXGEN_THROUGHPUT_RECIPIENT must not be one of the $TXGEN_TRANSFER_ACCOUNTS throughput senders"
    ACTIVE_TRANSFER_ACCOUNTS[${#ACTIVE_TRANSFER_ACCOUNTS[@]}]="$account"
    index=$((index + 1))
  done
  TXGEN_ACTIVE_ACCOUNTS="$TXGEN_TRANSFER_ACCOUNTS"
  export TXGEN_ACTIVE_ACCOUNTS
}

fund_throughput_accounts() {
  local token="$1"
  local per_account_required total_missing source_required index account balance missing
  local original_amount deadline balance_now
  local -a missing_by_index

  # Random selection can legally choose one signer for the entire workload.
  # Fund every signer for that worst case so selection skew cannot invalidate
  # a throughput result.
  per_account_required=$((COUNT * (TXGEN_TRANSFER_AMOUNT + TXGEN_TRANSFER_FEE_BUFFER)))
  total_missing=0
  missing_by_index=(0)
  index=1
  while [ "$index" -lt "${#ACTIVE_TRANSFER_ACCOUNTS[@]}" ]; do
    account="${ACTIVE_TRANSFER_ACCOUNTS[$index]}"
    balance="$(zone_balance "$token" "$account")"
    require_uint "throughput sender balance" "$balance"
    if [ "$balance" -lt "$per_account_required" ]; then
      missing=$((per_account_required - balance))
    else
      missing=0
    fi
    missing_by_index[$index]="$missing"
    total_missing=$((total_missing + missing))
    index=$((index + 1))
  done

  source_required=$((per_account_required + total_missing))
  ensure_account0_transfer_funds "$token" "$source_required"

  original_amount="$TXGEN_TRANSFER_AMOUNT"
  TXGEN_ACTIVE_ACCOUNTS=1
  export TXGEN_ACTIVE_ACCOUNTS
  index=1
  while [ "$index" -lt "${#ACTIVE_TRANSFER_ACCOUNTS[@]}" ]; do
    missing="${missing_by_index[$index]}"
    if [ "$missing" -gt 0 ]; then
      account="${ACTIVE_TRANSFER_ACCOUNTS[$index]}"
      balance="$(zone_balance "$token" "$account")"
      TXGEN_TRANSFER_AMOUNT="$missing"
      TXGEN_TRANSFER_RECIPIENT="$account"
      export TXGEN_TRANSFER_AMOUNT TXGEN_TRANSFER_RECIPIENT
      run_workload l2-tip20-transfers.yaml "$ZONE_RPC_URL" \
        "l2-fund-throughput-sender-$index:$token" "$L2_CHAIN_ID" 1 success
      deadline=$(( $(date +%s) + SYNC_TIMEOUT ))
      while :; do
        balance_now="$(zone_balance "$token" "$account")"
        [ "$balance_now" -eq $((balance + missing)) ] && break
        [ "$(date +%s)" -lt "$deadline" ] || \
          die "timed out funding throughput sender $account (balance=$balance_now expected=$((balance + missing)))"
        sleep 1
      done
      echo "TIP-20 throughput sender funded: index=$index account=$account balance=$balance_now"
    fi
    index=$((index + 1))
  done
  TXGEN_TRANSFER_AMOUNT="$original_amount"
  TXGEN_ACTIVE_ACCOUNTS="$TXGEN_TRANSFER_ACCOUNTS"
  export TXGEN_TRANSFER_AMOUNT TXGEN_ACTIVE_ACCOUNTS
}

ensure_policy_transfer_funds() {
  local token="$1"
  ensure_transfer_funds "$token" \
    "$(policy_transfer_workload_count)" \
    "$(policy_success_workload_count)"
}

run_policy_mode() {
  local token="$1"
  local mode="$2"

  case "$mode" in
    allow-all)
      set_token_policy "$token" 1 "$COUNT" l1-policy-allow-all "$mode"
      assert_authorized 'isAuthorized(uint64,address)(bool)' 1 "$TXGEN_ACCOUNT" true
      assert_authorized 'isAuthorized(uint64,address)(bool)' 1 "$TXGEN_ALLOWED_RECIPIENT" true
      run_allowed_transfers "$token" "$mode"
      ;;
    reject-all)
      set_token_policy "$token" 0 "$COUNT" l1-policy-reject-all "$mode"
      assert_authorized 'isAuthorized(uint64,address)(bool)' 0 "$TXGEN_ACCOUNT" false
      run_denied_transfers "$token" "$mode"
      ;;
    whitelist)
      set_token_policy "$token" "$WHITELIST_POLICY_ID" "$COUNT" l1-policy-whitelist "$mode"
      assert_authorized 'isAuthorized(uint64,address)(bool)' "$WHITELIST_POLICY_ID" "$TXGEN_ACCOUNT" true
      assert_authorized 'isAuthorized(uint64,address)(bool)' "$WHITELIST_POLICY_ID" "$TXGEN_ALLOWED_RECIPIENT" true
      assert_authorized 'isAuthorized(uint64,address)(bool)' "$WHITELIST_POLICY_ID" "$TXGEN_DENIED_RECIPIENT" false
      run_allowed_transfers "$token" "$mode"
      run_denied_transfers "$token" "$mode"
      ;;
    blacklist)
      set_token_policy "$token" "$BLACKLIST_POLICY_ID" "$COUNT" l1-policy-blacklist "$mode"
      assert_authorized 'isAuthorized(uint64,address)(bool)' "$BLACKLIST_POLICY_ID" "$TXGEN_ACCOUNT" true
      assert_authorized 'isAuthorized(uint64,address)(bool)' "$BLACKLIST_POLICY_ID" "$TXGEN_ALLOWED_RECIPIENT" true
      assert_authorized 'isAuthorized(uint64,address)(bool)' "$BLACKLIST_POLICY_ID" "$TXGEN_DENIED_RECIPIENT" false
      run_allowed_transfers "$token" "$mode"
      run_denied_transfers "$token" "$mode"
      ;;
    compound)
      set_token_policy "$token" "$COMPOUND_POLICY_ID" "$COUNT" l1-policy-compound "$mode"
      assert_authorized 'isAuthorizedSender(uint64,address)(bool)' "$COMPOUND_POLICY_ID" "$TXGEN_ACCOUNT" true
      assert_authorized 'isAuthorizedRecipient(uint64,address)(bool)' "$COMPOUND_POLICY_ID" "$TXGEN_ALLOWED_RECIPIENT" true
      assert_authorized 'isAuthorizedRecipient(uint64,address)(bool)' "$COMPOUND_POLICY_ID" "$TXGEN_DENIED_RECIPIENT" false
      assert_authorized 'isAuthorizedMintRecipient(uint64,address)(bool)' "$COMPOUND_POLICY_ID" "$TXGEN_ACCOUNT" true
      run_allowed_transfers "$token" "$mode"
      run_denied_transfers "$token" "$mode"
      ;;
  esac
}

ensure_token_admin_role() {
  local token="$1"
  local zero_role has_admin client_version inner_slot admin_slot

  zero_role=0x0000000000000000000000000000000000000000000000000000000000000000
  has_admin="$(cast call "$token" 'hasRole(address,bytes32)(bool)' \
    "$TXGEN_ACCOUNT" "$zero_role" --rpc-url "$L1_HTTP_URL")"
  if [ "$has_admin" = "true" ]; then
    return 0
  fi

  case "$ANVIL_ADMIN_BOOTSTRAP_MODE" in
    false)
      die "txgen account $TXGEN_ACCOUNT lacks DEFAULT_ADMIN_ROLE on enabled token $token"
      ;;
    auto)
      client_version="$(cast rpc --rpc-url "$L1_HTTP_URL" web3_clientVersion | tr -d '"')"
      case "$client_version" in
        *[Aa]nvil*) ;;
        *) die "txgen account $TXGEN_ACCOUNT lacks DEFAULT_ADMIN_ROLE on enabled token $token; refusing Anvil-only bootstrap on $client_version" ;;
      esac
      ;;
    true) ;;
  esac

  # Tempo TIP-20 stores role membership as:
  # roles[role][account] = keccak256(abi.encode(role,
  #   keccak256(abi.encode(account, uint256(0)))))
  inner_slot="$(cast index address "$TXGEN_ACCOUNT" 0)"
  admin_slot="$(cast index bytes32 "$zero_role" "$inner_slot")"
  echo "Bootstrapping Anvil TIP-20 admin role: token=$token account=$TXGEN_ACCOUNT slot=$admin_slot"
  cast rpc --rpc-url "$L1_HTTP_URL" anvil_setStorageAt \
    "$token" "$admin_slot" \
    0x0000000000000000000000000000000000000000000000000000000000000001 \
    >/dev/null

  has_admin="$(cast call "$token" 'hasRole(address,bytes32)(bool)' \
    "$TXGEN_ACCOUNT" "$zero_role" --rpc-url "$L1_HTTP_URL")"
  [ "$has_admin" = "true" ] || die "failed to bootstrap DEFAULT_ADMIN_ROLE for $TXGEN_ACCOUNT on Anvil token $token"
}

run_policies() {
  local token mode

  for token in "${ENABLED_TOKENS[@]}"; do
    ensure_token_admin_role "$token"
  done

  create_policy_matrix

  # Make every enabled token safe to mint before adding any L2 transfer funds.
  for token in "${ENABLED_TOKENS[@]}"; do
    set_token_policy "$token" 1 1 l1-policy-funding-allow-all allow-all
    ensure_policy_transfer_funds "$token"
  done

  for token in "${ENABLED_TOKENS[@]}"; do
    for mode in "${POLICY_MODES[@]}"; do
      run_policy_mode "$token" "$mode"
    done
  done

  # Leave bridge traffic usable even when a custom mode list ends in reject-all.
  for token in "${ENABLED_TOKENS[@]}"; do
    set_token_policy "$token" 1 1 l1-policy-restore-allow-all allow-all
  done
  export TXGEN_TOKEN="$TXGEN_TOKEN_OVERRIDE"
}

run_throughput() {
  local batch_before started account
  ensure_token_admin_role "$TXGEN_TOKEN_OVERRIDE"
  set_token_policy "$TXGEN_TOKEN_OVERRIDE" 1 1 l1-policy-throughput-allow-all allow-all
  prepare_throughput_accounts
  TXGEN_ALLOWED_RECIPIENT="$TXGEN_THROUGHPUT_RECIPIENT"
  for account in "${ACTIVE_TRANSFER_ACCOUNTS[@]}"; do
    assert_authorized 'isAuthorized(uint64,address)(bool)' 1 "$account" true
  done
  assert_authorized 'isAuthorized(uint64,address)(bool)' 1 "$TXGEN_THROUGHPUT_RECIPIENT" true
  fund_throughput_accounts "$TXGEN_TOKEN_OVERRIDE"
  echo "TIP-20 throughput senders (${#ACTIVE_TRANSFER_ACCOUNTS[@]}): ${ACTIVE_TRANSFER_ACCOUNTS[*]}"
  echo "TIP-20 throughput recipient: $TXGEN_THROUGHPUT_RECIPIENT"
  batch_before="$(cast call "$TXGEN_PORTAL" 'withdrawalBatchIndex()(uint64)' --rpc-url "$L1_HTTP_URL" | first_word)"
  require_uint "portal withdrawal batch index" "$batch_before"
  started="$(date +%s)"
  run_allowed_transfers "$TXGEN_TOKEN_OVERRIDE" "throughput-${TPS}tps"
  verify_l1_batch_settlement "$batch_before" "$started"
}

# Preserve the requested bridge token while policy workloads iterate all tokens.
TXGEN_TOKEN_OVERRIDE="$TXGEN_TOKEN"

echo "ZonePortal:       $TXGEN_PORTAL"
echo "L1 RPC/chain:     $L1_HTTP_URL / $L1_CHAIN_ID"
echo "L2 RPC/chain:     $ZONE_RPC_URL / $L2_CHAIN_ID"
echo "Txgen account:    $TXGEN_ACCOUNT"
echo "Allowed target:   $TXGEN_ALLOWED_RECIPIENT"
echo "Denied target:    $TXGEN_DENIED_RECIPIENT"
echo "Bridge token:     $TXGEN_TOKEN"
echo "Enabled tokens (${#ENABLED_TOKENS[@]}): ${ENABLED_TOKENS[*]}"
echo "TIP-403 modes:     ${POLICY_MODES[*]}"
echo "Reports:           $TXGEN_REPORT_DIR"

case "$ACTION" in
  deposits) run_deposits ;;
  withdrawals) run_withdrawals ;;
  policies) run_policies ;;
  throughput) run_throughput ;;
  all)
    run_deposits
    run_policies
    run_withdrawals
    ;;
esac
