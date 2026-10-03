# ethereum_prover v3 service

The service fetches Ethereum blocks and execution witnesses, records prover input
with zksync-os `av_integrate_v3`, and uses a persistent
`airbender-host::GpuProver` backed by the v3 `prover_pipeline`. Fetching runs ahead
of proving through a bounded queue. A separate submission worker reports status
and delivers archived proofs to EthProofs from a durable outbox.

## Build and runtime artifacts

Use the four-repository sibling layout in the [root README](../README.md),
`nightly-2026-08-09`, and CUDA 13.3.1. The [Dockerfile](../docker/ethereum-prover/Dockerfile)
uses matching CUDA builder/runtime images on Ubuntu 26.04. Native builds also
need Clang, CMake, OpenSSL development libraries, Docker, and the sibling
`cargo-airbender` tool. Guest compiler tools run in its pinned container. See the
[build scripts](../scripts/README.md) for commands.

```sh
# From the repository root, with the guest tools installed:
RUST_MIN_STACK=1073741824 scripts/rebuild_artifacts.sh
```

This runs `dump_bin.sh --type eth-stf-fusaka --reproducible`, copies the whole guest
distribution to `artifacts/eth_stf`, copies the trusted FSV programs to
`artifacts/fsv`, and generates `recursion_unified_v3_security_100.vk.bin`.
`build_metadata.txt` records source commits, hashes, resolved Blake modes, and
`EPROOF01` v2 / `EVKEY001` v2 formats. Do not mix a guest, FSV set, or key from a
different guest or Blake mode. Native and Docker bundles consume the reproducible
guest distribution and derive the same key.

The guest directory must contain `manifest.toml`, `app.bin`, `app.elf`, and
`app.text`. The GPU service is linked against CUDA even in `cpu_witness` mode;
CPU execution and key generation do not require a GPU device. The old CUDA-stub
build recipe is not used by the v3 packaging.

The binary's default FSV directory is a compile-time path into the Airbender
checkout. For native builds and extracted bundles, enter `ethereum_prover/` and
set the runtime paths explicitly:

```sh
. ../artifacts/recursion.env
export FSV_DIR="$(realpath ../artifacts/fsv)"
export RUST_MIN_STACK=1073741824
# Optional: use an absolute persistent data directory.
export eth_prover_data_dir=/srv/ethereum-prover
```

Examples below use `../target/release/ethereum_prover`. In an extracted runtime
bundle, substitute `./ethereum_prover`. The container sets the guest, FSV, and
data paths and loads `recursion.env` itself; see the
[container/bundle README](../docker/ethereum-prover/README.md).

## Commands

```sh
# Process one block (cache first, RPC on a miss). Local debug never submits.
../target/release/ethereum_prover --config configs/local_debug.yaml block 26078503

# Follow the newest owned block, with WS notifications or polling.
../target/release/ethereum_prover --config configs/ethproofs_staging.yaml run

# Process every owned block in an inclusive range, or omit --end to continue.
../target/release/ethereum_prover --config configs/ethproofs_staging.yaml \
  run --start 26078427 --end 26078715

# Prove local inputs, without submission. The config supplies app_dir/security.
# input-dir contains block.json + execution_witness.json (plain JSON or RPC responses).
../target/release/ethereum_prover --config configs/local_debug.yaml prove \
  --input-dir /path/to/block-input --output /path/to/proof.bin.gz \
  --artifact-json /path/to/artifact.json

# Verify an exported/archived proof and print eight public output words.
../target/release/ethereum_prover verify /path/to/proof.bin.gz \
  --key ../artifacts/recursion_unified_v3_security_100.vk.bin

# Regenerate the key from the guest and trusted FSV binaries (CPU-only).
../target/release/ethereum_prover generate-verifier-artifacts \
  --app-dir ../artifacts/eth_stf --output-dir ../artifacts --security security_100
```

`prove` always proves, regardless of the config's `mode`, and never submits.
`block` and `run` honor `mode`. `run` owns blocks satisfying
`block_number % block_mod == prover_id`; require `block_mod > 0` and
`prover_id < block_mod`. Tip mode skips backlog while proving is busy; use
`--start` for a backfill. `block N` explicitly processes N without shard filtering.
`--end` requires `--start`.

## Configuration

YAML uses the `eth_prover:` root. Precedence is defaults, YAML, `.env` in the
process working directory, then environment. Every field can be set with an
`eth_prover_` prefix, e.g. `eth_prover_rpc_url`. Paths are relative to the process
working directory, not the YAML file. Templates assume the `ethereum_prover/`
working directory.

