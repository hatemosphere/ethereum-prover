use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use alloy::primitives::B256;
use anyhow::Context as _;
use smart_config::value::ExposeSecret as _;
use tokio::sync::{Notify, mpsc};
use url::Url;

use crate::{
    block_stream::{BlockRange, BlockSelector, BlockStream, HeadWatcher, Heads},
    cache::CacheStorage,
    clients::ethproofs::{ETHPROOFS_PRODUCTION_URL, ETHPROOFS_STAGING_URL, EthproofsClient},
    config::EthProverConfig,
    fetcher::{Fetcher, RetryPolicy},
    metrics::{InflightGuard, METRICS},
    observability,
    proof_output::ProofArchive,
    prover::{
        cpu_witness::{CpuWitnessGenerator, DebuggerTxCallback},
        gpu_prover::Prover,
        types::EthBlockInput,
    },
    submission::{Outbox, OutboxRecord, SubmissionEvent, SubmissionWorker},
    types::{CachePolicy, EthProofsSubmission, Mode, OnFailure},
};

/// What to process: the chain (tip or range) or one block.
pub enum Work {
    Chain(BlockRange),
    Block(u64),
}

/// Intake-to-worker hand-off. `Fifo` keeps every block in order and makes the intake wait
/// when it is full; `Latest` (tip mode) keeps only the newest block, replacing a queued
/// older one, and reports `queued` when proving actually starts.
enum JobSender {
    Fifo(mpsc::Sender<Job>),
    Latest(Arc<LatestSlot>),
}

enum JobReceiver {
    Fifo(mpsc::Receiver<Job>),
    Latest(Arc<LatestSlot>),
}

fn job_queue(range: Option<BlockRange>, prefetch: usize) -> (JobSender, JobReceiver) {
    if matches!(range, Some(BlockRange::Tip)) {
        let slot = Arc::new(LatestSlot::default());
        (JobSender::Latest(slot.clone()), JobReceiver::Latest(slot))
    } else {
        let (tx, rx) = mpsc::channel(prefetch.max(1));
        (JobSender::Fifo(tx), JobReceiver::Fifo(rx))
    }
}

impl JobSender {
    /// Hands over a job; false once the worker is gone.
    async fn send(&self, job: Job) -> bool {
        match self {
            Self::Fifo(tx) => tx.send(job).await.is_ok(),
            Self::Latest(slot) => {
                let block_number = job.block_number;
                if let Some(superseded) = slot.put(job) {
                    tracing::info!("Block {block_number} supersedes queued block {superseded}");
                }
                true
            }
        }
    }

    fn reports_queued(&self) -> bool {
        matches!(self, Self::Fifo(_))
    }
}

impl Drop for JobSender {
    fn drop(&mut self) {
        if let Self::Latest(slot) = self {
            slot.close();
        }
    }
}

impl JobReceiver {
    async fn next(&mut self) -> Option<Job> {
        match self {
            Self::Fifo(rx) => rx.recv().await,
            Self::Latest(slot) => slot.take().await,
        }
    }
}

#[derive(Default)]
struct LatestSlot {
    job: Mutex<Option<Job>>,
    closed: AtomicBool,
    notify: Notify,
}

