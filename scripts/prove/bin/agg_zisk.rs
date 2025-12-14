use std::{fs, path::PathBuf};

use alloy_primitives::{Address, B256};
use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use clap::Parser;
use op_zisk_client_utils::boot::BootInfoStruct;
use op_zisk_host_utils::{
    b256_to_u32x8_be, fetcher::OPZisKDataFetcher, get_agg_proof_stdin,
    range_vkey_commitment_from_elf,
};
use zisk_sdk::ProverClientBuilder;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Proof file stems (without extension) located in `data/<chain_id>/proofs/range/`.
    /// Example: `105-115,115-125`
    #[arg(short, long, num_args = 1.., value_delimiter = ',')]
    proofs: Vec<String>,

    /// Generate aggregation proof (otherwise run verify_constraints).
    #[arg(short, long)]
    prove: bool,

    /// Prover address.
    #[arg(short, long)]
    prover: Address,

    /// Env file path.
    #[arg(default_value = ".env", short, long)]
    env_file: String,
}

fn parse_b256_hex(s: &str) -> Result<B256> {
    let s = s.trim();
    Ok(s.parse()?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    dotenv::from_filename(&args.env_file).ok();
    op_zisk_host_utils::setup_logger();

    let fetcher = OPZisKDataFetcher::new_with_rollup_config().await?;
    let l2_chain_id = fetcher.get_l2_chain_id().await?;

    let metadata = MetadataCommand::new().exec().unwrap();
    let workspace_root = PathBuf::from(metadata.workspace_root);
    let proof_directory = workspace_root.join(format!("data/{}/proofs/range", l2_chain_id));

    // Load proofs + boot infos.
    let mut proofs_bytes: Vec<Vec<u8>> = Vec::with_capacity(args.proofs.len());
    let mut boot_infos: Vec<BootInfoStruct> = Vec::with_capacity(args.proofs.len());
    for stem in &args.proofs {
        let proof_path = proof_directory.join(format!("{stem}.bin"));
        let boot_info_path = proof_directory.join(format!("{stem}.bootinfo.json"));

        let proof = fs::read(&proof_path)
            .with_context(|| format!("missing proof file: {}", proof_path.display()))?;
        let boot_info: BootInfoStruct = serde_json::from_slice(
            &fs::read(&boot_info_path)
                .with_context(|| format!("missing boot info file: {}", boot_info_path.display()))?,
        )?;

        proofs_bytes.push(proof);
        boot_infos.push(boot_info);
    }

    let checkpoint_header = fetcher.get_latest_l1_head_in_batch(&boot_infos).await?;
    let headers = fetcher
        .get_header_preimages(&boot_infos, checkpoint_header.hash_slow())
        .await?;

    // multi_block_vkey is the range vkey commitment. For ZisK this must be provided.
    let range_vkey_commitment_b256 = match std::env::var("RANGE_VKEY_COMMITMENT") {
        Ok(v) => parse_b256_hex(&v)?,
        Err(_) => {
            let range_elf_path = std::env::var("RANGE_ELF_PATH")
                .ok()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("target/riscv64ima-zisk-zkvm-elf/release/range"));
            let derived = range_vkey_commitment_from_elf(&range_elf_path).with_context(|| {
                format!(
                    "failed to derive RANGE_VKEY_COMMITMENT from ELF: {}",
                    range_elf_path.display()
                )
            })?;
            println!("Derived RANGE_VKEY_COMMITMENT: 0x{:x}", derived);
            derived
        }
    };
    let multi_block_vkey = b256_to_u32x8_be(range_vkey_commitment_b256);

    let stdin = get_agg_proof_stdin(
        proofs_bytes,
        boot_infos,
        headers,
        multi_block_vkey,
        checkpoint_header.hash_slow(),
        args.prover,
    )
    .context("failed to build aggregation stdin")?;

    let agg_elf_path = std::env::var("AGG_ELF_PATH")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/riscv64ima-zisk-zkvm-elf/release/aggregation"));
    let agg_proving_key_path = std::env::var("AGG_PROVING_KEY_PATH")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
            PathBuf::from(format!("{}/.zisk/provingKey", home))
        });
    let witness_lib_path = std::env::var("ZISK_WITNESS_LIB_PATH").ok().map(PathBuf::from);

    if args.prove {
        let prover = ProverClientBuilder::new()
            .emu()
            .prove()
            .elf_path(agg_elf_path)
            .proving_key_path(agg_proving_key_path)
            .witness_lib_path_opt(witness_lib_path)
            .verify_proofs(true)
            .aggregation(true)
            .build()
            .context("failed to build ZisK prover")?;

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

        let out_dir = workspace_root.join(format!("data/{}/proofs/agg", l2_chain_id));
        fs::create_dir_all(&out_dir)?;
        fs::write(out_dir.join("agg.bin"), &proof_bytes)?;

        println!("Aggregation proof saved: {}", out_dir.join("agg.bin").display());
        println!("Executed steps: {}", result.execution.executed_steps);
        println!("Proof bytes: {}", proof_bytes.len());
    } else {
        let prover = ProverClientBuilder::new()
            .emu()
            .verify_constraints()
            .elf_path(agg_elf_path)
            .proving_key_path(agg_proving_key_path)
            .witness_lib_path_opt(witness_lib_path)
            .build()
            .context("failed to build ZisK executor")?;

        let result = tokio::task::spawn_blocking(move || prover.verify_constraints(stdin))
            .await?
            .context("ZisK verify_constraints() failed")?;

        println!("Executed steps: {}", result.execution.executed_steps);
    }

    Ok(())
}


