//! Launch policy contract, independent of the enforcing backend (ADR 0040).
//!
//! Host settings can add denials only. Mandatory roots come from the host's
//! verified launch context and have no deserialisable override surface.
use super::validation::{
    CREDENTIAL_DIRECTORIES, NESTED_CREDENTIAL_DIRECTORIES, PROVIDER_CREDENTIAL_DIRECTORIES,
    profile_path_representable, refuses_lane_root, validate_absolute,
};
use super::{NetworkAccess, PathGrant, PathScope, PermissionError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

/// Tolerant envelope for optional host policy. Decode entries at resolution,
/// so a wrong type cannot discard a valid sibling or turn an optional setting
/// into a boundary config error. Only resolved types reach the renderer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostPolicySettings(Value);

/// Host-derived mandatory roots, never loaded from optional settings.
/// Paths must be canonical host paths; the compiler must hold/revalidate their
/// anchors before use. This inventory performs no filesystem access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MandatoryRoots {
    credentials: Vec<PathGrant>,
    state: PathGrant,
    source: PathGrant,
    git: Vec<PathGrant>,
    home: PathBuf,
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
            [".cargo/credentials", ".cargo/credentials.toml"].map(|relative| PathGrant {
                path: home.join(relative),
                scope: PathScope::Exact,
            }),
        );
        let roots = Self {
            credentials,
            state: subtree(state),
            source: subtree(source),
            git: Vec::new(),
            home: home.to_owned(),
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

    /// Add both the verified common Git dir and linked worktree Git dir.
    /// The writable lane-private Git dir is a separate host-owned exception.
    pub fn add_git(&mut self, root: PathGrant) -> Result<(), PermissionError> {
        root.validate()?;
        self.git.push(root);
        Ok(())
    }

    pub fn credentials(&self) -> &[PathGrant] {
        &self.credentials
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
    pub fn optional(&self) -> &OptionalPolicy {
        &self.optional
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
