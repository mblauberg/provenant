//! Best-effort local capture. Never substitute the current host for history.
use provenant_core::WallTime;
use provenant_records::{
    ExtraFields,
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
    static EXECUTABLE: std::sync::OnceLock<Option<HashedBytes>> = std::sync::OnceLock::new();
    if let Some(executable) = EXECUTABLE
        .get_or_init(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| bytes(&path, 256 << 20))
                .map(|bytes| hash(&bytes))
        })
        .clone()
    {
        // Bind adjacent release metadata to these executable bytes, rather
        // than the installation's potentially upgraded current selector.
        static RELEASE_SOURCE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        let release_source = RELEASE_SOURCE.get_or_init(|| {
            let path = std::env::current_exe().ok()?;
            if path.file_name()? != "provenant" || path.parent()?.file_name()? != "bin" {
                return None;
            }
            let manifest: provenant_records::ReleaseManifest = serde_json::from_slice(&bytes(
                &path.parent()?.parent()?.join("release.json"),
                1 << 20,
            )?)
            .ok()?;
            if manifest.schema_version != 1 {
                return None;
            }
            let binary = manifest
                .files
                .iter()
                .find(|file| file.path.as_path_text() == Some("bin/provenant"))?;
            (provenant_core::hex(binary.digest.bytes()) == executable.sha256
                && binary.bytes == executable.bytes)
                .then(|| manifest.source_digest.to_string())
        });
        context.release_source_digest = release_source.clone().map(|digest| {
            observe(
                digest,
                "adjacent_release_manifest_matching_executable",
                "declared release source inventory digest",
                at,
            )
        });
        context.provenant_digest = Some(observe(
            executable,
            "current_exe",
            "complete host executable bytes",
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
