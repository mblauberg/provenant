//! Bounded decoding of Git tree entries for an attempt-owned source view.
use provenant_confinement::repo::RepoPath;
use provenant_core::GitOid;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::PathBuf,
};

const MAX_ENTRIES: usize = 100_000;
const MAX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceKind {
    File,
    Executable,
    Symlink,
}

#[derive(Debug)]
pub(super) struct SourceEntry {
    pub path: RepoPath,
    pub oid: GitOid,
    pub kind: SourceKind,
    pub size: u64,
}

/// `git ls-tree -r -l -z` is byte-oriented: paths need not be UTF-8.
/// Refuse unsupported objects rather than quietly presenting a partial tree.
pub(super) fn parse_source_tree(bytes: &[u8]) -> Result<Vec<SourceEntry>, &'static str> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err("source tree listing is incomplete");
    }
    let mut entries = Vec::new();
    let mut paths = HashSet::new();
    let mut total = 0u64;
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("invalid source tree entry")?;
        let mut fields = record[..tab]
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty());
        let mode = fields.next().ok_or("invalid source tree mode")?;
        let kind = match mode {
            b"100644" => SourceKind::File,
            b"100755" => SourceKind::Executable,
            b"120000" => SourceKind::Symlink,
            _ => return Err("source tree contains an unsupported entry"),
        };
        if fields.next() != Some(b"blob".as_slice()) {
            return Err("source tree contains an unsupported object");
        }
        let oid = std::str::from_utf8(fields.next().ok_or("missing source object")?)
            .map_err(|_| "invalid source object")?
            .parse::<GitOid>()
            .map_err(|_| "invalid source object")?;
        let size = std::str::from_utf8(fields.next().ok_or("missing source size")?)
            .map_err(|_| "invalid source size")?
            .parse::<u64>()
            .map_err(|_| "invalid source size")?;
        if fields.next().is_some() {
            return Err("invalid source tree entry");
        }
        let path = RepoPath::new(PathBuf::from(OsString::from_vec(
            record[tab + 1..].to_vec(),
        )))
        .map_err(|_| "unsafe source path")?;
        if path.as_path().components().any(|component| {
            component
                .as_os_str()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(b".git")
        }) {
            return Err("source tree contains a reserved Git path");
        }
        if !paths.insert(path.as_path().to_owned()) {
            return Err("duplicate source path");
        }
        if size > 64 * 1024 * 1024 {
            return Err("source object is too large");
        }
        total = total.checked_add(size).ok_or("source tree is too large")?;
        if total > MAX_BYTES || entries.len() >= MAX_ENTRIES {
            return Err("source tree is too large");
        }
        entries.push(SourceEntry {
            path,
            oid,
            kind,
            size,
        });
    }
    Ok(entries)
}

