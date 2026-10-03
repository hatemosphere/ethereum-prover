use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use alloy::primitives::B256;
use anyhow::Context as _;
use flate2::{Compression, write::GzEncoder};
use serde::{Deserialize, Serialize};

/// Compresses an encoded proof envelope. The archived file and the EthProofs payload (before
/// base64) are these exact bytes, which the JS/WASM verifier accepts directly.
pub(crate) fn gzip_proof_bytes(proof_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(proof_bytes)
        .context("failed to write proof bytes into gzip encoder")?;
    encoder.finish().context("failed to finish gzip encoding")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofManifest {
    pub block_number: u64,
    pub block_hash: B256,
    pub cycles: u64,
    /// Reported proving time: prover input recording + proving.
    pub proving_time_ms: u64,
    pub prover_input_ms: u64,
    pub proof_sha256: String,
    pub proof_bytes: usize,
    pub created_at_unix: u64,
}

/// `<dir>/<block>/proof.bin.gz` and `manifest.json`, each written atomically.
#[derive(Debug, Clone)]
pub struct ProofArchive {
    dir: PathBuf,
}

impl ProofArchive {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn store(
        &self,
        block_number: u64,
        block_hash: B256,
        cycles: u64,
        proving_time_ms: u64,
        prover_input_ms: u64,
        envelope: &[u8],
    ) -> anyhow::Result<PathBuf> {
        let block_dir = self.dir.join(block_number.to_string());
        std::fs::create_dir_all(&block_dir)
            .with_context(|| format!("failed to create {}", block_dir.display()))?;
        let proof = gzip_proof_bytes(envelope)?;
        let proof_path = block_dir.join("proof.bin.gz");
        write(&proof_path, &proof)?;
        let manifest = ProofManifest {
            block_number,
            block_hash,
            cycles,
            proving_time_ms,
            prover_input_ms,
            proof_sha256: alloy::hex::encode(sha256(&proof)),
            proof_bytes: proof.len(),
            created_at_unix: unix_now(),
        };
        write(
            &block_dir.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        Ok(proof_path)
    }
}

fn write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    crate::utils::write_atomic(path, bytes)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_writes_proof_and_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let archive = ProofArchive::new(dir.path().to_path_buf());
        let path = archive
            .store(7, B256::repeat_byte(0xab), 123, 4567, 89, b"envelope")
            .unwrap();
        let proof = std::fs::read(&path).unwrap();
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::GzDecoder::new(proof.as_slice()),
            &mut decoded,
        )
        .unwrap();
        assert_eq!(decoded, b"envelope");
        let manifest: ProofManifest =
            serde_json::from_slice(&std::fs::read(dir.path().join("7/manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest.cycles, 123);
        assert_eq!(manifest.proving_time_ms, 4567);
        assert_eq!(manifest.proof_bytes, proof.len());
        assert_eq!(manifest.proof_sha256, alloy::hex::encode(sha256(&proof)));
    }
}
