//! Root selection consumes one captured environment/home snapshot. Alias binding
//! is supplied by OS crate's safe filesystem boundary, never performed here.
use std::io;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

/// Run storage for children submitted by one confined attempt.
pub const RUN_STATE_DIRECTORY_ENV: &str = "PROVENANT_RUN_STATE_DIR";

pub const STATE_DIRECTORY_ENV: &str = "PROVENANT_STATE_DIR";

/// The home-relative default state root, as a literal for `concat!`. Every
/// other spelling of the default root derives from this one.
#[macro_export]
macro_rules! default_state_suffix {
    () => {
        ".local/share/provenant/v2"
    };
}
/// The home-relative default state root.
pub const DEFAULT_STATE_SUFFIX: &str = default_state_suffix!();
/// The 1.x default state root. 2.0 never writes it: install copies owner
/// configuration out of it once, and no state root may lie inside it.
pub const LEGACY_V1_STATE_SUFFIX: &str = ".local/share/provenant/v1";

/// The home whose default state root is `state`, when it is one.
pub fn default_state_home(state: &Path) -> Option<&Path> {
    home_with_suffix(state, DEFAULT_STATE_SUFFIX)
}
/// The home whose 1.x default state root is `state`, when it is one.
pub fn legacy_state_home(state: &Path) -> Option<&Path> {
    home_with_suffix(state, LEGACY_V1_STATE_SUFFIX)
}
fn home_with_suffix<'a>(state: &'a Path, suffix: &str) -> Option<&'a Path> {
    let home = state
        .ancestors()
        .nth(Path::new(suffix).components().count())?;
    (state == home.join(suffix)).then_some(home)
}

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
pub struct StateRoot {
    path: PathBuf,
    warnings: Vec<RootWarning>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootWarning {
    pub path: PathBuf,
    pub protected: PathBuf,
    pub source: &'static str,
}
impl RootWarning {
    pub fn message(&self) -> String {
        format!(
            "state root {} is nested in advisory protected location {} (source: {}); proceeding",
            self.path.display(),
            self.protected.display(),
            self.source
        )
    }
}
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
            ProtectedRoot::advisory(inputs.checkout.clone()),
            ProtectedRoot::boundary(
                inputs.checkout.join(".git"),
                RootBoundary::GitIsolation,
            ),
            ProtectedRoot::boundary(inputs.home.join(".agents"), RootBoundary::Credentials),
            ProtectedRoot::advisory(inputs.home.join(".local/state/agent-harness")),
            ProtectedRoot::boundary(
                inputs.home.join(".local/state/provenant"),
                RootBoundary::DataIntegrity,
            ),
            ProtectedRoot::boundary(
                inputs.home.join(LEGACY_V1_STATE_SUFFIX),
                RootBoundary::DataIntegrity,
            ),
        ];
        protected.extend(inputs.workspaces.iter().flat_map(|path| {
            [
                ProtectedRoot::advisory(path.clone()),
                ProtectedRoot::boundary(path.join(".git"), RootBoundary::GitIsolation),
                ProtectedRoot::boundary(path.join(".agent-run"), RootBoundary::DataIntegrity),
            ]
        }));
        protected.extend(
            inputs
                .old_roots
                .iter()
                .cloned()
                .map(|path| ProtectedRoot::boundary(path, RootBoundary::DataIntegrity)),
        );
        for path in &protected {
            absolute_normal(&path.path)?;
        }
        let _ = classify(&candidate, &inputs.home, &protected, None)?;
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
            .map(|path| resolve(&path.path))
            .collect::<Result<Vec<_>, _>>()?;
        let bound = resolve(&candidate)?;
        let warnings = classify(
            &bound,
            &bound_home,
            &protected,
            Some(&bound_protected),
        )?;
        Ok(Self {
            path: bound,
            warnings,
        })
    }
    pub fn as_path(&self) -> &Path {
        &self.path
    }
    pub fn warnings(&self) -> &[RootWarning] {
        &self.warnings
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
#[derive(Clone, Copy)]
enum RootBoundary {
    Credentials,
    GitIsolation,
    DataIntegrity,
}

struct ProtectedRoot {
    path: PathBuf,
    boundary: Option<RootBoundary>,
    source: &'static str,
}
impl ProtectedRoot {
    fn boundary(path: PathBuf, boundary: RootBoundary) -> Self {
        Self {
            path,
            boundary: Some(boundary),
            source: match boundary {
                RootBoundary::Credentials => "built-in credential-root policy",
                RootBoundary::GitIsolation => "built-in Git-isolation policy",
                RootBoundary::DataIntegrity => "built-in state-integrity policy",
            },
        }
    }

    fn advisory(path: PathBuf) -> Self {
        Self {
            path,
            boundary: None,
            source: "built-in state-root selection policy",
        }
    }
}

fn classify(
    candidate: &Path,
    home: &Path,
    protected: &[ProtectedRoot],
    bound_paths: Option<&[PathBuf]>,
) -> Result<Vec<RootWarning>, RootError> {
    if candidate.parent().is_none()
        || candidate == home
        || candidate.components().any(|c| c.as_os_str() == ".agent-run")
    {
        return Err(RootError::Protected {
            path: candidate.into(),
        });
    }
    let mut warnings = Vec::new();
    for (index, root) in protected.iter().enumerate() {
        let path = bound_paths
            .and_then(|paths| paths.get(index))
            .unwrap_or(&root.path);
        let nested = candidate.starts_with(path) && candidate != path;
        let ancestor_or_equal = path.starts_with(candidate);
        if !nested && !ancestor_or_equal {
            continue;
        }
        if root.boundary.is_some() || !nested {
            return Err(RootError::Protected {
                path: candidate.into(),
            });
        }
        warnings.push(RootWarning {
            path: candidate.into(),
            protected: path.clone(),
            source: root.source,
        });
    }
    Ok(warnings)
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
            Path::new("/fixture/home").join(DEFAULT_STATE_SUFFIX)
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
    fn boundary_roots_refuse_overlap_and_advisory_nested_roots_warn() {
        let mut input = inputs();
        input.workspaces.push("/selected/workspace".into());
        input.old_roots.push("/explicit/old".into());
        for forbidden in [
            "/",
            "/fixture/home",
            "/fixture/checkout",
            "/fixture/checkout/.git/objects",
            "/fixture/home/.agents/new",
            "/fixture/home/.local/state/provenant/new",
            "/selected/workspace/.agent-run/new",
            "/selected/workspace/.git/objects",
            "/any/.agent-run/tail",
            "/explicit/old/new",
        ] {
            input.state_dir = Some(forbidden.into());
            assert!(
                matches!(input.resolve(&Resolver), Err(RootError::Protected { .. })),
                "{forbidden}"
            );
        }
        // 2.0 never writes the 1.x root, so no state root may overlap it.
        let legacy = Path::new("/fixture/home").join(LEGACY_V1_STATE_SUFFIX);
        for forbidden in [legacy.clone(), legacy.join("nested")] {
            input.state_dir = Some(forbidden);
            assert!(matches!(
                input.resolve(&Resolver),
                Err(RootError::Protected { .. })
            ));
        }
        for nested in [
            "/fixture/checkout/nested",
            "/selected/workspace/nested",
            "/fixture/home/.local/state/agent-harness/fabric",
        ] {
            input.state_dir = Some(nested.into());
            let roots = input.resolve(&Resolver).unwrap();
            assert_eq!(roots.state.as_path(), Path::new(nested));
            let [warning] = roots.state.warnings() else {
                panic!("nested advisory root must expose one warning")
            };
            assert!(warning.message().contains("source: built-in state-root selection policy"));
        }
        input.state_dir = Some("/explicit/old-sibling".into());
        assert!(input.resolve(&Resolver).unwrap().state.warnings().is_empty());
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
