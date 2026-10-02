#![feature(allocator_api)]

use anyhow::Context as _;

use crate::{
    config::{Cli, Command, EthProverConfig},
    prover::gpu_prover::Prover,
};

pub mod block_stream;
pub mod config;
pub mod fetcher;
pub mod service;
pub mod submission;

pub(crate) mod cache;
pub(crate) mod clients;
pub mod metrics;
pub(crate) mod observability;
pub(crate) mod proof_output;
pub mod prover;
pub mod types;
pub(crate) mod utils;
pub mod verification;
pub(crate) mod verification_key_format;
pub mod verifier_artifacts;

#[derive(Debug, Default)]
pub struct Runner {}

impl Runner {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run(self, cli: Cli, config: EthProverConfig) -> anyhow::Result<()> {
        match cli.command {
            Command::GenerateVerifierArtifacts {
                output_dir,
                app_dir,
                security,
            } => verifier_artifacts::generate_verifier_artifacts(&output_dir, &app_dir, security),
            Command::Prove {
                input_dir,
                output,
                artifact_json,
            } => prove_one(&config, &input_dir, &output, artifact_json.as_deref()).await,
            Command::Verify { proof, key } => {
                let output = verification::verify_proof_file(&proof, &key)?;
                println!("verified; public output {output:08x?}");
                Ok(())
            }
            Command::Run { start: None, .. } => {
                service::run(config, service::Work::Chain(block_stream::BlockRange::Tip)).await
            }
            Command::Run {
                start: Some(start),
                end,
            } => {
                service::run(
                    config,
                    service::Work::Chain(block_stream::BlockRange::Range { start, end }),
                )
                .await
            }
            Command::Block { block_number } => {
                service::run(config, service::Work::Block(block_number)).await
            }
        }
    }
}

async fn prove_one(
    config: &EthProverConfig,
    input_dir: &std::path::Path,
    output: &std::path::Path,
    artifact_json: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let input = prover::types::EthBlockInput::from_dir(input_dir)?;
    let block_number = input.block_header.number;
    let app_dir = config.app_dir.clone();
    let security = config.security;
    let mut gpu_prover = observability::spawn_blocking_on_current_hub(move || {
        Prover::new(app_dir.as_path(), None, security)
    })
    .await
    .context("prover initialization task panicked")??;
    let proved = gpu_prover.prove(block_number, input).await?;
    tracing::info!(
        "Proved block {block_number}: {} cycles in {:.3} s",
        proved.cycles,
        proved.proving_time_secs
    );
    let artifact = prover::proof_format::final_artifact(proved.proof)?;
    if let Some(path) = artifact_json {
        let json = serde_json::to_vec(&artifact).context("failed to serialize the artifact")?;
        std::fs::write(path, json)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    let bytes = proof_output::gzip_proof_bytes(&prover::proof_format::encode_artifact(&artifact)?)?;
    std::fs::write(output, &bytes)
        .with_context(|| format!("failed to write {}", output.display()))?;
    tracing::info!("Wrote {} ({} bytes)", output.display(), bytes.len());
    Ok(())
}
