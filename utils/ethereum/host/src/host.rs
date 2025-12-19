use std::sync::Arc;

use crate::witness_generator::ETHDAWitnessGenerator;
use alloy_eips::BlockId;
use alloy_primitives::B256;
use anyhow::Result;
use async_trait::async_trait;
use kona_host::single::SingleChainHost;
use op_zisk_ethereum_client_utils::executor::ETHDAWitnessExecutor;
use op_zisk_host_utils::{fetcher::OPZisKDataFetcher, host::OPZisKHost};

#[derive(Clone)]
pub struct SingleChainOPZisKHost {
    pub fetcher: Arc<OPZisKDataFetcher>,
    witness_generator: Arc<ETHDAWitnessGenerator>,
}

#[async_trait]
impl OPZisKHost for SingleChainOPZisKHost {
    type Args = SingleChainHost;
    type WitnessGenerator = ETHDAWitnessGenerator;

    fn witness_generator(&self) -> &Self::WitnessGenerator {
        &self.witness_generator
    }

    async fn fetch(
        &self,
        l2_start_block: u64,
        l2_end_block: u64,
        l1_head_hash: Option<B256>,
        safe_db_fallback: bool,
    ) -> Result<SingleChainHost> {
        // Calculate L1 head hash using simple logic if not provided.
        let l1_head_hash = match l1_head_hash {
            Some(hash) => hash,
            None => {
                self.calculate_safe_l1_head(&self.fetcher, l2_end_block, safe_db_fallback).await?
            }
        };

        let host = self.fetcher.get_host_args(l2_start_block, l2_end_block, l1_head_hash).await?;
        Ok(host)
    }

    fn get_l1_head_hash(&self, args: &Self::Args) -> Option<B256> {
        Some(args.l1_head)
    }

    async fn get_finalized_l2_block_number(
        &self,
        fetcher: &OPZisKDataFetcher,
        _: u64,
    ) -> Result<Option<u64>> {
        let finalized_l2_block_number = fetcher.get_l2_header(BlockId::finalized()).await?;
        Ok(Some(finalized_l2_block_number.number))
    }

    async fn calculate_safe_l1_head(
        &self,
        fetcher: &OPZisKDataFetcher,
        l2_end_block: u64,
        safe_db_fallback: bool,
    ) -> Result<B256> {
        // Try to get the L1 head using SafeDB first
        match fetcher.get_l1_head(l2_end_block, false).await {
            Ok((_, l1_head_number)) => {
                // SafeDB worked - add small buffer but cap at chain head
                let chain_head = fetcher.get_l1_header(BlockId::latest()).await?;
                let l1_head_number_with_buffer = l1_head_number + 20;
                let safe_l1_head_number = std::cmp::min(l1_head_number_with_buffer, chain_head.number);
                Ok(fetcher.get_l1_header(safe_l1_head_number.into()).await?.hash_slow())
            }
            Err(_) if safe_db_fallback => {
                // SafeDB not available - use latest chain head for devnet
                // For devnet, finalized() might be too old (block 0), so use latest() instead
                // However, we need to use a block that's well behind latest to ensure
                // all batches are finalized and available for reading, and to account for chain
                // head changes between reads
                // Re-check the chain head right before calculating to get the most current value
                let final_chain_head = fetcher.get_l1_header(BlockId::latest()).await?;
                // Use a block 15 blocks behind latest to ensure batches are finalized and available
                // This larger margin accounts for potential race conditions where the chain head
                // changes between reads, especially in fast-moving devnets
                let safe_l1_head_number = final_chain_head.number.saturating_sub(15);
                // Ensure we don't go below block 0
                let safe_l1_head_number = std::cmp::max(safe_l1_head_number, 0);
                Ok(fetcher.get_l1_header(safe_l1_head_number.into()).await?.hash_slow())
            }
            Err(e) => Err(e),
        }
    }
}

impl SingleChainOPZisKHost {
    pub fn new(fetcher: Arc<OPZisKDataFetcher>) -> Self {
        Self {
            fetcher,
            witness_generator: Arc::new(ETHDAWitnessGenerator {
                executor: ETHDAWitnessExecutor::new(),
            }),
        }
    }
}
