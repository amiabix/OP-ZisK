use alloy_primitives::{Address, B256};
use alloy_provider::Provider;
use anyhow::{Context, Result};
use op_zisk_client_utils::boot::BootInfoStruct;
use op_zisk_host_utils::{
    fetcher::OPZisKDataFetcher, get_agg_proof_stdin, host::OPSuccinctHost,
    metrics::MetricsGauge, witness_generation::WitnessGenerator,
};
use zisk_sdk::ProverClientBuilder;
use zisk_common::io::ZiskStdin;
use std::path::PathBuf;
use std::{
    sync::Arc,
    time::Instant,
};
use tracing::{info, warn};

use crate::{
    db::DriverDBClient, OPSuccinctRequest, ProgramConfig, RequestExecutionStatistics,
    RequestStatus, RequestType, ValidityGauge,
};

pub struct OPSuccinctProofRequester<H: OPSuccinctHost> {
    pub host: Arc<H>,
    pub fetcher: Arc<OPZisKDataFetcher>,
    pub db_client: Arc<DriverDBClient>,
    pub program_config: ProgramConfig,
    pub mock: bool,
    pub safe_db_fallback: bool,
    pub proving_timeout: u64,
    pub output_dir: PathBuf,
}

impl<H: OPSuccinctHost> OPSuccinctProofRequester<H> {
    pub fn new(
        host: Arc<H>,
        fetcher: Arc<OPZisKDataFetcher>,
        db_client: Arc<DriverDBClient>,
        program_config: ProgramConfig,
        mock: bool,
        safe_db_fallback: bool,
        proving_timeout: u64,
        output_dir: PathBuf,
    ) -> Self {
        Self {
            host,
            fetcher,
            db_client,
            program_config,
            mock,
            safe_db_fallback,
            proving_timeout,
            output_dir,
        }
    }

    /// Generates the witness for a range proof.
    pub async fn range_proof_witnessgen(&self, request: &OPSuccinctRequest) -> Result<ZiskStdin> {
        let host_args = self
            .host
            .fetch(
                request.start_block as u64,
                request.end_block as u64,
                None,
                self.safe_db_fallback,
            )
            .await?;

        if let Some(l1_head) = self.host.get_l1_head_hash(&host_args) {
            let l1_head_block_number = self.fetcher.get_l1_header(l1_head.into()).await?.number;
            self.db_client
                .update_l1_head_block_number(request.id, l1_head_block_number as i64)
                .await?;
        }

        let witness = self.host.run(&host_args).await?;
        let zisk_stdin = self.host.witness_generator().get_zisk_stdin(witness).unwrap();

        Ok(zisk_stdin)
    }

