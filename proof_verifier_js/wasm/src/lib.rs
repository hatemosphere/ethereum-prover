use console_error_panic_hook::set_once as set_panic_hook;
use wasm_bindgen::prelude::*;

mod proof_format;
mod unified_verifier;
mod verification_key_format;

const MAX_PROOF_BYTES: usize = 64 * 1024 * 1024;

fn envelope_body<'a>(bytes: &'a [u8], magic: &[u8; 8], what: &str) -> Result<&'a [u8], String> {
    if bytes.len() < 10 {
        return Err(format!("truncated {what} envelope"));
    }
    if &bytes[..8] != magic {
        return Err(format!(
            "unsupported {what} magic; legacy formats are not supported"
        ));
    }
    if bytes[8] != 2 {
        return Err(format!(
            "unsupported {what} version {}; expected 2",
            bytes[8]
        ));
    }
    if bytes[9] != 100 {
        return Err(format!(
            "unsupported {what} security {}; expected 100",
            bytes[9]
        ));
    }
    Ok(&bytes[10..])
}

fn decode_exact<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T, String> {
    let (value, read) = bincode::serde::decode_from_slice(
        bytes,
        bincode::config::standard().with_limit::<MAX_PROOF_BYTES>(),
    )
    .map_err(|err| format!("failed to parse {what}: {err}"))?;
    if read != bytes.len() {
        return Err(format!("trailing bytes after {what}"));
    }
    Ok(value)
}

#[wasm_bindgen]
pub struct WasmVerifier {
    expected_chain_hashes: [[u32; 8]; 3],
}

#[wasm_bindgen]
impl WasmVerifier {
    #[wasm_bindgen(js_name = fromKey)]
    pub fn from_key(vk_bin: &[u8]) -> Result<Self, JsValue> {
        set_panic_hook();
        let expected_chain_hashes = verification_key_format::decode_verification_key(vk_bin)
            .map_err(|err| JsValue::from_str(&err))?;
        Ok(Self {
            expected_chain_hashes,
        })
    }

    #[wasm_bindgen(js_name = fromLegacyKey)]
    pub fn from_legacy_key(_setup_bin: &[u8], _layout_bin: &[u8]) -> Result<Self, JsValue> {
        Err(JsValue::from_str(
            "legacy split keys are not supported; use an EVKEY001 v2 key",
        ))
    }

    #[wasm_bindgen(js_name = verifyProof)]
    pub fn verify_proof(
        &self,
        handle: &ProofHandle,
        expected_output: Option<Vec<u32>>,
    ) -> VerifyResult {
        match unified_verifier::verify(
            &handle.words,
            &self.expected_chain_hashes,
            expected_output.as_deref(),
        ) {
            Ok(output) => VerifyResult {
                success: true,
                error: None,
                public_output: Some(output),
            },
            Err(err) => VerifyResult {
                success: false,
                error: Some(err),
                public_output: None,
            },
        }
    }
}

#[wasm_bindgen]
pub struct ProofHandle {
    words: Vec<u32>,
}

#[wasm_bindgen]
pub fn deserialize_proof_bytes(proof_bytes: &[u8]) -> Result<ProofHandle, JsValue> {
    let words =
        proof_format::decode_proof_bytes(proof_bytes).map_err(|err| JsValue::from_str(&err))?;
    Ok(ProofHandle { words })
}

#[wasm_bindgen]
pub struct VerifyResult {
    success: bool,
    error: Option<String>,
    public_output: Option<[u32; 8]>,
}

#[wasm_bindgen]
impl VerifyResult {
    #[wasm_bindgen(getter)]
    pub fn success(&self) -> bool {
        self.success
    }

    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }

    #[wasm_bindgen(getter, js_name = publicOutput)]
    pub fn public_output(&self) -> Option<Vec<u32>> {
        self.public_output.map(|output| output.to_vec())
    }
}
