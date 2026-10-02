use alloy::{
    consensus::Header,
    eips::Encodable2718 as _,
    rlp::Encodable as _,
    rpc::types::{Block, Transaction, debug::ExecutionWitness},
};
use anyhow::Context as _;
use zksync_os_interface::traits::EncodedTx;

#[derive(Clone)]
pub struct EthBlockInput {
    pub transactions: Vec<Transaction>,
    pub encoded_transactions: Vec<EncodedTx>,
    pub execution_witness: ExecutionWitness,
    pub block_header: Header,
    pub withdrawals_rlp: Vec<u8>,
}

impl EthBlockInput {
    /// Reads `block.json` and `execution_witness.json` (or `witness.json`) from `dir`, as
    /// plain values or as the raw JSON-RPC responses that carry them in `result`.
    pub fn from_dir(dir: &std::path::Path) -> anyhow::Result<Self> {
        fn read<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> anyhow::Result<T> {
            let json = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let mut value: serde_json::Value = serde_json::from_str(&json)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            if value.get("jsonrpc").is_some() {
                value = value["result"].take();
            }
            serde_json::from_value(value)
                .with_context(|| format!("failed to decode {}", path.display()))
        }
        let witness_path = ["execution_witness.json", "witness.json"]
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
            .with_context(|| format!("no witness file in {}", dir.display()))?;
        Ok(Self::new(
            read(&dir.join("block.json"))?,
            read(&witness_path)?,
        ))
    }

    pub fn new(block: Block, execution_witness: ExecutionWitness) -> Self {
        let withdrawals_rlp = if let Some(withdrawals) = block.withdrawals.clone() {
            let mut buffer = Vec::new();
            withdrawals.encode(&mut buffer);
            buffer
        } else {
            Vec::new()
        };
        let encoded_transactions = block
            .transactions
            .clone()
            .into_transactions()
            .map(|tx| EncodedTx::Rlp(tx.inner.inner().encoded_2718(), tx.inner.signer()))
            .collect();

        Self {
            transactions: block.transactions.into_transactions().collect(),
            encoded_transactions,
            execution_witness,
            block_header: block.header.clone().into(),
            withdrawals_rlp,
        }
    }
}
