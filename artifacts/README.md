# v3 runtime artifacts

Regenerate with `scripts/rebuild_artifacts.sh` from the repository root. It runs
`dump_bin.sh --type eth-stf-fusaka --reproducible` in the sibling zksync-os checkout.
The pinned guest builder mounts the sibling cluster at `/src`, so host checkout
paths do not enter the guest. Docker packaging consumes this prebuilt distribution.
The complete runtime set is:

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
Reproducible builds with the same guest sources, features, builder image, trusted
FSVs, and Blake modes must produce the same `app.bin`, `app.text`, `app.elf`, and
VK bytes. The manifest also records Git
branch/dirty state, and `build_metadata.txt` records source commits and build time;
those provenance files can differ between checkouts without changing the VK.
Artifact assembly rejects guests whose manifest is not marked reproducible.
Legacy v1/security-80 artifacts belong to the old repository/package versions.

## Reproducibility check (2026-10-02)

Independent builds from the sibling cluster and a copy under `/tmp` produced
byte-identical `app.bin`, `app.text`, `app.elf`, and independently generated VKs.
The source revisions were zksync-os `a9eacb39`, airbender-platform `6e43c882`, and
zksync-airbender `1d94f2a77`; the current hashes are in `build_metadata.txt`.
Both runs used `airbender-build:nightly-2026-08-09`, local image ID
`sha256:8a4a51899b08809ad36d6f480e9619bf5e517848975b135e6c17bbe3b9c06c92`.
Recreating that builder image on another machine was not part of this check.
The local `ethereum-prover-v3:repro` image shipped the same guest, manifest, FSV,
mode file, and VK bytes as native packaging. Its UID 10001 runtime regenerated
the same key using only the shipped files, with no source checkout present.

A baseline using pre-safegcd zksync-os `85c318c8` and airbender-platform `617caf03`
produced the same executable `app.bin`, `app.text`, and VK. Its ELF SHA-256 was
`343031b7c9ae97478ec29b3080cb82847f026c377aea500c69007dd14c7e3125`, versus
`517e3ef2e01e4f79c27f6e1fb4ae3c723ad2e985db4b4dffd0154bc858732525` after safegcd.
Only the non-allocated `.symtab` and `.strtab` sections differ; all other bytes
and the stripped ELF match. These symbol-table changes do not change the VK.
The bundle retains the original ELF and its matching manifest hash.
