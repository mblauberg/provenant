//! The first 2.0 install on a host that ran 1.x. 2.0 keeps its state under a
//! fresh default root and copies only the owner's configuration out of the
//! 1.x root, which it reads and never writes, moves or deletes. Rollback is
//! reinstalling 1.x, whose root is still intact.
use serde::{Deserialize, Serialize};
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

/// This view exists only at the one-way root handoff. Read ownership and
/// selections from the released v1 receipt; ignore retired, unrelated fields.
#[derive(Deserialize)]
struct V1Receipt {
    source_digest: provenant_records::SourceDigest,
    manifest_digest: provenant_records::FileDigest,
    asset_digest: provenant_records::SourceDigest,
    target: String,
    toolchain: String,
    bin_dir: provenant_records::AbsolutePath,
    harnesses: Vec<provenant_records::Harness>,
    harness_targets: Vec<V1Target>,
    entries: Vec<provenant_records::OwnedEntry>,
}

#[derive(Deserialize)]
struct V1Target {
    harness: provenant_records::Harness,
    skills_dir: provenant_records::AbsolutePath,
    mcp_file: provenant_records::AbsolutePath,
    hook_file: Option<provenant_records::AbsolutePath>,
    instruction: Option<provenant_records::AbsolutePath>,
    #[serde(default)]
    hooks_relinquished: bool,
}

fn transfer_receipt(path: &Path) -> io::Result<()> {
    let old: V1Receipt = provenant_records::decode_v1(
        std::fs::File::open(path)?,
        provenant_records::Limits::CONFIG,
    )
    .map_err(io::Error::other)?;
    let receipt = provenant_records::InstallReceipt {
        extra: Default::default(),
        schema_version: 1,
        state_format_version: provenant_records::STATE_FORMAT_VERSION,
        source_digest: old.source_digest,
        manifest_digest: old.manifest_digest,
        asset_digest: old.asset_digest,
        target: old.target,
        toolchain: old.toolchain,
        bin_dir: old.bin_dir,
        harnesses: old.harnesses,
        harness_targets: old
            .harness_targets
            .into_iter()
            .map(|target| provenant_records::ResolvedHarnessTarget {
                extra: Default::default(),
                harness: target.harness,
                skills_dir: target.skills_dir,
                config_file: target.mcp_file,
                hook_file: target.hook_file,
                instruction: target.instruction,
                hooks_relinquished: target.hooks_relinquished,
            })
            .collect(),
        entries: old
            .entries
            .into_iter()
            .filter(|entry| {
                !matches!(
                    entry.selector.as_str(),
                    "toml:mcp_servers.fabric"
                        | "json:/mcp/fabric"
                        | "json:/mcpServers/fabric"
                        | "disk-guard-agent"
                        | "gc-agent"
                )
            })
            .collect(),
    };
    std::fs::write(
        path,
        provenant_records::encode(&receipt).map_err(io::Error::other)?,
    )
}

/// Validate the staged bytes with their runtime owners before stopping anything.
/// Config remains TOML at the same schema version; host-id remains 32 hex bytes.
/// Neither needs a legacy codec. Invalid input is a preflight error.
fn validate_stage(
    stage: &Path,
    environment: &crate::roots::Resolved,
) -> io::Result<()> {
    let roots = crate::roots::state_candidate(environment, stage.to_path_buf())
        .map_err(io::Error::other)?;
    provenant_config::config_show(&roots, &provenant_cli::domains::SECTIONS)
        .map_err(io::Error::other)?;
    provenant_config::lookup_host(&roots).map_err(io::Error::other)?;
    let targets = provenant_config::Targets::new(roots, &environment.home, &[]);
    // This invokes the complete install ownership validator, including selectors.
    provenant_config::installed_binary(&targets).map_err(io::Error::other)?;
    Ok(())
}

