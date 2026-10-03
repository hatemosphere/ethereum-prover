/** Browser build: fetch the WASM shipped next to the package. */
export async function loadWasmBytes(): Promise<ArrayBuffer> {
  const response = await fetch(new URL("../wasm/pkg/proof_verifier_wasm_bg.wasm", import.meta.url));
  if (!response.ok) throw new Error(`WASM download failed: ${response.status}`);
  return response.arrayBuffer();
}
