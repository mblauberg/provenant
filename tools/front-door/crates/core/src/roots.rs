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
        let normal_state = inputs.home.join(DEFAULT_STATE_SUFFIX);
        let mut warnings = classify(&candidate, &protected, &inputs.home, Some(&normal_state))?;
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
        let bound_home = resolve(&inputs.home).unwrap_or_else(|_| inputs.home.clone());
        let default = resolve(&normal_state).ok();
        for warning in classify(&bound, &bound_protected, &bound_home, default.as_deref())? {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        // The normal home-relative store remains normal even when the caller's
        // working directory is HOME or an ancestor (as in scheduled ticks).
        // Other advisory overlaps, including project configuration, still warn.
        if default.as_ref() == Some(&bound) {
            let home = resolve(&inputs.home).unwrap_or_else(|_| inputs.home.clone());
            warnings.retain(|warning| {
                !((warning.source == "root inputs: workspace"
                    || warning.source == "root inputs: checkout")
                    && (home.starts_with(&warning.protected)
                        || inputs.home.starts_with(&warning.protected)))
            });
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
    lane_roots: Vec<PathBuf>,
}

impl CredentialRoots {
    /// Bind the actual launch roots once. Only strict descendants of a store
    /// are carve-outs; equality and stores inside lane roots remain refused.
    pub fn with_lane_roots(mut self, lane_roots: Vec<PathBuf>) -> Self {
        self.lane_roots = lane_roots;
        self
    }

    pub fn lane_roots(&self) -> &[PathBuf] {
        &self.lane_roots
    }

    pub fn lane_exceptions<'a>(
        &'a self,
        root: &'a CredentialRoot,
    ) -> impl Iterator<Item = &'a PathBuf> {
        self.lane_roots.iter().filter(move |lane| {
            root.scope == CredentialScope::Subtree
                && **lane != root.path
                && lane.starts_with(&root.path)
        })
    }

    /// Sandbox read grants retain independent exact endpoint denials.
    pub fn permits_read_carve_out(
        &self,
        root: &CredentialRoot,
        path: &Path,
        scope: CredentialScope,
    ) -> bool {
        self.carve_out(root, path, scope, true)
    }

    fn carve_out(
        &self,
        root: &CredentialRoot,
        path: &Path,
        scope: CredentialScope,
        endpoint_denials: bool,
    ) -> bool {
        self.lane_exceptions(root).next().is_some()
            && path != root.path
            && path.starts_with(&root.path)
            && !self.entries.iter().any(|endpoint| {
                endpoint.path == path
                    || (scope == CredentialScope::Subtree
                        && (!endpoint_denials || endpoint.scope == CredentialScope::Subtree)
                        && endpoint.path.starts_with(path))
            })
    }

    pub fn for_home(home: &Path) -> Self {
        let mut roots = Self::default();
        for declaration in crate::CredentialDeclaration::for_home(home) {
            roots.add_entry(declaration.root);
        }
        roots
    }

    /// Keep every declaration. Only an exact repeat of path, scope and kind
    /// is dropped, so a store declared at a Codex home's path keeps its own
    /// protection and the package exception cannot absorb it.
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
            .any(|entry| entry.path == path && entry.scope == scope && entry.kind == kind)
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

    /// Add each declaration's resolved spelling, then give every spelling of
    /// a declared location each declaration at or beneath that location. A
    /// store declared through an alias of a Codex home therefore protects
    /// both spellings, and no spelling keeps only the weaker package exception.
    pub fn with_aliases<E>(
        mut self,
        mut resolve: impl FnMut(&Path) -> Result<PathBuf, E>,
    ) -> Result<Self, E> {
        let declarations = self.entries.clone();
        let resolved = declarations
            .iter()
            .map(|root| resolve(&root.path))
            .collect::<Result<Vec<_>, E>>()?;
        for (root, location) in declarations.iter().zip(&resolved) {
            self.add(
                location.clone(),
                root.scope,
                root.source.clone(),
                root.kind.clone(),
            );
        }
        for (spelling, location) in declarations.iter().map(|root| &root.path).zip(&resolved) {
            if spelling == location {
                continue;
            }
            for (root, root_location) in declarations.iter().zip(&resolved) {
                let Ok(rest) = root_location.strip_prefix(location) else {
                    continue;
                };
                let path = if rest.as_os_str().is_empty() {
                    spelling.clone()
                } else {
                    spelling.join(rest)
                };
                self.add(path, root.scope, root.source.clone(), root.kind.clone());
            }
        }
        Ok(self)
    }

    pub fn entries(&self) -> &[CredentialRoot] {
        &self.entries
    }

    /// Any overlap with a declared store is refused, including a grant which
    /// encloses that store. Exact entries protect the file and enclosing
    /// grants; subtree entries also protect descendants. This strict check also
    /// applies to materialised inputs, which cannot inherit sandbox denials.
    pub fn refuses(&self, path: &Path, scope: CredentialScope, read: bool) -> bool {
        self.refusal(path, scope, read).is_some()
    }

    pub fn refusal(
        &self,
        path: &Path,
        scope: CredentialScope,
        read: bool,
    ) -> Option<&CredentialRoot> {
        self.refusal_impl(path, scope, read, false)
    }

    /// Admission for sandbox read/exec grants only. Exact endpoints enclosed
    /// by the grant remain protected by independently rendered denials. Never
    /// use this predicate for copying inputs or admitting lane write authority.
    pub fn read_grant_refusal(
        &self,
        path: &Path,
        scope: CredentialScope,
    ) -> Option<&CredentialRoot> {
        self.refusal_impl(path, scope, true, true)
    }

    fn refusal_impl(
        &self,
        path: &Path,
        scope: CredentialScope,
        read: bool,
        endpoint_denials: bool,
    ) -> Option<&CredentialRoot> {
        // Report the enclosed endpoint's origin rather than the enclosing
        // shared store when a requested carve-out would expose that endpoint.
        if let Some(endpoint) = self.entries.iter().find(|root| {
            root.path == path
                || (scope == CredentialScope::Subtree
                    && (!endpoint_denials || root.scope == CredentialScope::Subtree)
                    && root.path.starts_with(path))
        }) {
            return Some(endpoint);
        }
        self.entries.iter().find(|root| {
            if (read
                || self
                    .lane_exceptions(root)
                    .any(|lane| path.starts_with(lane)))
                && self.carve_out(root, path, scope, endpoint_denials)
            {
                return false;
            }
            if read
                && root.kind == CredentialKind::CodexHome
                && codex_standalone_path(path, &root.path)
            {
                return false;
            }
            root.path == path
                || (scope == CredentialScope::Subtree
                    && (!endpoint_denials || root.scope == CredentialScope::Subtree)
                    && root.path.starts_with(path))
                || (root.scope == CredentialScope::Subtree && path.starts_with(&root.path))
        })
    }

    /// A read of a standalone Codex release package beneath a declared Codex
    /// home, which no other declaration protects. The exception is relative
    /// to the Codex home alone; any overlapping store keeps the path refused.
    pub fn is_codex_standalone_package(&self, path: &Path) -> bool {
        self.entries.iter().any(|root| {
            root.kind == CredentialKind::CodexHome && codex_standalone_path(path, &root.path)
        }) && self
            .read_grant_refusal(path, CredentialScope::Subtree)
            .is_none()
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
    credential_directory: bool,
    source: String,
}

