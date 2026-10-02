use airbender_host::{GpuProver, GpuProverConfig, Program, Prover as _};
use anyhow::Context as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::{
    observability,
    prover::{oracle::record_prover_input, proof_format::encode_proof, types::EthBlockInput},
    types::ProofSecurity,
};

#[derive(Debug)]
pub struct ProofResult {
    pub proof_bytes: Vec<u8>,
    pub cycles: u64,
    pub proving_time_secs: f64,
}

pub struct Prover {
    app_dir: PathBuf,
    worker_threads: Option<usize>,
    inner: Arc<Mutex<Option<GpuProver>>>,
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
        let inner = create_gpu_prover(app_dir, worker_threads).with_context(|| {
            format!(
                "failed to create the GPU prover for the program in {}",
                app_dir.display()
            )
        })?;
        Ok(Self {
            app_dir: app_dir.to_path_buf(),
            worker_threads,
            inner: Arc::new(Mutex::new(Some(inner))),
        })
    }

    /// Records the prover input of the block and proves it; `proving_time_secs` covers both.
    pub async fn prove(
        &mut self,
        block_number: u64,
        input: EthBlockInput,
    ) -> anyhow::Result<ProofResult> {
        let start = Instant::now();

        let inner = self.inner.clone();

        // We execute the heavy work on a blocking thread, but keep the
        // current hub bound so a panic still lands in Sentry with the block tag.
        let future_result = observability::spawn_blocking_on_current_hub(move || {
            let words = record_prover_input(input).with_context(|| {
                format!("failed to record the prover input for block {block_number}")
            })?;
            let prover = inner.lock().map_err(|_| {
                anyhow::anyhow!("prover mutex is poisoned while processing block {block_number}")
            })?;
            let prover = prover.as_ref().ok_or_else(|| {
                anyhow::anyhow!("prover is not available while processing block {block_number}")
            })?;
            let result = prover
                .prove(&words)
                .with_context(|| format!("failed to prove block {block_number}"));
            Ok((result, prover.is_poisoned()))
        })
        .await;
        let result = match future_result {
            Ok(Ok((result, poisoned))) => {
                if poisoned {
                    self.replace_prover(block_number)?;
                }
                result?
            }
            Ok(Err(err)) => return Err(err),
            Err(err) => {
                let panic_msg = crate::utils::extract_panic_message(err);
                tracing::error!("Prover panicked for block {}: {}", block_number, panic_msg);
                self.replace_prover(block_number)?;
                return Err(anyhow::anyhow!(
                    "prover task panicked while processing block {block_number}: {panic_msg}"
                ));
            }
        };
        anyhow::ensure!(
            result.receipt.output.iter().any(|word| *word != 0),
            "proof output for block {block_number} is all zeroes, the block execution failed inside the guest"
        );

        let proving_time_secs = start.elapsed().as_secs_f64();
        let cycles = result.program_cycles;
        let proof_bytes = encode_proof(result.proof)
            .with_context(|| format!("failed to encode proof bytes for block {block_number}"))?;
        Ok(ProofResult {
            proof_bytes,
            cycles,
            proving_time_secs,
        })
    }

    /// A failed or panicked prover is not safe to reuse, since some of its threads may be
    /// poisoned or dead, so it is replaced by a new instance.
    fn replace_prover(&mut self, block_number: u64) -> anyhow::Result<()> {
        let strong_count = Arc::strong_count(&self.inner);
        anyhow::ensure!(
            strong_count == 1,
            "failed to recover prover after block {block_number}: expected exactly one strong reference, found {}",
            strong_count
        );

        let mut inner = self.inner.lock().map_err(|_| {
            anyhow::anyhow!("prover mutex is poisoned while recovering after block {block_number}")
        })?;
        tracing::info!("Dropping the existing (poisoned) prover instance");
        drop(inner.take());

        tracing::info!("Re-creating a new prover instance to replace the poisoned one");
        let replacement = create_gpu_prover(self.app_dir.as_path(), self.worker_threads)
            .with_context(|| {
                format!("failed to re-instantiate prover after block {block_number}")
            })?;
        *inner = Some(replacement);
        Ok(())
    }
}

fn create_gpu_prover(app_dir: &Path, worker_threads: Option<usize>) -> anyhow::Result<GpuProver> {
    let program = Program::load(app_dir)?;
    let prover = program
        .gpu_prover()
        .with_config(GpuProverConfig::default().maybe_worker_threads(worker_threads))
        .build()?;
    Ok(prover)
}
