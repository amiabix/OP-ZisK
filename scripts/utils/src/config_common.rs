use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Context, Result};
use op_zisk_client_utils::boot::hash_rollup_config;
use op_zisk_host_utils::fetcher::OPZisKDataFetcher;
use op_zisk_host_utils::range_vkey_commitment_from_elf;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

pub const TWO_WEEKS_IN_SECONDS: u64 = 14 * 24 * 60 * 60;

/// Shared configuration data that both L2OO and FDG configs use.
#[derive(Debug, Clone)]
pub struct SharedConfigData {
    pub rollup_config_hash: String,
    pub aggregation_vkey: String,
    pub range_vkey_commitment: String,
    pub verifier_address: String,
    pub use_sp1_mock_verifier: bool,
}

/// Returns an address based on environment variables and private key settings:
/// - If env_var exists, returns that address
/// - Otherwise if private_key_by_default=true and PRIVATE_KEY exists, returns address derived from
///   private key
/// - Otherwise returns zero address
pub fn get_address(env_var: &str, private_key_by_default: bool) -> String {
    // First try to get address directly from env var.
    if let Ok(addr) = env::var(env_var) {
        return addr;
    }

    // Next try to derive address from private key if enabled.
    if private_key_by_default {
        if let Ok(pk) = env::var("PRIVATE_KEY") {
            let signer: PrivateKeySigner = pk.parse().unwrap();
            return signer.address().to_string();
        }
    }

    // Fallback to zero address.
    Address::ZERO.to_string()
}

/// Parse comma-separated addresses from environment variable.
pub fn parse_addresses(env_var: &str) -> Vec<String> {
    env::var(env_var)
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect()
}

/// Get shared configuration data that both L2OO and FDG configs need.
pub async fn get_shared_config_data(
    data_fetcher: OPZisKDataFetcher,
) -> Result<SharedConfigData> {
    // Determine if we're using mock verifier.
    let use_sp1_mock_verifier = env::var("OP_ZISK_MOCK")
        .unwrap_or("false".to_string())
        .parse::<bool>()
        .unwrap_or(false);

    // Set the verifier address.
    let verifier_address = env::var("VERIFIER_ADDRESS").unwrap_or_else(|_| {
        // For ZisK, verifier deployment is chain/environment specific.
        // Default to zero address if not provided.
        Address::ZERO.to_string()
    });

    let rollup_config = data_fetcher.rollup_config.as_ref().unwrap();
    let rollup_config_hash = format!("0x{:x}", hash_rollup_config(rollup_config));

    // ZisK does not expose SP1-style in-process verifying keys for these programs.
    // Require these values to be provided explicitly (e.g., from a deployment/config process).
    let aggregation_vkey = env::var("AGGREGATION_VKEY")
        .or_else(|_| env::var("AGG_VKEY"))
        .expect("AGGREGATION_VKEY (or AGG_VKEY) must be set for config generation");
    let range_vkey_commitment = match env::var("RANGE_VKEY_COMMITMENT") {
        Ok(v) => v,
        Err(_) => {
            let range_elf_path = env::var("RANGE_ELF_PATH")
                .ok()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("target/riscv64ima-zisk-zkvm-elf/release/range"));

            let commitment = range_vkey_commitment_from_elf(&range_elf_path)
                .with_context(|| format!("failed to derive commitment from ELF: {}", range_elf_path.display()))?;

            // Keep it in the same format expected by other tooling/configs.
            format!("0x{:x}", commitment)
        }
    };

    Ok(SharedConfigData {
        rollup_config_hash,
        aggregation_vkey,
        range_vkey_commitment,
        verifier_address,
        use_sp1_mock_verifier,
    })
}

/// Write a JSON config to a file.
pub fn write_config_file<T: serde::Serialize>(
    config: &T,
    file_path: &Path,
    description: &str,
) -> Result<()> {
    // Create parent directories if they don't exist.
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Write the config to the file.
    fs::write(file_path, serde_json::to_string_pretty(config)?)?;

    log::info!("Wrote {} configuration to: {}", description, file_path.display());

    Ok(())
}

/// Find the project root directory (where .git exists).
pub fn find_project_root() -> Option<PathBuf> {
    let mut path = std::env::current_dir().ok()?;
    while !path.join(".git").exists() {
        if !path.pop() {
            return None;
        }
    }
    Some(path)
}
