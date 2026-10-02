//! Important: each top-level file in `tests/` is compiled as a separate crate.
//! This might explode the compile time for tests, so avoid adding new files here.
//! Wherever possible, prefer using test vectors within a single test, or creating
//! unit tests.
//! This module is for heavy integration tests only.

mod common;

use ethereum_prover::prover::cpu_witness::CpuWitnessGenerator;
use ethereum_prover::prover::gpu_prover::Prover;
use ethereum_prover::prover::oracle::record_prover_input;
use ethereum_prover::types::ProofSecurity;

macro_rules! require_gpu_tests {
    () => {
        if std::env::var("RUN_GPU_TESTS").ok().as_deref() != Some("1") {
            eprintln!("Skipping GPU test. Set RUN_GPU_TESTS=1 to enable.");
            return;
        }
    };
}

#[tokio::test]
async fn cpu_witness_from_fixture_block() {
    common::init_tracing();
    let input = common::load_fixture_input("24073997");
    let block_number = input.block_header.number;
    let generator = CpuWitnessGenerator::new();

    generator
        .forward_run(block_number, input.clone())
        .await
        .expect("forward run");
    let witness = generator
        .generate_witness(block_number, input)
        .await
        .expect("generate witness");

    assert!(!witness.is_empty());
}

#[tokio::test]
async fn gpu_prover_from_fixture_block() {
    require_gpu_tests!();

    common::init_tracing();
    let input = common::load_fixture_input("24073997");
    let mut prover = Prover::new(
        common::app_dir().as_path(),
        None,
        ProofSecurity::Security100,
    )
    .expect("create prover");

    let result = prover
        .prove(input.block_header.number, input)
        .await
        .expect("prove block");

    assert!(!result.proof_bytes.is_empty());
    assert!(result.cycles > 0);
    assert!(result.proving_time_secs > 0.0);
}

/// Prover input parity with zksync-os eth_runner: each directory in `PARITY_BLOCK_DIRS`
/// (colon separated) holds the raw JSON-RPC responses `block.json` and `witness.json` and
/// the words eth_runner recorded for them, `prover_input.bincode`.
#[test]
fn prover_input_matches_eth_runner() {
    let Ok(dirs) = std::env::var("PARITY_BLOCK_DIRS") else {
        eprintln!("Skipping prover input parity. Set PARITY_BLOCK_DIRS to enable.");
        return;
    };
    for dir in dirs.split(':').map(std::path::PathBuf::from) {
        let input = common::load_rpc_response_input(&dir);
        let words = record_prover_input(input).expect("record prover input");
        let expected = common::load_eth_runner_prover_input(&dir.join("prover_input.bincode"));
        assert_eq!(
            words.len(),
            expected.len(),
            "word count for {}",
            dir.display()
        );
        assert!(
            words == expected,
            "prover input differs for {}",
            dir.display()
        );
        eprintln!("{}: {} words match", dir.display(), words.len());
    }
}
