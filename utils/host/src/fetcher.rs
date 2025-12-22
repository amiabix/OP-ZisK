use std::{
    cmp::{min, Ordering},
    env, fs,
    future::Future,
    path::PathBuf,
    str::FromStr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy_consensus::{BlockHeader, Header};
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{keccak256, Address, Bytes, B256, U256, U64};
use alloy_provider::{Provider, ProviderBuilder, RootProvider};
use alloy_rlp::Decodable;
use alloy_sol_types::SolValue;
use anyhow::{anyhow, bail, Context, Result};
use futures::{stream, StreamExt};
use kona_genesis::RollupConfig;
use kona_host::single::SingleChainHost;
use kona_protocol::L2BlockInfo;
use kona_registry::L1_CONFIGS;
use kona_rpc::{OutputResponse, SafeHeadResponse};
use op_alloy_consensus::OpBlock;
use op_alloy_network::{primitives::HeaderResponse, BlockResponse, Network, Optimism};
use op_zisk_client_utils::boot::BootInfoStruct;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};

use crate::L2Output;

fn is_method_not_found_str(s: &str) -> bool {
    s.contains("method not found") || s.contains("Method not found")
}

fn is_historical_state_unavailable_str(s: &str) -> bool {
    s.contains("not supported")
        || s.contains("missing trie node")
        || s.contains("unknown ancestor")
        || s.contains("header not found")
        || s.contains("state is not available")
        || s.contains("historical state")
        || s.contains("proof window")
        || s.contains("distance to target block")
}

fn is_method_not_found_opaque(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}");
    is_method_not_found_str(&s)
}

fn is_historical_state_unavailable_opaque(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}");
    is_historical_state_unavailable_str(&s)
}

fn rpc_concurrency() -> usize {
    env::var("OP_ZISK_RPC_CONCURRENCY").or_else(|_| env::var("OP_SUCCINCT_RPC_CONCURRENCY"))
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v >= 1)
        .unwrap_or(2)
}

fn rpc_retries() -> usize {
    env::var("OP_ZISK_RPC_RETRIES").or_else(|_| env::var("OP_SUCCINCT_RPC_RETRIES"))
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v >= 1)
        .unwrap_or(6)
}

fn should_retry_rpc_error(err: &anyhow::Error) -> bool {
    // Providers (reqwest/alloy) surface transport errors via error strings. Prefer being resilient
    // to transient upstream failures (rate limits, overload, short-lived disconnects).
    let s = format!("{err:?}");
    s.contains("HTTP error 503")
        || s.contains("Unable to complete request at this time")
        || s.contains("HTTP error 429")
        || s.contains("Too Many Requests")
        || s.contains("timed out")
        || s.contains("timeout")
        || s.contains("connection reset")
        || s.contains("Connection refused")
        || s.contains("broken pipe")
        || s.contains("temporarily unavailable")
}

async fn retry_rpc<T, F, Fut>(mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let attempts = rpc_retries();
    let mut last_err: Option<anyhow::Error> = None;

    for i in 0..attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !should_retry_rpc_error(&e) || i + 1 == attempts {
                    return Err(e);
                }
                last_err = Some(e);
                // Exponential backoff with a small cap.
                let backoff_ms = (250u64.saturating_mul(2u64.saturating_pow(i as u32))).min(5000);
                sleep(Duration::from_millis(backoff_ms)).await;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("retry loop exhausted without error state")))
}

#[derive(Clone)]
/// The OPZisKDataFetcher struct is used to fetch the L2 output data and L2 claim data for a
/// given block number. It is used to generate the boot info for the native host program.
pub struct OPZisKDataFetcher {
    pub rpc_config: RPCConfig,
    pub l1_provider: Arc<RootProvider>,
    pub l2_provider: Arc<RootProvider<Optimism>>,
    pub rollup_config: Option<RollupConfig>,
    pub rollup_config_path: Option<PathBuf>,
    pub l1_config_path: Option<PathBuf>,
}

// Removed Default implementation - use new() which returns Result
// This prevents panics from unwrap() in Default::default()
// Callers should use OPZisKDataFetcher::new()? instead

#[derive(Debug, Clone)]
pub struct RPCConfig {
    pub l1_rpc: Url,
    pub l1_beacon_rpc: Option<Url>,
    pub l2_rpc: Url,
    pub l2_node_rpc: Url,
}

/// The mode corresponding to the chain we are fetching data for.
#[derive(Clone, Copy, Debug)]
pub enum RPCMode {
    L1,
    L1Beacon,
    L2,
    L2Node,
}

/// Gets the RPC URLs from environment variables.
///
/// L1_RPC: The L1 RPC URL.
/// L1_BEACON_RPC: The L1 beacon RPC URL.
/// L2_RPC: The L2 RPC URL.
/// L2_NODE_RPC: The L2 node RPC URL.
///
/// This function now uses proper error handling instead of panicking.
pub fn get_rpcs_from_env() -> Result<RPCConfig> {
    let l1_rpc = env::var("L1_RPC")
        .context("L1_RPC environment variable is required for proof generation")?;
    let maybe_l1_beacon_rpc = env::var("L1_BEACON_RPC").ok();

    // L1_BEACON_RPC is optional. If not set or empty, set to None.
    let l1_beacon_rpc = maybe_l1_beacon_rpc
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Url::parse(s))
        .transpose()
        .context("L1_BEACON_RPC must be a valid URL if provided")?;

    let l2_rpc = env::var("L2_RPC")
        .context("L2_RPC environment variable is required for proof generation")?;
    let l2_node_rpc = env::var("L2_NODE_RPC")
        .context("L2_NODE_RPC environment variable is required for proof generation")?;

    Ok(RPCConfig {
        l1_rpc: Url::parse(&l1_rpc)
            .with_context(|| format!("L1_RPC must be a valid URL, got: {}", l1_rpc))?,
        l1_beacon_rpc,
        l2_rpc: Url::parse(&l2_rpc)
            .with_context(|| format!("L2_RPC must be a valid URL, got: {}", l2_rpc))?,
        l2_node_rpc: Url::parse(&l2_node_rpc)
            .with_context(|| format!("L2_NODE_RPC must be a valid URL, got: {}", l2_node_rpc))?,
    })
}

