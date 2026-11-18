use super::grants::write_grants_fixture;
use super::*;
#[cfg(target_os = "macos")]
use crate::repo::GitRefName;
#[cfg(target_os = "macos")]
use provenant_os::exec::{ExecOptions, StreamPolicy};
#[cfg(target_os = "macos")]
use std::collections::BTreeMap;
#[cfg(target_os = "macos")]
use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn fixture_credentials() -> provenant_core::CredentialRoots {
    provenant_core::CredentialRoots::for_home(Path::new("/Users/alice"))
}

#[test]
// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
fn read_refusal_reports_the_specific_credential_declaration_in_either_order() {
    use provenant_core::{CredentialKind, CredentialRoots, CredentialScope};

    let shared = PathBuf::from("/fixture/shared");
    let nested = shared.join("nested");
    let lane = nested.join("lane");
    // The first declaration is the expected attribution. Enclosed exact files
    // keep independent sandbox denials; enclosed subtree stores refuse reads.
    for (grant, declarations, lane_roots) in [
        (
            nested.join("tool"),
            vec![
                (nested.clone(), CredentialScope::Subtree),
                (shared.clone(), CredentialScope::Subtree),
            ],
            vec![],
        ),
        (
            lane.clone(),
            vec![
                (lane.join("store/deep"), CredentialScope::Subtree),
                (lane.join("store/deep/auth"), CredentialScope::Exact),
                (lane.join("store"), CredentialScope::Subtree),
                (shared.clone(), CredentialScope::Subtree),
            ],
            vec![lane.clone()],
        ),
        (
            lane.join("endpoint"),
            vec![
                (lane.join("endpoint"), CredentialScope::Exact),
                (lane.join("endpoint/deeper"), CredentialScope::Subtree),
                (shared.clone(), CredentialScope::Subtree),
            ],
            vec![lane.clone()],
        ),
    ] {
        for reverse in [false, true] {
            let expected_path = declarations[0].0.clone();
            let mut declarations: Vec<_> = declarations.iter().enumerate().collect();
            if reverse {
                declarations.reverse();
            }
            let mut credentials = CredentialRoots::default();
            for (index, (path, scope)) in declarations {
                credentials.add(
                    path.clone(),
                    *scope,
                    format!("source {index}"),
                    CredentialKind::Store,
                );
            }
            let credentials = credentials.with_lane_roots(lane_roots.clone());
            let error = validate_read_exec_grant(
                &PathGrant {
                    path: grant.clone(),
                    scope: PathScope::Subtree,
                },
                &credentials,
            )
            .unwrap_err();
            assert!(matches!(error, PermissionError::CredentialStore {
                path, root, declaration_source
            } if path == grant && root == expected_path && declaration_source == "source 0"));
        }
    }
}

#[cfg(target_os = "macos")]
use std::time::Duration;

#[test]
fn git_control_link_refusal_names_the_file_and_the_recovery() {
    let path = Path::new("/repo/.git/config");
    let error = PermissionError::GitControlHardLinked(path.to_owned());
    assert!(matches!(error, PermissionError::GitControlHardLinked(ref refused) if refused == path));
}

