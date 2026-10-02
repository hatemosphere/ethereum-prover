# v3 WASM verifier

This crate accepts gzip-compressed `EPROOF01` version 2 word streams and trusted
`EVKEY001` version 2 verification keys, both at security 100. It rejects v1,
security 80, and legacy split keys. The verifier checks the complete word stream
and accepts only a chain hash listed in the supplied key.

```js
const verifier = wasm.WasmVerifier.fromKey(trustedKeyBytes);
const proof = wasm.deserialize_proof_bytes(gzipProofBytes);
const result = verifier.verifyProof(proof); // Optional second argument: Uint32Array(8).
if (result.success) console.log(result.publicOutput); // Eight verified u32 words.
else console.error(result.error());
result.free();
proof.free();
verifier.free();
```

Decode errors throw; ordinary verification errors return `success: false` with no
`publicOutput`. Malformed proof words can also trap in the upstream verifier.
The JS caller must catch exceptions, report failure, and discard the entire WASM
instance before another verification. Do not call `free()` on a trapped instance.
`tests/node.cjs` demonstrates this boundary with a fresh worker for each case.
Updating the TypeScript package and browser demo is a separate port step.

Compressed and decompressed proofs are each limited to 64 MiB, the decoded vector
to 16M words, and keys to 194 bytes. Prefix validation precedes body decoding;
trailing bincode bytes, proof words, and gzip bytes or members are rejected.

From the repository root, with `wasm32-unknown-unknown` and `wasm-pack` installed:

```sh
cargo check -p proof_verifier_wasm --lib --target wasm32-unknown-unknown
(cd proof_verifier_js/wasm && wasm-pack build --target nodejs)
V3_VERIFIER_FIXTURES=/path/to/fixtures cargo test -p proof_verifier_wasm --lib --release
node proof_verifier_js/wasm/tests/node.cjs /path/to/fixtures
```

The fixture directory contains `blocks/{26078427,26078503,26078715}/proof_v2.bin.gz` and
`vk/recursion_unified_v3_security_100.vk.bin`. The committed native reference was
obtained by calling `prover_pipeline::verify_artifact` on each corresponding
`artifact.json`, with the producing `eth-stf-fusaka` guest and in-tree FSV binaries
(`FSV_DIR` unset). It stores all 16 native output words and the compressed proof
hashes; the Node test compares the first eight words with `publicOutput`.
Block 26078503 has no unrolled recursion layer and three chain entries. The other
two fixtures have one unrolled recursion layer and four chain entries. The test
also changes only the key's hash[0]: it rejects 26078503 and accepts the other two.
The two-or-more-unrolled shape still needs a fixture.

The Node test checks valid proofs, format and corruption failures, trusted-key
and optional-output mismatches, limits, and recovery with a fresh instance after
a trap. Timings cover `verifyProof` only: one first call and five subsequent calls
in the same instance. Decode, module loading, and worker startup are excluded.
