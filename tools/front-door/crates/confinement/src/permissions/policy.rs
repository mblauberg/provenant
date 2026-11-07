//! Launch policy contract, independent of the enforcing backend (ADR 0040).
//!
//! Host settings can add denials only. Mandatory roots come from the host's
//! verified launch context and have no deserialisable override surface.
mod host;
use super::validation::{
    CREDENTIAL_DIRECTORIES, NESTED_CREDENTIAL_DIRECTORIES, PROVIDER_CREDENTIAL_DIRECTORIES,
    profile_path_representable, refuses_lane_root, validate_absolute,
};
use super::{NetworkAccess, PathGrant, PathScope, PermissionError};
pub use host::{host_credential_roots, host_github_roots};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

/// Resolve a denial without losing a dangling public symlink. A missing
/// literal leaf is safe to deny by name, but a dangling link could acquire an
/// unprotected target after compilation, when an ambient lane creates it.
/// Inspect only metadata; credential contents are never opened.
pub fn resolve_boundary_path(path: &Path) -> Result<PathBuf, PermissionError> {
    validate_absolute(path)?;
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if std::fs::canonicalize(ancestor).is_err() {
                    return Err(PermissionError::InvalidPath(ancestor.to_owned()));
                }
            }
            Ok(_) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.kind() == std::io::ErrorKind::NotADirectory => {}
            Err(_) => return Err(PermissionError::InvalidPath(ancestor.to_owned())),
        }
    }
    for ancestor in path.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(base) => {
                let suffix = path
                    .strip_prefix(ancestor)
                    .map_err(|_| PermissionError::InvalidPath(path.to_owned()))?;
                return Ok(if suffix.as_os_str().is_empty() {
                    base
                } else {
                    base.join(suffix)
                });
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(_) => return Err(PermissionError::InvalidPath(ancestor.to_owned())),
        }
    }
    Err(PermissionError::InvalidPath(path.to_owned()))
}

/// Tolerant envelope for optional host policy. Decode entries at resolution,
/// so a wrong type cannot discard a valid sibling or turn an optional setting
/// into a boundary config error. Only resolved types reach the renderer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostPolicySettings(Value);

/// Host-derived mandatory roots, never loaded from optional settings.
/// Paths must be canonical host paths; the compiler must hold/revalidate their
/// anchors before use. Construction records paths without filesystem access;
/// compilation separately revalidates them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MandatoryRoots {
    credentials: Vec<PathGrant>,
    non_exempt: Vec<PathGrant>,
    state: PathGrant,
    source: PathGrant,
    git: Vec<PathGrant>,
    home: PathBuf,
    protected: Vec<PathGrant>,
    state_reads: Vec<PathGrant>,
    state_exec: Vec<PathGrant>,
    github: Vec<PathGrant>,
    state_aliases: Vec<PathGrant>,
    source_aliases: Vec<PathGrant>,
    credential_endpoints: Vec<(PathBuf, PathBuf)>,
}

impl MandatoryRoots {
    pub fn new(home: &Path, state: &Path, source: &Path) -> Result<Self, PermissionError> {
        validate_absolute(home)?;
        let subtree = |path: &Path| PathGrant {
            path: path.to_owned(),
            scope: PathScope::Subtree,
        };
        let mut credentials: Vec<_> = CREDENTIAL_DIRECTORIES
            .iter()
            .chain(NESTED_CREDENTIAL_DIRECTORIES)
            .chain(PROVIDER_CREDENTIAL_DIRECTORIES)
            .map(|relative| subtree(&home.join(relative)))
            .collect();
        credentials.extend(
            [
                ".cargo/credentials",
                ".cargo/credentials.toml",
                ".claude.json",
            ]
            .map(|relative| PathGrant {
                path: home.join(relative),
                scope: PathScope::Exact,
            }),
        );
        let roots = Self {
            non_exempt: [
                subtree(&home.join("Library/Keychains")),
                subtree(&home.join("Archives/git-preservation")),
                PathGrant {
                    path: home.join(".claude.json"),
                    scope: PathScope::Exact,
                },
            ]
            .to_vec(),
            credentials,
            state: subtree(state),
            source: subtree(source),
            git: Vec::new(),
            home: home.to_owned(),
            protected: Vec::new(),
            state_reads: Vec::new(),
            state_exec: Vec::new(),
            github: [".config/gh", "Library/Application Support/GitHub CLI"]
                .map(|relative| subtree(&home.join(relative)))
                .to_vec(),
            state_aliases: Vec::new(),
            source_aliases: Vec::new(),
            credential_endpoints: Vec::new(),
        };
        for grant in roots
            .credentials
            .iter()
            .chain([&roots.state, &roots.source])
        {
            grant.validate()?;
        }
        Ok(roots)
    }

