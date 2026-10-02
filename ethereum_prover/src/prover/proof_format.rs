use airbender_host::{Proof, ProverLevel, raw::ProofArtifact};
use full_statement_verifier::host_utils::build_unified_stream;

pub(crate) const PROOF_MAGIC: [u8; 8] = *b"EPROOF01";
const PROOF_FORMAT_VERSION: u8 = 2;
const PROOF_SECURITY: u8 = 100;

/// Version 2 carries the word stream the final unified full-statement verifier consumes
/// (`build_unified_stream` of the final recursion layer), not a native proof structure.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct EncodedProof {
    magic: [u8; 8],
    version: u8,
    security: u8,
    proof_words: Vec<u32>,
}

pub(crate) fn encode_proof(proof: Proof) -> anyhow::Result<Vec<u8>> {
    encode_artifact(&final_artifact(proof)?)
}

/// The native artifact of a final unified recursion proof.
pub fn final_artifact(proof: Proof) -> anyhow::Result<ProofArtifact> {
    let Proof::Real(proof) = proof else {
        anyhow::bail!("only real proofs can be encoded for EthProofs");
    };
    anyhow::ensure!(
        proof.level() == ProverLevel::RecursionUnified,
        "only final unified recursion proofs can be encoded for EthProofs, got {:?}",
        proof.level()
    );
    Ok(proof.into_inner())
}

pub fn encode_artifact(artifact: &ProofArtifact) -> anyhow::Result<Vec<u8>> {
    let proof_words = build_unified_stream(&artifact.setups, &artifact.proof);
    Ok(encode_envelope(proof_words)?)
}

fn encode_envelope(proof_words: Vec<u32>) -> Result<Vec<u8>, bincode::error::EncodeError> {
    // The outer EthProofs transport still handles gzip + base64. This envelope
    // is only the inner bincode payload, and starts with a fixed magic so
    // verifiers can distinguish it from legacy raw Airbender proofs.
    let encoded = EncodedProof {
        magic: PROOF_MAGIC,
        version: PROOF_FORMAT_VERSION,
        security: PROOF_SECURITY,
        proof_words,
    };
    bincode::serde::encode_to_vec(&encoded, bincode::config::standard())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Magic, version 2, security 100, then three words: 1, 300 and u32::MAX cover the
    // one-, three- and five-byte varint forms.
    const THREE_WORD_ENVELOPE_HEX: &str = "4550524f4f46303102640301fb2c01fcffffffff";

    #[test]
    fn envelope_matches_golden_vector() {
        let bytes = encode_envelope(vec![1, 300, u32::MAX]).expect("encode envelope");
        assert_eq!(to_hex(&bytes), THREE_WORD_ENVELOPE_HEX);
    }

    #[test]
    fn envelope_round_trips() {
        let bytes = encode_envelope(vec![7, 8, 9]).expect("encode envelope");
        let (decoded, read): (EncodedProof, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .expect("decode envelope");
        assert_eq!(read, bytes.len());
        assert_eq!(decoded.magic, PROOF_MAGIC);
        assert_eq!(decoded.version, PROOF_FORMAT_VERSION);
        assert_eq!(decoded.security, PROOF_SECURITY);
        assert_eq!(decoded.proof_words, vec![7, 8, 9]);
    }

    fn to_hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}
