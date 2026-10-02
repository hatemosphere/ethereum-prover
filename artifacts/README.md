# v3 runtime artifacts

Regenerate with `scripts/rebuild_artifacts.sh` from the repository root. The
complete runtime set is:

- `eth_stf/manifest.toml`, `app.bin`, `app.elf`, `app.text`: the Fusaka Ethereum STF
  distribution from the sibling zksync-os checkout.
- `fsv/*.bin` and `fsv/*.text`: trusted in-tree recursion verifier programs for
  the selected unrolled, bridge, and final Blake modes.
- `recursion_unified_v3_security_100.vk.bin`: `EVKEY001` version 2, security 100.
- `recursion.env`: canonical Blake modes to source before starting the service.
- `build_metadata.txt`: four source commits, formats, modes, and artifact hashes.

The already-tracked guest binary and text have moved under `eth_stf/`. They, the
small manifest, v2 VK, mode file, and metadata are tracked. The guest ELF and FSV
binaries are generated, gitignored, and shipped in the Docker/runtime bundle.
A source checkout must rebuild the bundle before proving; the tracked files alone
are not a complete guest distribution.

Set `FSV_DIR` to the bundled `fsv` directory after relocation. Keep these files
and the VK together; a different guest or Blake mode can require a different key.
Guest binaries embed compiler source paths, so native and container builds can
also produce different hashes and keys at the same source revisions. Distribute
the VK from the actual prover bundle; do not substitute a key from another build.
Legacy v1/security-80 artifacts belong to the old repository/package versions.