    /// Add registered identity homes, relocated XDG stores and isolated Agy
    /// homes, including identities not selected by this attempt. Additive only.
    pub fn add_credentials(&mut self, root: PathGrant) -> Result<(), PermissionError> {
        root.validate()?;
        self.credentials.push(root);
        Ok(())
    }

    /// Preserve mandatory category when a public credential spelling is a link.
    pub fn add_credentials_alias(
        &mut self,
        original: &PathGrant,
        alias: PathGrant,
    ) -> Result<(), PermissionError> {
        self.add_credentials(alias.clone())?;
        if self.non_exempt.contains(original) {
            self.non_exempt.push(alias.clone());
        }
        if self.github.contains(original) {
            self.github.push(alias);
        }
        Ok(())
    }

    pub fn non_exempt(&self) -> &[PathGrant] {
        &self.non_exempt
    }

    /// Add both the verified common Git dir and linked worktree Git dir.
    /// The writable lane-private Git dir is a separate host-owned exception.
    pub fn add_git(&mut self, root: PathGrant) -> Result<(), PermissionError> {
        root.validate()?;
        self.git.push(root);
        Ok(())
    }

    /// Other host-owned worktrees and controls remain readable and immutable.
    pub fn add_protected(&mut self, root: PathGrant) -> Result<(), PermissionError> {
        root.validate()?;
        self.protected.push(root);
        Ok(())
    }

    pub fn add_state_alias(&mut self, root: PathGrant) -> Result<(), PermissionError> {
        root.validate()?;
        self.state_aliases.push(root);
        Ok(())
    }

    pub fn add_source_alias(&mut self, root: PathGrant) -> Result<(), PermissionError> {
        root.validate()?;
        self.source_aliases.push(root);
        Ok(())
    }

    /// Only host-prepared prompt, input, skill and immutable release paths.
    pub fn add_state_read(&mut self, root: PathGrant) -> Result<(), PermissionError> {
        root.validate()?;
        if std::iter::once(&self.state)
            .chain(&self.state_aliases)
            .any(|state| state.path == root.path || state.path.starts_with(&root.path))
        {
            return Err(PermissionError::ProtectedOverlap);
        }
        self.state_reads.push(root);
        Ok(())
    }

    /// Verified immutable releases and staged user-skill snapshots may execute.
    pub fn add_state_read_exec(&mut self, root: PathGrant) -> Result<(), PermissionError> {
        self.add_state_read(root.clone())?;
        self.state_exec.push(root);
        Ok(())
    }

    pub fn state_exec(&self) -> &[PathGrant] {
        &self.state_exec
    }

