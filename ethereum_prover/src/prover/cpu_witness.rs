use std::collections::VecDeque;

use alloy::providers::DynProvider;
use alloy::providers::Provider;
use alloy::rpc::types::Transaction;
use anyhow::Context as _;
use forward_system::run::InvalidTransaction;
use forward_system::run::TxResultCallback;
use forward_system::run::result_keeper::TxProcessingOutputOwned;
use forward_system::run::test_impl::NoopTxCallback;

use crate::prover::oracle::{record_prover_input, run_forward};
use crate::prover::types::EthBlockInput;
use crate::{cache::CacheStorage, observability};

#[derive(Debug, Clone, Default)]
pub struct CpuWitnessGenerator;

impl CpuWitnessGenerator {
    pub fn new() -> Self {
        Self
    }

    pub async fn forward_run(&self, block_number: u64, input: EthBlockInput) -> anyhow::Result<()> {
        match observability::spawn_blocking_on_current_hub(move || {
            let (_, result) = run_forward(input, NoopTxCallback)?;
            result.context("failed to run the STF in forward-run mode")
        })
        .await
        {
            Ok(result) => result,
            Err(err) => {
                let panic_msg = crate::utils::extract_panic_message(err);
                Err(anyhow::anyhow!(
                    "forward-run task panicked while processing block {block_number}: {panic_msg}"
                ))
            }
        }
    }

    pub async fn debug(
        &self,
        block_number: u64,
        input: EthBlockInput,
        debugger: DebuggerTxCallback,
    ) -> anyhow::Result<DebuggerTxCallback> {
        match observability::spawn_blocking_on_current_hub(move || {
            // We ignore the run result, as we are debugging and getting the results.
            let (debugger, _) = run_forward(input, debugger)?;
            Ok(debugger)
        })
        .await
        {
            Ok(result) => result,
            Err(err) => {
                let panic_msg = crate::utils::extract_panic_message(err);
                Err(anyhow::anyhow!(
                    "debug task panicked while processing block {block_number}: {panic_msg}"
                ))
            }
        }
    }

    pub async fn generate_witness(
        &self,
        block_number: u64,
        input: EthBlockInput,
    ) -> anyhow::Result<Vec<u32>> {
        match observability::spawn_blocking_on_current_hub(move || record_prover_input(input)).await
        {
            Ok(Ok(witness)) => Ok(witness),
            Ok(Err(err)) => Err(err).with_context(|| {
                format!("failed to record the prover input for block {block_number}")
            }),
            Err(err) => {
                let panic_msg = crate::utils::extract_panic_message(err);
                Err(anyhow::anyhow!(
                    "witness generation task panicked while processing block {block_number}: {panic_msg}"
                ))
            }
        }
    }
}

#[derive(Clone)]
pub struct DebuggerTxCallback {
    block_number: u64,
    txs: VecDeque<Transaction>,
    provider: DynProvider,
    problems: Vec<String>,
    cache: CacheStorage,
}

impl DebuggerTxCallback {
    pub fn new(
        block_number: u64,
        txs: Vec<Transaction>,
        provider: DynProvider,
        cache: CacheStorage,
    ) -> Self {
        Self {
            block_number,
            txs: VecDeque::from(txs),
            provider,
            problems: vec![],
            cache,
        }
    }

    pub fn get_problems(&self) -> &[String] {
        &self.problems
    }
}

impl TxResultCallback for DebuggerTxCallback {
    fn tx_executed(
        &mut self,
        tx_execution_result: Result<TxProcessingOutputOwned, InvalidTransaction>,
    ) {
        let Some(executed_tx) = self.txs.pop_front() else {
            tracing::error!("Transaction stream is empty, but tx_executed was called");
            return;
        };

        let Ok(tx_execution_result) = tx_execution_result else {
            let Err(err) = tx_execution_result else {
                return;
            };
            tracing::error!(
                "Transaction {:?} was considered invalid: {:?}",
                executed_tx.inner.tx_hash(),
                err
            );
            return;
        };

        let rt_handle = tokio::runtime::Handle::current();
        let tx_hash = executed_tx.inner.tx_hash();
        tracing::debug!("Debugging transaction {tx_hash:?}");

        let receipt = if let Ok(Some(receipt)) = self.cache.load_receipt(self.block_number, tx_hash)
        {
            receipt
        } else {
            let receipt_result =
                rt_handle.block_on(async { self.provider.get_transaction_receipt(*tx_hash).await });
            let receipt = match receipt_result {
                Ok(Some(receipt)) => receipt,
                Ok(None) => {
                    tracing::error!("Transaction receipt not found for {:?}", tx_hash);
                    return;
                }
                Err(err) => {
                    tracing::error!(
                        "Failed to get transaction receipt for {:?}: {}",
                        tx_hash,
                        err
                    );
                    return;
                }
            };

            if let Err(err) = self.cache.save_receipt(self.block_number, receipt.clone()) {
                tracing::error!("Failed to save cache entry: {err}");
            }

            receipt
        };

        tracing::debug!("Fetched receipt for transaction {tx_hash:?}");

        if tx_execution_result.status != receipt.status() {
            tracing::error!(
                "Transaction {:?} execution status mismatch: STF status = {:?}, Ethereum status = {:?}",
                tx_hash,
                tx_execution_result.status,
                receipt.status()
            );
        }
        if tx_execution_result.gas_used != receipt.gas_used {
            tracing::error!(
                "Transaction {:?} gas used mismatch: STF gas used = {}, Ethereum gas used = {}",
                tx_hash,
                tx_execution_result.gas_used,
                receipt.gas_used
            );
        }
    }
}
