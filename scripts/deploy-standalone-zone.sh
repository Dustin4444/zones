#!/usr/bin/env bash
#
# Deploy a standalone zone — Verifier, ZoneMessenger, and an initialized ZonePortal — without the
# native TIP-1091 ZoneFactory. Needs a deployer key, an active sequencer transaction signer, an
# encryption key, and an RPC URL.
#
#   PRIVATE_KEY=0x... \
#   SEQUENCER_ENCRYPTION_PRIVATE_KEY=0x... \
#   ETH_RPC_URL=http://localhost:8545 \
#   ./scripts/deploy-standalone-zone.sh
#
# Why this needs to patch the reference contracts
# ------------------------------------------------
# ZonePortal.initialize is guarded by `msg.sender != ZONE_FACTORY_ADDRESS -> NotFactory()`, and that
# address is a compile-time constant. On a live chain nobody can call from the canonical factory
# address, so this script rewrites the constant to an authority it controls (the deployer by
# default), rebuilds, deploys, initializes, and then restores the source.
#
# The shared ZoneMessenger normally authenticates portals through `ZoneFactory.zones(zoneId)`.
# This deployment has no factory, so after deploying the portal the script temporarily rewrites
# that check to bind the messenger directly to the portal and its zone ID. This preserves caller
# authentication without requiring a factory shim.

set -euo pipefail

export FOUNDRY_DISABLE_NIGHTLY_WARNING=1

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REF_IMPLS="${REPO_ROOT}/specs/ref-impls"
CONSTANTS_FILE="${REF_IMPLS}/src/interfaces/IZone.sol"
MESSENGER_FILE="${REF_IMPLS}/src/tempo/ZoneMessenger.sol"

if [ -z "${PRIVATE_KEY:-}" ]; then
  echo "PRIVATE_KEY must be set (hex encoded, funded on the target chain)" >&2
  exit 1
fi

if [ -z "${SEQUENCER_ENCRYPTION_PRIVATE_KEY:-}" ]; then
  echo "SEQUENCER_ENCRYPTION_PRIVATE_KEY must be set (the shared Zone deposit-decryption key)" >&2
  exit 1
fi

ETH_RPC_URL="${ETH_RPC_URL:-http://localhost:8545}"

for tool in forge cast jq; do
  command -v "$tool" >/dev/null || { echo "$tool is required but not installed" >&2; exit 1; }
done

DEPLOYER="$(cast wallet address "$PRIVATE_KEY")"
# This key pays for setSequencerEncryptionKey and must be included in SEQUENCERS. In the default
# single-sequencer configuration, the deployer is also the active sequencer.
SEQUENCER_TRANSACTION_PRIVATE_KEY="${SEQUENCER_TRANSACTION_PRIVATE_KEY:-$PRIVATE_KEY}"
SEQUENCER_TRANSACTION_SIGNER="$(cast wallet address "$SEQUENCER_TRANSACTION_PRIVATE_KEY")"

# Zone parameters. Defaults produce a single-sequencer, fully open zone owned by the deployer.
ZONE_ID="${ZONE_ID:-1}"
INITIAL_TOKEN="$(cast to-check-sum-address "${INITIAL_TOKEN:-0x20C0000000000000000000000000000000000000}")"
ADMIN="$(cast to-check-sum-address "${ADMIN:-$DEPLOYER}")"
SEQUENCERS="${SEQUENCERS:-[$DEPLOYER]}"
THRESHOLD="${THRESHOLD:-1}"
ACCESS_ENFORCED="${ACCESS_ENFORCED:-false}"
GATEWAY_ENFORCED="${GATEWAY_ENFORCED:-false}"
ALLOWED_ACCOUNTS="${ALLOWED_ACCOUNTS:-[]}"
ZONE_GATEWAYS="${ZONE_GATEWAYS:-[]}"
ZONE_RPC_URL="${ZONE_RPC_URL:-}"

# Whoever is allowed to call initialize. Must be the caller below, so it defaults to the deployer.
FACTORY_AUTHORITY="$(cast to-check-sum-address "${FACTORY_AUTHORITY:-$DEPLOYER}")"

if ! tr -d '[:space:]' <<<"$SEQUENCERS" | tr ',[]' '\n' | tr '[:upper:]' '[:lower:]' \
  | grep -Fxq "${SEQUENCER_TRANSACTION_SIGNER,,}"; then
  echo "SEQUENCER_TRANSACTION_PRIVATE_KEY must belong to one of SEQUENCERS" >&2
  exit 1
fi

for patched_file in "$CONSTANTS_FILE" "$MESSENGER_FILE"; do
  if ! git -C "$REPO_ROOT" diff --quiet -- "$patched_file" \
    || ! git -C "$REPO_ROOT" diff --cached --quiet -- "$patched_file"; then
    echo "${patched_file} has uncommitted changes; commit or stash them first" >&2
    exit 1
  fi
