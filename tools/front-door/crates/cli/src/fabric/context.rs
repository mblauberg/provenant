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

pub struct Context {
    pub roots: Roots,
    pub identity: Identity,
    pub config: FabricConfig,
    pub(super) executor: SystemExecutor,
    pub(super) home: PathBuf,
    pub(super) cwd: PathBuf,
}
pub(super) fn canonical(path: &Path) -> Result<PathBuf, EdgeError> {
    Ok(provenant_supervisor::fs::canonicalize(path)?)
}
pub(crate) fn private_dir(root: &Path, path: &Path) -> Result<Directory, EdgeError> {
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
            dir = match dir.open_verified_directory(name) {
                Ok(child) => child,
                Err(error) if error.is_not_found() => {
                    match dir.mkdir(name, 0o700) {
                        Ok(()) => dir.sync()?,
                        Err(error) if error.is_exists() => {}
                        Err(error) => return Err(error.into()),
                    }
                    dir.open_verified_directory(name)?
                }
                Err(error) => return Err(error.into()),
            };
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
        let utility_root = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| roots.state.as_path().to_owned());
        let utility_home = utility_root.join("tmp/git-home");
        private_dir(roots.state.as_path(), &utility_home)?;
        let lane = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some()
            || std::env::var_os(provenant_run::LANE_DEPTH_ENV).is_some();
        let git = crate::git::git_binary().ok_or(EdgeError::GitUnavailable)?;
        let runtime = provenant_confinement::UtilityRuntime::new(
            Arc::new(LocalFs),
            canonical(&git)?,
            &utility_home,
        )?;
        let executor = SystemExecutor::new(1)?;
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let discovered = runtime.project_location(&executor, cwd, budget, &CancelToken::new())?;
        let binding = if lane {
            provenant_config::lookup_project(&roots, &discovered.location, &LocalFs)?
                .ok_or(EdgeError::Invalid)?
        } else {
            provenant_config::bind_project(&roots, discovered.location, &LocalFs)?
        };
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
            executor,
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
        })
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
/// Publish a result in the selected lane or host result area.
pub fn publish<T: Serialize>(value: &CommandResult<T>, roots: &Roots) -> io::Result<AbsolutePath> {
    fn perform<T: Serialize>(
        value: &CommandResult<T>,
        roots: &Roots,
    ) -> Result<AbsolutePath, EdgeError> {
        let base = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| roots.state.as_path().to_owned());
        let path = base.join("tmp/results");
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
