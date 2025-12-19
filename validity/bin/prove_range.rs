use std::path::PathBuf;
use std::sync::Arc;

use alloy_provider::Provider;
use anyhow::{Context, Result};
use clap::Parser;
use op_zisk_config::RPCConfig;
use op_zisk_host_utils::{block_range::get_rolling_block_range, fetcher::OPZisKDataFetcher, setup_logger};
use op_zisk_host_utils::host::OPZisKHost;
use op_zisk_host_utils::witness_generation::WitnessGenerator;
use op_zisk_proof_utils::initialize_host;
use rkyv::to_bytes;
use tracing::info;
use zisk_sdk::ProverClientBuilder;

#[derive(Parser)]
#[command(author, version, about = "One-shot ZisK range proof (Ethereum DA) using local keys", long_about = None)]
struct Args {
    /// Path to environment file (must include L1_RPC / L2_RPC, etc. as required by the host).
    #[arg(long, default_value = ".env")]
    env_file: String,

    /// Start L2 block (inclusive). Mutually exclusive with --last.
    #[arg(long, required_unless_present = "last")]
    start: Option<u64>,

    /// End L2 block (exclusive). Mutually exclusive with --last.
    #[arg(long, required_unless_present = "last")]
    end: Option<u64>,

    /// Prove the last N finalized L2 blocks (DA-aware). If set, start/end are ignored.
    #[arg(long)]
    last: Option<u64>,

    /// Optional override for the range ELF path (otherwise uses RANGE_ELF_PATH env var or default).
    #[arg(long)]
    range_elf_path: Option<PathBuf>,

    /// Optional override for proving key path (otherwise uses RANGE_PROVING_KEY_PATH env var or default).
    #[arg(long)]
    proving_key_path: Option<PathBuf>,

    /// Optional override for witness computation library path (defaults to ~/.zisk/bin/libzisk_witness.(dylib|so)).
    #[arg(long)]
    witness_lib_path: Option<PathBuf>,

    /// Whether to enable proof verification inside the prover pipeline (if supported).
    #[arg(long, default_value_t = true)]
    verify_proofs: bool,

    /// Whether to fallback to timestamp-based L1 head estimation even though SafeDB is not activated for op-node.
    #[arg(long, default_value_t = false)]
    safe_db_fallback: bool,

    /// Optional path to save the witness data (input.bin) for later proof generation.
    /// If set, the stdin data will be saved to this file and proof generation will be skipped.
    #[arg(long)]
    save_input: Option<PathBuf>,

    /// Path to load witness data (input.bin) from file instead of generating from RPCs.
    /// If set, RPC fetching and witness generation will be skipped.
    #[arg(long)]
    load_input: Option<PathBuf>,

    /// Optional DevnetManager API URL for automatic RPC discovery.
    /// If set, RPCs will be discovered from the DevnetManager API instead of environment variables.
    /// Example: http://127.0.0.1:8080
    #[arg(long)]
    devnet_api_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let provider = rustls::crypto::ring::default_provider();
    provider
        .install_default()
        .map_err(|e| anyhow::anyhow!("Failed to install default provider: {:?}", e))?;

    let args = Args::parse();

    // Env file is optional: if it doesn't exist, rely on already-exported env vars.
    if let Err(e) = dotenv::from_filename(&args.env_file) {
        tracing::warn!(
            env_file = %args.env_file,
            error = ?e,
            "Env file could not be loaded; continuing with process environment"
        );
    }

    setup_logger();

