use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use op_zisk_host_utils::block_range::get_validated_block_range;
use op_zisk_host_utils::fetcher::OPZisKDataFetcher;
use op_zisk_host_utils::host::OPSuccinctHost;
use op_zisk_host_utils::setup_logger;
use op_zisk_host_utils::witness_generation::WitnessGenerator;
use op_zisk_proof_utils::initialize_host;
use zisk_sdk::ProverClientBuilder;

/// ZisK-based local preflight: witness generation + local range proof generation.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The environment file path.
    #[arg(long, default_value = ".env")]
    env_file: PathBuf,

    /// Start L2 block (inclusive). Required unless --last is provided.
    #[arg(long, required_unless_present = "last")]
    start: Option<u64>,

    /// End L2 block (exclusive). Required unless --last is provided.
    #[arg(long, required_unless_present = "last")]
    end: Option<u64>,

    /// Prove the last N finalized L2 blocks (DA-aware).
    #[arg(long)]
    last: Option<u64>,

    /// Whether to fallback to timestamp-based L1 head estimation even though SafeDB is not
    /// activated for op-node.
    #[arg(long, default_value_t = false)]
    safe_db_fallback: bool,

    /// Path to the ZisK range ELF. Defaults to `target/riscv64ima-zisk-zkvm-elf/release/range`.
    #[arg(long)]
    range_elf_path: Option<PathBuf>,

    /// Path to the ZisK proving key. Defaults to `~/.zisk/provingKey`.
    #[arg(long)]
    proving_key_path: Option<PathBuf>,

    /// Optional witness computation library path (defaults to `~/.zisk/bin/libzisk_witness.*`).
    #[arg(long)]
    witness_lib_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_logger();

    let args = Args::parse();
    dotenv::from_path(&args.env_file)
        .with_context(|| format!("Environment file not found: {}", args.env_file.display()))?;

    let fetcher = OPZisKDataFetcher::new_with_rollup_config().await?;
    let host = initialize_host(Arc::new(fetcher.clone()));

    let (start, end) = if let Some(last) = args.last {
        // Reuse existing validated range logic to anchor on finalized L2 where possible.
        get_validated_block_range(host.as_ref(), &fetcher, None, None, last)
            .await
            .context("Failed to compute validated block range")?
    } else {
        (
            args.start.expect("start is required"),
            args.end.expect("end is required"),
        )
    };

    // Build witness.
    let host_args = host
        .fetch(start, end, None, args.safe_db_fallback)
        .await
        .context("Failed to build host args")?;
    let witness = host.run(&host_args).await.context("Host witness generation failed")?;
    let stdin = host
        .witness_generator()
        .get_zisk_stdin(witness)
        .context("Failed to build ZiskStdin from witness")?;

    let range_elf_path = args.range_elf_path.unwrap_or_else(|| {
        std::env::var("RANGE_ELF_PATH")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/riscv64ima-zisk-zkvm-elf/release/range"))
    });
    let proving_key_path = args.proving_key_path.unwrap_or_else(|| {
        std::env::var("RANGE_PROVING_KEY_PATH")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
                PathBuf::from(format!("{}/.zisk/provingKey", home))
            })
    });

    // Prove.
    let prover = ProverClientBuilder::new()
        .emu()
        .prove()
        .elf_path(range_elf_path)
        .proving_key_path(proving_key_path)
        .witness_lib_path_opt(args.witness_lib_path)
        .verify_proofs(true)
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

    println!("Preflight completed.");
    println!("Executed steps: {}", result.execution.executed_steps);
    println!("Proof bytes (u64->le bytes): {}", proof_bytes.len());

    Ok(())
}


