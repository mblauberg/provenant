#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::{Release, asset_inventory_digest, package_release};
use crate::{InstallError, ReleaseSmoke};
use provenant_core::CancelToken;
use provenant_records::{SourceDigest, VersionInfo};
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

struct Fixture {
    _temp: tempfile::TempDir,
    source: PathBuf,
    version: VersionInfo,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().canonicalize().unwrap().join("release");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir_all(source.join("skills/example")).unwrap();
        fs::write(source.join("bin/provenant"), b"fixture binary").unwrap();
        fs::set_permissions(
            source.join("bin/provenant"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(source.join("skills/example/SKILL.md"), b"skill content").unwrap();
        fs::set_permissions(
            source.join("skills/example/SKILL.md"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        fs::write(source.join("skills/example/run"), b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(
            source.join("skills/example/run"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let files = BTreeMap::from([
            (
                PathBuf::from("skills/example/SKILL.md"),
                (b"skill content".to_vec(), 0o600),
            ),
            (
                PathBuf::from("skills/example/run"),
                (b"#!/bin/sh\nexit 0\n".to_vec(), 0o700),
            ),
        ]);
        let version = VersionInfo {
            source_digest: SourceDigest::from_bytes([0x11; 32]),
            asset_digest: asset_inventory_digest(&files).unwrap(),
            target: "fixture-target".into(),
            toolchain: "fixture-toolchain".into(),
            record_schema_version: 1,
            config_schema_version: 1,
        };
        Self {
            _temp: temp,
            source,
            version,
        }
    }
}
struct Smoke(VersionInfo);
impl ReleaseSmoke for Smoke {
    fn check(&self, _: &Release, _: &CancelToken) -> io::Result<VersionInfo> {
        Ok(self.0.clone())
    }
}

#[test]
fn package_records_executable_asset_modes_and_checks_binary_identity() {
    let fixture = Fixture::new();
    let release = package_release(
        &fixture.source,
        &fixture.version,
        &Smoke(fixture.version.clone()),
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(release.manifest.asset_digest, fixture.version.asset_digest);
    assert_eq!(release.files[Path::new("bin/provenant")].1, 0o755);
    assert_eq!(release.files[Path::new("skills/example/SKILL.md")].1, 0o600);
    assert_eq!(release.files[Path::new("skills/example/run")].1, 0o700);
    assert!(fixture.source.join("release.json").is_file());
}

#[test]
fn installed_release_read_requires_exact_recorded_modes() {
    let fixture = Fixture::new();
    package_release(
        &fixture.source,
        &fixture.version,
        &Smoke(fixture.version.clone()),
        &CancelToken::new(),
    )
    .unwrap();

    assert_eq!(
        fs::metadata(fixture.source.join("bin/provenant"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(fixture.source.join("skills/example/SKILL.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(fixture.source.join("skills/example/run"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    Release::read(&fixture.source, &fixture.version.target).unwrap();

    let executable_asset = fixture.source.join("skills/example/run");
    fs::set_permissions(&executable_asset, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(matches!(
        Release::read(&fixture.source, &fixture.version.target),
        Err(crate::InstallError::Tampered(path)) if path == executable_asset
    ));
}

#[test]
fn package_refuses_asset_drift_before_publishing_completion_marker() {
    let fixture = Fixture::new();
    fs::write(
        fixture.source.join("skills/example/SKILL.md"),
        b"changed after build inventory",
    )
    .unwrap();
    let error = package_release(
        &fixture.source,
        &fixture.version,
        &Smoke(fixture.version.clone()),
        &CancelToken::new(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        InstallError::Manifest("release assets differ from build inventory")
    ));
    assert!(!fixture.source.join("release.json").exists());
}

#[test]
fn package_refuses_when_binary_smoke_identity_differs_from_manifest() {
    let fixture = Fixture::new();
    let mut binary_version = fixture.version.clone();
    binary_version.target = "other-target".into();
    let error = package_release(
        &fixture.source,
        &fixture.version,
        &Smoke(binary_version),
        &CancelToken::new(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        InstallError::Manifest("binary identity differs from release manifest")
    ));
    assert!(!fixture.source.join("release.json").exists());
}

#[test]
fn completed_release_with_a_different_manifest_returns_typed_conflict() {
    let fixture = Fixture::new();
    package_release(
        &fixture.source,
        &fixture.version,
        &Smoke(fixture.version.clone()),
        &CancelToken::new(),
    )
    .unwrap();
    let mut changed = fixture.version.clone();
    changed.toolchain = "other-toolchain".into();
    let error = package_release(
        &fixture.source,
        &changed,
        &Smoke(changed.clone()),
        &CancelToken::new(),
    )
    .unwrap_err();
    assert!(matches!(error, InstallError::ReleaseConflict { .. }));
}
