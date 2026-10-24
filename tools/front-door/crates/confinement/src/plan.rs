pub(crate) mod read_only;
use crate::permissions::{
    AccessMode, Anchors, ConfinedLaunch, ConfinementBackend, PathGrant, PathScope, PermissionError,
    PlanPermissions, ProviderProfile,
};
use provenant_core::{RelativePath, WorkScope};
use provenant_supervisor::home::{HomeError, HomeFiles, HomeRecipe};
pub use read_only::{ReadOnlyGrants, ReadOnlyPolicy};
use std::path::PathBuf;
use thiserror::Error;

pub const MAX_READ_ONLY_WRITABLE: usize = 128;

pub fn valid_read_only_writable(paths: &[String]) -> bool {
    paths.len() <= MAX_READ_ONLY_WRITABLE
        && paths.iter().all(|path| RelativePath::parse(path).is_ok())
}

/// Clean config must suppress inherited hooks, MCP and plugins. Inherited
/// config is refused for read-only work until confined extension evidence exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexPolicy {
    Clean,
    Inherited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionPolicy {
    Deny,
    Unconfined,
}

impl PlanRequest {
    /// Resolve Git and write exceptions only from the trusted host repository.
    pub fn with_read_only_repo(
        mut self,
        repo: &crate::ReadOnlyRepo,
        binding: &provenant_config::ProjectBinding,
        policy: &ReadOnlyPolicy,
    ) -> Result<Self, PlanError> {
        if !matches!(self.permissions.mode, AccessMode::ReadOnly)
            || self.scope != WorkScope::Project(binding.project_id())
        {
            return Err(PermissionError::InvalidReadOnlyPolicy.into());
        }
        self.permissions.read_roots.push(PathGrant {
            path: repo.root_path().to_owned(),
            scope: PathScope::Subtree,
        });
        self.permissions.read_only = Some(policy.resolve(repo, binding)?);
        Ok(self)
    }
}

pub struct PlanRequest {
    pub scope: WorkScope,
    pub permissions: PlanPermissions,
    pub codex: CodexPolicy,
    pub extensions: ExtensionPolicy,
    /// Selected non-credential Agy state directory, never the user's whole home.
    pub agy_state: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("read-only execution requires clean config and denied hooks, MCP and plugins")]
    UnconfinedExtensions,
    #[error("plan roots are invalid, overlapping or exceed their bound")]
    InvalidRoots,
    #[error(transparent)]
    Permission(#[from] PermissionError),
    #[error(transparent)]
    Home(#[from] HomeError),
}

/// No deserialisation, environment values, prompt, credential bytes or mutable
/// policy escape. This is policy, not proof of an enforcing backend: compile
/// immediately before handoff, retaining verified anchors and writer exclusion.
pub struct ConfinedPlan {
    scope: WorkScope,
    permissions: PlanPermissions,
    codex: CodexPolicy,
    extensions: ExtensionPolicy,
    agy_home: Option<HomeRecipe>,
}

impl ConfinedPlan {
    pub fn new(input: PlanRequest) -> Result<Self, PlanError> {
        let p = &input.permissions;
        if matches!(p.mode, AccessMode::ReadOnly)
            && (input.codex != CodexPolicy::Clean || input.extensions != ExtensionPolicy::Deny)
        {
            return Err(PlanError::UnconfinedExtensions);
        }
        if p.nested_confinement {
            return Err(PermissionError::NestedConfinementUnsupported.into());
        }
        if p.read_roots.len() + p.protected_roots.len() + p.secret_roots.len() > 128 {
            return Err(PlanError::InvalidRoots);
        }
        for grant in &p.read_roots {
            crate::permissions::validate_read_exec_grant(grant)?;
        }
        for grant in &p.protected_roots {
            crate::permissions::validate_noncredential_grant(grant)?;
        }
        for secret in &p.secret_roots {
            secret.validate()?;
            if p.read_roots
                .iter()
                .chain(&p.protected_roots)
                .any(|g| g.overlaps(secret))
            {
                return Err(PlanError::InvalidRoots);
            }
        }
        for private in std::iter::once(&p.scratch).chain(&p.private_state) {
            crate::permissions::validate_noncredential_grant(private)?;
            if private.scope != PathScope::Subtree
                || p.read_roots
                    .iter()
                    .chain(&p.protected_roots)
                    .chain(&p.secret_roots)
                    .any(|g| private.overlaps(g))
            {
                return Err(PlanError::InvalidRoots);
            }
        }
        if p.private_state
            .as_ref()
            .is_some_and(|g| g.overlaps(&p.scratch))
        {
            return Err(PlanError::InvalidRoots);
        }
        let agy_home = input
            .agy_state
            .map(|source| {
                let selected = PathGrant {
                    path: source.clone(),
                    scope: PathScope::Subtree,
                };
                // Only allowlisted state files are read by the recipe, never
                // a provider grant to the source credential-containing subtree.
                selected.validate()?;
                let private = p.private_state.as_ref().ok_or(PlanError::InvalidRoots)?;
                if selected.overlaps(private)
                    || selected.overlaps(&p.scratch)
                    || p.secret_roots
                        .iter()
                        .chain(&p.protected_roots)
                        .any(|g| selected.overlaps(g))
                {
                    return Err(PlanError::InvalidRoots);
                }
                // Fixed allowlist only. No credential files, executable helpers,
                // extensions, config or arbitrary home entries are mirrored.
                Ok(HomeRecipe::new(
                    source,
                    private.path.clone(),
                    RelativePath::parse("agy-home").map_err(|_| PlanError::InvalidRoots)?,
                    RelativePath::parse(".gemini/antigravity-cli")
                        .map_err(|_| PlanError::InvalidRoots)?,
                    HomeFiles {
                        copy: [
                            "history.jsonl",
                            "jetbox_summaries_proto.pb",
                            "jetski_state.pbtxt",
                            "last_check.timestamp",
                            "installation_id",
                            "conversation_summaries.db",
                        ]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                        // Never claim a live SQLite inode is a coherent snapshot.
                        refuse_source: [
                            "conversation_summaries.db",
                            "conversation_summaries.db-wal",
                            "conversation_summaries.db-shm",
                            "conversation_summaries.db-journal",
                        ]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                        refuse_private: [
                            "conversation_summaries.db-wal",
                            "conversation_summaries.db-shm",
                            "conversation_summaries.db-journal",
                        ]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    },
                    64 * 1024 * 1024,
                )?)
            })
            .transpose()?;
        Ok(Self {
            scope: input.scope,
            permissions: input.permissions,
            codex: input.codex,
            extensions: input.extensions,
            agy_home,
        })
    }

    pub fn scope(&self) -> WorkScope {
        self.scope
    }
    pub fn permissions(&self) -> &PlanPermissions {
        &self.permissions
    }
    pub fn codex_policy(&self) -> CodexPolicy {
        self.codex
    }
    pub fn extension_policy(&self) -> ExtensionPolicy {
        self.extensions
    }
    pub fn agy_home(&self) -> Option<&HomeRecipe> {
        self.agy_home.as_ref()
    }

    pub fn compile(
        &self,
        backend: &dyn ConfinementBackend,
        profile: &ProviderProfile,
        anchors: &Anchors<'_>,
    ) -> Result<ConfinedLaunch, PermissionError> {
        backend.compile(&self.permissions, profile, anchors)
    }
}

#[cfg(test)]
mod tests;
