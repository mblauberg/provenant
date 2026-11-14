//! The first 2.0 install on a host that ran 1.x. 2.0 keeps its state under a
//! fresh default root and copies only the owner's configuration out of the
//! 1.x root, which it reads and never writes, moves or deletes. Rollback is
//! reinstalling 1.x, whose root is still intact.
use serde::Serialize;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// The files of a 1.x root that hold owner configuration, relative to it.
/// Run state, stores, ledgers, worktree records and caches stay behind, and
/// provider credentials live with each provider, never in the state root.
/// `agy-identity-<digest>.json` is a TTL probe cache and
/// `projects/<id>/project.json` a path-derived binding; both are re-derived.
pub(crate) const OWNER_FILES: &[&str] = &[
    // Every configuration domain: accounts, routing, profile presets,
    // Fabric peers, lanes, landing and the shared settings source.
    "config.toml",
    // This host's Fabric identity, which the peers' configuration names.
    "host-id",
    // The recorded install selection and the links 1.x owns, so the first
    // 2.0 install replaces those links instead of refusing them as foreign.
    "install/receipt.json",
];

/// Launch agents a 1.x install may have loaded. 2.0's peer and tick agents
/// use the same labels and plist paths, so a stop by label also stops a 2.0
/// agent of that name; the install restarts the 2.0 agents when it is done.
pub(crate) const LEGACY_AGENT_LABELS: &[&str] = &[
    "dev.provenant.peer",
    "dev.provenant.disk-guard",
    "dev.provenant.gc",
    "dev.provenant.tick",
];

/// One copy of owner configuration from the 1.x root into the new root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Handoff {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Copied files, relative to both roots.
    pub files: Vec<PathBuf>,
    /// False while the copy is only planned, as in a dry run.
    pub copied: bool,
}

impl Handoff {
    /// One human line naming the files, both roots and what was left alone.
    pub(crate) fn note(&self) -> String {
        let files = self
            .files
            .iter()
            .map(|file| file.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} {files} from {} to {}; {} left in place\n",
            if self.copied {
                "Copied 1.x configuration"
            } else {
                "Would copy 1.x configuration"
            },
            self.from.display(),
            self.to.display(),
            self.from.display(),
        )
    }
}

