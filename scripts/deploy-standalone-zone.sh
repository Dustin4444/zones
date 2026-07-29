#!/usr/bin/env bash
#
# Deploy a standalone zone — Verifier, ZoneMessenger, and an initialized ZonePortal — without the
# native TIP-1091 ZoneFactory. Needs only a private key and an RPC URL.
#
#   PRIVATE_KEY=0x... ETH_RPC_URL=http://localhost:8545 ./scripts/deploy-standalone-zone.sh
#
# Why this needs to patch a constant
# ----------------------------------
# ZonePortal.initialize is guarded by `msg.sender != ZONE_FACTORY_ADDRESS -> NotFactory()`, and that
# address is a compile-time constant. On a live chain nobody can call from the canonical factory
# address, so this script rewrites the constant to an authority it controls (the deployer by
# default), rebuilds, deploys, initializes, and then restores the source.
#
# The messenger and verifier do NOT need patching: the portal takes both as `initialize` arguments
# and stores them, so it points at whatever this script deploys.
#
# Known limitation: ZoneMessenger reaches the factory through the same constant
# (`zoneFactory.zones(zoneId)`). With the default EOA authority that call has no code to hit, so
# cross-zone paths that resolve zone metadata will revert. Set FACTORY_AUTHORITY to a contract
# implementing `zones(uint32)` if you need those.

set -euo pipefail

export FOUNDRY_DISABLE_NIGHTLY_WARNING=1

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REF_IMPLS="${REPO_ROOT}/specs/ref-impls"
CONSTANTS_FILE="${REF_IMPLS}/src/interfaces/IZone.sol"

if [ -z "${PRIVATE_KEY:-}" ]; then
  echo "PRIVATE_KEY must be set (hex encoded, funded on the target chain)" >&2
  exit 1
fi

ETH_RPC_URL="${ETH_RPC_URL:-http://localhost:8545}"

for tool in forge cast jq; do
  command -v "$tool" >/dev/null || { echo "$tool is required but not installed" >&2; exit 1; }
done

DEPLOYER="$(cast wallet address "$PRIVATE_KEY")"

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

if ! git -C "$REPO_ROOT" diff --quiet -- "$CONSTANTS_FILE"; then
  echo "${CONSTANTS_FILE} has uncommitted changes; commit or stash them first" >&2
  exit 1
fi

echo "Deploying a standalone zone"
echo "  rpc               ${ETH_RPC_URL}"
echo "  chain             $(cast chain-id --rpc-url "$ETH_RPC_URL")"
echo "  deployer          ${DEPLOYER}"
echo "  factory authority ${FACTORY_AUTHORITY}"
echo "  initial token     ${INITIAL_TOKEN}"
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

# Always put the canonical constant back, however this exits.
restore_constants() {
  git -C "$REPO_ROOT" checkout -- "$CONSTANTS_FILE"
  echo
  echo "Restored the canonical ZONE_FACTORY_ADDRESS in $(basename "$CONSTANTS_FILE")"
}
trap restore_constants EXIT

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
MESSENGER="$(deploy src/tempo/ZoneMessenger.sol:ZoneMessenger ZoneMessenger)"
PORTAL="$(deploy src/tempo/ZonePortal.sol:ZonePortal ZonePortal)"

echo
echo "Initializing portal ${PORTAL}"
cast send "$PORTAL" \
  "initialize(uint32,address,bool,bool,address[],address[],address,address,address[],uint8,address,string)" \
  "$ZONE_ID" "$INITIAL_TOKEN" "$ACCESS_ENFORCED" "$GATEWAY_ENFORCED" \
  "$ALLOWED_ACCOUNTS" "$ZONE_GATEWAYS" "$MESSENGER" "$ADMIN" \
  "$SEQUENCERS" "$THRESHOLD" "$VERIFIER" "$ZONE_RPC_URL" \
  --rpc-url "$ETH_RPC_URL" --private-key "$PRIVATE_KEY" >/dev/null

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
