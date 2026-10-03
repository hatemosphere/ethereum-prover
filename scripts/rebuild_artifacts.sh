#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
zkos_dir=$(cd -- "${repo_dir}/../zksync-os" && pwd)
export RUST_MIN_STACK=${RUST_MIN_STACK:-1073741824}

if [[ $# -gt 1 || ( $# -eq 1 && $1 != --guest-only ) ]]; then
    echo "Usage: $0 [--guest-only]" >&2
    exit 2
fi

# Docker and the sibling cargo-airbender must be installed; see scripts/README.md.
(cd "${zkos_dir}/zksync_os" && ./dump_bin.sh --type eth-stf-fusaka --reproducible)
if [[ ${1:-} == --guest-only ]]; then exit 0; fi
exec "${repo_dir}/scripts/copy_artifacts.sh"
