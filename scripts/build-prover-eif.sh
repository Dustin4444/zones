#!/usr/bin/env bash

set -euo pipefail

REPOSITORY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPOSITORY_ROOT
readonly PROVER_IMAGE="${PROVER_IMAGE:-ghcr.io/tempoxyz/tempo-zone-prover:latest}"
readonly EIF_BUILDER_IMAGE="${EIF_BUILDER_IMAGE:-tempo-zone-prover-eif-builder:local}"
readonly EIF_OUTPUT_DIR="${EIF_OUTPUT_DIR:-${REPOSITORY_ROOT}/target/tempo-zone-prover-eif}"
readonly EIF_OUTPUT="${EIF_OUTPUT_DIR}/tempo-zone-prover.eif"
readonly EIF_OUTPUT_TMP="${EIF_OUTPUT_DIR}/tempo-zone-prover.eif.tmp"
readonly MEASUREMENTS_OUTPUT="${EIF_OUTPUT_DIR}/measurements.json"
readonly MEASUREMENTS_OUTPUT_TMP="${EIF_OUTPUT_DIR}/measurements.json.tmp"

docker image inspect "${PROVER_IMAGE}" >/dev/null 2>&1 || {
    echo "Prover image is not present in the local Docker image store: ${PROVER_IMAGE}" >&2
    echo "Build or pull it before creating the EIF." >&2
    exit 1
}

docker build \
    --platform linux/amd64 \
    --file "${REPOSITORY_ROOT}/Dockerfile.prover-eif-builder" \
    --tag "${EIF_BUILDER_IMAGE}" \
    "${REPOSITORY_ROOT}"

mkdir -p "${EIF_OUTPUT_DIR}"
rm -f "${EIF_OUTPUT_TMP}" "${MEASUREMENTS_OUTPUT_TMP}"

docker run --rm \
    --platform linux/amd64 \
    --volume /var/run/docker.sock:/var/run/docker.sock \
    --volume "${EIF_OUTPUT_DIR}:/output" \
    "${EIF_BUILDER_IMAGE}" \
    build-enclave \
    --docker-uri "${PROVER_IMAGE}" \
    --output-file /output/tempo-zone-prover.eif.tmp \
    | tee "${MEASUREMENTS_OUTPUT_TMP}"

test -s "${EIF_OUTPUT_TMP}" || {
    echo "Nitro CLI did not produce a non-empty EIF." >&2
    exit 1
}

mv -f "${EIF_OUTPUT_TMP}" "${EIF_OUTPUT}"
mv -f "${MEASUREMENTS_OUTPUT_TMP}" "${MEASUREMENTS_OUTPUT}"

echo "Built ${EIF_OUTPUT}"
echo "Recorded PCR measurements in ${MEASUREMENTS_OUTPUT}"
