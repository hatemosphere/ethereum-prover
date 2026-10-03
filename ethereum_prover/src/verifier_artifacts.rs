use std::path::Path;

use airbender_host::{Program, ProverLevel, SecurityLevel, compute_real_vk};
use anyhow::Context as _;
use prover_pipeline::{ProgramSource, unified_verification_chain_hashes};

use crate::{types::ProofSecurity, verification_key_format::encode_verification_key};

const VERIFICATION_KEY_FILE: &str = "recursion_unified_v3_security_100.vk.bin";

/// Writes the v2 verification key of the guest program in `app_dir`. The recursion-chain
/// hashes are recomputed from the trusted guest and verifier binaries (the producer's
/// `FSV_DIR` and Blake-mode environment), never taken from a proof.
pub fn generate_verifier_artifacts(
    output_dir: &Path,
    app_dir: &Path,
    security: Option<ProofSecurity>,
) -> anyhow::Result<()> {
    if let Some(security) = security {
        anyhow::ensure!(
            security == ProofSecurity::Security100,
            "v3 verification keys only exist for 100-bit security, got {}-bit",
            security.proof_wire_value()
        );
    }

    let program = Program::load(app_dir).with_context(|| {
        format!(
            "failed to load the guest program from {}",
            app_dir.display()
        )
    })?;
    let vk = compute_real_vk(
        program.app_bin(),
        ProverLevel::RecursionUnified,
        SecurityLevel::Bits100,
    )?;
    let source = ProgramSource::from_paths(
        path_string(program.app_bin())?,
        Some(path_string(program.app_text())?),
    );
    let expected_chain_hashes = unified_verification_chain_hashes(&source)
        .map_err(|err| anyhow::anyhow!("failed to compute the trusted chain hashes: {err}"))?;

    std::fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "failed to create verifier artifact output directory {}",
            output_dir.display()
        )
    })?;
    let verification_key =
        encode_verification_key(vk.app_bin_hash, vk.app_text_hash, expected_chain_hashes)
            .context("failed to encode the verification key")?;
    let path = output_dir.join(VERIFICATION_KEY_FILE);
    std::fs::write(&path, verification_key)
        .with_context(|| format!("failed to write the verification key {}", path.display()))?;

    tracing::info!("Generated the v3 verification key {}", path.display());
    Ok(())
}

fn path_string(path: &Path) -> anyhow::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("path {} is not valid UTF-8", path.display()))
}
