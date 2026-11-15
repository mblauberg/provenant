//! The first 2.0 install on a host that ran 1.x. 2.0 keeps its state under a
//! fresh default root and copies only the owner's configuration out of the
//! 1.x root, which it reads and never writes, moves or deletes. Rollback is
//! reinstalling 1.x, whose root is still intact.
use serde::Serialize;
use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// The files of a 1.x root that hold owner configuration, relative to it.
/// Run state, stores, ledgers, worktree records and caches stay behind, and
/// provider credentials live with each provider, never in the state root.
pub(crate) const OWNER_FILES: &[&str] = &[
    // Every configuration domain: accounts, routing, profile presets,
    // Fabric peers, lanes, landing and the shared settings source.
    "config.toml",
    // This host's Fabric identity, which the peers' configuration names.
    "host-id",
];

/// Markers that distinguish the 1.x root layout without reading a receipt or
/// interpreting a database. Current writers keep these stores in the writer
/// registry, not at the state root.
const LEGACY_ROOT_MARKERS: &[&str] = &["admission.sqlite3", "pr-contributions.sqlite3"];

#[derive(Debug)]
pub(crate) enum PlanError {
    Io(io::Error),
    StateFormatBreak { root: PathBuf },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::StateFormatBreak { root } => write!(
                formatter,
                "state root {} has the 1.x layout; 1.x state is not migrated. Install into the default v2 root; if this is a custom 1.x root, copy owner configuration manually",
                root.display()
            ),
        }
    }
}

impl std::error::Error for PlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::StateFormatBreak { .. } => None,
        }
    }
}

