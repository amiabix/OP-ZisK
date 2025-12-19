use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;
use url::Url;

/// Unified configuration for OP-ZisK proof generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofConfig {
    pub rpc: RPCConfig,
    pub block_range: Option<BlockRangeConfig>,
    pub zisk: ZiskConfig,
    pub paths: PathConfig,
}

/// RPC endpoint configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RPCConfig {
    pub l1_rpc: Url,
    pub l2_rpc: Url,
    pub l2_node_rpc: Url,
    pub l1_beacon_rpc: Option<Url>,
}

/// Block range configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockRangeConfig {
    pub start: u64,
    pub end: u64,
}

/// ZisK-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZiskConfig {
    pub range_elf_path: Option<PathBuf>,
    pub proving_key_path: Option<PathBuf>,
    pub witness_lib_path: Option<PathBuf>,
    pub verify_proofs: bool,
}

/// Path configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathConfig {
    pub l1_config_dir: Option<PathBuf>,
    pub l2_config_dir: Option<PathBuf>,
}

/// Health status of RPC endpoints
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub l1_available: bool,
    pub l2_available: bool,
    pub l2_node_available: bool,
    pub l1_beacon_available: bool,
    pub l2_block_number: Option<u64>,
    pub l1_block_number: Option<u64>,
}

impl ProofConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self> {
        let rpc = RPCConfig::from_env()?;
        let zisk = ZiskConfig::from_env()?;
        let paths = PathConfig::from_env()?;

        Ok(ProofConfig {
            rpc,
            block_range: None,
            zisk,
            paths,
        })
    }

    /// Load configuration from a file
    pub fn from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let config: ProofConfig = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
        Ok(config)
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<()> {
        // Validate RPC URLs
        if self.rpc.l1_rpc.scheme() != "http" && self.rpc.l1_rpc.scheme() != "https" {
            anyhow::bail!("L1_RPC must be an HTTP/HTTPS URL, got: {}", self.rpc.l1_rpc);
        }
        if self.rpc.l2_rpc.scheme() != "http" && self.rpc.l2_rpc.scheme() != "https" {
            anyhow::bail!("L2_RPC must be an HTTP/HTTPS URL, got: {}", self.rpc.l2_rpc);
        }
        if self.rpc.l2_node_rpc.scheme() != "http" && self.rpc.l2_node_rpc.scheme() != "https" {
            anyhow::bail!("L2_NODE_RPC must be an HTTP/HTTPS URL, got: {}", self.rpc.l2_node_rpc);
        }
        if let Some(ref beacon) = self.rpc.l1_beacon_rpc {
            if beacon.scheme() != "http" && beacon.scheme() != "https" {
                anyhow::bail!("L1_BEACON_RPC must be an HTTP/HTTPS URL, got: {}", beacon);
            }
        }

        // Validate block range if provided
        if let Some(ref range) = self.block_range {
            if range.start >= range.end {
                anyhow::bail!("Block range invalid: start ({}) must be less than end ({})", range.start, range.end);
            }
        }

        Ok(())
    }

    /// Perform health check on RPC endpoints
    pub async fn health_check(&self) -> Result<HealthStatus> {
        use alloy_provider::{Provider, ProviderBuilder};
        use alloy_network::Ethereum;
        use op_alloy_network::Optimism;

        let mut status = HealthStatus {
            l1_available: false,
            l2_available: false,
            l2_node_available: false,
            l1_beacon_available: false,
            l2_block_number: None,
            l1_block_number: None,
        };

        // Check L1 RPC
        let l1_provider: alloy_provider::RootProvider<Ethereum> = ProviderBuilder::default()
            .connect_http(self.rpc.l1_rpc.clone());
        match l1_provider.get_block_number().await {
            Ok(block_num) => {
                status.l1_available = true;
                // get_block_number returns u64 directly
                status.l1_block_number = Some(block_num);
            }
            Err(e) => {
                tracing::warn!("L1 RPC health check failed: {}", e);
            }
        }

        // Check L2 RPC
        let l2_provider: alloy_provider::RootProvider<Optimism> = ProviderBuilder::default()
            .connect_http(self.rpc.l2_rpc.clone());
        match l2_provider.get_block_number().await {
            Ok(block_num) => {
                status.l2_available = true;
                // get_block_number returns u64 directly
                status.l2_block_number = Some(block_num);
            }
            Err(e) => {
                tracing::warn!("L2 RPC health check failed: {}", e);
            }
        }

        // Check L2 Node RPC (op-node uses custom RPC methods, not standard Ethereum RPC)
        // Try calling optimism_rollupConfig to verify op-node is available
        let client = reqwest::Client::new();
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "optimism_rollupConfig",
            "params": [],
            "id": 1
        });
        match client
            .post(self.rpc.l2_node_rpc.clone())
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                // Check if response contains an error about method not found
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if json.get("error").is_none() {
                        status.l2_node_available = true;
                    } else {
                        tracing::warn!("L2 Node RPC returned error: {:?}", json.get("error"));
                    }
                } else {
                    status.l2_node_available = true; // Assume available if we got a response
                }
            }
            Ok(_) => {
                tracing::warn!("L2 Node RPC returned non-success status");
            }
            Err(e) => {
                tracing::warn!("L2 Node RPC health check failed: {}", e);
            }
        }

        // Check L1 Beacon RPC (optional)
        if let Some(ref beacon) = self.rpc.l1_beacon_rpc {
            let client = reqwest::Client::new();
            match client.get(beacon.clone()).send().await {
                Ok(resp) if resp.status().is_success() => {
                    status.l1_beacon_available = true;
                }
                Ok(_) => {
                    tracing::warn!("L1 Beacon RPC returned non-success status");
                }
                Err(e) => {
                    tracing::warn!("L1 Beacon RPC health check failed: {}", e);
                }
            }
        }

        Ok(status)
    }
}

