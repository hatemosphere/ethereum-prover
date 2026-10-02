# v3 prover container and runtime bundle

The image uses CUDA 13.3.1 / Ubuntu 26.04 for both build and runtime, matching
`zksync-airbender/.github/workflows/test-gpu.yaml`. The Rust toolchain is
`nightly-2026-08-09`, with a 1 GiB `RUST_MIN_STACK`. Builds target CUDA architectures
80, 89, 90, and 120 by default; override with `--build-arg CUDA_ARCHS=120` for a
single-architecture deployment. No GPU is needed to build or generate the key.

## Build

Place `ethereum-prover`, `zksync-os`, `airbender-platform`, and `zksync-airbender`
next to each other. From the prover repository:

```sh
scripts/build_docker.sh -t ethereum-prover:v3
```

The script sends a filtered cluster-root tar context with the selected prover
worktree and the three siblings. It excludes build caches, `.env` files, local
proof data, and generated guest distributions. It also passes the four source
commits into the build metadata. `DOCKER` may name a Docker-compatible executable
wrapper when local daemon access requires it.

The Dockerfile copies the repositories to `/src/<repository>`, preserving relative
Cargo path dependencies. It installs the sibling `cargo-airbender`, rebuilds the
Fusaka guest, copies the selected in-tree FSV binaries, and generates the v2 key.
`BUILD_JOBS` controls Cargo and CMake parallelism (default 8). Blake modes can be
selected with the three `RECURSION_*_BLAKE` build arguments; use matching unrolled
and bridge modes unless intentionally producing a different trusted key.

Use the key generated inside this image for its proofs. Guest binaries embed
compiler source paths; a native rebuild at the same commits can have a different
guest hash and VK. To distribute the image's matching key:

```sh
container_id=$(docker create ethereum-prover:v3)
docker cp "$container_id:/opt/ethereum-prover/artifacts/recursion_unified_v3_security_100.vk.bin" .
docker rm -v "$container_id"
```

## Run

The image runs as UID/GID 10001. Use NVIDIA Container Toolkit and a driver that
supports CUDA 13.3.1. Mount a persistent writable data directory:

```sh
sudo install -d -o 10001 -g 10001 /srv/ethereum-prover
# Set eth_prover_rpc_url and, for submission, the EthProofs credentials first.
docker run --rm --gpus all \
  -v /srv/ethereum-prover:/data \
  -e eth_prover_rpc_url -e eth_prover_ethproofs_token \
  -e eth_prover_ethproofs_cluster_id -e eth_prover_ethproofs_verifier_id \
  ethereum-prover:v3 --config configs/ethproofs_staging.yaml run
```

Use `configs/local_debug.yaml` for CPU witness execution with submission off.
Mount a custom config and pass `--config /path/to/config.yaml` for other settings.
The container sets `eth_prover_app_dir` to `/opt/ethereum-prover/artifacts/eth_stf`,
`eth_prover_data_dir` to `/data`, and `FSV_DIR` to `/opt/ethereum-prover/artifacts/fsv`.
The entrypoint loads the packaged Blake modes from `artifacts/recursion.env`.
Environment values override YAML, so override these environment variables as well
if relocating the guest or data directories.

## Extracted runtime bundle

Release archives preserve this layout:

```text
ethereum_prover/ethereum_prover
ethereum_prover/configs/*.yaml
artifacts/eth_stf/{manifest.toml,app.bin,app.elf,app.text}
artifacts/fsv/*.{bin,text}
artifacts/recursion_unified_v3_security_100.vk.bin
artifacts/recursion.env
artifacts/build_metadata.txt
```

On a compatible CUDA 13.3.1 / Ubuntu 26.04 host, enter the extracted
`ethereum_prover/` directory before running:

```sh
. ../artifacts/recursion.env
export FSV_DIR="$(realpath ../artifacts/fsv)"
export eth_prover_data_dir=/srv/ethereum-prover
export RUST_MIN_STACK=1073741824
./ethereum_prover --config configs/local_debug.yaml block 26078503
```

The binary's default FSV path embeds its build checkout; it does not relocate.
Keep `FSV_DIR` explicit. Proofs and the outbox belong in the persistent data
mount, not in the image. See the service README in the source repository for
configuration, verification, and outbox recovery semantics.
