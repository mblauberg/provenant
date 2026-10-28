//! Host-owned advisory GitHub policy for a bound project.
use crate::ConfigError;
use provenant_core::{ProjectId, StateRoot};
use provenant_os::fs::Directory;
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, io::Read};

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
