use std::time::{Duration, Instant};

use alloy::providers::{DynProvider, Provider as _, ProviderBuilder, WsConnect};
use alloy::pubsub::Subscription;
use alloy::rpc::types::Header;
use url::Url;

use crate::fetcher::Fetcher;

/// Blocks this prover owns: `block % block_mod == prover_id`, so provers with different ids
/// and the same `block_mod` never pick the same block.
#[derive(Debug, Clone, Copy)]
pub struct BlockSelector {
    block_mod: u64,
    prover_id: u64,
}

impl BlockSelector {
    pub fn new(block_mod: u64, prover_id: u64) -> anyhow::Result<Self> {
        anyhow::ensure!(block_mod > 0, "block_mod must be positive");
        anyhow::ensure!(
            prover_id < block_mod,
            "prover_id ({prover_id}) must be below block_mod ({block_mod})"
        );
        Ok(Self {
            block_mod,
            prover_id,
        })
    }

    pub fn owns(&self, block_number: u64) -> bool {
        block_number % self.block_mod == self.prover_id
    }

    pub fn latest_at_or_below(&self, head: u64) -> Option<u64> {
        (head >= self.prover_id).then(|| head - (head - self.prover_id) % self.block_mod)
    }

    pub fn first_at_or_above(&self, block_number: u64) -> u64 {
        block_number
            + (self.block_mod + self.prover_id - block_number % self.block_mod) % self.block_mod
    }
}

/// Which blocks to prove: always the newest owned block (`Tip`), or every owned block of a
/// range in order (`Range`, unbounded `end` follows the chain).
#[derive(Debug, Clone, Copy)]
pub enum BlockRange {
    Tip,
    Range { start: u64, end: Option<u64> },
}

pub trait Heads: Send {
    /// The current chain head.
    fn head(&mut self) -> impl Future<Output = anyhow::Result<u64>> + Send;
    /// Waits until the chain head is above `known` and returns it.
    fn wait_above(&mut self, known: u64) -> impl Future<Output = anyhow::Result<u64>> + Send;
}

pub struct BlockStream<H> {
    selector: BlockSelector,
    range: BlockRange,
    heads: H,
    last_head: Option<u64>,
    last_selected: Option<u64>,
}

impl<H: Heads> BlockStream<H> {
    pub fn new(selector: BlockSelector, range: BlockRange, heads: H) -> Self {
        Self {
            selector,
            range,
            heads,
            last_head: None,
            last_selected: None,
        }
    }

    /// The next block to prove; `None` once a bounded range is exhausted.
    pub async fn next_block(&mut self) -> anyhow::Result<Option<u64>> {
        match self.range {
            BlockRange::Tip => loop {
                let head = match self.last_head {
                    Some(known) => self.heads.wait_above(known).await?,
                    None => self.heads.head().await?,
                };
                self.last_head = Some(head);
                if let Some(block) = self.selector.latest_at_or_below(head)
                    && self.last_selected.is_none_or(|last| block > last)
                {
                    self.last_selected = Some(block);
                    return Ok(Some(block));
                }
            },
            BlockRange::Range { start, end } => {
                let block = match self.last_selected {
                    Some(last) => last + self.selector.block_mod,
                    None => self.selector.first_at_or_above(start),
                };
                if end.is_some_and(|end| block > end) {
                    return Ok(None);
                }
                if self.last_head.is_none_or(|head| head < block) {
                    let head = self.heads.head().await?;
                    self.last_head = Some(if head >= block {
                        head
                    } else {
                        self.heads.wait_above(block - 1).await?
                    });
                }
                self.last_selected = Some(block);
                Ok(Some(block))
            }
        }
    }
}

const SUBSCRIPTION_WAIT: Duration = Duration::from_secs(2);
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(60);

/// Chain heads over HTTP. With a WebSocket URL, `newHeads` notifications wake the waiter
/// early; the head itself is always read over HTTP, and a lost subscription falls back to
/// polling while reconnecting with backoff.
pub struct HeadWatcher {
    fetcher: Fetcher,
    poll_interval: Duration,
    ws_url: Option<Url>,
    subscription: Option<(DynProvider, Subscription<Header>)>,
    reconnect_at: Instant,
    reconnect_backoff: Duration,
}

impl HeadWatcher {
    pub fn new(fetcher: Fetcher, poll_interval: Duration, ws_url: Option<Url>) -> Self {
        Self {
            fetcher,
            poll_interval,
            ws_url,
            subscription: None,
            reconnect_at: Instant::now(),
            reconnect_backoff: Duration::from_secs(1),
        }
    }

