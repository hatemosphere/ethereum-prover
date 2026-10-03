use alloy::consensus::Header;
use alloy::rlp::{Decodable, Encodable};
use anyhow::{anyhow, bail};
use basic_bootloader::bootloader::BasicBootloader;
use basic_bootloader::bootloader::block_flow::ethereum::PectraForkHeader;
use basic_bootloader::bootloader::config::BasicBootloaderForwardETHLikeConfig;
use basic_bootloader::bootloader::transaction_flow::ethereum::EthereumTransactionFlow;
use basic_system::system_implementation::ethereum_storage_model::caches::account_properties::EthereumAccountProperties;
use basic_system::system_implementation::ethereum_storage_model::vec_trait::VecCtor;
use basic_system::system_implementation::ethereum_storage_model::{
    BoxInterner, EthereumMPT, Path as MptPath, digits_from_key,
};
use crypto::MiniDigest;
use forward_system::run::TxResultCallback;
use forward_system::run::query_processors::{
    ChainConfigResponder, DACommitmentSchemeResponder, EthereumCLResponder,
    EthereumTargetBlockHeaderResponder, GenericPreimageResponder,
    InMemoryEthereumInitialAccountStateResponder, InMemoryEthereumInitialStorageSlotValueResponder,
    TxDataResponder, UARTPrintResponder,
};
use forward_system::run::result_keeper::ForwardRunningResultKeeper;
use forward_system::run::test_impl::{InMemoryPreimageSource, NoopTxCallback};
use forward_system::system::system_types::ethereum::EthereumStorageSystemTypesWithPostOps;
use oracle_provider::{ReadWitnessSource, RunMode, ZkEENonDeterminismSource};
use ruint::aliases::B160;
use std::alloc::Global;
use std::collections::{BTreeMap, HashMap};
use zk_ee::common_structs::da_commitment_scheme::DACommitmentScheme;
use zk_ee::oracle::IOOracle;
use zk_ee::system::metadata::chain_config::ChainConfig;
use zk_ee::system::tracer::NopTracer;
use zk_ee::system::validator::NopTxValidator;
use zk_ee::utils::Bytes32;
use zksync_os_interface::traits::TxListSource;

use crate::prover::types::EthBlockInput;

