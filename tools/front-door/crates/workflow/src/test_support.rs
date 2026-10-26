pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
    provenant_os::fixture::tempdir(|root| tempfile::Builder::new().prefix(".tmp").tempdir_in(root))
}

pub(crate) fn short_tempdir(socket_name: &str) -> std::io::Result<tempfile::TempDir> {
    provenant_os::fixture::short_tempdir(socket_name, |root| {
        tempfile::Builder::new().prefix(".tmp").tempdir_in(root)
    })
}
