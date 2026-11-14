//! Host-only boundary discovery shared by every enforcing caller.
use super::{
    MandatoryRoots, PathGrant, PathScope, PermissionError, profile_path_representable,
    resolve_boundary_path,
};
use provenant_core::Roots;
use provenant_os::fs::Directory;
use std::{
    ffi::{OsStr, OsString},
    io::Write,
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
    if let Some(directory) = lookup("CLAUDE_CONFIG_DIR").map(PathBuf::from) {
        roots.push(PathGrant {
            path: directory.with_extension("json"),
            scope: PathScope::Exact,
        });
    }
    if let Some(config) = lookup("XDG_CONFIG_HOME").map(PathBuf::from) {
        roots.extend(["gh", "gcloud", "cursor", "opencode"].map(|name| subtree(config.join(name))));
    }
    if let Some(data) = lookup("XDG_DATA_HOME").map(PathBuf::from) {
        roots.push(subtree(data.join("opencode")));
    }
    if let Some(directory) = lookup("ZDOTDIR").map(PathBuf::from) {
        roots.extend(HOME_SHELL_STARTUP.iter().map(|name| PathGrant {
            path: directory.join(name),
            scope: PathScope::Exact,
        }));
    }
    if let Some(socket) = lookup("SSH_AUTH_SOCK").map(PathBuf::from) {
        roots.push(PathGrant {
            path: socket,
            scope: PathScope::Exact,
        });
    }
    if let Some(path) = lookup("CARGO_HOME").filter(|value| !value.is_empty()) {
        let path = std::path::absolute(PathBuf::from(path)).expect("host cwd resolves CARGO_HOME");
        roots.extend(crate::cargo::CREDENTIAL_FILES.iter().map(|name| PathGrant {
            path: path.join(name),
            scope: PathScope::Exact,
        }));
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

// Declared file layouts in provider identities and host credential stores.
// Identity directory names are arbitrary. These endpoints are also tracked
// when missing, so a later change is caught during compilation.
const HOST_CREDENTIAL_ENDPOINTS: &[&str] = &[
    "oauth_creds.json",
    "hosts.yml",
    "config.yml",
    "login.keychain-db",
    "Library/Keychains/login.keychain-db",
];

/// Login shells run these. They reorder `PATH`, which puts a system Git ahead of
/// the lane's Git wrapper, and they can source secrets. `PATH` already comes from
/// the launching environment, so a lane loses nothing it needs.
const SYSTEM_SHELL_STARTUP: &[&str] = &["/etc/zprofile", "/etc/profile"];
const HOME_SHELL_STARTUP: &[&str] = &[
    ".zshenv",
    ".zprofile",
    ".zshrc",
    ".zlogin",
    ".bash_profile",
    ".bash_login",
    ".bashrc",
    ".profile",
];

const CREDENTIAL_WALK_MAX_DEPTH: usize = 8;
const CREDENTIAL_WALK_MAX_ENTRIES: usize = 4096;

#[derive(Default)]
struct InventorySummary {
    unresolved: std::collections::BTreeMap<PathBuf, usize>,
    unresolved_paths: std::collections::BTreeSet<PathBuf>,
    depth: std::collections::BTreeMap<PathBuf, usize>,
    entries: std::collections::BTreeMap<PathBuf, usize>,
    outside_home: std::collections::BTreeMap<PathBuf, usize>,
    unrepresentable: std::collections::BTreeMap<PathBuf, usize>,
    inspection: std::collections::BTreeMap<PathBuf, usize>,
    covering_work: std::collections::BTreeMap<PathBuf, usize>,
    endpoint_outside_home: std::collections::BTreeMap<PathBuf, usize>,
    endpoint_covering_work: std::collections::BTreeMap<PathBuf, usize>,
    endpoint_unrepresentable: std::collections::BTreeMap<PathBuf, usize>,
}

impl InventorySummary {
    fn record(counts: &mut std::collections::BTreeMap<PathBuf, usize>, root: &Path) {
        *counts.entry(root.to_owned()).or_default() += 1;
    }

    fn record_unresolved(&mut self, root: &Path, path: &Path) {
        if self.unresolved_paths.insert(path.to_owned()) {
            Self::record(&mut self.unresolved, root);
        }
    }

    fn report(&self, warnings: &mut impl Write) {
        let mut details = Vec::new();
        for (label, counts) in [
            ("cannot resolve credential symlink", &self.unresolved),
            (
                "skipping credential symlink directory outside home",
                &self.outside_home,
            ),
            (
                "cannot represent credential symlink target",
                &self.unrepresentable,
            ),
            ("cannot inspect credential inventory", &self.inspection),
            (
                "skipping credential symlink target covering source or scratch",
                &self.covering_work,
            ),
            (
                "skipping credential endpoint directory outside home",
                &self.endpoint_outside_home,
            ),
            (
                "skipping credential endpoint target covering source or scratch",
                &self.endpoint_covering_work,
            ),
            (
                "cannot represent credential endpoint target",
                &self.endpoint_unrepresentable,
            ),
            (
                "credential symlink inventory depth limit reached",
                &self.depth,
            ),
            (
                "credential symlink inventory entry limit reached",
                &self.entries,
            ),
        ] {
            if counts.is_empty() {
                continue;
            }
            let total: usize = counts.values().sum();
            let mut roots: Vec<_> = counts.iter().collect();
            roots.sort_by(|(a_path, a), (b_path, b)| b.cmp(a).then_with(|| a_path.cmp(b_path)));
            let top = roots
                .iter()
                .take(3)
                .map(|(path, count)| format!("{} ({count})", path.display()))
                .collect::<Vec<_>>()
                .join(", ");
            details.push(format!("{label}: {total}; top roots: {top}"));
        }
        if !details.is_empty() {
            let _ = writeln!(
                warnings,
                "warning: credential symlink inventory incomplete: {}; repair the reported links or move credentials into their configured home stores",
                details.join(" | ")
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn inventory_links(
    mandatory: &mut MandatoryRoots,
    root: &PathGrant,
    home: &Path,
    source: &Path,
    scratch: &Path,
    summary: &mut InventorySummary,
) -> Result<(), PermissionError> {
    let mut pending = vec![(root.path.clone(), 0)];
    let mut count = 0;
    while let Some((directory, depth)) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(_) => {
                InventorySummary::record(&mut summary.inspection, &root.path);
                continue;
            }
        };
        for entry in entries {
            let Ok(entry) = entry else {
                InventorySummary::record(&mut summary.inspection, &root.path);
                continue;
            };
            if count == CREDENTIAL_WALK_MAX_ENTRIES {
                InventorySummary::record(&mut summary.entries, &root.path);
                return Ok(());
            }
            count += 1;
            let Ok(kind) = entry.file_type() else {
                InventorySummary::record(&mut summary.inspection, &root.path);
                continue;
            };
            let path = entry.path();
            if kind.is_symlink() {
                let (Ok(target), Ok(metadata)) =
                    (resolve_boundary_path(&path), std::fs::metadata(&path))
                else {
                    summary.record_unresolved(&root.path, &path);
                    continue;
                };
                let scope = if metadata.is_dir() {
                    PathScope::Subtree
                } else {
                    PathScope::Exact
                };
                // Provider stores also link to their own executables and to
                // installed instructions and skills. Those are not secrets:
                // denying them stops the provider starting or loading its
                // instructions, and state keeps its own boundary.
                let executable = !metadata.is_dir()
                    && std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o111
                        != 0;
                if executable
                    || std::iter::once(mandatory.state())
                        .chain(mandatory.state_aliases())
                        .any(|state| target.starts_with(&state.path))
                {
                    continue;
                }
                if scope == PathScope::Subtree && !target.starts_with(home) {
                    InventorySummary::record(&mut summary.outside_home, &root.path);
                    continue;
                }
                if scope == PathScope::Subtree
                    && (source.starts_with(&target) || scratch.starts_with(&target))
                {
                    InventorySummary::record(&mut summary.covering_work, &root.path);
                    continue;
                }
                if !profile_path_representable(&path)
                    || !profile_path_representable(&target)
                    || (PathGrant {
                        path: target.clone(),
                        scope,
                    })
                    .validate()
                    .is_err()
                    || (PathGrant {
                        path: path.clone(),
                        scope,
                    })
                    .validate()
                    .is_err()
                {
                    InventorySummary::record(&mut summary.unrepresentable, &root.path);
                    continue;
                }
                for path in [path, target] {
                    mandatory.add_credentials_alias(root, PathGrant { path, scope })?;
                }
            } else if kind.is_dir() {
                // OpenCode keeps session data here, separately from its root
                // credential endpoints. Do not spend the inventory budget on
                // logs, messages or Git snapshots. Inspect links at these names
                // above, and keep walking similarly named trees in other stores.
                // A basename alone does not identify a provider's data root.
                if depth == 0
                    && root.path == home.join(".local/share/opencode")
                    && ["log", "storage", "snapshot", "shell"]
                        .iter()
                        .any(|name| entry.file_name() == *name)
                {
                    continue;
                }
                if depth + 1 >= CREDENTIAL_WALK_MAX_DEPTH {
                    InventorySummary::record(&mut summary.depth, &root.path);
                } else {
                    pending.push((path, depth + 1));
                }
            }
        }
    }
    Ok(())
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
        Self::for_host_with_scratch(
            roots,
            home,
            source,
            roots.state.as_path(),
            credentials,
            github,
            protected,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn for_host_with_scratch(
        roots: &Roots,
        home: &Path,
        source: &Path,
        scratch: &Path,
        credentials: &[PathGrant],
        github: &[PathGrant],
        protected: &[PathGrant],
    ) -> Result<Self, PermissionError> {
        Self::for_host_with_scratch_mode(
            roots,
            home,
            source,
            scratch,
            credentials,
            github,
            protected,
            true,
        )
    }

    /// Keep structural integrity roots for native lanes without inspecting
    /// credentials that only an enforcing sandbox needs to deny.
    #[allow(clippy::too_many_arguments)]
    pub fn for_host_with_scratch_mode(
        roots: &Roots,
        home: &Path,
        source: &Path,
        scratch: &Path,
        credentials: &[PathGrant],
        github: &[PathGrant],
        protected: &[PathGrant],
        sandboxed: bool,
    ) -> Result<Self, PermissionError> {
        let subtree = |path: &Path| PathGrant {
            path: path.to_owned(),
            scope: PathScope::Subtree,
        };
        let mut mandatory = Self::new(home, roots.state.as_path(), source)?;
        for root in credentials {
            mandatory.add_credentials(root.clone())?;
        }
        for path in SYSTEM_SHELL_STARTUP
            .iter()
            .map(PathBuf::from)
            .chain(HOME_SHELL_STARTUP.iter().map(|name| home.join(name)))
        {
            mandatory.add_credentials(PathGrant {
                path,
                scope: PathScope::Exact,
            })?;
        }
        if sandboxed {
            let entries = match std::fs::read_dir(home) {
                Ok(entries) => Some(entries),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err(inventory_error(home, "credential name inventory")),
            };
            for entry in entries.into_iter().flatten() {
                let entry =
                    entry.map_err(|_| inventory_error(home, "credential name inventory"))?;
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".agy-"))
                {
                    mandatory.add_credentials(subtree(&entry.path()))?;
                }
            }
        }
        for root in github {
            mandatory.add_github(root.clone())?;
        }
        let resolved_source = resolve_boundary_path(source)?;
        // Freeze each declared credential root in both launch modes. Sandbox
        // mode controls recursive inventory, not mandatory alias validation.
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
        if sandboxed {
            let endpoints: std::collections::BTreeSet<_> = provenant_records::ADAPTER_SPECS
                .iter()
                .flat_map(|spec| spec.credential_files.iter().copied())
                .chain(HOST_CREDENTIAL_ENDPOINTS.iter().copied())
                .collect();
            let resolved_scratch = resolve_boundary_path(scratch)?;
            let resolved_home = resolve_boundary_path(home)?;
            let mut summary = InventorySummary::default();
            for root in mandatory.credentials().to_vec() {
                if root.scope == PathScope::Subtree {
                    inventory_links(
                        &mut mandatory,
                        &root,
                        &resolved_home,
                        &resolved_source,
                        &resolved_scratch,
                        &mut summary,
                    )?;
                    for endpoint in &endpoints {
                        let path = root.path.join(endpoint);
                        let Ok(target) = resolve_boundary_path(&path) else {
                            summary.record_unresolved(&root.path, &path);
                            continue;
                        };
                        if target != path && target.is_dir() && !target.starts_with(&resolved_home)
                        {
                            InventorySummary::record(
                                &mut summary.endpoint_outside_home,
                                &root.path,
                            );
                            continue;
                        }
                        if target != path
                            && target.is_dir()
                            && (resolved_source.starts_with(&target)
                                || resolved_scratch.starts_with(&target))
                        {
                            InventorySummary::record(
                                &mut summary.endpoint_covering_work,
                                &root.path,
                            );
                            continue;
                        }
                        if target != path
                            && (!profile_path_representable(&path)
                                || !profile_path_representable(&target)
                                || (PathGrant {
                                    path: target.clone(),
                                    scope: PathScope::Exact,
                                })
                                .validate()
                                .is_err())
                        {
                            InventorySummary::record(
                                &mut summary.endpoint_unrepresentable,
                                &root.path,
                            );
                            continue;
                        }
                        // Also retain regular and missing endpoints for the final
                        // metadata recheck, without rendering redundant denials
                        // for files already covered by the store subtree.
                        mandatory
                            .credential_endpoints
                            .push((path.clone(), target.clone()));
                        if target != path {
                            // Keep the public endpoint too: compilation must catch
                            // retargeting after inventory, even inside a real store.
                            for path in [path, target] {
                                mandatory.add_credentials_alias(
                                    &root,
                                    PathGrant {
                                        path,
                                        scope: PathScope::Exact,
                                    },
                                )?;
                            }
                        }
                    }
                }
            }
            summary.report(&mut std::io::stderr().lock());
        }
        let state = resolve_boundary_path(roots.state.as_path())?;
        if state != roots.state.as_path() {
            mandatory.add_state_alias(subtree(&state))?;
        }
        if resolved_source != source {
            mandatory.add_source_alias(subtree(&resolved_source))?;
        }
        let selected = resolve_boundary_path(
            source
                .ancestors()
                .find(|path| path.join(".git").exists())
                .unwrap_or(source),
        )?;
        // Common Git directories are flagged so no host write can open them.
        let mut controls: Vec<(PathGrant, bool)> =
            protected.iter().map(|root| (root.clone(), false)).collect();
        for binding in provenant_config::registered_projects(roots)
            .map_err(|_| inventory_error(roots.state.as_path(), "registered project inventory"))?
        {
            let common = binding.common_dir();
            let git = common.file_name() == Some(OsStr::new(".git"))
                || (common.join("HEAD").is_file() && common.join("objects").is_dir());
            if !git {
                continue;
            }
            controls.push((subtree(common), true));
            for allocation in crate::worktrees::allocation_directories(common)
                .map_err(|_| inventory_error(common, "worktree allocation inventory"))?
            {
                controls.push((subtree(&allocation), false));
            }
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
                    .map(|path| (subtree(&path), false)),
            );
        }
        for (root, git) in controls {
            let alias = resolve_boundary_path(&root.path)?;
            if alias == selected {
                continue;
            }
            let alias = (alias != root.path).then_some(PathGrant {
                path: alias,
                scope: root.scope,
            });
            for root in std::iter::once(root).chain(alias) {
                if git {
                    mandatory.add_protected_git(root)?;
                } else {
                    mandatory.add_protected(root)?;
                }
            }
        }
        Ok(mandatory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocated_cargo_credentials_are_mandatory_denials() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let cargo = root.join("custom-cargo");
        let inventory = host_credential_roots(|name| {
            (name == "CARGO_HOME").then(|| cargo.clone().into_os_string())
        });
        let mandatory =
            MandatoryRoots::for_host(&roots, &home, &root.join("source"), &inventory, &[], &[])?;
        for name in crate::cargo::CREDENTIAL_FILES {
            let credential = PathGrant {
                path: cargo.join(name),
                scope: PathScope::Exact,
            };
            assert!(mandatory.credentials().contains(&credential));
            assert!(matches!(
                mandatory.validate_extra_write(&credential),
                Err(PermissionError::CredentialRootAt { .. })
            ));
        }
        Ok(())
    }

    fn fixture_roots(root: &Path) -> Result<Roots, Box<dyn std::error::Error>> {
        for name in ["home", "state", "source"] {
            std::fs::create_dir_all(root.join(name))?;
        }
        // The registry only reads a private state directory, whatever the umask.
        std::fs::set_permissions(
            root.join("state"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )?;
        let mut input = provenant_core::RootInputs::new(root.join("home"), root.join("source"));
        input.state_dir = Some(root.join("state"));
        Ok(input.resolve(&provenant_os::fs::SystemPathResolver)?)
    }

    #[test]
    fn credential_alias_inventory_runs_only_for_sandboxed_lanes()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let store = home.join(".ssh");
        std::fs::create_dir(&store)?;
        let key = home.join("private-key");
        std::fs::write(&key, b"synthetic credential")?;
        std::os::unix::fs::symlink(&key, store.join("identity"))?;
        let alias = PathGrant {
            path: key,
            scope: PathScope::Exact,
        };
        for sandboxed in [false, true] {
            let inventory = MandatoryRoots::for_host_with_scratch_mode(
                &roots,
                &home,
                &root.join("source"),
                &root.join("scratch"),
                &[],
                &[],
                &[],
                sandboxed,
            )?;
            assert_eq!(inventory.credentials().contains(&alias), sandboxed);
        }
        Ok(())
    }

    #[test]
    fn inventory_finds_dotfiles_links_and_skips_lane_directories()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let store = home.join(".ssh");
        let dotfiles = home.join("dotfiles");
        let scratch = root.join("scratch");
        for path in [&store, &dotfiles, &scratch, &dotfiles.join("keys")] {
            std::fs::create_dir_all(path)?;
        }
        let key = dotfiles.join("id_rsa");
        std::fs::write(&key, b"fixture")?;
        std::os::unix::fs::symlink(&key, store.join("id_rsa"))?;
        std::os::unix::fs::symlink(dotfiles.join("keys"), store.join("keys"))?;
        let hidden = root.join("not-followed");
        std::fs::write(&hidden, b"fixture")?;
        std::os::unix::fs::symlink(&hidden, dotfiles.join("keys/nested"))?;
        std::os::unix::fs::symlink(root.join("source"), store.join("source"))?;
        std::os::unix::fs::symlink(root.join("source"), store.join("auth.json"))?;
        std::os::unix::fs::symlink(&scratch, store.join("scratch"))?;
        let inventory = MandatoryRoots::for_host_with_scratch(
            &roots,
            &home,
            &root.join("source"),
            &scratch,
            &[],
            &[],
            &[],
        )?;
        for (path, scope) in [
            (key, PathScope::Exact),
            (dotfiles.join("keys"), PathScope::Subtree),
        ] {
            assert!(inventory.credentials().contains(&PathGrant { path, scope }));
        }
        for path in [root.join("source"), scratch] {
            assert!(
                !inventory
                    .credentials()
                    .iter()
                    .any(|grant| grant.path == path)
            );
        }
        assert!(
            !inventory
                .credentials()
                .iter()
                .any(|grant| grant.path == hidden)
        );
        Ok(())
    }

    #[test]
    fn inventory_denies_shell_startup_through_links_and_zdotdir()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().canonicalize()?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let dotfiles = root.join("dotfiles");
        std::fs::create_dir_all(&dotfiles)?;
        std::fs::write(dotfiles.join("zshenv"), "")?;
        std::os::unix::fs::symlink(dotfiles.join("zshenv"), home.join(".zshenv"))?;
        let zdotdir = root.join("zdot");
        let credentials = host_credential_roots(|name| {
            (name == "ZDOTDIR").then(|| zdotdir.clone().into_os_string())
        });
        let inventory =
            MandatoryRoots::for_host(&roots, &home, &root.join("source"), &credentials, &[], &[])?;
        let exact = |path: PathBuf| PathGrant {
            path,
            scope: PathScope::Exact,
        };
        for path in [
            home.join(".zshenv"),
            dotfiles.join("zshenv"),
            home.join(".zprofile"),
            home.join(".bash_profile"),
            zdotdir.join(".zshenv"),
            PathBuf::from("/etc/zprofile"),
            PathBuf::from("/etc/profile"),
        ] {
            assert!(
                inventory.credentials().contains(&exact(path.clone())),
                "{}",
                path.display()
            );
        }
        Ok(())
    }

    #[test]
    fn inventory_summarises_walk_failures_across_roots() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        let target = base.join("credential");
        std::fs::create_dir(&home)?;
        std::fs::write(&target, b"fixture")?;
        let mut mandatory = MandatoryRoots::new(&home, &base.join("state"), &base.join("source"))?;
        let mut summary = InventorySummary::default();
        let mut warnings = Vec::new();
        for name in ["first", "second"] {
            let store = home.join(name);
            std::fs::create_dir(&store)?;
            for index in 0..2 {
                std::os::unix::fs::symlink(
                    base.join("missing"),
                    store.join(format!("dangling{index}")),
                )?;
            }
            std::os::unix::fs::symlink(&target, store.join("valid"))?;
            let mut deep = store.clone();
            for _ in 0..CREDENTIAL_WALK_MAX_DEPTH {
                deep.push("nested");
                std::fs::create_dir(&deep)?;
            }
            let grant = PathGrant {
                path: store,
                scope: PathScope::Subtree,
            };
            inventory_links(
                &mut mandatory,
                &grant,
                &home,
                &base.join("source"),
                &base.join("scratch"),
                &mut summary,
            )?;
        }
        assert!(warnings.is_empty());
        summary.report(&mut warnings);
        let output = String::from_utf8(warnings)?;
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), 1, "inventory warnings must stay on one line");
        assert!(
            lines[0].contains("cannot resolve credential symlink: 4;"),
            "{output}"
        );
        assert!(output.contains("depth limit reached: 2;"), "{output}");
        assert!(output.contains("first (2)"));
        assert!(output.contains("second (2)"));
        assert!(mandatory.credentials().contains(&PathGrant {
            path: target,
            scope: PathScope::Exact
        }));
        Ok(())
    }

    #[test]
    fn inventory_summarises_skipped_links_and_inspection_failures()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        let outside = base.join("outside");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&outside)?;
        let target = home.join("bad\u{1}name");
        std::fs::write(&target, b"fixture")?;
        let mut mandatory = MandatoryRoots::new(&home, &base.join("state"), &base.join("source"))?;
        let mut summary = InventorySummary::default();
        let mut warnings = Vec::new();
        for index in 0..4 {
            let store = home.join(format!("store{index}"));
            std::fs::create_dir(&store)?;
            for link in 0..(4 - index) {
                std::os::unix::fs::symlink(&outside, store.join(format!("outside{link}")))?;
                std::os::unix::fs::symlink(&target, store.join(format!("bad{link}")))?;
                std::os::unix::fs::symlink(&home, store.join(format!("covering{link}")))?;
                InventorySummary::record(&mut summary.inspection, &store);
            }
            inventory_links(
                &mut mandatory,
                &PathGrant {
                    path: store,
                    scope: PathScope::Subtree,
                },
                &home,
                &home.join("source"),
                &home.join("scratch"),
                &mut summary,
            )?;
        }
        assert!(warnings.is_empty());
        summary.report(&mut warnings);
        let output = String::from_utf8(warnings)?;
        // Every skipped category remains diagnosable even when warnings are
        // coalesced or their presentation changes.
        for category in [
            "skipping credential symlink directory outside home",
            "cannot represent credential symlink target",
            "cannot inspect credential inventory",
            "skipping credential symlink target covering source or scratch",
        ] {
            assert!(output.contains(category), "{category}: {output}");
        }
        assert!(output.contains("store0"), "{output}");
        assert!(
            !mandatory
                .credentials()
                .iter()
                .any(|grant| grant.path == target || grant.path == outside)
        );
        Ok(())
    }

    #[test]
    fn inventory_summarises_directory_inspection_errors() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        // Linux returns NotFound for a missing parent before inspecting the
        // overlong component; create it so both platforms reach that error.
        std::fs::create_dir(&home)?;
        let mut mandatory = MandatoryRoots::new(&home, &base.join("state"), &base.join("source"))?;
        let root = PathGrant {
            path: home.join("x".repeat(1024)),
            scope: PathScope::Subtree,
        };
        let mut summary = InventorySummary::default();
        let mut warnings = Vec::new();
        for _ in 0..2 {
            inventory_links(
                &mut mandatory,
                &root,
                &home,
                &base.join("source"),
                &base.join("scratch"),
                &mut summary,
            )?;
        }
        assert!(warnings.is_empty());
        summary.report(&mut warnings);
        let output = String::from_utf8(warnings)?;
        assert_eq!(output.lines().count(), 1);
        assert!(
            output.contains("cannot inspect credential inventory: 2;"),
            "{output}"
        );
        Ok(())
    }

    #[test]
    fn inventory_summary_limits_roots_and_deduplicates_unresolved_paths() {
        let mut summary = InventorySummary::default();
        for index in 0..5 {
            let root = PathBuf::from(format!("/root{index}"));
            summary.record_unresolved(&root, &root.join("missing"));
            summary.record_unresolved(&root, &root.join("missing"));
            InventorySummary::record(&mut summary.entries, &root);
            InventorySummary::record(&mut summary.endpoint_outside_home, &root);
            InventorySummary::record(&mut summary.endpoint_covering_work, &root);
            InventorySummary::record(&mut summary.endpoint_unrepresentable, &root);
        }
        let mut warnings = Vec::new();
        summary.report(&mut warnings);
        let output = String::from_utf8(warnings).unwrap();
        assert_eq!(
            output.lines().count(),
            1,
            "inventory warnings must stay on one line"
        );
        assert!(output.contains("cannot resolve credential symlink: 5;"));
        assert!(output.contains("entry limit reached: 5;"));
        assert!(output.contains("endpoint directory outside home: 5;"));
        assert!(output.contains("endpoint target covering source or scratch: 5;"));
        assert!(output.contains("cannot represent credential endpoint target: 5;"));
        assert!(!output.contains("/root3"));
    }

    // Medium: real filesystem inventory, no native sandbox required.
    #[test]
    fn inventory_skips_opencode_runtime_trees() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let home = root.join("home");
        let store = home.join(".local/share/opencode");
        let target = root.join("credential");
        std::fs::write(&target, b"fixture")?;
        for name in ["log", "storage", "snapshot", "shell"] {
            let directory = store.join(name);
            std::fs::create_dir_all(&directory)?;
            for index in 0..=CREDENTIAL_WALK_MAX_ENTRIES {
                std::fs::File::create(directory.join(index.to_string()))?;
            }
        }
        // A symlink at a runtime directory name must still be inventoried.
        std::fs::remove_dir_all(store.join("shell"))?;
        std::os::unix::fs::symlink(&target, store.join("shell"))?;
        let mut mandatory = MandatoryRoots::new(
            &root.join("home"),
            &root.join("state"),
            &root.join("source"),
        )?;
        let mut warnings = Vec::new();
        let mut summary = InventorySummary::default();
        inventory_links(
            &mut mandatory,
            &PathGrant {
                path: store,
                scope: PathScope::Subtree,
            },
            &home,
            &root.join("source"),
            &root.join("scratch"),
            &mut summary,
        )?;
        summary.report(&mut warnings);
        assert!(
            warnings.is_empty(),
            "{}",
            String::from_utf8_lossy(&warnings)
        );
        assert!(mandatory.credentials().contains(&PathGrant {
            path: target,
            scope: PathScope::Exact
        }));
        Ok(())
    }

    // Medium: a registered store's name does not establish its provider.
    #[test]
    fn inventory_walks_unrelated_store_named_opencode() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let store = root.join("identity/opencode");
        let target = root.join("credential");
        std::fs::create_dir_all(store.join("storage"))?;
        std::fs::write(&target, b"fixture")?;
        std::os::unix::fs::symlink(&target, store.join("storage/link"))?;
        let inventory = MandatoryRoots::for_host(
            &roots,
            &home,
            &root.join("source"),
            &[PathGrant {
                path: store,
                scope: PathScope::Subtree,
            }],
            &[],
            &[],
        )?;
        assert!(inventory.credentials().contains(&PathGrant {
            path: target,
            scope: PathScope::Exact,
        }));
        Ok(())
    }

    // Medium: exceed the real budget without depending on directory entry order.
    #[test]
    fn inventory_root_limit_warns_and_preserves_later_roots()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let huge = home.join("huge");
        let later = home.join("later");
        let target = home.join("credential");
        let missed = home.join("beyond-budget");
        for directory in [&huge, &later, &huge.join("storage")] {
            std::fs::create_dir_all(directory)?;
        }
        for index in 0..=CREDENTIAL_WALK_MAX_ENTRIES {
            std::fs::File::create(huge.join(index.to_string()))?;
        }
        for path in [&target, &missed] {
            std::fs::write(path, b"fixture")?;
        }
        std::os::unix::fs::symlink(&target, later.join("link"))?;
        std::os::unix::fs::symlink(&missed, huge.join("storage/link"))?;
        let subtree = |path| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let huge = subtree(huge);
        let mut mandatory = MandatoryRoots::new(&home, &root.join("state"), &root.join("source"))?;
        let mut warnings = Vec::new();
        let mut summary = InventorySummary::default();
        inventory_links(
            &mut mandatory,
            &huge,
            &home,
            &root.join("source"),
            &root.join("scratch"),
            &mut summary,
        )?;
        summary.report(&mut warnings);
        let warnings = String::from_utf8(warnings)?;
        assert!(warnings.contains("credential symlink inventory entry limit reached"));
        assert!(warnings.contains(&huge.path.display().to_string()));
        assert!(
            !mandatory
                .credentials()
                .iter()
                .any(|grant| grant.path == missed)
        );
        let inventory = MandatoryRoots::for_host(
            &roots,
            &home,
            &root.join("source"),
            &[huge, subtree(later)],
            &[],
            &[],
        )?;
        assert!(inventory.credentials().contains(&PathGrant {
            path: target,
            scope: PathScope::Exact
        }));
        assert!(
            !inventory
                .credentials()
                .iter()
                .any(|grant| grant.path == missed)
        );
        Ok(())
    }

    #[test]
    fn inventory_depth_limit_keeps_earlier_links() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let store = root.join("store");
        let target = root.join("target");
        std::fs::create_dir(&store)?;
        std::fs::write(&target, b"fixture")?;
        std::os::unix::fs::symlink(&target, store.join("found"))?;
        let mut deep = store.clone();
        for _ in 0..CREDENTIAL_WALK_MAX_DEPTH {
            deep.push("nested");
            std::fs::create_dir(&deep)?;
        }
        let missed = root.join("missed");
        std::fs::write(&missed, b"fixture")?;
        std::os::unix::fs::symlink(&missed, deep.join("link"))?;
        let mut mandatory = MandatoryRoots::new(
            &root.join("home"),
            &root.join("state"),
            &root.join("source"),
        )?;
        let grant = PathGrant {
            path: store,
            scope: PathScope::Subtree,
        };
        inventory_links(
            &mut mandatory,
            &grant,
            &root.join("home"),
            &root.join("source"),
            &root.join("scratch"),
            &mut InventorySummary::default(),
        )?;
        assert!(
            mandatory
                .credentials()
                .iter()
                .any(|grant| grant.path == target)
        );
        assert!(
            !mandatory
                .credentials()
                .iter()
                .any(|grant| grant.path == missed)
        );
        Ok(())
    }

    #[test]
    fn inventory_leaves_provider_executables_and_installed_state_usable()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let store = home.join(".codex");
        std::fs::create_dir_all(store.join("tmp"))?;
        let provider = root.join("codex");
        std::fs::write(&provider, b"fixture")?;
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755))?;
        std::os::unix::fs::symlink(&provider, store.join("tmp/apply_patch"))?;
        let installed = root.join("state/releases/skill");
        std::fs::create_dir_all(&installed)?;
        std::os::unix::fs::symlink(&installed, store.join("skill"))?;
        let secret = root.join("dotfiles-token");
        std::fs::write(&secret, b"fixture")?;
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))?;
        std::os::unix::fs::symlink(&secret, store.join("token"))?;

        let mandatory =
            MandatoryRoots::for_host(&roots, &home, &root.join("source"), &[], &[], &[])?;
        let denied = |path: &Path| {
            mandatory
                .credentials()
                .iter()
                .any(|credential| credential.path == path)
        };
        assert!(!denied(&provider), "a provider executable must still run");
        assert!(
            !denied(&installed),
            "installed state keeps its own boundary"
        );
        assert!(denied(&secret), "a linked secret stays protected");
        Ok(())
    }

    #[test]
    fn inventory_skips_unrepresentable_links_and_external_directories()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let store = home.join(".ssh");
        let external_dir = root.join("external-dir");
        let external_file = root.join("external-file");
        let internal_dir = home.join("internal-dir");
        for dir in [&store, &external_dir, &internal_dir] {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&external_file, b"fixture")?;
        let bad_target = root.join("bad\u{1}target");
        std::fs::write(&bad_target, b"fixture")?;
        let bad_name = store.join("bad-\u{1}name");
        std::os::unix::fs::symlink(&external_file, &bad_name)?;
        let non_utf8_name = store.join(OsString::from_vec(b"bad-\xff".to_vec()));
        let non_utf8_created = std::os::unix::fs::symlink(&external_file, &non_utf8_name).is_ok();
        std::os::unix::fs::symlink(&bad_target, store.join("bad-target"))?;
        std::os::unix::fs::symlink(&bad_target, store.join("auth.json"))?;
        std::os::unix::fs::symlink(&external_dir, store.join("external-dir"))?;
        std::os::unix::fs::symlink(&external_dir, store.join("config.yml"))?;
        std::os::unix::fs::symlink(root.join("missing"), store.join("hosts.yml"))?;
        std::os::unix::fs::symlink(&external_file, store.join("external-file"))?;
        std::os::unix::fs::symlink(&internal_dir, store.join("internal-dir"))?;
        let inventory =
            MandatoryRoots::for_host(&roots, &home, &root.join("source"), &[], &[], &[])?;
        for path in [
            &bad_name,
            &bad_target,
            &store.join("bad-target"),
            &store.join("auth.json"),
            &external_dir,
            &store.join("external-dir"),
            &store.join("config.yml"),
            &store.join("hosts.yml"),
        ] {
            assert!(
                !inventory
                    .credentials()
                    .iter()
                    .any(|grant| &grant.path == path),
                "unexpected {}",
                path.display()
            );
        }
        for path in [&external_file, &internal_dir] {
            assert!(
                inventory
                    .credentials()
                    .iter()
                    .any(|grant| &grant.path == path)
            );
        }
        if non_utf8_created {
            assert!(
                !inventory
                    .credentials()
                    .iter()
                    .any(|grant| grant.path == non_utf8_name)
            );
        }
        assert!(
            !inventory
                .credential_endpoints
                .iter()
                .any(|(path, _)| path == &store.join("auth.json"))
        );
        assert!(
            !inventory
                .credential_endpoints
                .iter()
                .any(|(path, _)| path == &store.join("config.yml"))
        );
        assert!(
            !inventory
                .credential_endpoints
                .iter()
                .any(|(path, _)| path == &store.join("hosts.yml"))
        );
        Ok(())
    }

    #[cfg(any(target_os = "macos", feature = "snapshot"))]
    #[test]
    fn selected_endpoint_does_not_exempt_state_or_non_exempt_targets()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let store = home.join(".local/share/opencode");
        std::fs::create_dir_all(&store)?;
        let endpoint = store.join("auth.json");
        let exact = |path| PathGrant {
            path,
            scope: PathScope::Exact,
        };
        for target in [
            root.join("state/credential-authority.json"),
            home.join("Library/Keychains/fixture"),
        ] {
            std::fs::create_dir_all(target.parent().ok_or("target parent")?)?;
            std::fs::write(&target, b"fixture")?;
            std::os::unix::fs::symlink(&target, &endpoint)?;
            let inventory =
                MandatoryRoots::for_host(&roots, &home, &root.join("source"), &[], &[], &[])?;
            assert!(inventory.credentials().contains(&exact(target)));
            assert!(
                inventory
                    .credential_alias_reads(&[exact(endpoint.clone())])
                    .is_empty()
            );
            std::fs::remove_file(&endpoint)?;
        }
        Ok(())
    }

    #[cfg(any(target_os = "macos", feature = "snapshot"))]
    #[test]
    fn selected_exact_endpoint_keeps_only_its_dotfiles_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let store = home.join(".local/share/opencode");
        let dotfiles = root.join("dotfiles");
        std::fs::create_dir_all(&store)?;
        std::fs::create_dir_all(&dotfiles)?;
        let endpoint = store.join("auth.json");
        let target = dotfiles.join("opencode-auth");
        std::fs::write(&target, b"fixture")?;
        std::os::unix::fs::symlink(&target, &endpoint)?;
        let inventory =
            MandatoryRoots::for_host(&roots, &home, &root.join("source"), &[], &[], &[])?;
        let exact = |path| PathGrant {
            path,
            scope: PathScope::Exact,
        };
        assert!(inventory.credentials().contains(&exact(target.clone())));
        assert_eq!(
            inventory.credential_alias_reads(&[exact(endpoint)]),
            vec![exact(target)]
        );
        assert!(
            inventory
                .credential_alias_reads(&[exact(store.join("other"))])
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn host_inventory_tracks_credential_endpoints_and_their_categories()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let roots = fixture_roots(&root)?;
        let home = root.join("home");
        let subtree = |path: PathBuf| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let exact = |path: PathBuf| PathGrant {
            path,
            scope: PathScope::Exact,
        };
        // Registered identities need no selected-profile information. Include
        // several provider layouts, an isolated home and a relocated gh store.
        let mut endpoints = vec![
            (home.join(".codex"), "auth.json"),
            (root.join("selected"), "auth.json"),
            (root.join("unselected"), "auth.json"),
            (root.join("claude"), ".credentials.json"),
            (root.join("cursor"), "cli-config.json"),
            (root.join("cursor-desktop"), "state.vscdb"),
            (root.join("opencode"), "auth.json"),
            (
                home.join(".agy-synthetic"),
                "Library/Keychains/login.keychain-db",
            ),
            (root.join("github"), "hosts.yml"),
            (home.join("Library/Keychains"), "login.keychain-db"),
        ];
        let declared: std::collections::BTreeSet<_> = provenant_records::ADAPTER_SPECS
            .iter()
            .flat_map(|spec| spec.credential_files.iter().copied())
            .collect();
        endpoints.extend(
            declared
                .into_iter()
                .map(|name| (root.join("declared"), name)),
        );
        let regular = home.join(".codex/config.toml");
        std::fs::create_dir_all(regular.parent().ok_or("regular parent")?)?;
        std::fs::File::create(&regular)?;
        let mut targets = Vec::new();
        for (index, (store, endpoint)) in endpoints.iter().enumerate() {
            let path = store.join(endpoint);
            std::fs::create_dir_all(path.parent().ok_or("endpoint parent")?)?;
            let target = root.join(format!("outside-{index}"));
            std::fs::File::create(&target)?;
            std::os::unix::fs::symlink(&target, &path)?;
            targets.push(exact(target));
        }
        let mut credentials = endpoints[1..7]
            .iter()
            .map(|(store, _)| subtree(store.clone()))
            .collect::<Vec<_>>();
        credentials.push(subtree(root.join("declared")));
        let inventory = MandatoryRoots::for_host(
            &roots,
            &home,
            &root.join("source"),
            &credentials,
            &[subtree(root.join("github"))],
            &[],
        )?;
        for ((store, endpoint), target) in endpoints.iter().zip(&targets) {
            assert!(
                inventory.credentials().contains(target),
                "missing target for {endpoint}"
            );
            assert!(
                inventory
                    .credentials()
                    .iter()
                    .any(|grant| grant.path == store.join(endpoint)),
                "endpoint must be revalidated"
            );
            assert!(matches!(
                inventory.validate_extra_write(target),
                Err(PermissionError::CredentialRootAt { .. })
            ));
        }
        assert!(inventory.github().contains(&targets[8]));
        assert!(inventory.non_exempt().contains(&targets[9]));
        assert!(!inventory.non_exempt().contains(&targets[1]));
        #[cfg(target_os = "macos")]
        {
            inventory.revalidate_paths()?;
            let replacement = root.join("replacement");
            std::fs::File::create(&replacement)?;
            // Retarget both an existing link and initially regular/absent files.
            for endpoint in [
                home.join(".codex/auth.json"),
                regular,
                home.join(".codex/settings.json"),
            ] {
                let before = std::fs::read_link(&endpoint).ok();
                let existed = std::fs::symlink_metadata(&endpoint).is_ok();
                if existed {
                    std::fs::remove_file(&endpoint)?;
                }
                std::os::unix::fs::symlink(&replacement, &endpoint)?;
                assert!(inventory.revalidate_paths().is_err());
                std::fs::remove_file(&endpoint)?;
                if let Some(target) = before {
                    std::os::unix::fs::symlink(target, &endpoint)?;
                } else if existed {
                    std::fs::File::create(&endpoint)?;
                }
                inventory.revalidate_paths()?;
            }
        }
        Ok(())
    }

    #[test]
    fn host_inventory_protects_later_worktree_allocations() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let roots = fixture_roots(&root)?;
        let mut allocations = Vec::new();
        for project in ["source", "other-project"] {
            let primary = root.join(project);
            let common = primary.join(".git");
            std::fs::create_dir_all(&common)?;
            provenant_config::bind_project(
                &roots,
                provenant_config::ProjectLocation::new(common)?,
            )?;
            let config = primary.join(".provenant");
            std::fs::create_dir_all(&config)?;
            let mut ports = provenant_config::PortTable::builtin();
            ports.worktree_layout.roots = vec![PathBuf::from(".claude/worktrees")];
            ports.worktree_layout.default = PathBuf::from(".claude/worktrees/<branch>");
            std::fs::write(config.join("ports.json"), serde_json::to_vec(&ports)?)?;
            allocations.push(primary.join(".claude/worktrees"));
        }
        let separate = root.join("separate-project");
        let common = separate.join("git-meta");
        std::fs::create_dir_all(common.join("objects"))?;
        std::fs::write(common.join("HEAD"), b"ref: refs/heads/main\n")?;
        std::fs::write(separate.join(".git"), b"gitdir: git-meta\n")?;
        provenant_config::bind_project(&roots, provenant_config::ProjectLocation::new(common)?)?;
        let config = separate.join(".provenant");
        std::fs::create_dir_all(&config)?;
        let mut ports = provenant_config::PortTable::builtin();
        ports.worktree_layout.roots = vec![PathBuf::from(".claude/worktrees")];
        ports.worktree_layout.default = PathBuf::from(".claude/worktrees/<branch>");
        std::fs::write(config.join("ports.json"), serde_json::to_vec(&ports)?)?;
        allocations.push(separate.join(".claude/worktrees"));
        let inventory = MandatoryRoots::for_host(
            &roots,
            &root.join("home"),
            &root.join("source"),
            &[],
            &[],
            &[],
        )?;
        for allocation in allocations {
            // Capture before even the allocation directory exists.
            let later = allocation.join("later-checkout");
            std::fs::create_dir_all(&later)?;
            assert!(
                inventory.protected().iter().any(
                    |grant| grant.scope == PathScope::Subtree && later.starts_with(&grant.path)
                )
            );
            assert!(matches!(
                inventory.validate_extra_write(&PathGrant {
                    path: later,
                    scope: PathScope::Subtree
                }),
                Err(PermissionError::ProtectedOverlap)
            ));
            let sibling = PathGrant {
                path: allocation.with_file_name(".worktrees-other"),
                scope: PathScope::Subtree,
            };
            assert!(
                inventory
                    .protected()
                    .iter()
                    .all(|root| !root.overlaps(&sibling))
            );
        }
        Ok(())
    }
}
