use std::time::Duration;

use base64::Engine as _;
use reqwest::StatusCode;
use serde::Serialize;
use url::Url;

use crate::metrics::METRICS;

pub const ETHPROOFS_STAGING_URL: &str = "https://staging--ethproofs.netlify.app/api/v0/";
pub const ETHPROOFS_PRODUCTION_URL: &str = "https://ethproofs.org/api/v0/";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome of a single request that did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// Worth retrying later (transport failure, timeout, 408, 425, 429, 5xx).
    Retryable {
        reason: String,
        retry_after: Option<Duration>,
    },
    /// HTTP 409: the request conflicts with what EthProofs already recorded for the block
    /// (for `proofs/proved`, the proof was already accepted).
    Conflict { reason: String },
    /// The server rejected the request; retrying the same request will not help.
    Permanent { reason: String },
}

/// Outcome of an accepted `proofs/proved` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proved {
    Accepted {
        proof_id: Option<String>,
    },
    /// EthProofs answered 409: it already has a proof of this block from this cluster.
    AlreadyRecorded,
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable { reason, .. } => write!(f, "retryable: {reason}"),
            Self::Conflict { reason } => write!(f, "conflict: {reason}"),
            Self::Permanent { reason } => write!(f, "permanent: {reason}"),
        }
    }
}

/// EthProofs API client. Every call is a single attempt; retry policy belongs to the caller.
#[derive(Clone, Debug)]
pub struct EthproofsClient {
    base_url: Url,
    auth_token: String,
    cluster_id: u64,
    verifier_id: Option<String>,
    client: reqwest::Client,
    dry_run_dir: Option<std::path::PathBuf>,
}

#[derive(Debug, Serialize)]
struct ProofRequest {
    block_number: u64,
    cluster_id: u64,
}

#[derive(Debug, Serialize)]
struct ProvedRequest<'a> {
    block_number: u64,
    cluster_id: u64,
    proving_time: u64,
    proving_cycles: u64,
    proof: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    verifier_id: Option<&'a str>,
}

impl EthproofsClient {
    pub fn new(
        base_url: Url,
        auth_token: String,
        cluster_id: u64,
        verifier_id: Option<String>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            base_url.path().ends_with('/'),
            "the EthProofs base URL must end with '/', got {base_url}"
        );
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Ok(Self {
            base_url,
            auth_token,
            cluster_id,
            verifier_id,
            client,
            dry_run_dir: None,
        })
    }

    /// Writes every request to `dir` as `<block>.<endpoint>.json` (`{"url", "body"}`) instead
    /// of sending it, and answers as an accepting server would.
    pub fn dry_run(mut self, dir: std::path::PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        self.dry_run_dir = Some(dir);
        Ok(self)
    }

    pub async fn queued(&self, block_number: u64) -> Result<(), SubmitError> {
        self.post("proofs/queued", &self.request(block_number))
            .await
            .map(drop)
    }

    pub async fn proving(&self, block_number: u64) -> Result<(), SubmitError> {
        self.post("proofs/proving", &self.request(block_number))
            .await
            .map(drop)
    }

    /// Submits a proof; `gzip_proof` is the archived gzip proof file, sent base64 encoded.
    pub async fn proved(
        &self,
        block_number: u64,
        proving_time_ms: u64,
        proving_cycles: u64,
        gzip_proof: &[u8],
    ) -> Result<Proved, SubmitError> {
        let payload = ProvedRequest {
            block_number,
            cluster_id: self.cluster_id,
            proving_time: proving_time_ms,
            proving_cycles,
            proof: base64::engine::general_purpose::STANDARD.encode(gzip_proof),
            verifier_id: self.verifier_id.as_deref(),
        };
        match self.post("proofs/proved", &payload).await {
            Ok(response) => Ok(Proved::Accepted {
                proof_id: response
                    .get("proof_id")
                    .map(|id| id.as_str().map_or_else(|| id.to_string(), str::to_owned)),
            }),
            Err(SubmitError::Conflict { .. }) => Ok(Proved::AlreadyRecorded),
            Err(err) => Err(err),
        }
    }

    fn request(&self, block_number: u64) -> ProofRequest {
        ProofRequest {
            block_number,
            cluster_id: self.cluster_id,
        }
    }

    /// One POST; returns the JSON response body (null if it is not JSON).
    async fn post<T: Serialize>(
        &self,
        path: &str,
        payload: &T,
    ) -> Result<serde_json::Value, SubmitError> {
        let url = self
            .base_url
            .join(path)
            .map_err(|err| SubmitError::Permanent {
                reason: format!("invalid endpoint {path}: {err}"),
            })?;
        if let Some(dir) = &self.dry_run_dir {
            return dry_run_write(dir, path, url, payload);
        }
        let latency = METRICS.ethproofs_request_duration.start();
        let result = match self
            .client
            .post(url)
            .bearer_auth(&self.auth_token)
            .json(payload)
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    Ok(response.json().await.unwrap_or_default())
                } else {
                    let retry_after = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let body = response.text().await.unwrap_or_default();
                    let reason = format!("{path}: HTTP {status}: {}", truncate(&body, 300));
                    Err(classify(status, reason, retry_after))
                }
            }
            Err(err) => Err(SubmitError::Retryable {
                reason: format!("{path}: {err}"),
                retry_after: None,
            }),
        };
        latency.observe();
        match &result {
            Ok(_) => METRICS.ethproofs_request_success_total.inc(),
            Err(_) => METRICS.ethproofs_request_failure_total.inc(),
        };
        result
    }
}