/// The info to fetch for a block.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BlockInfo {
    pub block_number: u64,
    pub transaction_count: u64,
    pub gas_used: u64,
    pub total_l1_fees: u128,
    pub total_tx_fees: u128,
}

/// The fee data for a block.
pub struct FeeData {
    pub block_number: u64,
    pub tx_index: u64,
    pub tx_hash: B256,
    pub l1_gas_cost: U256,
    pub tx_fee: u128,
}

impl OPZisKDataFetcher {
    /// Gets the RPC URL's and saves the rollup config for the chain to the rollup config file.
    pub fn new() -> Result<Self> {
        let rpc_config = get_rpcs_from_env()?;

        let l1_provider =
            Arc::new(ProviderBuilder::default().connect_http(rpc_config.l1_rpc.clone()));
        let l2_provider =
            Arc::new(ProviderBuilder::default().connect_http(rpc_config.l2_rpc.clone()));

        Ok(OPZisKDataFetcher {
            rpc_config,
            l1_provider,
            l2_provider,
            rollup_config: None,
            rollup_config_path: None,
            l1_config_path: None,
        })
    }

    /// Initialize the fetcher with a rollup config.
    pub async fn new_with_rollup_config() -> Result<Self> {
        let rpc_config = get_rpcs_from_env()?;
        Self::new_with_rpc_config_internal(rpc_config).await
    }

    /// Initialize the fetcher with a rollup config using the provided RPCConfig from op_zisk_config.
    /// This allows using RPCs discovered from DevnetManager API.
    pub async fn new_with_rpc_config(rpc_config: op_zisk_config::RPCConfig) -> Result<Self> {
        // Convert from op_zisk_config::RPCConfig to local RPCConfig
        let local_rpc_config = RPCConfig {
            l1_rpc: rpc_config.l1_rpc,
            l1_beacon_rpc: rpc_config.l1_beacon_rpc,
            l2_rpc: rpc_config.l2_rpc,
            l2_node_rpc: rpc_config.l2_node_rpc,
        };
        Self::new_with_rpc_config_internal(local_rpc_config).await
    }

    /// Internal helper to create fetcher with rollup config from local RPCConfig
    async fn new_with_rpc_config_internal(rpc_config: RPCConfig) -> Result<Self> {
        let l1_provider =
            Arc::new(ProviderBuilder::default().connect_http(rpc_config.l1_rpc.clone()));
        let l2_provider =
            Arc::new(ProviderBuilder::default().connect_http(rpc_config.l2_rpc.clone()));

        let (rollup_config, rollup_config_path) =
            Self::fetch_and_save_rollup_config(&rpc_config).await?;

        // Add warning if the chain is pre-Holocene, as derivation is significantly slower.
        let unix_timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        if !rollup_config.is_holocene_active(unix_timestamp) {
            tracing::warn!("Chain is not using Holocene hard fork. This will cause significant performance degradation compared to chains that have activated Holocene.");
        }

        // Fetch and save L1 config based on the rollup config's L1 chain ID
        let l1_config_path = Self::fetch_and_save_l1_config(&rollup_config).await?;

        Ok(OPZisKDataFetcher {
            rpc_config,
            l1_provider,
            l2_provider,
            rollup_config: Some(rollup_config),
            rollup_config_path: Some(rollup_config_path),
            l1_config_path: Some(l1_config_path),
        })
    }

    pub async fn get_l2_chain_id(&self) -> Result<u64> {
        Ok(self.l2_provider.get_chain_id().await?)
    }

    pub async fn get_l2_head(&self) -> Result<Header> {
        let block = retry_rpc(|| async {
            Ok(self
                .l2_provider
                .get_block_by_number(BlockNumberOrTag::Latest)
                .await?)
        })
        .await?;
        if let Some(block) = block {
            Ok(block.header.inner)
        } else {
            bail!("Failed to get L2 head");
        }
    }

    /// Get the aggregate block statistics for a range of blocks exclusive of the start block.
    ///
    /// When proving a range in OP-ZisK, we are proving the transition from the block hash
    /// of the start block to the block hash of the end block. This means that we don't expend
    /// resources to "prove" the start block. This is why the start block is not included in the
    /// range for which we fetch block data.
    pub async fn get_l2_block_data_range(&self, start: u64, end: u64) -> Result<Vec<BlockInfo>> {
        use futures::stream::{self, StreamExt};

        let block_data = stream::iter(start + 1..=end)
            .map(|block_number| async move {
                let block = retry_rpc(|| async {
                    Ok(self
                        .l2_provider
                        .get_block_by_number(block_number.into())
                        .await?
                        .unwrap())
                })
                .await?;
                let receipts = retry_rpc(|| async {
                    Ok(self
                        .l2_provider
                        .get_block_receipts(block_number.into())
                        .await?
                        .unwrap())
                })
                .await?;
                let total_l1_fees: u128 =
                    receipts.iter().map(|tx| tx.l1_block_info.l1_fee.unwrap_or(0)).sum();
                let total_tx_fees: u128 = receipts
                    .iter()
                    .map(|tx| {
                        // tx.inner.effective_gas_price * tx.inner.gas_used +
                        // tx.l1_block_info.l1_fee is the total fee for the transaction.
                        // tx.inner.effective_gas_price * tx.inner.gas_used is the tx fee on L2.
                        tx.inner.effective_gas_price * tx.inner.gas_used as u128 +
                            tx.l1_block_info.l1_fee.unwrap_or(0)
                    })
                    .sum();

                Ok(BlockInfo {
                    block_number,
                    transaction_count: block.transactions.len() as u64,
                    gas_used: block.header.gas_used,
                    total_l1_fees,
                    total_tx_fees,
                })
            })
            .buffered(rpc_concurrency())
            .collect::<Vec<Result<BlockInfo>>>()
            .await;

        block_data.into_iter().collect()
    }