impl RPCConfig {
    /// Load RPC configuration from environment variables
    pub fn from_env() -> Result<Self> {
        let l1_rpc = std::env::var("L1_RPC")
            .context("L1_RPC environment variable is required for proof generation")?;
        let l2_rpc = std::env::var("L2_RPC")
            .context("L2_RPC environment variable is required for proof generation")?;
        let l2_node_rpc = std::env::var("L2_NODE_RPC")
            .context("L2_NODE_RPC environment variable is required for proof generation")?;
        let l1_beacon_rpc = std::env::var("L1_BEACON_RPC").ok();

        Ok(RPCConfig {
            l1_rpc: Url::parse(&l1_rpc)
                .with_context(|| format!("L1_RPC must be a valid URL, got: {}", l1_rpc))?,
            l2_rpc: Url::parse(&l2_rpc)
                .with_context(|| format!("L2_RPC must be a valid URL, got: {}", l2_rpc))?,
            l2_node_rpc: Url::parse(&l2_node_rpc)
                .with_context(|| format!("L2_NODE_RPC must be a valid URL, got: {}", l2_node_rpc))?,
            l1_beacon_rpc: l1_beacon_rpc
                .map(|s| Url::parse(&s))
                .transpose()
                .context("L1_BEACON_RPC must be a valid URL if provided")?,
        })
    }

    /// Discover RPCs from DevnetManager API
    pub async fn from_devnet_api(api_url: &str) -> Result<Self> {
        let client = reqwest::Client::new();
        let url = format!("{}/rpcs", api_url.trim_end_matches('/'));
        
        info!("Discovering RPCs from DevnetManager API: {}", url);
        
        let response = client
            .get(&url)
            .send()
            .await
            .context("Failed to connect to DevnetManager API")?;

        if !response.status().is_success() {
            anyhow::bail!(
                "DevnetManager API returned error: {}",
                response.status()
            );
        }

        #[derive(Deserialize)]
        struct RPCEndpoints {
            l1_rpc: String,
            l2_rpc: String,
            l2_node_rpc: String,
            #[serde(default)]
            l1_beacon_rpc: Option<String>,
        }

        let endpoints: RPCEndpoints = response
            .json()
            .await
            .context("Failed to parse RPC endpoints from DevnetManager API")?;

        Ok(RPCConfig {
            l1_rpc: Url::parse(&endpoints.l1_rpc)
                .with_context(|| format!("Invalid L1_RPC from API: {}", endpoints.l1_rpc))?,
            l2_rpc: Url::parse(&endpoints.l2_rpc)
                .with_context(|| format!("Invalid L2_RPC from API: {}", endpoints.l2_rpc))?,
            l2_node_rpc: Url::parse(&endpoints.l2_node_rpc)
                .with_context(|| format!("Invalid L2_NODE_RPC from API: {}", endpoints.l2_node_rpc))?,
            l1_beacon_rpc: endpoints.l1_beacon_rpc
                .map(|s| Url::parse(&s))
                .transpose()
                .context("Invalid L1_BEACON_RPC from API")?,
        })
    }

    /// Try to discover RPCs from DevnetManager API, fallback to environment
    pub async fn discover() -> Result<Self> {
        // Try DevnetManager API first if URL is provided
        if let Ok(api_url) = std::env::var("DEVNET_API_URL") {
            match Self::from_devnet_api(&api_url).await {
                Ok(rpc) => {
                    info!("Successfully discovered RPCs from DevnetManager API");
                    return Ok(rpc);
                }
                Err(e) => {
                    tracing::warn!("Failed to discover RPCs from DevnetManager API: {}. Falling back to environment variables.", e);
                }
            }
        }

        // Fallback to environment variables
        info!("Loading RPC configuration from environment variables");
        Self::from_env()
    }
}

impl ZiskConfig {
    pub fn from_env() -> Result<Self> {
        Ok(ZiskConfig {
            range_elf_path: std::env::var("RANGE_ELF_PATH")
                .ok()
                .map(PathBuf::from),
            proving_key_path: std::env::var("RANGE_PROVING_KEY_PATH")
                .ok()
                .map(PathBuf::from),
            witness_lib_path: std::env::var("WITNESS_LIB_PATH")
                .ok()
                .map(PathBuf::from),
            verify_proofs: std::env::var("VERIFY_PROOFS")
                .unwrap_or_else(|_| "true".to_string())
                .parse::<bool>()
                .unwrap_or(true),
        })
    }
}

impl PathConfig {
    pub fn from_env() -> Result<Self> {
        Ok(PathConfig {
            l1_config_dir: std::env::var("L1_CONFIG_DIR")
                .ok()
                .map(PathBuf::from),
            l2_config_dir: std::env::var("L2_CONFIG_DIR")
                .ok()
                .map(PathBuf::from),
        })
    }
}

