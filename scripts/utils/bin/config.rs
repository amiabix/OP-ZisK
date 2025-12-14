use anyhow::Result;
use clap::Parser;
use op_zisk_client_utils::boot::hash_rollup_config;
use op_zisk_host_utils::fetcher::OPZisKDataFetcher;
use op_zisk_host_utils::range_vkey_commitment_from_elf;
use op_zisk_scripts::ConfigArgs;
use op_zisk_host_utils::setup_logger;
use std::path::PathBuf;

// Get the verification keys for the ELFs and check them against the contract.
#[tokio::main]
async fn main() -> Result<()> {
    let args = ConfigArgs::parse();

    if let Some(env_file) = args.env_file {
        dotenv::from_path(env_file).ok();

        setup_logger();

        let data_fetcher = OPZisKDataFetcher::new_with_rollup_config().await?;

        let rollup_config = data_fetcher.rollup_config.as_ref().unwrap();
        println!("Rollup Config Hash: 0x{:x}", hash_rollup_config(rollup_config));
    }

    // For ZisK, these values must be provided by configuration/deployment tooling.
    if let Ok(v) = std::env::var("RANGE_VKEY_COMMITMENT") {
        println!("Range VKey Commitment: {v}");
    } else {
        let range_elf_path = std::env::var("RANGE_ELF_PATH")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/riscv64ima-zisk-zkvm-elf/release/range"));
        match range_vkey_commitment_from_elf(&range_elf_path) {
            Ok(v) => println!("Range VKey Commitment (derived from ELF): 0x{:x}", v),
            Err(e) => println!(
                "Range VKey Commitment: <missing> (failed to derive from {}: {e})",
                range_elf_path.display()
            ),
        }
    }
    if let Ok(v) = std::env::var("AGGREGATION_VKEY").or_else(|_| std::env::var("AGG_VKEY")) {
        println!("Aggregation VKey: {v}");
    }

    Ok(())
}
