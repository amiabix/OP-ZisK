use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use kona_preimage::{HintWriter, NativeChannel, OracleReader};
use kona_proof::{
    l1::{OracleBlobProvider, OracleL1ChainProvider},
    l2::OracleL2ChainProvider,
    CachingOracle,
};
use op_zisk_client_utils::witness::{
    executor::{get_inputs_for_pipeline, WitnessExecutor},
    preimage_store::PreimageStore,
    BlobData, WitnessData,
};
use zisk_common::io::ZiskStdin;

use crate::witness_generation::{OnlineBlobStore, PreimageWitnessCollector};

pub type DefaultOracleBase = CachingOracle<OracleReader<NativeChannel>, HintWriter<NativeChannel>>;

#[async_trait]
pub trait WitnessGenerator {
    type WitnessData: WitnessData;
    type WitnessExecutor: WitnessExecutor<
            O = PreimageWitnessCollector<DefaultOracleBase>,
            B = OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>,
            L1 = OracleL1ChainProvider<PreimageWitnessCollector<DefaultOracleBase>>,
            L2 = OracleL2ChainProvider<PreimageWitnessCollector<DefaultOracleBase>>,
        > + Sync
        + Send;

    fn get_executor(&self) -> &Self::WitnessExecutor;

    async fn run(
        &self,
        preimage_chan: NativeChannel,
        hint_chan: NativeChannel,
    ) -> Result<Self::WitnessData> {
        let preimage_witness_store = Arc::new(Mutex::new(PreimageStore::default()));
        let blob_data = Arc::new(Mutex::new(BlobData::default()));

        let preimage_oracle = Arc::new(CachingOracle::new(
            2048,
            OracleReader::new(preimage_chan),
            HintWriter::new(hint_chan),
        ));
        let blob_provider = OracleBlobProvider::new(preimage_oracle.clone());

        let oracle = Arc::new(PreimageWitnessCollector {
            preimage_oracle: preimage_oracle.clone(),
            preimage_witness_store: preimage_witness_store.clone(),
        });
        let beacon = OnlineBlobStore { provider: blob_provider.clone(), store: blob_data.clone() };

        let (boot_info, input) = get_inputs_for_pipeline(oracle.clone()).await
            .map_err(|e| anyhow::anyhow!("Failed to get inputs for pipeline: {:#}", e))?;
        if let Some((cursor, l1_provider, l2_provider)) = input {
            let rollup_config = Arc::new(boot_info.rollup_config.clone());
            let l1_config = Arc::new(boot_info.l1_config.clone());
            let pipeline = self
                .get_executor()
                .create_pipeline(
                    rollup_config,
                    l1_config,
                    cursor.clone(),
                    oracle.clone(),
                    beacon,
                    l1_provider.clone(),
                    l2_provider.clone(),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create pipeline: {:#}", e))?;
            self.get_executor().run(boot_info, pipeline, cursor, l2_provider).await
                .map_err(|e| anyhow::anyhow!("Failed to run executor: {:#}", e))?;
        }

        let witness = Self::WitnessData::from_parts(
            preimage_witness_store.lock().unwrap().clone(),
            blob_data.lock().unwrap().clone(),
        );

        Ok(witness)
    }

    fn get_zisk_stdin(&self, witness: Self::WitnessData) -> Result<ZiskStdin>;
}
