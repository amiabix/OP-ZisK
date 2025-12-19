use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use hokulea_compute_proof::create_kzg_proofs_for_eigenda_preimage;
use hokulea_proof::{
    eigenda_provider::OracleEigenDAPreimageProvider,
    eigenda_witness::{EigenDAPreimage, EigenDAWitness},
};
use hokulea_witgen::witness_provider::OracleEigenDAPreimageProviderWithPreimage;
use kona_preimage::{HintWriter, NativeChannel, OracleReader};
use kona_proof::l1::OracleBlobProvider;
use op_zisk_client_utils::witness::{
    executor::{get_inputs_for_pipeline, WitnessExecutor as WitnessExecutorTrait},
    preimage_store::PreimageStore,
    BlobData, EigenDAWitnessData,
};
use op_zisk_eigenda_client_utils::executor::EigenDAWitnessExecutor;
use op_zisk_host_utils::witness_generation::{
    online_blob_store::OnlineBlobStore, preimage_witness_collector::PreimageWitnessCollector,
    DefaultOracleBase, WitnessGenerator,
};
use rkyv::to_bytes;
use zisk_common::io::ZiskStdin;

type WitnessExecutor = EigenDAWitnessExecutor<
    PreimageWitnessCollector<DefaultOracleBase>,
    OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>,
    OracleEigenDAPreimageProvider<DefaultOracleBase>,
>;

pub struct EigenDAWitnessGenerator {}

#[async_trait]
impl WitnessGenerator for EigenDAWitnessGenerator {
    type WitnessData = EigenDAWitnessData;
    type WitnessExecutor = WitnessExecutor;

    fn get_executor(&self) -> &Self::WitnessExecutor {
        panic!("get_executor should not be called directly for EigenDAWitnessGenerator")
    }

    fn get_zisk_stdin(&self, witness: Self::WitnessData) -> Result<ZiskStdin> {
        // Serialize witness data
        let buffer = to_bytes::<rkyv::rancor::Error>(&witness)?;
        
        // ZisK requires input format: [8-byte size header (u64 LE) | data bytes]
        let size = buffer.len() as u64;
        let mut stdin_data = Vec::with_capacity(8 + buffer.len());
        stdin_data.extend_from_slice(&size.to_le_bytes());
        stdin_data.extend_from_slice(&buffer);
        
        Ok(ZiskStdin::from_vec(stdin_data))
    }

    async fn run(
        &self,
        preimage_chan: NativeChannel,
        hint_chan: NativeChannel,
    ) -> Result<Self::WitnessData> {
        let preimage_witness_store = Arc::new(std::sync::Mutex::new(PreimageStore::default()));
        let blob_data = Arc::new(std::sync::Mutex::new(BlobData::default()));

        let preimage_oracle = Arc::new(kona_proof::CachingOracle::new(
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

        // Create EigenDA blob provider that collects witness data
        let eigenda_preimage_provider = OracleEigenDAPreimageProvider::new(oracle.clone());
        let eigenda_preimage = Arc::new(Mutex::new(EigenDAPreimage::default()));

        let eigenda_preimage_provider = OracleEigenDAPreimageProviderWithPreimage {
            provider: eigenda_preimage_provider,
            preimage: eigenda_preimage.clone(),
        };

        let executor = EigenDAWitnessExecutor::new(eigenda_preimage_provider);

        let (boot_info, input) = get_inputs_for_pipeline(oracle.clone()).await.unwrap();
        if let Some((cursor, l1_provider, l2_provider)) = input {
            let rollup_config = Arc::new(boot_info.rollup_config.clone());
            let l1_config = Arc::new(boot_info.l1_config.clone());
            let pipeline = WitnessExecutorTrait::create_pipeline(
                &executor,
                rollup_config,
                l1_config,
                cursor.clone(),
                oracle.clone(),
                beacon,
                l1_provider.clone(),
                l2_provider.clone(),
            )
            .await
            .unwrap();
            WitnessExecutorTrait::run(&executor, boot_info.clone(), pipeline, cursor, l2_provider)
                .await
                .unwrap();
        }

        // Extract the EigenDA preimage data
        let eigenda_preimage_data = std::mem::take(&mut *eigenda_preimage.lock().unwrap());

        let kzg_proofs = create_kzg_proofs_for_eigenda_preimage(&eigenda_preimage_data);

        // ZisK migration: Canoe reduced proofs are SP1-specific and are not generated here.
        let maybe_canoe_proof_bytes: Option<Vec<u8>> = None;

        let eigenda_witness = EigenDAWitness::from_preimage(
            eigenda_preimage_data,
            kzg_proofs,
            maybe_canoe_proof_bytes,
        )?;

        let eigenda_witness_bytes =
            serde_cbor::to_vec(&eigenda_witness).expect("Failed to serialize EigenDA witness data");

        let witness = EigenDAWitnessData {
            preimage_store: preimage_witness_store.lock().unwrap().clone(),
            blob_data: blob_data.lock().unwrap().clone(),
            eigenda_data: Some(eigenda_witness_bytes),
        };

        Ok(witness)
    }
}
