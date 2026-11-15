//! Optional setup for newly created worktrees, using host and project settings.
use provenant_core::ProjectId;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorktreeSettings {
    /// Total wall-clock budget for setup, in seconds.
    pub timeout_seconds: u64,
    pub projects: BTreeMap<String, ProjectWorktreeSettings>,
}

impl Default for WorktreeSettings {
    fn default() -> Self {
        Self {
            timeout_seconds: 300,
            projects: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectWorktreeSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

/// Effective post-create command time budget for one project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeSetup {
    pub timeout_seconds: u64,
}

impl WorktreeSettings {
    pub fn read(state: &Path) -> std::io::Result<Self> {
        crate::loader::read_settings(state, "worktrees", Self::validate)
    }

    pub fn for_project(&self, project: ProjectId) -> WorktreeSetup {
        let project = self.projects.get(&project.to_string());
        WorktreeSetup {
            timeout_seconds: project
                .and_then(|p| p.timeout_seconds)
                .unwrap_or(self.timeout_seconds),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !(1..=3600).contains(&self.timeout_seconds)
            || self
                .projects
                .values()
                .filter_map(|p| p.timeout_seconds)
                .any(|seconds| !(1..=3600).contains(&seconds))
        {
            return Err("timeout_seconds_range");
        }
        Ok(())
    }
}
