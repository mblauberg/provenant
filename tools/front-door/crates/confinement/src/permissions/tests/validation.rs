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
#[cfg(target_os = "macos")]
use std::time::Duration;

#[test]
fn git_control_link_refusal_names_the_file_and_the_recovery() {
    let error = PermissionError::GitControlHardLinked("/repo/.git/config".into());
    let message = error.to_string();
    assert!(message.contains("/repo/.git/config"));
    assert!(message.contains("samefile"));
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
        assert_eq!(
            error.to_string(),
            format!("path is not a permitted absolute, normalised path: {path:?}")
        );
    }
}

#[test]
fn codex_standalone_package_is_read_exec_only() {
    let grant = |path: &str| PathGrant {
        path: PathBuf::from(path),
        scope: PathScope::Subtree,
    };
    let release = "/Users/alice/.codex/packages/standalone/releases/0.163.0-aarch64-apple-darwin";
    for path in [release, &format!("{release}/bin/codex")] {
        assert!(validate_read_exec_grant(&grant(path)).is_ok(), "{path}");
        assert!(
            matches!(
                validate_noncredential_grant(&grant(path)),
                Err(PermissionError::CredentialRoot)
            ),
            "writes stay strict: {path}"
        );
    }
    for path in [
        "/Users/alice/.codex",
        "/Users/alice/.codex/auth.json",
        "/Users/alice/.codex/config.toml",
        "/Users/alice/.codex/sessions",
        "/Users/alice/.codex/packages",
        "/Users/alice/.codex/packages/standalone",
        "/Users/alice/.codex/packages/standalone/releases",
        "/Users/alice/.codex/packages/standalone/current",
        "/Users/alice/.codex/packages/other/releases/1",
        "/Users/alice/.codex/packages/standalone/releases/1/.codex/auth.json",
        "/Users/alice/.claude/packages/standalone/releases/1",
        "/tmp/x/.codex/packages/standalone/releases/1",
    ] {
        assert!(
            matches!(
                validate_read_exec_grant(&grant(path)),
                Err(PermissionError::CredentialRoot | PermissionError::BroadRoot)
            ),
            "{path}"
        );
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
    let error = validate_profile_spec_with_home(&spec, Path::new("/fixture/home"), 64).unwrap_err();
    assert!(matches!(
        &error,
        PermissionError::GrantLimitExceeded {
            family: "native controls",
            limit: 64
        }
    ));
    assert!(error.to_string().contains("domains.run.grant_limit"));
    assert!(validate_profile_spec_with_home(&spec, Path::new("/fixture/home"), 65).is_ok());
    spec.native_controls.clear();
    spec.authentication.push(AuthenticationProfile {
        keychain: None,
        mechanism: "fixture-extra".into(),
        roots: vec![grant("/fixture/auth/extra", PathScope::Exact)],
        in_place_writes: vec![],
    });
    assert!(validate_profile_spec_with_home(&spec, Path::new("/fixture/home"), 17).is_err());
    spec.authentication.truncate(2);
    spec.native_controls = vec!["first".into(), "second".into()];
    assert!(validate_profile_spec_with_home(&spec, Path::new("/fixture/home"), 1).is_ok());
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
    assert!(validate_profile_spec(&spec).is_ok());
    for path in [
        "/Users/fixture/.config/gh/token",
        "/Users/fixture/Library/Application Support/GitHub CLI/token",
        "/Users/fixture/Library/Keychains/login.keychain-db",
        "/Users/fixture/.ssh/key",
        "/Users/fixture/.config/gcloud/token",
    ] {
        spec.tool_exec_roots[0].path = path.into();
        assert!(matches!(
            validate_profile_spec(&spec),
            Err(PermissionError::CredentialRoot)
        ));
    }
}

#[test]
fn broad_home_tmp_and_credential_roots_are_refused() {
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
    for path in [
        "/Users/alice/.ssh",
        "/Users/alice/.config/gh",
        "/Users/alice/.kube",
        "/Users/alice/.secrets/tokens",
        "/Users/alice/Monero",
        "/Users/alice/Archives/git-preservation/repo",
    ] {
        let grant = PathGrant {
            path: PathBuf::from(path),
            scope: PathScope::Subtree,
        };
        assert!(
            matches!(
                validate_noncredential_grant(&grant),
                Err(PermissionError::CredentialRoot)
            ),
            "{path}"
        );
        // A shared tool store under a credential root is refused too.
        let spec = ProviderProfileSpec {
            process_executables: vec![PathGrant {
                path: PathBuf::from("/usr/bin/true"),
                scope: PathScope::Exact,
            }],
            tool_writes: vec![grant],
            ..ProviderProfileSpec::default()
        };
        assert!(validate_profile_spec(&spec).is_err(), "{path}");
    }
    for path in [
        "/private/tmp/scoped",
        "/private/var/tmp/scoped",
        "/var/tmp/scoped",
    ] {
        let grant = PathGrant {
            path: PathBuf::from(path),
            scope: PathScope::Subtree,
        };
        assert!(grant.validate().is_ok(), "{path}");
    }
}

#[test]
fn grants_enclosing_a_credential_root_are_refused() {
    let grant = |path: &str, scope| PathGrant {
        path: PathBuf::from(path),
        scope,
    };
    for path in [
        "/Users/alice/.config",
        "/Users/alice/Library",
        "/Users/alice/Library/Application Support",
        "/Users/alice/Archives",
    ] {
        for validate in [validate_read_exec_grant, validate_noncredential_grant] {
            assert!(
                matches!(
                    validate(&grant(path, PathScope::Subtree)),
                    Err(PermissionError::CredentialRoot)
                ),
                "{path}"
            );
        }
        // An exact grant exposes the directory entry, not what it holds.
        assert!(validate_read_exec_grant(&grant(path, PathScope::Exact)).is_ok());
    }
    for path in [
        "/Users/alice/.config/cursor",
        "/Users/alice/.config/opencode",
        "/Users/alice/Library/pnpm",
        "/Users/alice/Library/Caches/pnpm",
        "/Users/alice/project/.config",
        "/Users/alice/project/Archives",
        "/Library/Developer/CommandLineTools",
        "/srv/work/.config",
    ] {
        assert!(
            validate_read_exec_grant(&grant(path, PathScope::Subtree)).is_ok(),
            "{path}"
        );
    }
    // An unconventional home is checked against the host's own path.
    let home = Path::new("/srv/alice");
    for path in [
        "/srv",
        "/srv/alice",
        "/srv/alice/.config",
        "/srv/alice/Library",
    ] {
        assert!(
            encloses_home_credentials(&grant(path, PathScope::Subtree), home),
            "{path}"
        );
    }
    for path in ["/srv/alice/project", "/srv/alice/project/.config"] {
        assert!(
            !encloses_home_credentials(&grant(path, PathScope::Subtree), home),
            "{path}"
        );
    }
}

#[test]
fn lane_root_table_agrees_with_compiler_checks() {
    let standalone =
        "/Users/alice/.codex/packages/standalone/releases/0.163.0-aarch64-apple-darwin";
    let home = Path::new("/fixture/host-home");
    let credential_policy = provenant_records::policy::Policy::default();
    let credential_rule = credential_policy.rule("VALIDATION_REFUSES_LANE_ROOT_CREDENTIALS");
    assert_eq!(
        credential_rule.rule.boundary,
        provenant_records::policy::Boundary::Credentials
    );
    assert_eq!(
        credential_rule.rule.strength,
        provenant_records::policy::Strength::Refuse
    );
    for (path, read_allowed, write_allowed) in [
        ("/Users/alice/.ssh", false, false),
        ("/Users/alice/.config", false, false),
        (standalone, true, false),
        ("/Users/alice/project", true, true),
    ] {
        let grant = PathGrant {
            path: PathBuf::from(path),
            scope: PathScope::Subtree,
        };
        assert_eq!(
            validate_read_exec_grant(&grant).is_ok(),
            read_allowed,
            "read {path}"
        );
        assert_eq!(
            validate_noncredential_grant(&grant).is_ok(),
            write_allowed,
            "write {path}"
        );
        assert_eq!(
            !refuses_lane_root(Path::new(path), true, Some(home)),
            read_allowed,
            "entry read {path}"
        );
        assert_eq!(
            !refuses_lane_root(Path::new(path), false, Some(home)),
            write_allowed,
            "entry write {path}"
        );
    }
    // An injected nonconventional home is refused at entry exactly as the
    // compiler refuses it, without consulting this host's HOME.
    let home = Path::new("/srv/team/alice");
    for (path, read_allowed, write_allowed) in [
        ("/srv/team", false, false),
        ("/srv/team/alice", false, false),
        ("/srv/team/alice/.config", false, false),
        ("/srv/team/alice/project", true, true),
    ] {
        assert_eq!(
            !refuses_lane_root(Path::new(path), true, Some(home)),
            read_allowed,
            "entry read {path}"
        );
        assert_eq!(
            !refuses_lane_root(Path::new(path), false, Some(home)),
            write_allowed,
            "entry write {path}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn compiler_refuses_grants_of_or_around_an_unconventional_host_home() {
    // Static grant checks know only /Users and /home; the compiler must
    // still refuse the host's actual home and any root that encloses it.
    let home = Path::new("/srv/team/alice");
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
            assert!(validate_read_exec_grant(&grant(root)).is_ok(), "{root:?}");
            assert!(
                matches!(
                    compile_sbpl(&plan, &profile, &anchors),
                    Err(PermissionError::CredentialRoot)
                ),
                "{root:?}"
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
    let authority = WorktreeWriteAuthority::for_worktree(&linked, &selected_ref, &home)?;
    prepare_writer_directories(&linked, &selected_ref, &authority, &home)?;
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
        validate_profile_spec(&spec),
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
    assert!(validate_profile_spec(&spec(auth.clone(), transcript.clone())).is_ok());
    for in_place in [
        grant("/Users/fixture/.provider/other.json", PathScope::Exact),
        grant("/Users/fixture/.provider", PathScope::Subtree),
    ] {
        assert!(matches!(
            validate_profile_spec(&spec(in_place, transcript.clone())),
            Err(PermissionError::InvalidFixture)
        ));
    }
    assert!(matches!(
        validate_profile_spec(&spec(
            auth,
            grant("/Users/fixture/.provider", PathScope::Subtree)
        )),
        Err(PermissionError::CredentialRoot)
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