    /// Generates the witness for an aggregation proof.
    pub async fn agg_proof_witnessgen(
        &self,
        start_block: i64,
        end_block: i64,
        checkpointed_l1_block_hash: B256,
        l1_chain_id: i64,
        l2_chain_id: i64,
        prover_address: Address,
    ) -> Result<ZiskStdin> {
        // Fetch consecutive range proofs from the database.
        let range_proofs = self
            .db_client
            .get_consecutive_complete_range_proofs(
                start_block,
                end_block,
                &self.program_config.commitments,
                l1_chain_id,
                l2_chain_id,
            )
            .await?;

        // Extract proof bytes and boot_infos from database
        let mut boot_infos = Vec::new();
        let mut proof_bytes_vec = Vec::new();
        
        for range_proof in &range_proofs {
            let proof_bytes = range_proof.proof.as_ref().unwrap().clone();
            proof_bytes_vec.push(proof_bytes);
            
            // Try to load boot_info from execution_statistics JSON
            let boot_info = if let Some(boot_info_json) = range_proof.execution_statistics.get("boot_info") {
                match serde_json::from_value(boot_info_json.clone()) {
                    Ok(boot_info) => boot_info,
                    Err(_) => {
                        warn!("Failed to deserialize boot_info from execution_statistics, reconstructing");
                        Self::reconstruct_boot_info_from_range(range_proof, &self.fetcher)
                            .await
                            .unwrap_or_else(|_| BootInfoStruct {
                                l1Head: B256::ZERO,
                                l2PreRoot: B256::ZERO,
                                l2PostRoot: B256::ZERO,
                                l2BlockNumber: range_proof.end_block as u64,
                                rollupConfigHash: B256::from_slice(&range_proof.rollup_config_hash),
                            })
                    }
                }
            } else {
                Self::reconstruct_boot_info_from_range(range_proof, &self.fetcher)
                    .await
                    .unwrap_or_else(|_| BootInfoStruct {
                        l1Head: B256::ZERO,
                        l2PreRoot: B256::ZERO,
                        l2PostRoot: B256::ZERO,
                        l2BlockNumber: range_proof.end_block as u64,
                        rollupConfigHash: B256::from_slice(&range_proof.rollup_config_hash),
                    })
            };
            boot_infos.push(boot_info);
        }

        // This can fail for a few reasons:
        // 1. The L1 RPC is down (e.g. error code 32001). Double-check the L1 RPC is running
        //    correctly.
        // 2. The L1 head was re-orged and the block is no longer available. This is unlikely given
        //    we wait for 3 confirmations on a transaction.
        let headers = self
            .fetcher
            .get_header_preimages(&boot_infos, checkpointed_l1_block_hash)
            .await
            .context("Failed to get header preimages")?;

        // Convert range vkey commitment to u32 array format
        // The commitment is stored as B256, we need to convert to [u32; 8]
        let range_vkey_commitment = self.program_config.commitments.range_vkey_commitment;
        // Convert B256 to [u32; 8] - each u32 is 4 bytes, so 8 u32s = 32 bytes
        let mut range_vkey_u32 = [0u32; 8];
        for (i, chunk) in range_vkey_commitment.as_slice().chunks(4).enumerate().take(8) {
            range_vkey_u32[i] = u32::from_le_bytes(chunk.try_into().unwrap());
        }
        
        let stdin = get_agg_proof_stdin(
            proof_bytes_vec,
            boot_infos,
            headers,
            range_vkey_u32,
            checkpointed_l1_block_hash,
            prover_address,
        )
        .context("Failed to get agg proof stdin")?;

        Ok(stdin)
    }

    /// Requests a range proof using ZisK SDK.
    pub async fn request_range_proof(&self, stdin: ZiskStdin) -> Result<Vec<u8>> {
        // Build ZisK prover for range proof
        let prover = ProverClientBuilder::new()
            .emu()
            .prove()
            .elf_path(self.program_config.range_elf_path.clone())
            .proving_key_path(self.program_config.range_proving_key_path.clone())
            .save_proofs(true)
            .output_dir(self.output_dir.clone())
            .verify_proofs(true)
            .aggregation(true)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build ZisK prover: {}", e))?;

        // Generate proof
        let result = prover.prove(stdin)
            .map_err(|e| {
                ValidityGauge::RangeProofRequestErrorCount.increment(1.0);
                anyhow::anyhow!("Failed to generate range proof: {}", e)
            })?;

        // Extract proof bytes
        let proof_bytes = result.proof.proof
            .ok_or_else(|| anyhow::anyhow!("Proof generation returned no proof"))?
            .iter()
            .flat_map(|&x| x.to_le_bytes())
            .collect();

        Ok(proof_bytes)
    }

    /// Requests an aggregation proof using ZisK SDK.
    pub async fn request_agg_proof(&self, stdin: ZiskStdin) -> Result<Vec<u8>> {
        // Build ZisK prover for aggregation proof
        let prover = ProverClientBuilder::new()
            .emu()
            .prove()
            .elf_path(self.program_config.agg_elf_path.clone())
            .proving_key_path(self.program_config.agg_proving_key_path.clone())
            .save_proofs(true)
            .output_dir(self.output_dir.clone())
            .verify_proofs(true)
            .aggregation(true)
            .final_snark(true)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build ZisK prover: {}", e))?;

        // Generate proof
        let result = prover.prove(stdin)
            .map_err(|e| {
                ValidityGauge::AggProofRequestErrorCount.increment(1.0);
                anyhow::anyhow!("Failed to generate aggregation proof: {}", e)
            })?;

        // Extract proof bytes
        let proof_bytes = result.proof.proof
            .ok_or_else(|| anyhow::anyhow!("Proof generation returned no proof"))?
            .iter()
            .flat_map(|&x| x.to_le_bytes())
            .collect();

        Ok(proof_bytes)
    }