done

echo "Deploying a standalone zone"
echo "  rpc               ${ETH_RPC_URL}"
echo "  chain             $(cast chain-id --rpc-url "$ETH_RPC_URL")"
echo "  deployer          ${DEPLOYER}"
echo "  factory authority ${FACTORY_AUTHORITY}"
echo "  initial token     ${INITIAL_TOKEN}"
echo "  encryption tx signer ${SEQUENCER_TRANSACTION_SIGNER}"
echo

# ZonePortal.initialize enables the initial token through the TIP-403 registry, whose
# tokenTransferPolicyId/migrateTransferPolicyIds entry points are TIP-1092 and only exist from T9
# onwards. Check before spending gas on three deployments that would fail to initialize.
TIP403_REGISTRY=0x403C000000000000000000000000000000000000
if ! cast call "$TIP403_REGISTRY" 'tokenTransferPolicyId(address)(bool,uint64)' "$INITIAL_TOKEN" \
  --rpc-url "$ETH_RPC_URL" >/dev/null 2>&1; then
  echo "This chain's TIP-403 registry does not expose tokenTransferPolicyId, so the portal cannot" >&2
  echo "enable its initial token. Those entry points are TIP-1092 and activate at T9 — wait for the" >&2
  echo "chain to reach T9, or target one where it is already active." >&2
  exit 1
fi

# Always put the canonical sources back, however this exits.
restore_sources() {
  git -C "$REPO_ROOT" checkout -- "$CONSTANTS_FILE" "$MESSENGER_FILE"
  echo
  echo "Restored the canonical Zone contracts"
}
trap restore_sources EXIT

echo "Patching ZONE_FACTORY_ADDRESS -> ${FACTORY_AUTHORITY}"
# Solidity rejects non-checksummed address literals, hence the normalization above.
perl -pi -e "s{^address constant ZONE_FACTORY_ADDRESS = .*;}{address constant ZONE_FACTORY_ADDRESS = ${FACTORY_AUTHORITY};}" \
  "$CONSTANTS_FILE"
grep -q "ZONE_FACTORY_ADDRESS = ${FACTORY_AUTHORITY};" "$CONSTANTS_FILE" \
  || { echo "failed to patch ${CONSTANTS_FILE}" >&2; exit 1; }

echo "Building"
(cd "$REF_IMPLS" && forge build >/dev/null)

deploy() {
  local target="$1" label="$2"
  local address
  address="$(cd "$REF_IMPLS" && forge create "$target" \
    --rpc-url "$ETH_RPC_URL" --private-key "$PRIVATE_KEY" --broadcast --json | jq -r '.deployedTo')"
  [ -n "$address" ] && [ "$address" != "null" ] || { echo "failed to deploy ${label}" >&2; exit 1; }
  echo "  deployed ${label} at ${address}" >&2
  echo "$address"
}

echo "Deploying contracts"
VERIFIER="$(deploy src/tempo/Verifier.sol:Verifier Verifier)"
PORTAL="$(deploy src/tempo/ZonePortal.sol:ZonePortal ZonePortal)"

echo "Binding ZoneMessenger directly to portal ${PORTAL}"
PORTAL="$PORTAL" perl -0pi -e '
  $old = "        ZoneInfo memory zone = zoneFactory.zones(zoneId);\n" .
         "        if (zone.portal != msg.sender) revert UnauthorizedPortal();";
  $new = "        if (msg.sender != $ENV{PORTAL} || IZonePortal(msg.sender).zoneId() != zoneId) {\n" .
         "            revert UnauthorizedPortal();\n" .
         "        }";
  index($_, $old) >= 0 or die "factory authorization not found";
  s/\Q$old\E/$new/;
' "$MESSENGER_FILE"
grep -q "msg.sender != ${PORTAL}" "$MESSENGER_FILE" \
  || { echo "failed to bind ZoneMessenger to ${PORTAL}" >&2; exit 1; }

(cd "$REF_IMPLS" && forge build >/dev/null)
MESSENGER="$(deploy src/tempo/ZoneMessenger.sol:ZoneMessenger ZoneMessenger)"

echo
echo "Initializing portal ${PORTAL}"
cast send "$PORTAL" \
  "initialize(uint32,address,bool,bool,address[],address[],address,address,address[],uint8,address,string)" \
  "$ZONE_ID" "$INITIAL_TOKEN" "$ACCESS_ENFORCED" "$GATEWAY_ENFORCED" \
  "$ALLOWED_ACCOUNTS" "$ZONE_GATEWAYS" "$MESSENGER" "$ADMIN" \
  "$SEQUENCERS" "$THRESHOLD" "$VERIFIER" "$ZONE_RPC_URL" \
  --rpc-url "$ETH_RPC_URL" --private-key "$PRIVATE_KEY" >/dev/null

