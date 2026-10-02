const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { performance } = require('node:perf_hooks');
const { gzipSync, gunzipSync } = require('node:zlib');
const { Worker, isMainThread, parentPort, workerData } = require('node:worker_threads');

// Every case owns a fresh WASM instance. Never reuse WASM state after a trap.
if (!isMainThread) {
    const wasm = require('../pkg/proof_verifier_wasm.js');
    if (workerData.quiet) console.error = () => {};
    let verifier, handle, result;
    try {
        if (workerData.legacy) {
            wasm.WasmVerifier.fromLegacyKey(new Uint8Array(), new Uint8Array());
            throw new Error('legacy split key unexpectedly accepted');
        }
        verifier = wasm.WasmVerifier.fromKey(workerData.key);
        handle = wasm.deserialize_proof_bytes(workerData.proof);
        const times = [];
        let output;
        for (let i = 0; i < (workerData.iterations || 1); i++) {
            const start = performance.now();
            result = verifier.verifyProof(handle, workerData.expectedOutput);
            times.push(performance.now() - start);
            if (!result.success) {
                parentPort.postMessage({ success: false, trapped: false, error: result.error() });
                result.free();
                handle.free();
                verifier.free();
                return;
            }
            output = Array.from(result.publicOutput);
            result.free();
        }
        handle.free();
        verifier.free();
        parentPort.postMessage({ success: true, output, times });
    } catch (err) {
        // No WASM calls (including free) after a trap; worker exit discards the instance.
        parentPort.postMessage({ success: false, trapped: err instanceof WebAssembly.RuntimeError, error: String(err) });
    }
} else {
    main().catch(err => { console.error(err); process.exitCode = 1; });
}

function run(data) {
    return new Promise((resolve, reject) => {
        const worker = new Worker(__filename, { workerData: data });
        let response;
        worker.once('message', value => { response = value; });
        worker.once('error', reject);
        worker.once('exit', code => {
            if (code !== 0 || !response) reject(new Error(`verification worker exited ${code}`));
            else resolve(response);
        });
    });
}

function readInteger(bytes, cursor) {
    const tag = bytes[cursor.offset++];
    if (tag < 251) return tag;
    if (tag === 251) { const n = bytes.readUInt16LE(cursor.offset); cursor.offset += 2; return n; }
    if (tag === 252) { const n = bytes.readUInt32LE(cursor.offset); cursor.offset += 4; return n; }
    throw new Error(`unexpected fixture integer tag ${tag}`);
}

function writeInteger(bytes, offset, value) {
    if (value < 251) { bytes[offset] = value; return offset + 1; }
    if (value <= 65535) { bytes[offset] = 251; bytes.writeUInt16LE(value, offset + 1); return offset + 3; }
    bytes[offset] = 252;
    bytes.writeUInt32LE(value >>> 0, offset + 1);
    return offset + 5;
}

function encodeIntegers(prefix, words, includeLength) {
    const bytes = Buffer.alloc(prefix.length + 5 + words.length * 5);
    prefix.copy(bytes);
    let offset = prefix.length;
    if (includeLength) offset = writeInteger(bytes, offset, words.length);
    for (const word of words) offset = writeInteger(bytes, offset, word);
    return bytes.subarray(0, offset);
}

