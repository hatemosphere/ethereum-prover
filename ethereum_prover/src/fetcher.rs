use std::{future::Future, time::Duration};

use alloy::{
    consensus::Header,
    eips::BlockNumberOrTag,
    primitives::Bytes,
    providers::{DynProvider, Provider, ext::DebugApi as _},
    rpc::types::{Block, debug::ExecutionWitness},
};
use anyhow::Context as _;
use serde::Deserialize;
use url::Url;

/// Shape of the `debug_executionWitness` response of the execution client the prover talks to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WitnessFormat {
    /// Reth: headers are RLP bytes, `state`/`codes`/`keys` are byte lists.
    #[default]
    Reth,
    /// Geth: headers are JSON objects, `state`/`codes` may be hash-to-bytes maps, and there
    /// are no key preimages.
    Geth,
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub attempts: usize,
    pub base_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            base_backoff: Duration::from_millis(500),
        }
    }
}

/// Block and execution-witness fetching over JSON-RPC.
#[derive(Clone)]
pub struct Fetcher {
    provider: DynProvider,
    http: reqwest::Client,
    rpc_url: Url,
    format: WitnessFormat,
    retry: RetryPolicy,
}

impl Fetcher {
    pub fn new(rpc_url: Url, format: WitnessFormat, retry: RetryPolicy) -> anyhow::Result<Self> {
        let provider = DynProvider::new(alloy::providers::builder().connect_http(rpc_url.clone()));
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("failed to build the RPC HTTP client")?;
        Ok(Self {
            provider,
            http,
            rpc_url,
            format,
            retry,
        })
    }

    pub fn provider(&self) -> &DynProvider {
        &self.provider
    }

    pub async fn head(&self) -> anyhow::Result<u64> {
        retry(self.retry, "fetch the chain head", || async {
            Ok(self.provider.get_block_number().await?)
        })
        .await
    }

    pub async fn block_with_witness(
        &self,
        block_number: u64,
    ) -> anyhow::Result<(Block, ExecutionWitness)> {
        retry(
            self.retry,
            &format!("fetch block {block_number} and its execution witness"),
            || async {
                let block = self
                    .provider
                    .get_block_by_number(BlockNumberOrTag::Number(block_number))
                    .full()
                    .await?
                    .with_context(|| format!("block {block_number} not found"))?;
                let witness = match self.format {
                    WitnessFormat::Reth => {
                        self.provider
                            .debug_execution_witness(BlockNumberOrTag::Number(block_number))
                            .await?
                    }
                    WitnessFormat::Geth => self.geth_witness(block_number).await?,
                };
                Ok((block, witness))
            },
        )
        .await
    }

    async fn geth_witness(&self, block_number: u64) -> anyhow::Result<ExecutionWitness> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "debug_executionWitness",
            "params": [format!("0x{block_number:x}")],
            "id": 1,
        });
        let mut response: serde_json::Value = self
            .http
            .post(self.rpc_url.clone())
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(error) = response.get("error") {
            anyhow::bail!("debug_executionWitness failed: {error}");
        }
        let witness: GethExecutionWitness = serde_json::from_value(response["result"].take())
            .context("failed to decode the geth execution witness")?;
        witness.into_execution_witness()
    }
}

/// Geth's `debug_executionWitness` result.
#[derive(Debug, Deserialize)]
struct GethExecutionWitness {
    #[serde(default)]
    headers: Vec<Header>,
    #[serde(default)]
    codes: serde_json::Value,
    #[serde(default)]
    state: serde_json::Value,
    #[serde(default)]
    keys: serde_json::Value,
}

impl GethExecutionWitness {
    fn into_execution_witness(self) -> anyhow::Result<ExecutionWitness> {
        let headers = self
            .headers
            .iter()
            .map(|header| Bytes::from(alloy::rlp::encode(header)))
            .collect();
        let keys = byte_list(&self.keys).context("execution witness keys")?;
        if keys.is_empty() {
            tracing::warn!("The geth execution witness has no key preimages");
        }
        Ok(ExecutionWitness {
            state: byte_list(&self.state).context("execution witness state")?,
            codes: byte_list(&self.codes).context("execution witness codes")?,
            keys,
            headers,
        })
    }
}

