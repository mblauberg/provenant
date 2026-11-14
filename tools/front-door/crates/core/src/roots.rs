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
pub const CARGO_CREDENTIAL_FILES: &[&str] = &["credentials", "credentials.toml"];
pub const CODEX_ACCOUNT_DIRECTORY: (&str, &str) = ("CODEX_HOME", ".codex");
pub const PROJECT_AGENT_CONFIG_DIRECTORIES: &[&str] = &[".claude", ".codex", ".cursor", ".gemini"];

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
    /// Credential stores declared by the host/provider/account registry.
    pub credential_roots: CredentialRoots,
}
impl RootInputs {
    pub fn new(home: PathBuf, checkout: PathBuf) -> Self {
        let credential_roots = CredentialRoots::for_home(&home);
        Self {
            state_dir: None,
            environment_state_dir: None,
            home,
            checkout,
            workspaces: Vec::new(),
            git_metadata: Vec::new(),
            old_roots: Vec::new(),
            credential_roots,
        }
    }
    /// Resolve captured inputs without filesystem discovery. Production OS
    /// binding supplies existing Git metadata before entering this pure core.
    pub fn resolve(&self, resolver: &impl PathResolver) -> Result<Roots, RootError> {
        let state = StateRoot::resolve(self, resolver)?;
        let mut credential_roots = CredentialRoots::default();
        for root in self.credential_roots.entries() {
            let path = bind_declared(resolver, &root.path)?;
            credential_roots.add(path, root.scope, root.source.clone(), root.kind.clone());
        }
        Ok(Roots {
            state,
            credential_roots,
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
        policy_source: String,
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
    pub source: String,
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
    credential_roots: CredentialRoots,
    inputs: RootInputs,
}
impl Roots {
    pub fn credential_roots(&self) -> &CredentialRoots {
        &self.credential_roots
    }

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
                let path = bind_declared(resolver, &root.path)?;
                absolute_normal(&path)?;
                Ok(ProtectedRoot { path, ..root })
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

/// Bind a declared protected location. A file where a directory is expected
/// means nothing can exist below it, so the declared literal stays protected;
/// every other resolution failure is still an error.
fn bind_declared(resolver: &impl PathResolver, path: &Path) -> Result<PathBuf, RootError> {
    match resolver.resolve_existing_ancestor(path) {
        Ok(bound) => Ok(bound),
        Err(error) if error.kind() == io::ErrorKind::NotADirectory => Ok(path.to_owned()),
        Err(source) => Err(RootError::Resolution {
            path: path.into(),
            source,
        }),
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

/// The reach of one declared credential store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialScope {
    Subtree,
    Exact,
}

/// How a credential path entered the host's declared table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    Store,
    /// The default Codex home is the only store with a read-only standalone
    /// package exception.
    CodexHome,
}

/// One concrete, sourced credential path. Names elsewhere in a path have no
/// credential meaning.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CredentialRoot {
    pub path: PathBuf,
    pub scope: CredentialScope,
    pub source: String,
    pub kind: CredentialKind,
}

/// The single declared set of credential stores used by root admission and
/// confinement. Host declarations extend this table with provider and account
/// paths after resolving their configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CredentialRoots {
    entries: Vec<CredentialRoot>,
}

impl CredentialRoots {
    pub fn for_home(home: &Path) -> Self {
        use CredentialKind::{CodexHome, Store};
        use CredentialScope::{Exact, Subtree};

        let mut roots = Self::default();
        for name in [".ssh", ".aws", ".azure", ".gnupg", ".kube", ".secrets"] {
            roots.add(home.join(name), Subtree, "built-in secret store", Store);
        }
        roots.add(
            home.join(".claude"),
            Subtree,
            "provider declaration: claude default home",
            Store,
        );
        roots.add(
            home.join(".claude.json"),
            Exact,
            "provider declaration: claude credentials file",
            Store,
        );
        roots.add(
            home.join(CODEX_ACCOUNT_DIRECTORY.1),
            Subtree,
            "provider declaration: codex default home",
            CodexHome,
        );
        roots.add(
            home.join(".cursor"),
            Subtree,
            "provider declaration: cursor default home",
            Store,
        );
        roots.add(
            home.join(".gemini"),
            Subtree,
            "provider declaration: gemini default home",
            Store,
        );
        roots.add(
            home.join(".agents"),
            Subtree,
            "built-in agent credential home",
            Store,
        );
        for (relative, source) in [
            (".config/cursor", "provider declaration: cursor config"),
            (".config/opencode", "provider declaration: opencode config"),
            (
                ".local/share/opencode",
                "provider declaration: opencode data",
            ),
            (".config/gh", "built-in GitHub CLI credentials"),
            (".config/gcloud", "built-in Google Cloud credentials"),
            ("Library/Keychains", "built-in platform keychain"),
            (
                "Library/Application Support/GitHub CLI",
                "built-in GitHub CLI support files",
            ),
            (
                "Archives/git-preservation",
                "built-in Git preservation archive",
            ),
            ("Monero", "built-in Monero credentials"),
        ] {
            roots.add(home.join(relative), Subtree, source, Store);
        }
        for &name in CARGO_CREDENTIAL_FILES {
            roots.add(
                home.join(".cargo").join(name),
                Exact,
                "built-in Cargo credential file",
                Store,
            );
        }
        roots
    }

    pub fn add(
        &mut self,
        path: impl Into<PathBuf>,
        scope: CredentialScope,
        source: impl Into<String>,
        kind: CredentialKind,
    ) {
        let path = path.into();
        if self
            .entries
            .iter()
            .any(|entry| entry.path == path && entry.scope == scope)
        {
            return;
        }
        self.entries.push(CredentialRoot {
            path,
            scope,
            source: source.into(),
            kind,
        });
    }

    pub fn add_entry(&mut self, root: CredentialRoot) {
        self.add(root.path, root.scope, root.source, root.kind);
    }

    pub fn entries(&self) -> &[CredentialRoot] {
        &self.entries
    }

    /// Any overlap with a declared store is refused, including a grant which
    /// encloses that store. Exact entries protect the file and its enclosing
    /// grants; subtree entries also protect descendants.
    pub fn refuses(&self, path: &Path, scope: CredentialScope, read: bool) -> bool {
        self.refusal(path, scope, read).is_some()
    }

    pub fn refusal(
        &self,
        path: &Path,
        scope: CredentialScope,
        read: bool,
    ) -> Option<&CredentialRoot> {
        self.entries.iter().find(|root| {
            if read
                && root.kind == CredentialKind::CodexHome
                && codex_standalone_path(path, &root.path)
            {
                return false;
            }
            root.path == path
                || (scope == CredentialScope::Subtree && root.path.starts_with(path))
                || (root.scope == CredentialScope::Subtree && path.starts_with(&root.path))
        })
    }

    pub fn is_codex_standalone_package(&self, path: &Path) -> bool {
        self.entries.iter().any(|root| {
            root.kind == CredentialKind::CodexHome && codex_standalone_path(path, &root.path)
        })
    }

    pub fn overlaps(&self, path: &Path) -> bool {
        self.refuses(path, CredentialScope::Subtree, false)
    }
}

fn codex_standalone_path(path: &Path, codex_home: &Path) -> bool {
    let Some(relative) = path.strip_prefix(codex_home).ok() else {
        return false;
    };
    let components: Vec<_> = relative.components().collect();
    components.len() >= 4
        && components[0].as_os_str() == "packages"
        && components[1].as_os_str() == "standalone"
        && components[2].as_os_str() == "releases"
        && !components[3].as_os_str().is_empty()
}

#[derive(Debug, Clone)]
struct ProtectedRoot {
    path: PathBuf,
    boundary: Option<RootBoundary>,
    source: String,
}

/// The sole declaration of protected locations and the boundary each owns.
fn protected_roots(inputs: &RootInputs) -> Vec<ProtectedRoot> {
    use RootBoundary::{Credentials, DataIntegrity, GitIsolation};
    let mut roots = vec![
        ProtectedRoot {
            path: inputs.checkout.clone(),
            boundary: None,
            source: "root inputs: checkout".into(),
        },
        ProtectedRoot {
            path: inputs.home.join(".local/state/agent-harness"),
            boundary: None,
            source: "built-in retired agent-harness location".into(),
        },
        ProtectedRoot {
            path: inputs.home.join(".local/state/provenant"),
            boundary: Some(DataIntegrity),
            source: "built-in previous state root".into(),
        },
        ProtectedRoot {
            path: inputs.home.join(LEGACY_V1_STATE_SUFFIX),
            boundary: Some(DataIntegrity),
            source: "built-in 1.x state root".into(),
        },
    ];
    roots.extend(
        PROJECT_AGENT_CONFIG_DIRECTORIES
            .iter()
            .map(|name| ProtectedRoot {
                path: inputs.checkout.join(name),
                boundary: None,
                source: format!("project-local agent configuration: {name}"),
            }),
    );
    roots.extend(
        inputs
            .credential_roots
            .entries()
            .iter()
            .map(|entry| ProtectedRoot {
                path: entry.path.clone(),
                boundary: Some(Credentials),
                source: entry.source.clone(),
            }),
    );
    roots.extend(inputs.workspaces.iter().map(|path| ProtectedRoot {
        path: path.clone(),
        boundary: None,
        source: "root inputs: workspace".into(),
    }));
    roots.extend(inputs.git_metadata.iter().map(|path| ProtectedRoot {
        path: path.clone(),
        boundary: Some(GitIsolation),
        source: "root inputs: existing Git metadata".into(),
    }));
    roots.extend(inputs.old_roots.iter().map(|path| ProtectedRoot {
        path: path.clone(),
        boundary: Some(DataIntegrity),
        source: "root inputs: old state root".into(),
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
                policy_source: root.source.clone(),
            });
        }
        warnings.push(RootWarning {
            path: candidate.into(),
            protected: root.path.clone(),
            source: root.source.clone(),
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
    fn project_local_agent_configuration_warns_with_its_source() -> Result<(), RootError> {
        let mut input = RootInputs::new("/fixture/home".into(), "/fixture/project".into());
        input.state_dir = Some("/fixture/project/.claude/worktrees/lane/state".into());
        let roots = input.resolve(&Resolver)?;
        assert!(roots.state.warnings().iter().any(|warning| {
            warning.protected == Path::new("/fixture/project/.claude")
                && warning.source == "project-local agent configuration: .claude"
        }));
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