/// Read, transform, validate and durably stage every input before stopping the
/// agents. A preflight failure stops nothing. A failed stop publishes nothing.
/// Nothing under `plan.from` is written.
pub(crate) fn run(
    plan: &Handoff,
    environment: &crate::roots::Resolved,
    stop_agents: impl FnOnce() -> io::Result<()>,
) -> io::Result<Handoff> {
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
                transfer_receipt(&target).map_err(|error| {
                    io::Error::other(format!("handoff install/receipt.json: {error}"))
                })?;
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
        validate_stage(&stage, environment).map_err(|error| {
            io::Error::other(format!(
                "handoff preflight failed; agents left running: {error}"
            ))
        })?;
        if !absent_or_empty(&plan.to)? {
            return Err(io::Error::other(
                "handoff destination gained entries; agents left running",
            ));
        }
        stop_agents()?;
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

    fn resolved_environment(home: &Path) -> Result<crate::roots::Resolved> {
        let assets = home.join("product");
        let project = home.join("checkout");
        std::fs::create_dir_all(&assets)?;
        std::fs::create_dir_all(&project)?;
        let mut inputs = provenant_core::RootInputs::new(home.to_path_buf(), assets.clone());
        inputs.workspaces.push(project.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        Ok(crate::roots::Resolved {
            assets,
            home: home.to_path_buf(),
            project,
            roots,
            claude_config: None,
            codex_home: None,
        })
    }

    fn legacy_home() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        let legacy = home.join(provenant_core::LEGACY_V1_STATE_SUFFIX);
        std::fs::create_dir_all(legacy.join("install"))?;
        std::fs::create_dir_all(legacy.join("runs/run"))?;
        std::fs::write(legacy.join("config.toml"), b"schema_version = 1\n")?;
        std::fs::write(legacy.join("host-id"), [b'a'; 32])?;
        // Shape from v1.11.2 crates/records/src/install.rs, including mcp_file.
        let fixture = include_str!("../tests/fixtures/install/v1-receipt.json").replace(
            "fixture/home",
            home.strip_prefix("/")?.to_str().ok_or("UTF-8 home")?,
        );
        std::fs::write(legacy.join("install/receipt.json"), fixture)?;
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

    #[test]
    fn copies_only_owner_configuration_and_never_writes_the_old_root() -> Result {
        let (_temp, home, state) = legacy_home()?;
        let legacy = home.join(provenant_core::LEGACY_V1_STATE_SUFFIX);
        let before = tree(&legacy)?;
        let plan = plan(&state, &home)?.ok_or("hand-off planned")?;
        let environment = resolved_environment(&home)?;
        assert_ne!(environment.assets, environment.home);
        let mut stopped = false;
        let done = run(&plan, &environment, || {
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
                let original: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(legacy.join(&path))?)?;
                assert_eq!(
                    copied.state_format_version,
                    provenant_records::STATE_FORMAT_VERSION
                );
                assert_eq!(
                    copied.harness_targets[0].config_file,
                    serde_json::from_value(original["harness_targets"][0]["mcp_file"].clone())?
                );
                assert!(
                    copied
                        .entries
                        .iter()
                        .any(|entry| entry.selector == "binary")
                );
                assert!(
                    !copied
                        .entries
                        .iter()
                        .any(|entry| entry.selector == "json:/mcpServers/fabric")
                );
            } else {
                assert_eq!(bytes, std::fs::read(legacy.join(&path))?);
            }
        }
        // An existing root is left alone: a second install copies nothing.
        assert!(super::plan(&state, &home)?.is_none());
        Ok(())
    }

    #[test]
    fn invalid_owner_input_never_stops_agents_or_publishes_state() -> Result {
        for (file, invalid) in [
            ("config.toml", "not valid TOML ["),
            ("host-id", "invalid host identity"),
            ("install/receipt.json", "{}"),
            ("install/receipt.json", "invalid JSON"),
        ] {
            let (_temp, home, state) = legacy_home()?;
            let legacy = home.join(provenant_core::LEGACY_V1_STATE_SUFFIX);
            std::fs::write(legacy.join(file), invalid)?;
            let before = tree(&legacy)?;
            let plan = plan(&state, &home)?.ok_or("hand-off planned")?;
            let mut stopped = false;
            assert!(
                run(&plan, &resolved_environment(&home)?, || {
                    stopped = true;
                    Ok(())
                })
                .is_err()
            );
            assert!(!stopped);
            assert!(!state.exists());
            assert_eq!(tree(&legacy)?, before);
        }
        Ok(())
    }

    #[test]
    fn a_failed_agent_stop_copies_nothing() -> Result {
        let (_temp, home, state) = legacy_home()?;
        let plan = plan(&state, &home)?.ok_or("hand-off planned")?;
        assert!(
            run(
                &plan,
                &resolved_environment(&home)?,
                || Err(io::Error::other("still running"))
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
        std::fs::create_dir_all(&state)?;
        assert!(plan(&state, &home)?.is_some());
        std::fs::write(state.join("config.toml"), b"kept")?;
        assert!(plan(&state, &home)?.is_none());
        Ok(())
    }
}
