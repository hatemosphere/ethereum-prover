use crate::{decode_exact, envelope_body};

const VERIFICATION_KEY_MAGIC: [u8; 8] = *b"EVKEY001";

pub(crate) fn decode_verification_key(bytes: &[u8]) -> Result<[[u32; 8]; 3], String> {
    // 10 prefix bytes, two 32-byte app identifiers, and 24 u32s (at most 5 bytes each).
    if bytes.len() > 194 {
        return Err("verification key exceeds 194-byte limit".to_string());
    }
    let body = envelope_body(bytes, &VERIFICATION_KEY_MAGIC, "verification key")?;
    let (_app_bin_keccak, _app_text_keccak, hashes): ([u8; 32], [u8; 32], [[u32; 8]; 3]) =
        decode_exact(body, "verification key")?;
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Vec<u8> {
        let mut bytes = b"EVKEY001\x02\x64".to_vec();
        bytes.extend([0x11; 32]);
        bytes.extend([0x22; 32]);
        bytes.extend([1; 8]);
        for _ in 0..8 {
            bytes.extend([251, 44, 1]);
        }
        bytes.extend([0; 8]);
        bytes
    }

    #[test]
    fn producer_golden_key_decodes() {
        assert_eq!(
            decode_verification_key(&key()).unwrap(),
            [[1; 8], [300; 8], [0; 8]]
        );
    }

    #[test]
    fn incompatible_or_inexact_keys_are_rejected() {
        for (index, value, message) in [(0, b'X', "magic"), (8, 1, "version"), (9, 80, "security")]
        {
            let mut bytes = key()[..10].to_vec();
            bytes[index] = value;
            assert!(decode_verification_key(&bytes)
                .unwrap_err()
                .contains(message));
        }
        let mut trailing = key();
        trailing.push(0);
        assert!(decode_verification_key(&trailing)
            .unwrap_err()
            .contains("trailing"));
        assert!(decode_verification_key(&key()[..20]).is_err());
        assert!(decode_verification_key(&[0; 195])
            .unwrap_err()
            .contains("limit"));
    }
}
