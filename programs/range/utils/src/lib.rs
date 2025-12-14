use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::sync::Arc;

use kona_proof::{l1::OracleL1ChainProvider, l2::OracleL2ChainProvider};
use op_zisk_client_utils::{
    boot::BootInfoStruct,
    witness::{
        executor::{get_inputs_for_pipeline, WitnessExecutor},
        preimage_store::PreimageStore,
    },
    BlobStore,
};

/// Execute an async future to completion without requiring an async runtime.
///
/// This is used inside zkVM programs where `tokio` is not available.
pub fn block_on<F: Future>(future: F) -> F::Output {
    fn no_op(_: *const ()) {}
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(core::ptr::null(), &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);

    let raw_waker = RawWaker::new(core::ptr::null(), &VTABLE);
    // SAFETY: The waker does not use the data pointer.
    let waker = unsafe { Waker::from_raw(raw_waker) };
    let mut cx = Context::from_waker(&waker);

    let mut future = core::pin::pin!(future);
    loop {
        match Future::poll(Pin::as_mut(&mut future), &mut cx) {
            Poll::Ready(val) => return val,
            Poll::Pending => {
                // In the zkVM environment, the futures we run should not rely on external wakeups.
                // If we ever hit Pending, spin until completion.
                continue;
            }
        }
    }
}

/// Sets up tracing for the range program
#[cfg(feature = "tracing-subscriber")]
pub fn setup_tracing() {
    use anyhow::anyhow;
    use tracing::Level;

    let subscriber = tracing_subscriber::fmt().with_max_level(Level::INFO).finish();
    tracing::subscriber::set_global_default(subscriber).map_err(|e| anyhow!(e)).unwrap();
}

pub async fn run_range_program<E>(executor: E, oracle: Arc<PreimageStore>, beacon: BlobStore)
where
    E: WitnessExecutor<
            O = PreimageStore,
            B = BlobStore,
            L1 = OracleL1ChainProvider<PreimageStore>,
            L2 = OracleL2ChainProvider<PreimageStore>,
        > + Send
        + Sync,
{
    ////////////////////////////////////////////////////////////////
    //                          PROLOGUE                          //
    ////////////////////////////////////////////////////////////////
    let (boot_info, input) = get_inputs_for_pipeline(oracle.clone()).await.unwrap();
    let boot_info = match input {
        Some((cursor, l1_provider, l2_provider)) => {
            let rollup_config = Arc::new(boot_info.rollup_config.clone());
            let l1_config = Arc::new(boot_info.l1_config.clone());

            let pipeline = executor
                .create_pipeline(
                    rollup_config,
                    l1_config,
                    cursor.clone(),
                    oracle,
                    beacon,
                    l1_provider,
                    l2_provider.clone(),
                )
                .await
                .unwrap();

            executor.run(boot_info, pipeline, cursor, l2_provider).await.unwrap()
        }
        None => boot_info,
    };

    // Output boot info using ZisK's set_output
    // ZisK requires output as u32 chunks
    let boot_info_struct = BootInfoStruct::from(boot_info);
    let output_bytes = bincode::serialize(&boot_info_struct)
        .expect("Failed to serialize boot info");
    
    for (i, chunk) in output_bytes.chunks(4).enumerate() {
        let mut chunk_array = [0u8; 4];
        chunk_array[..chunk.len()].copy_from_slice(chunk);
        let val = u32::from_le_bytes(chunk_array);
        ziskos::set_output(i, val);
    }
}
