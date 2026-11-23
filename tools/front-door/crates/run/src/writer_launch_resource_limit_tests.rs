use super::*;

#[test]
fn unavailable_resource_settings_use_compiled_limits() -> std::io::Result<()> {
    let state = tempfile::tempdir()?;
    std::fs::write(
        state.path().join("config.toml"),
        "schema_version=1\n[domains.run]\ngrant_limit=0\n",
    )?;
    let policy = provenant_records::policy::Policy::default();
    let limits = launch_resource_limits(state.path(), &policy);
    let defaults = ProfileLimits::default();
    assert_eq!(limits.limits.grant_limit, defaults.grant_limit);
    assert_eq!(
        limits.grant_origin,
        provenant_records::diagnostic::Layer::Default
    );
    assert_eq!(
        limits.limits.profile_limit_bytes,
        defaults.profile_limit_bytes
    );
    assert_eq!(
        limits.profile_origin,
        provenant_records::diagnostic::Layer::Default
    );
    std::fs::write(
        state.path().join("config.toml"),
        "schema_version=1\n[domains.run]\ngrant_limit=128\n",
    )?;
    let explicit_default = launch_resource_limits(state.path(), &policy);
    assert_eq!(
        explicit_default.grant_origin,
        provenant_records::diagnostic::Layer::Host
    );
    assert_eq!(
        explicit_default.profile_origin,
        provenant_records::diagnostic::Layer::Default
    );
    std::fs::write(
        state.path().join("config.toml"),
        "schema_version=1\n[domains.run]\ngrant_limit=256\nprofile_limit_bytes=1048576\n",
    )?;
    let configured = launch_resource_limits(state.path(), &policy);
    assert_eq!(configured.limits.grant_limit, 256);
    assert_eq!(
        configured.grant_origin,
        provenant_records::diagnostic::Layer::Host
    );
    assert_eq!(configured.limits.profile_limit_bytes, 1048576);
    assert_eq!(
        configured.profile_origin,
        provenant_records::diagnostic::Layer::Host
    );
    let unrelated =
        configured.annotate(provenant_confinement::permissions::PermissionError::ProtectedOverlap);
    assert!(matches!(
        unrelated,
        provenant_confinement::permissions::PermissionError::ProtectedOverlap
    ));
    Ok(())
}
