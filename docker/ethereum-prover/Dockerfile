# Context: the parent of the four sibling repositories. See scripts/build_docker.sh.
FROM nvidia/cuda:13.3.1-devel-ubuntu26.04 AS builder
ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git clang libclang-dev libssl-dev pkg-config build-essential cmake python3 \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:${PATH} \
    RUST_MIN_STACK=1073741824
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- \
    -y --profile minimal --default-toolchain nightly-2026-08-09 \
    --component rust-src --component llvm-tools-preview
RUN cargo install cargo-binutils --version 0.4.0 --locked

ARG PROVER_DIR=ethereum-prover
COPY zksync-airbender /src/zksync-airbender
COPY airbender-platform /src/airbender-platform
COPY zksync-os /src/zksync-os
COPY ${PROVER_DIR} /src/ethereum-prover
WORKDIR /src/ethereum-prover
ARG CUDA_ARCHS="80;89;90;120"
ARG BUILD_JOBS=8
ENV CUDAARCHS=${CUDA_ARCHS} CARGO_BUILD_JOBS=${BUILD_JOBS} CMAKE_BUILD_PARALLEL_LEVEL=${BUILD_JOBS}
RUN cargo install --path ../airbender-platform/crates/cargo-airbender --no-default-features --locked
RUN cargo build --locked --release -p ethereum_prover --bin ethereum_prover

# Git worktree pointers do not survive relocation; the context builder supplies these SHAs.
ARG PROVER_COMMIT
ARG ZKSYNC_OS_COMMIT
ARG AIRBENDER_PLATFORM_COMMIT
ARG ZKSYNC_AIRBENDER_COMMIT
ARG RECURSION_UNROLLED_BLAKE=compression
ARG RECURSION_BRIDGE_BLAKE=compression
ARG RECURSION_FINAL_BLAKE=compression
RUN PROVER_BIN=/src/ethereum-prover/target/release/ethereum_prover scripts/rebuild_artifacts.sh
RUN mkdir -p /out/ethereum_prover/configs /out/artifacts && \
    cp target/release/ethereum_prover /out/ethereum_prover/ && \
    cp ethereum_prover/configs/*.yaml /out/ethereum_prover/configs/ && \
    cp -a artifacts/eth_stf artifacts/fsv artifacts/recursion.env \
        artifacts/recursion_unified_v3_security_100.vk.bin artifacts/build_metadata.txt /out/artifacts/ && \
    cp docker/ethereum-prover/README.md /out/README.md

FROM nvidia/cuda:13.3.1-runtime-ubuntu26.04
ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3t64 libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --user-group --create-home --shell /usr/sbin/nologin prover \
    && install -d -o prover -g prover /data
COPY --from=builder /out /opt/ethereum-prover
COPY --from=builder /src/ethereum-prover/docker/ethereum-prover/entrypoint.sh /usr/local/bin/ethereum-prover-entrypoint
ENV NVIDIA_VISIBLE_DEVICES=all NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    RUST_MIN_STACK=1073741824 \
    FSV_DIR=/opt/ethereum-prover/artifacts/fsv \
    eth_prover_app_dir=/opt/ethereum-prover/artifacts/eth_stf \
    eth_prover_data_dir=/data
WORKDIR /opt/ethereum-prover/ethereum_prover
USER 10001:10001
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/ethereum-prover-entrypoint"]
CMD ["--help"]
