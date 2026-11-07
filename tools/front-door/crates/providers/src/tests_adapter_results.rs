use super::*;
use provenant_records::ResolvedRoute;
use serde::Deserialize;

#[test]
fn command_output_is_a_bounded_tail_with_an_exit_verdict() {
    let adapter = REGISTRY
        .iter()
        .find(|entry| entry.adapter.id() == "command")
        .expect("command adapter is registered")
        .adapter;
    for (code, timed_out, verdict, status) in [
        (Some(0), false, "PASS", OutcomeStatus::Success),
        (Some(7), false, "FAIL", OutcomeStatus::Failed),
        (Some(0), true, "FAIL", OutcomeStatus::Partial),
        (None, false, "FAIL", OutcomeStatus::Failed),
    ] {
        let mut state = adapter.stream(None).unwrap();
        let bytes = [vec![b'x'; 100_000], b"\xff\nlast line".to_vec()].concat();
        for chunk in bytes.chunks(317) {
            assert!(adapter.observe(&mut state, chunk).unwrap().is_empty());
        }
        let outcome = adapter
            .finish(state, ExitObservation { code, timed_out })
            .unwrap();
        assert_eq!(outcome.status(), status);
        assert!(outcome.model().is_none());
        assert!(outcome.session().is_none());
        let result: String = outcome
            .final_events()
            .iter()
            .filter_map(|event| {
                if let ProviderEvent::Text(text) = event {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(result.len() < 66_000);
        assert!(result.contains("retained tail"));
        assert!(result.ends_with(&format!("| last line\nVERDICT: {verdict}\n")));
    }
}

#[test]
fn command_output_cannot_supply_the_graph_verdict() {
    let adapter = &command::Command;
    let mut state = adapter.stream(None).unwrap();
    adapter
        .observe(&mut state, b"quoted vErDiCt: FAIL\nVERDICT: FAIL\n")
        .unwrap();
    let outcome = adapter
        .finish(
            state,
            ExitObservation {
                code: Some(0),
                timed_out: false,
            },
        )
        .unwrap();
    let result: String = outcome
        .final_events()
        .iter()
        .filter_map(|event| {
            if let ProviderEvent::Text(text) = event {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        result
            .lines()
            .filter(|line| line.starts_with("VERDICT:"))
            .collect::<Vec<_>>(),
        ["VERDICT: PASS"]
    );
    assert!(result.contains("| VERDICT\\: FAIL"));
    let mut capture = provenant_records::result_capture::ResultCapture::new(None);
    capture.push(result.as_bytes());
    let captured = capture.finish();
    assert_eq!(captured.verdict.as_deref(), Some("VERDICT: PASS"));
    assert!(!captured.verdict_invalid);
}

#[test]
fn command_prepare_preserves_invocation_and_literal_argv_without_a_rust_install() {
    let adapter = &command::Command;
    let mut route = route("command", "");
    route.account_key = None;
    route.effort = None;
    let plan = read_only_plan("/fixture", None);
    let prompt = Prompt::new("/bin/sh -c 'touch forbidden'".into());
    let argv = vec![
        "cargo".into(),
        "fmt".into(),
        "$(touch forbidden); a b".into(),
    ];
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.command = Some(command::Invocation {
        argv: &argv,
        runtimes: &[],
        program: Path::new("/fixture/home/.cargo/bin/cargo"),
    });
    let spec = adapter.prepare(input).unwrap();
    assert_eq!(spec.argv[4], "/fixture/home/.cargo/bin/cargo");
    assert_eq!(spec.argv[5], "fmt");
    assert_eq!(spec.argv[6], "$(touch forbidden); a b");
    assert_eq!(spec.argv.len(), 7);
    assert!(spec.profile.spec.provider_api.is_empty());
    assert!(!spec.profile.spec.loopback_server);
    assert!(spec.profile.spec.authentication.is_empty());
    assert!(spec.profile.spec.state_writes.is_empty());
    assert!(
        !spec
            .profile
            .spec
            .runtime_reads
            .iter()
            .any(|grant| grant.path == Path::new("/fixture/home/.rustup"))
    );
    assert_eq!(spec.stdin, Stdin::Null);
    let source = provenant_confinement::permissions::snapshot::render(
        plan.permissions(),
        &spec.profile,
        Path::new("/fixture/home"),
    )
    .unwrap();
    assert!(source.contains("(deny default)"));
    assert!(source.rfind("(deny network*)").unwrap() > source.rfind("(allow network-").unwrap());
    assert!(source.contains("(deny file-read* (subpath \"/fixture/home/.ssh\"))"));
    assert!(source.contains("(deny file-write* (subpath \"/fixture/home/.ssh\"))"));
    assert!(!source.contains("(allow file-write* (subpath \"/fixture/repo\"))"));
    assert!(spec.profile.spec.deny_keychain_access);
    assert!(source.contains("(deny process-exec (literal \"/usr/bin/security\"))"));
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    assert!(matches!(
        adapter.prepare(input),
        Err(ProviderError::UnsupportedPolicy)
    ));
    route.model = "invented-model".into();
    input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.command = Some(command::Invocation {
        argv: &argv,
        runtimes: &[],
        program: Path::new("/fixture/tool"),
    });
    assert!(matches!(
        adapter.prepare(input),
        Err(ProviderError::UnsupportedPolicy)
    ));
}

#[test]
fn command_credentials_stay_denied_in_every_network_mode() {
    use provenant_confinement::permissions::{NetworkAccess, PathGrant, PathScope};
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};

    let base = read_only_plan("/fixture", None);
    let mut route = route("command", "");
    route.account_key = None;
    let prompt = Prompt::new("check".into());
    let argv = vec!["check".into()];
    for network in [
        NetworkAccess::Provider,
        NetworkAccess::Local,
        NetworkAccess::Open,
    ] {
        let mut permissions = base.permissions().clone();
        permissions.network = network;
        let plan = ConfinedPlan::new(PlanRequest {
            scope: base.scope(),
            permissions,
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        })
        .unwrap();
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.command = Some(command::Invocation {
            argv: &argv,
            program: Path::new("/fixture/tool"),
            runtimes: &[],
        });
        let mut spec = command::Command.prepare(input).unwrap();
        assert!(spec.profile.spec.deny_credential_access);
        assert!(spec.profile.spec.deny_keychain_access);
        // An overlapping allow must never override the final credential denials.
        for relative in [
            ".config/gh",
            "Library/Application Support/GitHub CLI",
            "Library/Keychains",
            ".ssh",
            ".aws",
            ".azure",
            ".gnupg",
            ".kube",
            ".secrets",
            ".codex",
            ".claude",
            ".cursor",
            ".gemini",
            ".config/gcloud",
            ".config/cursor",
            ".config/opencode",
            ".local/share/opencode",
            "Monero",
            "Archives/git-preservation",
            ".cargo/credentials",
            ".cargo/credentials.toml",
        ] {
            let path = Path::new("/fixture/home").join(relative);
            spec.profile.spec.runtime_reads.push(PathGrant {
                path: path.clone(),
                scope: PathScope::Subtree,
            });
            let source = provenant_confinement::permissions::snapshot::render(
                plan.permissions(),
                &spec.profile,
                Path::new("/fixture/home"),
            )
            .unwrap();
            let allow = source
                .find(&format!(
                    "(allow file-read* (subpath \"{}\"))",
                    path.display()
                ))
                .unwrap();
            for operation in ["file-read*", "file-write*"] {
                let scope = if relative.starts_with(".cargo/credentials") {
                    "literal"
                } else {
                    "subpath"
                };
                let denial = format!("(deny {operation} ({scope} \"{}\"))", path.display());
                assert!(
                    source.rfind(&denial).unwrap() > allow,
                    "{network:?}: {denial}"
                );
            }
        }
    }
}

#[test]
fn command_runtimes_grant_exact_execution_only() {
    let base = read_only_plan("/fixture", None);
    let mut route = route("command", "");
    route.account_key = None;
    let prompt = Prompt::new("check".into());
    let argv = vec!["./check.py".into()];
    let runtimes = vec!["/opt/check/bin/python3".into()];
    let mut input = fixture_input(&route, &base, &prompt, Path::new("/fixture/repo"), None);
    input.command = Some(command::Invocation {
        argv: &argv,
        program: Path::new("/fixture/repo/check.py"),
        runtimes: &runtimes,
    });
    let spec = command::Command.prepare(input).unwrap();
    let runtime = Path::new("/opt/check/bin/python3");
    assert!(spec.profile.spec.tool_exec_roots.iter().any(|grant| {
        grant.path == runtime && grant.scope == provenant_confinement::permissions::PathScope::Exact
    }));
    for grant in spec
        .profile
        .spec
        .runtime_reads
        .iter()
        .chain(&spec.profile.spec.process_executables)
        .chain(&spec.profile.spec.state_writes)
        .chain(&spec.profile.spec.tool_writes)
    {
        assert!(!runtime.starts_with(&grant.path));
    }
    let source = provenant_confinement::permissions::snapshot::render(
        base.permissions(),
        &spec.profile,
        Path::new("/fixture/home"),
    )
    .unwrap();
    assert!(source.contains("(allow process-exec (literal \"/opt/check/bin/python3\"))"));
    assert!(!source.contains("(subpath \"/opt/check"));
    assert!(!source.contains("(allow file-read* (literal \"/opt/check/bin/python3\"))"));
    assert!(!source.contains("(allow file-write* (literal \"/opt/check/bin/python3\"))"));
    assert!(source.contains("(deny file-read* (subpath \"/fixture/home/.config/gh\"))"));
}

#[test]
fn command_runtimes_include_only_exact_linked_library_reads()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::PathScope;

    let temp = tempfile::tempdir()?;
    let library = temp.path().join("libcustom.dylib");
    std::fs::write(&library, test_macho(&[]))?;
    let library = library.canonicalize()?;
    let name = library.to_str().ok_or("library path")?;
    // A thin Mach-O load command, enough for the production dependency reader.
    let mut image = vec![0; 32];
    image[..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
    image[16..20].copy_from_slice(&1_u32.to_le_bytes());
    let size = (24 + name.len() + 1).next_multiple_of(8);
    image[20..24].copy_from_slice(&u32::try_from(size)?.to_le_bytes());
    image.extend(0xc_u32.to_le_bytes());
    image.extend(u32::try_from(size)?.to_le_bytes());
    image.extend(24_u32.to_le_bytes());
    image.resize(32 + 24, 0);
    image.extend(name.as_bytes());
    image.resize(32 + size, 0);
    let runtime = temp.path().join("interpreter");
    std::fs::write(&runtime, image)?;
    let runtimes = vec![runtime.clone()];
    let plan = read_only_plan("/fixture", None);
    let mut route = route("command", "");
    route.account_key = None;
    let prompt = Prompt::new("check".into());
    let argv = vec!["./check-script".into()];
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.command = Some(command::Invocation {
        argv: &argv,
        program: Path::new("/fixture/repo/check-script"),
        runtimes: &runtimes,
    });
    let spec = command::Command.prepare(input)?;
    assert!(
        spec.profile
            .spec
            .runtime_reads
            .iter()
            .any(|grant| { grant.path == library && grant.scope == PathScope::Exact })
    );
    assert!(!spec.profile.spec.runtime_reads.iter().any(|grant| {
        grant.path == runtime
            || library.starts_with(&grant.path) && grant.scope == PathScope::Subtree
    }));
    assert!(
        !spec
            .profile
            .spec
            .tool_writes
            .iter()
            .any(|grant| library.starts_with(&grant.path))
    );
    Ok(())
}

#[test]
fn prepared_writer_git_environment_is_private_for_every_adapter() -> Result<(), ProviderError> {
    use provenant_confinement::plan::ReadOnlyGrants;
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    for entry in REGISTRY.iter().filter(|entry| {
        provenant_records::AdapterSpec::get(entry.adapter.id())
            .is_some_and(|spec| spec.launch.model)
    }) {
        let id = entry.adapter.id();
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            let base = fixture_access_plan(
                access,
                provenant_records::AdapterSpec::get(id).and_then(|spec| spec.authentication),
                false,
            );
            let mut permissions = base.permissions().clone();
            if access == Access::ReadOnly {
                permissions.read_only =
                    Some(ReadOnlyGrants::for_test("/fixture/repo".into(), vec![]));
            }
            let plan = ConfinedPlan::new(PlanRequest {
                scope: base.scope(),
                permissions,
                codex: CodexPolicy::Clean,
                extensions: ExtensionPolicy::Deny,
                agy_state: None,
            })
            .unwrap();
            let spec = entry.adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                (id == "claude").then_some(&token),
            ))?;
            let bound = spec.git_wrapper.bound.as_ref();
            assert!(bound.is_some(), "{id}");
            if let Some(bound) = bound {
                assert_eq!(bound.worktree, Path::new("/fixture/repo"));
                for (key, path) in [
                    ("GIT_DIR", "/fixture/scratch/git-common"),
                    ("GIT_COMMON_DIR", "/fixture/scratch/git-common"),
                    ("GIT_WORK_TREE", "/fixture/repo"),
                    ("GIT_OBJECT_DIRECTORY", "/fixture/scratch/git-objects"),
                ] {
                    assert!(spec.env.iter().any(|(name, value)| name == key && value == path), "{id} {key}");
                    assert_eq!(
                        bound
                            .env
                            .iter()
                            .find(|(name, _)| name == key)
                            .map(|(_, value)| value.as_os_str()),
                        Some(std::ffi::OsStr::new(path)),
                        "{id} {key}"
                    );
                }
                for key in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_EMAIL"] {
                    assert!(spec.env.iter().all(|(name, _)| name != key), "{id} {key}");
                }
            }
            {
                assert!(spec.env.iter().any(|(key, value)| {
                    key == "GIT_CONFIG_GLOBAL" && value == "/fixture/scratch/gitconfig"
                }));
                assert!(spec.setup.iter().any(|step| matches!(
                    step,
                    SetupStep::WriteFile { path, bytes }
                        if path == Path::new("/fixture/scratch/gitconfig")
                            && bytes.starts_with(b"[user]\n\tuseConfigOnly = true\n")
                )));
                let path = &spec.env.iter().find(|(key, _)| key == "PATH").unwrap().1;
                assert_eq!(
                    std::env::split_paths(path).next().unwrap(),
                    Path::new("/fixture/scratch/bin")
                );
            }
        }
    }
    Ok(())
}

