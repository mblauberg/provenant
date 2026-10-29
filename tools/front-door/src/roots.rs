//! Read-only edge resolution. No provider, database, or legacy-content reads.
use provenant_core::{RootInputs, Roots};
use provenant_os::git::{RepositoryPath, repository_path};
use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum RootError {
    #[error("cannot resolve executable or root")]
    Io(#[from] io::Error),
    #[error("uninstalled binary requires an absolute directory in PROVENANT_ROOT")]
    Uninstalled,
    #[error("HOME must be an absolute directory")]
    Home,
    #[error("project must be a Git checkout")]
    Checkout,
    #[error(
        "session save requires a Git checkout from the working directory or --project PATH; change to the project checkout or pass --project PATH"
    )]
    SessionProjectMissing,
    #[error(
        "working directory resolves to {cwd}, but --project resolves to {selected}; change directory to the intended project or correct --project PATH"
    )]
    SessionProjectMismatch { cwd: PathBuf, selected: PathBuf },
    #[error("invalid state root: {0}")]
    State(#[from] provenant_core::RootError),
}
impl RootError {
    /// Stable machine-readable category for failures at the process boundary.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Checkout | Self::State(_) => "INVALID",
            Self::Io(_) | Self::Uninstalled | Self::Home => "ROOT",
            Self::SessionProjectMissing => "SESSION_PROJECT_MISSING",
            Self::SessionProjectMismatch { .. } => "SESSION_PROJECT_MISMATCH",
        }
    }
}
pub fn product_root() -> Result<PathBuf, RootError> {
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let parent = executable.parent().ok_or(RootError::Uninstalled)?;
    if parent.file_name() == Some(OsStr::new("bin")) {
        let candidate = parent.parent().ok_or(RootError::Uninstalled)?;
        if candidate.join("release.json").is_file() {
            return Ok(candidate.into());
        }
    }
    let root = PathBuf::from(std::env::var_os("PROVENANT_ROOT").ok_or(RootError::Uninstalled)?);
    if !root.is_absolute() || !root.is_dir() {
        return Err(RootError::Uninstalled);
    }
    Ok(fs::canonicalize(root)?)
}
pub fn selected_project(selected: Option<&Path>, check: bool) -> Result<PathBuf, RootError> {
    let path = match selected {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()?,
    };
    // A selected path that does not resolve is the caller's fault.
    let path = fs::canonicalize(path).map_err(|error| match selected {
        Some(_) => RootError::Checkout,
        None => error.into(),
    })?;
    if !path.is_dir() {
        return Err(RootError::Checkout);
    }
    if check || selected.is_some() {
        // Explicit projects and checks require a real checkout. Default root
        // reporting remains process-free.
        // The isolated Git environment ignores inherited redirects and config.
        let root = git_checkout(&path)?;
        return Ok(root);
    }
    Ok(path)
}
fn git_checkout(path: &Path) -> Result<PathBuf, RootError> {
    repository_path(path, RepositoryPath::Toplevel).map_err(discovery_error)
}

fn discovery_error(error: io::Error) -> RootError {
    match error
        .into_inner()
        .and_then(|error| error.downcast::<provenant_os::git::DiscoveryError>().ok())
    {
        Some(error) => match *error {
            provenant_os::git::DiscoveryError::Canonicalize(error) => RootError::Io(error),
            _ => RootError::Checkout,
        },
        None => RootError::Checkout,
    }
}

fn session_project(selected: Option<&Path>) -> Result<PathBuf, RootError> {
    let cwd = git_checkout(&std::env::current_dir()?).ok();
    let Some(selected) = selected else {
        return cwd.ok_or(RootError::SessionProjectMissing);
    };
    let selected =
        selected_project(Some(selected), true).map_err(|_| RootError::SessionProjectMissing)?;
    if let Some(cwd) = cwd
        && repository_path(&cwd, RepositoryPath::CommonDir).map_err(discovery_error)?
            != repository_path(&selected, RepositoryPath::CommonDir).map_err(discovery_error)?
    {
        return Err(RootError::SessionProjectMismatch { cwd, selected });
    }
    Ok(selected)
}
/// Validate caller-selected roots before composing any leaf. This creates no
/// state and does not require a product installation for an unavailable owner.
pub fn validate_globals(globals: &provenant_cli::Globals) -> Result<(), RootError> {
    if globals.state_dir.is_some() {
        resolve(globals, Workspace::Project, Product::Optional)?;
    } else if globals.project.is_some() {
        selected_project(globals.project.as_deref(), false)?;
    }
    Ok(())
}

/// Where a command's workspace comes from.
#[derive(Clone, Copy)]
pub enum Workspace<'a> {
    /// The selected or working directory, without a Git probe.
    Directory,
    /// As `Directory`, except that a selected project must be a checkout.
    Project,
    /// The Git checkout holding the selected or working directory.
    Checkout,
    /// Session saves require a checkout and reject conflicting selections.
    SessionSave,
    /// The home directory, for host-wide commands.
    Home,
    /// A canonical directory the command has already chosen.
    At(&'a Path),
}

/// Whether a command needs the installed product root. An optional product
/// root falls back to the workspace, which it then protects instead.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Product {
    Required,
    Optional,
}

