use alloy_consensus::Header;
use alloy_primitives::{keccak256, Address, B256};
use anyhow::Result;
use op_zisk_client_utils::{boot::BootInfoStruct, types::AggregationInputs};
use std::path::Path;
use zisk_common::io::ZiskStdin;

/// Derive a deterministic "range vkey commitment" from the range program ELF bytes.
///
/// For the current ZisK migration, this serves as a stable commitment identifier that can be
/// configured and checked consistently across tooling.
pub fn range_vkey_commitment_from_elf(range_elf_path: &Path) -> Result<B256> {
    let bytes = std::fs::read(range_elf_path)?;
    Ok(keccak256(bytes))
}

/// Convert a `B256` into 8 big-endian u32 words.
///
/// This matches the historical `[u32; 8]` representation used in OP-ZisK inputs.
pub fn b256_to_u32x8_be(v: B256) -> [u32; 8] {
    let b = v.as_slice();
    let mut out = [0u32; 8];
    for i in 0..8 {
        out[i] = u32::from_be_bytes(b[i * 4..(i + 1) * 4].try_into().expect("slice length"));
    }
    out
}

/// Get the stdin for the aggregation proof.
/// Note: ZisK doesn't support proof embedding like SP1, so proofs must be verified externally
/// before aggregation. This function serializes the aggregation inputs and headers.
pub fn get_agg_proof_stdin(
    _proofs: Vec<Vec<u8>>, // Proofs are now just bytes, verification happens externally
    boot_infos: Vec<BootInfoStruct>,
    headers: Vec<Header>,
    multi_block_vkey: [u32; 8], // Changed from SP1VerifyingKey to raw array
    latest_checkpoint_head: B256,
    prover_address: Address,
) -> Result<ZiskStdin> {
    // Serialize aggregation inputs
    let agg_inputs = AggregationInputs {
        boot_infos,
        latest_l1_checkpoint_head: latest_checkpoint_head,
        multi_block_vkey,
        prover_address,
    };
    let agg_inputs_bytes = bincode::serialize(&agg_inputs)?;
    
    // Serialize headers using serde_cbor (as in original)
    let headers_bytes = serde_cbor::to_vec(&headers)?;
    
    // Combine into single byte vector: [agg_inputs_len: u64][agg_inputs_bytes][headers_len: u64][headers_bytes]
    let mut combined = Vec::new();
    combined.extend_from_slice(&(agg_inputs_bytes.len() as u64).to_le_bytes());
    combined.extend_from_slice(&agg_inputs_bytes);
    combined.extend_from_slice(&(headers_bytes.len() as u64).to_le_bytes());
    combined.extend_from_slice(&headers_bytes);
    
    Ok(ZiskStdin::from_vec(combined))
}
