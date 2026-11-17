//! Native enforcement of aliases and checkouts allocated after rendering.
#![cfg(target_os = "macos")]
use super::*;

#[test]
fn native_host_inventory_protects_aliases_and_later_worktrees()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = crate::test_support::tempdir()?;
    let root = provenant_os::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let state = root.join("state");
    let primary = root.join("source");
    let other = root.join("other-project");
    for directory in [
        &home,
        &state,
        &primary.join(".git"),
        &other.join(".git"),
        &home.join(".codex"),
        &home.join(".config/gh"),
        &home.join(".config/cursor"),
    ] {
        std::fs::create_dir_all(directory)?;
    }
    // The registry only reads a private state directory, whatever the umask.
    std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), primary.clone());
    inputs.state_dir = Some(state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    for primary in [&primary, &other] {
        provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(primary.join(".git"))?,
        )?;
    }
    let secret = root.join("outside-credential");
    let github = root.join("outside-github");
    let login = root.join("outside-selected-login");
    for path in [&secret, &github, &login] {
        std::fs::write(path, b"synthetic")?;
    }
    std::os::unix::fs::symlink(&secret, home.join(".codex/auth.json"))?;
    std::os::unix::fs::symlink(&github, home.join(".config/gh/hosts.yml"))?;
    std::os::unix::fs::symlink(&login, home.join(".config/cursor/state.vscdb"))?;
    let own = primary.join(".worktrees/own");
    let other_primary_worktree = primary.join(".worktrees/other");
    let later = other.join(".git/.worktrees/later");
    let mut inventory = MandatoryRoots::for_host(&roots, &home, &own, &[], &[], &[])?;
    // Model the Git-aware caller adding the selected primary's verified
    // project allocation root. `for_host` itself stays context-free and does
    // not infer this path from the registered common directory.
    inventory.add_protected(PathGrant {
        path: primary.join(".worktrees"),
        scope: PathScope::Subtree,
    })?;
    let (mut plan, mut profile) = write_grants_fixture();
    plan.policy =
        Some(crate::run_config::RunConfig::default().policy_for_launch(&state, inventory));
    plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
        git_grants_digest: [0; 32],
        publishing: false,
        common_git_dir: primary.join(".git"),
        cargo_cache: PathGrant {
            path: root.join("cache"),
            scope: PathScope::Subtree,
        },
    }));
    plan.network = NetworkAccess::Open;
    profile.spec.process_executables.push(PathGrant {
        path: "/bin/sh".into(),
        scope: PathScope::Exact,
    });
    profile.spec.authentication = vec![AuthenticationProfile {
        mechanism: "login".into(),
        keychain: None,
        roots: vec![PathGrant {
            path: home.join(".config/cursor"),
            scope: PathScope::Subtree,
        }],
        in_place_writes: Vec::new(),
    }];
    let mut sources = Vec::new();
    for selected in [false, true] {
        plan.selected_authentication = selected.then(|| "login".into());
        let worktree = WorktreeWriteGrants::from_paths(
            &own,
            &primary.join(".git/worktrees/own"),
            &primary.join(".git"),
            "own",
        );
        let source = render(&GrantSet::build(&plan, &profile, &home, Some(worktree))?)?;
        for path in [&github, &login] {
            assert!(source.contains(&format!(
                "(deny file-write* (literal \"{}\"))",
                path.display()
            )));
        }
        assert!(source.contains(&format!(
            "(deny file-write* (subpath \"{}\"))",
            other.join(".git/.worktrees").display()
        )));
        assert!(source.contains(&format!(
            "(deny file-read* (literal \"{}\"))",
            secret.display()
        )));
        assert_eq!(
            source.contains(&format!(
                "(deny file-read* (literal \"{}\"))",
                login.display()
            )),
            !selected
        );
        if selected {
            // The selected login store is readable: no denial names it.
            assert!(
                !source
                    .lines()
                    .any(|line| line.starts_with("(deny file-read*")
                        && line.contains(&format!("\"{}\"", login.display())))
            );
        }
        sources.push((selected, source));
    }
    // These directories and files did not exist when the profile was rendered.
    for path in [
        &own,
        &other_primary_worktree,
        &later,
        &other.join(".worktrees-other"),
    ] {
        std::fs::create_dir_all(path)?;
    }
    let executor = SystemExecutor::new(2)?;
    let cancel = CancelToken::new();
    let env = [("LC_ALL".into(), "C".into())];
    let (availability, diagnostic) = probe_sandbox_with_diagnostic(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        env: &env,
    })?;
    if !matches!(availability, Availability::Available { .. }) {
        provenant_test_support::skip_sandbox_dependent_test(format!(
            "native sandbox unavailable: {availability:?}; {diagnostic:?}"
        ))?;
        return Ok(());
    }
    let script = r#"