/// One command's roots. Production reads `HOME`, the state root and harness
/// config roots from the environment only in [`resolve`].
#[derive(Debug, Clone)]
pub struct Resolved {
    pub assets: PathBuf,
    pub home: PathBuf,
    pub project: PathBuf,
    pub roots: Roots,
    /// `CLAUDE_CONFIG_DIR`, as the environment names it.
    pub claude_config: Option<PathBuf>,
    /// `CODEX_HOME`, as the environment names it.
    pub codex_home: Option<PathBuf>,
}

/// The one root resolver: the product root, the workspace, the canonical
/// home, the state root and the harness config roots, in that order.
pub fn resolve(
    globals: &provenant_cli::Globals,
    workspace: Workspace<'_>,
    product: Product,
) -> Result<Resolved, RootError> {
    let installed = match product {
        Product::Required => Some(product_root()?),
        Product::Optional => None,
    };
    let (project, home) = match workspace {
        Workspace::Home => {
            let home = home()?;
            (home.clone(), home)
        }
        Workspace::Directory => {
            let project = match &globals.project {
                Some(project) => fs::canonicalize(project).map_err(|_| RootError::Checkout)?,
                None => fs::canonicalize(std::env::current_dir()?)?,
            };
            (project, home()?)
        }
        Workspace::Project => (
            selected_project(globals.project.as_deref(), false)?,
            home()?,
        ),
        Workspace::Checkout => (selected_project(globals.project.as_deref(), true)?, home()?),
        Workspace::SessionSave => (session_project(globals.project.as_deref())?, home()?),
        Workspace::At(path) => (path.to_owned(), home()?),
    };
    let assets = match installed {
        Some(assets) => assets,
        None => product_root().unwrap_or_else(|_| project.clone()),
    };
    let mut inputs = RootInputs::new(home.clone(), assets.clone());
    inputs.state_dir = globals.state_dir.clone();
    inputs.environment_state_dir =
        std::env::var_os(provenant_core::STATE_DIRECTORY_ENV).map(PathBuf::from);
    inputs.workspaces.push(project.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    Ok(Resolved {
        assets,
        home,
        project,
        roots,
        claude_config: std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
        codex_home: std::env::var_os("CODEX_HOME").map(PathBuf::from),
    })
}

/// Bind a child store's `state_dir` with its resolved parent's `home` and
/// `assets`. No environment is read.
pub fn child(home: &Path, assets: &Path, state_dir: PathBuf) -> Result<Roots, RootError> {
    let mut inputs = RootInputs::new(home.to_owned(), assets.to_owned());
    inputs.state_dir = Some(state_dir);
    Ok(inputs.resolve(&provenant_os::fs::SystemPathResolver)?)
}
/// Canonical home used by root selection and home-relative Fabric identity.
fn home() -> Result<PathBuf, RootError> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or(RootError::Home)?);
    if !home.is_absolute() {
        return Err(RootError::Home);
    }
    let home = fs::canonicalize(home).map_err(|_| RootError::Home)?;
    if !home.is_dir() {
        return Err(RootError::Home);
    }
    Ok(home)
}

#[cfg(test)]
mod tests {
    #[test]
    fn discovery_canonicalize_error_remains_io() {
        let cause = provenant_os::git::DiscoveryError::Canonicalize(std::io::Error::from(
            std::io::ErrorKind::NotFound,
        ));
        let error = super::discovery_error(std::io::Error::other(cause));
        assert!(
            matches!(error, super::RootError::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn root_errors_have_stable_codes() {
        use crate::roots::RootError;

        assert_eq!(RootError::Uninstalled.code(), "ROOT");
        assert_eq!(RootError::Home.code(), "ROOT");
        assert_eq!(RootError::Checkout.code(), "INVALID");
        assert_eq!(
            RootError::SessionProjectMissing.code(),
            "SESSION_PROJECT_MISSING"
        );
        assert_eq!(
            RootError::SessionProjectMismatch {
                cwd: "/cwd".into(),
                selected: "/project".into(),
            }
            .code(),
            "SESSION_PROJECT_MISMATCH"
        );
    }

    #[test]
    fn resolve_directory_workspace_uses_the_current_directory() {
        let globals = provenant_cli::Globals::default();
        let resolved = crate::roots::resolve(
            &globals,
            crate::roots::Workspace::Directory,
            crate::roots::Product::Optional,
        )
        .unwrap();
        assert_eq!(
            resolved.project,
            std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap()
        );
    }

    #[test]
    fn resolve_at_workspace_uses_the_supplied_directory() {
        let temp = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(temp.path()).unwrap();
        let globals = provenant_cli::Globals::default();
        let resolved = crate::roots::resolve(
            &globals,
            crate::roots::Workspace::At(&path),
            crate::roots::Product::Optional,
        )
        .unwrap();
        assert_eq!(resolved.project, path);
    }

    #[test]
    fn child_resolves_the_supplied_state_directory() {
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let home = base.join("home");
        let assets = base.join("assets");
        let state = base.join("state").join("child");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&assets).unwrap();
        let roots = crate::roots::child(&home, &assets, state.clone()).unwrap();
        assert_eq!(roots.state.as_path(), state);
    }
}
