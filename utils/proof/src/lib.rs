use std::sync::Arc;

use op_zisk_host_utils::fetcher::OPZisKDataFetcher;

/// Get the range ELF depending on the feature flag.
pub fn get_range_elf_embedded() -> &'static [u8] {
    cfg_if::cfg_if! {
        if #[cfg(feature = "celestia")] {
            use op_zisk_elfs::CELESTIA_RANGE_ELF_EMBEDDED;

            CELESTIA_RANGE_ELF_EMBEDDED
        } else if #[cfg(feature = "eigenda")] {
            use op_zisk_elfs::EIGENDA_RANGE_ELF_EMBEDDED;

            EIGENDA_RANGE_ELF_EMBEDDED
        } else {
            use op_zisk_elfs::RANGE_ELF_EMBEDDED;

            RANGE_ELF_EMBEDDED
        }
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "celestia")] {
        use op_zisk_celestia_host_utils::host::CelestiaOPZisKHost;

        /// Initialize the Celestia host.
        pub fn initialize_host(
            fetcher: Arc<OPZisKDataFetcher>,
        ) -> Arc<CelestiaOPZisKHost> {
            tracing::info!("Initializing host with Celestia DA");
            Arc::new(CelestiaOPZisKHost::new(fetcher))
        }
    } else if #[cfg(feature = "eigenda")] {
        use op_zisk_eigenda_host_utils::host::EigenDAOPZisKHost;

        /// Initialize the EigenDA host.
        pub fn initialize_host(
            fetcher: Arc<OPZisKDataFetcher>,
        ) -> Arc<EigenDAOPZisKHost> {
            tracing::info!("Initializing host with EigenDA");
            Arc::new(EigenDAOPZisKHost::new(fetcher))
        }
    } else {
        use op_zisk_ethereum_host_utils::host::SingleChainOPZisKHost;

        /// Initialize the default (ETH-DA) host.
        pub fn initialize_host(
            fetcher: Arc<OPZisKDataFetcher>,
        ) -> Arc<SingleChainOPZisKHost> {
            tracing::info!("Initializing host with Ethereum DA");
            Arc::new(SingleChainOPZisKHost::new(fetcher))
        }
    }
}
