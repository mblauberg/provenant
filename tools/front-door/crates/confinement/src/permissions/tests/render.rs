#[cfg(any(target_os = "macos", feature = "snapshot"))]
use super::grants::write_grants_fixture;
use super::*;
#[cfg(target_os = "macos")]
use crate::repo::GitRefName;
use provenant_core::CancelToken;
use provenant_os::exec::SystemExecutor;
#[cfg(target_os = "macos")]
use provenant_os::exec::{ExecOutcome, Termination};
#[cfg(target_os = "macos")]
use std::collections::BTreeMap;
#[cfg(any(target_os = "macos", feature = "snapshot"))]
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn profile_path_representability_matches_encoder() {
    use std::os::unix::ffi::OsStringExt;
    let unrepresentable =
        PathBuf::from(std::ffi::OsString::from_vec(b"/fixture/name-\xff".to_vec()));
    for path in [
        Path::new("/fixture/normal"),
        Path::new("/fixture/with\nline"),
        Path::new("/fixture/with\t tab"),
    ] {
        assert!(profile_path_representable(path));
        assert!(sbpl_path(path).is_ok());
    }
    for path in [
        Path::new("/fixture/control-\u{1}"),
        unrepresentable.as_path(),
    ] {
        assert!(!profile_path_representable(path));
        assert!(sbpl_path(path).is_err());
    }
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn expanded_authentication_family_grants_only_selected_credentials() -> Result<(), PermissionError>
{
    let (plan, mut profile) = write_grants_fixture();
    profile.spec.process_executables.push(PathGrant {
        path: "/fixture/provider".into(),
        scope: PathScope::Exact,
    });
    for index in 0..20 {
        profile.spec.authentication.push(AuthenticationProfile {
            mechanism: format!("extra-{index}"),
            roots: vec![PathGrant {
                path: format!("/fixture/credential-{index}").into(),
                scope: PathScope::Exact,
            }],
            in_place_writes: vec![],
        });
    }
    validate_profile_spec_with_home(&profile.spec, Path::new("/fixture/home"), 128)?;
    let grants = GrantSet::build(&plan, &profile, Path::new("/fixture/home"), None)?;
    for auth in &profile.spec.authentication {
        for root in &auth.roots {
            assert_eq!(grants.reads.contains(root), auth.mechanism == "selected");
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn profiles_between_the_old_and_new_limits_compile_and_larger_ones_name_their_size()
-> Result<(), Box<dyn std::error::Error>> {
    let (plan, profile) = write_grants_fixture();
    let mut many = plan.clone();
    many.read_roots = (0..129)
        .map(|index| PathGrant {
            path: PathBuf::from(format!("/fixture/root/{index}")),
            scope: PathScope::Subtree,
        })
        .collect();
    assert!(validate_plan_grant_limit(&many, 128).is_err());
    assert!(validate_plan_grant_limit(&many, 256).is_ok());
    let mut grants = GrantSet::build(&plan, &profile, Path::new("/fixture/home"), None)?;
    let root = |index: usize, width: usize| PathGrant {
        path: PathBuf::from(format!("/fixture/reads/{index}/{}", "x".repeat(width))),
        scope: PathScope::Subtree,
    };
    // About 130 KiB: over the former 64 KiB cap, inside the launch limit.
    grants.reads = (0..1000).map(|index| root(index, 100)).collect();
    let source = render(&grants)?;
    assert!(source.len() > 64 * 1024 && source.len() <= PROFILE_LIMIT);
    grants.reads = (0..4000).map(|index| root(index, 200)).collect();
    let error = render(&grants).unwrap_err();
    assert!(
        matches!(error, PermissionError::ProfileTooLarge { bytes, limit: PROFILE_LIMIT } if bytes > PROFILE_LIMIT)
    );
    let expanded = render_with_limit(&grants, 2 * 1024 * 1024)?;
    assert!(expanded.len() > PROFILE_LIMIT);
    Ok(())
}

#[test]
fn launch_command_allows_only_the_attempts_own_mach_name() -> Result<(), PermissionError> {
    let launch = ConfinedLaunch {
        enforcement: Enforcement::MacOsSandboxExec,
        profile: b"(version 1)\n(deny default)\n".to_vec(),
        profile_digest: [0; 32],
        native_controls: Vec::new(),
    };
    let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
    let command = launch.command(&["tool".into()], &attempt)?;
    assert_eq!(command[3], "tool");
    let profile = command[2].to_string_lossy();
    assert!(profile.starts_with("(version 1)\n(deny default)\n"));
    assert!(profile.ends_with(&format!(
        "(allow mach-lookup (global-name \"dev.provenant.attempt.{attempt}\"))\n"
    )));
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn worktree_access_read_only_profile_limits_checkout_writes()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::plan::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest, ReadOnlyPolicy};
    use crate::utility::{GitMutation, execute_git_mutation};
    use provenant_core::WorkScope;
    use std::os::unix::fs::PermissionsExt;
    let f = crate::test_support::Fixture::new()?;
    let branch = GitRefName::branch("read-only-profile")?;
    let linked = f._temp.path().join("linked");
    let outcome = execute_git_mutation(
        &f.executor,
        &f.repo,
        &f.runtime,
        GitMutation::WorktreeAdd {
            branch,
            base: Some(f.head()?),
            path: linked.clone(),
            create_branch: true,
        },
        f.budget()?,
        &CancelToken::new(),
    )?;
    assert_eq!(outcome.termination, Termination::Exit(0));
    let repo = f
        .runtime
        .open_read_repo(&f.executor, &linked, f.budget()?, &CancelToken::new())?;
    let root = repo.root_path();
    std::fs::create_dir_all(root.join(".agent-run/locks"))?;
    let scratch = provenant_os::fs::canonicalize(f._temp.path())?.join("private-tmp");
    std::fs::create_dir(&scratch)?;
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
    let policy: ReadOnlyPolicy =
        serde_json::from_str(r#"{"readOnlyWritable":[".agent-run/locks"]}"#)?;
    let binding = f.binding()?;
    let plan = ConfinedPlan::new(
        PlanRequest {
            scope: WorkScope::Project(binding.project_id()),
            permissions: PlanPermissions {
                read_only: None,
                mode: AccessMode::ReadOnly,
                scratch: PathGrant {
                    path: scratch.clone(),
                    scope: PathScope::Subtree,
                },
                private_state: None,
                read_roots: vec![],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: None,
                include_continuation: false,
                write_roots: vec![],
                network: NetworkAccess::Provider,
                nested_confinement: false,
            },
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        }
        .with_read_only_repo(&repo, &binding, &policy)?,
    )?;
    use FixtureOperation as O;
    use FixtureOutcome::{Allowed, Denied};
    let home = provenant_os::fs::canonicalize(&f.home)?;
    for directory in [
        home.join(".cargo/bin"),
        home.join(".cargo/registry"),
        home.join(".rustup/toolchains"),
        scratch.join("target"),
    ] {
        std::fs::create_dir_all(directory)?;
    }
    std::fs::write(
        home.join(".rustup/settings.toml"),
        "default_toolchain = 'fixture'",
    )?;
    std::fs::write(home.join(".cargo/credentials.toml"), "fixture-secret")?;
    // A provider may list a configuration directory without reading its
    // children, as OpenCode does for its XDG config directory.
    let listed = home.join(".config/fixture");
    std::fs::create_dir_all(&listed)?;
    let subtree = |path| PathGrant {
        path,
        scope: PathScope::Subtree,
    };
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            process_executables: vec![PathGrant {
                path: "/usr/bin/true".into(),
                scope: PathScope::Exact,
            }],
            runtime_reads: vec![
                subtree(home.join(".cargo/bin")),
                subtree(home.join(".cargo/registry")),
                subtree(home.join(".rustup/toolchains")),
                PathGrant {
                    path: home.join(".rustup/settings.toml"),
                    scope: PathScope::Exact,
                },
                PathGrant {
                    path: listed.clone(),
                    scope: PathScope::Exact,
                },
            ],
            tool_exec_roots: vec![
                subtree(home.join(".cargo/bin")),
                subtree(home.join(".rustup/toolchains")),
                subtree(scratch.join("target")),
            ],
            ..Default::default()
        },
        observations: BTreeMap::from([
            (O::DeclaredRead, Allowed),
            (O::UndeclaredRead, Denied),
            (O::UndeclaredWrite, Denied),
            (O::SecretRead, Denied),
            (O::ProtectedWrite, Denied),
            (O::OtherNetwork, Denied),
            (O::DeclaredProcessExec, Allowed),
            (O::UndeclaredProcessExec, Denied),
            (O::ProcessFork, Denied),
            (O::RuntimeRead, Allowed),
        ]),
    };
    let paths = required_anchors(plan.permissions(), &profile, None);
    let directories = paths
        .iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: &home,
        directories: &directories,

        worktree: None,
    };
    let compiled = MacOsSandboxBackend {
        ready: true,
        limits: ProfileLimits::default(),
    }
    .compile(plan.permissions(), &profile, &anchors)?;
    let source = String::from_utf8(compiled.profile)?;
    let mut strict_permissions = plan.permissions().clone();
    strict_permissions.read_only = None;
    strict_permissions.read_roots.clear();
    let strict = ConfinedPlan::new(
        PlanRequest {
            scope: WorkScope::Project(binding.project_id()),
            permissions: strict_permissions,
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        }
        .with_live_read_only_repo(&repo, &binding)?,
    )?;
    let strict_profile = MacOsSandboxBackend {
        ready: true,
        limits: ProfileLimits::default(),
    }
    .compile(strict.permissions(), &profile, &anchors)?;
    let strict_source = String::from_utf8(strict_profile.profile)?;
    assert!(strict_source.contains(&format!(
        "(deny file-write* (subpath \"{}\"))",
        root.display()
    )));
    assert!(!strict_source.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        root.display()
    )));
    assert!(!strict_source.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        root.join(".agent-run/locks").display()
    )));
    let git = binding.common_dir();
    assert!(repo.0.private_git_dir().starts_with(git));
    assert!(source.contains(&format!(
        "(allow file-read* (subpath \"{}\"))",
        git.display()
    )));
    assert!(source.contains(&format!(
        "(deny file-write* (subpath \"{}\"))",
        git.display()
    )));
    assert!(!source.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        git.display()
    )));
    assert!(source.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        root.join(".agent-run/locks").display()
    )));
    assert!(!source.contains(&format!(
        "(allow process-exec (subpath \"{}\"))",
        root.join(".agent-run/locks").display()
    )));
    for tool in [home.join(".cargo/bin"), home.join(".rustup/toolchains")] {
        assert!(source.contains(&format!(
            "(allow file-read* (subpath \"{}\"))",
            tool.display()
        )));
        assert!(source.contains(&format!(
            "(allow process-exec (subpath \"{}\"))",
            tool.display()
        )));
    }
    // Cargo credentials appear only as hard denies.
    assert!(
        source
            .lines()
            .filter(|line| line.contains("credentials"))
            .all(|line| line.starts_with("(deny "))
    );
    assert!(!source.contains(&format!(
        "(allow file-read* (subpath \"{}\"))",
        home.join(".cargo").display()
    )));
    assert!(source.contains(&format!(
        "(deny file-write* (require-all (subpath \"{}\") (regex #",
        root.join(".agent-run/locks").display()
    )));
    assert!(!source.contains("(deny file-write* (regex #"));
    assert!(source.contains(&crate::plan::read_only::exclusion_regex()));
    assert!(source.contains(&format!("(literal \"{}\")", listed.display())));
    assert!(!source.contains(&format!("(subpath \"{}\")", listed.display())));
    // The listed entry is resolved without following links.
    std::fs::rename(&listed, home.join(".config/elsewhere"))?;
    std::os::unix::fs::symlink(home.join(".config/elsewhere"), &listed)?;
    assert!(
        MacOsSandboxBackend {
            ready: true,
            limits: ProfileLimits::default()
        }
        .compile(plan.permissions(), &profile, &anchors)
        .is_err()
    );
    std::fs::remove_file(&listed)?;
    std::fs::rename(home.join(".config/elsewhere"), &listed)?;
    // A component swapped for a symlink after resolution cannot be compiled.
    std::fs::rename(root.join(".agent-run"), root.join("moved"))?;
    std::os::unix::fs::symlink(root.join("moved"), root.join(".agent-run"))?;
    assert!(
        MacOsSandboxBackend {
            ready: true,
            limits: ProfileLimits::default()
        }
        .compile(plan.permissions(), &profile, &anchors)
        .is_err()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn denied_probe_requires_an_operating_system_refusal() {
    let mut outcome = ExecOutcome {
        termination: Termination::Exit(44),
        stdout: Default::default(),
        stderr: provenant_os::exec::CapturedStream {
            bytes: b"security: The specified item could not be found in the keychain.".to_vec(),
            ..Default::default()
        },
        timed_out: false,
        drain_truncated: false,
        overflow_stopped: false,
        cancelled: false,
        elapsed_micros: 0,
    };
    assert!(!denied_probe(&outcome));
    outcome.stderr.bytes =
        b"sandbox-exec: execvp() of '/usr/bin/security' failed: Operation not permitted".to_vec();
    assert!(denied_probe(&outcome));
    outcome.termination = Termination::Exit(0);
    assert!(!denied_probe(&outcome));
    outcome.termination = Termination::Exit(1);
    outcome.cancelled = true;
    assert!(!denied_probe(&outcome));
}

#[cfg(target_os = "macos")]
#[test]
fn sbpl_process_authority_is_exact_and_fork_requires_profile_data() {
    let executable = PathGrant {
        path: PathBuf::from("/usr/bin/true"),
        scope: PathScope::Exact,
    };
    let mut spec = ProviderProfileSpec {
        process_executables: vec![executable],
        allow_process_fork: true,
        ..ProviderProfileSpec::default()
    };
    let profile = ProviderProfile {
        spec: spec.clone(),
        observations: BTreeMap::new(),
    };
    let plan = PlanPermissions {
        read_only: None,
        private_state: None,
        mode: AccessMode::ReadOnly,
        scratch: PathGrant {
            path: PathBuf::from("/tmp/attempt-scratch"),
            scope: PathScope::Subtree,
        },
        read_roots: Vec::new(),
        protected_roots: Vec::new(),
        secret_roots: Vec::new(),
        selected_authentication: None,
        include_continuation: false,
        write_roots: vec![],
        network: NetworkAccess::Provider,
        nested_confinement: false,
    };
    let rendered = String::from_utf8(
        render_profile(&plan, &profile, &[], &[], None, Path::new("/Users/fixture")).expect("SBPL"),
    )
    .expect("UTF-8 SBPL");
    assert!(rendered.contains("(allow process-exec (literal \"/usr/bin/true\"))"));
    assert!(rendered.contains("(allow process-fork)"));
    assert!(!rendered.contains("localhost"));
    assert!(!rendered.contains("pseudo-tty") && !rendered.contains("/dev/ttys"));

    spec.allow_process_fork = false;
    spec.loopback_server = true;
    spec.allow_pseudo_tty = true;
    let profile = ProviderProfile {
        spec,
        observations: BTreeMap::new(),
    };
    let rendered = String::from_utf8(
        render_profile(&plan, &profile, &[], &[], None, Path::new("/Users/fixture")).expect("SBPL"),
    )
    .expect("UTF-8 SBPL");
    assert!(!rendered.contains("(allow process-fork)"));
    // A private server binds and is reached on loopback only.
    assert!(rendered.contains("(allow network-bind (local ip \"localhost:*\"))"));
    assert!(rendered.contains("(allow network-outbound (remote ip \"localhost:*\"))"));
    assert!(!rendered.contains("\"*:*\""));
    // A pseudo-terminal opens the multiplexer and only its own allocations.
    assert!(rendered.contains("(allow pseudo-tty)"));
    assert!(rendered.contains("(literal \"/dev/ptmx\")"));
    assert!(rendered.contains(
        "(require-all (regex #\"^/dev/ttys[0-9]+$\") (extension \"com.apple.sandbox.pty\"))"
    ));
    assert!(!rendered.contains("(subpath \"/dev\")"));
}

#[cfg(target_os = "macos")]
#[test]
fn network_levels_shared_writes_and_hard_denies_render_in_order() {
    let grant = |path: &str| PathGrant {
        path: PathBuf::from(path),
        scope: PathScope::Subtree,
    };
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            process_executables: vec![PathGrant {
                path: PathBuf::from("/usr/bin/true"),
                scope: PathScope::Exact,
            }],
            tool_writes: vec![grant("/Users/fixture/.cargo/registry")],
            ..ProviderProfileSpec::default()
        },
        observations: BTreeMap::new(),
    };
    let mut plan = PlanPermissions {
        read_only: None,
        private_state: None,
        mode: AccessMode::ReadOnly,
        scratch: grant("/tmp/attempt-scratch"),
        read_roots: Vec::new(),
        protected_roots: Vec::new(),
        secret_roots: Vec::new(),
        selected_authentication: None,
        include_continuation: false,
        write_roots: vec![grant("/work/repo/.agent-run")],
        network: NetworkAccess::Provider,
        nested_confinement: false,
    };
    let render = |plan: &PlanPermissions| {
        let bytes = render_profile(plan, &profile, &[], &[], None, Path::new("/Users/fixture"))
            .expect("SBPL");
        String::from_utf8(bytes).expect("UTF-8 SBPL")
    };
    let provider = render(&plan);
    // Read-only lanes never exec from write roots.
    assert!(!provider.contains("(allow process-exec (subpath \"/work/repo/.agent-run\"))"));
    assert!(!provider.contains("localhost"));
    assert!(!provider.contains("PGSQL"));
    assert!(!provider.contains("\"*:*\""));
    let last_allow = ["/Users/fixture/.cargo/registry", "/work/repo/.agent-run"]
        .map(|path| {
            provider
                .find(&format!("(allow file-write* (subpath \"{path}\"))"))
                .expect(path)
        })
        .into_iter()
        .max()
        .expect("allow rules");
    for denied in [
        "(deny file-read* (subpath \"/Users/fixture/.ssh\"))",
        "(deny file-write* (subpath \"/Users/fixture/.kube\"))",
        "(deny file-read* (subpath \"/Users/fixture/Monero\"))",
        "(deny file-write* (subpath \"/Users/fixture/Archives/git-preservation\"))",
        "(deny file-read* (literal \"/Users/fixture/.cargo/credentials.toml\"))",
        "(deny file-read* (subpath \"/Users/fixture/.gnupg\"))",
        "(deny file-read* (subpath \"/Users/fixture/.azure\"))",
        "(deny file-read* (subpath \"/Users/fixture/.config/gh\"))",
    ] {
        assert!(
            provider.find(denied).expect(denied) > last_allow,
            "{denied}"
        );
    }
    plan.network = NetworkAccess::Local;
    let local = render(&plan);
    assert!(local.contains("(allow network-outbound (remote ip \"localhost:*\"))"));
    assert!(local.contains("(allow network-bind (local ip \"localhost:*\"))"));
    assert!(local.contains("\\.s\\.PGSQL\\."));
    assert!(!local.contains("\"*:*\""));
    plan.network = NetworkAccess::Open;
    let open = render(&plan);
    assert!(open.contains("(allow network-outbound (remote ip \"*:*\"))"));
    assert!(open.contains("PGSQL"));
    for path in [
        "/Users/fixture/.config/gh",
        "/Users/fixture/Library/Application Support/GitHub CLI",
    ] {
        let read = format!("(allow file-read* (subpath \"{path}\"))");
        let deny_read = format!("(deny file-read* (subpath \"{path}\"))");
        let deny_write = format!("(deny file-write* (subpath \"{path}\"))");
        assert!(!open.contains(&read), "{path}");
        assert!(open.contains(&deny_read), "{path}");
        assert!(open.contains(&deny_write), "{path}");
        assert!(!provider.contains(&read), "{path}");
        assert!(!local.contains(&read), "{path}");
        assert!(provider.contains(&deny_read), "{path}");
    }
    let ssh_read = "(deny file-read* (subpath \"/Users/fixture/.ssh\"))";
    assert!(
        open.find(ssh_read).expect("ssh deny") > open.rfind("(allow file-read*").expect("allow")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_sandbox_probe_runs_through_executor_with_isolated_environment() {
    let temp = crate::test_support::tempdir().expect("temporary isolated root");
    let root = provenant_os::fs::canonicalize(temp.path()).expect("canonical isolated root");
    let home = root.join("home");
    let state = root.join("state");
    let xdg = root.join("xdg");
    let claude = root.join("claude");
    let codex = root.join("codex");
    for directory in [&home, &state, &xdg, &claude, &codex] {
        std::fs::create_dir_all(directory).expect("isolated process directory");
    }
    let env = [
        ("HOME".into(), home.into_os_string()),
        ("PROVENANT_STATE_DIR".into(), state.into_os_string()),
        ("XDG_CONFIG_HOME".into(), xdg.into_os_string()),
        (
            "GIT_CONFIG_GLOBAL".into(),
            root.join("gitconfig").into_os_string(),
        ),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("CLAUDE_CONFIG_DIR".into(), claude.into_os_string()),
        ("CODEX_HOME".into(), codex.into_os_string()),
    ];

    let executor = SystemExecutor::new(2).expect("bounded utility executor");
    let cancel = CancelToken::new();
    let context = BackendContext {
        executor: &executor,
        cancel: &cancel,
        env: &env,
    };
    match system_backend(&context) {
        Ok(_) | Err(PermissionError::BackendNotAvailable(_)) => {}
        Err(error) => panic!("sandbox probe did not fail closed: {error}"),
    }
}

#[test]
fn linux_has_no_confinement_backend() {
    #[cfg(target_os = "linux")]
    {
        let executor = SystemExecutor::new(1).expect("bounded executor");
        let cancel = CancelToken::new();
        let context = BackendContext {
            executor: &executor,
            cancel: &cancel,
        };
        assert_eq!(
            system_backend(&context)
                .err()
                .map(|error| error.to_string()),
            Some(
                PermissionError::BackendNotAvailable(AvailabilityReason::UnsupportedPlatform)
                    .to_string()
            )
        );
    }
}

#[test]
fn network_endpoints_accept_only_sbpl_hosts_and_checked_ports() {
    for address in ["*", "localhost"] {
        assert!(
            validate_endpoint(&NetworkEndpoint {
                address: address.into(),
                port: 443,
            })
            .is_ok()
        );
    }
    for address in ["127.0.0.1", "::1", "example.test"] {
        assert!(
            validate_endpoint(&NetworkEndpoint {
                address: address.into(),
                port: 443,
            })
            .is_err()
        );
    }
    for port in [0, u64::from(u16::MAX) + 1] {
        assert!(
            validate_endpoint(&NetworkEndpoint {
                address: "localhost".into(),
                port,
            })
            .is_err()
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_open_reader_cannot_read_github_credentials() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
    use std::time::Duration;

    let temp = tempfile::tempdir()?;
    let root = provenant_os::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let scratch = root.join("scratch");
    std::fs::create_dir_all(home.join(".config/gh"))?;
    std::fs::create_dir(&scratch)?;
    let secret = home.join(".config/gh/hosts.yml");
    let control = scratch.join("control");
    std::fs::write(&secret, "fixture token")?;
    std::fs::write(&control, "control")?;
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    if let Err(error) = system_backend(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        env: &[],
    }) {
        provenant_test_support::skip_sandbox_dependent_test(format!(
            "native sandbox unavailable: {error}"
        ))?;
        return Ok(());
    }
    let subtree = |path: PathBuf| PathGrant {
        path,
        scope: PathScope::Subtree,
    };
    let exact = |path: &str| PathGrant {
        path: path.into(),
        scope: PathScope::Exact,
    };
    let plan = PlanPermissions {
        read_only: None,
        mode: AccessMode::ReadOnly,
        scratch: subtree(scratch.clone()),
        private_state: None,
        read_roots: vec![],
        protected_roots: vec![],
        secret_roots: vec![],
        selected_authentication: None,
        include_continuation: false,
        write_roots: vec![],
        network: NetworkAccess::Open,
        nested_confinement: false,
    };
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            process_executables: vec![exact("/bin/cat")],
            ..ProviderProfileSpec::default()
        },
        observations: BTreeMap::new(),
    };
    let source = render_profile(&plan, &profile, &[], &[], None, &home)?;
    let launch = ConfinedLaunch {
        enforcement: Enforcement::MacOsSandboxExec,
        profile: source,
        profile_digest: [0; 32],
        native_controls: vec![],
    };
    let read = |path: &Path| -> Result<_, Box<dyn std::error::Error>> {
        let argv = ["/bin/cat".into(), path.as_os_str().to_owned()];
        let command = launch.command(&argv, &provenant_core::AttemptId::from_bytes([9; 16]))?;
        let mut options = ExecOptions::new(&command, &scratch, Duration::from_secs(10));
        options.stdout = StreamPolicy::Capture { limit: 1024 };
        options.stderr = StreamPolicy::Discard;
        Ok(executor.execute(options)?)
    };
    let allowed = read(&control)?;
    assert_eq!(allowed.termination, Termination::Exit(0));
    assert_eq!(allowed.stdout.bytes, b"control");
    let denied = read(&secret)?;
    assert_eq!(denied.termination, Termination::Exit(1));
    assert!(denied.stdout.bytes.is_empty());
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn invalid_provider_endpoints_warn_and_preserve_open_network() -> Result<(), PermissionError> {
    let (mut plan, mut profile) = write_grants_fixture();
    plan.network = NetworkAccess::Open;
    profile.spec.process_executables.push(PathGrant {
        path: "/fixture/provider".into(),
        scope: PathScope::Exact,
    });
    profile.spec.provider_api = vec![
        NetworkEndpoint {
            address: "localhost".into(),
            port: 443,
        },
        NetworkEndpoint {
            address: "invalid\") (allow file-read* (subpath \"/".into(),
            port: 443,
        },
        NetworkEndpoint {
            address: "*".into(),
            port: 0,
        },
    ];
    validate_profile_spec_with_home(&profile.spec, Path::new("/fixture/home"), 128)?;
    let grants = GrantSet::build(&plan, &profile, Path::new("/fixture/home"), None)?;
    let source = render(&grants)?;
    assert!(source.contains("(allow network-outbound (remote tcp \"localhost:443\"))"));
    assert!(source.contains("(allow network-outbound (remote ip \"*:*\"))"));
    assert!(!source.contains("invalid"));
    assert!(!source.contains("remote tcp \"*:0\""));
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn invalid_discovery_reads_are_omitted_before_anchoring_and_rendering()
-> Result<(), PermissionError> {
    use std::os::unix::ffi::OsStringExt;
    let (plan, mut profile) = write_grants_fixture();
    profile.spec.process_executables.push(PathGrant {
        path: "/fixture/provider".into(),
        scope: PathScope::Exact,
    });
    let invalid_native = PathBuf::from(std::ffi::OsString::from_vec(
        b"/fixture/invalid-\xff/.agents".to_vec(),
    ));
    profile.spec.discovery_reads = vec![
        PathGrant {
            path: "/fixture/repo/.agents".into(),
            scope: PathScope::Exact,
        },
        PathGrant {
            path: "/fixture/home/.config/gh/hosts.yml".into(),
            scope: PathScope::Exact,
        },
        PathGrant {
            path: invalid_native.clone(),
            scope: PathScope::Exact,
        },
        PathGrant {
            path: "/fixture/unsafe/.codex".into(),
            scope: PathScope::Subtree,
        },
    ];
    validate_profile_spec_with_home(&profile.spec, Path::new("/fixture/home"), 128)?;
    let anchors = required_anchors(&plan, &profile, None);
    assert!(!anchors.contains(Path::new("/fixture/home/.config/gh")));
    assert!(!anchors.contains(invalid_native.parent().expect("fixture parent")));
    let grants = GrantSet::build(&plan, &profile, Path::new("/fixture/home"), None)?;
    let source = render(&grants)?;
    assert!(source.contains("(allow file-read* (literal \"/fixture/repo/.agents\"))"));
    assert!(!source.contains("hosts.yml"));
    assert!(!source.contains("/fixture/unsafe/.codex"));
    Ok(())
}
