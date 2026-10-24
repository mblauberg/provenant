//! Root selection consumes one captured environment/home snapshot. Alias binding
//! is supplied by supervisor's safe filesystem boundary, never performed here.
use std::io;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

pub const STATE_DIRECTORY_ENV: &str = "PROVENANT_STATE_DIR";
const DEFAULT_STATE_SUFFIX: &str = ".local/share/provenant/v1";

/// The sole filesystem seam for root binding. Implementations resolve existing
/// ancestor aliases and append only normal missing components; they must fail
/// on loops/inaccessible ancestors, create nothing, and never open legacy root
/// contents. Supervisor owns the production binding and handle-based checks.
pub trait PathResolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf>;
}

#[derive(Debug, Clone)]
pub struct RootInputs {
    pub state_dir: Option<PathBuf>,
    pub environment_state_dir: Option<PathBuf>,
    pub home: PathBuf,
    pub checkout: PathBuf,
    pub workspaces: Vec<PathBuf>,
    pub old_roots: Vec<PathBuf>,
}
impl RootInputs {
    pub fn new(home: PathBuf, checkout: PathBuf) -> Self {
        Self {
            state_dir: None,
            environment_state_dir: None,
            home,
            checkout,
            workspaces: Vec::new(),
            old_roots: Vec::new(),
        }
    }
    pub fn resolve(&self, resolver: &impl PathResolver) -> Result<Roots, RootError> {
        StateRoot::resolve(self, resolver).map(|state| Roots { state })
    }
}

#[derive(Debug, Error)]
pub enum RootError {
    #[error("root input must be an absolute path with only normal components: {path}")]
    InvalidPath { path: PathBuf },
    #[error("state root is a protected location: {path}")]
    Protected { path: PathBuf },
    #[error("cannot bind root input: {path}")]
    Resolution {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRoot(PathBuf);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pub state: StateRoot,
}
impl StateRoot {
    pub fn resolve(inputs: &RootInputs, resolver: &impl PathResolver) -> Result<Self, RootError> {
        absolute_normal(&inputs.home)?;
        let candidate = inputs
            .state_dir
            .as_ref()
            .or(inputs.environment_state_dir.as_ref())
            .cloned()
            .unwrap_or_else(|| inputs.home.join(DEFAULT_STATE_SUFFIX));
        absolute_normal(&candidate)?;
        let mut protected = vec![
            inputs.checkout.clone(),
            inputs.home.join(".agents"),
            inputs.home.join(".local/state/agent-harness"),
            inputs.home.join(".local/state/provenant"),
        ];
        protected.extend(inputs.workspaces.iter().map(|p| p.join(".agent-run")));
        protected.extend(inputs.old_roots.iter().cloned());
        for path in &protected {
            absolute_normal(path)?;
        }
        refuse(&candidate, &inputs.home, &protected)?;
        let resolve = |path: &Path| -> Result<PathBuf, RootError> {
            let bound = resolver.resolve_existing_ancestor(path).map_err(|source| {
                RootError::Resolution {
                    path: path.into(),
                    source,
                }
            })?;
            absolute_normal(&bound)?;
            Ok(bound)
        };
        let bound_home = resolve(&inputs.home)?;
        let bound_protected = protected
            .iter()
            .map(|path| resolve(path))
            .collect::<Result<Vec<_>, _>>()?;
        let bound = resolve(&candidate)?;
        refuse(&bound, &bound_home, &bound_protected)?;
        Ok(Self(bound))
    }
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}
impl AsRef<Path> for StateRoot {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

fn absolute_normal(path: &Path) -> Result<(), RootError> {
    if !path.is_absolute()
        || crate::paths::has_dot_or_nul(path)
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(RootError::InvalidPath { path: path.into() });
    }
    Ok(())
}
fn refuse(candidate: &Path, home: &Path, protected: &[PathBuf]) -> Result<(), RootError> {
    if candidate.parent().is_none()
        || candidate == home
        || candidate
            .components()
            .any(|c| c.as_os_str() == ".agent-run")
        || protected.iter().any(|root| candidate.starts_with(root))
    {
        return Err(RootError::Protected {
            path: candidate.into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
            Ok(path.into())
        }
    }
    fn inputs() -> RootInputs {
        RootInputs::new("/fixture/home".into(), "/fixture/checkout".into())
    }
    #[test]
    fn explicit_selection_has_priority_and_never_falls_back_on_invalid_input() {
        let mut input = inputs();
        assert_eq!(
            input.resolve(&Resolver).unwrap().state.as_path(),
            Path::new("/fixture/home/.local/share/provenant/v1")
        );
        input.environment_state_dir = Some("/fixture/env".into());
        assert_eq!(
            input.resolve(&Resolver).unwrap().state.as_path(),
            Path::new("/fixture/env")
        );
        input.state_dir = Some("/fixture/flag".into());
        assert_eq!(
            input.resolve(&Resolver).unwrap().state.as_path(),
            Path::new("/fixture/flag")
        );
        for bad in ["", "relative", "/fixture/../escape", "/fixture/./tail"] {
            input.state_dir = Some(bad.into());
            assert!(matches!(
                input.resolve(&Resolver),
                Err(RootError::InvalidPath { .. })
            ));
        }
    }
    #[test]
    fn roots_refuse_home_checkout_old_roots_and_any_agent_run_component() {
        let mut input = inputs();
        input.workspaces.push("/selected/workspace".into());
        input.old_roots.push("/explicit/old".into());
        for forbidden in [
            "/",
            "/fixture/home",
            "/fixture/checkout",
            "/fixture/checkout/nested",
            "/fixture/home/.agents/new",
            "/fixture/home/.local/state/agent-harness/fabric",
            "/fixture/home/.local/state/provenant/new",
            "/selected/workspace/.agent-run/new",
            "/any/.agent-run/tail",
            "/explicit/old/new",
        ] {
            input.state_dir = Some(forbidden.into());
            assert!(
                matches!(input.resolve(&Resolver), Err(RootError::Protected { .. })),
                "{forbidden}"
            );
        }
        input.state_dir = Some("/explicit/old-sibling".into());
        assert!(input.resolve(&Resolver).is_ok());
    }
    #[test]
    fn canonical_aliases_are_checked_after_binding_and_failures_are_not_absence() {
        struct Aliases;
        impl PathResolver for Aliases {
            fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
                if let Ok(tail) = path.strip_prefix("/alias") {
                    return Ok(Path::new("/fixture/home/.agents").join(tail));
                }
                if path == Path::new("/unreadable") {
                    return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                }
                Ok(path.into())
            }
        }
        let mut input = inputs();
        input.state_dir = Some("/alias/missing/tail".into());
        assert!(matches!(
            input.resolve(&Aliases),
            Err(RootError::Protected { .. })
        ));
        input.state_dir = Some("/unreadable".into());
        assert!(matches!(
            input.resolve(&Aliases),
            Err(RootError::Resolution { .. })
        ));
    }
}