    pub async fn get_l1_header(&self, block_number: BlockId) -> Result<Header> {
        let block = retry_rpc(|| async { Ok(self.l1_provider.get_block(block_number).await?) }).await?;

        if let Some(block) = block {
            Ok(block.header.inner)
        } else {
            bail!("Failed to get L1 header for block {block_number}");
        }
    }

    pub async fn get_l2_header(&self, block_number: BlockId) -> Result<Header> {
        let block = retry_rpc(|| async { Ok(self.l2_provider.get_block(block_number).await?) }).await?;

        if let Some(block) = block {
            Ok(block.header.inner)
        } else {
            // WORKAROUND: If eth_getBlockByNumber fails, try debug_getRawHeader
            // This works during snap sync pivot when blocks exist but aren't queryable via standard API
            let block_num_u64 = match block_number {
                BlockId::Number(BlockNumberOrTag::Number(n)) => {
                    // Convert BlockNumberOrTag::Number to u64
                    let n_str = format!("{n}");
                    n_str.parse::<u64>()
                        .map_err(|_| anyhow::anyhow!("Failed to convert block number to u64"))?
                }
                _ => bail!("Failed to get L2 header for block {block_number}: cannot use debug fallback for non-numeric block IDs"),
            };
            
            // Use get_l2_block_by_number which uses debug_getRawBlock and returns OpBlock
            // This works during snap sync pivot when blocks exist but aren't queryable via standard API
            let block = self.get_l2_block_by_number(block_num_u64).await?;
            Ok(block.header)
        }
    }

    async fn ensure_l2_execution_ready_for_block(&self, l2_block: u64) -> Result<()> {
        // During op-geth snap sync pivot (and some restart states), RPC can return blockNumber=0 and
        // "latest" can resolve to genesis. Proving requires:
        // - canonical blocks available for the target block
        // - eth_getProof available for that block (account proof at minimum)
        // - debug_getRawBlock available (used by Kona witness generation)
        let latest = retry_rpc(|| async {
            Ok(self
                .l2_provider
                .get_block_by_number(BlockNumberOrTag::Latest)
                .await?)
        })
        .await?;

        if let Some(latest) = latest {
            if latest.header.number == 0 {
                // TEMPORARY: Allow testing if the target block exists, even if latest is still 0
                // This is a workaround for snap sync pivot - blocks may exist before pivot completes
                let target_block_exists = retry_rpc(|| async {
                    Ok(self.l2_provider.get_block(l2_block.into()).await?.is_some())
                })
                .await?;
                
                if !target_block_exists {
                    bail!(
                        "L2 execution RPC is not ready: eth_getBlockByNumber(\"latest\") returned genesis (block 0). \
                         This typically means op-geth snap sync pivot/state healing has not completed yet. \
                         Wait until eth_blockNumber is non-zero and latest returns a real head."
                    );
                }
                // Continue if target block exists, even though latest is 0
            }
        } else {
            bail!(
                "L2 execution RPC is not ready: eth_getBlockByNumber(\"latest\") returned null. \
                 Ensure op-geth is running and reachable."
            );
        }

        // Try to get block via standard API first
        let block = retry_rpc(|| async { Ok(self.l2_provider.get_block(l2_block.into()).await?) }).await?;
        if block.is_none() {
            // WORKAROUND: If eth_getBlockByNumber fails, try debug_getRawBlock as fallback
            // This works during snap sync pivot when blocks exist but aren't queryable via standard API
            let raw_block_res: Result<Bytes> = self
                .l2_provider
                .raw_request("debug_getRawBlock".into(), [U64::from(l2_block)])
                .await
                .map_err(Into::into);
            
            match raw_block_res {
                Ok(_) => {
                    // Block exists via debug API, continue despite eth_getBlockByNumber failing
                    // This is expected during snap sync pivot
                }
                Err(e) => {
                    bail!(
                        "L2 execution RPC is not ready for block {l2_block}: block not found via eth_getBlockByNumber or debug_getRawBlock. \
                         If op-geth is still syncing, wait for it to finish snap sync pivot/state healing. Error: {e:#}"
                    );
                }
            }
        }

        // Probe eth_getProof at the OutputOracle address; used later to compute storage hash.
        let output_oracle = Address::from_str("0x4200000000000000000000000000000000000016")
            .expect("OutputOracle address must be valid");
        let proof_res = retry_rpc(|| async {
            self.l2_provider
                .get_proof(output_oracle, Vec::new())
                .block_id(l2_block.into())
                .await
                .map_err(Into::into)
        })
        .await;

        // Check if eth_getProof failed due to proof window restrictions
        let proof_res = match proof_res {
            Ok(proof) => Ok(proof),
            Err(e) => {
                let error_str = format!("{e:#}");
                // If it's a proof window error, we can't proceed - this is a hard requirement
                if error_str.contains("proof window") || error_str.contains("distance to target block") {
                    bail!(
                        "L2 execution RPC cannot serve eth_getProof for block {l2_block} due to proof window restrictions. \
                         Public RPCs often limit eth_getProof to recent blocks only. \
                         To prove this block, you need: \
                         (1) A self-hosted L2 node with full state, or \
                         (2) An archive L2 node, or \
                         (3) Wait for the block to be within the RPC's proof window (if applicable). \
                         Error: {e:#}"
                    );
                }
                Err(e)
            }
        };
        
        if let Err(e) = proof_res {
            if is_historical_state_unavailable_opaque(&e) {
                bail!(
                    "L2 execution RPC cannot serve eth_getProof for block {l2_block}. \
                     This is required for proving. If your L2 node is a full (pruned) node, it may only be able \
                     to serve eth_getProof for recent blocks; proving older blocks requires an archive L2 node. \
                     Under snap sync pivot/state healing, eth_getProof may also be temporarily unavailable: {e:#}"
                );
            }
            return Err(e);
        }

        // Probe debug_getRawBlock, required by the Kona witness path.
        let raw_block_res: Result<Bytes> = self
            .l2_provider
            .raw_request("debug_getRawBlock".into(), [U64::from(l2_block)])
            .await
            .map_err(Into::into);
        match raw_block_res {
            Ok(_) => Ok(()),
            Err(e) => {
                if is_method_not_found_opaque(&e) {
                    bail!(
                        "L2 execution RPC does not expose debug_getRawBlock, which is required for witness generation. \
                         Ensure op-geth is started with the debug API enabled on its HTTP RPC (e.g., include 'debug' in --http.api). \
                         Under Docker-based OP node setups, this usually means adjusting the op-geth flags."
                    );
                }
                Err(e)
            }
        }
    }