/// Publish a verified tree below a host-owned empty directory without following
/// any tree-provided symlink. The caller supplies bounded Git object reads.
pub(super) fn materialise_source_tree(
    root: &provenant_os::fs::Directory,
    entries: &[SourceEntry],
    mut read_blob: impl FnMut(GitOid) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    use std::io::Write;
    use std::path::{Component, Path};

    // Resolve against the complete, immutable Git tree, not the host filesystem.
    // A later symlink may change the meaning of an earlier link's target.
    let mut links = HashMap::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.kind == SourceKind::Symlink)
    {
        let bytes = read_blob(entry.oid)?;
        if u64::try_from(bytes.len()).ok() != Some(entry.size) {
            return Err("source object size changed".into());
        }
        let target = PathBuf::from(OsString::from_vec(bytes));
        if target.as_os_str().is_empty()
            || target.as_os_str().as_encoded_bytes().contains(&0)
            || target.is_absolute()
        {
            return Err("unsafe source symlink".into());
        }
        links.insert(entry.path.as_path().to_owned(), target);
    }
    // On macOS the source volume can match ASCII names without regard to
    // case. Non-ASCII filesystem equivalence is volume-dependent; fail closed
    // if it could affect a parent traversal after a named component.
    let non_ascii = links.iter().any(|(path, target)| {
        !path.as_os_str().as_encoded_bytes().is_ascii()
            || !target.as_os_str().as_encoded_bytes().is_ascii()
    });
    if cfg!(target_os = "macos") && non_ascii {
        for target in links.values() {
            let mut named = false;
            for component in target.components() {
                match component {
                    Component::Normal(_) => named = true,
                    Component::ParentDir if named => return Err("unsafe source symlink".into()),
                    _ => {}
                }
            }
        }
    }
    let folded: HashMap<_, _> = links
        .iter()
        .map(|(path, target)| {
            (
                path.as_os_str().as_encoded_bytes().to_ascii_lowercase(),
                target,
            )
        })
        .collect();
    for path in links.keys() {
        let mut resolved = PathBuf::new();
        let mut pending: VecDeque<OsString> = path
            .components()
            .map(|c| c.as_os_str().to_owned())
            .collect();
        let mut expansions = 0;
        while let Some(component) = pending.pop_front() {
            match Path::new(&component).components().next() {
                Some(Component::CurDir) => {}
                Some(Component::ParentDir) => {
                    if !resolved.pop() {
                        return Err("unsafe source symlink".into());
                    }
                }
                Some(Component::Normal(name)) => {
                    resolved.push(name);
                    let target = if cfg!(target_os = "macos") {
                        folded
                            .get(&resolved.as_os_str().as_encoded_bytes().to_ascii_lowercase())
                            .copied()
                    } else {
                        links.get(&resolved)
                    };
                    if let Some(target) = target {
                        expansions += 1;
                        if expansions > 40 {
                            return Err("unsafe source symlink".into());
                        }
                        resolved.pop();
                        for part in target.components().rev() {
                            pending.push_front(part.as_os_str().to_owned());
                        }
                    }
                }
                _ => return Err("unsafe source symlink".into()),
            }
        }
    }
    for entry in entries {
        let bytes = if let Some(target) = links.get(entry.path.as_path()) {
            target.as_os_str().as_encoded_bytes().to_vec()
        } else {
            read_blob(entry.oid)?
        };
        if u64::try_from(bytes.len()).ok() != Some(entry.size) {
            return Err("source object size changed".into());
        }
        let mut parent = root.try_clone().map_err(|error| error.to_string())?;
        let path = entry.path.as_path();
        let name = path.file_name().ok_or("invalid source path")?;
        if let Some(ancestors) = path.parent() {
            for component in ancestors.components() {
                let Component::Normal(name) = component else {
                    return Err("invalid source path".into());
                };
                match parent.mkdir(name, 0o700) {
                    Ok(()) => {}
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.to_string()),
                }
                parent = parent
                    .open_verified_directory(name)
                    .map_err(|error| error.to_string())?;
            }
        }
        match entry.kind {
            SourceKind::File | SourceKind::Executable => {
                // The attempt-owned tree is private until it is complete.
                // Exclusive creation also permits tracked names such as Cargo.lock.
                let mut file = parent.create_new(name).map_err(|error| error.to_string())?;
                file.write_all(&bytes).map_err(|error| error.to_string())?;
                if entry.kind == SourceKind::Executable {
                    file.make_owner_executable()
                        .map_err(|error| error.to_string())?;
                }
            }
            SourceKind::Symlink => {
                let target = PathBuf::from(OsString::from_vec(bytes));
                parent
                    .symlink(Path::new(&target), name)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

fn overlaps_source(
    source: &std::path::Path,
    grant: &provenant_confinement::permissions::PathGrant,
) -> bool {
    use provenant_confinement::permissions::PathScope;
    let path = std::fs::canonicalize(&grant.path).unwrap_or_else(|_| grant.path.clone());
    path.starts_with(source) || (grant.scope == PathScope::Subtree && source.starts_with(path))
}

/// An explicit access or asset grant must not reopen the live source beside
/// the pinned view. The existing policy may still grant other project roots.
pub(super) fn ensure_source_roots(
    source: &std::path::Path,
    access: &super::run::LaneAccess,
    assets: &std::path::Path,
) -> Result<(), String> {
    use provenant_confinement::permissions::{PathGrant, PathScope};
    let asset = PathGrant {
        path: assets.to_owned(),
        scope: PathScope::Subtree,
    };
    if overlaps_source(source, &asset)
        || access
            .read_roots
            .iter()
            .chain(&access.write_roots)
            .chain(&access.inputs_root)
            .any(|path| {
                overlaps_source(
                    source,
                    &PathGrant {
                        path: path.clone(),
                        scope: PathScope::Subtree,
                    },
                )
            })
        || access.attachments.iter().any(|path| {
            overlaps_source(
                source,
                &PathGrant {
                    path: path.clone(),
                    scope: PathScope::Exact,
                },
            )
        })
        || (cfg!(target_os = "macos")
            && ["/usr/share", "/private/var/db/timezone"]
                .iter()
                .any(|path| source.starts_with(path)))
    {
        return Err("an access grant overlaps the live graph source checkout".into());
    }
    Ok(())
}

pub(super) fn ensure_profile_isolated(
    source: &std::path::Path,
    profile: &provenant_confinement::permissions::ProviderProfileSpec,
) -> Result<(), String> {
    let grants = profile
        .runtime_reads
        .iter()
        .chain(&profile.process_executables)
        .chain(&profile.continuation_reads)
        .chain(&profile.state_writes)
        .chain(&profile.tool_writes)
        .chain(&profile.tool_exec_roots)
        .chain(
            profile
                .authentication
                .iter()
                .flat_map(|auth| auth.roots.iter().chain(&auth.in_place_writes)),
        );
    if grants
        .into_iter()
        .any(|grant| overlaps_source(source, grant))
    {
        return Err("a provider grant overlaps the live graph source checkout".into());
    }
    Ok(())
}

/// No provider-owned write grant may cover the immutable view.
pub(super) fn ensure_view_read_only(
    view: &std::path::Path,
    grants: &[provenant_confinement::permissions::WriteGrant],
) -> Result<(), String> {
    use provenant_confinement::permissions::PathGrant;
    if grants.iter().any(|grant| {
        overlaps_source(
            view,
            &PathGrant {
                path: grant.path.clone(),
                scope: grant.scope,
            },
        )
    }) {
        return Err("a provider write grant overlaps the pinned source view".into());
    }
    Ok(())
}

/// Materialise a pinned Git commit into an attempt-private, read-only view.
/// The Git object reader is host-owned and applies the same bounded repository
/// checks as the ordinary read-only preparation path.
pub(super) fn snapshot_source(
    registry: &super::Registry<'_>,
    task: &super::run::TaskRequest,
    paths: &super::run::LaunchPaths,
    source: &std::path::Path,
    git: &std::path::Path,
) -> Result<PathBuf, String> {
    snapshot_source_inner(registry, task, paths, source, git).map_err(|error| {
        match error.as_str() {
            "pinned source tree is unavailable"
            | "pinned source object is unavailable"
            | "pinned source preparation timed out"
            | "source tree is too large"
            | "source object is too large"
            | "source tree contains an unsupported entry"
            | "source tree contains an unsupported object"
            | "unsafe source symlink"
            | "source tree contains a reserved Git path" => error,
            _ => "pinned source view could not be constructed".into(),
        }
    })
}

fn snapshot_source_inner(
    registry: &super::Registry<'_>,
    task: &super::run::TaskRequest,
    paths: &super::run::LaunchPaths,
    source: &std::path::Path,
    git: &std::path::Path,
) -> Result<PathBuf, String> {
    use provenant_confinement::utility::GitObjectKind;
    use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime, run_utility};
    use provenant_core::{Clock, WaitBudget};
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use provenant_os::fs::{Directory, PublishMode};
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt, time::Duration};

    let roots = registry.roots.ok_or("routing context is unavailable")?;
    let project = registry
        .project_id
        .ok_or("selected project identity is unavailable")?;
    let head = task
        .head
        .as_deref()
        .ok_or("graph source head missing")?
        .parse::<GitOid>()
        .map_err(|_| "invalid graph source head")?;
    let host_home = super::host_git_home(roots.state.as_path())?;
    let utility = UtilityRuntime::new(git, host_home.path()).map_err(|error| error.to_string())?;
    let executor = SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let clock = provenant_os::clock::SystemClock::new();
    let deadline = WaitBudget::cli(clock.mono_now(), 120).map_err(|error| error.to_string())?;
    let remaining = || -> Result<Duration, String> {
        let duration = Duration::from_micros(deadline.remaining(clock.mono_now()).as_micros());
        if duration.is_zero() {
            Err("pinned source preparation timed out".into())
        } else {
            Ok(duration)
        }
    };
    let budget = || -> Result<UtilityBudget, String> {
        let remaining = remaining()?;
        UtilityBudget::new(
            remaining.min(Duration::from_secs(30)),
            Duration::from_secs(2),
            Duration::from_secs(2),
            64 * 1024 * 1024,
            1024 * 1024,
        )
        .map_err(|error| error.to_string())
    };
    let local_cancel = provenant_core::CancelToken::new();
    let cancel = registry.cancel.unwrap_or(&local_cancel);
    let repo = utility
        .open_repo(&executor, source, budget()?, cancel)
        .map_err(|error| error.to_string())?;
    let location = provenant_config::ProjectLocation::new(repo.common_git_dir())
        .map_err(|error| error.to_string())?;
    let binding = provenant_config::lookup_project(roots, &location)
        .map_err(|error| error.to_string())?
        .ok_or("dispatch project binding missing")?;
    if binding.project_id() != project {
        return Err("source repository does not match dispatch project".into());
    }
    let tree = run_utility(
        &executor,
        GitRead::AllTreeEntries { oid: head },
        &repo,
        &utility,
        budget()?,
        cancel,
    )
    .map_err(|error| error.to_string())?;
    if tree.termination != Termination::Exit(0) {
        return Err("pinned source tree is unavailable".into());
    }
    let entries = parse_source_tree(&tree.stdout.bytes).map_err(str::to_owned)?;
    let temporary = Directory::open_anchor(&paths.temporary).map_err(|error| error.to_string())?;
    let nonce = provenant_os::fs::entropy_128().map_err(|error| error.to_string())?;
    let name = format!("source-{}", provenant_core::hex(&nonce));
    temporary
        .mkdir(OsStr::new(&name), 0o700)
        .map_err(|error| error.to_string())?;
    let root = temporary
        .open_verified_directory(OsStr::new(&name))
        .map_err(|error| error.to_string())?;
    materialise_source_tree(&root, &entries, |oid| {
        let object = run_utility(
            &executor,
            GitRead::RawObject {
                kind: GitObjectKind::Blob,
                oid,
            },
            &repo,
            &utility,
            budget()?,
            cancel,
        )
        .map_err(|error| error.to_string())?;
        if object.termination != Termination::Exit(0) {
            return Err("pinned source object is unavailable".into());
        }
        Ok(object.stdout.bytes)
    })?;
    // Give Git-aware tools an isolated detached HEAD and access to the same
    // verified object store. No branch or mutable checkout index is copied.
    let objects = repo.common_git_dir().join("objects");
    if objects.as_os_str().as_bytes().contains(&b'\n')
        || objects.as_os_str().as_bytes().contains(&b'\r')
    {
        return Err("source object location cannot be represented safely".into());
    }
    root.mkdir(OsStr::new(".git"), 0o700)
        .map_err(|error| error.to_string())?;
    let git_dir = root
        .open_verified_directory(OsStr::new(".git"))
        .map_err(|error| error.to_string())?;
    git_dir
        .mkdir(OsStr::new("objects"), 0o700)
        .map_err(|error| error.to_string())?;
    git_dir
        .mkdir(OsStr::new("refs"), 0o700)
        .map_err(|error| error.to_string())?;
    let objects_dir = git_dir
        .open_verified_directory(OsStr::new("objects"))
        .map_err(|error| error.to_string())?;
    objects_dir
        .mkdir(OsStr::new("info"), 0o700)
        .map_err(|error| error.to_string())?;
    let info = objects_dir
        .open_verified_directory(OsStr::new("info"))
        .map_err(|error| error.to_string())?;
    let mut alternate = objects.as_os_str().as_bytes().to_vec();
    alternate.push(b'\n');
    info.atomic_write(
        OsStr::new("alternates"),
        ".source-",
        &alternate,
        PublishMode::NoReplace,
    )
    .map_err(|error| error.to_string())?;
    git_dir
        .atomic_write(
            OsStr::new("HEAD"),
            ".source-",
            format!("{head}\n").as_bytes(),
            PublishMode::NoReplace,
        )
        .map_err(|error| error.to_string())?;
    let config = if matches!(head, GitOid::Sha256(_)) {
        "[core]\n\trepositoryformatversion = 1\n\tbare = false\n[extensions]\n\tobjectFormat = sha256\n"
    } else {
        "[core]\n\trepositoryformatversion = 0\n\tbare = false\n"
    };
    git_dir
        .atomic_write(
            OsStr::new("config"),
            ".source-",
            config.as_bytes(),
            PublishMode::NoReplace,
        )
        .map_err(|error| error.to_string())?;
    // Populate only the synthetic index. The synthetic Git configuration is
    // host-written, and this process receives no user configuration or env.
    let snapshot = paths.temporary.join(name);
    let argv: Vec<OsString> = vec![
        git.as_os_str().to_owned(),
        "--git-dir".into(),
        snapshot.join(".git").into_os_string(),
        "--work-tree".into(),
        snapshot.clone().into_os_string(),
        "-c".into(),
        "core.hooksPath=/dev/null".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-c".into(),
        "core.sparseCheckout=false".into(),
        "read-tree".into(),
        head.to_string().into(),
    ];
    let env: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), host_home.path().as_os_str().to_owned()),
        ("TMPDIR".into(), host_home.path().as_os_str().to_owned()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
        ("GIT_GRAFT_FILE".into(), "/dev/null".into()),
        ("GIT_NO_LAZY_FETCH".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_ATTR_NOSYSTEM".into(), "1".into()),
    ];
    let mut options = ExecOptions::new(
        &argv,
        host_home.path(),
        remaining()?.min(Duration::from_secs(30)),
    );
    options.env = Some(&env);
    options.cancel = Some(cancel);
    repo.revalidate().map_err(|error| error.to_string())?;
    let indexed = executor
        .execute(options)
        .map_err(|error| error.to_string())?;
    repo.revalidate().map_err(|error| error.to_string())?;
    if indexed.termination != Termination::Exit(0) || indexed.timed_out || indexed.drain_truncated {
        return Err("pinned source index could not be constructed".into());
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_tree_materialisation_is_independent_of_a_moving_checkout()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let checkout = temp.path().join("checkout");
        let snapshot = temp.path().join("snapshot");
        std::fs::create_dir(&checkout)?;
        std::fs::create_dir(&snapshot)?;
        std::fs::set_permissions(&snapshot, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(checkout.join("subject"), "later checkout")?;
        let oid = "1".repeat(40);
        let listing = format!("100644 blob {oid} 14\tsubject\0100644 blob {oid} 14\tCargo.lock\0");
        let entries = parse_source_tree(listing.as_bytes())?;
        let root = provenant_os::fs::Directory::open_anchor(&snapshot)?;
        materialise_source_tree(&root, &entries, |_| Ok(b"pinned subject".to_vec()))?;
        std::fs::rename(&checkout, temp.path().join("moved"))?;
        std::fs::create_dir(&checkout)?;
        std::fs::write(checkout.join("subject"), "replacement")?;
        assert_eq!(std::fs::read(snapshot.join("subject"))?, b"pinned subject");
        let unsafe_link = format!("120000 blob {oid} 9\tlink\0");
        let entries = parse_source_tree(unsafe_link.as_bytes())?;
        assert!(materialise_source_tree(&root, &entries, |_| Ok(b"../escape".to_vec())).is_err());
        Ok(())
    }

    #[test]
    fn source_tree_symlinks_stay_inside_through_chains() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        // Expanding a directory link before .. is necessary to detect escape.
        for (links, safe) in [
            (vec![("src/current", "../shared/current")], true),
            (vec![("src/current", "../../outside")], false),
            (
                vec![("src/current", "../alias"), ("alias", "shared/current")],
                true,
            ),
            (
                vec![("src/current", "../alias"), ("alias", "../outside")],
                false,
            ),
            (
                vec![("dir", "nested/deeper"), ("link", "dir/../../../outside")],
                false,
            ),
            (vec![("a", "b"), ("b", "a")], false),
            (vec![("link", "/outside")], false),
        ] {
            let temp = tempfile::tempdir()?;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
            std::fs::create_dir(temp.path().join("shared"))?;
            std::fs::write(temp.path().join("shared/current"), b"inside")?;
            let root = provenant_os::fs::Directory::open_anchor(temp.path())?;
            let mut blobs = HashMap::new();
            let mut listing = String::new();
            for (index, (path, target)) in links.iter().enumerate() {
                let oid = format!("{:040x}", index + 1);
                listing.push_str(&format!("120000 blob {oid} {}\t{path}\0", target.len()));
                blobs.insert(oid.parse::<GitOid>()?, target.as_bytes().to_vec());
            }
            let entries = parse_source_tree(listing.as_bytes())?;
            let result = materialise_source_tree(&root, &entries, |oid| {
                blobs
                    .get(&oid)
                    .cloned()
                    .ok_or_else(|| "missing blob".into())
            });
            assert_eq!(result.is_ok(), safe, "{links:?}: {result:?}");
            if safe {
                assert_eq!(std::fs::read(temp.path().join("src/current"))?, b"inside");
                for (path, target) in links {
                    assert_eq!(
                        std::fs::read_link(temp.path().join(path))?,
                        PathBuf::from(target)
                    );
                }
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn source_tree_refuses_case_aliased_directory_escape() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let root = provenant_os::fs::Directory::open_anchor(temp.path())?;
        let first = "1".repeat(40);
        let second = "2".repeat(40);
        let listing = format!("120000 blob {first} 1\tdir\0120000 blob {second} 14\tlink\0");
        let entries = parse_source_tree(listing.as_bytes())?;
        let result = materialise_source_tree(&root, &entries, |oid| {
            if oid.to_string() == first {
                Ok(b".".to_vec())
            } else {
                Ok(b"DIR/../outside".to_vec())
            }
        });
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn source_tree_refuses_grants_that_reopen_the_live_checkout() {
        use provenant_confinement::permissions::{PathGrant, PathScope, ProviderProfileSpec};
        let source = std::path::Path::new("/fixture/project/writer");
        let mut access = super::super::run::LaneAccess::default();
        assert!(
            ensure_source_roots(source, &access, std::path::Path::new("/fixture/assets")).is_ok()
        );
        access.read_roots.push("/fixture/project".into());
        assert!(
            ensure_source_roots(source, &access, std::path::Path::new("/fixture/assets")).is_err()
        );
        access.read_roots.clear();
        assert!(ensure_source_roots(source, &access, source).is_err());
        let mut profile = ProviderProfileSpec::default();
        profile.runtime_reads.push(PathGrant {
            path: "/fixture/project".into(),
            scope: PathScope::Exact,
        });
        assert!(ensure_profile_isolated(source, &profile).is_ok());
        profile.runtime_reads[0].scope = PathScope::Subtree;
        assert!(ensure_profile_isolated(source, &profile).is_err());
        profile.runtime_reads[0] = PathGrant {
            path: "/fixture/project/writer/file".into(),
            scope: PathScope::Exact,
        };
        assert!(ensure_profile_isolated(source, &profile).is_err());
        let view = std::path::Path::new("/fixture/state/source");
        let mut write = provenant_confinement::WriteGrant {
            source: provenant_confinement::WriteSource::Scratch,
            path: "/fixture/state/scratch".into(),
            scope: PathScope::Subtree,
        };
        assert!(ensure_view_read_only(view, &[write.clone()]).is_ok());
        write.path = "/fixture/state".into();
        assert!(ensure_view_read_only(view, &[write]).is_err());
    }

    #[test]
    fn source_tree_rejects_traversal_duplicates_and_unsupported_objects() {
        let oid = "1".repeat(40);
        let record =
            |mode: &str, kind: &str, name: &str| format!("{mode} {kind} {oid} 3\t{name}\0");
        let valid = format!(
            "{}{}",
            record("100644", "blob", "src/a"),
            record("100755", "blob", "run")
        );
        let parsed = parse_source_tree(valid.as_bytes()).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path.as_path(), std::path::Path::new("src/a"));
        assert_eq!(parsed[0].size, 3);
        assert_eq!(parsed[0].oid.to_string(), oid);
        assert_eq!(parsed[1].kind, SourceKind::Executable);
        for bad in [
            record("100644", "blob", "../escape"),
            record("100644", "blob", ".GiT/config"),
            record("160000", "commit", "submodule"),
            format!(
                "{}{}",
                record("100644", "blob", "same"),
                record("100644", "blob", "same")
            ),
        ] {
            assert!(parse_source_tree(bad.as_bytes()).is_err(), "{bad:?}");
        }
        assert!(parse_source_tree(&valid.as_bytes()[..valid.len() - 1]).is_err());
    }
}
