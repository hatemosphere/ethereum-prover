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
    Block {
        block_number: Option<u64>,
    },
    GenerateVerifierArtifacts {
        #[arg(long, default_value = "../artifacts")]
        output_dir: PathBuf,
        /// Distribution directory of the guest program the key is for.
        #[arg(long, default_value = "../artifacts/eth_stf")]
        app_dir: PathBuf,
        #[arg(long)]
        security: Option<ProofSecurity>,
    },
    Run,
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