async function main() {
    const [fixtures, nativePath = path.join(__dirname, 'native_reference.json')] = process.argv.slice(2);
    assert(fixtures, 'usage: node tests/node.cjs FIXTURE_DIR [NATIVE_REFERENCE_JSON]');
    const native = JSON.parse(fs.readFileSync(nativePath));
    const key = fs.readFileSync(path.join(fixtures, 'vk/recursion_unified_v3_security_100.vk.bin'));
    const cursor = { offset: 74 };
    const hashes = Array.from({ length: 24 }, () => readInteger(key, cursor));
    assert.equal(cursor.offset, key.length);
    const report = { node: process.version, cases: [], timings: {} };
    async function check(name, data, success, errorPattern) {
        const result = await run({ key, quiet: !success, ...data });
        assert.equal(result.success, success, `${name}: ${JSON.stringify(result)}`);
        if (errorPattern) assert.match(result.error, errorPattern, name);
        report.cases.push({ name, success: true, trapped: result.trapped || false });
        console.log(`PASS ${name}${result.trapped ? ' (trap caught as failure)' : ''}`);
        return result;
    }
    for (const block of ['26078427', '26078715']) {
        const shape = [0, 1, 2].find(index => hashes.slice(index * 8, index * 8 + 8).every((word, i) => word === native[block].output[8 + i]));
        assert.notEqual(shape, undefined, `${block}: native chain hash must match the VK`);
        assert.equal(native[block].chain_entries, shape + 3);
        const proof = fs.readFileSync(path.join(fixtures, 'blocks', block, 'proof_v2.bin.gz'));
        if (native[block].proof_sha256) {
            assert.equal(createHash('sha256').update(proof).digest('hex'), native[block].proof_sha256);
        }
        const decoded = gunzipSync(proof);
        const wordCursor = { offset: 10 };
        const count = readInteger(decoded, wordCursor);
        const words = Array.from({ length: count }, () => readInteger(decoded, wordCursor));
        assert.equal(wordCursor.offset, decoded.length);
        const pack = items => gzipSync(encodeIntegers(decoded.subarray(0, 10), items, true));
        const expected = native[block].output.slice(0, 8);
        const valid = await check(`${block}: valid/native parity`, { proof, iterations: 6 }, true);
        assert.deepEqual(valid.output, expected);
        const warm = valid.times.slice(1).sort((a, b) => a - b);
        report.timings[block] = { chain_entries: native[block].chain_entries, first_ms: valid.times[0], warm_median_ms: warm[2], samples_ms: valid.times, public_output: valid.output };
        await check(`${block}: matching expected output`, { proof, expectedOutput: Uint32Array.from(expected) }, true);
        const wrongOutput = expected.slice(); wrongOutput[0] ^= 1;
        await check(`${block}: wrong expected output`, { proof, expectedOutput: Uint32Array.from(wrongOutput) }, false, /public output/);
        const flipped = words.slice();
        const flipIndex = Math.floor(count / 2);
        flipped[flipIndex] = (flipped[flipIndex] ^ 1) >>> 0;
        await check(`${block}: bit flip`, { proof: pack(flipped) }, false);
        const wrongHashes = hashes.slice(); wrongHashes[shape * 8] = (wrongHashes[shape * 8] ^ 1) >>> 0;
        await check(`${block}: wrong matching VK hash`, { proof, key: encodeIntegers(key.subarray(0, 74), wrongHashes, false) }, false, /chain/);
        await check(`${block}: truncated word stream`, { proof: pack(words.slice(0, -1)) }, false);
        await check(`${block}: appended word`, { proof: pack([...words, 0]) }, false, /trailing words/);
        await check(`${block}: appended bincode byte`, { proof: gzipSync(Buffer.concat([decoded, Buffer.from([0])])) }, false, /trailing/);
        await check(`${block}: truncated bincode`, { proof: gzipSync(decoded.subarray(0, decoded.length - 1)) }, false);
        await check(`${block}: truncated gzip`, { proof: proof.subarray(0, proof.length - 1) }, false, /gzip/);
        const corrupt = Buffer.from(proof); corrupt[corrupt.length - 1] ^= 1;
        await check(`${block}: corrupt gzip`, { proof: corrupt }, false, /gzip/);
        await check(`${block}: appended gzip bytes`, { proof: Buffer.concat([proof, Buffer.from([0])]) }, false, /trailing/);
        for (const [label, index, value] of [ ['magic', 0, 0], ['v1', 8, 1], ['unknown version', 8, 3], ['security 80', 9, 80], ['unknown security', 9, 99] ]) {
            const prefix = Buffer.from(decoded.subarray(0, 10)); prefix[index] = value;
            await check(`${block}: proof ${label} before body`, { proof: gzipSync(prefix) }, false, /magic|version|security/);
            const badKey = Buffer.from(key.subarray(0, 10)); badKey[index] = value;
            await check(`${block}: VK ${label} before body`, { proof, key: badKey }, false, /magic|version|security/);
        }
        const oversized = Buffer.concat([decoded.subarray(0, 10), Buffer.from([253]), Buffer.alloc(8, 255)]);
        await check(`${block}: oversized word count`, { proof: gzipSync(oversized) }, false, /word count.*limit/);
        const trap = await check(`${block}: empty stream traps`, { proof: pack([]) }, false);
        assert.equal(trap.trapped, true);
        const recovered = await check(`${block}: fresh instance after trap`, { proof }, true);
        assert.deepEqual(recovered.output, expected);
    }
    const proof = fs.readFileSync(path.join(fixtures, 'blocks/26078427/proof_v2.bin.gz'));
    await check('legacy split keys', { proof, legacy: true }, false, /legacy split keys/);
    await check('oversized key', { proof, key: Buffer.alloc(195) }, false, /limit/);
    await check('truncated key', { proof, key: key.subarray(0, key.length - 1) }, false);
    const smallKey = Buffer.concat([key.subarray(0, 74), Buffer.alloc(24), Buffer.from([0])]);
    await check('trailing key byte', { proof, key: smallKey }, false, /trailing/);
    await check('invalid expected-output length', { proof, expectedOutput: new Uint32Array(7) }, false, /exactly 8/);
    await check('oversized compressed input', { proof: Buffer.alloc(64 * 1024 * 1024 + 1) }, false, /compressed proof.*limit/);
    const bomb = Buffer.alloc(64 * 1024 * 1024 + 1);
    Buffer.from('EPROOF01\x02\x64', 'binary').copy(bomb);
    await check('oversized gzip expansion', { proof: gzipSync(bomb) }, false, /decompressed proof.*limit/);
    await check('concatenated gzip members', { proof: Buffer.concat([proof, proof]) }, false, /trailing/);
    console.log(JSON.stringify(report, null, 2));
}