    async fn ensure_l2_node_ready_for_block(&self, l2_block: u64) -> Result<()> {
        // Probing optimism_outputAtBlock early gives a clearer error when op-node is down/unreachable.
        // TEMPORARY WORKAROUND: Skip this check if op-node can't serve the block yet (for testing during sync)
        let l2_block_hex = format!("0x{l2_block:x}");
        let res: Result<OutputResponse> = self
            .fetch_rpc_data_with_mode(RPCMode::L2Node, "optimism_outputAtBlock", vec![l2_block_hex.into()])
            .await;
        match res {
            Ok(_) => Ok(()),
            Err(e) => {
                // WORKAROUND: If the block exists in op-geth, allow testing even if op-node can't serve it yet
                let block_exists = retry_rpc(|| async {
                    Ok(self.l2_provider.get_block(l2_block.into()).await?.is_some())
                })
                .await?;
                
                if block_exists {
                    // Block exists in op-geth, allow testing (op-node will catch up)
                    tracing::warn!(
                        "op-node cannot serve optimism_outputAtBlock for block {l2_block} yet, but block exists in op-geth. \
                         Continuing with witness generation (op-node may catch up during sync). Error: {e:#}"
                    );
                    Ok(())
                } else {
                    bail!(
                        "L2 node RPC is not ready for block {l2_block}: optimism_outputAtBlock failed. \
                         Ensure op-node is running and reachable at L2_NODE_RPC. Error: {e:#}"
                    )
                }
            }
        }
    }

    /// Finds the L1 block at the provided timestamp.
    pub async fn find_l1_block_by_timestamp(&self, target_timestamp: u64) -> Result<(B256, u64)> {
        self.find_block_by_timestamp(&self.l1_provider, target_timestamp).await
    }

    /// Finds the L2 block at the provided timestamp.
    pub async fn find_l2_block_by_timestamp(&self, target_timestamp: u64) -> Result<(B256, u64)> {
        self.find_block_by_timestamp(&self.l2_provider, target_timestamp).await
    }

    /// Finds the block at the provided timestamp, using the provided provider.
    async fn find_block_by_timestamp<N>(
        &self,
        provider: &RootProvider<N>,
        target_timestamp: u64,
    ) -> Result<(B256, u64)>
    where
        N: Network,
    {
        let latest_block =
            retry_rpc(|| async { Ok(provider.get_block(BlockId::finalized()).await?) }).await?;
        let mut low = 0;
        let mut high = if let Some(block) = latest_block {
            block.header().number()
        } else {
            bail!("Failed to get latest block");
        };

        while low <= high {
            let mid = (low + high) / 2;
            let block = retry_rpc(|| async { Ok(provider.get_block(mid.into()).await?) }).await?;
            if let Some(block) = block {
                let block_timestamp = block.header().timestamp();

                match block_timestamp.cmp(&target_timestamp) {
                    Ordering::Equal => {
                        return Ok((block.header().hash().0.into(), block.header().number()));
                    }
                    Ordering::Less => low = mid + 1,
                    Ordering::Greater => high = mid - 1,
                }
            } else {
                bail!("Failed to get block for block {mid}");
            }
        }

        // Return the block hash of the closest block after the target timestamp
        let block = provider.get_block(low.into()).await?;
        if let Some(block) = block {
            Ok((block.header().hash().0.into(), block.header().number()))
        } else {
            bail!("Failed to get block for block {low}");
        }
    }

    /// Get the RPC URL for the given RPC mode.
    pub fn get_rpc_url(&self, rpc_mode: RPCMode) -> Result<&Url> {
        match rpc_mode {
            RPCMode::L1 => Ok(&self.rpc_config.l1_rpc),
            RPCMode::L2 => Ok(&self.rpc_config.l2_rpc),
            RPCMode::L1Beacon => self
                .rpc_config
                .l1_beacon_rpc
                .as_ref()
                .ok_or_else(|| anyhow!("L1 beacon RPC URL is not set")),
            RPCMode::L2Node => Ok(&self.rpc_config.l2_node_rpc),
        }
    }

