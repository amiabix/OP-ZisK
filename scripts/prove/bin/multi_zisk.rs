use std::{fs, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use clap::Parser;
use op_zisk_client_utils::boot::BootInfoStruct;
use op_zisk_host_utils::{
    block_range::get_validated_block_range,
    fetcher::OPZisKDataFetcher,
    host::OPSuccinctHost,
    setup_logger,
    witness_generation::WitnessGenerator,
};
use op_zisk_proof_utils::initialize_host;
use op_zisk_prove::DEFAULT_RANGE;
use op_zisk_scripts::HostExecutorArgs;
use zisk_sdk::ProverClientBuilder;

/// Execute (and optionally prove) the OP-ZisK range program for a block range using ZisK.
#[tokio::main]
async fn main() -> Result<()> {
    let args = HostExecutorArgs::parse();

    dotenv::from_path(&args.env_file)
        .with_context(|| format!("Environment file not found: {}", args.env_file.display()))?;
    setup_logger();

    let fetcher = OPZisKDataFetcher::new_with_rollup_config().await?;
    let l2_chain_id = fetcher.get_l2_chain_id().await?;

    let host = initialize_host(Arc::new(fetcher.clone()));

    let (l2_start_block, l2_end_block) = get_validated_block_range(
        host.as_ref(),
        &fetcher,
        args.start,
        args.end,
        DEFAULT_RANGE,
    )
    .await?;

    let host_args = host
        .fetch(l2_start_block, l2_end_block, None, args.safe_db_fallback)
        .await?;

    // Derive the BootInfoStruct from host args (this is what the zkVM proves publicly).
    let rollup_config = fetcher
        .rollup_config
        .as_ref()
        .expect("rollup config must be loaded");
    let boot_info = BootInfoStruct {
        l1Head: host_args.l1_head,
        l2PreRoot: host_args.agreed_l2_output_root,
        l2PostRoot: host_args.claimed_l2_output_root,
        l2BlockNumber: host_args.claimed_l2_block_number,
        rollupConfigHash: op_zisk_client_utils::boot::hash_rollup_config(rollup_config),
    };

    // Witness + stdin.
    let witness = host.run(&host_args).await?;
    let stdin = host
        .witness_generator()
        .get_zisk_stdin(witness)
        .context("failed to build ZiskStdin from witness")?;

    let range_elf_path = std::env::var("RANGE_ELF_PATH")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/riscv64ima-zisk-zkvm-elf/release/range"));
    let proving_key_path = std::env::var("RANGE_PROVING_KEY_PATH")
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
            .elf_path(range_elf_path)
            .proving_key_path(proving_key_path)
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

        let out_dir = PathBuf::from(format!("data/{}/proofs/range", l2_chain_id));
        fs::create_dir_all(&out_dir)?;

        let stem = format!("{l2_start_block}-{l2_end_block}");
        fs::write(out_dir.join(format!("{stem}.bin")), &proof_bytes)?;
        fs::write(
            out_dir.join(format!("{stem}.bootinfo.json")),
            serde_json::to_vec_pretty(&boot_info)?,
        )?;

        println!("Range proof saved: {}", out_dir.join(format!("{stem}.bin")).display());
        println!(
            "Boot info saved: {}",
            out_dir.join(format!("{stem}.bootinfo.json")).display()
        );
        println!("Executed steps: {}", result.execution.executed_steps);
        println!("Proof bytes: {}", proof_bytes.len());
    } else {
        let prover = ProverClientBuilder::new()
            .emu()
            .verify_constraints()
            .elf_path(range_elf_path)
            .proving_key_path(proving_key_path)
            .witness_lib_path_opt(witness_lib_path)
            .build()
            .context("failed to build ZisK executor")?;

        let result = tokio::task::spawn_blocking(move || prover.verify_constraints(stdin))
            .await?
            .context("ZisK verify_constraints() failed")?;

        println!("Executed steps: {}", result.execution.executed_steps);
        println!("Boot info: {boot_info:?}");
    }

    Ok(())
}


