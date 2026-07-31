#!/usr/bin/env bash

# Write every transaction receipt in an inclusive block range as JSON Lines.
set -Eeuo pipefail

die() { echo "error: $*" >&2; exit 1; }

if (( $# < 3 || $# > 4 )); then
    die "usage: $0 RPC_URL OUTPUT START_BLOCK [END_BLOCK]"
fi

rpc_url="$1"
output="$2"
start_block="$3"
end_block="${4:-}"

[[ "$start_block" =~ ^[0-9]+$ ]] || die "START_BLOCK must be an unsigned integer"
if [[ -z "$end_block" ]]; then
    end_block="$(cast block-number --rpc-url "$rpc_url")" ||
        die "could not read the latest block number"
fi
[[ "$end_block" =~ ^[0-9]+$ ]] || die "END_BLOCK must be an unsigned integer"

mkdir -p "$(dirname -- "$output")"
temporary="$(mktemp "${output}.tmp.XXXXXX")"
trap 'rm -f -- "$temporary"' EXIT

if (( 10#$start_block <= 10#$end_block )); then
    for ((block = 10#$start_block; block <= 10#$end_block; block++)); do
        block_hex="$(printf '0x%x' "$block")"
        receipts="$(cast rpc --rpc-url "$rpc_url" eth_getBlockReceipts "$block_hex")" ||
            die "could not read receipts for block $block"
        jq -e 'type == "array"' <<<"$receipts" >/dev/null ||
            die "receipt response for block $block was not an array"
        jq -c '.[]' <<<"$receipts" >> "$temporary"
    done
fi

mv -- "$temporary" "$output"
trap - EXIT
echo "wrote $(wc -l < "$output") receipts from blocks $start_block through $end_block to $output"