    /// Generates a mock range proof and writes the execution statistics to the database.
    pub async fn generate_mock_range_proof(
        &self,
        request: &OPSuccinctRequest,
        stdin: ZiskStdin,
    ) -> Result<Vec<u8>> {
        info!(
            request_id = request.id,
            request_type = ?request.req_type,
            start_block = request.start_block,
            end_block = request.end_block,
            "Executing mock range proof"
        );

        let start_time = Instant::now();
        
        // Build ZisK prover for execution (not proof generation)
        let prover = ProverClientBuilder::new()
            .emu()
            .verify_constraints()
            .elf_path(self.program_config.range_elf_path.clone())
            .proving_key_path(self.program_config.range_proving_key_path.clone())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build ZisK prover: {}", e))?;

        // Execute to get statistics
        let result = tokio::task::spawn_blocking(move || {
            prover.verify_constraints(stdin)
        })
        .await?
        .map_err(|e| {
            ValidityGauge::ExecutionErrorCount.increment(1.0);
            anyhow::anyhow!("Failed to execute range program: {}", e)
        })?;

        let execution_duration = start_time.elapsed().as_secs();

        info!(
            request_id = request.id,
            request_type = ?request.req_type,
            start_block = request.start_block,
            end_block = request.end_block,
            duration_s = execution_duration,
            "Executed mock range proof.",
        );

        let execution_statistics = RequestExecutionStatistics::from_executor_stats(result.stats);

        // Write the execution data to the database.
        self.db_client
            .insert_execution_statistics(
                request.id,
                serde_json::to_value(execution_statistics)?,
                execution_duration as i64,
            )
            .await?;

        // Return empty proof bytes for mock mode
        Ok(Vec::new())
    }

    /// Generates a mock aggregation proof.
    pub async fn generate_mock_agg_proof(
        &self,
        request: &OPSuccinctRequest,
        stdin: ZiskStdin,
    ) -> Result<Vec<u8>> {
        let start_time = Instant::now();
        
        // Build ZisK prover for execution (not proof generation)
        let prover = ProverClientBuilder::new()
            .emu()
            .verify_constraints()
            .elf_path(self.program_config.agg_elf_path.clone())
            .proving_key_path(self.program_config.agg_proving_key_path.clone())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build ZisK prover: {}", e))?;

        // Execute to get statistics
        let result = tokio::task::spawn_blocking(move || {
            prover.verify_constraints(stdin)
        })
        .await?
        .map_err(|e| {
            ValidityGauge::ExecutionErrorCount.increment(1.0);
            anyhow::anyhow!("Failed to execute aggregation program: {}", e)
        })?;

        let execution_duration = start_time.elapsed().as_secs();

        info!(
            request_id = request.id,
            request_type = ?request.req_type,
            start_block = request.start_block,
            end_block = request.end_block,
            duration_s = execution_duration,
            "Executed mock aggregation proof.",
        );

        let execution_statistics = RequestExecutionStatistics::from_executor_stats(result.stats);

        // Write the execution data to the database.
        self.db_client
            .insert_execution_statistics(
                request.id,
                serde_json::to_value(execution_statistics)?,
                execution_duration as i64,
            )
            .await?;

        // Return empty proof bytes for mock mode
        Ok(Vec::new())
    }

