use super::*;
use provenant_core::CancelToken;
use provenant_os::exec::SystemExecutor;
#[cfg(target_os = "macos")]
use provenant_os::exec::{ExecOptions, StreamPolicy, Termination};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
#[test]
fn native_sandbox_blocks_path_escape_probes() -> Result<(), Box<dyn std::error::Error>> {
    use crate::repo::{GitRefName, verify_worktree};
    use crate::utility::{GitMutation, execute_git_mutation};
    use std::os::unix::fs::symlink;

    let fixture = crate::test_support::Fixture::new()?;
    let root = provenant_os::fs::canonicalize(fixture._temp.path())?;
    let home = root.join("home");
    let state = root.join("state");
    let xdg = root.join("xdg");
    let provider_state = root.join("provider-state");
    for path in [&home, &state, &xdg, &provider_state] {
        std::fs::create_dir_all(path)?;
    }
    let keychain = home.join("Library/Keychains/login.keychain-db");
    std::fs::create_dir_all(keychain.parent().ok_or("keychain parent")?)?;
    std::fs::write(&keychain, b"isolated fake credential")?;
    let head = fixture.head()?;
    let selected_ref = GitRefName::branch("s3/native")?;
    let worktree_path = root.join("linked");
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
    let authority = WorktreeWriteAuthority::for_worktree(
        &linked,
        &selected_ref,
        &provenant_os::fs::canonicalize(&fixture.home)?,
    )?;
    prepare_writer_directories(
        &linked,
        &selected_ref,
        &authority,
        &provenant_os::fs::canonicalize(&fixture.home)?,
    )?;
    let scratch = root.join("scratch");
    std::fs::create_dir(&scratch)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
    }
    let protected_parent = root.join("protected-parent");
    let protected = protected_parent.join("protected");
    std::fs::create_dir_all(&protected)?;
    let protected_file = protected.join("secret");
    std::fs::write(&protected_file, b"protected")?;
    let nested_git = linked.path().join("sub/.git");
    std::fs::create_dir_all(&nested_git)?;
    std::fs::write(nested_git.join("config"), b"nested")?;
    let inside = linked.path().join("inside");
    std::fs::write(&inside, b"")?;
    let symlink_path = linked.path().join("symlink");
    symlink(&protected_file, &symlink_path)?;
    // Shared tool stores and dispatch write roots are writable; Cargo
    // credentials stay hidden inside an otherwise readable Cargo home.
    let cargo = provenant_os::fs::canonicalize(&fixture.home)?.join(".cargo");
    std::fs::create_dir_all(cargo.join("registry"))?;
    std::fs::create_dir_all(cargo.join("bin"))?;
    let cargo_credentials = cargo.join("credentials.toml");
    std::fs::write(&cargo_credentials, b"token")?;
    let project_locks = root.join("agent-run");
    std::fs::create_dir(&project_locks)?;
    let shared_locks = fixture.repo.root_path().join(".agent-run/locks");
    let configured_tools = root.join("configured-tools");
    std::fs::create_dir_all(&shared_locks)?;
    std::fs::create_dir(&configured_tools)?;
    let private = linked.repo().private_git_dir().to_owned();
    let common = linked.repo().common_git_dir().to_owned();
    let private_logs = private.join("logs");
    let common_heads = common.join("refs/heads/s3");
    let common_logs = common.join("logs/refs/heads/s3");
    for directory in [&private_logs, &common_heads, &common_logs] {
        std::fs::create_dir_all(directory)?;
    }
    // Probe existing files so a denial cannot be an absent parent.
    for relative in [
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "logs/refs/remotes/origin/HEAD",
        "refs/remotes/upstream/HEAD",
        "logs/refs/remotes/upstream/HEAD",
    ] {
        let path = common.join(relative);
        std::fs::create_dir_all(path.parent().ok_or("remote ref parent")?)?;
        std::fs::write(path, b"ref: refs/remotes/origin/main\n")?;
    }
    let executables = [
        provenant_os::fs::canonicalize(Path::new("/usr/bin/touch"))?,
        provenant_os::fs::canonicalize(Path::new("/bin/mv"))?,
        provenant_os::fs::canonicalize(Path::new("/bin/ln"))?,
        provenant_os::fs::canonicalize(Path::new("/bin/sh"))?,
        provenant_os::fs::canonicalize(Path::new("/bin/cat"))?,
        provenant_os::fs::canonicalize(Path::new("/usr/sbin/sysctl"))?,
        provenant_os::fs::canonicalize(Path::new("/usr/bin/perl"))?,
    ];

    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            state_writes: vec![PathGrant {
                path: provider_state.clone(),
                scope: PathScope::Subtree,
            }],
            deny_keychain_access: true,
            allow_process_fork: true,
            allow_database_ipc: true,
            runtime_reads: [cargo.clone(), configured_tools.clone(), private.clone()]
                .map(|path| PathGrant {
                    path,
                    scope: PathScope::Subtree,
                })
                .to_vec(),
            tool_writes: [cargo.join("registry"), shared_locks.clone()]
                .map(|path| PathGrant {
                    path,
                    scope: PathScope::Subtree,
                })
                .to_vec(),
            tool_exec_roots: ["/bin", "/usr/bin", "/usr/libexec"]
                .map(|path| PathGrant {
                    path: path.into(),
                    scope: PathScope::Subtree,
                })
                .into_iter()
                .chain(std::iter::once(PathGrant {
                    path: configured_tools.clone(),
                    scope: PathScope::Subtree,
                }))
                .collect(),
            process_executables: executables
                .iter()
                .cloned()
                .map(|path| PathGrant {
                    path,
                    scope: PathScope::Exact,
                })
                .collect(),
            ..ProviderProfileSpec::default()
        },
        observations: [
            (FixtureOperation::DeclaredRead, FixtureOutcome::Allowed),
            (FixtureOperation::UndeclaredRead, FixtureOutcome::Denied),
            (FixtureOperation::UndeclaredWrite, FixtureOutcome::Denied),
            (FixtureOperation::SecretRead, FixtureOutcome::Denied),
            (FixtureOperation::ProtectedWrite, FixtureOutcome::Denied),
            (FixtureOperation::OtherNetwork, FixtureOutcome::Denied),
            (
                FixtureOperation::DeclaredProcessExec,
                FixtureOutcome::Allowed,
            ),
            (
                FixtureOperation::UndeclaredProcessExec,
                FixtureOutcome::Denied,
            ),
            (FixtureOperation::ProcessFork, FixtureOutcome::Allowed),
            (FixtureOperation::StateWrite, FixtureOutcome::Allowed),
            (FixtureOperation::RuntimeRead, FixtureOutcome::Allowed),
            (FixtureOperation::ShellExec, FixtureOutcome::Allowed),
            (FixtureOperation::KeychainTool, FixtureOutcome::Denied),
            (FixtureOperation::KeychainRead, FixtureOutcome::Denied),
            (
                FixtureOperation::ProtectedInstructionRead,
                FixtureOutcome::Allowed,
            ),
            (FixtureOperation::WorktreeWrite, FixtureOutcome::Allowed),
            (FixtureOperation::GitIndexWrite, FixtureOutcome::Denied),
            (FixtureOperation::GitHeadWrite, FixtureOutcome::Denied),
            (FixtureOperation::GitObjectsWrite, FixtureOutcome::Denied),
            (FixtureOperation::SelectedRefWrite, FixtureOutcome::Denied),
            (FixtureOperation::GitConfigWrite, FixtureOutcome::Denied),
            (FixtureOperation::OtherRefWrite, FixtureOutcome::Denied),
        ]
        .into(),
    };
    let cache = authority.cargo_cache.path.clone();
    let plan = PlanPermissions {
        read_only: None,
        private_state: None,
        mode: AccessMode::WorktreeWrite(Box::new(authority)),
        scratch: PathGrant {
            path: scratch.clone(),
            scope: PathScope::Subtree,
        },
        read_roots: Vec::new(),
        protected_roots: [protected.clone(), nested_git.clone()]
            .map(|path| PathGrant {
                path,
                scope: PathScope::Subtree,
            })
            .to_vec(),
        secret_roots: Vec::new(),
        selected_authentication: None,
        include_continuation: false,
        write_roots: vec![PathGrant {
            path: project_locks.clone(),
            scope: PathScope::Subtree,
        }],
        network: NetworkAccess::Provider,
        nested_confinement: false,
    };

    let mut anchor_paths = vec![
        root.clone(),
        cargo.clone(),
        shared_locks.clone(),
        configured_tools.clone(),
        scratch.clone(),
        cache.clone(),
        provider_state.clone(),
        protected.clone(),
        nested_git.clone(),
        linked.path().to_owned(),
        private.clone(),
        common.clone(),
        common.join("info"),
        common.join("objects"),
        private_logs,
        common_heads,
        common_logs,
    ];
    let executable_directories = executables
        .iter()
        .filter_map(|path| path.parent().map(Path::to_owned))
        .collect::<BTreeSet<_>>();
    anchor_paths.extend(executable_directories);
    let directories = anchor_paths
        .iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: &provenant_os::fs::canonicalize(&fixture.home)?,
        directories: &directories,
        worktree: Some((&linked, &selected_ref)),
    };
    let executor = SystemExecutor::new(2)?;
    let cancel = CancelToken::new();
    let env = [
        ("HOME".into(), home.into_os_string()),
        ("PROVENANT_STATE_DIR".into(), state.clone().into_os_string()),
        ("XDG_CONFIG_HOME".into(), xdg.into_os_string()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        (
            "PROVENANT_PRIMARY_CHECKOUT".into(),
            fixture.repo.root_path().as_os_str().to_owned(),
        ),
        (
            "PROVENANT_WORKTREE_GITDIR".into(),
            private.as_os_str().to_owned(),
        ),
    ];
    let (baseline, baseline_diagnostic) = probe_sandbox_with_diagnostic(&BackendContext {
        executor: &fixture.executor,
        cancel: &CancelToken::new(),
        env: &env,
    })?;
    if !matches!(baseline, Availability::Available { .. }) {
        let diagnostic = baseline_diagnostic
            .as_deref()
            .unwrap_or("no command outcome was captured");
        provenant_test_support::skip_sandbox_dependent_test(format!(
            "the required native sandbox baseline is unavailable: {baseline:?}; {diagnostic}"
        ))?;
        return Ok(());
    }
    let backend = MacOsSandboxBackend {
        ready: true,
        limits: ProfileLimits::default(),
    };
    // Existing hard links in the scratch, the worktree and its ignored
    // build and dependency trees do not refuse the launch: the sandbox
    // denies new links, and the few Git control files are checked directly.
    let alias = scratch.join("hardlink-probe");
    std::fs::hard_link(&protected_file, &alias)?;
    let hardlink_path = linked.path().join("hardlink");
    std::fs::write(linked.path().join(".gitignore"), "target/\nnode_modules/\n")?;
    let aliases = [
        "target/debug/object.o",
        "node_modules/pkg/file",
        "node_modules/pkg/alias",
    ]
    .map(|path| linked.path().join(path));
    for alias in &aliases {
        std::fs::create_dir_all(alias.parent().ok_or("alias parent")?)?;
        std::fs::hard_link(&protected_file, alias)?;
    }
    let compiled = backend.compile(&plan, &profile, &anchors)?;
    let sbpl = String::from_utf8(compiled.profile)?;
    assert!(!sbpl.contains("hardlink-probe"));
    for alias in &aliases {
        assert!(!sbpl.contains(&alias.display().to_string()));
    }
    assert!(!sbpl.contains("file-write-unlink"));
    std::fs::remove_file(alias)?;
    // A second name for a Git control file is the one link that refuses.
    // (Repository revalidation already refuses a second name for `config`.)
    std::fs::create_dir_all(common.join("hooks"))?;
    let hook = common.join("hooks/pre-commit");
    std::fs::write(&hook, b"#!/bin/sh\n")?;
    let hook_alias = linked.path().join("hook-alias");
    std::fs::hard_link(&hook, &hook_alias)?;
    assert!(matches!(
        backend.compile(&plan, &profile, &anchors),
        Err(PermissionError::GitControlHardLinked(path)) if path == hook
    ));
    std::fs::remove_file(hook_alias)?;
    std::fs::remove_file(hook)?;
    assert!(sbpl.contains("(deny file-link)"));
    assert!(sbpl.contains("(allow sysctl-read)"));
    // A writer may run what it builds into a write root.
    assert!(sbpl.contains(&format!(
        "(allow process-exec (subpath \"{}\"))",
        project_locks.display()
    )));
    // A read-only lane over the same grants, for the agent socket probe.
    let read_only_plan = PlanPermissions {
        mode: AccessMode::ReadOnly,
        ..plan.clone()
    };
    let read_only_anchors = Anchors {
        worktree: None,
        ..anchors
    };
    let read_only_sbpl = String::from_utf8(
        backend
            .compile(&read_only_plan, &profile, &read_only_anchors)?
            .profile,
    )?;
    let run_in = |sbpl: &str, program: &Path, args: &[&Path]| {
        let mut argv = vec![
            OsString::from(SANDBOX_EXEC),
            OsString::from("-p"),
            OsString::from(sbpl),
            OsString::from(program.as_os_str()),
        ];
        argv.extend(args.iter().map(|path| path.as_os_str().to_owned()));
        let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(3));
        options.env = Some(&env);
        options.cancel = Some(&cancel);
        options.stdout = StreamPolicy::Capture { limit: 1024 };
        options.stderr = StreamPolicy::Capture { limit: 1024 };
        options.terminate_grace = CLEANUP_TIMEOUT;
        options.reap_timeout = CLEANUP_TIMEOUT;
        executor.execute(options).expect("bounded sandbox child")
    };
    let run = |program: &Path, args: &[&Path]| run_in(&sbpl, program, args);
    // The per-user peer agent relays to fresh `fabric peer` children in
    // the login session, so no lane may connect to its socket. The
    // unconfined connection proves the listener; the system DNS socket
    // proves the confined client runs and can connect where allowed.
    let agent = state.join("peer-agent");
    std::fs::create_dir_all(&agent)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700))?;
    }
    let peer_socket = agent.join("peer.sock");
    let _peer_listener = std::os::unix::net::UnixListener::bind(&peer_socket)?;
    std::os::unix::net::UnixStream::connect(&peer_socket)?;
    let connect = |sbpl: &str, socket: &Path| {
        run_in(
            sbpl,
            &executables[6],
            &[
                Path::new("-MIO::Socket::UNIX"),
                Path::new("-e"),
                Path::new("IO::Socket::UNIX->new(Peer => $ARGV[0]) or die \"connect: $!\\n\""),
                socket,
            ],
        )
    };
    let system_socket = Path::new("/private/var/run/mDNSResponder");
    for (name, sbpl) in [("writer", &sbpl), ("read-only", &read_only_sbpl)] {
        let outcome = connect(sbpl, system_socket);
        assert!(
            successful_probe(&outcome),
            "{name} system socket: {:?}; {}",
            outcome.termination,
            String::from_utf8_lossy(&outcome.stderr.bytes)
        );
        let mut audit =
            sysv_audit::SysvAudit::new(&executor, &plan.scratch.path.join(format!("sysv-{name}")))?;
        let script = format!(
            "{}{}",
            sysv_audit::PERL_JOURNAL,
            r#"my ($shm, $sem);
END {
    my @errors;
    push @errors, "shmctl: $!\n" if defined $shm && !shmctl($shm, IPC_RMID, 0);
    push @errors, "semctl: $!\n" if defined $sem && !semctl($sem, 0, IPC_RMID, 0);
    die join("", @errors) if @errors;
}
$shm = shmget(IPC_PRIVATE, 56, IPC_CREAT | 0600);
die "shmget: $!\n" unless defined $shm;
own("m", $shm);
shmwrite($shm, "ok", 0, 2) or die "shmwrite: $!\n";
shmread($shm, my $value, 0, 2) or die "shmread: $!\n";
die "shared memory contents\n" unless $value eq "ok";
$sem = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
die "semget: $!\n" unless defined $sem;
own("s", $sem);
semop($sem, pack("s!3", 0, 1, 0)) or die "semop: $!\n";
"#
        );
        let outcome = run_in(
            sbpl,
            &executables[6],
            &[
                Path::new("-MIPC::SysV=IPC_PRIVATE,IPC_CREAT,IPC_RMID"),
                Path::new("-e"),
                Path::new(&script),
                audit.journal(),
            ],
        );
        audit.finish()?;
        assert!(
            successful_probe(&outcome),
            "{name} SysV IPC: {:?}; {}",
            outcome.termination,
            String::from_utf8_lossy(&outcome.stderr.bytes)
        );
        let outside = root.join("outside-write-roots");
        assert!(denied_probe(&run_in(sbpl, &executables[0], &[&outside])));
        assert!(!outside.exists(), "{name} wrote outside its writable roots");
    }

    let shell_outcome = run(&executables[3], &[Path::new("-c"), Path::new("printf ok")]);
    assert!(
        successful_probe(&shell_outcome),
        "shell: {:?}; {}",
        shell_outcome.termination,
        String::from_utf8_lossy(&shell_outcome.stderr.bytes)
    );
    let memory_outcome = run(&executables[5], &[Path::new("-n"), Path::new("hw.memsize")]);
    assert!(
        successful_probe(&memory_outcome)
            && String::from_utf8_lossy(&memory_outcome.stdout.bytes)
                .trim()
                .parse::<u64>()
                .is_ok_and(|bytes| bytes > 0),
        "hw.memsize sysctl-read failed under the compiled provider profile: {:?}; {}",
        memory_outcome.termination,
        String::from_utf8_lossy(&memory_outcome.stderr.bytes)
    );
    assert!(successful_probe(&run(
        &executables[0],
        &[&provider_state.join("transcript")]
    )));
    let cache_outcome = run(&executables[0], &[&cache.join("artifact")]);
    assert!(
        successful_probe(&cache_outcome),
        "writer cache: {:?}",
        cache_outcome.termination
    );
    let inside_outcome = run(&executables[0], &[&inside]);
    assert!(
        successful_probe(&inside_outcome),
        "worktree write failed: {:?}; stderr={}",
        inside_outcome.termination,
        String::from_utf8_lossy(&inside_outcome.stderr.bytes)
    );
    for name in [
        "index",
        "index.lock",
        "HEAD",
        "HEAD.lock",
        "FETCH_HEAD",
        "COMMIT_EDITMSG",
        "MERGE_HEAD",
        "ORIG_HEAD",
        "next-index-123.lock",
    ] {
        let outcome = run(&executables[0], &[&private.join(name)]);
        assert!(
            denied_probe(&outcome),
            "actual linked metadata {name} must be immutable: {:?}; {}",
            outcome.termination,
            String::from_utf8_lossy(&outcome.stderr.bytes)
        );
    }
    for relative in [
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "logs/refs/remotes/origin/HEAD",
    ] {
        assert!(!successful_probe(&run(
            &executables[0],
            &[&common.join(relative)]
        )));
    }
    // Package managers link into node_modules; a link grants nothing,
    // since the sandbox checks the resolved path.
    let new_link = linked.path().join("new-link");
    // A build script built into a declared write root (a shared
    // CARGO_TARGET_DIR) runs; the sandbox checks the exec of the file.
    let build_script = project_locks.join("build-script-main");
    let configured_script = configured_tools.join("tool");
    let scratch_script = scratch.join("build-script-main");
    let undeclared_target = root.join("undeclared-target");
    std::fs::create_dir_all(&undeclared_target)?;
    let undeclared_script = undeclared_target.join("build-script-main");
    {
        use std::os::unix::fs::PermissionsExt;
        for script in [
            &build_script,
            &scratch_script,
            &undeclared_script,
            &configured_script,
        ] {
            std::fs::write(script, "#!/bin/sh\nexit 0\n")?;
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    for profile in [&sbpl, &read_only_sbpl] {
        let identity = run_in(
            profile,
            &executables[3],
            &[
                Path::new("-c"),
                Path::new(
                    "test -d \"$PROVENANT_PRIMARY_CHECKOUT\" && /bin/cat \"$PROVENANT_WORKTREE_GITDIR/HEAD\"",
                ),
            ],
        );
        assert!(successful_probe(&identity));
        assert!(!successful_probe(&run_in(
            profile,
            &executables[0],
            &[&private.join("HEAD")]
        )));
        assert!(successful_probe(&run_in(profile, &configured_script, &[])));
        assert!(!successful_probe(&run_in(
            profile,
            &executables[0],
            &[&configured_script]
        )));
    }
    // The configured shared directory is writable but is not an executable
    // root. Its lock is the same host inode, rather than a lane-local copy.
    let shared_lock = shared_locks.join("heavy.lock");
    let lock_directory = provenant_os::fs::Directory::open_anchor(&shared_locks)?;
    let held = lock_directory.try_lock(
        std::ffi::OsStr::new("heavy.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let lock_probe = |profile: &str| {
        run_in(
            profile,
            &executables[6],
            &[
                Path::new("-e"),
                Path::new(
                    "open(my $f, '>>', $ARGV[0]) or die $!; flock($f, 2|4) or exit 35; print qq(locked\\n);",
                ),
                &shared_lock,
            ],
        )
    };
    assert!(!successful_probe(&lock_probe(&sbpl)));
    drop(held);
    assert!(successful_probe(&lock_probe(&sbpl)));
    // A lane-created lock excludes the host until the lane releases it.
    let lane_ready = shared_locks.join("lane-ready");
    let host_started = shared_locks.join("host-started");
    let host_acquired = shared_locks.join("host-acquired");
    let lane_lock = shared_locks.join("lane-created.lock");
    let release_path = shared_locks.join("release.lock");
    let release = lock_directory.try_lock(
        std::ffi::OsStr::new("release.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
        let lane = scope.spawn(|| {
            let argv = [OsString::from(SANDBOX_EXEC), "-p".into(), sbpl.clone().into(), executables[6].as_os_str().to_owned(), "-e".into(),
                "open(my $f, '>>', $ARGV[0]) or die $!; flock($f, 2) or die $!; open(my $r, '>', $ARGV[1]) or die $!; close($r); open(my $g, '>>', $ARGV[2]) or die $!; flock($g, 2) or die $!;".into(),
                lane_lock.as_os_str().to_owned(), lane_ready.as_os_str().to_owned(), release_path.as_os_str().to_owned()];
            let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(20));
            options.env = Some(&env);
            options.stderr = StreamPolicy::Capture { limit: 1024 };
            executor.execute(options)
        });
        provenant_test_support::wait_for(
            "lane lock",
            Duration::from_secs(10),
            || lane_ready.exists(),
            |ready| *ready,
        )?;
        assert!(
            lock_directory
                .try_lock(
                    std::ffi::OsStr::new("lane-created.lock"),
                    provenant_os::fs::LockMode::Exclusive
                )
                .is_err()
        );
        let host = scope.spawn(|| {
            let argv = [executables[6].as_os_str().to_owned(), "-e".into(),
                "open(my $f, '>>', $ARGV[0]) or die $!; open(my $s, '>', $ARGV[1]) or die $!; close($s); flock($f, 2) or die $!; open(my $a, '>', $ARGV[2]) or die $!;".into(),
                lane_lock.as_os_str().to_owned(), host_started.as_os_str().to_owned(), host_acquired.as_os_str().to_owned()];
            let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(20));
            options.stderr = StreamPolicy::Capture { limit: 1024 };
            executor.execute(options)
        });
        provenant_test_support::wait_for(
            "host contender",
            Duration::from_secs(10),
            || host_started.exists(),
            |started| *started,
        )?;
        assert!(
            !host_acquired.exists(),
            "host acquired a lock held by the lane"
        );
        drop(release);
        let lane_outcome = lane.join().map_err(|_| "lane thread panicked")??;
        assert!(
            successful_probe(&lane_outcome),
            "lane lock: {lane_outcome:?}"
        );
        let host_outcome = host.join().map_err(|_| "host thread panicked")??;
        assert!(
            successful_probe(&host_outcome) && host_acquired.exists(),
            "host lock: {host_outcome:?}"
        );
        Ok(())
    })?;
    for (name, outcome) in [
        (
            "tool store write",
            run(&executables[0], &[&cargo.join("registry/crate")]),
        ),
        (
            "write root",
            run(&executables[0], &[&project_locks.join("lock")]),
        ),
        ("write root exec", run(&build_script, &[])),
        ("attempt tmp exec", run(&scratch_script, &[])),
        (
            "child signal",
            run(
                &executables[3],
                &[
                    Path::new("-c"),
                    Path::new(
                        "/bin/sleep 1 & child=$!; kill -TERM \"$child\"; result=$?; wait \"$child\"; exit \"$result\"",
                    ),
                ],
            ),
        ),
        (
            "descriptor read",
            run(
                &executables[3],
                &[
                    Path::new("-c"),
                    Path::new("test \"$(printf ok | /bin/cat /dev/fd/0)\" = ok"),
                ],
            ),
        ),
        (
            "new symlink in worktree",
            run(
                &executables[2],
                &[Path::new("-s"), &protected_file, &new_link],
            ),
        ),
    ] {
        assert!(
            successful_probe(&outcome),
            "{name}: {:?}",
            outcome.termination
        );
    }
    let mut probes = vec![
        ("writer peer agent socket", connect(&sbpl, &peer_socket)),
        (
            "read-only peer agent socket",
            connect(&read_only_sbpl, &peer_socket),
        ),
        ("undeclared target exec", run(&undeclared_script, &[])),
        (
            "parent signal probe",
            run(
                &executables[3],
                &[Path::new("-c"), Path::new("kill -0 \"$PPID\"")],
            ),
        ),
        (
            "git config write",
            run(&executables[0], &[&common.join("config")]),
        ),
        (
            "packed refs write",
            run(&executables[0], &[&common.join("packed-refs")]),
        ),
        (
            "other branch write",
            run(&executables[0], &[&common.join("refs/heads/main")]),
        ),
        (
            "other remote ref write",
            run(
                &executables[0],
                &[&common.join("refs/remotes/upstream/HEAD")],
            ),
        ),
        (
            "other remote reflog write",
            run(
                &executables[0],
                &[&common.join("logs/refs/remotes/upstream/HEAD")],
            ),
        ),
        (
            "git hooks write",
            run(&executables[0], &[&common.join("hooks/pre-push")]),
        ),
        (
            "cargo credentials read",
            run(&executables[4], &[&cargo_credentials]),
        ),
        (
            "cargo bin write",
            run(&executables[0], &[&cargo.join("bin/cargo")]),
        ),
        (
            "write through new symlink",
            run(&executables[0], &[&new_link]),
        ),
        (
            "keychain tool",
            run(Path::new("/usr/bin/security"), &[Path::new("help")]),
        ),
        ("keychain read", run(&executables[4], &[&keychain])),
        (
            "sibling cache",
            run(&executables[0], &[&cache.with_file_name("other-lane")]),
        ),
        ("protected write", run(&executables[0], &[&protected_file])),
        ("symlink write", run(&executables[0], &[&symlink_path])),
        (
            "new hardlink",
            run(&executables[2], &[&protected_file, &hardlink_path]),
        ),
        (
            "nested git write",
            run(&executables[0], &[&nested_git.join("config")]),
        ),
    ];
    let moved_protected_parent = root.join("protected-parent-moved");
    probes.push((
        "protected ancestor rename",
        run(
            &executables[1],
            &[&protected_parent, &moved_protected_parent],
        ),
    ));
    let moved_worktree = root.join("linked-moved");
    probes.push((
        "worktree root rename",
        run(&executables[1], &[&worktree_path, &moved_worktree]),
    ));
    let roots_remained = protected_parent.exists() && worktree_path.exists();

    // Simulate a root swap in the gap between compiling the pathname
    // policy and starting its child: the authorised worktree spelling now
    // resolves to the protected directory.
    std::fs::rename(&worktree_path, &moved_worktree)?;
    symlink(&protected, &worktree_path)?;
    let swapped_probe = worktree_path.join("root-swap-probe");
    probes.push((
        "worktree root swap",
        run(&executables[0], &[&swapped_probe]),
    ));
    let root_swap_did_not_write = !protected.join("root-swap-probe").exists();
    std::fs::remove_file(&worktree_path)?;
    std::fs::rename(&moved_worktree, &worktree_path)?;
    assert!(
        roots_remained
            && root_swap_did_not_write
            && !hardlink_path.exists()
            && probes.iter().all(|(_, outcome)| denied_probe(outcome)),
        "native sandbox probes failed: {:?}; roots_remained={roots_remained}; root_swap_did_not_write={root_swap_did_not_write}; hardlink_absent={}",
        probes
            .iter()
            .filter(|(_, outcome)| !denied_probe(outcome))
            .map(|(name, outcome)| (*name, outcome.termination))
            .collect::<Vec<_>>(),
        !hardlink_path.exists(),
    );
    Ok(())
}