fn dry_run_write<T: Serialize>(
    dir: &std::path::Path,
    path: &str,
    url: Url,
    payload: &T,
) -> Result<serde_json::Value, SubmitError> {
    let body = serde_json::to_value(payload).map_err(|err| SubmitError::Permanent {
        reason: format!("cannot serialize {path}: {err}"),
    })?;
    let block_number = body["block_number"].as_u64().unwrap_or_default();
    let endpoint = path.rsplit('/').next().unwrap_or(path);
    let file = dir.join(format!("{block_number}.{endpoint}.json"));
    let request = serde_json::json!({ "url": url.as_str(), "body": body });
    crate::utils::write_atomic(
        &file,
        &serde_json::to_vec_pretty(&request).unwrap_or_default(),
    )
    .map_err(|err| SubmitError::Retryable {
        reason: format!("dry run: cannot write {}: {err}", file.display()),
        retry_after: None,
    })?;
    Ok(serde_json::json!({ "proof_id": "dry-run" }))
}

fn classify(status: StatusCode, reason: String, retry_after: Option<Duration>) -> SubmitError {
    if status == StatusCode::CONFLICT {
        SubmitError::Conflict { reason }
    } else if status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS
        )
    {
        SubmitError::Retryable {
            reason,
            retry_after,
        }
    } else {
        SubmitError::Permanent { reason }
    }
}

fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_statuses() {
        let retryable = [500, 502, 503, 408, 425, 429];
        let permanent = [400, 401, 403, 404, 422];
        assert!(matches!(
            classify(StatusCode::CONFLICT, String::new(), None),
            SubmitError::Conflict { .. }
        ));
        for code in retryable {
            let status = StatusCode::from_u16(code).unwrap();
            assert!(
                matches!(
                    classify(status, String::new(), None),
                    SubmitError::Retryable { .. }
                ),
                "{code}"
            );
        }
        for code in permanent {
            let status = StatusCode::from_u16(code).unwrap();
            assert!(
                matches!(
                    classify(status, String::new(), None),
                    SubmitError::Permanent { .. }
                ),
                "{code}"
            );
        }
    }

    #[test]
    fn base_url_must_be_a_directory() {
        let url = Url::parse("https://example.com/api/v0").unwrap();
        assert!(EthproofsClient::new(url, String::new(), 1, None).is_err());
    }
}
