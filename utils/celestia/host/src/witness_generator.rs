use anyhow::Result;
use async_trait::async_trait;
use kona_proof::l1::OracleBlobProvider;
use op_zisk_celestia_client_utils::executor::CelestiaDAWitnessExecutor;
use op_zisk_client_utils::witness::DefaultWitnessData;
use op_zisk_host_utils::witness_generation::{
    online_blob_store::OnlineBlobStore, preimage_witness_collector::PreimageWitnessCollector,
    DefaultOracleBase, WitnessGenerator,
};
use rkyv::to_bytes;
use zisk_common::io::ZiskStdin;

type WitnessExecutor = CelestiaDAWitnessExecutor<
    PreimageWitnessCollector<DefaultOracleBase>,
    OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>,
>;

pub struct CelestiaDAWitnessGenerator {
    pub executor: WitnessExecutor,
}

#[async_trait]
impl WitnessGenerator for CelestiaDAWitnessGenerator {
    type WitnessData = DefaultWitnessData;
    type WitnessExecutor = WitnessExecutor;

    fn get_executor(&self) -> &Self::WitnessExecutor {
        &self.executor
    }

    fn get_zisk_stdin(&self, witness: Self::WitnessData) -> Result<ZiskStdin> {
        let buffer = to_bytes::<rkyv::rancor::Error>(&witness)?;
        
        // ZisK requires input format: [8-byte size header (u64 LE) | data bytes]
        let size = buffer.len() as u64;
        let mut stdin_data = Vec::with_capacity(8 + buffer.len());
        stdin_data.extend_from_slice(&size.to_le_bytes());
        stdin_data.extend_from_slice(&buffer);
        
        Ok(ZiskStdin::from_vec(stdin_data))
    }
}
