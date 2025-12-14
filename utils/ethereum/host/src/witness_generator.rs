use anyhow::Result;
use async_trait::async_trait;
use kona_proof::l1::OracleBlobProvider;
use op_zisk_client_utils::witness::DefaultWitnessData;
use op_zisk_ethereum_client_utils::executor::ETHDAWitnessExecutor;
use op_zisk_host_utils::witness_generation::{
    online_blob_store::OnlineBlobStore, preimage_witness_collector::PreimageWitnessCollector,
    DefaultOracleBase, WitnessGenerator,
};
use rkyv::to_bytes;
use zisk_common::io::ZiskStdin;

type WitnessExecutor = ETHDAWitnessExecutor<
    PreimageWitnessCollector<DefaultOracleBase>,
    OnlineBlobStore<OracleBlobProvider<DefaultOracleBase>>,
>;

pub struct ETHDAWitnessGenerator {
    pub executor: WitnessExecutor,
}

#[async_trait]
impl WitnessGenerator for ETHDAWitnessGenerator {
    type WitnessData = DefaultWitnessData;
    type WitnessExecutor = WitnessExecutor;

    fn get_executor(&self) -> &Self::WitnessExecutor {
        &self.executor
    }

    fn get_zisk_stdin(&self, witness: Self::WitnessData) -> Result<ZiskStdin> {
        let buffer = to_bytes::<rkyv::rancor::Error>(&witness)?;
        Ok(ZiskStdin::from_vec(buffer.to_vec()))
    }
}
