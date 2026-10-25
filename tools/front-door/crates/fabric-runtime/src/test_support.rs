pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
    provenant_supervisor::fixture::tempdir(".tmp")
}
