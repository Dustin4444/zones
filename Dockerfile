ARG CHEF_IMAGE=chef

FROM ${CHEF_IMAGE} AS builder

ARG RUST_PROFILE=profiling
ARG VERGEN_GIT_SHA
ARG VERGEN_GIT_SHA_SHORT
ARG EXTRA_RUSTFLAGS=""

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked,id=cargo-registry \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked,id=cargo-git \
    --mount=type=cache,target=$SCCACHE_DIR,sharing=locked,id=sccache \
    RUSTFLAGS="-C link-arg=-fuse-ld=mold ${EXTRA_RUSTFLAGS}" \
    cargo build --profile ${RUST_PROFILE} \
        --bin tempo-zone --features "jemalloc" \
    && RUSTFLAGS="-C link-arg=-fuse-ld=mold ${EXTRA_RUSTFLAGS}" \
    cargo build --profile ${RUST_PROFILE} \
        --bin tempo-xtask

# Build the same pinned, Tempo-patched Foundry used by the Specs workflow.
FROM rust:1.96-bookworm@sha256:5e2214abe154fe26e39f64488952e5c991eeed1d6d6da7cc8381ae83927f0cfc AS foundry
ARG TEMPO_FOUNDRY_REF=18a5d71d6ab621433145e5ea86dfe3dbace0763a
ARG FOUNDRY_REF=6902a96211da7bcc3d9c4d8e97910ac5c9d5d2c6
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang cmake git libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN git init /tempo \
    && git -C /tempo remote add origin https://github.com/tempoxyz/tempo.git \
    && git -C /tempo fetch --depth=1 origin "${TEMPO_FOUNDRY_REF}" \
    && git -C /tempo checkout --detach FETCH_HEAD \
    && git init /foundry \
    && git -C /foundry remote add origin https://github.com/foundry-rs/foundry.git \
    && git -C /foundry fetch --depth=1 origin "${FOUNDRY_REF}" \
    && git -C /foundry checkout --detach FETCH_HEAD
RUN /tempo/scripts/foundry-patch.sh /tempo /foundry
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked,id=foundry-cargo-registry \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked,id=foundry-cargo-git \
    cd /foundry \
    && cargo build --bin forge --bin cast --profile release --no-default-features

# Solidity ref-impls compiled for shared runtimes, routers, and zone genesis artifacts.
# Requires the specs/ref-impls/lib submodules to be checked out.
FROM debian:bookworm-slim@sha256:4724b8cc51e33e398f0e2e15e18d5ec2851ff0c2280647e1310bc1642182655d AS solidity
COPY --from=foundry /foundry/target/release/forge /usr/local/bin/forge
COPY --from=foundry /foundry/target/release/cast /usr/local/bin/cast
WORKDIR /app/specs/ref-impls
COPY specs/ref-impls .
RUN forge build --skip test

FROM debian:bookworm-slim@sha256:4724b8cc51e33e398f0e2e15e18d5ec2851ff0c2280647e1310bc1642182655d AS base

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /data

# tempo-zone
FROM base AS tempo-zone
ARG RUST_PROFILE=profiling
COPY --from=builder /app/target/${RUST_PROFILE}/tempo-zone /usr/local/bin/tempo-zone
ENTRYPOINT ["/usr/local/bin/tempo-zone"]

# tempo-zone-xtask: zone provisioning tooling (create-zone, zone-info, deploy-router).
# Ships the compiled ref-impls artifacts used by provisioning and router deployment.
FROM base AS tempo-zone-xtask
ARG RUST_PROFILE=profiling
RUN apt-get update && apt-get install -y --no-install-recommends \
    jq \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/${RUST_PROFILE}/tempo-xtask /usr/local/bin/tempo-xtask
COPY --from=solidity /usr/local/bin/cast /usr/local/bin/cast
COPY --from=solidity /app/specs/ref-impls/out /app/specs/ref-impls/out
WORKDIR /app
ENTRYPOINT ["/usr/local/bin/tempo-xtask"]