# The Portal only accepts encrypted deposits after an active sequencer publishes the shared
# deposit-decryption key. The transaction signer is intentionally separate from that key: in a
# multi-sequencer deployment the shared encryption key is normally not itself a Portal sequencer.
ENCRYPTION_PUBLIC_KEY="$(cast wallet public-key --raw-private-key "$SEQUENCER_ENCRYPTION_PRIVATE_KEY")"
ENCRYPTION_PUBLIC_KEY="${ENCRYPTION_PUBLIC_KEY#0x}"
ENCRYPTION_KEY_X="0x${ENCRYPTION_PUBLIC_KEY:0:64}"
ENCRYPTION_KEY_Y_LAST_BYTE="${ENCRYPTION_PUBLIC_KEY: -2}"
if (( 16#$ENCRYPTION_KEY_Y_LAST_BYTE % 2 == 0 )); then
  ENCRYPTION_KEY_Y_PARITY=2
else
  ENCRYPTION_KEY_Y_PARITY=3
fi

# The proof is a raw ECDSA signature over keccak256(abi.encode(portal, x, yParity)).
ENCRYPTION_POP_MESSAGE="$(cast keccak "$(cast abi-encode 'f(address,bytes32,uint256)' \
  "$PORTAL" "$ENCRYPTION_KEY_X" "$ENCRYPTION_KEY_Y_PARITY")")"
ENCRYPTION_POP_SIGNATURE="$(cast wallet sign --no-hash \
  --private-key "$SEQUENCER_ENCRYPTION_PRIVATE_KEY" "$ENCRYPTION_POP_MESSAGE")"
ENCRYPTION_POP_SIGNATURE="${ENCRYPTION_POP_SIGNATURE#0x}"
ENCRYPTION_POP_R="0x${ENCRYPTION_POP_SIGNATURE:0:64}"
ENCRYPTION_POP_S="0x${ENCRYPTION_POP_SIGNATURE:64:64}"
ENCRYPTION_POP_V=$((16#${ENCRYPTION_POP_SIGNATURE:128:2}))
case "$ENCRYPTION_POP_V" in
  0|1) ENCRYPTION_POP_V=$((ENCRYPTION_POP_V + 27)) ;;
  27|28) ;;
  *)
    echo "invalid recovery id returned while signing encryption-key proof" >&2
    exit 1
    ;;
esac

echo
echo "Registering the shared sequencer encryption key"
cast send "$PORTAL" 'setSequencerEncryptionKey(bytes32,uint8,uint8,bytes32,bytes32)' \
  "$ENCRYPTION_KEY_X" "$ENCRYPTION_KEY_Y_PARITY" "$ENCRYPTION_POP_V" \
  "$ENCRYPTION_POP_R" "$ENCRYPTION_POP_S" \
  --rpc-url "$ETH_RPC_URL" --private-key "$SEQUENCER_TRANSACTION_PRIVATE_KEY" >/dev/null

mapfile -t REGISTERED_ENCRYPTION_KEY < <(
  cast call "$PORTAL" 'sequencerEncryptionKey()(bytes32,uint8)' --rpc-url "$ETH_RPC_URL"
)
if [ "${REGISTERED_ENCRYPTION_KEY[0]:-}" != "$ENCRYPTION_KEY_X" ] \
  || [ "${REGISTERED_ENCRYPTION_KEY[1]:-}" != "$ENCRYPTION_KEY_Y_PARITY" ]; then
  echo "registered encryption key does not match SEQUENCER_ENCRYPTION_PRIVATE_KEY" >&2
  exit 1
fi

echo
echo "Zone ${ZONE_ID} deployed:"
echo "  Verifier      ${VERIFIER}"
echo "  ZoneMessenger ${MESSENGER}"
echo "  ZonePortal    ${PORTAL}"
echo
echo "Verifying portal state:"
echo "  zoneId        $(cast call "$PORTAL" 'zoneId()(uint32)' --rpc-url "$ETH_RPC_URL")"
echo "  admin         $(cast call "$PORTAL" 'admin()(address)' --rpc-url "$ETH_RPC_URL")"
echo "  messenger     $(cast call "$PORTAL" 'messenger()(address)' --rpc-url "$ETH_RPC_URL")"
echo "  verifier      $(cast call "$PORTAL" 'verifier()(address)' --rpc-url "$ETH_RPC_URL")"
echo "  token enabled $(cast call "$PORTAL" 'isTokenEnabled(address)(bool)' "$INITIAL_TOKEN" --rpc-url "$ETH_RPC_URL")"
echo "  encryption key $(cast call "$PORTAL" 'sequencerEncryptionKey()(bytes32,uint8)' --rpc-url "$ETH_RPC_URL" | tr '\n' ' ')"
