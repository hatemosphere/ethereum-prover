import wasmBase64 from "../wasm/pkg/proof_verifier_wasm_bg.wasm.base64.js";

/**
 * Node build: the WASM is embedded, so serverless bundles that copy only traced JS files
 * (e.g. Next.js on Vercel) cannot lose it.
 */
export async function loadWasmBytes(): Promise<Uint8Array> {
  return Uint8Array.from(atob(wasmBase64), char => char.charCodeAt(0));
}
