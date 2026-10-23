//! Fabric identity consumes config's canonical project binding. No Git discovery.
use provenant_config::ProjectBinding;
use provenant_core::{ActorId, HostId, ProjectId};
use provenant_records::AbsolutePath;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::path::{Component, Path};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("federation requires a canonical project beneath the canonical home")]
    OutsideHome,
    #[error("identity requires UTF-8 project keys and labels")]
    Encoding,
    #[error("agent/client label must be 1-128 ASCII letters, digits, dot, underscore or hyphen")]
    Label,
}

pub fn validate_label(label: &str) -> Result<(), IdentityError> {
    if label.is_empty()
        || label.len() > 128
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(IdentityError::Label);
    }
    Ok(())
}

/// A separate federation namespace, never a host-local ProjectId. The source
/// is config's canonical common directory (or canonical non-Git directory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CrossHostProjectKey(String);
impl CrossHostProjectKey {
    pub fn from_home(home: &Path, common_dir: &Path) -> Result<Self, IdentityError> {
        if !home.is_absolute()
            || !common_dir.is_absolute()
            || home
                .components()
                .chain(common_dir.components())
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err(IdentityError::OutsideHome);
        }
        let relative = common_dir
            .strip_prefix(home)
            .map_err(|_| IdentityError::OutsideHome)?;
        if relative.as_os_str().is_empty() {
            return Err(IdentityError::OutsideHome);
        }
        let relative = relative.to_str().ok_or(IdentityError::Encoding)?;
        let mut digest = Sha256::new();
        digest.update(b"provenant.fabric.project.v1\0");
        digest.update(relative.as_bytes());
        Ok(Self(provenant_core::hex(&digest.finalize())))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FederatedProject {
    pub project: ProjectId,
    pub key: CrossHostProjectKey,
}
pub fn bind_federated_project(
    binding: &ProjectBinding,
    key: CrossHostProjectKey,
) -> FederatedProject {
    FederatedProject {
        project: binding.project_id(),
        key,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Address {
    pub host: HostId,
    pub agent: String,
    pub actor: ActorId,
}
impl Address {
    pub fn new(host: HostId, agent: &str) -> Result<Self, IdentityError> {
        validate_label(agent)?;
        let mut digest = Sha256::new();
        digest.update(b"provenant.fabric.actor.v1\0");
        digest.update(host.to_string().as_bytes());
        digest.update([0]);
        digest.update(agent.as_bytes());
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest.finalize()[..16]);
        Ok(Self {
            host,
            agent: agent.into(),
            actor: ActorId::from_bytes(bytes),
        })
    }
}

#[derive(Debug, Serialize)]
pub struct Identity {
    pub project: ProjectId,
    pub host: HostId,
    pub inbox: Address,
    pub client: String,
    pub cwd: AbsolutePath,
    pub common_dir: AbsolutePath,
    pub federation: Option<FederatedProject>,
}
impl Identity {
    pub fn bind(
        binding: &ProjectBinding,
        host: HostId,
        cwd: &Path,
        config: &crate::config::FabricConfig,
        agent_override: Option<&std::ffi::OsStr>,
        key: Option<CrossHostProjectKey>,
    ) -> Result<Self, IdentityError> {
        let agent = match agent_override {
            Some(value) => value.to_str().ok_or(IdentityError::Encoding)?,
            None => {
                if config.agent_label.is_empty() {
                    &config.client_label
                } else {
                    &config.agent_label
                }
            }
        };
        validate_label(&config.client_label)?;
        let federation = key.map(|key| bind_federated_project(binding, key));
        Ok(Self {
            project: binding.project_id(),
            host,
            inbox: Address::new(host, agent)?,
            client: config.client_label.clone(),
            cwd: AbsolutePath::new(cwd).map_err(|_| IdentityError::Encoding)?,
            common_dir: AbsolutePath::new(binding.common_dir())
                .map_err(|_| IdentityError::Encoding)?,
            federation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_are_home_relative_and_local_ids_are_separate() {
        assert_eq!(
            CrossHostProjectKey::from_home(Path::new("/home/a"), Path::new("/home/a/repo/.git"))
                .unwrap(),
            CrossHostProjectKey::from_home(Path::new("/Users/b"), Path::new("/Users/b/repo/.git"))
                .unwrap()
        );
        assert!(
            CrossHostProjectKey::from_home(Path::new("/home/a"), Path::new("/tmp/repo")).is_err()
        );
        assert!(
            CrossHostProjectKey::from_home(Path::new("/home/a"), Path::new("/home/a/../repo"))
                .is_err()
        );
        assert!(Address::new(HostId::from_bytes([1; 16]), "bad label").is_err());
        assert_ne!(
            Address::new(HostId::from_bytes([1; 16]), "agent")
                .unwrap()
                .actor,
            Address::new(HostId::from_bytes([2; 16]), "agent")
                .unwrap()
                .actor
        );
    }
}