/// The sole declaration of protected locations and the boundary each owns.
fn protected_roots(inputs: &RootInputs) -> Vec<ProtectedRoot> {
    use RootBoundary::{Credentials, DataIntegrity, GitIsolation};
    let mut roots = vec![
        ProtectedRoot {
            path: inputs.checkout.clone(),
            boundary: None,
            credential_directory: false,
            source: "root inputs: checkout".into(),
        },
        ProtectedRoot {
            path: inputs.home.join(".local/state/agent-harness"),
            boundary: None,
            credential_directory: false,
            source: "built-in retired agent-harness location".into(),
        },
        ProtectedRoot {
            path: inputs.home.join(".local/state/provenant"),
            boundary: Some(DataIntegrity),
            credential_directory: false,
            source: "built-in previous state root".into(),
        },
        ProtectedRoot {
            path: inputs.home.join(LEGACY_V1_STATE_SUFFIX),
            boundary: Some(DataIntegrity),
            credential_directory: false,
            source: "built-in 1.x state root".into(),
        },
    ];
    roots.extend(
        PROJECT_AGENT_CONFIG_DIRECTORIES
            .iter()
            .map(|name| ProtectedRoot {
                path: inputs.checkout.join(name),
                boundary: None,
                credential_directory: false,
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
                credential_directory: entry.scope == CredentialScope::Subtree,
                source: entry.source.clone(),
            }),
    );
    roots.extend(inputs.workspaces.iter().map(|path| ProtectedRoot {
        path: path.clone(),
        boundary: None,
        credential_directory: false,
        source: "root inputs: workspace".into(),
    }));
    roots.extend(inputs.git_metadata.iter().map(|path| ProtectedRoot {
        path: path.clone(),
        boundary: Some(GitIsolation),
        credential_directory: false,
        source: "root inputs: existing Git metadata".into(),
    }));
    roots.extend(inputs.old_roots.iter().map(|path| ProtectedRoot {
        path: path.clone(),
        boundary: Some(DataIntegrity),
        credential_directory: false,
        source: "root inputs: old state root".into(),
    }));
    roots
}