    /// Handles a failed proof request.
    ///
    /// If the request is a range proof and the number of failed requests is greater than 2 or the
    /// execution status is unexecutable, the request is split into two new requests. Otherwise,
    /// add_new_ranges will insert the new request. This ensures better failure-resilience. If the
    /// request to add two range requests fails, add_new_ranges will handle it gracefully by
    /// submitting the same range.
    #[tracing::instrument(
        name = "proof_requester.handle_failed_request",
        skip(self, request, _execution_status)
    )]
    pub async fn handle_failed_request(
        &self,
        request: OPSuccinctRequest,
        _execution_status: i32,
    ) -> Result<()> {
        warn!(
            id = request.id,
            start_block = request.start_block,
            end_block = request.end_block,
            req_type = ?request.req_type,
            "Setting request to failed"
        );

        self.db_client.update_request_status(request.id, RequestStatus::Failed).await?;

        let l1_chain_id = self.fetcher.l1_provider.get_chain_id().await?;
        let l2_chain_id = self.fetcher.l2_provider.get_chain_id().await?;

        if request.end_block - request.start_block > 1 && request.req_type == RequestType::Range {
            let num_failed_requests = self
                .db_client
                .fetch_failed_request_count_by_block_range(
                    request.start_block,
                    request.end_block,
                    request.l1_chain_id,
                    request.l2_chain_id,
                    &self.program_config.commitments,
                )
                .await?;

            // NOTE: The failed_requests check here can be removed in V5.
            // ZisK doesn't have ExecutionStatus enum, so we just check failed count
            if num_failed_requests > 2 {
                info!("Splitting failed request into two: {:?}", request.id);
                let mid_block = (request.start_block + request.end_block) / 2;
                let new_requests = vec![
                    OPSuccinctRequest::create_range_request(
                        request.mode,
                        request.start_block,
                        mid_block,
                        self.program_config.commitments.range_vkey_commitment,
                        self.program_config.commitments.rollup_config_hash,
                        l1_chain_id as i64,
                        l2_chain_id as i64,
                        self.fetcher.clone(),
                    )
                    .await?,
                    OPSuccinctRequest::create_range_request(
                        request.mode,
                        mid_block,
                        request.end_block,
                        self.program_config.commitments.range_vkey_commitment,
                        self.program_config.commitments.rollup_config_hash,
                        l1_chain_id as i64,
                        l2_chain_id as i64,
                        self.fetcher.clone(),
                    )
                    .await?,
                ];

                self.db_client.insert_requests(&new_requests).await?;
            }
        }

        Ok(())
    }

    #[tracing::instrument(name = "proof_requester.handle_cancelled_request", skip(self, request))]
    pub async fn handle_cancelled_request(&self, request: OPSuccinctRequest) -> Result<()> {
        warn!(
            id = request.id,
            start_block = request.start_block,
            end_block = request.end_block,
            req_type = ?request.req_type,
            "Setting request to cancelled"
        );

        self.db_client.update_request_status(request.id, RequestStatus::Cancelled).await?;

        Ok(())
    }

    /// Generates the stdin needed for a proof.
    async fn generate_proof_stdin(&self, request: &OPSuccinctRequest) -> Result<ZiskStdin> {
        let stdin = match request.req_type {
            RequestType::Range => self.range_proof_witnessgen(request).await?,
            RequestType::Aggregation => {
                self.agg_proof_witnessgen(
                    request.start_block,
                    request.end_block,
                    B256::from_slice(request.checkpointed_l1_block_hash.as_ref().ok_or_else(
                        || anyhow::anyhow!("Aggregation proof has no checkpointed block."),
                    )?),
                    request.l1_chain_id,
                    request.l2_chain_id,
                    Address::from_slice(request.prover_address.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("Prover address must be set for aggregation proofs.")
                    })?),
                )
                .await?
            }
        };

        Ok(stdin)
    }

    /// Makes a proof request by updating statuses, generating witnesses, and then either requesting
    /// or mocking the proof depending on configuration.
    ///
    /// Note: Any error from this function will cause the proof to be retried.
    #[tracing::instrument(name = "proof_requester.make_proof_request", skip(self, request))]
    pub async fn make_proof_request(&self, request: OPSuccinctRequest) -> Result<()> {
        // Update status to WitnessGeneration.
        self.db_client.update_request_status(request.id, RequestStatus::WitnessGeneration).await?;

        info!(
            request_id = request.id,
            request_type = ?request.req_type,
            start_block = request.start_block,
            end_block = request.end_block,
            "Starting witness generation"
        );

        let witnessgen_duration = Instant::now();
        // Generate the stdin needed for the proof. If this fails, retry the request.
        let stdin = match self.generate_proof_stdin(&request).await {
            Ok(stdin) => stdin,
            Err(e) => {
                ValidityGauge::WitnessgenErrorCount.increment(1.0);
                return Err(e);
            }
        };
        let duration = witnessgen_duration.elapsed();

        self.db_client.update_witnessgen_duration(request.id, duration.as_secs() as i64).await?;

        info!(
            request_id = request.id,
            start_block = request.start_block,
            end_block = request.end_block,
            request_type = ?request.req_type,
            duration_s = duration.as_secs(),
            "Completed witness generation"
        );

        // For mock mode, update status to Execution before proceeding.
        if self.mock {
            self.db_client.update_request_status(request.id, RequestStatus::Execution).await?;
        }

        match request.req_type {
            RequestType::Range => {
                if self.mock {
                    let proof_bytes = self.generate_mock_range_proof(&request, stdin).await?;
                    self.db_client.update_proof_to_complete(request.id, &proof_bytes).await?;
                } else {
                    let proof_bytes = self.request_range_proof(stdin).await?;
                    // Store proof bytes directly (ZisK generates proofs synchronously)
                    self.db_client.update_proof_to_complete(request.id, &proof_bytes).await?;

                    info!(
                        proof_id = request.id,
                        start_block = request.start_block,
                        end_block = request.end_block,
                        proof_request_time = ?request.created_at,
                        total_tx_fees = %request.total_tx_fees,
                        total_transactions = request.total_nb_transactions,
                        witnessgen_duration_s = request.witnessgen_duration,
                        total_eth_gas_used = request.total_eth_gas_used,
                        total_l1_fees = %request.total_l1_fees,
                        "Range proof generated with ZisK"
                    );
                }
            }
            RequestType::Aggregation => {
                if self.mock {
                    let proof_bytes = self.generate_mock_agg_proof(&request, stdin).await?;
                    self.db_client.update_proof_to_complete(request.id, &proof_bytes).await?;
                } else {
                    let proof_bytes = self.request_agg_proof(stdin).await?;
                    // Store proof bytes directly (ZisK generates proofs synchronously)
                    self.db_client.update_proof_to_complete(request.id, &proof_bytes).await?;

                    info!(
                        proof_id = request.id,
                        start_block = request.start_block,
                        end_block = request.end_block,
                        proof_request_time = ?request.created_at,
                        witnessgen_duration_s = request.witnessgen_duration,
                        checkpointed_l1_block_number = request.checkpointed_l1_block_number,
                        checkpointed_l1_block_hash = ?request.checkpointed_l1_block_hash,
                        "Aggregation proof generated with ZisK"
                    );
                }
            }
        }

        Ok(())
    }

    /// Helper function to reconstruct boot_info from block range when not stored in DB
    async fn reconstruct_boot_info_from_range(
        range_proof: &OPSuccinctRequest,
        fetcher: &Arc<OPZisKDataFetcher>,
    ) -> Result<BootInfoStruct> {
        // Use checkpointed L1 head if available, otherwise fetch it
        let l1_head = if let Some(l1_head_bytes) = &range_proof.checkpointed_l1_block_hash {
            B256::from_slice(l1_head_bytes)
        } else {
            // Fetch L1 head for the block range
            use alloy_eips::BlockNumberOrTag;
            fetcher
                .l1_provider
                .get_block_by_number(BlockNumberOrTag::Number(range_proof.start_block as u64))
                .await?
                .ok_or_else(|| anyhow::anyhow!("L1 block not found"))?
                .header
                .hash
        };
        
        // Create boot_info from block range
        // Note: l2PreRoot and l2PostRoot are set to ZERO as we don't have them
        Ok(BootInfoStruct {
            l1Head: l1_head,
            l2PreRoot: B256::ZERO, // These should come from actual execution
            l2PostRoot: B256::ZERO,
            l2BlockNumber: range_proof.end_block as u64,
            rollupConfigHash: B256::from_slice(&range_proof.rollup_config_hash),
        })
    }
}
