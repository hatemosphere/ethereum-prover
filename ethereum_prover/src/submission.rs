use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::clients::ethproofs::{EthproofsClient, SubmitError};

const RETRY_SCAN_INTERVAL: Duration = Duration::from_secs(15);
const MIN_RETRY_BACKOFF: Duration = Duration::from_secs(15);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// A proof waiting to be accepted by EthProofs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRecord {
    pub block_number: u64,
    pub proving_time_ms: u64,
    pub cycles: u64,
    /// Archived gzip proof (`ProofArchive`).
    pub proof_path: PathBuf,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// Durable submission queue: `pending/<block>.json` until EthProofs accepts the proof,
/// `quarantine/<block>.json` after a permanent rejection. Records are written before the
/// first attempt, so a restart resubmits everything that was not accepted. A crash between
/// acceptance and removal resubmits an accepted proof once more.
#[derive(Debug, Clone)]
pub struct Outbox {
    pending: PathBuf,
    quarantine: PathBuf,
}

impl Outbox {
    pub fn new(dir: &Path) -> anyhow::Result<Self> {
        let outbox = Self {
            pending: dir.join("pending"),
            quarantine: dir.join("quarantine"),
        };
        for dir in [&outbox.pending, &outbox.quarantine] {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }
        Ok(outbox)
    }

    pub fn put(&self, record: &OutboxRecord) -> anyhow::Result<()> {
        let path = self.pending_path(record.block_number);
        crate::utils::write_atomic(&path, &serde_json::to_vec_pretty(record)?)
            .with_context(|| format!("failed to write {}", path.display()))
    }

    /// Pending records in block order. Unreadable records are moved to quarantine.
    pub fn pending(&self) -> anyhow::Result<Vec<OutboxRecord>> {
        let mut records = Vec::new();
        for entry in std::fs::read_dir(&self.pending)? {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            match std::fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| Ok(serde_json::from_slice::<OutboxRecord>(&bytes)?))
            {
                Ok(record) => records.push(record),
                Err(err) => {
                    tracing::error!(
                        "Quarantining unreadable outbox record {}: {err:#}",
                        path.display()
                    );
                    let target = self.quarantine.join(path.file_name().unwrap_or_default());
                    std::fs::rename(&path, target)?;
                }
            }
        }
        records.sort_by_key(|record| record.block_number);
        Ok(records)
    }

    pub fn remove(&self, block_number: u64) -> anyhow::Result<()> {
        match std::fs::remove_file(self.pending_path(block_number)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
            _ => Ok(()),
        }
    }

    pub fn quarantine(&self, record: &OutboxRecord) -> anyhow::Result<()> {
        let path = self
            .quarantine
            .join(format!("{}.json", record.block_number));
        crate::utils::write_atomic(&path, &serde_json::to_vec_pretty(record)?)?;
        self.remove(record.block_number)
    }

    fn pending_path(&self, block_number: u64) -> PathBuf {
        self.pending.join(format!("{block_number}.json"))
    }
}

#[derive(Debug)]
pub enum SubmissionEvent {
    Queued(u64),
    Proving(u64),
    /// The block's record is in the outbox.
    Proved(u64),
}

/// Sends status updates in event order and submits outbox records until each is accepted or
/// permanently rejected. It never blocks the proving worker: events are queued, and
/// submissions that fail transiently are retried from the outbox with backoff.
pub struct SubmissionWorker {
    client: EthproofsClient,
    outbox: Outbox,
    events: mpsc::UnboundedReceiver<SubmissionEvent>,
    next_attempt: HashMap<u64, Instant>,
}

impl SubmissionWorker {
    pub fn new(
        client: EthproofsClient,
        outbox: Outbox,
        events: mpsc::UnboundedReceiver<SubmissionEvent>,
    ) -> Self {
        Self {
            client,
            outbox,
            events,
            next_attempt: HashMap::new(),
        }
    }

    /// Runs until the event channel closes, then makes one last pass over the outbox.
    pub async fn run(mut self) -> anyhow::Result<()> {
        self.submit_due().await?;
        let mut scan = tokio::time::interval(RETRY_SCAN_INTERVAL);
        loop {
            tokio::select! {
                event = self.events.recv() => match event {
                    Some(SubmissionEvent::Queued(block_number)) => {
                        status_update(block_number, "queued", self.client.queued(block_number).await);
                    }
                    Some(SubmissionEvent::Proving(block_number)) => {
                        status_update(block_number, "proving", self.client.proving(block_number).await);
                    }
                    Some(SubmissionEvent::Proved(block_number)) => {
                        self.next_attempt.remove(&block_number);
                        self.submit_due().await?;
                    }
                    None => break,
                },
                _ = scan.tick() => self.submit_due().await?,
            }
        }
        self.submit_due().await
    }

