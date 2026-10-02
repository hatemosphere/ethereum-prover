use std::time::Duration;

use alloy::primitives::B256;
use anyhow::Context as _;
use smart_config::value::ExposeSecret as _;
use tokio::sync::mpsc;
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
    types::{CachePolicy, Mode, OnFailure},
};

/// What to process: the chain (tip or range) or one block.
pub enum Work {
    Chain(BlockRange),
    Block(u64),
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
    let (jobs_tx, jobs_rx) = mpsc::channel(config.prefetch.max(1));

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
    let token = config
        .ethproofs_token
        .as_ref()
        .context("ethproofs_token is required when EthProofs submission is enabled")?;
    let cluster_id = config
        .ethproofs_cluster_id
        .context("ethproofs_cluster_id is required when EthProofs submission is enabled")?;
    let base_url = match &config.ethproofs_url {
        Some(url) => url.clone(),
        None if config.ethproofs_submission.is_staging() => ETHPROOFS_STAGING_URL.to_string(),
        None => ETHPROOFS_PRODUCTION_URL.to_string(),
    };
    let client = EthproofsClient::new(
        base_url.parse().context("invalid EthProofs URL")?,
        token.expose_secret().to_string(),
        cluster_id,
        config.ethproofs_verifier_id.clone(),
    )?;
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
    jobs: mpsc::Sender<Job>,
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
        if jobs.send(job).await.is_err() {
            return Ok(());
        }
        send(&events, SubmissionEvent::Queued(block_number));
    }
}

async fn intake_block(
    block_number: u64,
    fetcher: Option<Fetcher>,
    cache: CacheStorage,
    jobs: mpsc::Sender<Job>,
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
    if jobs.send(job).await.is_ok() {
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
    async fn run(mut self, mut jobs: mpsc::Receiver<Job>) -> anyhow::Result<()> {
        while let Some(job) = jobs.recv().await {
            let block_number = job.block_number;
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
        tracing::info!("Proved block {block_number}: {cycles} cycles in {proving_time_ms} ms");

        let envelope = proved.encode()?.proof_bytes;
        let proof_path = self.archive.store(
            block_number,
            job.block_hash,
            cycles,
            proving_time_ms,
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
                    Ok(_) => tracing::info!("Receipt comparison for block {block_number} finished"),
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
