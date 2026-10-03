import { defineConfig } from "tsup";

const external = ["../wasm/pkg/proof_verifier_wasm.js", "../wasm/pkg/proof_verifier_wasm_bg.wasm.base64.js"];

export default defineConfig([
  { entry: ["src/index.ts"], format: ["esm"], dts: true, external },
  {
    entry: { node: "src/index.ts" },
    format: ["esm"],
    external,
    esbuildPlugins: [{
      name: "node-wasm-loader",
      setup(build) {
        build.onResolve({ filter: /^\.\/load-wasm$/ }, args => ({ path: `${args.resolveDir}/load-wasm.node.ts` }));
      }
    }]
  }
]);
