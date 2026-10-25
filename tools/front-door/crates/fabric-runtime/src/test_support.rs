pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
    // Canonical roots avoid macOS temp aliases while honouring the lane's writable TMPDIR.
    tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir())?)
}
