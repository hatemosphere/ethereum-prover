import createBindings from "../wasm/pkg/proof_verifier_wasm.js";
import { loadWasmBytes } from "./load-wasm";

/** Opaque proof owned by the verifier that deserialized it. */
export type ProofHandle = { free(): void };

export type VerificationResult = {
  success: boolean;
  error: string | null;
  /** Eight verified words on success; null on failure. */
  publicOutput: Uint32Array | null;
};

export type VerificationKey = Uint8Array;

export type VerifierOptions = {
  /** Trusted EVKEY001 v2 key for the guest and security 100. */
  verificationKey: VerificationKey;
};

export type Verifier = {
  /** Decode a gzip EPROOF01 v2 proof. Throws on invalid input. */
  deserializeProofBytes(proofBytes: Uint8Array): ProofHandle;
  /** A trap returns failure and invalidates all handles from that instance. */
  verifyProof(handle: ProofHandle, expectedOutput?: Uint32Array): VerificationResult;
  /** Release the verifier and invalidate its handles. */
  free(): void;
};

type Bindings = ReturnType<typeof createBindings>;
type Instance = {
  bindings: Bindings;
  verifier: ReturnType<Bindings["WasmVerifier"]["fromKey"]>;
  active: boolean;
};
type HandleState = {
  instance: Instance;
  inner: ReturnType<Bindings["deserialize_proof_bytes"]>;
};

// Compiled once when the package is imported, so instances can be created synchronously.
const wasmModule = await WebAssembly.compile(await loadWasmBytes());

function failure(error: unknown) {
  return {
    success: false,
    error: error instanceof Error ? error.message : String(error),
    publicOutput: null
  };
}

class VerifierImpl implements Verifier {
  private instance?: Instance;
  private closed = false;
  private readonly handles = new WeakMap<ProofHandle, HandleState>();

  constructor(private readonly module: WebAssembly.Module, private readonly key: Uint8Array) {
    this.current();
  }

  private current(): Instance {
    if (this.closed) throw new Error("verifier has been freed");
    if (!this.instance) {
      const bindings = createBindings();
      bindings.initSync({ module: this.module });
      this.instance = { bindings, verifier: bindings.WasmVerifier.fromKey(this.key), active: true };
    }
    return this.instance;
  }

  private abandonOnTrap(error: unknown, instance: Instance) {
    if (error instanceof WebAssembly.RuntimeError) {
      instance.active = false;
      this.instance = undefined;
    }
  }

  deserializeProofBytes(proofBytes: Uint8Array): ProofHandle {
    const instance = this.current();
    try {
      const state: HandleState = { instance, inner: instance.bindings.deserialize_proof_bytes(proofBytes) };
      const handle = { free: () => this.freeHandle(handle) };
      this.handles.set(handle, state);
      return handle;
    } catch (error) {
      this.abandonOnTrap(error, instance);
      throw new Error(failure(error).error);
    }
  }

  private freeHandle(handle: ProofHandle): void {
    const state = this.handles.get(handle);
    this.handles.delete(handle);
    if (state?.instance.active) {
      try { state.inner.free(); }
      catch (error) { this.abandonOnTrap(error, state.instance); throw error; }
    }
  }

  verifyProof(handle: ProofHandle, expectedOutput?: Uint32Array): VerificationResult {
    const state = this.handles.get(handle);
    if (!state?.inner || !state.instance.active || this.closed) {
      return failure("proof handle is foreign, freed, or invalidated; deserialize the proof again");
    }
    const { instance, inner } = state;
    try {
      const result = instance.verifier.verifyProof(inner, expectedOutput);
      const output = { success: result.success, error: result.error() ?? null, publicOutput: result.publicOutput ?? null };
      result.free();
      return output;
    } catch (error) {
      this.abandonOnTrap(error, instance);
      return failure(error);
    }
  }

  free(): void {
    if (this.closed) return;
    this.closed = true;
    const instance = this.instance;
    this.instance = undefined;
    if (instance) {
      instance.active = false;
      instance.verifier.free();
    }
  }
}

/** Create an isolated verifier with a trusted v3 key (the WASM module is compiled at import). */
export async function createVerifier(options: VerifierOptions): Promise<Verifier> {
  if (!options || "setupBin" in options || "layoutBin" in options) {
    throw new Error("legacy split keys are not supported; supply verificationKey (EVKEY001 v2)");
  }
  if (!(options.verificationKey instanceof Uint8Array)) {
    throw new Error("verificationKey must be a Uint8Array containing an EVKEY001 v2 key");
  }
  return new VerifierImpl(wasmModule, new Uint8Array(options.verificationKey));
}

/**
 * Synchronous one-shot verification for callers with a `verify_stark(proof, vk)` contract,
 * such as EthProofs. Returns whether the gzip EPROOF01 v2 proof verifies against the
 * EVKEY001 v2 key; malformed keys or proofs throw. Each call uses a fresh WASM instance.
 */
export function verify_stark(proofBytes: Uint8Array, verificationKey: Uint8Array): boolean {
  const verifier = new VerifierImpl(wasmModule, new Uint8Array(verificationKey));
  try {
    const handle = verifier.deserializeProofBytes(proofBytes);
    try {
      return verifier.verifyProof(handle).success;
    } finally {
      handle.free();
    }
  } finally {
    verifier.free();
  }
}
