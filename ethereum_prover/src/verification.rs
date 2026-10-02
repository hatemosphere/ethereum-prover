use std::io::Read as _;
use std::path::Path;

use anyhow::Context as _;
use full_statement_verifier::unified_circuit_statement::verify_unified_circuit_recursion_layer_sec_100;
use full_statement_verifier::verifier_common::{
    USE_REDUCED_BLAKE2_ROUNDS, errors::DebugErrorCreator,
};

use crate::{
    prover::proof_format::decode_proof_words, verification_key_format::decode_verification_key,
};

const MAX_DECOMPRESSED_PROOF_BYTES: u64 = 64 << 20;

/// Verifies a gzip EthProofs proof against a v2 verification key and returns the guest's
/// public output. The proof must verify and its authenticated recursion-chain hash must be
/// one the key trusts.
pub fn verify_proof_file(proof_path: &Path, key_path: &Path) -> anyhow::Result<[u32; 8]> {
    let key = decode_verification_key(
        &std::fs::read(key_path)
            .with_context(|| format!("failed to read {}", key_path.display()))?,
    )?;
    let compressed = std::fs::read(proof_path)
        .with_context(|| format!("failed to read {}", proof_path.display()))?;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .take(MAX_DECOMPRESSED_PROOF_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to decompress the proof")?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_DECOMPRESSED_PROOF_BYTES,
        "the decompressed proof exceeds {MAX_DECOMPRESSED_PROOF_BYTES} bytes"
    );
    let words = decode_proof_words(&bytes)?;
    let output = verify_proof_words(words)?;
    anyhow::ensure!(
        key.expected_chain_hashes
            .iter()
            .any(|hash| output[8..] == hash[..]),
        "the proof verifies but its recursion chain {:08x?} is not one the key trusts",
        &output[8..]
    );
    Ok(output[..8].try_into().expect("eight words"))
}

fn verify_proof_words(words: Vec<u32>) -> anyhow::Result<[u32; 16]> {
    std::thread::Builder::new()
        .name("unified verifier".to_string())
        .stack_size(1 << 27)
        .spawn(move || {
            let mut words = words.into_iter();
            let output = verify_unified_circuit_recursion_layer_sec_100::<
                _,
                DebugErrorCreator,
                USE_REDUCED_BLAKE2_ROUNDS,
            >(&mut words)
            .map_err(|err| anyhow::anyhow!("the proof does not verify: {err:?}"))?;
            anyhow::ensure!(words.next().is_none(), "words left after the proof");
            Ok(output)
        })
        .context("failed to spawn the verifier thread")?
        .join()
        .map_err(|_| anyhow::anyhow!("the verifier panicked on this proof"))?
}
