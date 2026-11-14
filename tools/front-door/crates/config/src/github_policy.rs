//! Host-owned advisory GitHub policy for a bound project, and the facts the
//! lane's `gh` wrapper and its settlement share.
//!
//! Everything here is advisory. Which credential a lane holds is decided on
//! the host (`gh_auth` in the CLI); the wrapper only counts, informs and
//! applies the project's deny lists, and a lane can bypass it.
use crate::ConfigError;
use provenant_core::{ProjectId, StateRoot};
use provenant_os::fs::Directory;
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, io::Read, path::Path};

/// File in the lane's scratch directory that the `gh` wrapper appends one
/// byte to per invocation. Its size is the count settlement records.
pub const GH_INVOCATIONS_FILE: &str = "gh-invocations";

/// The `lanes.gh_budget` registry default: warn after this many invocations.
pub const DEFAULT_GH_BUDGET: u64 = 100;

/// Bounds on the workflow scan: a lane's checkout is untrusted input.
const WORKFLOW_FILES: usize = 64;
const WORKFLOW_BYTES: u64 = 256 * 1024;

/// Whether a workflow in `checkout` runs on pull request label events, so a
/// label change on a pull request starts CI. A heuristic over
/// `.github/workflows/*.yml`: a `types` entry naming `labeled` or
/// `unlabeled`. Unreadable, linked or oversized files count as no.
pub fn label_events_trigger_ci(checkout: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(checkout.join(".github/workflows")) else {
        return false;
    };
    entries
        .map_while(Result::ok)
        .take(WORKFLOW_FILES)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            (name.ends_with(".yml") || name.ends_with(".yaml"))
                && entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry
                    .metadata()
                    .is_ok_and(|metadata| metadata.len() <= WORKFLOW_BYTES)
        })
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .any(|text| workflow_runs_on_labels(&text))
}

fn workflow_runs_on_labels(text: &str) -> bool {
    let mut in_types = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let names_label = || {
            trimmed
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|word| word == "labeled" || word == "unlabeled")
        };
        if let Some(rest) = trimmed.strip_prefix("types:") {
            if names_label() {
                return true;
            }
            in_types = rest.trim().is_empty();
        } else if in_types && trimmed.starts_with('-') {
            if names_label() {
                return true;
            }
        } else {
            in_types = false;
        }
    }
    false
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GithubPolicy {
    #[serde(default)]
    pub deny_verbs: Vec<String>,
    #[serde(default)]
    pub deny_labels: Vec<String>,
}

impl GithubPolicy {
    /// Read the policy for the host-bound project. Only absence means empty.
    pub fn read(state: &StateRoot, project_id: ProjectId) -> Result<Self, ConfigError> {
        let root = Directory::open_anchor(state.as_path())?;
        let projects = match root.open_verified_directory(OsStr::new("projects")) {
            Ok(dir) => dir,
            Err(e) if e.is_not_found() => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let project = match projects.open_verified_directory(OsStr::new(&project_id.to_string())) {
            Ok(dir) => dir,
            Err(e) if e.is_not_found() => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let file = match project.open_read(OsStr::new("github.json")) {
            Ok(file) => file,
            Err(e) if e.is_not_found() => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        if file.metadata()?.links != 1 {
            return Err(ConfigError::InvalidGithubPolicy);
        }
        let mut bytes = Vec::new();
        file.take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ConfigError::InvalidGithubPolicy)?;
        if bytes.len() > 16 * 1024 {
            return Err(ConfigError::InvalidGithubPolicy);
        }
        let mut policy: Self =
            serde_json::from_slice(&bytes).map_err(|_| ConfigError::InvalidGithubPolicy)?;
        policy.deny_labels = policy
            .deny_labels
            .into_iter()
            .map(|label| label.trim().to_owned())
            .filter(|label| !label.is_empty())
            .collect();
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::workflow_runs_on_labels;

    #[test]
    fn label_triggers_are_found_in_either_list_form_and_nothing_else() {
        assert!(workflow_runs_on_labels(
            "on:\n  pull_request:\n    types: [opened, labeled]\n"
        ));
        assert!(workflow_runs_on_labels(
            "on:\n  pull_request:\n    types:\n      - opened\n      - unlabeled\n"
        ));
        assert!(!workflow_runs_on_labels(
            "on:\n  pull_request:\n# types: [labeled]\njobs:\n  a:\n    if: github.event.action != 'labeled'\n"
        ));
    }
}