    async fn wait_for_change(&mut self) {
        if self.subscription.is_none()
            && Instant::now() >= self.reconnect_at
            && let Some(ws_url) = self.ws_url.clone()
        {
            match subscribe(&ws_url).await {
                Ok(subscription) => {
                    tracing::info!(
                        "Subscribed to newHeads at {}",
                        ws_url.host_str().unwrap_or("")
                    );
                    self.subscription = Some(subscription);
                    self.reconnect_backoff = Duration::from_secs(1);
                }
                Err(err) => {
                    tracing::warn!(
                        "newHeads subscription failed, polling for {:?}: {err:#}",
                        self.reconnect_backoff
                    );
                    self.reconnect_at = Instant::now() + self.reconnect_backoff;
                    self.reconnect_backoff =
                        (self.reconnect_backoff * 2).min(MAX_RECONNECT_BACKOFF);
                }
            }
        }
        match &mut self.subscription {
            Some((_, subscription)) => {
                match tokio::time::timeout(SUBSCRIPTION_WAIT, subscription.recv()).await {
                    Ok(Ok(_)) | Err(_) => {}
                    Ok(Err(err)) => {
                        tracing::warn!("newHeads subscription closed: {err}");
                        self.subscription = None;
                    }
                }
            }
            None => tokio::time::sleep(self.poll_interval).await,
        }
    }
}

async fn subscribe(ws_url: &Url) -> anyhow::Result<(DynProvider, Subscription<Header>)> {
    let provider = DynProvider::new(
        ProviderBuilder::new()
            .connect_ws(WsConnect::new(ws_url.as_str()))
            .await?,
    );
    let subscription = provider.subscribe_blocks().await?;
    Ok((provider, subscription))
}

impl Heads for HeadWatcher {
    async fn head(&mut self) -> anyhow::Result<u64> {
        self.fetcher.head().await
    }

    async fn wait_above(&mut self, known: u64) -> anyhow::Result<u64> {
        loop {
            let head = self.fetcher.head().await?;
            if head > known {
                return Ok(head);
            }
            self.wait_for_change().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// Heads replayed from a script; every read consumes the next value, and `wait_above`
    /// skips values that are not above `known`.
    struct ScriptedHeads(VecDeque<u64>);

    impl Heads for ScriptedHeads {
        async fn head(&mut self) -> anyhow::Result<u64> {
            self.0
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("script exhausted"))
        }

        async fn wait_above(&mut self, known: u64) -> anyhow::Result<u64> {
            loop {
                let head = self.head().await?;
                if head > known {
                    return Ok(head);
                }
            }
        }
    }

    fn stream(
        block_mod: u64,
        prover_id: u64,
        range: BlockRange,
        heads: &[u64],
    ) -> BlockStream<ScriptedHeads> {
        BlockStream::new(
            BlockSelector::new(block_mod, prover_id).unwrap(),
            range,
            ScriptedHeads(heads.iter().copied().collect()),
        )
    }

    #[test]
    fn selector_math() {
        let selector = BlockSelector::new(10, 2).unwrap();
        assert!(selector.owns(102));
        assert!(!selector.owns(103));
        assert_eq!(selector.latest_at_or_below(109), Some(102));
        assert_eq!(selector.latest_at_or_below(102), Some(102));
        assert_eq!(selector.latest_at_or_below(1), None);
        assert_eq!(selector.first_at_or_above(103), 112);
        assert_eq!(selector.first_at_or_above(102), 102);
        assert!(BlockSelector::new(0, 0).is_err());
        assert!(BlockSelector::new(4, 4).is_err());
    }

    #[tokio::test]
    async fn tip_takes_the_newest_owned_block_and_never_repeats() {
        let mut stream = stream(4, 1, BlockRange::Tip, &[100, 100, 101, 104, 106, 113]);
        assert_eq!(stream.next_block().await.unwrap(), Some(97));
        assert_eq!(stream.next_block().await.unwrap(), Some(101));
        // Head 104 still maps to 101, so the stream waits for 106 (-> 105).
        assert_eq!(stream.next_block().await.unwrap(), Some(105));
        assert_eq!(stream.next_block().await.unwrap(), Some(113));
        assert!(stream.next_block().await.is_err());
    }

    #[tokio::test]
    async fn range_visits_every_owned_block_and_waits_for_the_head() {
        let mut stream = stream(
            3,
            0,
            BlockRange::Range {
                start: 10,
                end: Some(20),
            },
            &[13, 14, 16, 30],
        );
        assert_eq!(stream.next_block().await.unwrap(), Some(12));
        assert_eq!(stream.next_block().await.unwrap(), Some(15));
        assert_eq!(stream.next_block().await.unwrap(), Some(18));
        assert_eq!(stream.next_block().await.unwrap(), None);
    }
}
