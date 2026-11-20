//! Conservative affected-crate selection for a Cargo workspace.
//!
//! `select` compares two revisions of a workspace and returns the changed
//! packages plus their reverse dependents. Anything that could invalidate the
//! dependency graph or the build configuration instead yields a full run with a
//! typed reason. The library is self-contained: callers pass the workspace,
//! revisions, policy, state directory, project key and the current wall time.
//! It reads no Provenant configuration of its own.

mod error;
mod git_support;
mod state;

use camino::Utf8Path;
use determinator::Determinator;
use globset::{Glob, GlobSet, GlobSetBuilder};
use guppy::graph::{DependencyDirection, PackageGraph};
use state::RunState;
use std::{
    fmt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

pub use error::Error;

/// How often a full run is forced regardless of the diff.
#[derive(Clone, Debug)]
pub struct Policy {
    /// Globs that, when matched by any changed path, force a full run.
    pub global_paths: Vec<String>,
    /// Force a full run on this numbered run since the last full run.
    ///
    /// The library default is 10; callers override by setting the field.
    pub full_every: u32,
    /// Force a full run once this much wall time has passed since the last one.
    ///
    /// The library default is 24 hours; callers override by setting the field.
    pub full_after: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            global_paths: Vec::new(),
            full_every: 10,
            full_after: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// Why a selection was returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reason {
    /// Ordinary selection: changed packages and their reverse dependents.
    Changed,
    /// A changed path matched a configured global path glob.
    GlobalPath {
        /// The changed path that matched.
        path: String,
    },
    /// A changed path belongs to no workspace package.
    UnownedPath {
        /// The unowned path.
        path: String,
    },
    /// A lockfile changed.
    Lockfile {
        /// The changed path.
        path: String,
    },
    /// A rust toolchain file changed.
    Toolchain {
        /// The changed path.
        path: String,
    },
    /// Cargo configuration changed.
    CargoConfig {
        /// The changed path.
        path: String,
    },
    /// A workspace-level manifest changed, outside any member's own manifest.
    WorkspaceManifest {
        /// The changed path.
        path: String,
    },
    /// Run state was missing or unreadable; the message is the warning.
    StateUnreadable {
        /// Why the state could not be used.
        message: String,
    },
    /// The numbered-run interval was reached.
    RunCount {
        /// The run number since the last full run.
        runs: u64,
        /// The configured interval.
        limit: u32,
    },
    /// The wall-clock interval was reached.
    Age {
        /// The configured interval in seconds.
        limit_secs: u64,
    },
}

impl fmt::Display for Reason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed => formatter.write_str("changed packages and their dependents"),
            Self::GlobalPath { path } => write!(formatter, "global path changed: {path}"),
            Self::UnownedPath { path } => write!(formatter, "unowned path changed: {path}"),
            Self::Lockfile { path } => write!(formatter, "lockfile changed: {path}"),
            Self::Toolchain { path } => write!(formatter, "toolchain changed: {path}"),
            Self::CargoConfig { path } => write!(formatter, "cargo config changed: {path}"),
            Self::WorkspaceManifest { path } => {
                write!(formatter, "workspace manifest changed: {path}")
            }
            Self::StateUnreadable { message } => write!(formatter, "state unreadable: {message}"),
            Self::RunCount { runs, limit } => {
                write!(formatter, "run {runs} of {limit} since the last full run")
            }
            Self::Age { limit_secs } => {
                write!(formatter, "more than {limit_secs}s since the last full run")
            }
        }
    }
}

/// The result of a selection. Package names are sorted and deduplicated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection {
    /// Workspace package names to build and test.
    pub packages: Vec<String>,
    /// The trigger that decided this selection.
    pub reason: Reason,
    /// Whether the caller must run the whole workspace suite.
    pub full: bool,
}