set -eu
printf own > "$1/allowed"
if printf foreign > "$8/blocked"; then exit 86; fi
if printf foreign > "$2/blocked"; then exit 81; fi
if /bin/cat "$3"; then exit 82; fi
if printf overwrite > "$3"; then exit 83; fi
/bin/cat "$4" > /dev/null
if printf overwrite > "$4"; then exit 84; fi
printf ambient > "$5/allowed"
if /bin/cat "$6" > /dev/null; then
    test "$7" = yes
else
    test "$7" = no
fi
if printf overwrite > "$6"; then exit 85; fi
"#;
    for (selected, source) in sources {
        let launch = ConfinedLaunch {
            enforcement: Enforcement::MacOsSandboxExec,
            profile: source.into_bytes(),
            profile_digest: [0; 32],
            native_controls: Vec::new(),
        };
        let command = launch.command(
            &[
                "/bin/sh".into(),
                "-c".into(),
                script.into(),
                "host-boundaries".into(),
                own.as_os_str().to_owned(),
                later.as_os_str().to_owned(),
                secret.as_os_str().to_owned(),
                github.as_os_str().to_owned(),
                other.join(".worktrees-other").into_os_string(),
                login.as_os_str().to_owned(),
                if selected { "yes" } else { "no" }.into(),
                other_primary_worktree.as_os_str().to_owned(),
            ],
            &provenant_core::AttemptId::from_bytes([9; 16]),
            &provenant_os::fs::Directory::open_anchor(&root)?,
        )?;
        let result = executor.execute(command.options(&root, Duration::from_secs(5))?)?;
        assert!(successful_probe(&result), "selected={selected}: {result:?}");
    }
    Ok(())
}

// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
#[test]
fn native_shared_stores_deny_unlisted_files_outside_lanes_under_both_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{CredentialKind, CredentialRoots, CredentialScope};
    let temp = crate::test_support::tempdir()?;
    let root = provenant_os::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let executor = SystemExecutor::new(2)?;
    for store in [
        home.clone(),
        home.join("relocated-codex"),
        home.join(".claude"),
    ] {
        let project = store.join("project");
        let cwd = project.join("cwd");
        let scratch = store.join("scratch");
        std::fs::create_dir_all(&cwd)?;
        std::fs::create_dir_all(&scratch)?;
        let allowed = cwd.join("ordinary");
        let secret = store.join("unlisted-secret");
        std::fs::write(&allowed, b"ordinary\n")?;
        std::fs::write(&secret, b"synthetic credential\n")?;
        let mut credentials = CredentialRoots::default();
        credentials.add(
            &store,
            CredentialScope::Subtree,
            "shared fixture",
            CredentialKind::Store,
        );
        let (mut plan, mut profile) = write_grants_fixture();
        plan.credential_roots =
            credentials.with_lane_roots(vec![project.clone(), cwd.clone(), scratch.clone()]);
        plan.read_roots = vec![PathGrant {
            path: project.clone(),
            scope: PathScope::Subtree,
        }];
        plan.selected_authentication = None;
        plan.scratch = PathGrant {
            path: scratch.clone(),
            scope: PathScope::Subtree,
        };
        plan.private_state = None;
        profile.spec.authentication.clear();
        profile.spec.allow_process_fork = true;
        profile.spec.process_executables = vec![PathGrant {
            path: "/bin/sh".into(),
            scope: PathScope::Exact,
        }];
        let rendered = render(&GrantSet::build(&plan, &profile, &home, None)?)?;
        // Production is open. Change only the ambient default to exercise the
        // same store boundary in an allowlist fixture, granting runtime files
        // and the lane roots explicitly, never the enclosing store.
        for default in ["allow", "deny"] {
            let mut source =
                rendered.replacen("(allow default)", &format!("({default} default)"), 1);
            source.push_str("(allow file-read-metadata)\n(allow file-read* (subpath \"/usr\") (subpath \"/System\") (subpath \"/bin\") (subpath \"/dev\"))\n");
            let launch = ConfinedLaunch {
                enforcement: Enforcement::MacOsSandboxExec,
                profile: source.into_bytes(),
                profile_digest: [0; 32],
                native_controls: Vec::new(),
            };
            let command = launch.command(
                &[
                    "/bin/sh".into(), "-c".into(),
                    "IFS= read -r value < \"$1\" || exit 81; test \"$value\" = ordinary || exit 82; if IFS= read -r value < \"$2\"; then exit 83; fi; printf lane > \"$3\" || exit 84".into(),
                    "shared-store".into(), allowed.as_os_str().to_owned(), secret.as_os_str().to_owned(), scratch.join("output").into_os_string(),
                ],
                &provenant_core::AttemptId::from_bytes([10;16]),
                &provenant_os::fs::Directory::open_anchor(&root)?,
            )?;
            let result = executor.execute(command.options(&cwd, Duration::from_secs(5))?)?;
            assert!(
                successful_probe(&result),
                "default={default}, store={store:?}: {result:?}"
            );
        }
    }
    Ok(())
}
