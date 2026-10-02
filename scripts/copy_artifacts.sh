#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cluster_dir=$(dirname -- "${repo_dir}")
zkos_dir="${cluster_dir}/zksync-os"
airbender_dir="${cluster_dir}/zksync-airbender"
artifacts_dir="${repo_dir}/artifacts"
dist_dir="${zkos_dir}/zksync_os/dist/eth_stf"
export RUST_MIN_STACK=${RUST_MIN_STACK:-1073741824}

commit() {
    local supplied=$1 directory=$2
    if [[ -n ${supplied} ]]; then printf '%s\n' "${supplied}";
    else git -C "${directory}" rev-parse HEAD; fi
}
prover_commit=$(commit "${PROVER_COMMIT:-}" "${repo_dir}")
zkos_commit=$(commit "${ZKSYNC_OS_COMMIT:-}" "${zkos_dir}")
platform_commit=$(commit "${AIRBENDER_PLATFORM_COMMIT:-}" "${cluster_dir}/airbender-platform")
airbender_commit=$(commit "${ZKSYNC_AIRBENDER_COMMIT:-}" "${airbender_dir}")

# Match host_utils::{unrolled,bridge,final}_blake_mode and BlakeMode::parse.
blake_mode() {
    case "$1" in
        compression|round|blake2_with_compression) echo blake2_with_compression ;;
        g_function|g|blake2_g_function) echo blake2_g_function ;;
        special_opcodes|spec|special_opcodes_extension) echo special_opcodes_extension ;;
        *) echo "Invalid Blake mode: $1" >&2; exit 1 ;;
    esac
}
RECURSION_UNROLLED_BLAKE=$(blake_mode "${RECURSION_UNROLLED_BLAKE-compression}")
RECURSION_BRIDGE_BLAKE=$(blake_mode "${RECURSION_BRIDGE_BLAKE-${RECURSION_UNROLLED_BLAKE}}")
RECURSION_FINAL_BLAKE=$(blake_mode "${RECURSION_FINAL_BLAKE-${RECURSION_UNIFIED_BLAKE-compression}}")
export RECURSION_UNROLLED_BLAKE RECURSION_BRIDGE_BLAKE RECURSION_FINAL_BLAKE
if [[ ${RECURSION_UNROLLED_BLAKE} == special_opcodes_extension || ${RECURSION_BRIDGE_BLAKE} == special_opcodes_extension ]]; then
    echo 'Unrolled/bridge verifiers do not support special opcodes' >&2
    exit 1
fi

for file in manifest.toml app.bin app.elf app.text; do
    test -s "${dist_dir}/${file}" || { echo "Missing guest artifact: ${dist_dir}/${file}" >&2; exit 1; }
done
if ! grep -qx 'reproducible = true' "${dist_dir}/manifest.toml"; then
    echo 'Guest must be built with dump_bin.sh --reproducible' >&2
    exit 1
fi
stems=()
for mode in "${RECURSION_UNROLLED_BLAKE}" "${RECURSION_BRIDGE_BLAKE}"; do
    stems+=("fsv_unrolled_base_layer_sec_100_${mode}" "fsv_unrolled_recursion_layer_sec_100_${mode}")
done
stems+=("fsv_unified_recursion_layer_sec_100_${RECURSION_FINAL_BLAKE}")
for stem in "${stems[@]}"; do
    for ext in bin text; do
        test -s "${airbender_dir}/tools/gkr_verifier/${stem}.${ext}" || {
            echo "Missing trusted in-tree verifier: ${stem}.${ext}" >&2; exit 1;
        }
    done
done

mkdir -p "${artifacts_dir}"
# Replace these generated directories so stale variants cannot enter a bundle.
rm -rf -- "${artifacts_dir}/eth_stf" "${artifacts_dir}/fsv"
cp -a "${dist_dir}" "${artifacts_dir}/eth_stf"
mkdir -p "${artifacts_dir}/fsv"
for stem in "${stems[@]}"; do
    cp "${airbender_dir}/tools/gkr_verifier/${stem}."{bin,text} "${artifacts_dir}/fsv/"
done
cat > "${artifacts_dir}/recursion.env" <<MODES
export RECURSION_UNROLLED_BLAKE=${RECURSION_UNROLLED_BLAKE}
export RECURSION_BRIDGE_BLAKE=${RECURSION_BRIDGE_BLAKE}
export RECURSION_FINAL_BLAKE=${RECURSION_FINAL_BLAKE}
MODES
export FSV_DIR="${artifacts_dir}/fsv"
cd "${repo_dir}"
if [[ -z ${PROVER_BIN:-} ]]; then
    cargo build --locked --release -p ethereum_prover --bin ethereum_prover
    PROVER_BIN="${CARGO_TARGET_DIR:-${repo_dir}/target}/release/ethereum_prover"
fi
"${PROVER_BIN}" generate-verifier-artifacts --app-dir "${artifacts_dir}/eth_stf" \
    --output-dir "${artifacts_dir}" --security security_100

{
    printf 'built_at_utc=%s\n' "$(date -u +%FT%TZ)"
    printf 'ethereum-prover=%s\n' "${prover_commit}"
    printf 'zksync-os=%s\n' "${zkos_commit}"
    printf 'airbender-platform=%s\n' "${platform_commit}"
    printf 'zksync-airbender=%s\n' "${airbender_commit}"
    printf 'rust_toolchain=nightly-2026-08-09\nguest_type=eth-stf-fusaka\n'
    printf 'guest_build=reproducible\n'
    printf 'proof_format=EPROOF01 v2\nverification_key_format=EVKEY001 v2\nsecurity=100\n'
    printf 'blake_unrolled=%s\nblake_bridge=%s\nblake_final=%s\n' \
        "${RECURSION_UNROLLED_BLAKE}" "${RECURSION_BRIDGE_BLAKE}" "${RECURSION_FINAL_BLAKE}"
    printf '\n# SHA-256 of every runtime artifact (metadata itself excluded)\n'
    cd "${artifacts_dir}"
    { find eth_stf fsv -type f -print0; printf '%s\0' recursion.env recursion_unified_v3_security_100.vk.bin; } \
        | sort -z | xargs -0 sha256sum
} > "${artifacts_dir}/build_metadata.txt"
echo "Prepared v3 runtime artifacts in ${artifacts_dir}"