/// The copy an install at `state` makes, or `None`. Only the default root of
/// `home` is seeded, only while it is absent or empty, and only from the 1.x
/// default root of that home. An explicit root elsewhere is never seeded, and
/// an existing root is left alone, so a repeated install copies nothing.
pub(crate) fn plan(state: &Path, home: &Path) -> io::Result<Option<Handoff>> {
    if provenant_core::default_state_home(state) != Some(home) {
        return Ok(None);
    }
    let from = home.join(provenant_core::LEGACY_V1_STATE_SUFFIX);
    match std::fs::metadata(&from) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    if !absent_or_empty(state)? {
        return Ok(None);
    }
    let mut files = Vec::new();
    for name in OWNER_FILES {
        match std::fs::symlink_metadata(from.join(name)) {
            // Only regular files are copied; a link is never followed.
            Ok(metadata) if metadata.file_type().is_file() => files.push(PathBuf::from(name)),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if files.is_empty() {
        return Ok(None);
    }
    Ok(Some(Handoff {
        from,
        to: state.to_path_buf(),
        files,
        copied: false,
    }))
}

/// Stops the 1.x agents, then copies. A failed stop copies nothing, so two
/// versions never run agents at once and the copy is a quiet snapshot.
pub(crate) fn run(
    plan: &Handoff,
    stop_agents: impl FnOnce() -> io::Result<()>,
) -> io::Result<Handoff> {
    stop_agents()?;
    copy(plan)
}

/// Builds the new root beside it and renames it into place, so the root is
/// either absent or complete. Nothing under `plan.from` is written.
fn copy(plan: &Handoff) -> io::Result<Handoff> {
    let parent = plan
        .to
        .parent()
        .ok_or_else(|| io::Error::other("state root has no parent"))?;
    let name = plan
        .to
        .file_name()
        .ok_or_else(|| io::Error::other("state root has no name"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let stage = parent.join(format!(
        ".{}.handoff-{}-{nonce}",
        name.to_string_lossy(),
        std::process::id()
    ));
    std::fs::DirBuilder::new().mode(0o700).create(&stage)?;
    let staged = (|| {
        for file in &plan.files {
            let target = stage.join(file);
            if let Some(directory) = target.parent() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(directory)?;
            }
            // Keeps the mode, which the receipt and host identity readers check.
            std::fs::copy(plan.from.join(file), &target)?;
            if file == Path::new("install/receipt.json") {
                let mut receipt: provenant_records::InstallReceipt = provenant_records::decode(
                    std::fs::File::open(&target)?,
                    provenant_records::Limits::default(),
                )
                .map_err(io::Error::other)?;
                receipt.state_format_version = provenant_records::STATE_FORMAT_VERSION;
                std::fs::write(
                    &target,
                    provenant_records::encode(&receipt).map_err(io::Error::other)?,
                )?;
            }
            std::fs::File::open(&target)?.sync_all()?;
        }
        for directory in std::iter::once(stage.clone()).chain(
            plan.files
                .iter()
                .filter_map(|file| stage.join(file).parent().map(Path::to_path_buf)),
        ) {
            std::fs::File::open(directory)?.sync_all()?;
        }
        // Replaces only an absent or empty root; a root that gained entries
        // since the plan refuses with ENOTEMPTY and keeps them.
        std::fs::rename(&stage, &plan.to)
    })();
    if let Err(error) = staged {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(error);
    }
    std::fs::File::open(parent)?.sync_all()?;
    Ok(Handoff {
        copied: true,
        ..plan.clone()
    })
}

fn absent_or_empty(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(std::fs::read_dir(path)?.next().is_none()),
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the worktree driver includes this module without registering its tests"
)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn tree(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
        let mut files = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory)? {
                let path = entry?.path();
                if path.is_dir() {
                    files.insert(path.clone(), Vec::new());
                    pending.push(path);
                } else {
                    files.insert(path.clone(), std::fs::read(&path)?);
                }
            }
        }
        Ok(files)
    }

    fn legacy_home() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        let legacy = home.join(provenant_core::LEGACY_V1_STATE_SUFFIX);
        std::fs::create_dir_all(legacy.join("install"))?;
        std::fs::create_dir_all(legacy.join("runs/run"))?;
        std::fs::write(legacy.join("config.toml"), b"schema_version = 1\n")?;
        std::fs::write(legacy.join("host-id"), [b'a'; 32])?;
        let receipt = provenant_records::InstallReceipt {
            extra: Default::default(),
            schema_version: 1,
            state_format_version: 1,
            source_digest: provenant_records::SourceDigest::from_bytes([1; 32]),
            manifest_digest: provenant_records::FileDigest::from_bytes([2; 32]),
            asset_digest: provenant_records::SourceDigest::from_bytes([3; 32]),
            target: "fixture".into(),
            toolchain: "fixture".into(),
            bin_dir: provenant_records::AbsolutePath::new(&home.join("bin"))?,
            harnesses: vec![],
            harness_targets: vec![],
            entries: vec![],
        };
        std::fs::write(
            legacy.join("install/receipt.json"),
            provenant_records::encode(&receipt)?,
        )?;
        std::fs::write(legacy.join("run.sqlite3"), b"run state")?;
        std::fs::write(legacy.join("runs/run/result.md"), b"result")?;
        Ok((
            temp,
            home.clone(),
            home.join(provenant_core::DEFAULT_STATE_SUFFIX),
        ))
    }

    #[test]
    fn copies_only_owner_configuration_and_never_writes_the_old_root() -> Result {
        let (_temp, home, state) = legacy_home()?;
        let legacy = home.join(provenant_core::LEGACY_V1_STATE_SUFFIX);
        let before = tree(&legacy)?;
        let plan = plan(&state, &home)?.ok_or("hand-off planned")?;
        let mut stopped = false;
        let done = run(&plan, || {
            assert!(!state.exists(), "agents stop before the root exists");
            stopped = true;
            Ok(())
        })?;
        assert!(stopped && done.copied);
        assert_eq!(tree(&legacy)?, before);
        let copied: Vec<_> = tree(&state)?
            .into_iter()
            .filter(|(_, bytes)| !bytes.is_empty())
            .map(|(path, bytes)| (path.strip_prefix(&state).map(Path::to_path_buf), bytes))
            .collect();
        assert_eq!(copied.len(), OWNER_FILES.len());
        for (path, bytes) in copied {
            let path = path?;
            assert!(OWNER_FILES.iter().any(|name| Path::new(name) == path));
            if path == Path::new("install/receipt.json") {
                let copied: provenant_records::InstallReceipt = serde_json::from_slice(&bytes)?;
                let mut original: provenant_records::InstallReceipt =
                    serde_json::from_slice(&std::fs::read(legacy.join(&path))?)?;
                assert_eq!(
                    copied.state_format_version,
                    provenant_records::STATE_FORMAT_VERSION
                );
                original.state_format_version = copied.state_format_version;
                assert_eq!(copied, original);
            } else {
                assert_eq!(bytes, std::fs::read(legacy.join(&path))?);
            }
        }
        // An existing root is left alone: a second install copies nothing.
        assert!(super::plan(&state, &home)?.is_none());
        Ok(())
    }

    #[test]
    fn a_failed_agent_stop_copies_nothing() -> Result {
        let (_temp, home, state) = legacy_home()?;
        let plan = plan(&state, &home)?.ok_or("hand-off planned")?;
        assert!(run(&plan, || Err(io::Error::other("still running"))).is_err());
        assert!(!state.exists());
        Ok(())
    }

    #[test]
    fn only_an_absent_or_empty_default_root_is_seeded() -> Result {
        let (temp, home, state) = legacy_home()?;
        assert!(plan(&temp.path().join("explicit"), &home)?.is_none());
        std::fs::create_dir_all(&state)?;
        assert!(plan(&state, &home)?.is_some());
        std::fs::write(state.join("config.toml"), b"kept")?;
        assert!(plan(&state, &home)?.is_none());
        Ok(())
    }
}