| Field | Meaning / default |
|---|---|
| `app_dir` | Complete guest distribution; templates use `../artifacts/eth_stf`. |
| `data_dir` | Persistent cache/archive/outbox root; default `.data`. |
| `mode` | `cpu_witness` records/executes on CPU; `gpu_prove` generates a proof. |
| `security` | Use `security_100`; v3 proof/VK export rejects security 80. |
| `rpc_url` | HTTP Ethereum RPC with full blocks and `debug_executionWitness`; set via environment for credentials. Required for chain following and cache misses. |
| `ws_url` | Optional `newHeads` endpoint; HTTP still supplies the head and block data. Default null; polling remains available between notifications or on WS failure. |
| `witness_format` | `reth` (default) or `geth`, matching the node's witness response. |
| `poll_interval_ms` | Head polling interval, default 1000. |
| `rpc_attempts` | Attempts per RPC operation, default 3. |
| `prefetch` | Blocks queued ahead of the worker, default 2 (minimum effective capacity 1). |
| `replay_threads` | CPU replay threads per GPU job; default null keeps the prover default (8). |
| `cache_policy` | `off`, `on_failure` (default), or `always`; governs retention in streaming operation. Explicit `block` commands populate the cache on a miss. |
| `ethproofs_submission` | `off` (default), `staging`, or `prod`. |
| `ethproofs_url` | Optional override of the staging/production API base URL; default null. |
| `ethproofs_token` | Bearer token for enabled submission; keep in the environment. |
| `ethproofs_cluster_id` | Cluster ID for enabled submission. |
| `ethproofs_verifier_id` | Registered verifier ID sent with proofs; default string `None`. Set the ID for this v3 guest/key before submitting. |
| `block_mod`, `prover_id` | Stream sharding divisor and remainder; defaults 1 and 0. |
| `on_failure` | `exit` (default) or `continue` after worker failures. |
| `sentry_dsn` | Optional Sentry integration. |
| `prometheus_port` | Optional Prometheus exporter port; templates use 9898. |

`local_debug.yaml` uses CPU witness execution and submission **off**.
`ethproofs_staging.yaml` and `ethproofs_prod.yaml` use GPU proving, enable the
named endpoint, and shard by 10 and 100 respectively. Set credentials, the
registered verifier ID, and your shard before using those templates. Each
service/shard should have its own writable data directory.

## Persistent data and submission recovery

```text
data_dir/
  cache/blocks/<n>/
    block.json
    execution_witness.json
    receipts/              # debugging receipts, when fetched
  proofs/<n>/
    proof.bin.gz
    manifest.json
  outbox/
    pending/<n>.json
    quarantine/<n>.json
```

Every successful GPU proof in `block`/`run` is archived, including when submission
is off. The manifest records block number/hash, program cycles, proving time,
proof size, SHA-256, and creation time. The archived gzip bytes are exactly the
payload base64-encoded for EthProofs; local verification takes the gzip file
without base64 conversion. `prove --output` instead writes the requested file.

With submission enabled, the proof is archived and a pending outbox record is
written **before the first submission attempt**. Pending records are replayed on
startup and retried with bounded exponential backoff while the worker runs.
Transport errors, HTTP 408/425/429, and 5xx are retryable. Other HTTP rejections,
unreadable proof files, and unreadable outbox records are permanent/quarantined.
Queued/proving status notifications are best effort; the durable retry guarantee
applies to the completed proof submission.

A successful submission removes its pending record. A crash after server
acceptance but before removal can resubmit an already accepted proof: delivery
is at least once, not exactly once. Permanent failures move to `quarantine` for
operator review and are not retried automatically. Preserve the proof archive
with the outbox. Keep the configured data path and working directory stable
because records contain proof paths.

A finite `block`/range run can exit with retryable records still pending; the
next submission-enabled start replays them. Submission-off runs leave the outbox
undelivered. The `on_failure` setting does not turn permanent HTTP rejection into
a retryable one.

## Verification and observability

Native `verify` and the [TS/WASM package 1.0.0](../proof_verifier_js/README.md)
accept only v2/security-100 proofs with a trusted v2 key. Verification checks the
proof and its authenticated recursion-chain hash; callers can compare the eight
public output words against application expectations. The TS API exposes
`publicOutput` and an optional expected-output check and recovers from WASM traps
with a fresh instance. Legacy v1/security-80 proofs require the old package.

The service supports tracing, optional Sentry, and Prometheus. The current
metric definitions are in [metrics.rs](src/metrics.rs); dashboards are in
[infra](../infra/). Use scoped CPU/unit checks when changing packaging; GPU
proving tests require the repository's GPU lock and suitable hardware.

## License

[MIT](../LICENSE-MIT) or [Apache 2.0](../LICENSE-APACHE).
