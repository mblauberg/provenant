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
        credential_roots: provenant_core::CredentialRoots::for_home(std::path::Path::new(
            "/fixture/home",
        )),
        policy: None,
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
                        keychain: None,
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
    let (mut plan, mut profile) = write_grants_fixture();
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
        common_git_dir: "/fixture/git".into(),
        git_state: PathGrant {
            path: "/fixture/home/Library/Application Support/provenant/worktree-git/fixture".into(),
            scope: PathScope::Subtree,
        },
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
    expected.extend(["git-common", "git-objects"].map(|name| {
        grant(
            WriteSource::GitState,
            &format!(
                "/fixture/home/Library/Application Support/provenant/worktree-git/fixture/{name}"
            ),
            PathScope::Subtree,
        )
    }));
    assert_eq!(write_grants(&plan, &profile), expected);

    // Spec: docs/specs/supervisor.md#permission-policy
    // Disabled authentication is absent from the disclosed write authority,
    // even when a mechanism is selected and recorded by the provider.
    profile.spec.confinement = profile.spec.confinement.clone().without_credentials();
    assert!(
        write_grants(&plan, &profile)
            .iter()
            .all(|g| g.source != WriteSource::CredentialWrite)
    );

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
    let git_state = PathGrant {
        path: "/fixture/home/Library/Application Support/provenant/worktree-git/fixture".into(),
        scope: PathScope::Subtree,
    };
    let other_git_state = PathGrant {
        path: "/fixture/home/Library/Application Support/provenant/worktree-git/other".into(),
        scope: PathScope::Subtree,
    };
    assert_ne!(
        grants.digest(&cache, &git_state),
        other.digest(&cache, &git_state)
    );
    assert_ne!(
        grants.digest(&cache, &git_state),
        grants.digest(&cache, &other_git_state)
    );
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
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
    use super::render::{
        RuleAction, has_rule_atoms, rule_facts, rule_index, rule_index_with_targets,
        rule_index_without_strings,
    };

    let (mut plan, profile) = write_grants_fixture();
    let scratch = "/fixture/attempts/own/tmp/candidate-0";
    plan.scratch.path = scratch.into();
    let private_state = "/fixture/attempts/own/provider-state/candidate-0";
    plan.private_state.as_mut().expect("private state").path = private_state.into();
    plan.policy = Some(crate::run_config::RunConfig::default().policy_for_launch(
        Path::new("/fixture/attempts"),
        MandatoryRoots::new(
            Path::new("/fixture/home"),
            Path::new("/fixture/attempts"),
            Path::new("/fixture/repo"),
        )?,
    ));
    let git = worktree_grants_fixture();
    for writer in [false, true] {
        if writer {
            plan.read_only = None;
            plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
                git_grants_digest: [7; 32],
                publishing: true,
                common_git_dir: "/fixture/git".into(),
                git_state: PathGrant {
                    path:
                        "/fixture/home/Library/Application Support/provenant/worktree-git/fixture"
                            .into(),
                    scope: PathScope::Subtree,
                },
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
        // Ordinary database IPC is ambient and has no grant flags.
        assert!(rule_index(&rendered, RuleAction::Allow, "default", None).is_some());
        assert!(rule_index(&rendered, RuleAction::Deny, "ipc-sysv-shm", None).is_none());
        assert!(rule_index(&rendered, RuleAction::Deny, "ipc-sysv-sem", None).is_none());
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
        assert!(rule_index(&opted_in, RuleAction::Allow, "ipc-sysv-shm", None).is_some());
        assert!(rule_index(&opted_in, RuleAction::Allow, "ipc-sysv-sem", None).is_some());
        assert_eq!(
            rule_index_without_strings(&rendered, RuleAction::Allow, "process-exec").is_some(),
            writer
        );
        assert!(rule_index(&rendered, RuleAction::Allow, "file-read*", Some(scratch)).is_some());
        for operation in ["file-read*", "file-write*", "process-exec"] {
            assert!(
                rule_index_with_targets(
                    &rendered,
                    RuleAction::Deny,
                    operation,
                    &["/fixture/attempts", scratch, private_state],
                )
                .is_some()
            );
        }
        assert!(rule_index(&rendered, RuleAction::Deny, "signal", None).is_some());
        assert!(has_rule_atoms(
            &rendered,
            RuleAction::Allow,
            "signal",
            &["same-sandbox"]
        ));
        assert!(
            rule_facts(&rendered)
                .iter()
                .filter(|rule| {
                    rule.action == RuleAction::Allow
                        && rule.operations.iter().any(|name| name == "signal")
                })
                .all(|rule| rule.atoms.iter().any(|atom| atom == "same-sandbox"))
        );
        assert!(
            rule_index(
                &rendered,
                RuleAction::Deny,
                "sysctl-read",
                Some("kern.procargs")
            )
            .is_some()
        );
        assert!(
            rule_index(
                &rendered,
                RuleAction::Deny,
                "sysctl-read",
                Some("kern.proc.")
            )
            .is_none()
        );
        assert!(rule_index(&rendered, RuleAction::Allow, "process-info*", None).is_some());
        assert!(rule_index(&rendered, RuleAction::Deny, "process-info*", None).is_none());
        assert!(rule_index(&rendered, RuleAction::Deny, "process-info-setcontrol", None).is_some());
        assert!(
            rule_index(
                &rendered,
                RuleAction::Allow,
                "process-info-setcontrol",
                None
            )
            .is_some()
        );
        assert!(has_rule_atoms(
            &rendered,
            RuleAction::Allow,
            "process-info-setcontrol",
            &["process-info-dirtycontrol", "same-sandbox"]
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
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
                common_git_dir: "/fixture/git".into(),
                git_state: PathGrant {
                    path:
                        "/fixture/home/Library/Application Support/provenant/worktree-git/fixture"
                            .into(),
                    scope: PathScope::Subtree,
                },
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
        let baseline_writes = [
            r#"(allow file-read* file-write-data (literal "/dev/null"))"#,
            r#"(allow file-read-metadata file-write-data (literal "/private/var/tmp"))"#,
            r##"(allow file-read* file-write* (regex #"^/private/var/tmp/sh-thd[^/]*$"))"##,
        ];
        for baseline in baseline_writes {
            assert!(rendered.lines().any(|line| line == baseline), "{baseline}");
        }
        let pty_rules: Vec<_> = super::super::render::PSEUDO_TTY_RULES
            .lines()
            .filter(|line| line.contains("file-write*"))
            .collect();
        let allow_pty = writer || profile.spec.allow_pseudo_tty;
        for rule in &pty_rules {
            assert_eq!(rendered.lines().any(|line| line == *rule), allow_pty);
        }
        let grants = write_grants(&plan, &profile);
        let mut checked = 0;
        for line in rendered.lines().filter(|line| {
            line.starts_with("(allow ")
                && (line.contains("file-write*") || line.contains("file-write-data"))
        }) {
            if baseline_writes.contains(&line) {
                continue;
            }
            if allow_pty && pty_rules.contains(&line) {
                continue;
            }
            let (scope, tail) = if let Some((_, tail)) = line.split_once("(subpath \"") {
                (PathScope::Subtree, tail)
            } else if let Some((_, tail)) = line.split_once("(literal \"") {
                (PathScope::Exact, tail)
            } else {
                return Err(format!("unrecognised write rule: {line}").into());
            };
            let path = Path::new(tail.split('"').next().ok_or("write rule path")?);
            if writer && git.iter().any(|g| g.path == path) {
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
        common_git_dir: "/fixture/git".into(),
        git_state: PathGrant {
            path: "/fixture/home/Library/Application Support/provenant/worktree-git/fixture".into(),
            scope: PathScope::Subtree,
        },
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
        verify_worktree_authority(
            &authority,
            &anchors,
            &provenant_core::CredentialRoots::for_home(anchors.home)
        ),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));
}

#[test]
#[allow(clippy::disallowed_methods)]
fn concurrent_writers_prepare_caches_and_shared_git_state_in_one_home()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = crate::test_support::Fixture::new()?;
    let home = provenant_os::fs::canonicalize(&fixture.home)?;
    let credentials = provenant_core::CredentialRoots::for_home(&home);
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
            crate::worktrees::acquire_worktree_subject(worktree.repo())?;
            let branch = GitRefName::branch(&branch)?;
            let authority =
                WorktreeWriteAuthority::for_worktree(&worktree, &branch, &home, &credentials)?;
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
                    let credentials = &credentials;
                    scope.spawn(move || {
                        ready.wait();
                        prepare_writer_directories(worktree, branch, authority, home, credentials)
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
            for path in [
                authority
                    .git_state
                    .path
                    .parent()
                    .ok_or("Git state namespace")?
                    .to_owned(),
                authority.git_state.path.clone(),
                authority.git_state.path.join("git-common"),
                authority.git_state.path.join("git-objects"),
            ] {
                let directory = Directory::open_anchor(&path)?;
                verify_cache_metadata(&directory.metadata()?)?;
            }
        }
    }
    Ok(())
}

#[test]
fn writer_cache_and_shared_git_state_bind_to_repository_and_worktree_identity()
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
        let worktree = verify_worktree(&fixture.repo, path)?;
        crate::worktrees::acquire_worktree_subject(worktree.repo())?;
        Ok::<_, Box<dyn std::error::Error>>(worktree)
    };
    let path = fixture._temp.path().join("package");
    let linked = add(&fixture, &path, "cache-test")?;
    let home = provenant_os::fs::canonicalize(&fixture.home)?;
    let authority = WorktreeWriteAuthority::for_worktree(
        &linked,
        &selected_ref,
        &home,
        &provenant_core::CredentialRoots::for_home(&home),
    )?;
    let cache = &authority.cargo_cache.path;
    let git_state = authority.git_state.path.clone();
    assert_eq!(
        cache.parent(),
        Some(home.join("Library/Caches/provenant-lane-cargo").as_path())
    );
    assert_eq!(cache.file_name().ok_or("cache name")?.len(), 64);
    assert_eq!(git_state.file_name().ok_or("Git state name")?.len(), 64);
    assert_eq!(
        git_state.parent(),
        Some(
            home.join("Library/Application Support/provenant/worktree-git")
                .as_path()
        )
    );
    assert_eq!(
        authority,
        WorktreeWriteAuthority::for_worktree(
            &linked,
            &selected_ref,
            &home,
            &provenant_core::CredentialRoots::for_home(&home)
        )?
    );
    let other_branch_state =
        worktree_git_state_path(&home, &linked, &GitRefName::branch("another-branch")?)?;
    assert_ne!(git_state, other_branch_state);
    let mut credential_roots = provenant_core::CredentialRoots::default();
    credential_roots.add(
        git_state.clone(),
        provenant_core::CredentialScope::Subtree,
        "test secret root",
        provenant_core::CredentialKind::Store,
    );
    assert!(worktree_git_state(&home, &linked, &selected_ref, &credential_roots).is_err());

    let other_linked = add(&other, &other._temp.path().join("package"), "cache-test")?;
    assert_ne!(
        *cache,
        WorktreeWriteAuthority::for_worktree(
            &other_linked,
            &selected_ref,
            &home,
            &provenant_core::CredentialRoots::for_home(&home)
        )?
        .cargo_cache
        .path
    );
    assert_ne!(
        git_state,
        WorktreeWriteAuthority::for_worktree(
            &other_linked,
            &selected_ref,
            &home,
            &provenant_core::CredentialRoots::for_home(&home)
        )?
        .git_state
        .path
    );
    let sibling_path = fixture._temp.path().join("nested/package");
    std::fs::create_dir(sibling_path.parent().ok_or("parent")?)?;
    let sibling = add(&fixture, &sibling_path, "sibling")?;
    assert_ne!(
        *cache,
        WorktreeWriteAuthority::for_worktree(
            &sibling,
            &GitRefName::branch("sibling")?,
            &home,
            &provenant_core::CredentialRoots::for_home(&home)
        )?
        .cargo_cache
        .path
    );
    assert_ne!(
        git_state,
        WorktreeWriteAuthority::for_worktree(
            &sibling,
            &GitRefName::branch("sibling")?,
            &home,
            &provenant_core::CredentialRoots::for_home(&home)
        )?
        .git_state
        .path
    );

    prepare_writer_directories(
        &linked,
        &selected_ref,
        &authority,
        &home,
        &provenant_core::CredentialRoots::for_home(&home),
    )?;
    prepare_writer_directories(
        &linked,
        &selected_ref,
        &authority,
        &home,
        &provenant_core::CredentialRoots::for_home(&home),
    )?;
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
                prepare_writer_directories(
                    &linked,
                    &selected_ref,
                    &authority,
                    &home,
                    &provenant_core::CredentialRoots::for_home(&home)
                ),
                Err(PermissionError::AnchorMismatch)
            ),
            "mode {mode:o}"
        );
    }
    std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))?;
    let namespace = cache.parent().ok_or("namespace")?;
    std::fs::set_permissions(namespace, std::fs::Permissions::from_mode(0o755))?;
    assert!(matches!(
        prepare_writer_directories(
            &linked,
            &selected_ref,
            &authority,
            &home,
            &provenant_core::CredentialRoots::for_home(&home)
        ),
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
        WorktreeWriteAuthority::for_worktree(
            &recreated,
            &GitRefName::branch("recreated")?,
            &home,
            &provenant_core::CredentialRoots::for_home(&home)
        )?
        .cargo_cache
        .path
    );
    assert_ne!(
        git_state,
        WorktreeWriteAuthority::for_worktree(
            &recreated,
            &GitRefName::branch("recreated")?,
            &home,
            &provenant_core::CredentialRoots::for_home(&home)
        )?
        .git_state
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
    crate::worktrees::acquire_worktree_subject(linked.repo())?;
    let private = linked.repo().private_git_dir().to_owned();
    let common = linked.repo().common_git_dir().to_owned();
    assert_ne!(private, common);
    let grants = WorktreeWriteGrants::derive(&linked, &selected_ref);
    assert_eq!(grants.worktree.path, linked.path());
    assert_eq!(grants.private_git_dir.path, private);
    assert_eq!(grants.common_git_dir.path, common);
    assert!(grants.iter().all(|grant| grant.scope == PathScope::Subtree));

    let home = provenant_os::fs::canonicalize(&fixture.home)?;
    let credentials = provenant_core::CredentialRoots::for_home(&home);
    let authority =
        WorktreeWriteAuthority::for_worktree(&linked, &selected_ref, &home, &credentials)?;
    fixture.run(&["pack-refs", "--all"])?;
    let missing_parent = common.join("refs/heads/s3");
    if missing_parent.exists() {
        std::fs::remove_dir(&missing_parent)?;
    }
    assert!(!missing_parent.exists());

    prepare_writer_directories(&linked, &selected_ref, &authority, &home, &credentials)?;
    let grants = WorktreeWriteGrants::derive(&linked, &selected_ref);
    assert_eq!(authority.git_state.scope, PathScope::Subtree);
    assert_eq!(
        authority.git_state.path.parent(),
        Some(
            home.join("Library/Application Support/provenant/worktree-git")
                .as_path()
        )
    );
    assert_eq!(
        authority.git_grants_digest,
        grants.digest(&authority.cargo_cache, &authority.git_state)
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
        authority
            .git_state
            .path
            .parent()
            .ok_or("Git state namespace")?
            .to_owned(),
        authority.git_state.path.clone(),
        authority.git_state.path.join("git-common"),
        authority.git_state.path.join("git-objects"),
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
        verify_worktree_authority(
            &authority,
            &wrong_home,
            &provenant_core::CredentialRoots::for_home(wrong_home.home)
        ),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));
    let verified_grants = verify_worktree_authority(
        &authority,
        &anchors,
        &provenant_core::CredentialRoots::for_home(anchors.home),
    )?;
    assert_eq!(verified_grants.worktree.path, linked.path());
    assert_eq!(verified_grants.private_git_dir.path, private);
    assert_eq!(verified_grants.common_git_dir.path, common);
    assert!(
        verified_grants
            .iter()
            .all(|grant| grant.scope == PathScope::Subtree)
    );

    let other_ref = GitRefName::branch("s3/other")?;
    let mismatched = Anchors {
        home: &provenant_os::fs::canonicalize(&fixture.home)?,
        directories: &directories,
        worktree: Some((&linked, &other_ref)),
    };
    assert!(matches!(
        verify_worktree_authority(
            &authority,
            &mismatched,
            &provenant_core::CredentialRoots::for_home(mismatched.home)
        ),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));

    let mut tampered = authority.clone();
    tampered.git_grants_digest[0] ^= 1;
    assert!(matches!(
        verify_worktree_authority(
            &tampered,
            &anchors,
            &provenant_core::CredentialRoots::for_home(anchors.home)
        ),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));

    let mut tampered = authority.clone();
    tampered.common_git_dir = fixture._temp.path().join("other-git");
    assert!(matches!(
        verify_worktree_authority(
            &tampered,
            &anchors,
            &provenant_core::CredentialRoots::for_home(anchors.home)
        ),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));
    let mut tampered = authority.clone();
    tampered.git_state.path = fixture._temp.path().join("other-git-state");
    assert!(matches!(
        verify_worktree_authority(
            &tampered,
            &anchors,
            &provenant_core::CredentialRoots::for_home(anchors.home)
        ),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));
    let mut missing = serde_json::to_value(&authority)?;
    missing
        .as_object_mut()
        .ok_or("authority fields")?
        .remove("common_git_dir");
    assert!(serde_json::from_value::<WorktreeWriteAuthority>(missing).is_err());
    let mut missing = serde_json::to_value(&authority)?;
    missing
        .as_object_mut()
        .ok_or("authority fields")?
        .remove("git_state");
    assert!(serde_json::from_value::<WorktreeWriteAuthority>(missing).is_err());

    let original_head = std::fs::read(private.join("HEAD"))?;
    std::fs::write(private.join("HEAD"), b"ref: refs/heads/s3/other\n")?;
    assert!(matches!(
        verify_worktree_authority(
            &WorktreeWriteAuthority::for_worktree(&linked, &selected_ref, &home, &credentials)?,
            &anchors,
            &credentials
        ),
        Err(PermissionError::GitWriteAuthorityMissing)
    ));
    std::fs::write(private.join("HEAD"), original_head)?;
    verify_worktree_authority(
        &authority,
        &anchors,
        &provenant_core::CredentialRoots::for_home(anchors.home),
    )?;
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
