//! Host-owned advisory GitHub policy for a bound project.
use crate::ConfigError;
use provenant_core::{ProjectId, StateRoot};
use provenant_os::fs::Directory;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GithubAppConfig {
    pub app_id: u64,
    #[serde(default)]
    pub installation_id: Option<u64>,
    #[serde(default)]
    pub private_key_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GithubPolicy {
    #[serde(default)]
    pub deny_verbs: Vec<String>,
    #[serde(default)]
    pub deny_labels: Vec<String>,
    /// Per-project App override; wins over `apps`.
    #[serde(default)]
    pub app: Option<GithubAppConfig>,
    /// One App per repository owner, serving every repository under it.
    #[serde(default)]
    pub apps: BTreeMap<String, GithubAppConfig>,
}

impl GithubPolicy {
    /// The App for a repository owned by `owner`: the project override, else
    /// the owner's App (owner names compare case-insensitively).
    pub fn app_for(&self, owner: Option<&str>) -> Option<&GithubAppConfig> {
        self.app.as_ref().or_else(|| {
            let owner = owner?.to_ascii_lowercase();
            self.apps
                .iter()
                .find(|(name, _)| name.to_ascii_lowercase() == owner)
                .map(|(_, app)| app)
        })
    }

    /// Every configured key file, which no lane's sandbox may read.
    pub fn key_paths(&self) -> impl Iterator<Item = &Path> {
        self.app
            .iter()
            .chain(self.apps.values())
            .flat_map(|app| app.private_key_path.as_deref())
    }

    /// Read the policy for the host-bound project. Only absence means empty.
    pub fn read(state: &StateRoot, project_id: ProjectId) -> Result<Self, ConfigError> {
        Self::read_at(state.as_path(), project_id)
    }

    /// As `read`, for a caller that holds the state directory as a path.
    pub fn read_at(state: &Path, project_id: ProjectId) -> Result<Self, ConfigError> {
        // A state root that does not exist holds no policy file either.
        let root = match Directory::open_anchor(state) {
            Ok(root) => root,
            Err(e) if e.is_not_found() => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
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
