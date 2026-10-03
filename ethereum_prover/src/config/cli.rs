use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::types::ProofSecurity;

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Cli {
    #[arg(long)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Proves (or, with `mode: cpu_witness`, executes) one block, from the cache when it is
    /// there and over RPC otherwise.
    Block { block_number: u64 },
    GenerateVerifierArtifacts {
        #[arg(long, default_value = "../artifacts")]
        output_dir: PathBuf,
        /// Distribution directory of the guest program the key is for.
        #[arg(long, default_value = "../artifacts/eth_stf")]
        app_dir: PathBuf,
        #[arg(long)]
        security: Option<ProofSecurity>,
    },
    /// Follows the chain and proves every owned block in order, from `--start` (default: the
    /// current head) to `--end` (default: onwards). With `--tip`, proves only the newest owned
    /// block and drops queued blocks that a newer one supersedes.
    Run {
        #[arg(long, conflicts_with = "tip")]
        start: Option<u64>,
        #[arg(long, conflicts_with = "tip")]
        end: Option<u64>,
        #[arg(long)]
        tip: bool,
    },
    /// Proves the block in `--input-dir` once to warm up and then `--runs` more times with the
    /// same prover, reporting prover input, proving and total time per run.
    Bench {
        #[arg(long)]
        input_dir: PathBuf,
        #[arg(long, default_value_t = 5)]
        runs: usize,
    },
    /// Verifies a gzip EthProofs proof against a v2 verification key and prints the guest's
    /// public output.
    Verify {
        proof: PathBuf,
        #[arg(long)]
        key: PathBuf,
    },
    /// Proves one block from `--input-dir` (`block.json` + `execution_witness.json`, plain or
    /// raw JSON-RPC responses) and writes the gzip EthProofs proof, without submitting it.
    Prove {
        #[arg(long)]
        input_dir: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// Also write the native proof artifact as JSON (for verification and debugging).
        #[arg(long)]
        artifact_json: Option<PathBuf>,
    },
}
