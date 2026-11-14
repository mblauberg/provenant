pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
    provenant_os::fixture::tempdir(|root| tempfile::Builder::new().prefix(".tmp").tempdir_in(root))
}

pub(crate) fn observation_settings() -> crate::coordination::ObservationSettings {
    crate::coordination::ObservationSettings {
        local_interval: provenant_core::DurationMicros::from_micros(500_000),
        source: provenant_config::Origin::Compiled {
            profile: provenant_config::Profile::Balanced,
        },
    }
}