impl LatestSlot {
    /// Stores the job and returns the block number of the job it replaced.
    fn put(&self, job: Job) -> Option<u64> {
        let replaced = self.job.lock().unwrap().replace(job);
        self.notify.notify_one();
        replaced.map(|job| job.block_number)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    /// The queued job, waiting for one; `None` once closed and empty.
    async fn take(&self) -> Option<Job> {
        loop {
            if let Some(job) = self.job.lock().unwrap().take() {
                return Some(job);
            }
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            self.notify.notified().await;
        }
    }
}

struct Job {
    block_number: u64,
    block_hash: B256,
    input: EthBlockInput,
}

/// The prover service: one intake task (block selection, fetching, caching) feeding a bounded
/// queue, one proving worker that owns the GPU prover, and one submission worker that owns
/// all EthProofs traffic and the durable outbox.
pub async fn run(config: EthProverConfig, work: Work) -> anyhow::Result<()> {
    let cache = CacheStorage::new(config.data_dir.join("cache"))?;
    let archive = ProofArchive::new(config.data_dir.join("proofs"));
    let rpc_url = config
        .rpc_url
        .as_ref()
        .map(|url| {
            url.expose_secret()
                .parse::<Url>()
                .context("invalid rpc_url")
        })
        .transpose()?;
    let fetcher = rpc_url
        .clone()
        .map(|url| {
            Fetcher::new(
                url,
                config.witness_format,
                RetryPolicy {
                    attempts: config.rpc_attempts,
                    ..Default::default()
                },
            )
        })
        .transpose()?;

    let (events, submission) = start_submission(&config)?;
    let (jobs_tx, jobs_rx) = job_queue(
        match &work {
            Work::Chain(range) => Some(*range),
            Work::Block(_) => None,
        },
        config.prefetch,
    );

    let intake = match work {
        Work::Chain(range) => {
            let fetcher = fetcher
                .clone()
                .context("rpc_url is required to follow the chain")?;
            let ws_url = config
                .ws_url
                .as_ref()
                .map(|url| url.expose_secret().parse::<Url>().context("invalid ws_url"))
                .transpose()?;
            let heads = HeadWatcher::new(
                fetcher.clone(),
                Duration::from_millis(config.poll_interval_ms),
                ws_url,
            );
            let stream = BlockStream::new(
                BlockSelector::new(config.block_mod, config.prover_id)?,
                range,
                heads,
            );
            tokio::spawn(observability::bind_task(
                "intake",
                intake_chain(
                    stream,
                    fetcher,
                    cache.clone(),
                    config.cache_policy,
                    jobs_tx,
                    events.clone(),
                ),
            ))
        }
        Work::Block(block_number) => tokio::spawn(observability::bind_task(
            "intake",
            intake_block(
                block_number,
                fetcher.clone(),
                cache.clone(),
                jobs_tx,
                events.clone(),
            ),
        )),
    };

    let worker = Worker {
        mode: config.mode,
        on_failure: config.on_failure,
        cache_policy: config.cache_policy,
        cache,
        archive,
        outbox: events
            .as_ref()
            .map(|_| Outbox::new(&config.data_dir.join("outbox")))
            .transpose()?,
        events,
        fetcher,
        prover: match config.mode {
            Mode::GpuProve => Some(create_prover(&config).await?),
            Mode::CpuWitness => None,
        },
    };
    let worked = worker.run(jobs_rx).await;
    intake.abort();
    if let Some(submission) = submission {
        tracing::info!("Waiting for pending EthProofs submissions");
        submission
            .await
            .context("the submission worker panicked")??;
    }
    worked
}

fn start_submission(
    config: &EthProverConfig,
) -> anyhow::Result<(
    Option<mpsc::UnboundedSender<SubmissionEvent>>,
    Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
)> {
    if !config.ethproofs_submission.enabled() {
        return Ok((None, None));
    }
    let dry_run = matches!(config.ethproofs_submission, EthProofsSubmission::DryRun);
    let token = match (&config.ethproofs_token, dry_run) {
        (Some(token), _) => token.expose_secret().to_string(),
        (None, true) => String::new(),
        (None, false) => {
            anyhow::bail!("ethproofs_token is required when EthProofs submission is enabled")
        }
    };
    let cluster_id = match (config.ethproofs_cluster_id, dry_run) {
        (Some(cluster_id), _) => cluster_id,
        (None, true) => 0,
        (None, false) => {
            anyhow::bail!("ethproofs_cluster_id is required when EthProofs submission is enabled")
        }
    };
    let base_url = match &config.ethproofs_url {
        Some(url) => url.clone(),
        None if config.ethproofs_submission.is_staging() => ETHPROOFS_STAGING_URL.to_string(),
        None => ETHPROOFS_PRODUCTION_URL.to_string(),
    };
    let client = EthproofsClient::new(
        base_url.parse().context("invalid EthProofs URL")?,
        token,
        cluster_id,
        config.ethproofs_verifier_id.clone(),
    )?;
    let client = if dry_run {
        client.dry_run(config.data_dir.join("dry-run"))?
    } else {
        client
    };
    let outbox = Outbox::new(&config.data_dir.join("outbox"))?;
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(observability::bind_task(
        "submission",
        SubmissionWorker::new(client, outbox, rx).run(),
    ));
    Ok((Some(tx), Some(handle)))
}

async fn create_prover(config: &EthProverConfig) -> anyhow::Result<Prover> {
    tracing::info!("Creating the GPU prover");
    let app_dir = config.app_dir.clone();
    let security = config.security;
    let prover = observability::spawn_blocking_on_current_hub(move || {
        Prover::new(app_dir.as_path(), None, security)
    })
    .await
    .context("prover creation panicked")??;
    tracing::info!("GPU prover created");
    Ok(prover)
}

async fn intake_chain<H: Heads>(
    mut stream: BlockStream<H>,
    fetcher: Fetcher,
    cache: CacheStorage,
    cache_policy: CachePolicy,
    jobs: JobSender,
    events: Option<mpsc::UnboundedSender<SubmissionEvent>>,
) -> anyhow::Result<()> {
    loop {
        let block_number = tokio::select! {
            block = stream.next_block() => match block? {
                Some(block) => block,
                None => return Ok(()),
            },
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Interrupted; finishing the queued blocks");
                return Ok(());
            }
        };
        let (block, witness) = match fetcher.block_with_witness(block_number).await {
            Ok(fetched) => fetched,
            Err(err) => {
                observability::capture_anyhow(&err);
                tracing::error!("Skipping block {block_number}: {err:#}");
                continue;
            }
        };
        if !matches!(cache_policy, CachePolicy::Off)
            && let Err(err) = cache.cache_block(block_number, &block, &witness)
        {
            tracing::warn!("Failed to cache block {block_number}: {err:#}");
        }
        METRICS.blocks_received_total.inc();
        let job = Job {
            block_number,
            block_hash: block.header.hash,
            input: EthBlockInput::new(block, witness),
        };
        if !jobs.send(job).await {
            return Ok(());
        }
        if jobs.reports_queued() {
            send(&events, SubmissionEvent::Queued(block_number));
        }
    }
}

