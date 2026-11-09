//! Best-effort local capture. Never substitute the current host for history.
use provenant_core::WallTime;
use provenant_records::{
    ExtraFields, VersionInfo,
    run::TaskRequest,
    telemetry_context::{HashedBytes, Observation, TelemetryContext},
};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

fn observe<T>(value: T, source: &str, scope: &str, effective_at: WallTime) -> Observation<T> {
    Observation {
        value,
        source: source.into(),
        scope: scope.into(),
        effective_at,
    }
}

fn bytes(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= cap).then_some(bytes)
}

fn hash(bytes: &[u8]) -> HashedBytes {
    HashedBytes {
        sha256: provenant_core::hex(&Sha256::digest(bytes)),
        bytes: bytes.len() as u64,
    }
}

/// Compiled identity of the running binary, supplied by its entry point.
/// Without it, executable and release observations stay unknown.
static BUILD_IDENTITY: std::sync::OnceLock<VersionInfo> = std::sync::OnceLock::new();

pub fn set_build_identity(identity: VersionInfo) {
    let _ = BUILD_IDENTITY.set(identity);
}

struct InstalledRelease {
    executable: HashedBytes,
    source_digest: String,
}

static INSTALLED_RELEASE: std::sync::OnceLock<Option<InstalledRelease>> =
    std::sync::OnceLock::new();

/// Run creation sits on the launch path, so it never reads or hashes the
/// executable: unoptimised hashing of a large binary takes seconds. The
/// installer verified every manifest file's size and SHA-256. A manifest
/// whose source, asset, target and toolchain match this binary's compiled
/// identity, and whose executable size matches, describes these bytes, not an
/// upgraded selector. Development builds have no such manifest and stay unknown.
fn installed_release() -> Option<InstalledRelease> {
    let build = BUILD_IDENTITY.get()?;
    release_for_executable(&std::env::current_exe().ok()?, build)
}

/// `current_exe` may be the `~/.local/bin/provenant` symlink (macOS), so
/// resolve it before deriving the release directory on every platform.
fn release_for_executable(exe: &Path, build: &VersionInfo) -> Option<InstalledRelease> {
    let path = std::fs::canonicalize(exe).ok()?;
    let path = path.as_path();
    if path.file_name()? != "provenant" || path.parent()?.file_name()? != "bin" {
        return None;
    }
    let size = std::fs::metadata(path).ok()?.len();
    let manifest: provenant_records::ReleaseManifest = serde_json::from_slice(&bytes(
        &path.parent()?.parent()?.join("release.json"),
        1 << 20,
    )?)
    .ok()?;
    if manifest.schema_version != 1
        || manifest.source_digest != build.source_digest
        || manifest.asset_digest != build.asset_digest
        || manifest.target != build.target
        || manifest.toolchain != build.toolchain
    {
        return None;
    }
    let binary = manifest
        .files
        .iter()
        .find(|file| file.path.as_path_text() == Some("bin/provenant"))?;
    (binary.bytes == size).then(|| InstalledRelease {
        executable: HashedBytes {
            sha256: provenant_core::hex(binary.digest.bytes()),
            bytes: binary.bytes,
        },
        source_digest: manifest.source_digest.to_string(),
    })
}

pub(crate) fn capture(task: &TaskRequest, at: WallTime) -> ExtraFields {
    let mut context = TelemetryContext {
        provenant_version: Some(observe(
            env!("CARGO_PKG_VERSION").into(),
            "compiled_package",
            "provenant-run package",
            at,
        )),
        git_base: task
            .base
            .as_deref()
            .and_then(|base| provenant_core::GitOid::parse(base).ok())
            .map(|base| observe(base, "canonical_request", "pinned task base commit", at)),
        git_head: task
            .head
            .as_deref()
            .and_then(|head| provenant_core::GitOid::parse(head).ok())
            .map(|head| {
                observe(
                    head,
                    "canonical_request",
                    "pinned task input head commit",
                    at,
                )
            }),
        provenant_experiment: task
            .context
            .as_ref()
            .and_then(|work| work.experiment_arm.clone())
            .map(|arm| observe(arm, "work_context", "Provenant experiment arm", at)),
        ..TelemetryContext::default()
    };
    if let Some(release) = INSTALLED_RELEASE.get_or_init(installed_release) {
        context.provenant_digest = Some(observe(
            release.executable.clone(),
            "installed_release_manifest",
            "install-verified release manifest digest of the running executable, matched by compiled build identity and size; bytes are not rehashed at launch",
            at,
        ));
        context.release_source_digest = Some(observe(
            release.source_digest.clone(),
            "adjacent_release_manifest_matching_build_identity",
            "declared release source inventory digest",
            at,
        ));
    }
    // Cloud execution does not observe the local checkout as its instructions.
    if task.cloud.is_none() {
        let root = task
            .access
            .source_root
            .as_deref()
            .or_else(|| task.writer.as_ref().map(|writer| writer.project.as_path()));
        if let Some(instructions) = root.and_then(|root| bytes(&root.join("AGENTS.md"), 1 << 20)) {
            context.instructions = Some(observe(
                hash(&instructions),
                "source_checkout",
                "observed source-root AGENTS.md bytes; not full effective session instructions",
                at,
            ));
        }
    }
    let mut extra = ExtraFields::new();
    if let Ok(value) = serde_json::to_value(context) {
        extra.insert(TelemetryContext::TRACE_KEY.into(), value);
    }
    extra
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::{FileDigest, PathWire, ReleaseFile, ReleaseManifest, SourceDigest};

    #[cfg(unix)]
    #[test]
    fn release_is_found_through_install_symlink() {
        let binary = b"fake provenant";
        let build = VersionInfo {
            extra: Default::default(),
            source_digest: SourceDigest::from_bytes([1; 32]),
            asset_digest: SourceDigest::from_bytes([2; 32]),
            target: "t".into(),
            toolchain: "tc".into(),
            record_schema_version: 1,
            config_schema_version: 1,
        };
        let manifest = ReleaseManifest {
            extra: Default::default(),
            schema_version: 1,
            source_digest: build.source_digest,
            asset_digest: build.asset_digest,
            target: build.target.clone(),
            toolchain: build.toolchain.clone(),
            files: vec![ReleaseFile {
                extra: Default::default(),
                path: PathWire::from_relative(
                    &provenant_core::RelativePath::parse("bin/provenant").unwrap(),
                )
                .unwrap(),
                digest: FileDigest::from_bytes([3; 32]),
                bytes: binary.len() as u64,
                mode: 0o755,
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("releases/abc");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        std::fs::write(release.join("bin/provenant"), binary).unwrap();
        std::fs::write(
            release.join("release.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join(".local/bin")).unwrap();
        let link = dir.path().join(".local/bin/provenant");
        std::os::unix::fs::symlink(release.join("bin/provenant"), &link).unwrap();

        let found = release_for_executable(&link, &build).expect("manifest found");
        assert_eq!(found.executable.bytes, binary.len() as u64);
    }
}
