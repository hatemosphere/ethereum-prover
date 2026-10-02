#!/usr/bin/env bash
set -euo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cluster_dir=$(dirname -- "${repo_dir}")
prover_dir=$(basename -- "${repo_dir}")
# A filtered cluster-root tar context also works with Docker's legacy builder.
# Omit build caches, local proof data, and .env files.
tar -C "${cluster_dir}" -cf - \
    --exclude='*/.git' --exclude='*/target' --exclude='*/node_modules' \
    --exclude='*/.agents' --exclude='*/.data' --exclude='*/.cache' \
    --exclude='*/.env' --exclude='*/.env.*' --exclude='*/artifacts/.build' \
    --exclude='*/proof_verifier_js/ts/wasm' --exclude='*/proof_verifier_js/wasm/pkg' \
    --exclude='*/dist' --exclude='*/artifacts/eth_stf' --exclude='*/artifacts/fsv' \
    "${prover_dir}" zksync-os airbender-platform zksync-airbender \
    | "${DOCKER:-docker}" build -f "${prover_dir}/docker/ethereum-prover/Dockerfile" \
        --build-arg "PROVER_DIR=${prover_dir}" \
        --build-arg "PROVER_COMMIT=$(git -C "${repo_dir}" rev-parse HEAD)" \
        --build-arg "ZKSYNC_OS_COMMIT=$(git -C "${cluster_dir}/zksync-os" rev-parse HEAD)" \
        --build-arg "AIRBENDER_PLATFORM_COMMIT=$(git -C "${cluster_dir}/airbender-platform" rev-parse HEAD)" \
        --build-arg "ZKSYNC_AIRBENDER_COMMIT=$(git -C "${cluster_dir}/zksync-airbender" rev-parse HEAD)" \
        "$@" -
