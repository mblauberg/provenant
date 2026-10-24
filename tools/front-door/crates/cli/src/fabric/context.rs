use super::{EdgeError, Lease};
use provenant_core::{CancelToken, Roots};
use provenant_fabric_runtime::{
    config::FabricConfig,
    identity::{CrossHostProjectKey, Identity, IdentityError},
};
use provenant_records::{AbsolutePath, CommandResult};
use provenant_supervisor::{
    exec::SystemExecutor,
    fs::{Directory, LocalFs, PublishMode},
};
use serde::Serialize;
use std::{
    ffi::OsStr,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub(super) struct Context {
    pub roots: Roots,
    pub identity: Identity,
    config: FabricConfig,
}
pub(super) fn canonical(path: &Path) -> Result<PathBuf, EdgeError> {
    Ok(provenant_supervisor::fs::canonicalize(path)?)
}
fn private_dir(root: &Path, path: &Path) -> Result<Directory, EdgeError> {
    let mut dir = match Directory::open_anchor(root) {
        Ok(dir) => dir,
        Err(error) if error.is_not_found() => {
            // Only the configured root may be created. Its parent must exist;
            // the trusted initial path permits ancestor aliases such as /var.
            let parent = Directory::open_anchor(root.parent().ok_or(EdgeError::Invalid)?)?;
            let name = root.file_name().ok_or(EdgeError::Invalid)?;
            match parent.mkdir(name, 0o700) {
                Ok(()) => parent.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            Directory::open_anchor(root)?
        }
        Err(error) => return Err(error.into()),
    };
    if dir.metadata()?.mode & 0o077 != 0 {
        return Err(EdgeError::Invalid);
    }
    for part in path
        .strip_prefix(root)
        .map_err(|_| EdgeError::Invalid)?
        .components()
    {
        if let Component::Normal(name) = part {
            match dir.mkdir(name, 0o700) {
                Ok(()) => dir.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            dir = dir.open_verified_directory(name)?;
            if dir.metadata()?.mode & 0o077 != 0 {
                return Err(EdgeError::Invalid);
            }
        } else {
            return Err(EdgeError::Invalid);
        }
    }
    Ok(dir)
}
impl Context {
    pub fn open(roots: Roots, home: &Path, cwd: &Path) -> Result<Self, EdgeError> {
        // Validate configuration before allocating identities or opening SQLite.
        let config =
            provenant_config::read_with_domains(&roots, &LocalFs, &crate::domains::SECTIONS)?
                .domains
                .fabric;
        private_dir(roots.state.as_path(), roots.state.as_path())?;
        let utility_home = roots.state.as_path().join("tmp/git-home");
        private_dir(roots.state.as_path(), &utility_home)?;
        let git = [
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
            "/usr/bin/git",
            "/bin/git",
        ]
        .into_iter()
        .map(Path::new)
        .find(|path| provenant_supervisor::sys::is_executable(path))
        .ok_or(EdgeError::Invalid)?;
        let runtime = provenant_confinement::UtilityRuntime::new(
            Arc::new(LocalFs),
            canonical(git)?,
            &utility_home,
        )?;
        let executor = SystemExecutor::new(1)?;
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let discovered = runtime.project_location(&executor, cwd, budget, &CancelToken::new())?;
        let binding = provenant_config::bind_project(&roots, discovered.location, &LocalFs)?;
        let key = if discovered.repository.is_some() {
            match CrossHostProjectKey::from_home(home, binding.common_dir()) {
                Ok(key) => Some(key),
                Err(IdentityError::OutsideHome) => None,
                Err(error) => return Err(error.into()),
            }
        } else {
            None
        };
        let host = provenant_config::bind_host(&roots, &LocalFs)?;
        let identity = Identity::bind(
            &binding,
            host,
            cwd,
            &config,
            std::env::var_os("PROVENANT_AGENT_LABEL").as_deref(),
            key,
        )?;
        Ok(Self {
            roots,
            identity,
            config,
        })
    }
    pub fn require_local_home(&self) -> Result<(), EdgeError> {
        if let Some(project) = &self.identity.federation {
            let key = serde_json::to_value(&project.key)?;
            if let Some(host) = key.as_str().and_then(|key| self.config.home_hosts.get(key))
                && *host != self.identity.host
            {
                return Err(EdgeError::HomeUnavailable);
            }
        }
        Ok(())
    }
    pub fn text(&self, text: &str) -> Result<(), EdgeError> {
        if text.len() as u64 > self.config.limits.body_bytes {
            return Err(EdgeError::Invalid);
        }
        Ok(())
    }
    pub fn texts(&self, text: &str, note: Option<&str>) -> Result<(), EdgeError> {
        self.text(text)?;
        if let Some(note) = note {
            self.text(note)?;
        }
        Ok(())
    }
    pub fn page(&self, limit: u64) -> Result<(), EdgeError> {
        if limit > self.config.limits.page_rows {
            return Err(EdgeError::Invalid);
        }
        Ok(())
    }
    pub fn lease(&self, lease: Lease) -> Result<u64, EdgeError> {
        if lease.lease_seconds > self.config.limits.lease_seconds {
            return Err(EdgeError::Invalid);
        }
        Ok(lease.lease_seconds * 1000)
    }
}
pub(super) fn publish<T: Serialize>(
    value: &CommandResult<T>,
    roots: &Roots,
) -> io::Result<AbsolutePath> {
    fn perform<T: Serialize>(
        value: &CommandResult<T>,
        roots: &Roots,
    ) -> Result<AbsolutePath, EdgeError> {
        let path = roots.state.as_path().join("tmp/results");
        let dir = private_dir(roots.state.as_path(), &path)?;
        let name = provenant_supervisor::fs::temporary_name()?;
        let bytes = serde_json::to_vec(value)?;
        match dir.atomic_write(&name, ".result-", &bytes, PublishMode::NoReplace)? {
            provenant_supervisor::fs::Publication::Durable => {}
            provenant_supervisor::fs::Publication::VisibleDurabilityUnconfirmed { sync_error } => {
                return Err(sync_error.into());
            }
        }
        AbsolutePath::new(&path.join(OsStr::new(&name))).map_err(|_| EdgeError::Invalid)
    }
    perform(value, roots).map_err(|_| io::Error::other("cannot publish oversized result"))
}
