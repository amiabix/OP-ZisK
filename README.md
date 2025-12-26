# OP-ZisK

OP-ZisK is an experimental fork of `succinctlabs/op-succinct` that explores using ZisK as the zkVM backend for OP Stack proving workflows.

> **Note**: This is a personal research project and is not affiliated with or endorsed by the ZisK team. This repository builds on the excellent foundation work of the Succinct Labs team and adapts it to explore ZisK-based proving workflows.

## Current Status

The system uses a two-phase architecture: 
> Native execution collects preimages dynamically
> ZisK proof generation. 

v0.1 demonstrates testing: 
> Devnet setup
> Witness generation
> ZisK proof generation (~16s for 50,217,738 steps)

## Repository Overview

> [!CAUTION]
> This repository is experimental and containa few unstable sections of the code.

## Quick Start

### Prerequisites

- Rust toolchain (latest stable)
- ZisK CLI tools (`cargo-zisk`, `ziskemu`) installed
- Go 1.21+ (for devnet testing)
- Access to L1/L2 RPC endpoints (or use local devnet)

### Generating a Validity Proof

1. **Start a devnet** (or configure RPC endpoints):
   ```bash
   cd tests
   go test -v -timeout=60m -run TestValidityProposer_SingleSubmission ./e2e/validity/proving/
   ```

2. **Build the range program**:
   ```bash
   cargo-zisk build --release --manifest-path programs/range/ethereum/Cargo.toml
   ```

3. **Perform ROM setup** (one-time, takes a few minutes):
   ```bash
   cargo-zisk rom-setup \
     -e target/riscv64ima-zisk-zkvm-elf/release/range \
     -k $HOME/.zisk/provingKey
   ```

4. **Generate witness data**:
   ```bash
   cargo run --release --bin prove-range -- \
     --env-file .devnet.env \
     --start 2 \
     --end 3 \
     --save-input witness.bin \
     --safe-db-fallback
   ```

5. **Generate proof**:
   ```bash
   cargo-zisk prove \
     -e target/riscv64ima-zisk-zkvm-elf/release/range \
     -i witness.bin \
     -k $HOME/.zisk/provingKey \
     -o proof \
     -a -y
   ```

## Development

To configure or change the OP-ZisK codebase, refer to the repository source and release notes.

### Configuration

RPC endpoints can be configured via:
- Environment variables (`.env` files)
- DevnetManager API (automatic discovery when running Go tests)
- Command-line arguments

See `utils/config/src/lib.rs` for the unified configuration system.

### Architecture

The system follows a host-guest architecture:
- **Host**: Rust binaries (`prove-range`, `validity`) that fetch data from RPCs and generate witness files
- **Guest**: ZisK zkVM programs that execute inside the VM to validate state transitions
- **Prover**: ZisK CLI tools that generate proofs from ELF + witness data

## Acknowledgments

This repository is built on the foundation of excellent work from:

- **[Succinct Labs](https://succinctlabs.com/)**: This project is a fork of [`succinctlabs/op-succinct`](https://github.com/succinctlabs/op-succinct). The core architecture, design patterns, and much of the implementation work comes from the Succinct team. This fork adapts their work to explore ZisK as an alternative zkVM backend.
- [OP Stack](https://docs.optimism.io/stack/getting-started): Modular software components for building L2 blockchains.
- [Kona](https://github.com/anton-rs/kona/tree/main): A portable implementation of the OP Stack rollup state transition, namely the derivation pipeline and the block execution logic.
- [ZisK](https://github.com/0xPolygonHermez/zisk): The zkVM used by this fork for proof generation.
