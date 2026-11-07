//! Host-only boundary discovery shared by every enforcing caller.
use super::{MandatoryRoots, PathGrant, PathScope, PermissionError, resolve_boundary_path};
use provenant_core::Roots;
use provenant_os::fs::Directory;
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

pub fn host_credential_roots(lookup: impl Fn(&str) -> Option<OsString>) -> Vec<PathGrant> {
    let subtree = |path| PathGrant {
        path,
        scope: PathScope::Subtree,
    };
    let mut roots: Vec<_> = ["CODEX_HOME", "CLAUDE_CONFIG_DIR", "GH_CONFIG_DIR"]
        .into_iter()
        .filter_map(&lookup)
        .map(PathBuf::from)
        .map(subtree)
        .collect();
    if let Some(config) = lookup("XDG_CONFIG_HOME").map(PathBuf::from) {
        roots.extend(["gh", "gcloud", "cursor", "opencode"].map(|name| subtree(config.join(name))));
    }
    if let Some(data) = lookup("XDG_DATA_HOME").map(PathBuf::from) {
        roots.push(subtree(data.join("opencode")));
    }
    if let Some(socket) = lookup("SSH_AUTH_SOCK").map(PathBuf::from) {
        roots.push(PathGrant {
            path: socket,
            scope: PathScope::Exact,
        });
    }
    roots.retain(|root| root.path.is_absolute());
    roots
}

pub fn host_github_roots(lookup: impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(path) = lookup("GH_CONFIG_DIR").map(PathBuf::from) {
        roots.push(path);
    }
    if let Some(path) = lookup("XDG_CONFIG_HOME").map(PathBuf::from) {
        roots.push(path.join("gh"));
    }
    roots.retain(|path| path.is_absolute());
    roots
}

fn inventory_error(path: &Path, stage: &'static str) -> PermissionError {
    PermissionError::AnchorMismatchAt {
        path: path.to_owned(),
        stage,
    }
}

impl MandatoryRoots {
    /// Complete the host inventory before an enforcing launch. Credentials and
    /// GitHub overrides come from sealed host environment/account metadata;
    /// optional host settings cannot alter this mandatory inventory.
    pub fn for_host(
        roots: &Roots,
        home: &Path,
        source: &Path,
        credentials: &[PathGrant],
        github: &[PathGrant],
        protected: &[PathGrant],
    ) -> Result<Self, PermissionError> {
        let subtree = |path: &Path| PathGrant {
            path: path.to_owned(),
            scope: PathScope::Subtree,
        };
        let mut mandatory = Self::new(home, roots.state.as_path(), source)?;
        for root in credentials {
            mandatory.add_credentials(root.clone())?;
        }
        let entries = match std::fs::read_dir(home) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(inventory_error(home, "credential name inventory")),
        };
        for entry in entries.into_iter().flatten() {
            let entry = entry.map_err(|_| inventory_error(home, "credential name inventory"))?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(".agy-"))
            {
                mandatory.add_credentials(subtree(&entry.path()))?;
            }
        }
        for root in mandatory.credentials().to_vec() {
            let alias = resolve_boundary_path(&root.path)?;
            if alias != root.path {
                mandatory.add_credentials_alias(
                    &root,
                    PathGrant {
                        path: alias,
                        scope: root.scope,
                    },
                )?;
            }
        }
        let state = resolve_boundary_path(roots.state.as_path())?;
        if state != roots.state.as_path() {
            mandatory.add_state_alias(subtree(&state))?;
        }
        let resolved_source = resolve_boundary_path(source)?;
        if resolved_source != source {
            mandatory.add_source_alias(subtree(&resolved_source))?;
        }
        for root in github {
            mandatory.add_github(root.clone())?;
        }
        for root in mandatory.github().to_vec() {
            let alias = resolve_boundary_path(&root.path)?;
            if alias != root.path {
                mandatory.add_github(PathGrant {
                    path: alias,
                    scope: root.scope,
                })?;
            }
        }
        let selected = resolve_boundary_path(
            source
                .ancestors()
                .find(|path| path.join(".git").exists())
                .unwrap_or(source),
        )?;
        let mut controls = protected.to_vec();
        for binding in provenant_config::registered_projects(roots)
            .map_err(|_| inventory_error(roots.state.as_path(), "registered project inventory"))?
        {
            let common = binding.common_dir();
            let git = common.file_name() == Some(OsStr::new(".git"))
                || (common.join("HEAD").is_file() && common.join("objects").is_dir());
            if !git {
                continue;
            }
            controls.push(subtree(common));
            let project_state = roots
                .state
                .as_path()
                .join("worktrees")
                .join(binding.project_id().to_string());
            let slots = match Directory::open_anchor(&project_state) {
                Ok(directory) => {
                    let listing =
                        provenant_supervisor::custody::list_writer_reservations(&directory)
                            .map_err(|_| {
                                inventory_error(&project_state, "writer custody inventory")
                            })?;
                    if !listing.issues.is_empty() {
                        return Err(inventory_error(&project_state, "writer custody inventory"));
                    }
                    listing
                        .reservations
                        .iter()
                        .map(|reservation| {
                            reservation
                                .custody_directory()
                                .map(Path::to_owned)
                                .ok_or_else(|| {
                                    inventory_error(&project_state, "writer custody inventory")
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?
                }
                Err(error) if error.is_not_found() => Vec::new(),
                Err(_) => return Err(inventory_error(&project_state, "writer custody inventory")),
            };
            controls.extend(
                crate::worktrees::owned_worktree_paths(common, &slots)
                    .map_err(|_| inventory_error(common, "owned worktree inventory"))?
                    .into_iter()
                    .map(|path| subtree(&path)),
            );
        }
        for root in controls {
            let alias = resolve_boundary_path(&root.path)?;
            if alias == selected {
                continue;
            }
            mandatory.add_protected(root.clone())?;
            if alias != root.path {
                mandatory.add_protected(PathGrant {
                    path: alias,
                    scope: root.scope,
                })?;
            }
        }
        Ok(mandatory)
    }
}