pub fn build_oracle(
    input: EthBlockInput,
    mode: RunMode,
) -> anyhow::Result<ZkEENonDeterminismSource> {
    let mut headers: Vec<Header> = input
        .execution_witness
        .headers
        .iter()
        .map(|el| {
            let mut slice: &[u8] = &el.0;
            Header::decode(&mut slice).map_err(|_| anyhow!("failed to decode header"))
        })
        .collect::<anyhow::Result<_>>()?;

    if headers.is_empty() {
        bail!("execution witness contains no headers");
    }
    if !headers.is_sorted_by(|a, b| a.number < b.number) {
        bail!("execution witness headers are not sorted");
    }

    headers.reverse();

    let mut headers_encodings: Vec<_> = input
        .execution_witness
        .headers
        .iter()
        .map(|el| el.0.to_vec())
        .collect();
    headers_encodings.reverse();

    let initial_root = headers[0].state_root;

    let mut preimage_source = InMemoryPreimageSource::default();
    let mut preimages_oracle: BTreeMap<Bytes32, Vec<u8>> = BTreeMap::new();

    for el in input
        .execution_witness
        .state
        .iter()
        .chain(input.execution_witness.codes.iter())
    {
        let hash = Bytes32::from_array(crypto::sha3::Keccak256::digest(el).0);
        preimages_oracle.insert(hash, el.to_vec());
        preimage_source.inner.insert(hash, el.to_vec());
    }

    let mut interner = BoxInterner::with_capacity_in(1 << 26, Global);
    let mut hasher = crypto::sha3::Keccak256::new();
    let mut accounts_mpt: EthereumMPT<'_, Global, VecCtor, false> =
        EthereumMPT::new_in(initial_root.0, &mut interner, Global)
            .map_err(|_| anyhow!("failed to initialize accounts MPT"))?;

    let mut account_properties = HashMap::<B160, EthereumAccountProperties>::new();
    for el in input.execution_witness.keys.iter() {
        if el.len() == 20 {
            let hash = crypto::sha3::Keccak256::digest(el);
            let digits = digits_from_key(&hash);
            let path = MptPath::new(&digits);
            if let Ok(props) =
                accounts_mpt.get(path, &mut preimages_oracle, &mut interner, &mut hasher)
            {
                let props = EthereumAccountProperties::parse_from_rlp_bytes(props)
                    .map_err(|_| anyhow!("failed to parse account properties"))?;
                let key_bytes: [u8; 20] = el[..]
                    .try_into()
                    .map_err(|_| anyhow!("execution witness account key is not 20 bytes"))?;
                let key = B160::from_be_bytes::<20>(key_bytes);
                account_properties.insert(key, props);
            }
        }
    }

    let tx_source = TxListSource {
        transactions: input.encoded_transactions.into(),
    };

    let mut target_header_encoding = vec![];
    input.block_header.encode(&mut target_header_encoding);

    let target_header_responder = EthereumTargetBlockHeaderResponder {
        target_header: input.block_header,
        target_header_encoding,
    };
    let tx_data_responder = TxDataResponder {
        tx_source,
        next_tx: None,
        next_tx_format: None,
        next_tx_from: None,
    };
    let da_commitment_scheme_responder = DACommitmentSchemeResponder {
        da_commitment_scheme: Some(DACommitmentScheme::None),
    };
    let chain_config_responder = ChainConfigResponder {
        chain_config: ChainConfig::default(),
    };
    let preimage_responder = GenericPreimageResponder { preimage_source };
    let initial_account_state_responder = InMemoryEthereumInitialAccountStateResponder::new(
        initial_root.0,
        account_properties.clone(),
        preimages_oracle.clone(),
    );
    let initial_values_responder =
        InMemoryEthereumInitialStorageSlotValueResponder::new(account_properties, preimages_oracle);

    let cl_responder = EthereumCLResponder {
        withdrawals_list: input.withdrawals_rlp,
        parent_headers_list: headers,
        parent_headers_encodings_list: headers_encodings,
    };

    let mut oracle = ZkEENonDeterminismSource::new(mode);
    oracle.add_external_processor(chain_config_responder);
    oracle.add_external_processor(target_header_responder);
    oracle.add_external_processor(tx_data_responder);
    oracle.add_external_processor(preimage_responder);
    oracle.add_external_processor(initial_account_state_responder);
    oracle.add_external_processor(initial_values_responder);
    oracle.add_external_processor(cl_responder);
    oracle.add_external_processor(da_commitment_scheme_responder);
    if mode.produces_native_run_responses() {
        oracle.add_external_processor(
            callable_oracles::blob_kzg_commitment::NativeBlobCommitmentAndProofQuery,
        );
        oracle.add_external_processor(callable_oracles::arithmetic::NativeArithmeticQuery);
        oracle.add_external_processor(callable_oracles::field_hints::NativeFieldOpsQuery);
    } else {
        oracle.add_external_processor(
            callable_oracles::blob_kzg_commitment::BlobCommitmentAndProofQuery,
        );
        oracle.add_external_processor(callable_oracles::arithmetic::ArithmeticQuery);
        oracle.add_external_processor(callable_oracles::field_hints::FieldOpsQuery);
    }
    oracle.add_external_processor(UARTPrintResponder);

    Ok(oracle)
}

/// Runs the block natively once with the Ethereum proving system types and records every
/// non-determinism word the `eth_stf` RISC-V program reads: the prover input.
pub fn record_prover_input(input: EthBlockInput) -> anyhow::Result<Vec<u32>> {
    let oracle = build_oracle(input, RunMode::NativeRunSavingForRiscV)?;
    let (_, oracle) = run_block(ReadWitnessSource::new(oracle), NoopTxCallback);
    let words = oracle?.get_read_items().borrow().clone();
    Ok(words)
}

/// Runs the block natively without recording; `callback` sees every executed transaction
/// and is returned whether or not the run succeeds.
pub fn run_forward<C: TxResultCallback>(
    input: EthBlockInput,
    callback: C,
) -> anyhow::Result<(C, anyhow::Result<()>)> {
    let oracle = build_oracle(input, RunMode::NativeRunOnly)?;
    let (callback, oracle) = run_block(oracle, callback);
    Ok((callback, oracle.map(|_| ())))
}

fn run_block<O: IOOracle, C: TxResultCallback>(
    mut oracle: O,
    callback: C,
) -> (C, anyhow::Result<O>) {
    let mut result_keeper: ForwardRunningResultKeeper<C, PectraForkHeader> =
        ForwardRunningResultKeeper::new(callback);
    let result = ChainConfig::read_from_oracle(&mut oracle)
        .map_err(|err| anyhow!("failed to read the chain config: {err:?}"))
        .and_then(|chain_config| {
            BasicBootloader::<
                EthereumStorageSystemTypesWithPostOps<O>,
                EthereumTransactionFlow<EthereumStorageSystemTypesWithPostOps<O>>,
            >::run_prepared::<BasicBootloaderForwardETHLikeConfig>(
                oracle,
                &mut (),
                &mut result_keeper,
                &mut NopTracer::default(),
                &mut NopTxValidator,
                chain_config,
            )
            .map(|(oracle, _, _)| oracle)
            .map_err(|err| anyhow!("the STF failed: {err:?}"))
        });
    (result_keeper.tx_result_callback, result)
}
