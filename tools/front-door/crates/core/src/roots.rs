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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootInputs {
    pub state_dir: Option<PathBuf>,
    pub environment_state_dir: Option<PathBuf>,
    pub home: PathBuf,
    pub checkout: PathBuf,
    pub workspaces: Vec<PathBuf>,
    /// Existing Git markers, private directories and common directories,
    /// captured by the OS edge. Release assets need not be a checkout.
    pub git_metadata: Vec<PathBuf>,
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
            git_metadata: Vec::new(),
            old_roots: Vec::new(),
        }
    }
    /// Resolve captured inputs without filesystem discovery. Production OS
    /// binding supplies existing Git metadata before entering this pure core.
    pub fn resolve(&self, resolver: &impl PathResolver) -> Result<Roots, RootError> {
        StateRoot::resolve(self, resolver).map(|state| Roots {
            state,
            inputs: self.clone(),
        })
    }
}

#[derive(Debug, Error)]
pub enum RootError {
    #[error("root input must be an absolute path with only normal components: {path}")]
    InvalidPath { path: PathBuf },
    #[error(
        "state root {path} overlaps protected location {protected} ({boundary:?}; source: {policy_source})"
    )]
    Protected {
        path: PathBuf,
        protected: PathBuf,
        boundary: RootBoundary,
        policy_source: &'static str,
    },
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
            "state root {} overlaps advisory location {} (source: {}); proceeding",
            self.path.display(),
            self.protected.display(),
            self.source
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pub state: StateRoot,
    inputs: RootInputs,
}
impl Roots {
    /// Rebind only the state candidate against the captured root context. No
    /// environment is re-read and no workspace or old-root boundary is lost.
    pub fn with_state_dir(
        &self,
        candidate: PathBuf,
        resolver: &impl PathResolver,
    ) -> Result<Self, RootError> {
        let mut inputs = self.inputs.clone();
        inputs.state_dir = Some(candidate);
        inputs.resolve(resolver)
    }
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
        let protected = protected_roots(inputs);
        for root in &protected {
            absolute_normal(&root.path)?;
        }
        let mut warnings = classify(&candidate, &protected)?;
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
        let bound_protected = protected
            .into_iter()
            .map(|root| {
                Ok(ProtectedRoot {
                    path: resolve(&root.path)?,
                    ..root
                })
            })
            .collect::<Result<Vec<_>, RootError>>()?;
        let bound = resolve(&candidate)?;
        for warning in classify(&bound, &bound_protected)? {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
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
/// Hard holds are restricted to actual credentials, Git metadata and other
/// stores. An ordinary directory carries no boundary merely by being a parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootBoundary {
    Credentials,
    GitIsolation,
    DataIntegrity,
}

#[derive(Debug, Clone)]
struct ProtectedRoot {
    path: PathBuf,
    boundary: Option<RootBoundary>,
    source: &'static str,
}

/// The sole declaration of protected locations and the boundary each owns.
fn protected_roots(inputs: &RootInputs) -> Vec<ProtectedRoot> {
    use RootBoundary::{Credentials, DataIntegrity, GitIsolation};
    let mut roots = vec![
        ProtectedRoot {
            path: inputs.checkout.clone(),
            boundary: None,
            source: "root inputs: checkout",
        },
        ProtectedRoot {
            path: inputs.home.join(".agents"),
            boundary: Some(Credentials),
            source: "built-in credential root",
        },
        ProtectedRoot {
            path: inputs.home.join(".local/state/agent-harness"),
            boundary: None,
            source: "built-in retired agent-harness location",
        },
        ProtectedRoot {
            path: inputs.home.join(".local/state/provenant"),
            boundary: Some(DataIntegrity),
            source: "built-in previous state root",
        },
        ProtectedRoot {
            path: inputs.home.join(LEGACY_V1_STATE_SUFFIX),
            boundary: Some(DataIntegrity),
            source: "built-in 1.x state root",
        },
    ];
    roots.extend(inputs.workspaces.iter().map(|path| ProtectedRoot {
        path: path.clone(),
        boundary: None,
        source: "root inputs: workspace",
    }));
    roots.extend(inputs.git_metadata.iter().map(|path| ProtectedRoot {
        path: path.clone(),
        boundary: Some(GitIsolation),
        source: "root inputs: existing Git metadata",
    }));
    roots.extend(inputs.old_roots.iter().map(|path| ProtectedRoot {
        path: path.clone(),
        boundary: Some(DataIntegrity),
        source: "root inputs: old state root",
    }));
    roots
}

/// One decision for both lexical and bound paths. Containing a boundary is as
/// unsafe as being inside it; advisory overlaps always warn and proceed.
fn classify(candidate: &Path, protected: &[ProtectedRoot]) -> Result<Vec<RootWarning>, RootError> {
    let mut warnings = Vec::new();
    for root in protected {
        if !candidate.starts_with(&root.path) && !root.path.starts_with(candidate) {
            continue;
        }
        if let Some(boundary) = root.boundary {
            return Err(RootError::Protected {
                path: candidate.into(),
                protected: root.path.clone(),
                boundary,
                policy_source: root.source,
            });
        }
        warnings.push(RootWarning {
            path: candidate.into(),
            protected: root.path.clone(),
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
        let mut inputs = RootInputs::new("/fixture/home".into(), "/fixture/checkout".into());
        inputs.git_metadata.push(inputs.checkout.join(".git"));
        inputs
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
    fn protected_locations_enforce_only_their_declared_boundaries() -> Result<(), RootError> {
        let mut input = inputs();
        input.workspaces.push("/selected/workspace".into());
        input.old_roots.push("/explicit/old".into());
        let declared = protected_roots(&input);
        for root in &declared {
            let mut candidates = vec![root.path.clone(), root.path.join("nested")];
            if let Some(parent) = root.path.parent() {
                candidates.push(parent.into());
            }
            for candidate in candidates {
                input.state_dir = Some(candidate.clone());
                let overlaps = |root: &&ProtectedRoot| {
                    candidate.starts_with(&root.path) || root.path.starts_with(&candidate)
                };
                let boundary = declared
                    .iter()
                    .filter(overlaps)
                    .any(|root| root.boundary.is_some());
                match input.resolve(&Resolver) {
                    Err(RootError::Protected {
                        protected,
                        boundary: actual,
                        policy_source,
                        ..
                    }) => {
                        assert!(boundary, "{candidate:?}");
                        assert!(
                            declared
                                .iter()
                                .filter(overlaps)
                                .any(|root| root.path == protected
                                    && root.boundary == Some(actual)
                                    && root.source == policy_source)
                        );
                    }
                    Ok(roots) => {
                        assert!(!boundary, "{candidate:?}");
                        for root in declared.iter().filter(overlaps) {
                            assert!(
                                roots
                                    .state
                                    .warnings()
                                    .iter()
                                    .any(|warning| warning.protected == root.path
                                        && warning.source == root.source)
                            );
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        // A filename alone establishes no live-state boundary. Siblings of
        // declared roots also remain available for fresh stores and stages.
        for allowed in ["/any/.agent-run/tail", "/explicit/old-sibling"] {
            input.state_dir = Some(allowed.into());
            assert!(input.resolve(&Resolver).is_ok());
        }
        Ok(())
    }

    #[test]
    fn changing_the_candidate_preserves_all_captured_boundaries() -> Result<(), RootError> {
        let mut input = inputs();
        input.workspaces.push("/selected/workspace".into());
        input.old_roots.push("/explicit/old".into());
        let roots = input.resolve(&Resolver)?;
        for root in protected_roots(&input)
            .into_iter()
            .filter(|root| root.boundary.is_some())
        {
            assert!(
                matches!(roots.with_state_dir(root.path.join("nested"), &Resolver), Err(RootError::Protected { boundary, .. }) if Some(boundary) == root.boundary)
            );
        }
        let stage = input
            .home
            .join(".local/share/provenant/.v2.handoff-fixture");
        assert_eq!(
            roots
                .with_state_dir(stage.clone(), &Resolver)?
                .state
                .as_path(),
            stage
        );
        Ok(())
    }

    #[test]
    fn canonical_aliases_are_checked_after_binding_and_failures_are_not_absence()
    -> Result<(), RootError> {
        struct Aliases(PathBuf);
        impl PathResolver for Aliases {
            fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
                if let Ok(tail) = path.strip_prefix("/alias") {
                    return Ok(self.0.join(tail));
                }
                if path == Path::new("/unreadable") {
                    return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                }
                Ok(path.into())
            }
        }
        let mut input = inputs();
        input.workspaces.push("/selected/workspace".into());
        input.old_roots.push("/explicit/old".into());
        for root in protected_roots(&input) {
            input.state_dir = Some("/alias/missing/tail".into());
            match input.resolve(&Aliases(root.path.clone())) {
                Err(RootError::Protected { boundary, .. }) => {
                    assert_eq!(Some(boundary), root.boundary)
                }
                Ok(roots) => {
                    assert!(root.boundary.is_none());
                    assert!(
                        roots
                            .state
                            .warnings()
                            .iter()
                            .any(|warning| warning.protected == root.path
                                && warning.source == root.source)
                    );
                }
                Err(error) => return Err(error),
            }
        }
        input.state_dir = Some("/unreadable".into());
        assert!(matches!(
            input.resolve(&Aliases("/unused".into())),
            Err(RootError::Resolution { .. })
        ));
        Ok(())
    }
}
