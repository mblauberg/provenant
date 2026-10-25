pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
    provenant_supervisor::fixture::tempdir(".tmp")
}

pub(crate) fn short_tempdir(socket_name: &str) -> std::io::Result<tempfile::TempDir> {
    provenant_supervisor::fixture::short_tempdir(".tmp", socket_name)
}
