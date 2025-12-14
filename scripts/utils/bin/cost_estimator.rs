use anyhow::Result;
use clap::Parser;
use futures::StreamExt;
use log::info;
use op_zisk_host_utils::{
    block_range::{
        get_rolling_block_range, get_validated_block_range, split_range_based_on_safe_heads,
        split_range_basic, SpanBatchRange,
    },
    fetcher::{BlockInfo, OPZisKDataFetcher},
    host::OPSuccinctHost,
    setup_logger,
    witness_generation::WitnessGenerator,
};
use op_zisk_proof_utils::initialize_host;
use op_zisk_scripts::HostExecutorArgs;
use rayon::prelude::*;
use std::{
    cmp::{max, min},
    fs::{self, OpenOptions},
    io::Seek,
    path::PathBuf,
    sync::Arc,
    time::Instant,
};
use zisk_sdk::ProverClientBuilder;

/// Run the zkVM execution process for each split range in parallel. Writes the execution stats for
/// each block range to a CSV file after each execution completes (not guaranteed to be in order).
async fn execute_blocks_and_write_stats_csv<H: OPSuccinctHost>(
    host: Arc<H>,
    host_args: &[H::Args],
    ranges: Vec<SpanBatchRange>,
    l2_chain_id: u64,
    start: u64,
    end: u64,
    range_elf_path: PathBuf,
    proving_key_path: PathBuf,
    witness_lib_path: Option<PathBuf>,
) -> Result<()> {
    let data_fetcher = OPZisKDataFetcher::new_with_rollup_config().await?;

    // Fetch all of the execution stats block ranges in parallel.
    let block_data = futures::stream::iter(ranges.clone())
        .map(|range| async {
            let block_data = data_fetcher
                .get_l2_block_data_range(range.start, range.end)
                .await
                .expect("Failed to fetch block data range.");
            (range, block_data)
        })
        .buffered(15)
        .collect::<Vec<_>>()
        .await;

    let cargo_metadata = cargo_metadata::MetadataCommand::new().exec().unwrap();
    let root_dir = PathBuf::from(cargo_metadata.workspace_root);
    let report_path =
        root_dir.join(format!("execution-reports/{l2_chain_id}/{start}-{end}-report.csv"));
    // Create the parent directory if it doesn't exist
    if let Some(parent) = report_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).unwrap();
        }
    }

    // Create an empty file since canonicalize requires the path to exist
    fs::File::create(&report_path).unwrap();
    let report_path = report_path.canonicalize().unwrap();

    // Run the host tasks in parallel using join_all
    let handles = host_args.iter().map(|host_args| {
        let host_args = host_args.clone();
        let host = host.clone();
        tokio::spawn(async move {
            let witness_data = host.run(&host_args).await.unwrap();
            host.witness_generator().get_zisk_stdin(witness_data).unwrap()
        })
    });

    let stdins = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect::<Vec<_>>();

    let execution_inputs = stdins.into_iter().zip(block_data.into_iter()).collect::<Vec<_>>();

    // Execute the program for each block range in parallel.
    execution_inputs.into_par_iter().for_each(|(zisk_stdin, (range, block_data))| {
        let start_time = Instant::now();

        let prover = ProverClientBuilder::new()
            .emu()
            .verify_constraints()
            .elf_path(range_elf_path.clone())
            .proving_key_path(proving_key_path.clone())
            .witness_lib_path_opt(witness_lib_path.clone())
            .build();

        let prover = match prover {
            Ok(p) => p,
            Err(e) => {
                log::warn!("Failed to build ZisK prover: {e}");
                return;
            }
        };

        let result = prover.verify_constraints(zisk_stdin);
        let (executed_steps, duration_s) = match result {
            Ok(r) => (r.execution.executed_steps, start_time.elapsed().as_secs()),
            Err(e) => {
                log::warn!(
                    "Failed to execute blocks {:?} - {:?} because of {:?}.",
                    range.start,
                    range.end,
                    e
                );
                return;
            }
        };

        let row = ZiskExecutionStatsRow::new(range.start, range.end, block_data.as_slice(), executed_steps, duration_s);

        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&report_path)
            .unwrap();

        // Writes the headers only if the file is empty.
        let needs_header = file.seek(std::io::SeekFrom::End(0)).unwrap() == 0;

        let mut csv_writer = csv::WriterBuilder::new()
            .has_headers(needs_header)
            .from_writer(file);

        csv_writer
            .serialize(row)
            .expect("Failed to write execution stats to CSV.");
        csv_writer.flush().expect("Failed to flush CSV writer.");
    });

    info!("Execution is complete.");

    Ok(())
}
/// Aggregate the execution statistics for an array of execution stats objects.
fn aggregate_execution_stats(
    execution_stats: &[ZiskExecutionStatsRow],
    total_execution_time_sec: u64,
    witness_generation_time_sec: u64,
) -> ZiskExecutionStatsRow {
    let mut aggregate_stats = ZiskExecutionStatsRow::default();
    let mut batch_start = u64::MAX;
    let mut batch_end = u64::MIN;
    for stats in execution_stats {
        batch_start = min(batch_start, stats.batch_start);
        batch_end = max(batch_end, stats.batch_end);

        aggregate_stats.executed_steps += stats.executed_steps;
        aggregate_stats.nb_blocks += stats.nb_blocks;
        aggregate_stats.nb_transactions += stats.nb_transactions;
        aggregate_stats.eth_gas_used += stats.eth_gas_used;
        aggregate_stats.total_l1_fees += stats.total_l1_fees;
        aggregate_stats.total_tx_fees += stats.total_tx_fees;
    }

    if aggregate_stats.nb_blocks > 0 {
        aggregate_stats.avg_txns_per_block =
            aggregate_stats.nb_transactions as f64 / aggregate_stats.nb_blocks as f64;
        aggregate_stats.avg_gas_per_block =
            aggregate_stats.eth_gas_used as f64 / aggregate_stats.nb_blocks as f64;
        aggregate_stats.avg_steps_per_block =
            aggregate_stats.executed_steps as f64 / aggregate_stats.nb_blocks as f64;
    }

    // Use the earliest start and latest end across all blocks.
    aggregate_stats.batch_start = batch_start;
    aggregate_stats.batch_end = batch_end;

    // Set the total execution time to the total execution time of the entire range.
    aggregate_stats.total_execution_time_sec = total_execution_time_sec;
    aggregate_stats.witness_generation_time_sec = witness_generation_time_sec;

    aggregate_stats
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct ZiskExecutionStatsRow {
    batch_start: u64,
    batch_end: u64,
    nb_blocks: u64,
    nb_transactions: u64,
    eth_gas_used: u64,
    total_l1_fees: u128,
    total_tx_fees: u128,
    executed_steps: u64,
    total_execution_time_sec: u64,
    witness_generation_time_sec: u64,
    avg_txns_per_block: f64,
    avg_gas_per_block: f64,
    avg_steps_per_block: f64,
}

impl ZiskExecutionStatsRow {
    fn new(
        batch_start: u64,
        batch_end: u64,
        block_data: &[BlockInfo],
        executed_steps: u64,
        total_execution_time_sec: u64,
    ) -> Self {
        let nb_blocks = block_data.len() as u64;
        let total_txns_including_system: u64 = block_data.iter().map(|b| b.transaction_count).sum();
        let nb_transactions = total_txns_including_system.saturating_sub(nb_blocks);
        let eth_gas_used: u64 = block_data.iter().map(|b| b.gas_used).sum();
        let total_l1_fees: u128 = block_data.iter().map(|b| b.total_l1_fees).sum();
        let total_tx_fees: u128 = block_data.iter().map(|b| b.total_tx_fees).sum();

        let mut row = Self {
            batch_start,
            batch_end,
            nb_blocks,
            nb_transactions,
            eth_gas_used,
            total_l1_fees,
            total_tx_fees,
            executed_steps,
            total_execution_time_sec,
            witness_generation_time_sec: 0,
            avg_txns_per_block: 0.0,
            avg_gas_per_block: 0.0,
            avg_steps_per_block: 0.0,
        };

        if nb_blocks > 0 {
            row.avg_txns_per_block = nb_transactions as f64 / nb_blocks as f64;
            row.avg_gas_per_block = eth_gas_used as f64 / nb_blocks as f64;
            row.avg_steps_per_block = executed_steps as f64 / nb_blocks as f64;
        }

        row
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = HostExecutorArgs::parse();

    dotenv::from_path(&args.env_file).ok();
    setup_logger();

    let data_fetcher = OPZisKDataFetcher::new_with_rollup_config().await?;
    let l2_chain_id = data_fetcher.get_l2_chain_id().await?;

    // Get the host CLIs in order, in parallel.
    let host = initialize_host(Arc::new(data_fetcher.clone()));

    let (l2_start_block, l2_end_block) = if args.rolling {
        info!("Using rolling block range");
        get_rolling_block_range(host.as_ref(), &data_fetcher, args.default_range).await?
    } else {
        info!("Using validated block range");
        get_validated_block_range(
            host.as_ref(),
            &data_fetcher,
            args.start,
            args.end,
            args.default_range,
        )
        .await?
    };

    // Check if the safeDB is activated on the L2 node. If it is, we use the safeHead based range
    // splitting algorithm. Otherwise, we use the simple range splitting algorithm.
    let safe_db_activated = data_fetcher.is_safe_db_activated().await?;

    let split_ranges = if safe_db_activated {
        split_range_based_on_safe_heads(l2_start_block, l2_end_block, args.batch_size).await?
    } else {
        split_range_basic(l2_start_block, l2_end_block, args.batch_size)
    };

    info!("The span batch ranges which will be executed: {split_ranges:?}");

    let host_args = futures::stream::iter(split_ranges.iter())
        .map(|range| async {
            host.fetch(range.start, range.end, None, args.safe_db_fallback)
                .await
                .expect("Failed to get host CLI args")
        })
        .buffered(15)
        .collect::<Vec<_>>()
        .await;

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

    let total_start = Instant::now();
    execute_blocks_and_write_stats_csv(
        host,
        &host_args,
        split_ranges,
        l2_chain_id,
        l2_start_block,
        l2_end_block,
        range_elf_path,
        proving_key_path,
        witness_lib_path,
    )
    .await?;
    let total_execution_time_sec = total_start.elapsed().as_secs();

    // Get the path to the execution report CSV file.
    let cargo_metadata = cargo_metadata::MetadataCommand::new().exec().unwrap();
    let root_dir = PathBuf::from(cargo_metadata.workspace_root);
    let report_path = root_dir.join(format!(
        "execution-reports/{l2_chain_id}/{l2_start_block}-{l2_end_block}-report.csv"
    ));

    // Read the execution stats from the CSV file and aggregate them to output to the user.
    let mut final_execution_stats = Vec::new();
    let mut csv_reader = csv::Reader::from_path(&report_path)?;
    for result in csv_reader.deserialize() {
        let stats: ZiskExecutionStatsRow = result?;
        final_execution_stats.push(stats);
    }

    println!("Wrote execution stats to {}", report_path.display());

    // Aggregate the execution stats and print them to the user.
    let aggregate = aggregate_execution_stats(&final_execution_stats, total_execution_time_sec, 0);
    println!(
        "Aggregate Execution Stats for Chain {}:\n  executed_steps_total={} avg_steps_per_block={:.2} avg_txns_per_block={:.2} avg_gas_per_block={:.2}",
        l2_chain_id,
        aggregate.executed_steps,
        aggregate.avg_steps_per_block,
        aggregate.avg_txns_per_block,
        aggregate.avg_gas_per_block,
    );

    Ok(())
}
