use super::EdgeError;
use provenant_core::{CancelToken, Roots};
use provenant_fabric_runtime::{
    config::FabricConfig,
    identity::{CrossHostProjectKey, Identity, IdentityError},
};

use provenant_supervisor::{
    exec::SystemExecutor,
    fs::{Directory, LocalFs},
};

use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub(super) struct Context {
    pub roots: Roots,
    pub identity: Identity,
    pub config: FabricConfig,
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
        let config = provenant_config::read_with_domains(
            &roots,
            &LocalFs,
            &provenant_cli::domains::SECTIONS,
        )?
        .domains
        .fabric;
        private_dir(roots.state.as_path(), roots.state.as_path())?;
        let utility_home = roots.state.as_path().join("tmp/git-home");
        private_dir(roots.state.as_path(), &utility_home)?;
        let lane = std::env::var_os(provenant_run::LANE_DEPTH_ENV).is_some();
        let git = [
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
            "/usr/bin/git",
            "/bin/git",
        ]
        .into_iter()
        .map(Path::new)
        .filter(|path| !lane || *path == Path::new("/usr/bin/git"))
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
}
