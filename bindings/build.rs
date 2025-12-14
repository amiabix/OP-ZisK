use std::process::Command;
use std::{fs, fmt::Debug, path::Path};

use anyhow::Context;
use cargo_metadata::MetadataCommand;

fn ensure_codegen_stub<P>(bindings_codegen_path: P) -> anyhow::Result<()>
where
    P: AsRef<Path> + Debug,
{
    let bindings_codegen_path = bindings_codegen_path.as_ref();
    let mod_rs_path = bindings_codegen_path.join("mod.rs");
    if mod_rs_path.exists() {
        return Ok(());
    }

    fs::create_dir_all(bindings_codegen_path)
        .with_context(|| format!("Failed to create bindings codegen dir at {bindings_codegen_path:?}"))?;

    fs::write(
        &mod_rs_path,
        "// Auto-generated stub.\n\
// Contract bindings were not generated (missing dependencies or forge unavailable).\n\
// Initialize contract dependencies and re-run the build to regenerate bindings.\n",
    )
    .with_context(|| format!("Failed to write bindings stub at {mod_rs_path:?}"))?;

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let metadata =
        MetadataCommand::new().no_deps().exec().context("Failed to get cargo metadata")?;

    let workspace_root = metadata.workspace_root;
    let bindings_codegen_path = workspace_root.join("bindings/src/codegen");
    let contracts_package_path = workspace_root.join("contracts");

    // Check if the contracts directory exists.
    if !contracts_package_path.exists() {
        println!("cargo:warning=Contracts directory not found at {contracts_package_path:?}");
        ensure_codegen_stub(&bindings_codegen_path)?;
        return Ok(());
    }

    println!("cargo:rerun-if-changed={}", contracts_package_path.join("src"));
    println!("cargo:rerun-if-changed={}", contracts_package_path.join("remappings.txt"));
    println!("cargo:rerun-if-changed={}", contracts_package_path.join("foundry.toml"));

    // Check if forge is available
    if Command::new("forge").arg("--version").output().is_err() {
        println!("cargo:warning=Forge not found in PATH. Skipping bindings generation.");
        ensure_codegen_stub(&bindings_codegen_path)?;
        return Ok(());
    }

    // If contract dependencies are not present (e.g. submodules not initialized), skip generation
    // instead of failing the entire Rust workspace build.
    let required_path = contracts_package_path.join(
        "lib/optimism/packages/contracts-bedrock/src/dispute/lib/Types.sol",
    );
    if !required_path.exists() {
        println!(
            "cargo:warning=Contract dependency missing at {:?}. Skipping bindings generation. \
Initialize dependencies (e.g. `git submodule update --init --recursive`) and retry if you need bindings.",
            required_path
        );
        ensure_codegen_stub(&bindings_codegen_path)?;
        return Ok(());
    }

    // Use 'forge bind' to generate bindings for only the contracts we need for E2E tests
    let mut forge_command = Command::new("forge");
    forge_command.args([
        "bind",
        "--bindings-path",
        bindings_codegen_path.as_str(),
        "--module",
        "--overwrite",
        "--skip-extra-derives",
    ]);

    // Only generate bindings for the contracts we actually need for E2E testing
    let required_contracts = [
        "DisputeGameFactory",
        "SuperchainConfig",
        "MockOptimismPortal2",
        "AnchorStateRegistry",
        "AccessManager",
        "SP1MockVerifier",
        "OPSuccinctFaultDisputeGame",
        "ERC1967Proxy",
        "MockPermissionedDisputeGame",
        // Also include interfaces that we need
        "IDisputeGameFactory",
        "IDisputeGame",
        "IFaultDisputeGame",
    ];

    // Create a regex pattern that matches any of our required contracts
    let select_pattern = format!("^({})$", required_contracts.join("|"));
    forge_command.args(["--select", &select_pattern]);

    let status = forge_command.current_dir(&contracts_package_path).status()?;

    if !status.success() {
        anyhow::bail!("Forge command failed with exit code: {}", status);
    }

    Ok(())
}
