use super::*;
use crate::cache::verify_cache_metadata;
use crate::repo::GitRefName;
#[cfg(target_os = "macos")]
use provenant_core::CancelToken;
#[cfg(target_os = "macos")]
use provenant_os::exec::Termination;
use provenant_os::fs::Directory;
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn write_grants_fixture() -> (PlanPermissions, ProviderProfile) {
    let grant = |path: &str, scope| PathGrant {
        path: path.into(),
        scope,
    };
    let plan = PlanPermissions {
        read_only: Some(crate::plan::ReadOnlyGrants::for_test(
            "/fixture/repo".into(),
            vec![grant("/fixture/repo/locks", PathScope::Subtree)],
        )),
        mode: AccessMode::ReadOnly,
        scratch: grant("/fixture/scratch", PathScope::Subtree),
        private_state: Some(grant("/fixture/private", PathScope::Subtree)),
        read_roots: Vec::new(),
        protected_roots: Vec::new(),
        secret_roots: Vec::new(),
        selected_authentication: Some("selected".into()),
        include_continuation: false,
        write_roots: vec![grant("/fixture/shared", PathScope::Subtree)],
        network: NetworkAccess::Provider,
        nested_confinement: false,
    };
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            state_writes: vec![grant("/fixture/state/session", PathScope::Exact)],
            tool_writes: vec![grant("/fixture/tools", PathScope::Subtree)],
            authentication: ["unselected", "selected"]
                .map(|mechanism| {
                    let credential = grant(&format!("/fixture/auth/{mechanism}"), PathScope::Exact);
                    AuthenticationProfile {
                        mechanism: mechanism.into(),
                        roots: vec![credential.clone()],
                        in_place_writes: vec![credential],
                    }
                })
                .into(),
            ..ProviderProfileSpec::default()
        },
        observations: BTreeMap::new(),
    };
    (plan, profile)
}

#[test]
fn write_grants_list_every_write_source() {
    let (mut plan, profile) = write_grants_fixture();
    let grant = |source, path: &str, scope| WriteGrant {
        source,
        path: path.into(),
        scope,
    };
    let mut expected = vec![
        grant(
            WriteSource::StateWrite,
            "/fixture/state/session",
            PathScope::Exact,
        ),
        grant(
            WriteSource::CredentialWrite,
            "/fixture/auth/selected",
            PathScope::Exact,
        ),
        grant(
            WriteSource::WriteRoot,
            "/fixture/shared",
            PathScope::Subtree,
        ),
        grant(WriteSource::ToolWrite, "/fixture/tools", PathScope::Subtree),
        grant(WriteSource::Scratch, "/fixture/scratch", PathScope::Subtree),
        grant(
            WriteSource::PrivateState,
            "/fixture/private",
            PathScope::Subtree,
        ),
    ];
    let mut read_only_expected = expected.clone();
    read_only_expected.push(grant(
        WriteSource::ReadOnlyException,
        "/fixture/repo/locks",
        PathScope::Subtree,
    ));
    assert_eq!(write_grants(&plan, &profile), read_only_expected);

    plan.read_only = None;
    plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
        git_grants_digest: [7; 32],
        publishing: true,
        common_git_dir: Some("/fixture/git".into()),
        cargo_cache: PathGrant {
            path: "/fixture/cache".into(),
            scope: PathScope::Subtree,
        },
    }));
    expected.push(grant(
        WriteSource::LaneCache,
        "/fixture/cache",
        PathScope::Subtree,
    ));
    assert_eq!(write_grants(&plan, &profile), expected);

    plan.selected_authentication = None;
    assert!(
        !write_grants(&plan, &profile)
            .iter()
            .any(|g| g.source == WriteSource::CredentialWrite)
    );
}

