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
    /// Follows the chain and proves the newest owned block, or with `--start` every owned
    /// block from `--start` to `--end` (or onwards).
    Run {
        #[arg(long)]
        start: Option<u64>,
        #[arg(long, requires = "start")]
        end: Option<u64>,
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
