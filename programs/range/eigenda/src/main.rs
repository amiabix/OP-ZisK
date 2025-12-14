//! A program to verify a Optimism L2 block STF with EigenDA in the zkVM.
//!
//! This binary contains the client program for executing the Optimism rollup state transition
//! across a range of blocks, which can be used to generate an on chain validity proof. Depending on
//! the compilation pipeline, it will compile to be run either in native mode or in zkVM mode. In
//! native mode, the data for verifying the batch validity is fetched from RPC, while in zkVM mode,
//! the data is supplied by the host binary to the verifiable program.
//!
//! ZisK Migration Note:
//! This version does not use the SP1-specific in-zkVM Canoe verifier.
//! For now it uses a no-op Canoe verifier (testing-only) which bypasses certificate validity
//! verification, while still verifying the KZG proofs for encoded payloads inside the zkVM.
//! Full security requires a real Canoe verifier strategy (in-zkVM and/or on-chain).

#![no_main]
ziskos::entrypoint!(main);

use hokulea_proof::eigenda_witness::EigenDAWitness;
use hokulea_proof::recency::DisabledZeroRecencyWindowProvider;
use op_zisk_client_utils::witness::{EigenDAWitnessData, WitnessData};
use op_zisk_eigenda_client_utils::executor::EigenDAWitnessExecutor;
use op_zisk_range_utils::run_range_program;
#[cfg(feature = "tracing-subscriber")]
use op_zisk_range_utils::setup_tracing;
use rkyv::rancor::Error;

fn main() {
    #[cfg(feature = "tracing-subscriber")]
    setup_tracing();

    // ZisK reads input as a single byte vector
    let input: Vec<u8> = ziskos::read_input();

    // Deserialize witness data
    let witness_data = rkyv::from_bytes::<EigenDAWitnessData, Error>(&input)
        .expect("Failed to deserialize witness data.");

    // ZisK doesn't provide a Tokio runtime inside the zkVM. Use a minimal executor.
    op_zisk_range_utils::block_on(async {
        let (oracle, beacon) = witness_data
            .clone()
            .get_oracle_and_blob_provider()
            .await
            .expect("Failed to load oracle and blob provider");

        let eigenda_witness: EigenDAWitness = serde_cbor::from_slice(
            &witness_data.eigenda_data.clone().expect("eigenda witness data is not present"),
        )
        .expect("cannot deserialize eigenda witness");

        // Build the verified preimage provider for EigenDA derivation.
        //
        // SECURITY NOTE:
        // - We use CanoeNoOpVerifier (unsafe; testing-only), meaning certificate validity
        //   is not verified in-zkVM.
        // - KZG proof verification is still performed inside PreloadedEigenDAPreimageProvider.
        let preloaded_preimage_provider = hokulea_zkvm_verification::eigenda_witness_to_preloaded_provider(
            oracle.clone(),
            canoe_verifier::CanoeNoOpVerifier {},
            canoe_verifier_address_fetcher::CanoeNoOpVerifierAddressFetcher {},
            DisabledZeroRecencyWindowProvider {},
            eigenda_witness,
        )
        .await
        .expect("failed to convert EigenDA witness to preloaded preimage provider");

        run_range_program(EigenDAWitnessExecutor::new(preloaded_preimage_provider), oracle, beacon)
            .await;
    });
}
