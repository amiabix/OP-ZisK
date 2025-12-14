// ZisK build is handled via justfile and cargo-zisk
// This module is kept for compatibility but build functions are no longer needed

/// Build all the native programs and the native host runner. Optional flag to build the zkVM
/// programs.
pub fn build_all() {
    // build_program("aggregation", "aggregation-elf", None);
    // build_program("range/ethereum", "range-elf-bump", None);
    // build_program("range/ethereum", "range-elf-embedded", Some(vec!["embedded".to_string()]));
    // build_program(
    //     "range/celestia",
    //     "celestia-range-elf-embedded",
    //     Some(vec!["embedded".to_string()]),
    // );
    // build_program(
    //     "range/eigenda",
    //     "eigenda-range-elf-embedded",
    //     Some(vec!["embedded".to_string()]),
    // );
}
