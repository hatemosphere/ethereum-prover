pub(crate) const VERIFICATION_KEY_MAGIC: [u8; 8] = *b"EVKEY001";
const VERIFICATION_KEY_FORMAT_VERSION: u8 = 2;
const VERIFICATION_KEY_SECURITY: u8 = 100;

/// Version 2 binds proofs to one guest program: a proof is accepted only if the recursion
/// chain hash its final verifier outputs equals one of `expected_chain_hashes` (zero, one,
/// or two and more unrolled recursion layers). The app hashes identify the guest.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct EncodedVerificationKey {
    magic: [u8; 8],
    version: u8,
    security: u8,
    app_bin_keccak: [u8; 32],
    app_text_keccak: [u8; 32],
    expected_chain_hashes: [[u32; 8]; 3],
}

pub(crate) fn encode_verification_key(
    app_bin_keccak: [u8; 32],
    app_text_keccak: [u8; 32],
    expected_chain_hashes: [[u32; 8]; 3],
) -> Result<Vec<u8>, bincode::error::EncodeError> {
    let encoded = EncodedVerificationKey {
        magic: VERIFICATION_KEY_MAGIC,
        version: VERIFICATION_KEY_FORMAT_VERSION,
        security: VERIFICATION_KEY_SECURITY,
        app_bin_keccak,
        app_text_keccak,
        expected_chain_hashes,
    };
    bincode::serde::encode_to_vec(&encoded, bincode::config::standard())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_matches_golden_vector() {
        let bytes = encode_verification_key([0x11; 32], [0x22; 32], [[1; 8], [300; 8], [0; 8]])
            .expect("encode verification key");
        let expected = format!(
            "{}{}{}{}{}{}",
            "45564b4559303031", // magic
            "0264",             // version 2, security 100
            "11".repeat(32),
            "22".repeat(32),
            "01".repeat(8) + &"fb2c01".repeat(8),
            "00".repeat(8),
        );
        assert_eq!(to_hex(&bytes), expected);
    }

    #[test]
    fn key_round_trips() {
        let hashes = [[1, 2, 3, 4, 5, 6, 7, 8], [9; 8], [u32::MAX; 8]];
        let bytes = encode_verification_key([1; 32], [2; 32], hashes).expect("encode key");
        let (decoded, read): (EncodedVerificationKey, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .expect("decode key");
        assert_eq!(read, bytes.len());
        assert_eq!(decoded.magic, VERIFICATION_KEY_MAGIC);
        assert_eq!(decoded.version, VERIFICATION_KEY_FORMAT_VERSION);
        assert_eq!(decoded.security, VERIFICATION_KEY_SECURITY);
        assert_eq!(decoded.expected_chain_hashes, hashes);
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