    /// Fetch and save the rollup config to a temporary file.
    async fn fetch_and_save_rollup_config(
        rpc_config: &RPCConfig,
    ) -> Result<(RollupConfig, PathBuf)> {
        // Create configs directory if it doesn't exist
        let default_dir = PathBuf::from("configs/L2");
        let l2_config_dir = env::var("L2_CONFIG_DIR").map(PathBuf::from).unwrap_or(default_dir);
        fs::create_dir_all(&l2_config_dir)?;

        // Try to fetch from RPC first
        match Self::fetch_rpc_data::<RollupConfig>(&rpc_config.l2_node_rpc, "optimism_rollupConfig", vec![]).await {
            Ok(rollup_config) => {
                // Save rollup config to a file named by chain ID
                let rollup_config_path = l2_config_dir.join(format!("{}.json", rollup_config.l2_chain_id));
                let rollup_config_str = serde_json::to_string_pretty(&rollup_config)?;
                fs::write(&rollup_config_path, rollup_config_str)?;
                tracing::info!(
                    "Saved L2 config for chain ID {} to {}",
                    rollup_config.l2_chain_id,
                    rollup_config_path.display()
                );
                Ok((rollup_config, rollup_config_path))
            }
            Err(e) => {
                // Fallback: Try to load from existing config file
                // First, try to get the chain ID from L2_RPC
                let chain_id = match Self::fetch_rpc_data::<U64>(&rpc_config.l2_rpc, "eth_chainId", vec![]).await {
                    Ok(chain_id_u64) => Some(chain_id_u64.to::<u64>()),
                    Err(_) => None,
                };

                // If we couldn't get chain ID from RPC, try common devnet chain IDs
                let chain_ids_to_try = if let Some(cid) = chain_id {
                    vec![cid]
                } else {
                    vec![901u64, 900u64, 420u64, 10u64] // Common devnet/testnet chain IDs
                };

                for cid in chain_ids_to_try {
                    let rollup_config_path = l2_config_dir.join(format!("{}.json", cid));
                    if rollup_config_path.exists() {
                        tracing::warn!(
                            "Failed to fetch rollup config from RPC ({}), loading from existing file: {}",
                            e,
                            rollup_config_path.display()
                        );
                        let file = fs::File::open(&rollup_config_path)?;
                        let rollup_config: RollupConfig = serde_json::from_reader(file)?;
                        return Ok((rollup_config, rollup_config_path));
                    }
                }

                Err(anyhow::anyhow!(
                    "Failed to fetch rollup config from RPC ({}) and no config file found in: {}",
                    e,
                    l2_config_dir.display()
                ))
            }
        }
    }

    /// Fetch and save the L1 config based on the rollup config's L1 chain ID.
    async fn fetch_and_save_l1_config(rollup_config: &RollupConfig) -> Result<PathBuf> {
        let default_dir = PathBuf::from("configs/L1");
        let l1_config_dir = env::var("L1_CONFIG_DIR").map(PathBuf::from).unwrap_or(default_dir);

        // Check if the L1 config file exists. If it does, return the path to the file.
        let l1_config_path = l1_config_dir.join(format!("{}.json", rollup_config.l1_chain_id));
        if l1_config_path.exists() {
            tracing::info!(
                "L1 config for chain ID {} already exists at {}",
                rollup_config.l1_chain_id,
                l1_config_path.display()
            );

            let file = fs::File::open(&l1_config_path)?;
            let l1_config: Value = serde_json::from_reader(file)?;
            tracing::debug!(
                "Loaded L1 config for chain ID {} from file: {:?}",
                rollup_config.l1_chain_id,
                l1_config
            );

            return Ok(l1_config_path);
        }

        // Lookup the L1 config from the registry.
        let l1_config = L1_CONFIGS.get(&rollup_config.l1_chain_id).ok_or_else(|| {
            anyhow::anyhow!(
                "No built-in L1 config exists for chain ID {}.\n\
                 To proceed, either:\n\
                 • Create a config file at: {}\n\
                 • Or set L1_CONFIG_DIR to the directory containing <chain_id>.json",
                rollup_config.l1_chain_id,
                l1_config_path.display()
            )
        })?;

        tracing::debug!(
            "Fetched L1 config for chain ID {} from registry: {:?}",
            rollup_config.l1_chain_id,
            l1_config
        );

        // Create the L1 config directory if it doesn't exist.
        fs::create_dir_all(&l1_config_dir)
            .with_context(|| format!("creating {}", l1_config_dir.display()))?;

        // Write the L1 config to the file
        let l1_config_str = serde_json::to_string_pretty(l1_config)?;
        fs::write(&l1_config_path, l1_config_str)?;

        tracing::info!(
            "Saved L1 config for chain ID {} to {}",
            rollup_config.l1_chain_id,
            l1_config_path.display()
        );

        Ok(l1_config_path)
    }