/// Selects packages affected by the change between `base` and `head`.
///
/// `workspace` is the repository root containing `Cargo.toml`. `state_dir` is a
/// caller-supplied root under which `select/<project_key>.json` is maintained.
/// `now` is the injected wall clock; the function never samples a clock itself.
pub fn select(
    workspace: &Path,
    base: &str,
    head: &str,
    policy: &Policy,
    state_dir: &Path,
    project_key: &str,
    now: SystemTime,
) -> Result<Selection, Error> {
    let now_millis = to_unix_millis(now)?;
    let state_path = state_path(state_dir, project_key)?;
    let loaded = state::load(&state_path);
    let prior = match &loaded {
        Ok(state) => state.clone(),
        Err(_) => None,
    };
    // A missing or corrupt state file warns through the reason and forces a full
    // run; it never panics or refuses the selection.
    let mut reason = loaded
        .as_ref()
        .err()
        .map(|message| Reason::StateUnreadable {
            message: (*message).clone(),
        });

    let globals = global_paths(&policy.global_paths)?;
    let changed = git_support::changed_paths(workspace, base, head)?;
    let new_graph = git_support::build_graph(workspace, "head")?;

    if reason.is_none() {
        for path in changed.iter() {
            if let Some(trigger) = classify(path, &globals, &new_graph) {
                reason = Some(trigger);
                break;
            }
        }
    }

    if reason.is_none()
        && let Some(state) = &prior
    {
        let runs = state.runs_since_full.saturating_add(1);
        if u64::from(policy.full_every) > 0 && runs >= u64::from(policy.full_every) {
            reason = Some(Reason::RunCount {
                runs,
                limit: policy.full_every,
            });
        } else {
            let last = SystemTime::UNIX_EPOCH + Duration::from_millis(state.last_full_unix_millis);
            let elapsed = now.duration_since(last).unwrap_or_default();
            if elapsed >= policy.full_after {
                reason = Some(Reason::Age {
                    limit_secs: policy.full_after.as_secs(),
                });
            }
        }
    }

    let full = reason.is_some();
    let reason = reason.unwrap_or(Reason::Changed);
    let packages = if full {
        all_members(&new_graph)
    } else {
        let checkout = git_support::BaseCheckout::new(workspace, base, now_millis)?;
        let old_graph = git_support::build_graph(checkout.path(), "base")?;
        let mut determinator = Determinator::new(&old_graph, &new_graph);
        determinator.add_changed_paths(changed.iter());
        let set = determinator.compute();
        let mut names: Vec<String> = set
            .affected_set
            .packages(DependencyDirection::Forward)
            .map(|package| package.name().to_string())
            .collect();
        names.sort();
        names.dedup();
        names
    };

    let next = if full {
        RunState::fresh(now_millis)
    } else {
        let previous = prior.unwrap_or_else(|| RunState::fresh(now_millis));
        RunState {
            version: state::STATE_VERSION,
            runs_since_full: previous.runs_since_full.saturating_add(1),
            last_full_unix_millis: previous.last_full_unix_millis,
        }
    };
    state::store(&state_path, &next, now_millis)?;

    Ok(Selection {
        packages,
        reason,
        full,
    })
}

/// Maps a changed path to a full-run trigger, if any.
///
/// The order is fixed so the reported reason is deterministic. Package
/// ownership is checked against the head graph: a path no member owns cannot be
/// attributed to a crate and therefore forces a full run.
fn classify(path: &Utf8Path, globals: &GlobSet, head: &PackageGraph) -> Option<Reason> {
    let text = path.as_str().to_owned();
    let name = path.file_name().unwrap_or_default();

    if globals.is_match(path.as_str()) {
        return Some(Reason::GlobalPath { path: text });
    }
    if name == "Cargo.lock" {
        return Some(Reason::Lockfile { path: text });
    }
    if name.starts_with("rust-toolchain") {
        return Some(Reason::Toolchain { path: text });
    }
    if path
        .components()
        .any(|component| component.as_str() == ".cargo")
        && name.starts_with("config")
    {
        return Some(Reason::CargoConfig { path: text });
    }
    if name == "Cargo.toml"
        && path
            .parent()
            .is_none_or(|parent| parent.as_str().is_empty())
    {
        return Some(Reason::WorkspaceManifest { path: text });
    }
    if !owned_by_member(path, head) {
        return Some(Reason::UnownedPath { path: text });
    }
    None
}

fn owned_by_member(path: &Utf8Path, head: &PackageGraph) -> bool {
    head.workspace().member_by_path(path).is_ok()
        || path
            .ancestors()
            .skip(1)
            .any(|ancestor| head.workspace().member_by_path(ancestor).is_ok())
}

fn global_paths(patterns: &[String]) -> Result<GlobSet, Error> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|source| Error::InvalidGlobalPath {
            pattern: pattern.clone(),
            source,
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|source| Error::InvalidGlobalPath {
        pattern: "global_paths".to_owned(),
        source,
    })
}

fn all_members(graph: &PackageGraph) -> Vec<String> {
    let mut names: Vec<String> = graph
        .resolve_workspace()
        .packages(DependencyDirection::Forward)
        .map(|package| package.name().to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn state_path(state_dir: &Path, project_key: &str) -> Result<PathBuf, Error> {
    if project_key.is_empty()
        || project_key == "."
        || project_key == ".."
        || project_key.contains('/')
        || project_key.contains('\\')
        || project_key.contains('\0')
    {
        return Err(Error::InvalidProjectKey(project_key.to_owned()));
    }
    Ok(state_dir.join("select").join(format!("{project_key}.json")))
}

fn to_unix_millis(now: SystemTime) -> Result<u64, Error> {
    let duration = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| Error::OutOfRangeTime(now))?;
    u64::try_from(duration.as_millis()).map_err(|_| Error::OutOfRangeTime(now))
}