    let (stdin, fetcher_opt) = if let Some(input_path) = args.load_input {
        // Load witness data from file
        tracing::info!("Loading witness data from: {}", input_path.display());
        let input_bytes = std::fs::read(&input_path)
            .with_context(|| format!("Failed to read input file: {}", input_path.display()))?;
        (zisk_common::io::ZiskStdin::from_vec(input_bytes), None)
    } else {
        // Discover RPCs from DevnetManager API or environment
        let rpc_config = if let Some(api_url) = args.devnet_api_url.as_ref() {
            info!("Discovering RPCs from DevnetManager API: {}", api_url);
            RPCConfig::from_devnet_api(api_url)
                .await
                .context("Failed to discover RPCs from DevnetManager API")?
        } else {
            // Try DevnetManager API from environment, fallback to env vars
            RPCConfig::discover()
                .await
                .context("Failed to discover RPC configuration")?
        };

        // Set environment variables for the fetcher (it still uses env vars internally)
        std::env::set_var("L1_RPC", rpc_config.l1_rpc.to_string());
        std::env::set_var("L2_RPC", rpc_config.l2_rpc.to_string());
        std::env::set_var("L2_NODE_RPC", rpc_config.l2_node_rpc.to_string());
        if let Some(ref beacon) = rpc_config.l1_beacon_rpc {
            std::env::set_var("L1_BEACON_RPC", beacon.to_string());
        }

        // Perform health check before proceeding
        info!("Performing RPC health check...");
        let health = op_zisk_config::ProofConfig {
            rpc: rpc_config.clone(),
            block_range: None,
            zisk: op_zisk_config::ZiskConfig::from_env()?,
            paths: op_zisk_config::PathConfig::from_env()?,
        };
        let health_status = health.health_check().await?;
        
        if !health_status.l1_available {
            anyhow::bail!("L1 RPC is not available - check your L1_RPC configuration");
        }
        if !health_status.l2_available {
            anyhow::bail!("L2 RPC is not available - check your L2_RPC configuration");
        }
        if !health_status.l2_node_available {
            anyhow::bail!("L2 Node RPC is not available - check your L2_NODE_RPC configuration");
        }
        
        info!(
            "RPC health check passed - L1: block {}, L2: block {}",
            health_status.l1_block_number.unwrap_or(0),
            health_status.l2_block_number.unwrap_or(0)
        );

        // Generate witness data from RPCs
        let fetcher = OPZisKDataFetcher::new_with_rollup_config()
            .await
            .context("Failed to initialize OPZisKDataFetcher - check RPC configuration")?;
        let host = initialize_host(Arc::new(fetcher.clone()));

        let (start, end) = if let Some(last) = args.last {
            get_rolling_block_range(host.as_ref(), &fetcher, last)
                .await
                .context("Failed to compute rolling block range")?
        } else {
            let start = args.start
                .ok_or_else(|| anyhow::anyhow!("--start is required when --last is not provided"))?;
            let end = args.end
                .ok_or_else(|| anyhow::anyhow!("--end is required when --last is not provided"))?;
            (start, end)
        };

        let host_args = host
            .fetch(start, end, None, args.safe_db_fallback)
            .await
            .context("Failed to build host args for witness generation")?;

        let witness_data = host.run(&host_args).await.context("Host witness generation failed")?;
        
        // Save to file if requested (before converting to stdin)
        if let Some(save_path) = args.save_input {
            tracing::info!("Saving witness data to: {}", save_path.display());
            // Serialize witness_data to bytes and add 8-byte size header (same format as get_zisk_stdin uses)
            let witness_bytes = to_bytes::<rkyv::rancor::Error>(&witness_data)
                .context("Failed to serialize witness data for saving")?;
            
            // Add 8-byte size header (ZisK input format requirement)
            let size = witness_bytes.len() as u64;
            let mut stdin_data = Vec::with_capacity(8 + witness_bytes.len());
            stdin_data.extend_from_slice(&size.to_le_bytes());
            stdin_data.extend_from_slice(witness_bytes.as_ref());
            
            std::fs::write(&save_path, &stdin_data)
                .with_context(|| format!("Failed to write input file: {}", save_path.display()))?;
            println!("Witness data saved to: {}", save_path.display());
            println!("Size: {} bytes ({} bytes data + 8 bytes header)", stdin_data.len(), witness_bytes.len());
            println!("");
            println!("You can now generate a proof using cargo-zisk:");
            println!("  cargo-zisk prove -e target/riscv64ima-zisk-zkvm-elf/release/range \\");
            println!("    -i {} \\", save_path.display());
            println!("    -k $HOME/.zisk/provingKey \\");
            println!("    -o proof -a -y");
            println!("");
            println!("See README_PROOF_GENERATION.md for the complete workflow.");
            return Ok(());
        }
        
        let stdin = host
            .witness_generator()
            .get_zisk_stdin(witness_data)
            .context("Failed to build ZiskStdin from witness")?;

        (stdin, Some(fetcher))
    };

    let range_elf_path = args.range_elf_path.or_else(|| std::env::var("RANGE_ELF_PATH").ok().map(PathBuf::from)).unwrap_or_else(|| {
        PathBuf::from("target/riscv64ima-zisk-zkvm-elf/release/range")
    });
    let proving_key_path =
        args.proving_key_path.or_else(|| std::env::var("RANGE_PROVING_KEY_PATH").ok().map(PathBuf::from)).unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
            PathBuf::from(format!("{}/.zisk/provingKey", home))
        });

    // Default witness library path based on OS
    let witness_lib_path = args.witness_lib_path.or_else(|| {
        let home = std::env::var("HOME").ok()?;
        #[cfg(target_os = "macos")]
        let lib_name = "libzisk_witness.dylib";
        #[cfg(target_os = "linux")]
        let lib_name = "libzisk_witness.so";
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let lib_name = "libzisk_witness.so"; // Default to .so for other platforms
        Some(PathBuf::from(format!("{}/.zisk/bin/{}", home, lib_name)))
    });

    let prover = ProverClientBuilder::new()
        .emu()
        .prove()
        .elf_path(range_elf_path)
        .proving_key_path(proving_key_path)
        .witness_lib_path_opt(witness_lib_path)
        .verify_proofs(args.verify_proofs)
        .aggregation(true)
        .build()
        .context("Failed to build ZisK prover")?;

    let result = tokio::task::spawn_blocking(move || prover.prove(stdin))
        .await?
        .context("ZisK prove() failed")?;

    let proof_bytes: Vec<u8> = result
        .proof
        .proof
        .context("ZisK returned no proof bytes")?
        .iter()
        .flat_map(|&x| x.to_le_bytes())
        .collect();

    println!("Proof generated successfully.");
    println!("Executed steps: {}", result.execution.executed_steps);
    println!("Proof bytes (u64->le bytes): {}", proof_bytes.len());

    // Sanity: ensure L1 provider is reachable (helps catch env issues early) - only if we have a fetcher
    if let Some(fetcher) = fetcher_opt {
        let _chain_id = fetcher.l1_provider.get_chain_id().await?;
    }

    Ok(())
}


