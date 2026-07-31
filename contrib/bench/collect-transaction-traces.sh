#!/usr/bin/env bash

# Write call traces and storage state diffs for receipt transactions as JSON Lines.
set -Eeuo pipefail

die() { echo "error: $*" >&2; exit 1; }

if (( $# < 3 || $# > 4 )); then
    die "usage: $0 RPC_URL RECEIPTS_JSONL OUTPUT [PARALLELISM]"
fi

rpc_url="$1"
receipts="$2"
output="$3"
parallelism="${4:-4}"
trace_timeout="${ZONES_BENCH_TRACE_TIMEOUT:-30s}"

[[ -f "$receipts" ]] || die "receipt file does not exist: $receipts"
[[ "$parallelism" =~ ^[1-9][0-9]*$ ]] || die "PARALLELISM must be a positive integer"
jq -se 'all(.[]; (.transactionHash | type == "string"))' "$receipts" >/dev/null ||
    die "receipt file contains an invalid transaction hash"

mkdir -p "$(dirname -- "$output")"
temporary="$(mktemp "${output}.tmp.XXXXXX")"
trace_dir="$(mktemp -d "${output}.traces.XXXXXX")"
manifest="$trace_dir/manifest.tsv"
trap 'rm -f -- "$temporary"; rm -rf -- "$trace_dir"' EXIT

jq -r '[input_line_number - 1, .transactionHash, .blockNumber, .transactionIndex] | @tsv' \
    "$receipts" > "$manifest"

trace_one() {
    local record="$1"
    local index transaction_hash block_number transaction_index
    local call_trace state_diff destination

    IFS=$'\t' read -r index transaction_hash block_number transaction_index <<< "$record"
    destination="$(printf '%s/%08d.json' "$trace_dir" "$index")"

    call_trace="$(cast rpc --rpc-url "$rpc_url" debug_traceTransaction \
        "$transaction_hash" \
        "{\"tracer\":\"callTracer\",\"timeout\":\"$trace_timeout\"}")" || {
        echo "error: could not collect call trace for $transaction_hash" >&2
        return 1
    }
    state_diff="$(cast rpc --rpc-url "$rpc_url" debug_traceTransaction \
        "$transaction_hash" \
        "{\"tracer\":\"prestateTracer\",\"tracerConfig\":{\"diffMode\":true},\"timeout\":\"$trace_timeout\"}")" || {
        echo "error: could not collect state diff for $transaction_hash" >&2
        return 1
    }

    jq -cn \
        --arg transactionHash "$transaction_hash" \
        --arg blockNumber "$block_number" \
        --arg transactionIndex "$transaction_index" \
        --argjson callTrace "$call_trace" \
        --argjson stateDiff "$state_diff" \
        '{transactionHash: $transactionHash, blockNumber: $blockNumber,
          transactionIndex: $transactionIndex, callTrace: $callTrace, stateDiff: $stateDiff}' \
        > "$destination"
}
export -f trace_one
export rpc_url trace_dir trace_timeout

if [[ -s "$manifest" ]]; then
    xargs -r -d '\n' -n 1 -P "$parallelism" bash -c 'trace_one "$1"' _ < "$manifest"
fi

while IFS= read -r trace; do
    cat -- "$trace" >> "$temporary"
done < <(find "$trace_dir" -maxdepth 1 -name '*.json' -type f -print | sort)

expected="$(wc -l < "$receipts")"
actual="$(wc -l < "$temporary")"
[[ "$actual" == "$expected" ]] ||
    die "wrote $actual traces for $expected receipts"

mv -- "$temporary" "$output"
rm -rf -- "$trace_dir"
trap - EXIT
echo "wrote $actual transaction traces to $output"