/// Bytes from a JSON hex-string list, the values of a hash-to-hex-string map, or null.
fn byte_list(value: &serde_json::Value) -> anyhow::Result<Vec<Bytes>> {
    let items: Vec<&serde_json::Value> = match value {
        serde_json::Value::Null => return Ok(Vec::new()),
        serde_json::Value::Array(items) => items.iter().collect(),
        serde_json::Value::Object(map) => map.values().collect(),
        other => anyhow::bail!("expected a list, a map or null, got {other}"),
    };
    items
        .into_iter()
        .map(|item| {
            let hex = item
                .as_str()
                .with_context(|| format!("expected a hex string, got {item}"))?;
            Ok(alloy::hex::decode(hex)
                .with_context(|| format!("invalid hex {hex}"))?
                .into())
        })
        .collect()
}

pub(crate) async fn retry<T, F, Fut>(
    policy: RetryPolicy,
    operation: &str,
    mut call: F,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    anyhow::ensure!(
        policy.attempts > 0,
        "retry policy requires at least one attempt"
    );
    for attempt in 1..=policy.attempts {
        match call().await {
            Ok(value) => return Ok(value),
            Err(err) if attempt < policy.attempts => {
                let backoff = policy.base_backoff.saturating_mul(1 << (attempt - 1));
                tracing::warn!(
                    "{operation} failed: {err:#}. Retrying attempt {}/{} in {backoff:?}",
                    attempt + 1,
                    policy.attempts
                );
                tokio::time::sleep(backoff).await;
            }
            Err(err) => return Err(err).with_context(|| operation.to_owned()),
        }
    }
    unreachable!("the retry loop returns on success or on the final failure")
}

#[cfg(test)]
mod tests {
    use std::future;

    use super::*;

    const NO_BACKOFF: RetryPolicy = RetryPolicy {
        attempts: 3,
        base_backoff: Duration::ZERO,
    };

    #[tokio::test]
    async fn retry_retries_until_success() {
        let mut attempts = 0;
        let value = retry(NO_BACKOFF, "fetch block", || {
            attempts += 1;
            future::ready(if attempts < 3 {
                Err(anyhow::anyhow!("transient RPC error"))
            } else {
                Ok(42_u64)
            })
        })
        .await
        .expect("retry succeeds");
        assert_eq!(value, 42);
        assert_eq!(attempts, 3);
    }

    #[tokio::test]
    async fn retry_returns_the_final_error() {
        let mut attempts = 0;
        let err = retry(NO_BACKOFF, "fetch head", || {
            attempts += 1;
            future::ready(Err::<u64, _>(anyhow::anyhow!("still failing")))
        })
        .await
        .expect_err("retry should fail");
        assert_eq!(attempts, 3);
        assert!(err.to_string().contains("fetch head"));
        assert!(
            err.chain()
                .any(|cause| cause.to_string().contains("still failing"))
        );
    }

    #[test]
    fn geth_witness_converts_to_the_reth_shape() {
        let header = Header {
            number: 7,
            gas_limit: 30_000_000,
            ..Default::default()
        };
        let json = serde_json::json!({
            "headers": [serde_json::to_value(&header).unwrap()],
            "codes": {"0x01": "0x6001", "0x02": "0x6002"},
            "state": ["0xc0", "0xc180"],
            "keys": null,
        });
        let witness: GethExecutionWitness = serde_json::from_value(json).unwrap();
        let witness = witness.into_execution_witness().unwrap();

        assert_eq!(
            witness.headers,
            vec![Bytes::from(alloy::rlp::encode(&header))]
        );
        let mut codes = witness.codes.clone();
        codes.sort();
        assert_eq!(
            codes,
            vec![Bytes::from(vec![0x60, 0x01]), Bytes::from(vec![0x60, 0x02])]
        );
        assert_eq!(
            witness.state,
            vec![Bytes::from(vec![0xc0]), Bytes::from(vec![0xc1, 0x80])]
        );
        assert!(witness.keys.is_empty());
    }

    #[test]
    fn byte_list_rejects_non_hex() {
        assert!(byte_list(&serde_json::json!(["0xzz"])).is_err());
        assert!(byte_list(&serde_json::json!(5)).is_err());
    }
}
