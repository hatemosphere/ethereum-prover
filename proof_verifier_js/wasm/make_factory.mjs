import { readFileSync, writeFileSync, appendFileSync } from "node:fs";

const js = new URL("../ts/wasm/pkg/proof_verifier_wasm.js", import.meta.url);
// Each factory call owns its WASM state. Explicit frees prevent GC from calling
// into an abandoned instance after a trap.
writeFileSync(js, `export default function createBindings() {
const FinalizationRegistry = undefined;
${readFileSync(js, "utf8")}
return wasm_bindgen;
}
`);
appendFileSync(new URL("../ts/wasm/pkg/proof_verifier_wasm.d.ts", import.meta.url), `
export default function createBindings(): typeof wasm_bindgen & {
    initSync(input: { module: WebAssembly.Module }): InitOutput;
};
`);
// Embedded copy for the Node entry point.
const wasm = readFileSync(new URL("../ts/wasm/pkg/proof_verifier_wasm_bg.wasm", import.meta.url));
writeFileSync(new URL("../ts/wasm/pkg/proof_verifier_wasm_bg.wasm.base64.js", import.meta.url),
  `export default ${JSON.stringify(wasm.toString("base64"))};\n`);
writeFileSync(new URL("../ts/wasm/pkg/proof_verifier_wasm_bg.wasm.base64.d.ts", import.meta.url),
  "declare const wasmBase64: string;\nexport default wasmBase64;\n");
const manifest = new URL("../ts/wasm/pkg/package.json", import.meta.url);
writeFileSync(manifest, JSON.stringify({ ...JSON.parse(readFileSync(manifest, "utf8")), type: "module" }, null, 2) + "\n");