#[test]
fn writer_grants_do_not_include_shared_branch_paths() {
    let common = Path::new("/fixture/git");
    let grants = WorktreeWriteGrants::from_paths(
        Path::new("/fixture/lane"),
        Path::new("/fixture/git/worktrees/lane"),
        common,
        "lane/test",
    );
    for relative in [
        "refs/heads/lane/test",
        "refs/heads/lane/test.lock",
        "logs/refs/heads/lane/test",
        "logs/refs/heads/lane/test.lock",
    ] {
        assert!(
            !grants
                .iter()
                .any(|grant| grant.path == common.join(relative))
        );
    }
    let other = WorktreeWriteGrants::from_paths(
        Path::new("/fixture/lane"),
        Path::new("/fixture/git/worktrees/lane"),
        common,
        "lane/other",
    );
    let cache = PathGrant {
        path: "/fixture/cache".into(),
        scope: PathScope::Subtree,
    };
    assert_ne!(grants.digest(&cache), other.digest(&cache));
}

#[cfg(target_os = "macos")]
pub(super) fn worktree_grants_fixture() -> WorktreeWriteGrants {
    WorktreeWriteGrants::from_paths(
        Path::new("/fixture/repo"),
        Path::new("/fixture/git/private"),
        Path::new("/fixture/git"),
        "fixture",
    )
}

