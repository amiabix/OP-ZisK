use std::path::PathBuf;
use std::sync::Arc;

use alloy_provider::Provider;
use anyhow::{Context, Result};
use clap::Parser;
use op_zisk_host_utils::{block_range::get_rolling_block_range, fetcher::OPZisKDataFetcher, setup_logger};
use op_zisk_host_utils::host::OPSuccinctHost;
use op_zisk_host_utils::witness_generation::WitnessGenerator;
use op_zisk_proof_utils::initialize_host;
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

    let fetcher = OPZisKDataFetcher::new_with_rollup_config().await?;
    let host = initialize_host(Arc::new(fetcher.clone()));

    let (start, end) = if let Some(last) = args.last {
        get_rolling_block_range(host.as_ref(), &fetcher, last)
            .await
            .context("Failed to compute rolling block range")?
    } else {
        (args.start.expect("start is required"), args.end.expect("end is required"))
    };

    let host_args = host
        .fetch(start, end, None, args.safe_db_fallback)
        .await
        .context("Failed to build host args for witness generation")?;

    let witness_data = host.run(&host_args).await.context("Host witness generation failed")?;
    let stdin = host
        .witness_generator()
        .get_zisk_stdin(witness_data)
        .context("Failed to build ZiskStdin from witness")?;

    let range_elf_path = args.range_elf_path.or_else(|| std::env::var("RANGE_ELF_PATH").ok().map(PathBuf::from)).unwrap_or_else(|| {
        PathBuf::from("target/riscv64ima-zisk-zkvm-elf/release/range")
    });
    let proving_key_path =
        args.proving_key_path.or_else(|| std::env::var("RANGE_PROVING_KEY_PATH").ok().map(PathBuf::from)).unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
            PathBuf::from(format!("{}/.zisk/provingKey", home))
        });

    let prover = ProverClientBuilder::new()
        .emu()
        .prove()
        .elf_path(range_elf_path)
        .proving_key_path(proving_key_path)
        .witness_lib_path_opt(args.witness_lib_path)
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

    // Sanity: ensure L1 provider is reachable (helps catch env issues early).
    let _chain_id = fetcher.l1_provider.get_chain_id().await?;

    Ok(())
}


