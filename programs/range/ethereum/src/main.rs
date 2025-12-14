//! A program to verify a Optimism L2 block STF with Ethereum DA in the zkVM.
//!
//! This binary contains the client program for executing the Optimism rollup state transition
//! across a range of blocks, which can be used to generate an on chain validity proof. Depending on
//! the compilation pipeline, it will compile to be run either in native mode or in zkVM mode. In
//! native mode, the data for verifying the batch validity is fetched from RPC, while in zkVM mode,
//! the data is supplied by the host binary to the verifiable program.

#![no_main]
ziskos::entrypoint!(main);

use op_zisk_client_utils::witness::{DefaultWitnessData, WitnessData};
use op_zisk_ethereum_client_utils::executor::ETHDAWitnessExecutor;
use op_zisk_range_utils::{block_on, run_range_program};
#[cfg(feature = "tracing-subscriber")]
use op_zisk_range_utils::setup_tracing;
use rkyv::rancor::Error;

fn main() {
    #[cfg(feature = "tracing-subscriber")]
    setup_tracing();

    // ZisK reads input as a single byte vector
    let input: Vec<u8> = ziskos::read_input();
    
    // Deserialize witness data
    let witness_data = rkyv::from_bytes::<DefaultWitnessData, Error>(&input)
            .expect("Failed to deserialize witness data.");

    // ZisK doesn't provide a Tokio runtime inside the zkVM. Use a minimal executor.
    block_on(async {
        let (oracle, beacon) = witness_data
            .get_oracle_and_blob_provider()
            .await
            .expect("Failed to load oracle and blob provider");

        run_range_program(ETHDAWitnessExecutor::new(), oracle, beacon).await;
    });
}
