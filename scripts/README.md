# v3 build and packaging scripts

Use sibling checkouts of `ethereum-prover`, `zksync-os` (`av_integrate_v3`),
`airbender-platform`, and `zksync-airbender`. Their relative paths are Cargo
inputs; the old nested zksync-os submodule is not used. CI pins the compatible
revisions in [stack-revisions.env](../.github/stack-revisions.env).

## Rebuild artifacts

Install the pinned toolchain and the guest-build tools:

```sh
rustup toolchain install nightly-2026-08-09 --component rust-src --component llvm-tools-preview
cargo +nightly-2026-08-09 install cargo-binutils --version 0.4.0 --locked
RUST_MIN_STACK=1073741824 cargo +nightly-2026-08-09 install \
  --path ../airbender-platform/crates/cargo-airbender --no-default-features --locked
scripts/rebuild_artifacts.sh
```

`rebuild_artifacts.sh` runs the sibling `zksync-os/zksync_os/dump_bin.sh --type
eth-stf-fusaka`, then calls `copy_artifacts.sh`. The copy step:

1. Copies the complete `zksync_os/dist/eth_stf` distribution to `artifacts/eth_stf`.
2. Resolves the producer's Blake environment and copies trusted `.bin`/`.text`
   pairs from `zksync-airbender/tools/gkr_verifier` to `artifacts/fsv`.
3. Uses `FSV_DIR=artifacts/fsv` and `generate-verifier-artifacts` to generate the
   security-100 v2 key for that exact guest and verifier set.
4. Writes `recursion.env` and `build_metadata.txt` with four source commits,
   resolved Blake modes, wire formats, and SHA-256 hashes of all runtime artifacts
   (except the metadata file itself).

`copy_artifacts.sh` can reuse an already-built sibling guest distribution.
It normally builds `ethereum_prover --release --locked`; set `PROVER_BIN` to an
absolute path to a matching prebuilt binary to skip that build. Keep the binary
and all four checkouts at the intended revisions before generating metadata.

Blake resolution follows `full_statement_verifier::host_utils`: unrolled defaults
to compression; bridge inherits unrolled; final uses `RECURSION_FINAL_BLAKE`, then
`RECURSION_UNIFIED_BLAKE`, then compression. Both unrolled modes accept compression
or G-function; final also accepts special opcodes. Missing in-tree variants fail
the build. The bundle stores canonical mode names in `recursion.env`.

For a relocated service, source that file and set `FSV_DIR` to the bundled `fsv`
directory. `prover_pipeline::fsv_dir()` otherwise uses
`env!("CARGO_MANIFEST_DIR")/../tools/gkr_verifier`, which is a build-checkout path.
`host_utils::load_fsv_program()` loads the selected `.bin`/`.text` pair from it.
The v2 proof verification command itself needs only the proof and trusted VK.

## Docker

[build_docker.sh](build_docker.sh) builds from a filtered sibling-cluster context;
see the [container README](../docker/ethereum-prover/README.md). It embeds commit
arguments because `.git` worktree pointers are not portable inside an image.
The Dockerfile builds both guest and service using CUDA 13.3.1 and
`nightly-2026-08-09`. Generated ELF/FSV files are included in the image, but are not
added to Git.

`ubuntu_setup.sh` is a legacy machine-provisioning script for the pre-v3 stack;
it installs CUDA 12.9 and unrelated Bellman/CRS dependencies. Do not use it for
this v3 build. Use the pinned container or install the dependencies listed in its
Dockerfile on a matching host.
