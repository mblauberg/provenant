//! Explicit provider refresh and read-only access to the last successful listing.
use provenant_core::Roots;
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use provenant_routing::{Catalogue, DiscoverySnapshot, RoutingError};
use std::{ffi::OsString, time::Duration};

const LIMIT: u64 = 4 * 1024 * 1024;
const PROVIDERS: &[(&str, &str, &[&str])] = &[
    ("codex", "codex", &["debug", "models"]),
    ("agy", "agy", &["models"]),
    ("opencode", "opencode", &["models"]),
    ("cursor", "cursor-agent", &["models"]),
];

fn read(roots: &Roots, adapter: &str) -> Option<DiscoverySnapshot> {
    let root = provenant_os::fs::Directory::open_anchor(roots.state.as_path()).ok()?;
    let directory = root
        .open_verified_directory(std::ffi::OsStr::new("model-discovery"))
        .ok()?;
    let meta = directory.metadata().ok()?;
    if meta.uid != provenant_os::fs::effective_uid() || meta.mode & 0o077 != 0 {
        return None;
    }
    let mut file = directory
        .open_read(std::ffi::OsStr::new(&format!("{adapter}.json")))
        .ok()?;
    let meta = file.metadata().ok()?;
    if meta.uid != provenant_os::fs::effective_uid() || meta.mode & 0o777 != 0o600 {
        return None;
    }
    let bytes = file.read_bounded(LIMIT).ok()?;
    let snapshot: DiscoverySnapshot = serde_json::from_slice(&bytes).ok()?;
    (snapshot.adapter == adapter).then_some(snapshot)
}

#[allow(
    clippy::print_stderr,
    reason = "warn that the discovery cache is unusable"
)]
pub(crate) fn effective(roots: &Roots) -> Result<Catalogue, RoutingError> {
    let snapshots: Vec<_> = PROVIDERS
        .iter()
        .filter_map(|(adapter, _, _)| read(roots, adapter))
        .collect();
    let bundled = Catalogue::bundled()?;
    match bundled.clone().with_discovery(&snapshots) {
        Ok(catalogue) => Ok(catalogue),
        Err(_) => {
            eprintln!("warning: invalid provider model discovery cache; using bundled catalogue");
            Ok(bundled)
        }
    }
}

/// A probe failure leaves its previous snapshot untouched. Return bounded,
/// human-readable changes and warnings to the quota command.
pub(crate) fn refresh(roots: &Roots, filter: Option<&str>, now: i64) -> Vec<String> {
    let Ok(executor) = SystemExecutor::new(1) else {
        return vec!["model discovery: executor unavailable".into()];
    };
    let Ok(cwd) = std::env::current_dir() else {
        return vec!["model discovery: working directory unavailable".into()];
    };
    refresh_with(roots, filter, now, |binary, args| {
        let argv: Vec<OsString> = std::iter::once(binary)
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(20));
        options.stdout = StreamPolicy::Capture {
            limit: LIMIT as usize,
        };
        options.stderr = StreamPolicy::Discard;
        options.process_group = true;
        options.cleanup_process_group = true;
        options.stop_on_output_limit = true;
        executor
            .execute(options)
            .ok()
            .filter(|out| {
                out.termination == Termination::Exit(0) && !out.timed_out && !out.stdout.truncated
            })
            .map(|out| out.stdout.bytes)
    })
}

