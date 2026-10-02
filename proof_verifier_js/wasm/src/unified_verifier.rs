use full_statement_verifier::{
    unified_circuit_statement::verify_unified_circuit_recursion_layer_sec_100,
    verifier_common::{errors::DebugErrorCreator, USE_REDUCED_BLAKE2_ROUNDS},
};

pub(crate) fn verify(
    words: &[u32],
    expected_chain_hashes: &[[u32; 8]; 3],
    expected_output: Option<&[u32]>,
) -> Result<[u32; 8], String> {
    if expected_output.is_some_and(|output| output.len() != 8) {
        return Err("expected public output must contain exactly 8 words".to_string());
    }
    let mut stream = words.iter().copied();
    let output = verify_unified_circuit_recursion_layer_sec_100::<
        _,
        DebugErrorCreator,
        { USE_REDUCED_BLAKE2_ROUNDS },
    >(&mut stream)
    .map_err(|err| format!("proof verification failed: {err:?}"))?;
    if stream.len() != 0 {
        return Err("trailing words after proof".to_string());
    }
    if !expected_chain_hashes
        .iter()
        .any(|hash| hash.as_slice() == &output[8..16])
    {
        return Err("verified recursion chain does not match the verification key".to_string());
    }
    let public_output: [u32; 8] = output[..8].try_into().unwrap();
    if expected_output.is_some_and(|expected| expected != public_output) {
        return Err("verified public output does not match expected output".to_string());
    }
    Ok(public_output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_v3_fixtures() {
        let Some(root) = std::env::var_os("V3_VERIFIER_FIXTURES") else {
            eprintln!(
                "skipping real_v3_fixtures: set V3_VERIFIER_FIXTURES to the fixture directory"
            );
            return;
        };
        let root = std::path::PathBuf::from(root);
        let key = std::fs::read(root.join("vk/recursion_unified_v3_security_100.vk.bin")).unwrap();
        let hashes = crate::verification_key_format::decode_verification_key(&key).unwrap();
        for block in ["26078427", "26078715"] {
            let bytes =
                std::fs::read(root.join(format!("blocks/{block}/proof_v2.bin.gz"))).unwrap();
            let mut words = crate::proof_format::decode_proof_bytes(&bytes).unwrap();
            let output = verify(&words, &hashes, None).unwrap();
            assert_eq!(verify(&words, &hashes, Some(&output)).unwrap(), output);
            let mut wrong_hashes = hashes;
            for hash in &mut wrong_hashes {
                hash[0] ^= 1;
            }
            assert!(verify(&words, &wrong_hashes, None)
                .unwrap_err()
                .contains("chain"));
            let mut wrong_output = output;
            wrong_output[0] ^= 1;
            assert!(verify(&words, &hashes, Some(&wrong_output))
                .unwrap_err()
                .contains("public output"));
            words.push(0);
            assert!(verify(&words, &hashes, None)
                .unwrap_err()
                .contains("trailing words"));
        }
    }
}