/// One decision for both lexical and bound paths. Containing a boundary is as
/// unsafe as being inside it for data integrity. Lane checkouts, provider
/// homes and installed releases live under the state root, so containing Git
/// metadata, a credential store or an advisory location is expected; only a
/// state root inside one is refused, and a state root inside an advisory
/// location warns and proceeds.
fn classify(
    candidate: &Path,
    protected: &[ProtectedRoot],
    home: &Path,
    normal_state: Option<&Path>,
) -> Result<Vec<RootWarning>, RootError> {
    let mut warnings = Vec::new();
    for root in protected {
        // A home-rooted account must not displace Provenant's own normal
        // state tree. This is host state admission, not a launch carve-out;
        // raw declarations stay intact and every other store still refuses.
        if root.credential_directory
            && root.path == home
            && normal_state.is_some_and(|state| candidate.starts_with(state))
        {
            continue;
        }
        let inside = candidate.starts_with(&root.path);
        if !inside && !root.path.starts_with(candidate) {
            continue;
        }
        if !inside && contained_is_expected(root.boundary) {
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

fn contained_is_expected(boundary: Option<RootBoundary>) -> bool {
    matches!(
        boundary,
        None | Some(RootBoundary::GitIsolation | RootBoundary::Credentials)
    )
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
    // Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
    #[test]
    fn raw_host_stores_protect_state_without_classifying_a_launch() -> Result<(), RootError> {
        let mut input = inputs();
        input.credential_roots.add(
            "/private/codex",
            CredentialScope::Subtree,
            "provider environment: CODEX_HOME",
            CredentialKind::Store,
        );
        input.state_dir = Some("/private/codex/provenant-state".into());
        assert!(
            matches!(input.resolve(&Resolver), Err(RootError::Protected { policy_source, .. }) if policy_source.contains("CODEX_HOME"))
        );
        input.credential_roots.add(
            input.home.clone(),
            CredentialScope::Subtree,
            "home-rooted account",
            CredentialKind::Store,
        );
        input.state_dir = None;
        let roots = input.resolve(&Resolver)?;
        assert!(
            roots
                .credential_roots()
                .entries()
                .iter()
                .any(|root| root.path == input.home && root.scope == CredentialScope::Subtree)
        );
        input.state_dir = Some(input.home.join("auth.json"));
        assert!(matches!(
            input.resolve(&Resolver),
            Err(RootError::Protected { .. })
        ));
        input.state_dir = None;
        input.credential_roots.add(
            input
                .home
                .join(DEFAULT_STATE_SUFFIX)
                .join("credential-file"),
            CredentialScope::Exact,
            "explicit credential",
            CredentialKind::Store,
        );
        input.state_dir = Some(
            input
                .home
                .join(DEFAULT_STATE_SUFFIX)
                .join("credential-file"),
        );
        assert!(matches!(
            input.resolve(&Resolver),
            Err(RootError::Protected { .. })
        ));
        Ok(())
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
                // Containing Git metadata or credentials is expected (lane
                // checkouts and provider homes live under the state root).
                let overlaps = |root: &&ProtectedRoot| {
                    candidate.starts_with(&root.path)
                        || (root.path.starts_with(&candidate)
                            && !contained_is_expected(root.boundary))
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
    // Spec: docs/specs/consumer-contract.md#state-root
    fn default_root_ignores_home_ancestry_but_odd_root_warns() -> Result<(), RootError> {
        let mut input = RootInputs::new("/fixture/home".into(), "/fixture/home".into());
        input.workspaces.push("/".into());
        input.environment_state_dir = Some(input.home.join(DEFAULT_STATE_SUFFIX));
        assert!(input.resolve(&Resolver)?.state.warnings().is_empty());
        input.environment_state_dir = Some(input.home.join("unusual-state"));
        assert!(!input.resolve(&Resolver)?.state.warnings().is_empty());
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
    fn installed_release_assets_under_the_state_root_do_not_warn() -> Result<(), RootError> {
        let state = Path::new("/fixture/home").join(DEFAULT_STATE_SUFFIX);
        let input = RootInputs::new(
            "/fixture/home".into(),
            state.join("releases/sha256:fixture"),
        );
        assert!(input.resolve(&Resolver)?.state.warnings().is_empty());
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

    /// The package exception never absorbs another declaration, whichever
    /// order declarations arrive in and whichever spelling names them.
    #[test]
    fn codex_package_exception_applies_only_where_no_other_declaration_protects() {
        use CredentialKind::{CodexHome, Store};
        use CredentialScope::{Exact, Subtree};
        let home = Path::new("/fixture/real/.codex");
        let alias = Path::new("/fixture/home/.codex");
        let release = |base: &Path| base.join("packages/standalone/releases/1.0");
        // An alias spelling resolves to the real Codex home; every other path is real.
        let resolve = |path: &Path| -> Result<PathBuf, ()> {
            Ok(path
                .strip_prefix(alias)
                .map_or_else(|_| path.to_owned(), |rest| home.join(rest)))
        };
        type Declaration = (&'static str, CredentialScope, CredentialKind);
        let cases: &[(&str, &[Declaration], bool)] = &[
            (
                "codex home alone",
                &[("/fixture/home/.codex", Subtree, CodexHome)],
                true,
            ),
            (
                "store after codex home at the same path",
                &[
                    ("/fixture/home/.codex", Subtree, CodexHome),
                    ("/fixture/home/.codex", Subtree, Store),
                ],
                false,
            ),
            (
                "store before codex home at the same path",
                &[
                    ("/fixture/home/.codex", Subtree, Store),
                    ("/fixture/home/.codex", Subtree, CodexHome),
                ],
                false,
            ),
            (
                "store at the resolved spelling of the codex home",
                &[
                    ("/fixture/home/.codex", Subtree, CodexHome),
                    ("/fixture/real/.codex", Subtree, Store),
                ],
                false,
            ),
            (
                "store at an alias of the codex home",
                &[
                    ("/fixture/real/.codex", Subtree, CodexHome),
                    ("/fixture/home/.codex", Subtree, Store),
                ],
                false,
            ),
            (
                "exact endpoint inside the release keeps its independent denial",
                &[
                    ("/fixture/home/.codex", Subtree, CodexHome),
                    (
                        "/fixture/real/.codex/packages/standalone/releases/1.0/auth.json",
                        Exact,
                        Store,
                    ),
                ],
                true,
            ),
            (
                "unrelated store",
                &[
                    ("/fixture/home/.codex", Subtree, CodexHome),
                    ("/fixture/home/.codex-work", Subtree, Store),
                ],
                true,
            ),
        ];
        for (name, declarations, exempt) in cases {
            let mut roots = CredentialRoots::default();
            for (path, scope, kind) in *declarations {
                roots.add(*path, *scope, "fixture", kind.clone());
            }
            let roots = roots.with_aliases(resolve).unwrap();
            for spelling in [alias, home] {
                let package = release(spelling);
                assert_eq!(
                    roots.read_grant_refusal(&package, Subtree).is_none(),
                    *exempt,
                    "{name}: read {}",
                    package.display()
                );
                assert_eq!(
                    roots.is_codex_standalone_package(&package),
                    *exempt,
                    "{name}"
                );
                // Every endpoint itself remains refused, even when its parent
                // package can be read with the endpoint independently denied.
                for endpoint in roots.entries().iter().filter(|root| root.scope == Exact) {
                    assert!(
                        roots.refuses(&endpoint.path, Exact, true),
                        "{name}: endpoint"
                    );
                }
                // The exception never confers writes or reaches the home itself.
                assert!(roots.refuses(&package, Subtree, false), "{name}: write");
                assert!(roots.refuses(spelling, Subtree, true), "{name}: home");
            }
        }
    }
}