    /// Recheck public spellings and aliases immediately before and after
    /// compilation. Missing leaves are resolved through their existing parent;
    /// a newly introduced symlink target must already be denied in this family.
    #[cfg(target_os = "macos")]
    pub(crate) fn revalidate_paths(&self) -> Result<(), PermissionError> {
        for (path, target) in &self.credential_endpoints {
            if resolve_boundary_path(path).as_ref().ok() != Some(target) {
                return Err(super::anchors::anchor_at(
                    path,
                    "credential endpoint changed",
                ));
            }
        }
        let families: Vec<Vec<&PathGrant>> = vec![
            self.credentials.iter().collect(),
            std::iter::once(&self.state)
                .chain(&self.state_aliases)
                .collect(),
            std::iter::once(&self.source)
                .chain(&self.source_aliases)
                .collect(),
            self.git.iter().collect(),
            self.protected.iter().collect(),
        ];
        for roots in families {
            for root in &roots {
                let resolved = resolve_boundary_path(&root.path).map_err(|_| {
                    super::anchors::anchor_at(&root.path, "mandatory path resolution")
                })?;
                if !roots
                    .iter()
                    .any(|known| known.path == resolved && known.scope == root.scope)
                {
                    return Err(super::anchors::anchor_at(
                        &root.path,
                        "mandatory alias changed",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Host-resolved GitHub CLI configuration; always immutable.
    pub fn add_github(&mut self, root: PathGrant) -> Result<(), PermissionError> {
        self.add_credentials(root.clone())?;
        self.github.push(root);
        Ok(())
    }

    pub fn state_reads(&self) -> &[PathGrant] {
        &self.state_reads
    }
    pub fn github(&self) -> &[PathGrant] {
        &self.github
    }

    pub fn protected(&self) -> &[PathGrant] {
        &self.protected
    }
    pub fn state_aliases(&self) -> &[PathGrant] {
        &self.state_aliases
    }
    pub fn source_aliases(&self) -> &[PathGrant] {
        &self.source_aliases
    }

    pub fn credentials(&self) -> &[PathGrant] {
        &self.credentials
    }
    /// A selected login retains reads of its declared endpoints when
    /// those files resolve outside the store. This confers no refresh writes.
    #[cfg(any(target_os = "macos", feature = "snapshot"))]
    pub(crate) fn credential_alias_reads(&self, reads: &[PathGrant]) -> Vec<PathGrant> {
        self.credential_endpoints
            .iter()
            .filter(|(path, target)| {
                path != target
                    && !std::iter::once(&self.state)
                        .chain(&self.state_aliases)
                        .chain(&self.non_exempt)
                        .any(|root| {
                            root.overlaps(&PathGrant {
                                path: target.clone(),
                                scope: PathScope::Exact,
                            })
                        })
                    && reads.iter().any(|grant| match grant.scope {
                        PathScope::Subtree => path.starts_with(&grant.path),
                        PathScope::Exact => path == &grant.path,
                    })
            })
            .map(|(_, target)| PathGrant {
                path: target.clone(),
                scope: PathScope::Exact,
            })
            .collect()
    }
    pub fn state(&self) -> &PathGrant {
        &self.state
    }
    pub fn source(&self) -> &PathGrant {
        &self.source
    }
    pub fn git(&self) -> &[PathGrant] {
        &self.git
    }

    /// Extra writes cannot overlap credentials, host state, the source tree or
    /// host Git in either direction. Applies to requested extra roots, not
    /// host-derived scratch, private state or writer checkout exceptions.
    pub fn validate_extra_write(&self, grant: &PathGrant) -> Result<(), PermissionError> {
        grant.validate()?;
        if refuses_lane_root(&grant.path, false, Some(&self.home))
            || self.credentials.iter().any(|root| root.overlaps(grant))
        {
            return Err(PermissionError::credential_root_at(
                &grant.path,
                Some(&self.home),
            ));
        }
        if self
            .git
            .iter()
            .chain(&self.protected)
            .chain(&self.state_aliases)
            .chain(&self.source_aliases)
            .chain([&self.state, &self.source])
            .any(|root| root.overlaps(grant))
        {
            return Err(PermissionError::ProtectedOverlap);
        }
        Ok(())
    }
}

/// Additional restrictions only; an absent host policy is ambient/open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionalPolicy {
    pub deny_read: Vec<PathGrant>,
    pub deny_write: Vec<PathGrant>,
    pub network: NetworkAccess,
}

impl Default for OptionalPolicy {
    fn default() -> Self {
        Self {
            deny_read: Vec::new(),
            deny_write: Vec::new(),
            network: NetworkAccess::Open,
        }
    }
}

impl OptionalPolicy {
    /// A host network setting is a ceiling. It never widens explicit lane
    /// network authority, nor bypasses an adapter's offline restriction.
    pub fn network_for(&self, requested: NetworkAccess) -> NetworkAccess {
        match (self.network, requested) {
            (NetworkAccess::Provider, _) | (_, NetworkAccess::Provider) => NetworkAccess::Provider,
            (NetworkAccess::Local, _) | (_, NetworkAccess::Local) => NetworkAccess::Local,
            _ => NetworkAccess::Open,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum PolicySource {
    Ambient,
    Host(PathBuf),
}

/// Stable fields and codes, never rejected values (which may contain secrets).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PolicyWarning {
    pub field: String,
    pub code: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanePolicy {
    mandatory: MandatoryRoots,
    optional: OptionalPolicy,
    source: PolicySource,
    warnings: Vec<PolicyWarning>,
}

impl LanePolicy {
    pub fn mandatory(&self) -> &MandatoryRoots {
        &self.mandatory
    }
    /// Enrichment is host-owned, for verified Git handles acquired at launch.
    pub fn mandatory_mut(&mut self) -> &mut MandatoryRoots {
        &mut self.mandatory
    }
    pub fn optional(&self) -> &OptionalPolicy {
        &self.optional
    }
    /// Add resolved aliases without removing the public spelling. Denials only.
    pub fn resolve_optional_aliases(
        &mut self,
        mut resolve: impl FnMut(&Path) -> Result<PathBuf, PermissionError>,
    ) -> Result<(), PermissionError> {
        for (field, roots) in [
            ("deny_read", &mut self.optional.deny_read),
            ("deny_write", &mut self.optional.deny_write),
        ] {
            let mut retained = Vec::new();
            for root in std::mem::take(roots) {
                let alias = resolve(&root.path).and_then(|path| {
                    validate_absolute(&path)?;
                    if !profile_path_representable(&path) {
                        return Err(PermissionError::InvalidPath(path));
                    }
                    Ok(PathGrant {
                        path,
                        scope: root.scope,
                    })
                });
                match alias {
                    Ok(alias) => {
                        if !retained.contains(&root) {
                            retained.push(root);
                        }
                        if !retained.contains(&alias) {
                            retained.push(alias);
                        }
                    }
                    Err(_) => self.warnings.push(PolicyWarning {
                        field: field.into(),
                        code: "path",
                    }),
                }
            }
            *roots = retained;
        }
        Ok(())
    }

    pub fn source(&self) -> &PolicySource {
        &self.source
    }
    pub fn warnings(&self) -> &[PolicyWarning] {
        &self.warnings
    }

    /// The launch edge calls this once after resolution, before compiling.
    /// Rendering and explain paths inspect data without repeating output.
    pub fn report_launch(&self, output: &mut impl Write) -> std::io::Result<()> {
        match &self.source {
            PolicySource::Ambient => {
                writeln!(output, "lane policy: ambient (optional policy open)")?
            }
            PolicySource::Host(path) => writeln!(
                output,
                "lane policy: host {} [domains.run.host_policy]",
                path.display()
            )?,
        }
        for warning in &self.warnings {
            writeln!(
                output,
                "warning: domains.run.host_policy.{}: {}; omitted",
                warning.field, warning.code
            )?;
        }
        Ok(())
    }
}

impl HostPolicySettings {
    pub(crate) fn resolve(
        settings: Option<&Self>,
        state: &Path,
        mandatory: MandatoryRoots,
    ) -> LanePolicy {
        let mut resolved = LanePolicy {
            mandatory,
            optional: OptionalPolicy::default(),
            source: PolicySource::Ambient,
            warnings: Vec::new(),
        };
        let Some(Self(value)) = settings else {
            return resolved;
        };
        resolved.source = PolicySource::Host(state.join("config.toml"));
        let Some(table) = value.as_object() else {
            resolved.warnings.push(PolicyWarning {
                field: "table".into(),
                code: "type",
            });
            return resolved;
        };
        // Unknown key names can themselves contain private data.
        if table
            .keys()
            .any(|key| !matches!(key.as_str(), "deny_read" | "deny_write" | "network"))
        {
            resolved.warnings.push(PolicyWarning {
                field: "unknown".into(),
                code: "unknown_key",
            });
        }
        for (name, target) in [
            ("deny_read", &mut resolved.optional.deny_read),
            ("deny_write", &mut resolved.optional.deny_write),
        ] {
            let Some(value) = table.get(name) else {
                continue;
            };
            let Some(entries) = value.as_array() else {
                resolved.warnings.push(PolicyWarning {
                    field: name.into(),
                    code: "type",
                });
                continue;
            };
            for (index, entry) in entries.iter().enumerate() {
                // Denials may name broad roots and missing paths. They confer
                // no authority; require only a bounded absolute normalised
                // path the existing SBPL encoder can represent.
                let path = entry.as_str().map(Path::new);
                if let Some(path) = path.filter(|path| {
                    validate_absolute(path).is_ok() && profile_path_representable(path)
                }) {
                    target.push(PathGrant {
                        path: path.to_owned(),
                        scope: PathScope::Subtree,
                    });
                } else {
                    resolved.warnings.push(PolicyWarning {
                        field: format!("{name}.{index}"),
                        code: if path.is_some() { "path" } else { "type" },
                    });
                }
            }
        }
        if let Some(network) = table.get("network") {
            match network.as_str() {
                Some("open") => resolved.optional.network = NetworkAccess::Open,
                Some("local") => resolved.optional.network = NetworkAccess::Local,
                Some("provider") => resolved.optional.network = NetworkAccess::Provider,
                _ => resolved.warnings.push(PolicyWarning {
                    field: "network".into(),
                    code: if network.is_string() { "value" } else { "type" },
                }),
            }
        }
        resolved
    }
}

#[cfg(test)]
mod boundary_resolution_tests {
    use super::*;

    #[test]
    fn non_directory_ancestor_preserves_mandatory_literal() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        std::fs::create_dir(&home)?;
        std::fs::write(home.join(".cargo"), b"file")?;
        // Existing leaves (including symlink targets) must not acquire a
        // trailing slash when the unresolved suffix is empty.
        let credential = base.join("credential");
        std::fs::write(&credential, b"synthetic")?;
        std::os::unix::fs::symlink(&credential, home.join(".claude.json"))?;
        for path in [home.join(".cargo"), home.join(".claude.json")] {
            validate_absolute(&resolve_boundary_path(&path)?)?;
        }
        let source = base.join("source");
        std::fs::create_dir(&source)?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), source.clone());
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let mandatory = MandatoryRoots::for_host(&roots, &home, &source, &[], &[], &[])?;
        for name in [".cargo/credentials", ".cargo/credentials.toml"] {
            let path = home.join(name);
            assert_eq!(resolve_boundary_path(&path)?, path);
            assert!(
                mandatory
                    .credentials()
                    .iter()
                    .any(|g| g.path == path && g.scope == PathScope::Exact)
            );
        }
        Ok(())
    }

    #[test]
    fn optional_alias_failure_warns_and_retains_valid_siblings()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let link = base.join("dangling");
        std::os::unix::fs::symlink(base.join("missing"), &link)?;
        let valid = base.join("valid");
        let settings = HostPolicySettings(
            serde_json::json!({"deny_read": [link.join("secret"), valid], "deny_write": [link.join("write"), valid]}),
        );
        let mandatory = MandatoryRoots::new(
            &base.join("home"),
            &base.join("state"),
            &base.join("source"),
        )?;
        let mut policy = HostPolicySettings::resolve(Some(&settings), &base, mandatory.clone());
        policy.resolve_optional_aliases(resolve_boundary_path)?;
        for roots in [&policy.optional().deny_read, &policy.optional().deny_write] {
            assert_eq!(roots.len(), 1);
            assert_eq!(roots[0].path, valid);
        }
        assert_eq!(policy.mandatory(), &mandatory);
        assert_eq!(
            policy.warnings(),
            &[
                PolicyWarning {
                    field: "deny_read".into(),
                    code: "path"
                },
                PolicyWarning {
                    field: "deny_write".into(),
                    code: "path"
                }
            ]
        );
        Ok(())
    }

    #[test]
    fn typed_state_reads_cannot_open_an_entire_secondary_host_store() -> Result<(), PermissionError>
    {
        let mut roots = MandatoryRoots::new(
            Path::new("/fixture/home"),
            Path::new("/fixture/state"),
            Path::new("/fixture/source"),
        )?;
        roots.add_state_alias(PathGrant {
            path: "/fixture/secondary/state".into(),
            scope: PathScope::Subtree,
        })?;
        for path in [
            "/fixture/state",
            "/fixture/secondary/state",
            "/fixture/secondary",
        ] {
            assert!(matches!(
                roots.add_state_read(PathGrant {
                    path: path.into(),
                    scope: PathScope::Subtree
                }),
                Err(PermissionError::ProtectedOverlap)
            ));
        }
        roots.add_state_read(PathGrant {
            path: "/fixture/secondary/state/prompt.md".into(),
            scope: PathScope::Exact,
        })?;
        Ok(())
    }

    #[test]
    fn boundary_resolution_refuses_dangling_links_but_keeps_absent_roots()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let home = root.join("home");
        std::fs::create_dir(&home)?;
        let credential_target = root.join("absent-credential-target");
        let config_target = root.join("absent-config-target");
        std::os::unix::fs::symlink(&credential_target, home.join(".ssh"))?;
        std::os::unix::fs::symlink(&config_target, home.join(".config"))?;
        for path in [home.join(".ssh"), home.join(".config/opencode")] {
            assert!(matches!(
                resolve_boundary_path(&path),
                Err(PermissionError::InvalidPath(_))
            ));
        }
        assert_eq!(
            resolve_boundary_path(&home.join(".aws"))?,
            home.join(".aws")
        );
        std::fs::create_dir(&credential_target)?;
        std::fs::create_dir(&config_target)?;
        assert_eq!(
            resolve_boundary_path(&home.join(".ssh"))?,
            credential_target
        );
        assert_eq!(
            resolve_boundary_path(&home.join(".config/opencode"))?,
            config_target.join("opencode")
        );
        Ok(())
    }
}
