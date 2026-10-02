use std::io::Read;

use crate::{decode_exact, envelope_body, MAX_PROOF_BYTES};

const PROOF_MAGIC: [u8; 8] = *b"EPROOF01";
const MAX_PROOF_WORDS: u64 = 16 * 1024 * 1024;

pub(crate) fn decode_proof_bytes(bytes: &[u8]) -> Result<Vec<u32>, String> {
    if bytes.len() > MAX_PROOF_BYTES {
        return Err("compressed proof exceeds 64 MiB limit".to_string());
    }
    let mut decoder = flate2::bufread::GzDecoder::new(bytes);
    let mut prefix = [0; 10];
    decoder
        .read_exact(&mut prefix)
        .map_err(|err| format!("gzip decode failed: {err}"))?;
    envelope_body(&prefix, &PROOF_MAGIC, "proof")?;
    let mut decompressed = prefix.to_vec();
    (&mut decoder)
        .take((MAX_PROOF_BYTES - prefix.len() + 1) as u64)
        .read_to_end(&mut decompressed)
        .map_err(|err| format!("gzip decode failed: {err}"))?;
    if decompressed.len() > MAX_PROOF_BYTES {
        return Err("decompressed proof exceeds 64 MiB limit".to_string());
    }
    if !decoder.get_ref().is_empty() {
        return Err("trailing bytes after gzip proof".to_string());
    }
    decode_proof_payload(&decompressed)
}

fn decode_proof_payload(bytes: &[u8]) -> Result<Vec<u32>, String> {
    let body = envelope_body(bytes, &PROOF_MAGIC, "proof")?;
    let (word_count, prefix_len): (u64, usize) =
        bincode::decode_from_slice(body, bincode::config::standard())
            .map_err(|err| format!("invalid proof word count: {err}"))?;
    if word_count > MAX_PROOF_WORDS {
        return Err("proof word count exceeds 16M-word limit".to_string());
    }
    // Every encoded u32 takes at least one byte; reject impossible lengths before allocation.
    if word_count > (body.len() - prefix_len) as u64 {
        return Err("truncated proof words".to_string());
    }
    decode_exact(body, "proof words")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn payload(words: &[u32]) -> Vec<u8> {
        let mut bytes = b"EPROOF01\x02\x64".to_vec();
        bytes.extend(bincode::serde::encode_to_vec(words, bincode::config::standard()).unwrap());
        bytes
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn v2_golden_stream_decodes() {
        let bytes = b"EPROOF01\x02\x64\x03\x01\xfb\x2c\x01\xfc\xff\xff\xff\xff";
        assert_eq!(
            decode_proof_bytes(&gzip(bytes)).unwrap(),
            [1, 300, u32::MAX]
        );
    }

    #[test]
    fn prefix_is_checked_before_body() {
        for (index, value, message) in [(0, b'X', "magic"), (8, 1, "version"), (9, 80, "security")]
        {
            let mut bytes = b"EPROOF01\x02\x64".to_vec();
            bytes[index] = value;
            assert!(decode_proof_bytes(&gzip(&bytes))
                .unwrap_err()
                .contains(message));
        }
    }

    #[test]
    fn lengths_and_trailing_bytes_are_rejected() {
        let mut oversized = b"EPROOF01\x02\x64".to_vec();
        oversized.extend(bincode::encode_to_vec(u64::MAX, bincode::config::standard()).unwrap());
        assert!(decode_proof_payload(&oversized)
            .unwrap_err()
            .contains("limit"));
        let mut trailing = payload(&[1]);
        trailing.push(0);
        assert!(decode_proof_payload(&trailing)
            .unwrap_err()
            .contains("trailing"));
        let mut truncated = payload(&[u32::MAX]);
        truncated.pop();
        assert!(decode_proof_payload(&truncated).is_err());
        let mut compressed = gzip(&payload(&[1]));
        compressed.push(0);
        assert!(decode_proof_bytes(&compressed)
            .unwrap_err()
            .contains("trailing"));
        let mut corrupt = gzip(&payload(&[1]));
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        assert!(decode_proof_bytes(&corrupt).unwrap_err().contains("gzip"));
    }

    #[test]
    fn gzip_expansion_is_bounded() {
        let mut bytes = b"EPROOF01\x02\x64".to_vec();
        bytes.resize(MAX_PROOF_BYTES + 1, 0);
        assert!(decode_proof_bytes(&gzip(&bytes))
            .unwrap_err()
            .contains("decompressed"));
    }
}