#[test]
fn read_only_git_path_does_not_split_scratch_at_separator() -> Result<(), ProviderError> {
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let base = read_only_plan("/fixture", Some("keychain"));
    let mut permissions = base.permissions().clone();
    permissions.scratch.path = "/fixture/scratch:unexpected".into();
    let plan = ConfinedPlan::new(PlanRequest {
        scope: base.scope(),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .expect("fixture plan");
    let mut route = route("agy", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let launch = agy::Agy.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo"),
        None,
    ))?;
    let path = &launch
        .env
        .iter()
        .find(|(key, _)| key == "PATH")
        .expect("PATH")
        .1;
    let entries: Vec<_> = std::env::split_paths(path).collect();
    assert_eq!(
        entries,
        [
            std::path::PathBuf::from("/usr/bin"),
            std::path::PathBuf::from("/bin")
        ]
    );
    assert!(
        launch
            .profile
            .spec
            .process_executables
            .contains(&crate::launch::exact("/fixture/git/bin/git"))
    );
    Ok(())
}

#[test]
fn prepared_git_accepts_non_utf8_scratch() -> Result<(), ProviderError> {
    use provenant_confinement::plan::ReadOnlyGrants;
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    use std::os::unix::ffi::OsStringExt;
    let base = read_only_plan("/fixture", None);
    let mut permissions = base.permissions().clone();
    permissions.scratch.path =
        std::ffi::OsString::from_vec(b"/fixture/scratch-\xff".to_vec()).into();
    permissions.read_only = Some(ReadOnlyGrants::for_test("/fixture/repo".into(), vec![]));
    let plan = ConfinedPlan::new(PlanRequest {
        scope: base.scope(),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .unwrap();
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    claude::Claude.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo"),
        Some(&token),
    ))?;
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)]
fn prepared_private_git_supports_commits_fetch_merge_and_push()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::{PathGrant, PathScope};
    use provenant_confinement::plan::ReadOnlyGrants;
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;

    for (entry, access) in REGISTRY
        .iter()
        .filter(|entry| {
            provenant_records::AdapterSpec::get(entry.adapter.id())
                .is_some_and(|spec| spec.launch.write)
        })
        .flat_map(|entry| {
            [Access::WorktreeWrite, Access::ReadOnly]
                .into_iter()
                .map(move |access| (entry, access))
        })
    {
        let id = entry.adapter.id();
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let repo = root.join("repo café: '\"\\\nend");
        let other = root.join("other");
        let linked = root.join("linked");
        let host_git = |cwd: &Path, args: &[&std::ffi::OsStr]| {
            let output = provenant_os::fixture::git_command_at("/usr/bin/git")
                .current_dir(cwd)
                .env_clear()
                .envs(provenant_os::fixture::fixture_git_settings())
                .env(
                    "PATH",
                    "/Library/Developer/CommandLineTools/usr/bin:/usr/bin:/bin",
                )
                .env("TMPDIR", &root)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .envs(std::env::var_os("DEVELOPER_DIR").map(|value| ("DEVELOPER_DIR", value)))
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{id}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        for dir in [&repo, &other] {
            std::fs::create_dir(dir)?;
            host_git(dir, &["init".as_ref(), "-q".as_ref()]);
            host_git(
                dir,
                &[
                    "commit".as_ref(),
                    "--allow-empty".as_ref(),
                    "-qm".as_ref(),
                    dir.as_os_str(),
                ],
            );
        }
        host_git(
            &repo,
            &[
                "-c".as_ref(),
                "protocol.file.allow=always".as_ref(),
                "submodule".as_ref(),
                "add".as_ref(),
                "-q".as_ref(),
                other.as_os_str(),
                "vendor/lib".as_ref(),
            ],
        );
        host_git(
            &repo,
            &["commit".as_ref(), "-qam".as_ref(), "submodule".as_ref()],
        );
        host_git(
            &repo,
            &[
                "worktree".as_ref(),
                "add".as_ref(),
                "-qb".as_ref(),
                "linked".as_ref(),
                linked.as_os_str(),
            ],
        );
        host_git(
            &repo,
            &[
                "remote".as_ref(),
                "add".as_ref(),
                "origin".as_ref(),
                other.as_os_str(),
            ],
        );
        let scratch = root.join("scratch");
        let base = fixture_access_plan(
            access,
            provenant_records::AdapterSpec::get(id).and_then(|spec| spec.authentication),
            false,
        );
        let mut permissions = base.permissions().clone();
        permissions.scratch.path = scratch.clone();
        permissions.read_roots = [&linked, &other, &repo]
            .into_iter()
            .map(|path| PathGrant {
                path: path.clone(),
                scope: PathScope::Subtree,
            })
            .collect();
        if access == Access::ReadOnly {
            permissions.read_only = Some(ReadOnlyGrants::for_test(linked.clone(), vec![]));
        } else {
            permissions.read_only = None;
            let mut mode = serde_json::to_value(&permissions.mode)?;
            mode["authority"]["common_git_dir"] = serde_json::to_value(repo.join(".git"))?;
            permissions.mode = serde_json::from_value(mode)?;
        }
        let plan = ConfinedPlan::new(PlanRequest {
            scope: base.scope(),
            permissions,
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        })?;
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        let prompt = Prompt::new("fixture".into());
        let token = fixture_token();
        let mut input = fixture_input(
            &route,
            &plan,
            &prompt,
            &linked,
            (id == "claude").then_some(&token),
        );
        let clt_git = Path::new("/Library/Developer/CommandLineTools/usr/bin/git");
        let git = if clt_git.is_file() {
            clt_git
        } else {
            Path::new("/usr/bin/git")
        };
        input.provenant.git = git;
        let spec = entry.adapter.prepare(input)?;
        for step in &spec.setup {
            match step {
                SetupStep::CreateDir(path)
                    if path.starts_with(scratch.join("git-objects"))
                        || path == &scratch.join("bin") =>
                {
                    std::fs::create_dir_all(path).unwrap()
                }
                SetupStep::WriteFile { path, bytes }
                    if path.starts_with(scratch.join("git-objects")) =>
                {
                    std::fs::write(path, bytes).unwrap()
                }
                SetupStep::WriteExecutable { path, bytes } if path == &scratch.join("bin/git") => {
                    std::fs::write(path, bytes).unwrap();
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
                }
                _ => {}
            }
        }
        {
            let objects = scratch.join("git-objects");
            std::fs::create_dir_all(objects.join("info"))?;
            std::fs::write(
                objects.join("info/alternates"),
                format!(
                    "\"{}\"\n",
                    repo.join(".git/objects")
                        .to_str()
                        .ok_or("alternate path")?
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('\n', "\\n")
                ),
            )?;
            let private_common = scratch.join("git-common");
            std::fs::create_dir_all(private_common.join("objects"))?;
            std::fs::create_dir_all(private_common.join("refs"))?;
            std::fs::write(
                private_common.join("config"),
                std::fs::read(repo.join(".git/config"))?,
            )?;
            std::fs::write(private_common.join("HEAD"), b"ref: refs/heads/linked\n")?;
            let index = host_git(
                &linked,
                &[
                    "rev-parse".as_ref(),
                    "--git-path".as_ref(),
                    "index".as_ref(),
                ],
            );
            std::fs::copy(
                std::str::from_utf8(&index)?.trim(),
                private_common.join("index"),
            )?;
            let refs = host_git(
                &repo,
                &[
                    "for-each-ref".as_ref(),
                    "--format=%(objectname) %(refname)".as_ref(),
                ],
            );
            for line in std::str::from_utf8(&refs)?.lines() {
                let (oid, name) = line.split_once(' ').ok_or("fixture ref")?;
                let path = private_common.join(name);
                std::fs::create_dir_all(path.parent().ok_or("fixture ref parent")?)?;
                std::fs::write(path, format!("{oid}\n"))?;
            }
        }
        let lane_git = |args: &[&std::ffi::OsStr]| {
            let output = Command::new("/bin/sh")
                .arg(scratch.join("bin/git"))
                .current_dir(&linked)
                .env_clear()
                .envs(spec.env.iter().cloned())
                .envs(provenant_os::fixture::fixture_git_settings())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{id}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.stderr.is_empty(),
                "{id}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        let bound_head = host_git(&linked, &["rev-parse".as_ref(), "HEAD".as_ref()]);
        assert_eq!(
            lane_git(&["rev-parse".as_ref(), "HEAD".as_ref()]),
            bound_head
        );
        assert_eq!(
            lane_git(&["log".as_ref(), "-1".as_ref(), "--format=%H".as_ref()]),
            bound_head
        );
        lane_git(&["cat-file".as_ref(), "-e".as_ref(), "HEAD^{commit}".as_ref()]);
        let other_head = host_git(&other, &["rev-parse".as_ref(), "HEAD".as_ref()]);
        // Native Git can inspect a second repository by explicitly removing
        // the lane's repository selectors; no command-routing parser is needed.
        let other_read = provenant_os::fixture::git_command_at(git)
            .current_dir(&other)
            .env_clear()
            .envs(spec.env.iter().cloned())
            .env_remove("GIT_DIR")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .args(["log", "-1", "--format=%H"])
            .output()?;
        assert!(other_read.status.success());
        assert_eq!(other_read.stdout, other_head);
        // The wrapper must discard its binding when Git selects another repository,
        // even after its environment was cleared by the caller.
        let other_via_wrapper = Command::new(scratch.join("bin/git"))
            .current_dir(&linked)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args(["-C", other.to_str().ok_or("other path")?, "rev-parse", "HEAD"])
            .output()?;
        assert!(other_via_wrapper.status.success());
        assert_eq!(other_via_wrapper.stdout, other_head);
        let explicit_other = Command::new(scratch.join("bin/git"))
            .current_dir(&linked)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .arg(format!("--git-dir={}", other.join(".git").display()))
            .args(["rev-parse", "HEAD"])
            .output()?;
        assert!(explicit_other.status.success());
        assert_eq!(explicit_other.stdout, other_head);
        if access == Access::ReadOnly {
            std::fs::write(other.join("reader-upstream"), "fetched content")?;
            host_git(&other, &["add".as_ref(), "reader-upstream".as_ref()]);
            host_git(&other, &["commit".as_ref(), "-qm".as_ref(), "reader upstream".as_ref()]);
            let direct = provenant_os::fixture::git_command_at(git)
                .current_dir(&linked).env_clear().envs(spec.env.iter().cloned())
                .envs(provenant_os::fixture::fixture_git_settings())
                .args(["-c", "protocol.file.allow=always", "fetch", "--quiet", "origin", "HEAD:refs/remotes/origin/reader"])
                .output()?;
            assert!(direct.status.success(), "{id}: {}", String::from_utf8_lossy(&direct.stderr));
            assert_eq!(lane_git(&["rev-parse".as_ref(), "FETCH_HEAD".as_ref()]), host_git(&other, &["rev-parse".as_ref(), "HEAD".as_ref()]));
            assert_eq!(host_git(&linked, &["rev-parse".as_ref(), "HEAD".as_ref()]), bound_head);
        }
        let input = scratch.join("input");
        std::fs::write(&input, "lane scratch object")?;
        let oid = lane_git(&["hash-object".as_ref(), "-w".as_ref(), input.as_os_str()]);
        let oid = std::str::from_utf8(&oid)?.trim();
        let relative = PathBuf::from(&oid[..2]).join(&oid[2..]);
        assert!(
            scratch.join("git-objects").join(&relative).is_file(),
            "{id}"
        );
        assert!(!repo.join(".git/objects").join(&relative).exists(), "{id}");
        if access == Access::WorktreeWrite {
            std::fs::write(linked.join("lane-blob"), "private committed blob")?;
            lane_git(&["add".as_ref(), "lane-blob".as_ref()]);
            lane_git(&[
                "commit".as_ref(),
                "-qm".as_ref(),
                "private lane commit".as_ref(),
                "--".as_ref(),
                "lane-blob".as_ref(),
            ]);
            assert_eq!(
                host_git(&linked, &["rev-parse".as_ref(), "HEAD".as_ref()]),
                bound_head,
                "{id} shared branch after private commit"
            );
            let head = lane_git(&["rev-parse".as_ref(), "HEAD".as_ref()]);
            let head = std::str::from_utf8(&head)?.trim();
            let relative = PathBuf::from(&head[..2]).join(&head[2..]);
            assert!(
                scratch.join("git-objects").join(&relative).is_file(),
                "{id}"
            );
            assert!(!repo.join(".git/objects").join(&relative).exists(), "{id}");
            lane_git(&["cat-file".as_ref(), "-e".as_ref(), "HEAD^{commit}".as_ref()]);
            assert_eq!(
                lane_git(&["log".as_ref(), "-1".as_ref(), "--format=%H".as_ref()]),
                format!("{head}\n").as_bytes()
            );
            std::fs::write(other.join("upstream-blob"), "fetched upstream blob")?;
            host_git(&other, &["add".as_ref(), "upstream-blob".as_ref()]);
            host_git(
                &other,
                &["commit".as_ref(), "-qm".as_ref(), "upstream blob".as_ref()],
            );
            lane_git(&[
                "-c".as_ref(),
                "protocol.file.allow=always".as_ref(),
                "fetch".as_ref(),
                "--quiet".as_ref(),
                "--refmap=".as_ref(),
                "origin".as_ref(),
                "HEAD".as_ref(),
            ]);
            assert_eq!(
                lane_git(&["rev-parse".as_ref(), "FETCH_HEAD".as_ref()]),
                host_git(&other, &["rev-parse".as_ref(), "HEAD".as_ref()]),
                "{id} fetched head",
            );
            lane_git(&[
                "merge".as_ref(),
                "--quiet".as_ref(),
                "--no-ff".as_ref(),
                "--allow-unrelated-histories".as_ref(),
                "-m".as_ref(),
                "merge fetched blob".as_ref(),
                "FETCH_HEAD".as_ref(),
            ]);
            assert_eq!(
                std::fs::read(linked.join("upstream-blob"))?,
                b"fetched upstream blob"
            );
            lane_git(&[
                "-c".as_ref(),
                "protocol.file.allow=always".as_ref(),
                "push".as_ref(),
                "--quiet".as_ref(),
                "origin".as_ref(),
                "HEAD:refs/heads/published".as_ref(),
            ]);
            assert_eq!(
                host_git(
                    &other,
                    &["rev-parse".as_ref(), "refs/heads/published".as_ref()]
                ),
                lane_git(&["rev-parse".as_ref(), "HEAD".as_ref()]),
                "{id} pushed merged head",
            );
            assert_eq!(
                host_git(
                    &other,
                    &["show".as_ref(), "refs/heads/published:lane-blob".as_ref()]
                ),
                b"private committed blob",
                "{id} pushed private blob",
            );
            assert!(!repo.join(".git/objects").join(relative).exists(), "{id}");
            assert_eq!(
                host_git(&linked, &["rev-parse".as_ref(), "HEAD".as_ref()]),
                bound_head,
                "{id} shared branch remains at base"
            );
            host_git(
                &repo,
                &[
                    "-c".as_ref(),
                    "protocol.file.allow=always".as_ref(),
                    "fetch".as_ref(),
                    "--quiet".as_ref(),
                    "--refmap=".as_ref(),
                    "origin".as_ref(),
                    "HEAD".as_ref(),
                ],
            );
        }
    }
    Ok(())
}

macro_rules! fixture {
    ($file:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/providers/",
            $file
        ))
    };
}
const EXIT: ExitObservation = ExitObservation {
    code: Some(0),
    timed_out: false,
};
fn replay(
    adapter: &dyn Adapter,
    bytes: &str,
    exit: ExitObservation,
) -> Result<(Vec<ProviderEvent>, ProviderOutcome), ProviderError> {
    let mut state = adapter.stream(None)?;
    let mut events = Vec::new();
    // Split inside UTF-8 and JSON tokens, not just at event boundaries.
    for chunk in bytes.as_bytes().chunks(17) {
        events.extend(adapter.observe(&mut state, chunk)?);
    }
    Ok((events, adapter.finish(state, exit)?))
}
#[test]
fn system_notifications_without_models_do_not_poison_streams() -> Result<(), ProviderError> {
    for adapter in [&claude::Claude as &dyn Adapter, &cursor::Cursor] {
        let stream = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
            "{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"session_id\":\"s\"}\n",
            "{\"type\":\"system\",\"subtype\":\"api_retry\",\"session_id\":\"s\"}\n",
            "{\"type\":\"system\",\"subtype\":\"hook_response\",\"session_id\":\"s\"}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\",\"usage\":{\"inputTokens\":1,\"outputTokens\":2}}\n",
        );
        assert_eq!(
            replay(adapter, stream, EXIT)?.1.status(),
            OutcomeStatus::Success
        );
    }
    Ok(())
}
#[test]
fn error_results_need_neither_answer_nor_usage() -> Result<(), ProviderError> {
    for adapter in [&claude::Claude as &dyn Adapter, &cursor::Cursor] {
        for subtype in ["error_max_turns", "error_during_execution"] {
            let raw = format!(
                "{{\"type\":\"result\",\"subtype\":\"{subtype}\",\"is_error\":true,\"session_id\":\"s\"}}\n"
            );
            let (events, outcome) = replay(adapter, &raw, EXIT)?;
            assert!(events.contains(&ProviderEvent::Error));
            assert_eq!(outcome.status(), OutcomeStatus::Failed);
        }
    }
    Ok(())
}
#[test]
fn synthetic_and_api_error_messages_cannot_change_answering_identity() -> Result<(), ProviderError>
{
    let adapter = claude::Claude;
    let raw = fixture!("claude/direct-model.jsonl");
    let route = route("claude", "claude-haiku-4-5-20251001");
    let continuation = adapter.continuation(&replay(&adapter, raw, EXIT)?.1, &route, "grants")?;
    for (model, error, api_error) in [
        ("<synthetic>", "\"authentication_failed\"", false),
        ("error-model", "\"authentication_failed\"", false),
        ("error-model", "null", true),
        ("<synthetic>", "null", false),
    ] {
        let message = format!(
            "{{\"type\":\"assistant\",\"session_id\":\"FIXTURE_SESSION_IDENTIFIER_004\",\"error\":{error},\"is_api_error_message\":{api_error},\"message\":{{\"role\":\"assistant\",\"model\":\"{model}\",\"content\":[]}}}}\n"
        );
        let mut resumed = adapter.stream(Some(&continuation))?;
        let events = adapter.observe(&mut resumed, message.as_bytes())?;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ProviderEvent::Model(_)))
        );
        if error != "null" {
            assert_eq!(adapter.account(&resumed)?, AccountDetection::AuthRequired);
        }
        assert_ne!(
            adapter.finish(resumed, EXIT)?.status(),
            OutcomeStatus::Malformed
        );
        let (_, fresh) = replay(&adapter, &(message + raw), EXIT)?;
        assert_eq!(fresh.model(), Some(route.model.as_str()));
    }
    Ok(())
}
#[test]
fn recovered_transport_retry_succeeds_but_turn_failure_stays_failed() -> Result<(), ProviderError> {
    // Synthetic R9 replay: records recovery semantics, not CLI activation.
    for (message, failure, expected) in [
        ("Reconnecting… 1/5", "", OutcomeStatus::Success),
        (
            "Reconnecting… 1/5",
            "{\"type\":\"turn.failed\",\"error\":{\"message\":\"permanent\"}}\n",
            OutcomeStatus::Failed,
        ),
        ("invalid request", "", OutcomeStatus::Failed),
    ] {
        let raw = format!(
            "{{\"type\":\"error\",\"message\":\"{message}\"}}\n{failure}{}",
            fixture!("codex/success.jsonl")
        );
        assert_eq!(replay(&codex::Codex, &raw, EXIT)?.1.status(), expected);
    }
    Ok(())
}
#[test]
fn codex_capacity_failure_is_typed_without_misclassifying_other_failures()
-> Result<(), ProviderError> {
    for raw in [
        r#"{"type":"turn.failed","error":{"message":"Selected model is at capacity. Please try a different model.","codex_error_info":"server_overloaded"}}"#,
        r#"{"type":"turn.failed","error":{"message":"Temporarily unavailable","codex_error_info":"server_overloaded"}}"#,
    ] {
        let (events, outcome) = replay(&codex::Codex, &format!("{raw}\n"), EXIT)?;
        assert_eq!(outcome.status(), OutcomeStatus::Refused);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ProviderEvent::ModelOverloaded { .. }))
        );
    }
    for raw in [
        r#"{"type":"turn.failed","error":{"message":"upstream cache is at capacity"}}"#,
        r#"{"type":"turn.failed","error":{"codex_error_info":"server_overloaded"}}"#,
        r#"{"type":"turn.failed","error":{"codex_error_info":{"code":"server_overloaded"}}}"#,
        r#"{"type":"turn.failed","error":"unexpected"}"#,
    ] {
        let (events, outcome) = replay(&codex::Codex, &format!("{raw}\n"), EXIT)?;
        assert_eq!(outcome.status(), OutcomeStatus::Failed);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ProviderEvent::ModelOverloaded { .. }))
        );
    }
    Ok(())
}
#[test]
fn codex_retry_exhausted_429_is_rate_limited() -> Result<(), ProviderError> {
    for message in [
        "exceeded retry limit, last status: 429 Too Many Requests",
        "exceeded retry limit, last status: 429",
        "TOO MANY REQUESTS",
    ] {
        let raw = format!(
            "{{\"type\":\"error\",\"message\":\"{message}\"}}\n\
             {{\"type\":\"turn.failed\",\"error\":{{\"message\":\"{message}\"}}}}\n"
        );
        let (events, outcome) = replay(&codex::Codex, &raw, EXIT)?;
        assert_eq!(outcome.status(), OutcomeStatus::Refused);
        assert!(events.contains(&ProviderEvent::RateLimited {
            message: message.into(),
        }));
    }
    let recovered = format!(
        "{{\"type\":\"error\",\"message\":\"Reconnecting… 1/5 (429 Too Many Requests)\"}}\n{}",
        fixture!("codex/success.jsonl")
    );
    let (events, outcome) = replay(&codex::Codex, &recovered, EXIT)?;
    assert_eq!(outcome.status(), OutcomeStatus::Success);
    assert!(events.contains(&ProviderEvent::TransientError));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderEvent::RateLimited { .. }))
    );
    for message in [
        "invalid request",
        "request id: 429",
        "https://example.test/429",
    ] {
        let raw = format!("{{\"type\":\"error\",\"message\":\"{message}\"}}\n");
        let (events, outcome) = replay(&codex::Codex, &raw, EXIT)?;
        assert_eq!(outcome.status(), OutcomeStatus::Failed);
        assert_eq!(events, vec![ProviderEvent::Error]);
    }
    Ok(())
}
#[test]
fn agy_stream_denial_is_failure() -> Result<(), ProviderError> {
    let (events, outcome) = replay(&agy::Agy, fixture!("agy/denied-result.jsonl"), EXIT)?;
    assert_eq!(outcome.status(), OutcomeStatus::Failed);
    assert!(events.contains(&ProviderEvent::Denied {
        action: "RunCommand".into(),
    }));
    let unsafe_name = fixture!("agy/denied-result.jsonl").replace("RunCommand", "bad\\nname");
    let (events, outcome) = replay(&agy::Agy, &unsafe_name, EXIT)?;
    assert_eq!(outcome.status(), OutcomeStatus::Failed);
    assert!(events.contains(&ProviderEvent::Denied {
        action: "unknown action".into(),
    }));
    Ok(())
}
#[test]
fn agy_recovered_denial_is_non_fatal() -> Result<(), ProviderError> {
    let raw = fixture!("agy/denied-result.jsonl")
        .replace("\"response\":\"\"", "\"response\":\"Recovered answer\"");
    let (events, outcome) = replay(&agy::Agy, &raw, EXIT)?;
    assert_eq!(outcome.status(), OutcomeStatus::Success);
    assert!(events.contains(&ProviderEvent::Text("Recovered answer".into())));
    assert!(events.contains(&ProviderEvent::DeniedNote {
        action: "RunCommand".into()
    }));
    Ok(())
}
#[test]
fn agy_denials_accept_unexpected_shapes() -> Result<(), ProviderError> {
    for (denials, expected) in [
        (serde_json::json!(null), None),
        (serde_json::json!({"display_name": "RunCommand"}), None),
        (serde_json::json!([42]), Some("unknown action")),
    ] {
        let raw = serde_json::json!({"event":"result","result":{
            "conversation_id":"", "status":"SUCCESS", "response":"",
            "denied_actions":denials, "usage":{"input_tokens":0,"output_tokens":0}
        }})
        .to_string();
        let (events, outcome) = replay(&agy::Agy, &format!("{raw}\n"), EXIT)?;
        if let Some(action) = expected {
            assert_eq!(outcome.status(), OutcomeStatus::Failed);
            assert!(events.contains(&ProviderEvent::Denied {
                action: action.into()
            }));
        } else {
            assert_eq!(outcome.status(), OutcomeStatus::Success);
        }
    }
    Ok(())
}
#[test]
fn agy_denied_command_preserves_failed_response() -> Result<(), ProviderError> {
    // Synthetic stdout paired with the issue's recorded stderr diagnostic.
    let raw = serde_json::json!({"event":"result","result":{
        "conversation_id":"", "status":"ERROR", "response":"I could only verify part of the claim.",
        "error":"command permission denied", "usage":{"input_tokens":0,"output_tokens":0}
    }})
    .to_string();
    let (mut events, outcome) = replay(
        &agy::Agy,
        &raw,
        ExitObservation {
            code: Some(1),
            ..EXIT
        },
    )?;
    events.extend(outcome.final_events().iter().cloned());
    assert_eq!(outcome.status(), OutcomeStatus::Failed);
    assert!(events.contains(&ProviderEvent::Text(
        "I could only verify part of the claim.".into()
    )));
    assert_eq!(
        agy::Agy.failure_diagnostic(include_bytes!(
            "../../../tests/fixtures/providers/agy/command-denied.stderr"
        )),
        Some(r#"agy denied the "command" permission in headless mode"#.into())
    );
    for unrelated in [
        "a tool required the command permission",
        "jetski: unrelated failure",
        "jetski: no output produced \u{2014} a tool required the \"command\nforged\" permission that headless mode cannot prompt for, so it was auto-denied.",
        "jetski: no output produced \u{2014} a tool required the \"\" permission that headless mode cannot prompt for, so it was auto-denied.",
    ] {
        assert_eq!(agy::Agy.failure_diagnostic(unrelated.as_bytes()), None);
    }
    assert_eq!(
        codex::Codex.failure_diagnostic(include_bytes!(
            "../../../tests/fixtures/providers/agy/command-denied.stderr"
        )),
        None
    );
    Ok(())
}