    async fn fetch_rpc_data<T>(url: &Url, method: &str, params: Vec<Value>) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let client = reqwest::Client::new();
        let response = client
            .post(url.clone())
            .json(&json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
                "id": 1
            }))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        // Check for RPC error from the JSON RPC response.
        if let Some(error) = response.get("error") {
            let error_message = error["message"].as_str().unwrap_or("Unknown error");
            return Err(anyhow::anyhow!("Error calling {method}: {error_message}"));
        }

        serde_json::from_value(response["result"].clone()).map_err(Into::into)
    }

    /// Fetch arbitrary data from the RPC.
    pub async fn fetch_rpc_data_with_mode<T>(
        &self,
        rpc_mode: RPCMode,
        method: &str,
        params: Vec<Value>,
    ) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let url = self.get_rpc_url(rpc_mode)?;
        Self::fetch_rpc_data(url, method, params).await
    }

    /// Get the earliest L1 header in a batch of boot infos.
    pub async fn get_earliest_l1_head_in_batch(
        &self,
        boot_infos: &Vec<BootInfoStruct>,
    ) -> Result<Header> {
        let mut earliest_block_num: u64 = u64::MAX;
        let mut earliest_l1_header: Option<Header> = None;

        for boot_info in boot_infos {
            let l1_block_header = self.get_l1_header(boot_info.l1Head.into()).await?;
            if l1_block_header.number < earliest_block_num {
                earliest_block_num = l1_block_header.number;
                earliest_l1_header = Some(l1_block_header);
            }
        }
        Ok(earliest_l1_header.unwrap())
    }

    /// Get the latest L1 header in a batch of boot infos.
    pub async fn get_latest_l1_head_in_batch(
        &self,
        boot_infos: &Vec<BootInfoStruct>,
    ) -> Result<Header> {
        let mut latest_block_num: u64 = u64::MIN;
        let mut latest_l1_header: Option<Header> = None;

        for boot_info in boot_infos {
            let l1_block_header = self.get_l1_header(boot_info.l1Head.into()).await?;
            if l1_block_header.number > latest_block_num {
                latest_block_num = l1_block_header.number;
                latest_l1_header = Some(l1_block_header);
            }
        }
        if let Some(header) = latest_l1_header {
            Ok(header)
        } else {
            bail!("Failed to get latest L1 header");
        }
    }

    /// Fetch headers for a range of blocks inclusive.
    pub async fn fetch_headers_in_range(&self, start: u64, end: u64) -> Result<Vec<Header>> {
        let block_numbers: Vec<u64> = (start..=end).collect();
        let mut headers = Vec::new();

        // Process blocks in batches of 10, but maintain original order
        let results = stream::iter(block_numbers)
            .map(|block_number| self.get_l1_header(block_number.into()))
            .buffered(rpc_concurrency())
            .collect::<Vec<_>>()
            .await;

        for result in results {
            headers.push(result?);
        }

        Ok(headers)
    }

    /// Get the preimages for the headers corresponding to the boot infos. Specifically, fetch the
    /// headers corresponding to the boot infos and the latest L1 head.
    pub async fn get_header_preimages(
        &self,
        boot_infos: &Vec<BootInfoStruct>,
        checkpoint_block_hash: B256,
    ) -> Result<Vec<Header>> {
        // Get the earliest L1 Head from the boot_infos.
        let start_header = self.get_earliest_l1_head_in_batch(boot_infos).await?;

        // Fetch the full header for the latest L1 Head (which is validated on chain).
        let latest_header = self.get_l1_header(checkpoint_block_hash.into()).await?;

        // Create a vector of futures for fetching all headers
        let headers =
            self.fetch_headers_in_range(start_header.number, latest_header.number).await?;

        Ok(headers)
    }

    pub async fn get_l2_output_at_block(&self, block_number: u64) -> Result<OutputResponse> {
        let block_number_hex = format!("0x{block_number:x}");
        let l2_output_data: OutputResponse = self
            .fetch_rpc_data_with_mode(
                RPCMode::L2Node,
                "optimism_outputAtBlock",
                vec![block_number_hex.into()],
            )
            .await?;
        Ok(l2_output_data)
    }

    /// Get the L1 block from which the `l2_end_block` can be derived.
    ///
    /// Use binary search to find the first L1 block with an L2 safe head >= l2_end_block.
    pub async fn get_safe_l1_block_for_l2_block(&self, l2_end_block: u64) -> Result<(B256, u64)> {
        let latest_l1_header = self.get_l1_header(BlockId::finalized()).await?;

        // Get the l1 origin of the l2 end block.
        let l2_end_block_hex = format!("0x{l2_end_block:x}");
        let optimism_output_data: Result<OutputResponse> = self
            .fetch_rpc_data_with_mode(
                RPCMode::L2Node,
                "optimism_outputAtBlock",
                vec![l2_end_block_hex.into()],
            )
            .await;

        // FALLBACK: If optimism_outputAtBlock is not available (e.g., public RPCs),
        // return an error to trigger the timestamp-based fallback in get_l1_head().
        let optimism_output_data = match optimism_output_data {
            Ok(data) => data,
            Err(e) => {
                // Check if this is a "method not found", "proof window", or similar error
                let error_str = format!("{e:#}");
                if is_method_not_found_str(&error_str) 
                    || is_historical_state_unavailable_str(&error_str)
                    || error_str.contains("proof window")
                    || error_str.contains("distance to target block") {
                    // Return a specific error that get_l1_head() will catch and use fallback
                    return Err(anyhow::anyhow!(
                        "optimism_outputAtBlock not available (RPC method not supported or proof window exceeded). \
                         This will trigger timestamp-based fallback if SAFE_DB_FALLBACK is enabled."
                    ));
                }
                return Err(e);
            }
        };

        let l1_origin = optimism_output_data.block_ref.l1_origin;

        // Binary search for the first L1 block with L2 safe head >= l2_end_block.
        let mut low = l1_origin.number;
        let mut high = latest_l1_header.number;
        let mut first_valid = None;

        while low <= high {
            let mid = low + (high - low) / 2;
            let l1_block_number_hex = format!("0x{mid:x}");
            let result: Result<SafeHeadResponse> = self
                .fetch_rpc_data_with_mode(
                    RPCMode::L2Node,
                    "optimism_safeHeadAtL1Block",
                    vec![l1_block_number_hex.into()],
                )
                .await;
            
            // FALLBACK: If optimism_safeHeadAtL1Block is not available, return error to trigger fallback
            let result = match result {
                Ok(data) => data,
                Err(e) => {
                    let error_str = format!("{e:#}");
                    if is_method_not_found_str(&error_str) 
                        || is_historical_state_unavailable_str(&error_str)
                        || error_str.contains("proof window")
                        || error_str.contains("distance to target block") {
                        return Err(anyhow::anyhow!(
                            "optimism_safeHeadAtL1Block not available (RPC method not supported or proof window exceeded). \
                             This will trigger timestamp-based fallback if SAFE_DB_FALLBACK is enabled."
                        ));
                    }
                    return Err(e);
                }
            };
            
            let l2_safe_head = result.safe_head.number;

            if l2_safe_head >= l2_end_block {
                // Found a valid block, save it and keep searching lower.
                first_valid = Some((result.l1_block.hash, result.l1_block.number));
                high = mid - 1;
            } else {
                // Need to search higher
                low = mid + 1;
            }
        }

        first_valid.ok_or_else(|| {
            anyhow::anyhow!(
                "Could not find an L1 block with an L2 safe head greater than the L2 end block."
            )
        })
    }

    /// If the safeDB is activated, use it to fetch the L1 block where the batch including the data
    /// for the end L2 block was posted. If the safeDB is not activated:
    ///   - If `safe_db_fallback` is `true`, estimate the L1 head based on the L2 block timestamp.
    ///   - Else, return an error.
    pub async fn get_l1_head(
        &self,
        l2_end_block: u64,
        safe_db_fallback: bool,
    ) -> Result<(B256, u64)> {
        if self.rollup_config.is_none() {
            return Err(anyhow::anyhow!("Rollup config not loaded."));
        }

        match self.get_safe_l1_block_for_l2_block(l2_end_block).await {
            Ok(safe_head) => Ok(safe_head),
            Err(e) => {
                if safe_db_fallback {
                    tracing::warn!("SafeDB not activated - falling back to timestamp-based L1 head estimation. WARNING: This fallback method is more expensive and less reliable. Derivation may fail if the L2 block batch is posted after our estimated L1 head. Enable SafeDB on op-node to fix this.");
                    // Fallback: estimate L1 block based on timestamp
                    let max_batch_post_delay_minutes = 40;
                    let l2_block_timestamp =
                        self.get_l2_header(l2_end_block.into()).await?.timestamp;
                    let finalized_l1_timestamp =
                        self.get_l1_header(BlockId::finalized()).await?.timestamp;

                    let target_timestamp = min(
                        l2_block_timestamp + (max_batch_post_delay_minutes * 60),
                        finalized_l1_timestamp,
                    );
                    self.find_l1_block_by_timestamp(target_timestamp).await
                } else {
                    Err(anyhow::anyhow!(
                        "SafeDB is not activated on your op-node and the `SAFE_DB_FALLBACK` flag is set to false. Please enable the safeDB on your op-node to fix this, or set `SAFE_DB_FALLBACK` flag to true, which will be more expensive: {}",
                        e
                    ))
                }
            }
        }
    }

    // Source from: https://github.com/anton-rs/kona/blob/85b1c88b44e5f54edfc92c781a313717bad5dfc7/crates/derive-alloy/src/alloy_providers.rs#L225.
    pub async fn get_l2_block_by_number(&self, block_number: u64) -> Result<OpBlock> {
        let raw_block: Bytes = self
            .l2_provider
            .raw_request("debug_getRawBlock".into(), [U64::from(block_number)])
            .await?;
        let block = OpBlock::decode(&mut raw_block.as_ref()).map_err(|e| anyhow::anyhow!(e))?;
        Ok(block)
    }

    pub async fn l2_block_info_by_number(&self, block_number: u64) -> Result<L2BlockInfo> {
        // If the rollup config is not already loaded, fetch and save it.
        if self.rollup_config.is_none() {
            return Err(anyhow::anyhow!("Rollup config not loaded."));
        }
        let genesis = self.rollup_config.as_ref().unwrap().genesis;
        let block = self.get_l2_block_by_number(block_number).await?;
        Ok(L2BlockInfo::from_block_and_genesis(&block, &genesis)?)
    }

    /// Get the L2 safe head corresponding to the L1 block number using optimism_safeHeadAtL1Block.
    pub async fn get_l2_safe_head_from_l1_block_number(&self, l1_block_number: u64) -> Result<u64> {
        let l1_block_number_hex = format!("0x{l1_block_number:x}");
        let result: SafeHeadResponse = self
            .fetch_rpc_data_with_mode(
                RPCMode::L2Node,
                "optimism_safeHeadAtL1Block",
                vec![l1_block_number_hex.into()],
            )
            .await?;
        Ok(result.safe_head.number)
    }

    /// Check if the safeDB is activated on the L2 node.
    pub async fn is_safe_db_activated(&self) -> Result<bool> {
        let finalized_l1_header = self.get_l1_header(BlockId::finalized()).await?;
        let l1_block_number_hex = format!("0x{:x}", finalized_l1_header.number);
        let result: Result<SafeHeadResponse, _> = self
            .fetch_rpc_data_with_mode(
                RPCMode::L2Node,
                "optimism_safeHeadAtL1Block",
                vec![l1_block_number_hex.into()],
            )
            .await;
        Ok(result.is_ok())
    }

    /// Get the L2 output data for a given block number and save the boot info to a file in the data
    /// directory with block_number. Return the arguments to be passed to the native host for
    /// datagen.
    pub async fn get_host_args(
        &self,
        l2_start_block: u64,
        l2_end_block: u64,
        l1_head_hash: B256,
    ) -> Result<SingleChainHost> {
        // If the rollup config is not already loaded, fetch and save it.
        if self.rollup_config.is_none() {
            return Err(anyhow::anyhow!("Rollup config not loaded."));
        }

        if l2_start_block >= l2_end_block {
            return Err(anyhow::anyhow!(
                "L2 start block is greater than or equal to L2 end block. Start: {}, End: {}",
                l2_start_block,
                l2_end_block
            ));
        }

        // Fail fast with an actionable error message if the RPCs are not ready for proving.
        self.ensure_l2_execution_ready_for_block(l2_end_block).await?;
        self.ensure_l2_node_ready_for_block(l2_end_block).await?;

        let l2_provider = self.l2_provider.clone();

        // Get L2 output data.
        // IMPORTANT: The safe head must be BEFORE l2_start_block so derivation can derive
        // blocks starting from l2_start_block. Use l2_start_block - 1 (or genesis if start is 1).
        let safe_head_block = if l2_start_block > 1 {
            l2_start_block - 1
        } else {
            0 // Use genesis as safe head if start is block 1
        };
        
        let l2_output_block = retry_rpc(|| async {
            Ok(l2_provider.get_block_by_number(safe_head_block.into()).await?)
        })
        .await?
        .ok_or_else(|| anyhow::anyhow!("Block not found for block number {}", safe_head_block))?;
        let l2_output_state_root = l2_output_block.header.state_root;
        let agreed_l2_head_hash = l2_output_block.header.hash;
        let l2_output_storage_hash = retry_rpc(|| async {
            Ok(l2_provider
                .get_proof(
                    Address::from_str("0x4200000000000000000000000000000000000016")?,
                    Vec::new(),
                )
            .block_id(safe_head_block.into())
            .await?
                .storage_hash)
        })
        .await?;

        let l2_output_encoded = L2Output {
            zero: 0,
            l2_state_root: l2_output_state_root.0.into(),
            l2_storage_hash: l2_output_storage_hash.0.into(),
            l2_claim_hash: agreed_l2_head_hash.0.into(),
        };
        let agreed_l2_output_root = keccak256(l2_output_encoded.abi_encode());

        tracing::info!("=========================================");
        tracing::info!("L2 OUTPUT BLOCK (Start - 1):");
        tracing::info!("  Block Number: {}", safe_head_block);
        tracing::info!("  Block Hash: 0x{}", alloy_primitives::hex::encode(agreed_l2_head_hash.as_slice()));
        tracing::info!("  State Root: 0x{}", alloy_primitives::hex::encode(l2_output_state_root.as_slice()));
        tracing::info!("  Storage Hash: 0x{}", alloy_primitives::hex::encode(l2_output_storage_hash.as_slice()));
        tracing::info!("  Output Root: 0x{}", alloy_primitives::hex::encode(agreed_l2_output_root.as_slice()));
        tracing::info!("=========================================");

        // Get L2 claim data.
        let l2_claim_block = retry_rpc(|| async {
            Ok(l2_provider.get_block_by_number(l2_end_block.into()).await?)
        })
        .await?
        .ok_or_else(|| anyhow::anyhow!("Block not found for block number {}", l2_end_block))?;
        let l2_claim_state_root = l2_claim_block.header.state_root;
        let l2_claim_hash = l2_claim_block.header.hash;
        let l2_claim_storage_hash = retry_rpc(|| async {
            Ok(l2_provider
                .get_proof(
                    Address::from_str("0x4200000000000000000000000000000000000016")?,
                    Vec::new(),
                )
            .block_id(l2_end_block.into())
            .await?
                .storage_hash)
        })
        .await?;

        let l2_claim_encoded = L2Output {
            zero: 0,
            l2_state_root: l2_claim_state_root.0.into(),
            l2_storage_hash: l2_claim_storage_hash.0.into(),
            l2_claim_hash: l2_claim_hash.0.into(),
        };
        let claimed_l2_output_root = keccak256(l2_claim_encoded.abi_encode());

        tracing::info!("=========================================");
        tracing::info!("L2 CLAIM BLOCK (End):");
        tracing::info!("  Block Number: {}", l2_end_block);
        tracing::info!("  Block Hash: 0x{}", alloy_primitives::hex::encode(l2_claim_hash.as_slice()));
        tracing::info!("  State Root: 0x{}", alloy_primitives::hex::encode(l2_claim_state_root.as_slice()));
        tracing::info!("  Storage Hash: 0x{}", alloy_primitives::hex::encode(l2_claim_storage_hash.as_slice()));
        tracing::info!("  Output Root: 0x{}", alloy_primitives::hex::encode(claimed_l2_output_root.as_slice()));
        tracing::info!("=========================================");
        tracing::info!("L1 Head Hash: 0x{}", alloy_primitives::hex::encode(l1_head_hash.as_slice()));
        tracing::info!("=========================================");

        let l1_beacon_address = self
            .rpc_config
            .l1_beacon_rpc
            .as_ref()
            .map(|addr| addr.as_str().trim_end_matches('/').to_string());

        Ok(SingleChainHost {
            l1_head: l1_head_hash,
            agreed_l2_output_root,
            agreed_l2_head_hash,
            claimed_l2_output_root,
            claimed_l2_block_number: l2_end_block,
            l2_chain_id: None,
            // Trim the trailing slash to avoid double slashes in the URL.
            l2_node_address: Some(
                self.rpc_config.l2_rpc.as_str().trim_end_matches('/').to_string(),
            ),
            l1_node_address: Some(
                self.rpc_config.l1_rpc.as_str().trim_end_matches('/').to_string(),
            ),
            l1_beacon_address,
            data_dir: None, // Use in-memory key-value store.
            native: false,
            server: true,
            rollup_config_path: self.rollup_config_path.clone(),
            l1_config_path: self.l1_config_path.clone(),
            enable_experimental_witness_endpoint: false,
        })
    }
}
