#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
zkos_dir=$(cd -- "${repo_dir}/../zksync-os" && pwd)
export RUST_MIN_STACK=${RUST_MIN_STACK:-1073741824}

# cargo-airbender and cargo-binutils must be installed; see scripts/README.md.
(cd "${zkos_dir}/zksync_os" && ./dump_bin.sh --type eth-stf-fusaka)
exec "${repo_dir}/scripts/copy_artifacts.sh"