fn refresh_with(
    roots: &Roots,
    filter: Option<&str>,
    now: i64,
    probe: impl Fn(&str, &[&str]) -> Option<Vec<u8>>,
) -> Vec<String> {
    let mut messages = Vec::new();
    for (adapter, binary, args) in PROVIDERS {
        if filter.is_some_and(|filter| filter != *adapter) {
            continue;
        }
        let snapshot = probe(binary, args).and_then(|bytes| {
            if *adapter == "codex" {
                provenant_routing::parse_codex_models(&bytes, now).ok()
            } else {
                std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|text| provenant_routing::parse_text_models(adapter, text, now).ok())
            }
        });
        let Some(snapshot) = snapshot else {
            messages.push(format!(
                "model discovery: {adapter} probe failed; retaining previous snapshot"
            ));
            continue;
        };
        if Catalogue::bundled()
            .and_then(|catalogue| catalogue.with_discovery(std::slice::from_ref(&snapshot)))
            .is_err()
        {
            messages.push(format!(
                "model discovery: {adapter} listing is invalid; retaining previous snapshot"
            ));
            continue;
        }
        let publish = || -> Result<String, String> {
            let state = crate::quota::private(roots.state.as_path()).map_err(|e| e.to_string())?;
            let dir = crate::quota::child(&state, std::ffi::OsStr::new("model-discovery"))
                .map_err(|e| e.to_string())?;
            let _lock = dir
                .try_lock(
                    std::ffi::OsStr::new(&format!("{adapter}.lock")),
                    provenant_os::fs::LockMode::Exclusive,
                )
                .map_err(|e| e.to_string())?;
            let old = read(roots, adapter);
            if old
                .as_ref()
                .is_some_and(|old| old.observed_at > snapshot.observed_at)
            {
                return Err("a newer snapshot is already recorded".into());
            }
            let bundled = Catalogue::bundled().map_err(|e| e.to_string())?;
            let old_ids: std::collections::BTreeSet<String> = match &old {
                Some(old) => old
                    .models
                    .iter()
                    .map(|row| row.model.as_str().to_owned())
                    .collect(),
                None => bundled
                    .models()
                    .iter()
                    .filter(|row| row.model.adapter() == *adapter)
                    .map(|row| row.model.as_str().to_owned())
                    .collect(),
            };
            let new_ids: std::collections::BTreeSet<String> = snapshot
                .models
                .iter()
                .map(|row| row.model.as_str().to_owned())
                .collect();
            let added: Vec<_> = new_ids.difference(&old_ids).map(String::as_str).collect();
            let retired: Vec<_> = old_ids.difference(&new_ids).map(String::as_str).collect();
            let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;
            dir.atomic_write(
                std::ffi::OsStr::new(&format!("{adapter}.json")),
                ".models-",
                &bytes,
                provenant_os::fs::PublishMode::Replace,
            )
            .and_then(|p| p.require_durable())
            .map_err(|e| e.to_string())?;
            Ok(format!(
                "model discovery: {adapter}: new [{}]; retired [{}]",
                added.join(", "),
                retired.join(", ")
            ))
        };
        match publish() {
            Ok(message) => messages.push(message),
            Err(error) => messages.push(format!("model discovery: {adapter} cache write failed; retaining previous snapshot when possible: {error}")),
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    // Spec: docs/specs/routing.md#provider-model-discovery
    #[test]
    fn refresh_keeps_last_success_on_failure_and_reports_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        let old = br#"{"models":[{"slug":"gpt-6.2-sol","supported_reasoning_levels":[{"effort":"high"}]}]}"#;
        let report = refresh_with(&roots, Some("codex"), 1, |_, _| Some(old.to_vec()));
        assert!(report[0].contains("codex/gpt-6.2-sol"));
        assert!(
            effective(&roots)?
                .model(&provenant_routing::ModelId::parse("codex/gpt-6.2-sol")?)
                .is_some()
        );
        let report = refresh_with(&roots, Some("codex"), 2, |_, _| None);
        assert!(report[0].contains("retaining previous"));
        assert_eq!(read(&roots, "codex").unwrap().observed_at, 1);
        let new = br#"{"models":[{"slug":"gpt-6.3-sol","supported_reasoning_levels":[{"effort":"high"}]}]}"#;
        let report = refresh_with(&roots, Some("codex"), 3, |_, _| Some(new.to_vec()));
        assert!(report[0].contains("codex/gpt-6.2-sol") && report[0].contains("codex/gpt-6.3-sol"));
        assert_eq!(read(&roots, "codex").unwrap().observed_at, 3);
        let report = refresh_with(&roots, Some("codex"), 2, |_, _| Some(old.to_vec()));
        assert!(report[0].contains("newer snapshot"));
        assert_eq!(read(&roots, "codex").unwrap().observed_at, 3);
        Ok(())
    }
}
