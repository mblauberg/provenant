#[test]
fn configured_grants_cannot_expose_host_indexes() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::WriteGrant;
    for path in [
        "/state/work-index.sqlite3",
        "/state/work-index.sqlite3-wal",
        "/state/work-index.sqlite3-shm",
        "/state/work-index.sqlite3-journal",
        "/state/work-index.sqlite3.lock",
        "/state/fabric",
        "/state/fabric/fabric.sqlite3",
        "/state/fabric/fabric.sqlite3-wal",
        "/state/fabric/fabric.sqlite3-shm",
        "/state/fabric/fabric.sqlite3-journal",
        "/state/fabric/host.sqlite3",
        "/state/fabric/host.sqlite3-wal",
        "/state/fabric/host.sqlite3-shm",
        "/state/fabric/host.sqlite3-journal",
        "/state/fabric/host.sqlite3.lock",
    ] {
        let grant = WriteGrant {
            source: run::WriteSource::WriteRoot,
            path: path.into(),
            scope: PathScope::Subtree,
        };
        assert!(!host_indexes_protected(Path::new("/state"), &[grant]));
    }
    assert!(host_indexes_protected(Path::new("/state"), &[]));
    let temp = tempfile::tempdir()?;
    let fabric = temp.path().join("fabric");
    std::fs::create_dir(&fabric)?;
    let grant = WriteGrant {
        source: run::WriteSource::StateWrite,
        path: fabric,
        scope: PathScope::Exact,
    };
    assert!(!host_indexes_protected(temp.path(), &[grant]));
    for path in [
        "/state",
        "/state/cargo-target-seeds",
        "/state/cargo-target-seeds/generations/a/target",
    ] {
        let grant = WriteGrant {
            source: run::WriteSource::WriteRoot,
            path: path.into(),
            scope: PathScope::Subtree,
        };
        assert!(!target_seeds_protected(Path::new("/state"), &[grant]));
    }
    assert!(target_seeds_protected(Path::new("/state"), &[]));
    Ok(())
}
