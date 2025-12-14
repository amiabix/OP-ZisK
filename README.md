# OP-ZisK

OP-ZisK is an experimental fork of `succinctlabs/op-succinct` that explores using ZisK as the zkVM backend for OP Stack proving workflows.

> **Note**: This is a personal research project and is not affiliated with or endorsed by the ZisK team. This repository builds on the excellent foundation work of the Succinct Labs team and adapts it to explore ZisK-based proving workflows.

## Repository Overview

> [!CAUTION]
> This repository is experimental and may contain unstable code.
> If you are looking for a production-oriented implementation, refer to `succinctlabs/op-succinct` and its releases.

The repository is organized into the following directories:

- `programs`: The programs for proving the execution and derivation of the L2 state transitions and proof aggregation.
- `validity`: The implementation of the OP-ZisK validity service.
- `fault-proof`: The implementation of the OP-ZisK fault-proof service.
- `scripts`: Scripts for testing and deploying OP-ZisK.
- `utils`: Shared utilities for the host, client, and proposer.

Notes on upstream contract verification and the `contracts/` directory are kept in `archive/UPSTREAM_CONTRACTS.md`.

## Development

To configure or change the OP-ZisK codebase, refer to the repository source and release notes.

## Acknowledgments

This repository is built on the foundation of excellent work from:

- **[Succinct Labs](https://succinctlabs.com/)**: This project is a fork of [`succinctlabs/op-succinct`](https://github.com/succinctlabs/op-succinct). The core architecture, design patterns, and much of the implementation work comes from the Succinct team. This fork adapts their work to explore ZisK as an alternative zkVM backend.
- [OP Stack](https://docs.optimism.io/stack/getting-started): Modular software components for building L2 blockchains.
- [Kona](https://github.com/anton-rs/kona/tree/main): A portable implementation of the OP Stack rollup state transition, namely the derivation pipeline and the block execution logic.
- [ZisK](https://github.com/0xPolygonHermez/zisk): The zkVM used by this fork for proof generation.