#[cfg(target_os = "macos")]
#[test]
fn writer_test_grants_stay_within_the_attempt_and_mode() -> Result<(), Box<dyn std::error::Error>> {
    let (mut plan, profile) = write_grants_fixture();
    let scratch = "/fixture/runs/run/attempts/own/tmp/candidate-0";
    plan.scratch.path = scratch.into();
    let git = worktree_grants_fixture();
    for writer in [false, true] {
        if writer {
            plan.read_only = None;
            plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
                git_grants_digest: [7; 32],
                publishing: true,
                common_git_dir: Some("/fixture/git".into()),
                cargo_cache: PathGrant {
                    path: "/fixture/cache".into(),
                    scope: PathScope::Subtree,
                },
            }));
        }
        let rendered = String::from_utf8(render_profile(
            &plan,
            &profile,
            &[],
            &[],
            writer.then_some(&git),
            Path::new("/fixture/home"),
        )?)?;
        // Every ordinary writer/reviewer profile keeps database IPC denied.
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("(allow ipc-"))
                .collect::<Vec<_>>(),
            ["(allow ipc-posix-shm-read* (ipc-posix-name-prefix \"apple.cfprefs.\"))",]
        );
        let mut database = ProviderProfile {
            spec: profile.spec.clone(),
            observations: BTreeMap::new(),
        };
        database.spec.allow_database_ipc = true;
        let opted_in = String::from_utf8(render_profile(
            &plan,
            &database,
            &[],
            &[],
            writer.then_some(&git),
            Path::new("/fixture/home"),
        )?)?;
        assert!(opted_in.contains("(allow ipc-sysv-shm)\n(allow ipc-sysv-sem)\n"));
        assert!(!rendered.contains("SYS_shmget") && !rendered.contains("SYS_msgget"));
        let exec = format!("(allow process-exec (subpath \"{scratch}\"))");
        assert_eq!(rendered.lines().any(|line| line == exec), writer);
        assert!(rendered.contains(&format!("(allow file-read* (subpath \"{scratch}\"))")));
        for outside in [
            "/fixture/runs",
            "/fixture/runs/run",
            "/fixture/runs/run/attempts/own",
            "/fixture/runs/run/attempts/own/tmp",
            "/fixture/runs/run/attempts/own/tmp/candidate-1",
            "/fixture/runs/run/attempts/other/tmp/candidate-0",
        ] {
            for operation in ["process-exec", "file-read*"] {
                assert!(
                    !rendered.contains(&format!("(allow {operation} (subpath \"{outside}\"))")),
                    "{outside}"
                );
            }
        }
        assert!(!rendered.contains("(allow process-exec (subpath \"/fixture/private\"))"));
        let signals: Vec<_> = rendered
            .lines()
            .filter(|line| line.starts_with("(allow signal"))
            .collect();
        assert_eq!(signals, ["(allow signal (target same-sandbox))"]);
        assert!(rendered.contains("(deny sysctl-read (sysctl-name-prefix \"kern.procargs\") (sysctl-name-prefix \"kern.proc.\"))"));
        assert!(
            rendered.contains("(deny process-info*)\n(allow process-info* (target same-sandbox))")
        );
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("(allow process-info"))
                .collect::<Vec<_>>(),
            ["(allow process-info* (target same-sandbox))"]
        );
        assert_eq!(
            rendered.contains("(allow file-read* (subpath \"/dev/fd\"))"),
            writer
        );
        assert!(!rendered.contains("(allow file-read* (subpath \"/dev\"))"));
        assert!(!rendered.contains("(allow file-write* (subpath \"/dev/fd\"))"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn write_grants_cover_every_rendered_file_write() -> Result<(), Box<dyn std::error::Error>> {
    let (mut plan, profile) = write_grants_fixture();
    let git = worktree_grants_fixture();
    let subtree = |path: &str| PathGrant {
        path: path.into(),
        scope: PathScope::Subtree,
    };
    for writer in [false, true] {
        if writer {
            plan.read_only = None;
            plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
                git_grants_digest: [7; 32],
                publishing: true,
                common_git_dir: Some("/fixture/git".into()),
                cargo_cache: subtree("/fixture/cache"),
            }));
        }
        let credentials = &profile.spec.authentication[1].in_place_writes;
        let rendered = String::from_utf8(render_profile(
            &plan,
            &profile,
            &[],
            credentials,
            writer.then_some(&git),
            Path::new("/fixture/home"),
        )?)?;
        let grants = write_grants(&plan, &profile);
        let mut checked = 0;
        for line in rendered.lines().filter(|line| {
            line.starts_with("(allow ")
                && (line.contains("file-write*") || line.contains("file-write-data"))
        }) {
            let (scope, tail) = if let Some((_, tail)) = line.split_once("(subpath \"") {
                (PathScope::Subtree, tail)
            } else if let Some((_, tail)) = line.split_once("(literal \"") {
                (PathScope::Exact, tail)
            } else {
                return Err(format!("unrecognised write rule: {line}").into());
            };
            let path = Path::new(tail.split('"').next().ok_or("write rule path")?);
            if path == Path::new("/dev/null") || writer && git.iter().any(|g| g.path == path) {
                continue;
            }
            assert!(
                grants.iter().any(|g| g.path == path && g.scope == scope),
                "unrecorded rule: {line}"
            );
            checked += 1;
        }
        assert_eq!(checked, 7);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn serialised_digest_cannot_authorise_without_a_verified_worktree() {
    let authority = WorktreeWriteAuthority {
        git_grants_digest: [0; 32],
        publishing: true,
        common_git_dir: Some("/fixture/git".into()),
        cargo_cache: PathGrant {
            path: "/fixture/home/Library/Caches/provenant-lane-cargo/fixture".into(),
            scope: PathScope::Subtree,
        },
    };

    let directories = [];
    let anchors = Anchors {
        home: Path::new("/fixture/home"),
        directories: &directories,
        worktree: None,
    };
    assert!(matches!(
        verify_worktree_authority(&authority, &anchors),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));
}

#[test]
#[allow(clippy::disallowed_methods)]
fn concurrent_writers_prepare_their_caches_in_one_home() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = crate::test_support::Fixture::new()?;
    let home = provenant_os::fs::canonicalize(&fixture.home)?;
    let writers = (0..8)
        .map(|index| -> Result<_, Box<dyn std::error::Error>> {
            let branch = format!("concurrent-cache-{index}");
            let path = fixture._temp.path().join(&branch);
            fixture.run(&[
                "worktree",
                "add",
                "-b",
                &branch,
                path.to_str().ok_or("path")?,
            ])?;
            let worktree = crate::repo::verify_worktree(&fixture.repo, &path)?;
            let branch = GitRefName::branch(&branch)?;
            let authority = WorktreeWriteAuthority::for_worktree(&worktree, &branch, &home)?;
            Ok((worktree, branch, authority))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Repeat with missing shared parents to exercise the mkdir race rather
    // than only the already-created lane cache paths.
    for _ in 0..8 {
        let shared = home.join("Library");
        if shared.exists() {
            std::fs::remove_dir_all(&shared)?;
        }
        let ready = std::sync::Barrier::new(writers.len());
        std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
            let threads = writers
                .iter()
                .map(|(worktree, branch, authority)| {
                    let ready = &ready;
                    let home = &home;
                    scope.spawn(move || {
                        ready.wait();
                        prepare_writer_directories(worktree, branch, authority, home)
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread
                    .join()
                    .map_err(|_| "directory preparation panicked")??;
            }
            Ok(())
        })?;
        for (_, _, authority) in &writers {
            let cache = Directory::open_anchor(&authority.cargo_cache.path)?;
            verify_cache_metadata(&cache.metadata()?)?;
            assert!(
                authority
                    .cargo_cache
                    .path
                    .join(crate::cache::CARGO_CACHE_OWNER)
                    .is_file()
            );
        }
    }
    Ok(())
}

#[test]
fn writer_cache_is_private_and_bound_to_repository_and_worktree_identity()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::repo::verify_worktree;
    use std::os::unix::fs::PermissionsExt;

    let fixture = crate::test_support::Fixture::new()?;
    let other = crate::test_support::Fixture::new()?;
    let selected_ref = GitRefName::branch("cache-test")?;
    let add = |fixture: &crate::test_support::Fixture, path: &Path, branch: &str| {
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            branch,
            path.to_str().ok_or("path")?,
        ])?;
        Ok::<_, Box<dyn std::error::Error>>(verify_worktree(&fixture.repo, path)?)
    };
    let path = fixture._temp.path().join("package");
    let linked = add(&fixture, &path, "cache-test")?;
    let home = provenant_os::fs::canonicalize(&fixture.home)?;
    let authority = WorktreeWriteAuthority::for_worktree(&linked, &selected_ref, &home)?;
    let cache = &authority.cargo_cache.path;
    assert_eq!(
        cache.parent(),
        Some(home.join("Library/Caches/provenant-lane-cargo").as_path())
    );
    assert_eq!(cache.file_name().ok_or("cache name")?.len(), 64);
    assert_eq!(
        authority,
        WorktreeWriteAuthority::for_worktree(&linked, &selected_ref, &home)?
    );

    let other_linked = add(&other, &other._temp.path().join("package"), "cache-test")?;
    assert_ne!(
        *cache,
        WorktreeWriteAuthority::for_worktree(&other_linked, &selected_ref, &home)?
            .cargo_cache
            .path
    );
    let sibling_path = fixture._temp.path().join("nested/package");
    std::fs::create_dir(sibling_path.parent().ok_or("parent")?)?;
    let sibling = add(&fixture, &sibling_path, "sibling")?;
    assert_ne!(
        *cache,
        WorktreeWriteAuthority::for_worktree(&sibling, &GitRefName::branch("sibling")?, &home)?
            .cargo_cache
            .path
    );

    prepare_writer_directories(&linked, &selected_ref, &authority, &home)?;
    prepare_writer_directories(&linked, &selected_ref, &authority, &home)?;
    let mut metadata = Directory::open_anchor(cache)?.metadata()?;
    verify_cache_metadata(&metadata)?;
    metadata.uid = metadata.uid.wrapping_add(1);
    assert!(matches!(
        verify_cache_metadata(&metadata),
        Err(PermissionError::AnchorMismatch)
    ));
    for mode in [0o755, 0o770, 0o1700] {
        std::fs::set_permissions(cache, std::fs::Permissions::from_mode(mode))?;
        assert!(
            matches!(
                prepare_writer_directories(&linked, &selected_ref, &authority, &home),
                Err(PermissionError::AnchorMismatch)
            ),
            "mode {mode:o}"
        );
    }
    std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))?;
    let namespace = cache.parent().ok_or("namespace")?;
    std::fs::set_permissions(namespace, std::fs::Permissions::from_mode(0o755))?;
    assert!(matches!(
        prepare_writer_directories(&linked, &selected_ref, &authority, &home),
        Err(PermissionError::AnchorMismatch)
    ));
    std::fs::set_permissions(namespace, std::fs::Permissions::from_mode(0o700))?;

    let moved = fixture._temp.path().join("retired");
    fixture.run(&[
        "worktree",
        "move",
        path.to_str().ok_or("path")?,
        moved.to_str().ok_or("path")?,
    ])?;
    let recreated = add(&fixture, &path, "recreated")?;
    assert_ne!(
        *cache,
        WorktreeWriteAuthority::for_worktree(&recreated, &GitRefName::branch("recreated")?, &home)?
            .cargo_cache
            .path
    );
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn linked_worktree_grants_derive_from_verified_git_paths_and_ref()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::repo::verify_worktree;
    use crate::utility::{GitMutation, execute_git_mutation};

    let fixture = crate::test_support::Fixture::new()?;
    let head = fixture.head()?;
    let selected_ref = GitRefName::branch("s3/authority")?;
    let worktree_path = fixture._temp.path().join("linked");
    let added = execute_git_mutation(
        &fixture.executor,
        &fixture.repo,
        &fixture.runtime,
        GitMutation::WorktreeAdd {
            branch: selected_ref.clone(),
            base: Some(head),
            path: worktree_path.clone(),
            create_branch: true,
        },
        fixture.budget()?,
        &CancelToken::new(),
    )?;
    assert_eq!(added.termination, Termination::Exit(0));
    let linked = verify_worktree(&fixture.repo, &worktree_path)?;
    let private = linked.repo().private_git_dir().to_owned();
    let common = linked.repo().common_git_dir().to_owned();
    assert_ne!(private, common);

    let authority = WorktreeWriteAuthority::for_worktree(
        &linked,
        &selected_ref,
        &provenant_os::fs::canonicalize(&fixture.home)?,
    )?;
    fixture.run(&["pack-refs", "--all"])?;
    let missing_parent = common.join("refs/heads/s3");
    if missing_parent.exists() {
        std::fs::remove_dir(&missing_parent)?;
    }
    assert!(!missing_parent.exists());

    prepare_writer_directories(
        &linked,
        &selected_ref,
        &authority,
        &provenant_os::fs::canonicalize(&fixture.home)?,
    )?;
    let grants = WorktreeWriteGrants::derive(&linked, &selected_ref);
    assert_eq!(
        authority.git_grants_digest,
        grants.digest(&authority.cargo_cache)
    );
    assert_eq!(grants.common_git_dir.path, common);
    assert_eq!(grants.private_git_dir.path, private);
    assert!(!missing_parent.exists());

    let common_heads = common.join("refs/heads/s3");
    let common_logs = common.join("logs/refs/heads/s3");
    let private_logs = private.join("logs");
    for directory in [&common_heads, &common_logs, &private_logs] {
        std::fs::create_dir_all(directory)?;
    }
    let anchor_paths = [
        linked.path().to_owned(),
        private.clone(),
        common.clone(),
        common.join("info"),
        common.join("objects"),
        authority.cargo_cache.path.clone(),
        private_logs.clone(),
        common_heads,
        common_logs,
    ];

    let directories = anchor_paths
        .iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: &provenant_os::fs::canonicalize(&fixture.home)?,
        directories: &directories,
        worktree: Some((&linked, &selected_ref)),
    };
    assert!(missing_parent.is_dir());
    let wrong_home = Anchors {
        home: Path::new("/different/home"),
        ..anchors
    };
    assert!(matches!(
        verify_worktree_authority(&authority, &wrong_home),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));
    let verified_grants = verify_worktree_authority(&authority, &anchors)?;
    let mut source = String::new();
    push_worktree_rule(&mut source, &verified_grants)?;
    assert!(source.contains("(require-not (subpath "));
    for directory in [&common, &private] {
        assert!(source.contains(&format!(
            "(deny file-write* (subpath \"{}\"))",
            directory.display()
        )));
    }
    let git_writes: Vec<_> = source
        .lines()
        .filter(|line| line.starts_with("(allow file-write* (literal "))
        .collect();
    assert!(git_writes.is_empty());
    for relative in [
        "config",
        "config.lock",
        "packed-refs",
        "refs/heads/main",
        "refs/remotes",
        "refs/remotes/origin/s3/authority",
        "refs/remotes/origin/s3/authority.lock",
        "refs/remotes/upstream",
        "logs/refs/remotes",
        "logs/refs/remotes/origin/s3/authority",
        "logs/refs/remotes/origin/s3/authority.lock",
        "hooks",
    ] {
        assert!(
            !source.contains(&format!("\"{}\"", common.join(relative).display())),
            "{relative}"
        );
    }

    let other_ref = GitRefName::branch("s3/other")?;
    let mismatched = Anchors {
        home: &provenant_os::fs::canonicalize(&fixture.home)?,
        directories: &directories,
        worktree: Some((&linked, &other_ref)),
    };
    assert!(matches!(
        verify_worktree_authority(&authority, &mismatched),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));

    let mut tampered = authority.clone();
    tampered.git_grants_digest[0] ^= 1;
    assert!(matches!(
        verify_worktree_authority(&tampered, &anchors),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));

    for common_git_dir in [None, Some(fixture._temp.path().join("other-git"))] {
        let mut tampered = authority.clone();
        tampered.common_git_dir = common_git_dir;
        assert!(matches!(
            verify_worktree_authority(&tampered, &anchors),
            Err(PermissionError::GitWriteAuthorityMissing)
        ));
    }

    let original_head = std::fs::read(private.join("HEAD"))?;
    std::fs::write(private.join("HEAD"), b"ref: refs/heads/s3/other\n")?;
    assert!(matches!(
        verify_worktree_authority(
            &WorktreeWriteAuthority::for_worktree(
                &linked,
                &selected_ref,
                &provenant_os::fs::canonicalize(&fixture.home)?
            )?,
            &anchors
        ),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));
    std::fs::write(private.join("HEAD"), original_head)?;
    verify_worktree_authority(&authority, &anchors)?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn scratch_cannot_promote_git_or_read_authority_to_writes() -> Result<(), Box<dyn std::error::Error>>
{
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let root = provenant_os::fs::canonicalize(temp.path())?;

    let scratch_path = root.join("scratch");
    std::fs::create_dir(&scratch_path)?;
    std::fs::set_permissions(&scratch_path, std::fs::Permissions::from_mode(0o700))?;
    let scratch = PathGrant {
        path: scratch_path.clone(),
        scope: PathScope::Subtree,
    };
    let directories = [AnchoredDirectory::open_canonical(&scratch_path)?];
    let anchors = Anchors {
        home: Path::new("/fixture/home"),
        directories: &directories,

        worktree: None,
    };
    verify_private_scratch(&scratch, &[], &anchors)?;
    let common = PathGrant {
        path: root.clone(),
        scope: PathScope::Subtree,
    };
    assert!(matches!(
        verify_private_scratch(&scratch, &[&common], &anchors),
        Err(PermissionError::ProtectedOverlap)
    ));
    let read_root = PathGrant {
        path: scratch_path.join("read-only"),
        scope: PathScope::Subtree,
    };
    assert!(matches!(
        verify_private_scratch(&scratch, &[&read_root], &anchors),
        Err(PermissionError::ProtectedOverlap)
    ));
    std::fs::set_permissions(&scratch_path, std::fs::Permissions::from_mode(0o755))?;
    assert!(matches!(
        verify_private_scratch(&scratch, &[], &anchors),
        Err(PermissionError::AnchorMismatchAt { .. })
    ));
    Ok(())
}
