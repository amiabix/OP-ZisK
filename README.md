# OP-ZisK

OP-ZisK is an experimental fork of `succinctlabs/op-succinct` that explores using ZisK as the zkVM backend for OP Stack proving workflows.

This repository is intended for research and experimentation. It exists to build on the design and implementation work of the Succinct team and evaluate a ZisK-based proving stack.

## Repository Overview

> [!CAUTION]
> This repository is experimental and may contain unstable code.
> If you are looking for a production-oriented implementation, refer to `succinctlabs/op-succinct` and its releases.

The repository is organized into the following directories:

- `programs`: The programs for proving the execution and derivation of the L2 state transitions and proof aggregation.
- `validity`: The implementation of the `op-succinct/op-succinct` service.
- `fault-proof`: The implementation of the `op-succinct/fault-proof` service.
- `scripts`: Scripts for testing and deploying OP-ZisK.
- `utils`: Shared utilities for the host, client, and proposer.

Notes on upstream contract verification and the `contracts/` directory are kept in `archive/UPSTREAM_CONTRACTS.md`.

## Development

To configure or change the OP-ZisK codebase, refer to the repository source and release notes.

## Acknowledgments

This repo builds on and is inspired by:

- [`succinctlabs/op-succinct`](https://github.com/succinctlabs/op-succinct): the upstream project this repository is forked from.
- [OP Stack](https://docs.optimism.io/stack/getting-started): Modular software components for building L2 blockchains.
- [Kona](https://github.com/anton-rs/kona/tree/main): A portable implementation of the OP Stack rollup state transition, namely the derivation pipeline and the block execution logic.
- [SP1](https://github.com/succinctlabs/sp1): the zkVM used by the upstream project.
- [ZisK](https://github.com/0xPolygonHermez/zisk): the zkVM explored by this fork.
