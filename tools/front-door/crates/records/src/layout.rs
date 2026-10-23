//! Shared run layout contract. Discovery supplies resolved paths; the future
//! core owner runs Git with GIT_* removed and a five-second deadline. Recovery
//! remains with the process and run owners, rather than importing them here.
use std::path::{Path, PathBuf};

use jiff::{Timestamp, tz::TimeZone};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunKind {
    Dispatch,
    Batch,
    Orch,
    Delivery,
    Mission,
    Review,
    Wf,
}
impl RunKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::Batch => "batch",
            Self::Orch => "orch",
            Self::Delivery => "delivery",
            Self::Mission => "mission",
            Self::Review => "review",
            Self::Wf => "wf",
        }
    }
}
impl TryFrom<&str> for RunKind {
    type Error = LayoutError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "dispatch" => Ok(Self::Dispatch),
            "batch" => Ok(Self::Batch),
            "orch" => Ok(Self::Orch),
            "delivery" => Ok(Self::Delivery),
            "mission" => Ok(Self::Mission),
            "review" => Ok(Self::Review),
            "wf" => Ok(Self::Wf),
            _ => Err(LayoutError::RunKind),
        }
    }
}
#[derive(Debug, Error)]
pub enum LayoutError {
    #[error("invalid run kind")]
    RunKind,
    #[error("run layout I/O failed at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunLayout {
    workspace_root: PathBuf,
}
impl RunLayout {
    /// `cwd` and `git_common_dir` must already have Path.resolve semantics.
    /// Failed/empty Git discovery is None; worktrees share the common directory.
    pub fn from_resolved(cwd: &Path, git_common_dir: Option<&Path>) -> Self {
        Self {
            workspace_root: git_common_dir
                .and_then(Path::parent)
                .unwrap_or(cwd)
                .to_owned(),
        }
    }
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
    pub fn agent_run(&self) -> PathBuf {
        self.workspace_root.join(".agent-run")
    }
    pub fn runs(&self) -> PathBuf {
        self.agent_run().join("runs")
    }
    pub fn index(&self) -> PathBuf {
        self.runs().join("index.jsonl")
    }
    /// Explicit run paths can identify a non-Git workspace from a nested cwd.
    /// An unrelated ancestor .agent-run never changes discovery for a new run.
    pub fn workspace_for_run(&self, path: &Path, cwd: &Path, root_has_git: bool) -> PathBuf {
        if path.starts_with(&self.workspace_root) {
            return self.workspace_root.clone();
        }
        if self.workspace_root == cwd && !root_has_git {
            for ancestor in path.ancestors().skip(1) {
                if ancestor
                    .file_name()
                    .is_some_and(|name| name == ".agent-run")
                {
                    if let Some(parent) = ancestor.parent() {
                        if cwd.starts_with(parent) {
                            return parent.to_owned();
                        }
                    }
                }
            }
        }
        self.workspace_root.clone()
    }
    pub fn contains_run(&self, path: &Path, cwd: &Path, root_has_git: bool) -> bool {
        path.starts_with(self.workspace_for_run(path, cwd, root_has_git))
    }
    pub fn run_path(&self, at: Timestamp, kind: RunKind, slug: &str, entropy: [u8; 3]) -> PathBuf {
        let date = at.to_zoned(TimeZone::UTC).strftime("%Y%m%d-%H%M");
        self.runs().join(format!(
            "{date}-{}-{}-{:02x}{:02x}{:02x}",
            kind.as_str(),
            sanitize_slug(slug),
            entropy[0],
            entropy[1],
            entropy[2]
        ))
    }
}

pub fn sanitize_slug(slug: &str) -> String {
    let mut result = String::new();
    let mut replacing = false;
    for ch in slug.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            result.push(ch);
            replacing = false;
        } else if !replacing {
            result.push('-');
            replacing = true;
        }
    }
    let result: String = result.trim_matches(['-', '.']).chars().take(64).collect();
    if result.is_empty() {
        "task".into()
    } else {
        result
    }
}

#[cfg(test)]
mod tests;

/// Python layout.py#unit-01 effects, to be implemented by the discovery/run owners.
/// Optional cwd means the process cwd; paths have Python Path.resolve semantics.
/// new_run_dir must mkdir(parents=True, exist_ok=False), then optional _owner.
/// reap_orphans returns directory strings, and takes Unix float seconds.
pub trait LayoutIo {
    fn run_root(&self, cwd: Option<&Path>) -> Result<PathBuf, LayoutError>;
    fn run_workspace(&self, path: &Path, cwd: Option<&Path>) -> Result<PathBuf, LayoutError>;
    fn contains_run(&self, path: &Path, cwd: Option<&Path>) -> Result<bool, LayoutError>;
    /// None selects Python defaults: kind="dispatch", slug="task", owner=false.
    fn new_run_dir(
        &self,
        cwd: Option<&Path>,
        kind: Option<&str>,
        slug: Option<&str>,
        owner: Option<bool>,
    ) -> Result<PathBuf, LayoutError>;
    fn reap_orphans(&self, cwd: Option<&Path>, at: Option<f64>)
    -> Result<Vec<String>, LayoutError>;
}
