use airbender_host::{GpuProver, GpuProverConfig, Program, Proof, Prover as _};
use anyhow::Context as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::{
    observability,
    prover::{oracle::record_prover_input, proof_format::encode_proof, types::EthBlockInput},
    types::ProofSecurity,
};

/// A proof of one block with the figures reported to EthProofs.
pub struct ProvedBlock {
    pub proof: Proof,
    /// Cycles the guest program executed (the base layer).
    pub cycles: u64,
    /// Prover input recording + proving.
    pub proving_time_secs: f64,
}

#[derive(Debug)]
pub struct ProofResult {
    pub proof_bytes: Vec<u8>,
    pub cycles: u64,
    pub proving_time_secs: f64,
}

impl ProvedBlock {
    pub fn encode(self) -> anyhow::Result<ProofResult> {
        Ok(ProofResult {
            proof_bytes: encode_proof(self.proof)?,
            cycles: self.cycles,
            proving_time_secs: self.proving_time_secs,
        })
    }
}

/// Owns one GPU prover. A prover that failed is never reused: it is dropped and a new one
/// is built before the next block, outside that block's timing.
pub struct Prover {
    app_dir: PathBuf,
    worker_threads: Option<usize>,
    current: Option<GpuProver>,
}

impl std::fmt::Debug for Prover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prover").finish()
    }
}

impl Prover {
    pub fn new(
        app_dir: &Path,
        worker_threads: Option<usize>,
        security: ProofSecurity,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            security == ProofSecurity::Security100,
            "the v3 prover only supports 100-bit security, got {}-bit",
            security.proof_wire_value()
        );
        let current = create_gpu_prover(app_dir, worker_threads)?;
        Ok(Self {
            app_dir: app_dir.to_path_buf(),
            worker_threads,
            current: Some(current),
        })
    }

    /// Records the prover input of the block and proves it; `proving_time_secs` covers both.
    pub async fn prove(
        &mut self,
        block_number: u64,
        input: EthBlockInput,
    ) -> anyhow::Result<ProvedBlock> {
        if self.current.is_none() {
            self.rebuild(block_number).await?;
        }
        let prover = self.current.take().expect("a prover after rebuild");

        let start = Instant::now();
        // The heavy work runs on a blocking thread with the current hub bound, so a panic
        // still lands in Sentry with the block tag.
        let joined = observability::spawn_blocking_on_current_hub(move || {
            let result = record_prover_input(input)
                .with_context(|| {
                    format!("failed to record the prover input for block {block_number}")
                })
                .and_then(|words| {
                    prover
                        .prove(&words)
                        .with_context(|| format!("failed to prove block {block_number}"))
                });
            (prover, result)
        })
        .await;
        let proving_time_secs = start.elapsed().as_secs_f64();

        let result = match joined {
            Ok((prover, result)) => {
                if prover.is_poisoned() {
                    tracing::error!("The GPU prover failed on block {block_number}, replacing it");
                    observability::spawn_blocking_on_current_hub(move || drop(prover))
                        .await
                        .ok();
                    self.rebuild(block_number).await?;
                } else {
                    self.current = Some(prover);
                }
                result?
            }
            Err(err) => {
                let panic_msg = crate::utils::extract_panic_message(err);
                tracing::error!("Prover panicked for block {block_number}: {panic_msg}");
                self.rebuild(block_number).await?;
                anyhow::bail!(
                    "prover task panicked while processing block {block_number}: {panic_msg}"
                );
            }
        };
        anyhow::ensure!(
            result.receipt.output.iter().any(|word| *word != 0),
            "proof output for block {block_number} is all zeroes, the block execution failed inside the guest"
        );

        Ok(ProvedBlock {
            proof: result.proof,
            cycles: result.program_cycles,
            proving_time_secs,
        })
    }

    async fn rebuild(&mut self, block_number: u64) -> anyhow::Result<()> {
        let app_dir = self.app_dir.clone();
        let worker_threads = self.worker_threads;
        let replacement = observability::spawn_blocking_on_current_hub(move || {
            create_gpu_prover(&app_dir, worker_threads)
        })
        .await
        .map_err(|err| {
            anyhow::anyhow!(
                "rebuilding the prover panicked: {}",
                crate::utils::extract_panic_message(err)
            )
        })?
        .with_context(|| format!("failed to rebuild the prover after block {block_number}"))?;
        self.current = Some(replacement);
        Ok(())
    }
}

fn create_gpu_prover(app_dir: &Path, worker_threads: Option<usize>) -> anyhow::Result<GpuProver> {
    let program = Program::load(app_dir).with_context(|| {
        format!(
            "failed to load the guest program from {}",
            app_dir.display()
        )
    })?;
    let prover = program
        .gpu_prover()
        .with_config(GpuProverConfig::default().maybe_worker_threads(worker_threads))
        .build()
        .context("failed to build the GPU prover")?;
    Ok(prover)
}
