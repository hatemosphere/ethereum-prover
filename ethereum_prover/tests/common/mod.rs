use std::path::PathBuf;
use std::sync::Once;

use alloy::rpc::types::{Block as RpcBlock, debug::ExecutionWitness};
use ethereum_prover::prover::types::EthBlockInput;

fn manifest_dir() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .filter(|path| path.join("test_fixtures").is_dir())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

pub fn fixture_root() -> PathBuf {
    manifest_dir().join("test_fixtures")
}

pub fn fixture_block_path(fixture: &str) -> PathBuf {
    fixture_root()
        .join("blocks")
        .join(fixture)
        .join("block.json")
}

pub fn fixture_witness_path(fixture: &str) -> PathBuf {
    fixture_root()
        .join("blocks")
        .join(fixture)
        .join("execution_witness.json")
}

pub fn app_dir() -> PathBuf {
    manifest_dir().join("../artifacts/eth_stf")
}

pub fn load_fixture_input(fixture: &str) -> EthBlockInput {
    let block_path = fixture_block_path(fixture);
    let witness_path = fixture_witness_path(fixture);
    let block_json = std::fs::read_to_string(&block_path)
        .unwrap_or_else(|err| panic!("read fixture block {}: {err}", block_path.display()));
    let witness_json = std::fs::read_to_string(&witness_path)
        .unwrap_or_else(|err| panic!("read fixture witness {}: {err}", witness_path.display()));
    let block: RpcBlock = serde_json::from_str(&block_json).expect("parse fixture block");
    let witness: ExecutionWitness =
        serde_json::from_str(&witness_json).expect("parse fixture witness");
    EthBlockInput::new(block, witness)
}

/// Block and witness from raw JSON-RPC responses (`{"jsonrpc", "id", "result"}`) as
/// `eth_getBlockByNumber` and `debug_executionWitness` return them.
pub fn load_rpc_response_input(dir: &std::path::Path) -> EthBlockInput {
    fn result<T: serde::de::DeserializeOwned>(path: PathBuf) -> T {
        let json = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        let mut response: serde_json::Value = serde_json::from_str(&json).expect("parse response");
        serde_json::from_value(response["result"].take()).expect("parse result")
    }
    EthBlockInput::new(
        result(dir.join("block.json")),
        result(dir.join("witness.json")),
    )
}

/// Words written by zksync-os eth_runner's `write_prover_input` (bincode of a newtype over
/// `Vec<u32>`).
pub fn load_eth_runner_prover_input(path: &std::path::Path) -> Vec<u32> {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let (words, _): (Vec<u32>, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .expect("decode prover input");
    words
}

pub fn init_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::builder()
            .with_default_directive("info".parse().unwrap())
            .from_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        tracing_subscriber::fmt().with_env_filter(filter).init();
    });
}