async fn intake_block(
    block_number: u64,
    fetcher: Option<Fetcher>,
    cache: CacheStorage,
    jobs: JobSender,
    events: Option<mpsc::UnboundedSender<SubmissionEvent>>,
) -> anyhow::Result<()> {
    let (block, witness) = match cache.load_block(block_number)? {
        Some(cached) => {
            tracing::info!("Loaded block {block_number} from the cache");
            cached
        }
        None => {
            let fetched = fetcher
                .context("block is not cached and rpc_url is not set")?
                .block_with_witness(block_number)
                .await?;
            cache.cache_block(block_number, &fetched.0, &fetched.1)?;
            fetched
        }
    };
    let job = Job {
        block_number,
        block_hash: block.header.hash,
        input: EthBlockInput::new(block, witness),
    };
    if jobs.send(job).await {
        send(&events, SubmissionEvent::Queued(block_number));
    }
    Ok(())
}

fn send(events: &Option<mpsc::UnboundedSender<SubmissionEvent>>, event: SubmissionEvent) {
    if let Some(events) = events {
        let _ = events.send(event);
    }
}

struct Worker {
    mode: Mode,
    on_failure: OnFailure,
    cache_policy: CachePolicy,
    cache: CacheStorage,
    archive: ProofArchive,
    outbox: Option<Outbox>,
    events: Option<mpsc::UnboundedSender<SubmissionEvent>>,
    fetcher: Option<Fetcher>,
    prover: Option<Prover>,
}