impl From<io::Error> for PlanError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

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
/// default root of that home. An explicit 1.x root gets a typed refusal;
/// arbitrary fresh roots remain available.
pub(crate) fn plan(state: &Path, home: &Path) -> Result<Option<Handoff>, PlanError> {
    if provenant_core::default_state_home(state) != Some(home) {
        validate_selected_root(state, home)?;
        return Ok(None);
    }
    let from = home.join(provenant_core::LEGACY_V1_STATE_SUFFIX);
    match std::fs::metadata(&from) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
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
            Err(error) => return Err(error.into()),
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

/// Check the caller-selected path before root resolution can reject the 1.x
/// root as protected. A default v2 root remains valid even when it contains
/// state stores with names shared by older releases.
pub(crate) fn validate_selected_root(state: &Path, home: &Path) -> Result<(), PlanError> {
    if provenant_core::default_state_home(state) == Some(home) {
        return Ok(());
    }
    if is_legacy_root(state)? {
        return Err(PlanError::StateFormatBreak {
            root: state.to_path_buf(),
        });
    }
    Ok(())
}

fn is_legacy_root(state: &Path) -> io::Result<bool> {
    if provenant_core::legacy_state_home(state).is_some() {
        return Ok(true);
    }
    for marker in LEGACY_ROOT_MARKERS {
        match std::fs::symlink_metadata(state.join(marker)) {
            Ok(metadata) if metadata.file_type().is_file() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// Validate staged owner config before the install-specific preflight. Config
/// and host identity use their current owners; no receipt or old codec is read.
fn validate_stage(
    plan: &Handoff,
    stage: &Path,
    context: &provenant_core::Roots,
) -> io::Result<provenant_core::Roots> {
    if provenant_core::default_state_home(&plan.to).is_none() {
        return Err(io::Error::other(
            "owner-config handoff destination is not the default root",
        ));
    }
    let roots =
        crate::roots::state_candidate(context, stage.to_path_buf()).map_err(io::Error::other)?;
    provenant_config::config_show(&roots, &provenant_cli::domains::SECTIONS)
        .map_err(io::Error::other)?;
    provenant_config::lookup_host(&roots).map_err(io::Error::other)?;
    Ok(roots)
}

struct Staged {
    parent: PathBuf,
    path: PathBuf,
}

fn stage(
    plan: &Handoff,
    context: &provenant_core::Roots,
    preflight: impl FnOnce(&provenant_core::Roots) -> io::Result<()>,
) -> io::Result<Staged> {
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
    let nonce = provenant_core::hex(&provenant_os::fs::entropy_128().map_err(io::Error::other)?);
    let staged_path = parent.join(format!(
        ".{}.handoff-{}-{nonce}",
        name.to_string_lossy(),
        std::process::id()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&staged_path)?;
    let staged = (|| {
        for file in &plan.files {
            let target = staged_path.join(file);
            if let Some(directory) = target.parent() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(directory)?;
            }
            // Keep the mode expected by the owner-config readers.
            std::fs::copy(plan.from.join(file), &target)?;
            std::fs::File::open(&target)?.sync_all()?;
        }
        for directory in std::iter::once(staged_path.clone()).chain(
            plan.files
                .iter()
                .filter_map(|file| staged_path.join(file).parent().map(Path::to_path_buf)),
        ) {
            std::fs::File::open(directory)?.sync_all()?;
        }
        let roots = validate_stage(plan, &staged_path, context).map_err(|error| {
            io::Error::other(format!(
                "handoff preflight failed; agents left running: {error}"
            ))
        })?;
        preflight(&roots).map_err(|error| {
            io::Error::other(format!(
                "staged install preflight failed; agents left running: {error}"
            ))
        })?;
        if !absent_or_empty(&plan.to)? {
            return Err(io::Error::other(
                "handoff destination gained entries; agents left running",
            ));
        }
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = std::fs::remove_dir_all(&staged_path);
        return Err(error);
    }
    Ok(Staged {
        parent: parent.to_path_buf(),
        path: staged_path,
    })
}

/// Run every install preflight against staged state without stopping agents or
/// publishing the state root. Used by dry-run installs.
pub(crate) fn preview(
    plan: &Handoff,
    context: &provenant_core::Roots,
    preflight: impl FnOnce(&provenant_core::Roots) -> io::Result<()>,
) -> io::Result<()> {
    let staged = stage(plan, context, preflight)?;
    std::fs::remove_dir_all(&staged.path)
}

/// Stage and preflight owner config and the complete install before stopping
/// agents. A preflight or stop failure leaves the destination unpublished.
/// Nothing under `plan.from` is written.
pub(crate) fn run(
    plan: &Handoff,
    context: &provenant_core::Roots,
    preflight: impl FnOnce(&provenant_core::Roots) -> io::Result<()>,
    stop_agents: impl FnOnce() -> io::Result<()>,
) -> io::Result<Handoff> {
    let staged = stage(plan, context, preflight)?;
    let publish = (|| {
        if !absent_or_empty(&plan.to)? {
            return Err(io::Error::other(
                "handoff destination gained entries; agents left running",
            ));
        }
        stop_agents()?;
        // Replaces only an absent or empty root; a root that gained entries
        // refuses with ENOTEMPTY and keeps them.
        std::fs::rename(&staged.path, &plan.to)?;
        std::fs::File::open(&staged.parent)?.sync_all()
    })();
    if let Err(error) = publish {
        let _ = std::fs::remove_dir_all(&staged.path);
        return Err(error);
    }
    Ok(Handoff {
        copied: true,
        ..plan.clone()
    })
}

/// Process-edge seam shared by install preflight and LaunchAgent shutdown.
pub(crate) fn quiesce_launch_agents(
    home: &Path,
    labels: &[&'static str],
    mut run: impl FnMut(
        &[&OsStr],
        &Path,
    ) -> Result<(bool, String), provenant_fabric_runtime::peer_agent::AgentError>,
    mut record: impl FnMut(&'static str, io::Result<bool>) -> io::Result<()>,
) -> io::Result<()> {
    for &label in labels {
        record(
            label,
            provenant_fabric_runtime::peer_agent::stop_service_with(home, label, &mut run)
                .map_err(io::Error::other),
        )?;
    }
    Ok(())
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
    unused_imports,
    reason = "the worktree driver includes this module without registering its tests"
)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

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
        // Invalid bytes prove the receipt is neither decoded nor copied.
        std::fs::write(legacy.join("install/receipt.json"), b"not a receipt")?;
        use std::os::unix::fs::PermissionsExt;
        for file in ["host-id", "install/receipt.json"] {
            std::fs::set_permissions(legacy.join(file), std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::write(legacy.join("run.sqlite3"), b"run state")?;
        std::fs::write(legacy.join("runs/run/result.md"), b"result")?;
        Ok((
            temp,
            home.clone(),
            home.join(provenant_core::DEFAULT_STATE_SUFFIX),
        ))
    }

    fn install_roots(home: &Path) -> Result<provenant_core::Roots> {
        let mut inputs = provenant_core::RootInputs::new(home.into(), home.join("release"));
        inputs.workspaces.push(home.join("project"));
        Ok(inputs.resolve(&provenant_os::fs::SystemPathResolver)?)
    }

    #[test]
    fn copies_only_declared_owner_configuration_and_never_reads_receipts() -> Result {
        let (_temp, home, state) = legacy_home()?;
        let legacy = home.join(provenant_core::LEGACY_V1_STATE_SUFFIX);
        let before = tree(&legacy)?;
        let plan = plan(&state, &home)?.ok_or("hand-off planned")?;
        let preflighted = std::cell::Cell::new(false);
        let mut stopped = false;
        let done = run(
            &plan,
            &install_roots(&home)?,
            |_| {
                assert!(!state.exists());
                preflighted.set(true);
                Ok(())
            },
            || {
                assert!(preflighted.get());
                assert!(!state.exists());
                stopped = true;
                Ok(())
            },
        )?;
        assert!(stopped && done.copied);
        assert_eq!(tree(&legacy)?, before);
        let copied: BTreeMap<_, _> = tree(&state)?
            .into_iter()
            .filter(|(_, bytes)| !bytes.is_empty())
            .map(|(path, bytes)| Ok((path.strip_prefix(&state)?.to_path_buf(), bytes)))
            .collect::<Result<_>>()?;
        let expected: BTreeSet<_> = OWNER_FILES.iter().map(PathBuf::from).collect();
        assert_eq!(copied.keys().cloned().collect::<BTreeSet<_>>(), expected);
        for (path, bytes) in copied {
            assert_eq!(bytes, std::fs::read(legacy.join(path))?);
        }
        assert!(super::plan(&state, &home)?.is_none());
        Ok(())
    }

    #[test]
    fn failed_staged_preflight_leaves_agents_running_and_root_unpublished() -> Result {
        let (_temp, home, state) = legacy_home()?;
        let legacy = home.join(provenant_core::LEGACY_V1_STATE_SUFFIX);
        let before = tree(&legacy)?;
        let plan = plan(&state, &home)?.ok_or("hand-off planned")?;
        let mut stopped = false;
        assert!(
            run(
                &plan,
                &install_roots(&home)?,
                |_| Err(io::Error::other("release verification failed")),
                || {
                    stopped = true;
                    Ok(())
                },
            )
            .is_err()
        );
        assert!(!stopped);
        assert!(!state.exists());
        assert_eq!(tree(&legacy)?, before);
        Ok(())
    }

    #[test]
    fn invalid_owner_configuration_never_stops_agents_or_publishes_state() -> Result {
        for (file, invalid) in [
            ("config.toml", "not valid TOML ["),
            ("host-id", "invalid host identity"),
        ] {
            let (_temp, home, state) = legacy_home()?;
            let legacy = home.join(provenant_core::LEGACY_V1_STATE_SUFFIX);
            std::fs::write(legacy.join(file), invalid)?;
            let before = tree(&legacy)?;
            let plan = plan(&state, &home)?.ok_or("hand-off planned")?;
            let mut stopped = false;
            assert!(
                run(
                    &plan,
                    &install_roots(&home)?,
                    |_| Ok(()),
                    || {
                        stopped = true;
                        Ok(())
                    },
                )
                .is_err()
            );
            assert!(!stopped);
            assert!(!state.exists());
            assert_eq!(tree(&legacy)?, before);
        }
        Ok(())
    }

    #[test]
    fn a_failed_agent_stop_publishes_nothing() -> Result {
        let (_temp, home, state) = legacy_home()?;
        let plan = plan(&state, &home)?.ok_or("hand-off planned")?;
        assert!(
            run(
                &plan,
                &install_roots(&home)?,
                |_| Ok(()),
                || Err(io::Error::other("still running")),
            )
            .is_err()
        );
        assert!(!state.exists());
        Ok(())
    }

    #[test]
    fn only_an_absent_or_empty_default_root_is_seeded() -> Result {
        let (temp, home, state) = legacy_home()?;
        assert!(plan(&temp.path().join("explicit"), &home)?.is_none());
        let custom = temp.path().join("custom");
        std::fs::create_dir_all(&custom)?;
        assert!(plan(&custom, &home)?.is_none());
        std::fs::write(custom.join("admission.sqlite3"), b"1.x root marker")?;
        assert!(matches!(
            plan(&custom, &home),
            Err(PlanError::StateFormatBreak { root }) if root == custom
        ));
        std::fs::create_dir_all(&state)?;
        assert!(plan(&state, &home)?.is_some());
        std::fs::write(state.join("config.toml"), b"kept")?;
        assert!(plan(&state, &home)?.is_none());
        Ok(())
    }
}