    async fn submit_due(&mut self) -> anyhow::Result<()> {
        let now = Instant::now();
        for mut record in self.outbox.pending()? {
            if self
                .next_attempt
                .get(&record.block_number)
                .is_some_and(|at| *at > now)
            {
                continue;
            }
            let outcome = match std::fs::read(&record.proof_path) {
                Ok(proof) => {
                    self.client
                        .proved(
                            record.block_number,
                            record.proving_time_ms,
                            record.cycles,
                            &proof,
                        )
                        .await
                }
                Err(err) => Err(SubmitError::Permanent {
                    reason: format!("cannot read {}: {err}", record.proof_path.display()),
                }),
            };
            record.attempts += 1;
            match outcome {
                Ok(()) => {
                    tracing::info!(
                        "EthProofs accepted the proof of block {} (attempt {})",
                        record.block_number,
                        record.attempts
                    );
                    self.next_attempt.remove(&record.block_number);
                    self.outbox.remove(record.block_number)?;
                }
                Err(SubmitError::Retryable {
                    reason,
                    retry_after,
                }) => {
                    let backoff = retry_after
                        .unwrap_or_default()
                        .max(MIN_RETRY_BACKOFF.saturating_mul(1 << record.attempts.min(10)))
                        .min(MAX_RETRY_BACKOFF);
                    tracing::warn!(
                        "Submitting block {} failed ({reason}), retrying in {backoff:?}",
                        record.block_number
                    );
                    record.last_error = Some(reason);
                    self.next_attempt.insert(record.block_number, now + backoff);
                    self.outbox.put(&record)?;
                }
                Err(SubmitError::Permanent { reason }) => {
                    tracing::error!(
                        "EthProofs rejected the proof of block {}: {reason}; quarantined",
                        record.block_number
                    );
                    record.last_error = Some(reason);
                    self.next_attempt.remove(&record.block_number);
                    self.outbox.quarantine(&record)?;
                }
            }
        }
        Ok(())
    }
}

fn status_update(block_number: u64, status: &str, result: Result<(), SubmitError>) {
    if let Err(err) = result {
        tracing::warn!("EthProofs '{status}' update for block {block_number} failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn record(dir: &Path, block_number: u64) -> OutboxRecord {
        let proof_path = dir.join(format!("{block_number}.bin.gz"));
        std::fs::write(&proof_path, b"gzip-proof").unwrap();
        OutboxRecord {
            block_number,
            proving_time_ms: 1000,
            cycles: 42,
            proof_path,
            attempts: 0,
            last_error: None,
        }
    }

    async fn client(server: &MockServer) -> EthproofsClient {
        let url = url::Url::parse(&format!("{}/api/v0/", server.uri())).unwrap();
        EthproofsClient::new(url, "token".into(), 7, "verifier".into()).unwrap()
    }

    async fn run_once(client: EthproofsClient, outbox: Outbox) {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(tx);
        SubmissionWorker::new(client, outbox, rx)
            .run()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn accepted_records_leave_the_outbox() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v0/proofs/proved"))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(dir.path()).unwrap();
        outbox.put(&record(dir.path(), 11)).unwrap();
        outbox.put(&record(dir.path(), 10)).unwrap();

        run_once(client(&server).await, outbox.clone()).await;
        assert!(outbox.pending().unwrap().is_empty());
    }

    #[tokio::test]
    async fn transient_failures_stay_pending_and_permanent_ones_are_quarantined() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v0/proofs/proved"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"block_number": 20}),
            ))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v0/proofs/proved"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"block_number": 21}),
            ))
            .respond_with(ResponseTemplate::new(422).set_body_string("bad proof"))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(dir.path()).unwrap();
        outbox.put(&record(dir.path(), 20)).unwrap();
        outbox.put(&record(dir.path(), 21)).unwrap();

        run_once(client(&server).await, outbox.clone()).await;
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].block_number, 20);
        assert_eq!(pending[0].attempts, 1);
        assert!(pending[0].last_error.as_deref().unwrap().contains("503"));
        let quarantined: OutboxRecord =
            serde_json::from_slice(&std::fs::read(dir.path().join("quarantine/21.json")).unwrap())
                .unwrap();
        assert!(quarantined.last_error.unwrap().contains("bad proof"));
    }

    #[tokio::test]
    async fn a_restart_resubmits_pending_records() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(dir.path()).unwrap();
        outbox.put(&record(dir.path(), 30)).unwrap();

        let down = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&down)
            .await;
        run_once(client(&down).await, outbox.clone()).await;
        assert_eq!(outbox.pending().unwrap().len(), 1);

        let up = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v0/proofs/proved"))
            .and(wiremock::matchers::header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&up)
            .await;
        run_once(client(&up).await, Outbox::new(dir.path()).unwrap()).await;
        assert!(outbox.pending().unwrap().is_empty());
    }
}