impl Worker {
    async fn run(mut self, mut jobs: JobReceiver) -> anyhow::Result<()> {
        let queued_on_take = matches!(jobs, JobReceiver::Latest(_));
        while let Some(job) = jobs.next().await {
            let block_number = job.block_number;
            if queued_on_take {
                send(&self.events, SubmissionEvent::Queued(block_number));
            }
            let result = observability::bind_block(self.mode_name(), block_number, async {
                match self.mode {
                    Mode::GpuProve => self.prove(job).await,
                    Mode::CpuWitness => self.execute(job).await,
                }
            })
            .await;
            match result {
                Ok(()) => {
                    METRICS.last_processed_block.set(block_number);
                    if matches!(self.cache_policy, CachePolicy::OnFailure)
                        && let Err(err) = self.cache.remove_cached_block(block_number)
                    {
                        tracing::warn!("Failed to remove cached block {block_number}: {err:#}");
                    }
                }
                Err(err) => {
                    observability::capture_anyhow(&err);
                    match self.on_failure {
                        OnFailure::Exit => {
                            return Err(err).context(format!("block {block_number} failed"));
                        }
                        OnFailure::Continue => {
                            tracing::error!("Block {block_number} failed: {err:#}");
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn mode_name(&self) -> &'static str {
        match self.mode {
            Mode::GpuProve => "gpu_prove",
            Mode::CpuWitness => "cpu_witness",
        }
    }

    async fn prove(&mut self, job: Job) -> anyhow::Result<()> {
        let block_number = job.block_number;
        send(&self.events, SubmissionEvent::Proving(block_number));
        let _inflight = InflightGuard::new(&METRICS.inflight_proof_tasks);
        let latency = METRICS.proof_duration.start();
        let prover = self
            .prover
            .as_mut()
            .expect("a GPU prover in gpu_prove mode");
        let proved = prover.prove(block_number, job.input).await;
        latency.observe();
        let proved = match proved {
            Ok(proved) => proved,
            Err(err) => {
                METRICS.proof_failure_total.inc();
                return Err(err);
            }
        };
        METRICS.proof_success_total.inc();
        let cycles = proved.cycles;
        let proving_time_ms = (proved.proving_time_secs * 1000.0) as u64;
        let prover_input_ms = (proved.prover_input_secs * 1000.0) as u64;
        METRICS
            .proving_time
            .observe(Duration::from_secs_f64(proved.proving_time_secs));
        METRICS
            .prover_input_duration
            .observe(Duration::from_secs_f64(proved.prover_input_secs));
        tracing::info!(
            "Proved block {block_number}: {cycles} cycles in {proving_time_ms} ms \
             ({prover_input_ms} ms prover input)"
        );

        let envelope = proved.encode()?.proof_bytes;
        let proof_path = self.archive.store(
            block_number,
            job.block_hash,
            cycles,
            proving_time_ms,
            prover_input_ms,
            &envelope,
        )?;
        if let Some(outbox) = &self.outbox {
            outbox.put(&OutboxRecord {
                block_number,
                proving_time_ms,
                cycles,
                proof_path,
                attempts: 0,
                last_error: None,
            })?;
            send(&self.events, SubmissionEvent::Proved(block_number));
        }
        Ok(())
    }

    async fn execute(&mut self, job: Job) -> anyhow::Result<()> {
        let block_number = job.block_number;
        let generator = CpuWitnessGenerator::new();
        if let Err(err) = generator.forward_run(block_number, job.input.clone()).await {
            if let Some(fetcher) = &self.fetcher {
                let debugger = DebuggerTxCallback::new(
                    block_number,
                    job.input.transactions.clone(),
                    fetcher.provider().clone(),
                    self.cache.clone(),
                );
                match generator.debug(block_number, job.input, debugger).await {
                    Ok(debugger) => {
                        for problem in debugger.get_problems() {
                            tracing::error!("Block {block_number}: {problem}");
                        }
                        tracing::info!(
                            "Receipt comparison for block {block_number}: {} mismatches",
                            debugger.get_problems().len()
                        );
                    }
                    Err(debug_err) => {
                        tracing::warn!("Debugging block {block_number} failed: {debug_err:#}")
                    }
                }
            }
            return Err(err);
        }
        let words = generator.generate_witness(block_number, job.input).await?;
        tracing::info!("Block {block_number}: {} prover input words", words.len());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(block_number: u64) -> Job {
        let block: alloy::rpc::types::Block = serde_json::from_value(serde_json::json!({
            "hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "parentHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "sha3Uncles": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "miner": "0x0000000000000000000000000000000000000000",
            "stateRoot": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "transactionsRoot": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "receiptsRoot": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "logsBloom": format!("0x{}", "00".repeat(256)),
            "difficulty": "0x0",
            "number": format!("{block_number:#x}"),
            "gasLimit": "0x0",
            "gasUsed": "0x0",
            "timestamp": "0x0",
            "extraData": "0x",
            "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "nonce": "0x0000000000000000",
            "uncles": [],
            "transactions": [],
        }))
        .unwrap();
        Job {
            block_number,
            block_hash: B256::ZERO,
            input: EthBlockInput::new(block, Default::default()),
        }
    }

    #[tokio::test]
    async fn latest_slot_keeps_only_the_newest_job() {
        let slot = Arc::new(LatestSlot::default());
        assert_eq!(slot.put(job(1)), None);
        assert_eq!(slot.put(job(2)), Some(1));
        assert_eq!(slot.take().await.map(|job| job.block_number), Some(2));

        let waiter = tokio::spawn({
            let slot = slot.clone();
            async move { slot.take().await.map(|job| job.block_number) }
        });
        tokio::task::yield_now().await;
        slot.put(job(3));
        assert_eq!(waiter.await.unwrap(), Some(3));

        slot.put(job(4));
        slot.close();
        assert_eq!(slot.take().await.map(|job| job.block_number), Some(4));
        assert!(slot.take().await.is_none());
    }
}
