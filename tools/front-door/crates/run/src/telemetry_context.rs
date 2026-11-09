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
    let path = std::env::current_exe().ok()?;
    if path.file_name()? != "provenant" || path.parent()?.file_name()? != "bin" {
        return None;
    }
    let size = std::fs::metadata(&path).ok()?.len();
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