#[cfg(target_os = "macos")]
#[test]
// CI-required fixture: lane sandboxes may deny hard_link with EPERM.
fn only_git_control_files_are_checked_for_other_names() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = crate::test_support::Fixture::new()?;
    let git_dir = fixture.repo.common_git_dir().to_owned();
    verify_git_control_links(&fixture.repo)?;
    // Build output and dependency trees hold hard links and are not walked.
    let tree = fixture.root.join("target/debug");
    std::fs::create_dir_all(&tree)?;
    std::fs::write(tree.join("object"), b"o")?;
    std::fs::hard_link(tree.join("object"), tree.join("alias"))?;
    verify_git_control_links(&fixture.repo)?;
    std::fs::create_dir_all(git_dir.join("hooks"))?;
    std::fs::create_dir_all(git_dir.join("info"))?;
    std::fs::write(git_dir.join("hooks/pre-commit"), b"#!/bin/sh\n")?;
    std::fs::write(git_dir.join("info/exclude"), b"")?;
    for control in ["config", "hooks/pre-commit", "info/exclude"] {
        let alias = fixture.root.join("alias");
        std::fs::hard_link(git_dir.join(control), &alias)?;
        let error = verify_git_control_links(&fixture.repo).unwrap_err();
        assert!(
            matches!(&error, PermissionError::GitControlHardLinked(path) if path == &git_dir.join(control)),
            "{control}: {error}"
        );
        let policy = provenant_records::policy::Policy::default();
        let rule = policy.rule("VALIDATION_VERIFY_GIT_CONTROL_LINKS_GIT_ISOLATION");
        assert_eq!(
            rule.rule.boundary,
            provenant_records::policy::Boundary::GitIsolation
        );
        assert_eq!(
            rule.rule.strength,
            provenant_records::policy::Strength::Refuse
        );
        assert!(matches!(
            policy.check(
                "VALIDATION_VERIFY_GIT_CONTROL_LINKS_GIT_ISOLATION",
                provenant_records::policy::Facts::violation("guard failed"),
            ),
            provenant_records::policy::Effect::Refuse(_)
        ));
        std::fs::remove_file(alias)?;
        verify_git_control_links(&fixture.repo)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
// CI-required fixture: lane sandboxes may deny hard_link with EPERM.
fn a_hard_linked_exact_grant_names_its_path() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    std::fs::write(root.join("file"), b"x")?;
    std::fs::hard_link(root.join("file"), root.join("linked"))?;
    let directories = [AnchoredDirectory::open_canonical(&root)?];
    let anchors = Anchors {
        home: &root,
        directories: &directories,
        worktree: None,
    };
    let grant = |name: &str| PathGrant {
        path: root.join(name),
        scope: PathScope::Exact,
    };
    assert!(matches!(
        anchors.verify_single_link_grant(&grant("linked")),
        Err(PermissionError::AnchorMismatchAt { path, stage })
            if path == root.join("linked") && stage == "file has other hard links"
    ));
    anchors.verify_single_link_grant(&grant("absent"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn held_path_failures_name_the_path_and_the_check() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let present = root.join("present");
    std::fs::create_dir(&present)?;
    std::fs::write(present.join("file"), b"x")?;
    let directories = [AnchoredDirectory::open_canonical(&present)?];
    let anchors = Anchors {
        home: &root,
        directories: &directories,
        worktree: None,
    };
    let exact = |path: PathBuf| PathGrant {
        path,
        scope: PathScope::Exact,
    };
    let at = |error: PermissionError| match error {
        PermissionError::AnchorMismatchAt { path, stage } => (path, stage),
        other => panic!("unexpected {other}"),
    };
    assert_eq!(
        at(anchors
            .verify_executable(&exact(present.join("gone")))
            .unwrap_err()),
        (present.join("gone"), "executable is missing")
    );
    assert_eq!(
        at(anchors
            .verify_grant_held(&PathGrant {
                path: root.clone(),
                scope: PathScope::Subtree,
            })
            .unwrap_err()),
        (root.clone(), "directory anchor not held")
    );
    // A parent that exists but was never held is an error; one that does
    // not exist grants nothing.
    std::fs::create_dir(root.join("elsewhere"))?;
    assert_eq!(
        at(anchors
            .verify_grant_held(&exact(root.join("elsewhere/file")))
            .unwrap_err()),
        (root.join("elsewhere"), "parent directory anchor not held")
    );
    anchors.verify_grant_held(&exact(root.join("absent/file")))?;
    Ok(())
}

#[test]
fn a_grant_in_a_directory_that_does_not_exist_needs_no_anchor()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let (mut plan, profile) = write_grants_fixture();
    plan.scratch = PathGrant {
        path: root.join("scratch"),
        scope: PathScope::Subtree,
    };
    plan.private_state = None;
    plan.read_only = None;
    let missing = root.join("not-yet/created");
    let mut profile = profile;
    profile.spec.state_writes = vec![
        PathGrant {
            path: missing.join("session"),
            scope: PathScope::Exact,
        },
        PathGrant {
            path: root.join("present-file"),
            scope: PathScope::Exact,
        },
    ];
    let anchors = required_anchors(&plan, &profile, None);
    assert!(!anchors.contains(&missing));
    assert!(anchors.contains(&root));
    std::fs::create_dir_all(&missing)?;
    assert!(required_anchors(&plan, &profile, None).contains(&missing));
    Ok(())
}

#[test]
fn invalid_path_error_names_the_refused_path() {
    for path in [
        "/",
        "relative/path",
        "/opt/homebrew/bin/../Cellar/opencode-v2/2.0.32",
    ] {
        let error = validate_absolute(Path::new(path)).unwrap_err();
        assert!(matches!(
            error,
            PermissionError::InvalidPath(ref refused) if refused == Path::new(path)
        ));
    }
}

#[test]
fn codex_standalone_package_is_read_exec_only() {
    use provenant_core::CredentialKind;

    let credentials = fixture_credentials();
    let grant = |path: PathBuf| PathGrant {
        path,
        scope: PathScope::Subtree,
    };
    let codex = credentials
        .entries()
        .iter()
        .find(|root| root.kind == CredentialKind::CodexHome)
        .expect("default Codex home is declared");
    let release = codex
        .path
        .join("packages/standalone/releases/0.163.0-aarch64-apple-darwin");
    for path in [release.clone(), release.join("bin/codex")] {
        assert!(validate_read_exec_grant(&grant(path.clone()), &credentials).is_ok());
        assert!(
            matches!(
                validate_noncredential_grant(&grant(path), &credentials),
                Err(PermissionError::CredentialRoot | PermissionError::CredentialStore { .. })
            ),
            "writes stay strict"
        );
    }
    for relative in [
        "",
        "auth.json",
        "config.toml",
        "sessions",
        "packages",
        "packages/standalone",
        "packages/standalone/releases",
        "packages/standalone/current",
        "packages/other/releases/1",
    ] {
        let path = if relative.is_empty() {
            codex.path.clone()
        } else {
            codex.path.join(relative)
        };
        assert!(matches!(
            validate_read_exec_grant(&grant(path), &credentials),
            Err(PermissionError::CredentialRoot
                | PermissionError::CredentialStore { .. }
                | PermissionError::BroadRoot)
        ));
    }
    for path in [
        release.join(".codex/auth.json"),
        PathBuf::from("/tmp/x/.claude/packages/standalone/releases/1"),
        PathBuf::from("/tmp/x/.codex/packages/standalone/releases/1"),
        PathBuf::from("/Users/alice/project/.claude"),
    ] {
        assert!(validate_read_exec_grant(&grant(path.clone()), &credentials).is_ok());
        assert!(!refuses_lane_root(&path, true, &credentials));
    }
}

#[test]
fn profile_family_limits_follow_configured_grant_limit() {
    use super::super::validation::validate_profile_spec_with_home;
    let grant = |path: &str, scope| PathGrant {
        path: path.into(),
        scope,
    };
    let mut spec = ProviderProfileSpec {
        process_executables: vec![grant("/fixture/provider", PathScope::Exact)],
        authentication: (0..17)
            .map(|index| AuthenticationProfile {
                keychain: None,
                mechanism: format!("fixture-{index}"),
                roots: vec![grant(&format!("/fixture/auth/{index}"), PathScope::Exact)],
                in_place_writes: vec![],
            })
            .collect(),
        native_controls: (0..65).map(|index| format!("fixture-{index}")).collect(),
        ..ProviderProfileSpec::default()
    };
    let error = validate_profile_spec_with_home(
        &spec,
        Path::new("/fixture/home"),
        &provenant_core::CredentialRoots::for_home(Path::new("/fixture/home")),
        64,
    )
    .unwrap_err();
    assert!(matches!(
        &error,
        PermissionError::GrantLimitExceeded {
            family: "native controls",
            limit: 64
        }
    ));
    assert_eq!(error.setting_key(), Some(GRANT_LIMIT_SETTING_KEY));
    assert!(
        validate_profile_spec_with_home(
            &spec,
            Path::new("/fixture/home"),
            &provenant_core::CredentialRoots::for_home(Path::new("/fixture/home")),
            65
        )
        .is_ok()
    );
    spec.native_controls.clear();
    spec.authentication.push(AuthenticationProfile {
        keychain: None,
        mechanism: "fixture-extra".into(),
        roots: vec![grant("/fixture/auth/extra", PathScope::Exact)],
        in_place_writes: vec![],
    });
    assert!(
        validate_profile_spec_with_home(
            &spec,
            Path::new("/fixture/home"),
            &provenant_core::CredentialRoots::for_home(Path::new("/fixture/home")),
            17
        )
        .is_err()
    );
    spec.authentication.truncate(2);
    spec.native_controls = vec!["first".into(), "second".into()];
    assert!(
        validate_profile_spec_with_home(
            &spec,
            Path::new("/fixture/home"),
            &provenant_core::CredentialRoots::for_home(Path::new("/fixture/home")),
            1
        )
        .is_ok()
    );
}

#[test]
fn exact_tool_exec_grants_validate_without_allowing_credential_paths() {
    let mut spec = ProviderProfileSpec {
        process_executables: vec![PathGrant {
            path: "/bin/sh".into(),
            scope: PathScope::Exact,
        }],
        allow_process_fork: true,
        tool_exec_roots: vec![PathGrant {
            path: "/opt/check/bin/python3".into(),
            scope: PathScope::Exact,
        }],
        ..Default::default()
    };
    let credentials = fixture_credentials();
    assert!(validate_profile_spec(&spec, &credentials).is_ok());
    for root in credentials.entries() {
        spec.tool_exec_roots[0].path = root.path.clone();
        assert!(
            matches!(
                validate_profile_spec(&spec, &credentials),
                Err(PermissionError::CredentialRoot | PermissionError::CredentialStore { .. })
            ),
            "{}",
            root.path.display()
        );
    }
}

#[test]
fn broad_write_grant_roots_are_rejected() {
    for path in [
        "/",
        "/Users/alice",
        "/home/alice",
        "/tmp",
        "/private",
        "/private/tmp",
        "/private/var",
        "/private/var/tmp",
        "/var",
        "/var/tmp",
    ] {
        let grant = PathGrant {
            path: PathBuf::from(path),
            scope: PathScope::Subtree,
        };
        assert!(grant.validate().is_err(), "{path}");
    }
}

#[test]
fn every_declared_credential_root_protects_grants_and_enclosing_paths() {
    use provenant_core::{CredentialKind, CredentialScope};

    let home = Path::new("/Users/alice");
    let mut credentials = provenant_core::CredentialRoots::for_home(home);
    credentials.add(
        home.join(".claude-registered-account"),
        CredentialScope::Subtree,
        "account registry: claude account directory",
        CredentialKind::Store,
    );
    for root in credentials.entries() {
        let scope = match root.scope {
            CredentialScope::Exact => PathScope::Exact,
            CredentialScope::Subtree => PathScope::Subtree,
        };
        let grant = PathGrant {
            path: root.path.clone(),
            scope,
        };
        for result in [
            validate_read_exec_grant(&grant, &credentials),
            validate_noncredential_grant(&grant, &credentials),
        ] {
            assert!(result.is_err());
        }
        assert!(refuses_lane_root(&grant.path, true, &credentials));
        assert!(refuses_lane_root(&grant.path, false, &credentials));
        assert!(!root.source.is_empty());

        if scope == PathScope::Subtree {
            let worktree = PathGrant {
                path: root.path.join(".worktrees/lane"),
                scope: PathScope::Subtree,
            };
            assert!(validate_read_exec_grant(&worktree, &credentials).is_err());
            assert!(validate_noncredential_grant(&worktree, &credentials).is_err());
        }
        if let Some(parent) = root.path.parent() {
            let enclosing = PathGrant {
                path: parent.to_owned(),
                scope: PathScope::Subtree,
            };
            // Exact endpoints stay denied independently of an enclosing read.
            // Other declarations (or a broad path) may still refuse this parent.
            let mut dedicated = provenant_core::CredentialRoots::default();
            dedicated.add_entry(root.clone());
            assert_eq!(
                validate_read_exec_grant(&enclosing, &dedicated).is_err(),
                scope == PathScope::Subtree || enclosing.validate().is_err(),
                "{}",
                root.path.display()
            );
            // Materialisation must not copy an endpoint into an unprotected
            // staging path, even though the sandbox can deny it in place.
            assert!(refuses_lane_root(&enclosing.path, true, &dedicated));
            assert!(validate_noncredential_grant(&enclosing, &credentials).is_err());
        }
    }

    for path in [home.to_owned(), home.parent().unwrap().to_owned()] {
        assert!(refuses_lane_root(&path, true, &credentials));
        assert!(refuses_lane_root(&path, false, &credentials));
    }

    for name in provenant_core::PROJECT_AGENT_CONFIG_DIRECTORIES
        .iter()
        .copied()
        .chain([".ssh"])
    {
        let project_local = PathBuf::from("/Users/alice/project").join(name);
        let grant = PathGrant {
            path: project_local.clone(),
            scope: PathScope::Subtree,
        };
        assert!(validate_read_exec_grant(&grant, &credentials).is_ok());
        assert!(validate_noncredential_grant(&grant, &credentials).is_ok());
        assert!(!refuses_lane_root(&project_local, false, &credentials));
    }

    let codex = credentials
        .entries()
        .iter()
        .find(|root| root.kind == CredentialKind::CodexHome)
        .expect("default Codex home is declared");
    let release = codex
        .path
        .join("packages/standalone/releases/current/bin/codex");
    assert!(credentials.is_codex_standalone_package(&release));
    assert!(
        validate_read_exec_grant(
            &PathGrant {
                path: release.clone(),
                scope: PathScope::Subtree,
            },
            &credentials,
        )
        .is_ok()
    );
    assert!(
        validate_noncredential_grant(
            &PathGrant {
                path: release,
                scope: PathScope::Subtree,
            },
            &credentials,
        )
        .is_err()
    );

    let overlapping_store = codex
        .path
        .join("packages/standalone/releases/current/registered-store");
    credentials.add(
        overlapping_store.clone(),
        CredentialScope::Subtree,
        "account registry: nested credential store",
        CredentialKind::Store,
    );
    assert!(
        validate_read_exec_grant(
            &PathGrant {
                path: overlapping_store,
                scope: PathScope::Subtree,
            },
            &credentials,
        )
        .is_err()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn compiler_uses_the_declared_table_for_an_unconventional_home() {
    let home = Path::new("/srv/team/alice");
    let credentials = provenant_core::CredentialRoots::for_home(home);
    let grant = |path: &Path| PathGrant {
        path: path.to_owned(),
        scope: PathScope::Subtree,
    };
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            process_executables: vec![PathGrant {
                path: "/opt/provider/bin/cli".into(),
                scope: PathScope::Exact,
            }],
            ..ProviderProfileSpec::default()
        },
        observations: BTreeMap::new(),
    };
    let plan = |read_roots: Vec<PathGrant>, write_roots: Vec<PathGrant>| PlanPermissions {
        credential_roots: credentials.clone(),
        policy: None,
        read_only: None,
        mode: AccessMode::ReadOnly,
        scratch: grant(Path::new("/srv/scratch")),
        private_state: None,
        read_roots,
        protected_roots: Vec::new(),
        secret_roots: Vec::new(),
        selected_authentication: None,
        include_continuation: false,
        write_roots,
        network: NetworkAccess::Provider,
        nested_confinement: false,
    };
    let anchors = Anchors {
        home,
        directories: &[],

        worktree: None,
    };
    for root in [home, Path::new("/srv/team"), Path::new("/srv")] {
        for plan in [
            plan(vec![grant(root)], Vec::new()),
            plan(Vec::new(), vec![grant(root)]),
        ] {
            assert!(
                validate_read_exec_grant(&grant(root), &credentials).is_err(),
                "{root:?}"
            );
            assert!(
                matches!(
                    compile_sbpl(&plan, &profile, &anchors),
                    Err(PermissionError::CredentialRoot | PermissionError::CredentialStore { .. })
                ),
                "{root:?}: {:?}",
                compile_sbpl(&plan, &profile, &anchors).err()
            );
        }
    }
    // A project beneath the home passes this check and fails later only
    // because the fixture holds no anchors.
    assert!(matches!(
        compile_sbpl(
            &plan(vec![grant(&home.join("project"))], Vec::new()),
            &profile,
            &anchors
        ),
        Err(PermissionError::AnchorMismatchAt { .. })
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn anchored_directory_rejects_noncanonical_tmp_aliases() {
    let temp = crate::test_support::tempdir().expect("temporary anchor");
    // An explicit alias exercises macOS symlink rejection inside a writable fixture.
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(temp.path(), &alias).expect("temporary anchor alias");
    let canonical = provenant_os::fs::canonicalize(&alias).expect("canonical temporary anchor");
    assert_ne!(alias, canonical);
    assert!(matches!(
        AnchoredDirectory::open_canonical(&alias),
        Err(PermissionError::InvalidPath(path)) if path == alias
    ));
    assert!(AnchoredDirectory::open_canonical(canonical).is_ok());
}

#[cfg(target_os = "macos")]
#[test]
fn anchored_directory_detects_replacement_at_its_sbpl_path() {
    let temp = crate::test_support::tempdir().expect("temporary anchor root");
    let original = temp.path().join("root");
    std::fs::create_dir(&original).expect("anchor directory");
    let path = provenant_os::fs::canonicalize(&original).expect("canonical anchor path");

    let anchor = AnchoredDirectory::open_canonical(&path).expect("open anchor");
    let moved = temp.path().join("moved");
    std::fs::rename(&original, &moved).expect("move held anchor");
    std::fs::create_dir(&original).expect("replacement directory");
    let directories = [anchor];
    let anchors = Anchors {
        home: Path::new("/fixture/home"),
        directories: &directories,
        worktree: None,
    };
    let error = anchors.verify_all().expect_err("replaced anchor accepted");
    assert!(
        matches!(error, PermissionError::AnchorMismatchAt { path: actual, .. } if actual == path)
    );
}

#[cfg(target_os = "macos")]
#[test]
#[allow(
    clippy::print_stderr,
    reason = "Native probe omissions must be visible in test output"
)]
fn directory_discovery_keeps_literal_scope_and_refuses_symlinks()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let fixture = crate::test_support::Fixture::new()?;
    let root = provenant_os::fs::canonicalize(fixture._temp.path())?;
    let home = provenant_os::fs::canonicalize(&fixture.home)?;
    let project = root.join("lane");
    fixture.run(&[
        "worktree",
        "add",
        "-b",
        "discovery",
        project.to_str().ok_or("path")?,
    ])?;
    let linked = crate::repo::verify_worktree(&fixture.repo, &project)?;
    let selected_ref = GitRefName::branch("discovery")?;
    let authority = WorktreeWriteAuthority::for_worktree(
        &linked,
        &selected_ref,
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
    let scratch = root.join("scratch");
    std::fs::create_dir(&scratch)?;
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
    let discovery = [&project, &root]
        .into_iter()
        .flat_map(|parent| {
            [".agents", ".claude", ".codex"].map(|name| PathGrant {
                path: parent.join(name),
                scope: PathScope::Exact,
            })
        })
        .collect::<Vec<_>>();
    for grant in &discovery {
        std::fs::create_dir(&grant.path)?;
        std::fs::write(grant.path.join("config.json"), b"private config")?;
    }
    let mut profile = ProviderProfile {
        spec: ProviderProfileSpec {
            process_executables: vec![PathGrant {
                path: "/bin/cat".into(),
                scope: PathScope::Exact,
            }],
            discovery_reads: discovery,
            ..ProviderProfileSpec::default()
        },
        observations: BTreeMap::new(),
    };
    let mut plan = PlanPermissions {
        credential_roots: provenant_core::CredentialRoots::for_home(std::path::Path::new(
            "/fixture/home",
        )),
        policy: None,
        mode: AccessMode::ReadOnly,
        read_only: None,
        read_roots: vec![PathGrant {
            path: project.clone(),
            scope: PathScope::Subtree,
        }],
        protected_roots: Vec::new(),
        secret_roots: Vec::new(),
        scratch: PathGrant {
            path: scratch.clone(),
            scope: PathScope::Subtree,
        },
        private_state: None,
        selected_authentication: None,
        include_continuation: false,
        write_roots: Vec::new(),
        network: NetworkAccess::Provider,
        nested_confinement: false,
    };
    for mode in [
        AccessMode::ReadOnly,
        AccessMode::WorktreeWrite(Box::new(authority)),
    ] {
        plan.mode = mode;
        let worktree =
            matches!(plan.mode, AccessMode::WorktreeWrite(_)).then_some((&linked, &selected_ref));
        let directories = required_anchors(&plan, &profile, worktree)
            .iter()
            .map(AnchoredDirectory::open_canonical)
            .collect::<Result<Vec<_>, _>>()?;
        let anchors = Anchors {
            home: &home,
            directories: &directories,
            worktree,
        };
        // Spec: docs/specs/run-launch.md#policy-and-confinement
        // Shared private state is allowed by default and can be refused by policy.
        plan.private_state = Some(plan.scratch.clone());
        let default_policy = provenant_records::policy::Policy::default();
        assert!(matches!(
            default_policy.check(
                "PLAN_STATE_SCRATCH_OVERLAP",
                provenant_records::policy::Facts::violation("private state overlaps scratch"),
            ),
            provenant_records::policy::Effect::Warn(_)
        ));
        compile_sbpl_impl(
            &plan,
            &profile,
            &anchors,
            ProfileLimits::default(),
            &default_policy,
        )?;
        let mut strict = default_policy;
        strict.overlay(
            &BTreeMap::from([(
                "PLAN_STATE_SCRATCH_OVERLAP".to_owned(),
                provenant_records::policy::Strength::Refuse,
            )]),
            provenant_records::policy::Source::Project,
        );
        assert!(matches!(
            compile_sbpl_impl(&plan, &profile, &anchors, ProfileLimits::default(), &strict),
            Err(PermissionError::ProtectedOverlap)
        ));
        plan.private_state = None;
        let compiled = compile_sbpl(&plan, &profile, &anchors)?;
        let sbpl = std::str::from_utf8(&compiled.profile)?;
        let held_without_parent = directories.iter().filter(|anchor| anchor.path != root);
        let missing_parent = Anchors {
            home: &home,
            directories: &held_without_parent
                .map(|anchor| AnchoredDirectory::open_canonical(&anchor.path))
                .collect::<Result<Vec<_>, _>>()?,
            worktree,
        };
        let error = compile_sbpl(&plan, &profile, &missing_parent)
            .err()
            .ok_or("missing discovery parent accepted")?;
        assert!(
            matches!(error, PermissionError::AnchorMismatchAt { path, .. } if path == root.join(".agents"))
        );
        // Discovery grants retain exact scope. Ordinary project configuration
        // contents are ambient; credential confidentiality is enforced by the
        // explicit mandatory home roots, covered by the escape probes.
        for grant in profile
            .spec
            .discovery_reads
            .iter()
            .filter(|grant| grant.path.parent() == Some(root.as_path()))
        {
            let argv = [
                OsString::from(SANDBOX_EXEC),
                "-p".into(),
                sbpl.into(),
                "/bin/cat".into(),
                grant.path.join("config.json").into_os_string(),
            ];
            let mut options = ExecOptions::new(&argv, &scratch, Duration::from_secs(2));
            options.stderr = StreamPolicy::Capture { limit: 1024 };
            let outcome = fixture.executor.execute(options)?;
            if String::from_utf8_lossy(&outcome.stderr.bytes)
                .contains("sandbox_apply: Operation not permitted")
            {
                provenant_test_support::skip_sandbox_dependent_test(
                    "native discovery denial probe unavailable under inherited sandbox",
                )?;
                break;
            }
            assert!(successful_probe(&outcome), "{outcome:?}");
            assert_eq!(outcome.stdout.bytes, b"private config");
        }
        // An exact runtime read may also name a real directory: it stays a
        // literal (entry names only), never a subtree of its children.
        profile
            .spec
            .runtime_reads
            .push(profile.spec.discovery_reads[0].clone());
        let listed = compile_sbpl(&plan, &profile, &anchors)?;
        let listed = std::str::from_utf8(&listed.profile)?;
        let literal = profile.spec.discovery_reads[0].path.display();
        assert!(listed.contains(&format!("(literal \"{literal}\")")));
        assert!(!listed.contains(&format!("(subpath \"{literal}\")")));
        profile.spec.runtime_reads.pop();
        let path = &profile.spec.discovery_reads[0].path;
        let moved = project.join("moved-config");
        std::fs::rename(path, &moved)?;
        symlink(&moved, path)?;
        let error = compile_sbpl(&plan, &profile, &anchors)
            .err()
            .ok_or("discovery symlink accepted")?;
        assert!(
            matches!(error, PermissionError::AnchorMismatchAt { path: actual, .. } if actual == *path)
        );
        std::fs::remove_file(path)?;
        std::fs::rename(moved, path)?;
    }
    Ok(())
}

#[test]
fn provider_profile_without_a_declared_executable_is_refused() {
    let spec = ProviderProfileSpec::default();
    assert!(matches!(
        validate_profile_spec(&spec, &fixture_credentials()),
        Err(PermissionError::ProcessPolicyMissing)
    ));
}

#[test]
fn credential_writes_stay_inside_auth_roots_and_state_stays_outside() {
    let grant = |path: &str, scope| PathGrant {
        path: path.into(),
        scope,
    };
    let spec = |in_place: PathGrant, state: PathGrant| ProviderProfileSpec {
        process_executables: vec![grant("/opt/provider/bin/cli", PathScope::Exact)],
        authentication: vec![AuthenticationProfile {
            keychain: None,
            mechanism: "login".into(),
            roots: vec![grant(
                "/Users/fixture/.provider/auth.json",
                PathScope::Exact,
            )],
            in_place_writes: vec![in_place],
        }],
        state_writes: vec![state],
        ..ProviderProfileSpec::default()
    };
    let auth = grant("/Users/fixture/.provider/auth.json", PathScope::Exact);
    let transcript = grant("/Users/fixture/.provider/projects/p", PathScope::Subtree);
    assert!(
        validate_profile_spec(
            &spec(auth.clone(), transcript.clone()),
            &fixture_credentials()
        )
        .is_ok()
    );
    for in_place in [
        grant("/Users/fixture/.provider/other.json", PathScope::Exact),
        grant("/Users/fixture/.provider", PathScope::Subtree),
    ] {
        assert!(matches!(
            validate_profile_spec(&spec(in_place, transcript.clone()), &fixture_credentials()),
            Err(PermissionError::InvalidFixture)
        ));
    }
    assert!(matches!(
        validate_profile_spec(
            &spec(auth, grant("/Users/fixture/.provider", PathScope::Subtree)),
            &fixture_credentials()
        ),
        Err(PermissionError::CredentialRoot | PermissionError::CredentialStore { .. })
    ));
}

#[test]
fn invalid_native_control_labels_are_omitted() {
    use super::super::validation::{
        filtered_native_controls, filtered_native_controls_with_policy,
    };
    use provenant_records::policy::{Effect, Facts, Policy, Source, Strength};
    use std::collections::BTreeMap;
    let mut spec = ProviderProfileSpec {
        process_executables: vec![PathGrant {
            path: "/fixture/provider".into(),
            scope: PathScope::Exact,
        }],
        native_controls: vec!["valid.control".into(), "bad\nlabel".into(), "".into()],
        ..ProviderProfileSpec::default()
    };
    assert!(
        super::super::validation::validate_profile_spec_with_home(
            &spec,
            Path::new("/fixture/home"),
            &provenant_core::CredentialRoots::for_home(Path::new("/fixture/home")),
            64
        )
        .is_ok()
    );
    assert_eq!(
        filtered_native_controls(&spec.native_controls),
        ["valid.control"]
    );
    let mut policy = Policy::default();
    let code = "RENDER_FILTERED_NATIVE_CONTROLS_NONE";
    assert_eq!(policy.rule(code).rule.strength, Strength::Warn);
    assert!(matches!(
        policy.check(code, Facts::violation("predicate observed")),
        Effect::Warn(_)
    ));
    policy.overlay(
        &BTreeMap::from([(code.to_owned(), Strength::Off)]),
        Source::Host,
    );
    assert_eq!(
        policy.check(code, Facts::violation("predicate observed")),
        Effect::Proceed
    );
    assert_eq!(
        filtered_native_controls_with_policy(&spec.native_controls, &policy),
        ["valid.control"]
    );
    spec.native_controls = filtered_native_controls(&spec.native_controls);
    assert_eq!(spec.native_controls, ["valid.control"]);
}
