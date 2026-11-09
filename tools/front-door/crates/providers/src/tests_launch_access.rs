
/// Exercises the actual Seatbelt file-open decision without a provider login
/// or an external HTTPS service. An inherited lane cannot install a nested
/// sandbox; CI requires this probe instead of accepting that limitation.
#[cfg(target_os = "macos")]
#[test]
#[allow(clippy::disallowed_methods)]
#[allow(clippy::print_stderr)] // Test-only sandbox skip diagnostic.
fn claude_network_on_native_tls_files_are_readable() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::{NetworkAccess, snapshot::render};
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    for network in [
        NetworkAccess::Open,
        NetworkAccess::Provider,
        NetworkAccess::Local,
    ] {
        let plan = fixture_network_plan(Access::ReadOnly, None, network);
        let launch = claude::Claude.prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            Some(&token),
        ))?;
        let profile = render(
            plan.permissions(),
            &launch.profile,
            Path::new("/fixture/home"),
        )?;
        for file in ["/private/etc/ssl/openssl.cnf", "/private/etc/ssl/cert.pem"] {
            let output = std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &profile, "/bin/cat", file])
                .env_clear()
                .output()?;
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            if diagnostic.contains("sandbox_apply: Operation not permitted") {
                provenant_test_support::skip_sandbox_dependent_test(format!(
                    "native TLS file probe unavailable under inherited sandbox: {diagnostic}"
                ))?;
                return Ok(());
            }
            assert!(output.status.success(), "{network:?} {file}: {diagnostic}");
            assert!(
                !output.stdout.is_empty(),
                "{file} must have been opened and read"
            );
        }
    }
    Ok(())
}

pub(super) fn fixture_token() -> crate::claude::OauthToken {
    let item = format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{FIXTURE_TOKEN}","refreshToken":"fixture-refresh","expiresAt":4000000000000}}}}"#
    );
    crate::claude::OauthToken::parse(item.as_bytes(), 1_000).expect("fixture token")
}

pub(super) fn fixture_input<'a>(
    route: &'a ResolvedRoute,
    confined: &'a provenant_confinement::ConfinedPlan,
    prompt: &'a Prompt,
    cwd: &'a std::path::Path,
    token: Option<&'a crate::claude::OauthToken>,
) -> PrepareInput<'a> {
    let executable = std::path::Path::new("/opt/homebrew/Cellar/provider/1.0/bin/provider");
    PrepareInput {
        images: &[],
        command: None,
        route,
        confined,
        prompt,
        continuation: None,
        grant_digest: "fixture",
        executable,
        home: std::path::Path::new("/fixture/home"),
        user: std::ffi::OsStr::new("fixture"),
        host_var: &|_| None,
        skill_stage_max_bytes: 16 * 1024 * 1024,
        account: None,
        token,
        cwd,
        git_context: false,
        nonce: [9; 16],
        provenant: crate::LaneRuntime {
            executable: std::path::Path::new("/fixture/provenant/bin/provenant"),
            git: std::path::Path::new("/fixture/git/bin/git"),
            state: std::path::Path::new("/fixture/state"),
            home: std::path::Path::new("/fixture/home"),
            depth: 1,
            attempt: provenant_core::AttemptId::from_bytes([7; 16]),
            assets: std::path::Path::new("/fixture/provenant"),
        },
    }
}

#[test]
fn spilled_prompts_reach_argv_adapters_with_file_read_permissions() -> Result<(), ProviderError> {
    let file = Path::new("/fixture/attempt/prompt.md");
    let prompt = Prompt::from_file(file.to_owned());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for entry in REGISTRY
            .iter()
            .filter(|entry| matches!(entry.adapter.id(), "cursor" | "agy"))
        {
            let id = entry.adapter.id();
            let authentication = match id {
                "cursor" => Some("login"),
                "agy" => Some("keychain"),
                _ => None,
            };
            let plan = fixture_access_plan(access, authentication, false);
            let mut route = route(id, "fixture-model");
            route.account_key = None;
            let spec = entry.adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                None,
            ))?;
            assert_eq!(spec.argv.last(), Some(&OsString::from(prompt.text())));
            if id == "agy" {
                assert!(
                    spec.argv
                        .windows(2)
                        .any(|pair| { pair[0] == "--add-dir" && pair[1] == "/fixture/attempt" })
                );
            }
            if id == "agy" && access == Access::ReadOnly {
                assert!(
                    spec.argv
                        .iter()
                        .any(|arg| arg == "--dangerously-skip-permissions")
                );
            }
        }
    }
    Ok(())
}

#[test]
fn image_attachments_reach_codex_and_claude() -> Result<(), ProviderError> {
    let images = vec![
        Path::new("/outside/first image.png").to_owned(),
        Path::new("/outside/second.png").to_owned(),
    ];
    let prompt = Prompt::new("Describe the images".into());
    let token = fixture_token();
    for id in ["codex", "claude"] {
        let plan = fixture_access_plan(Access::ReadOnly, (id == "codex").then_some("login"), false);
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        let mut input = fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            (id == "claude").then_some(&token),
        );
        input.images = &images;
        let adapter = REGISTRY
            .iter()
            .find(|entry| entry.adapter.id() == id)
            .unwrap()
            .adapter;
        let spec = adapter.prepare(input)?;
        if id == "codex" {
            for image in &images {
                assert!(
                    spec.argv
                        .windows(2)
                        .any(|pair| pair[0] == "--image" && pair[1] == image.as_os_str())
                );
            }
        } else {
            let Stdin::Bytes(bytes) = spec.stdin else {
                panic!("expected prompt on stdin")
            };
            let text = String::from_utf8(bytes).unwrap();
            for image in &images {
                assert!(text.contains(&image.display().to_string()));
            }
            assert!(!text.contains("/outside/sibling.png"));
            assert!(
                !spec
                    .argv
                    .windows(2)
                    .any(|pair| pair[0] == "--add-dir" && pair[1] == "/outside")
            );
            let settings_path = &spec.argv[spec
                .argv
                .iter()
                .position(|arg| arg == "--settings")
                .unwrap()
                + 1];
            let settings = spec
                .setup
                .iter()
                .find_map(|step| match step {
                    SetupStep::WriteFile { path, bytes } if path.as_os_str() == settings_path => {
                        Some(bytes)
                    }
                    _ => None,
                })
                .unwrap();
            let settings: serde_json::Value = serde_json::from_slice(settings).unwrap();
            let rules = settings["permissions"]["allow"].as_array().unwrap();
            assert!(rules.contains(&serde_json::json!(r"Read(//outside/first\ image.png)")));
            assert!(rules.contains(&serde_json::json!("Read(//outside/second.png)")));
            assert!(!rules.contains(&serde_json::json!("Read(//outside/sibling.png)")));
        }
    }
    Ok(())
}

#[test]
fn claude_writer_uses_confinement_instead_of_native_tool_permissions() -> Result<(), ProviderError>
{
    let images = vec![Path::new("/outside/image.png").to_owned()];
    let prompt = Prompt::new("Describe the image".into());
    let token = fixture_token();
    let plan = fixture_access_plan(Access::WorktreeWrite, None, false);
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    let mut input = fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo"),
        Some(&token),
    );
    input.images = &images;
    let spec = REGISTRY
        .iter()
        .find(|entry| entry.adapter.id() == "claude")
        .unwrap()
        .adapter
        .prepare(input)?;
    assert!(!spec.argv.iter().any(|arg| arg == "--allowedTools"));
    assert!(
        spec.argv
            .windows(2)
            .any(|pair| pair[0] == "--permission-mode" && pair[1] == "bypassPermissions")
    );
    assert!(spec.profile.spec.deny_keychain_access);
    assert!(spec.profile.spec.authentication.is_empty());
    Ok(())
}

#[test]
fn stdin_adapters_receive_oversized_prompts_in_full() -> Result<(), ProviderError> {
    let prompt = Prompt::new(format!(
        "Durable output instructions\n\n{}",
        "é".repeat(75_000)
    ));
    let token = fixture_token();
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for entry in REGISTRY
            .iter()
            .filter(|entry| matches!(entry.adapter.id(), "claude" | "codex" | "opencode"))
        {
            let id = entry.adapter.id();
            let plan = fixture_access_plan(
                access,
                matches!(id, "codex" | "opencode").then_some("login"),
                false,
            );
            let mut route = route(id, "fixture-model");
            route.account_key = None;
            let spec = entry.adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                (id == "claude").then_some(&token),
            ))?;
            assert_eq!(spec.stdin, Stdin::Bytes(prompt.text().as_bytes().to_vec()));
            assert!(!spec.argv.iter().any(|arg| arg == "--add-dir"));
        }
    }
    Ok(())
}

#[test]
fn temporary_environment_follows_the_plan_for_readers_and_writers() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let token = fixture_token();
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for entry in REGISTRY.iter().filter(|entry| {
            provenant_records::AdapterSpec::get(entry.adapter.id())
                .is_some_and(|spec| spec.launch.model)
        }) {
            let id = entry.adapter.id();
            let authentication = match id {
                "codex" | "cursor" | "opencode" => Some("login"),
                "agy" => Some("keychain"),
                _ => None,
            };
            let plan = fixture_access_plan(access, authentication, false);
            let mut route = route(id, "fixture-model");
            route.account_key = None;
            let spec = entry.adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                (id == "claude").then_some(&token),
            ))?;
            for key in
                std::iter::once("TMPDIR").chain((id == "claude").then_some("CLAUDE_CODE_TMPDIR"))
            {
                assert_eq!(
                    spec.env
                        .iter()
                        .find(|(name, _)| name == key)
                        .map(|(_, value)| value),
                    Some(&plan.permissions().scratch.path.as_os_str().to_owned()),
                    "{id} {access:?} {key}"
                );
            }
            // zsh creates here-document files under TMPPREFIX, not TMPDIR.
            assert_eq!(
                spec.env
                    .iter()
                    .find(|(name, _)| name == "TMPPREFIX")
                    .map(|(_, value)| value),
                Some(&plan.permissions().scratch.path.join("zsh").into_os_string()),
                "{id} {access:?} TMPPREFIX"
            );
        }
    }
    Ok(())
}

#[test]
fn claude_read_only_adds_external_git_dir_only() {
    let common_dir = Path::new("/fixture/repo/.git");
    let mut argv = Vec::new();
    claude::add_git_dirs(
        &mut argv,
        [common_dir],
        Path::new("/fixture/repo/.worktrees/x"),
    );
    assert_eq!(
        argv,
        [
            OsString::from("--add-dir"),
            common_dir.as_os_str().to_owned()
        ]
    );

    argv.clear();
    claude::add_git_dirs(&mut argv, [common_dir], Path::new("/fixture/repo"));
    assert!(argv.is_empty());
}

#[test]
fn claude_lanes_disable_commit_and_pr_attribution() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("claude", "fixture-model");
    let account = crate::identity::RegisteredAccount::new(
        "claude",
        "opaque-account",
        "/fixture/home/.claude".into(),
    )?;
    let token = fixture_token().for_account(account.key(), account.directory());
    let continuation = Continuation::retained("claude", "fixture-session", &route, "fixture")?;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for resume in [false, true] {
            let plan = fixture_access_plan(access, None, resume);
            let mut input = fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                Some(&token),
            );
            input.account = Some(&account);
            input.continuation = resume.then_some(&continuation);
            let spec = claude::Claude.prepare(input)?;
            let settings_path = spec
                .argv
                .windows(2)
                .find(|args| args[0] == "--settings")
                .map(|args| Path::new(&args[1]))
                .expect("lane settings argument");
            let bytes = spec
                .setup
                .iter()
                .find_map(|step| match step {
                    SetupStep::WriteFile { path, bytes } if path == settings_path => Some(bytes),
                    _ => None,
                })
                .expect("lane settings file");
            let settings: serde_json::Value =
                serde_json::from_slice(bytes).expect("lane settings JSON");
            assert_eq!(settings["attribution"]["commit"], "");
            assert_eq!(settings["attribution"]["pr"], "");
        }
    }
    Ok(())
}

#[test]
fn claude_requested_permission_modes_are_not_overridden() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let token = fixture_token();
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    for (access, mode) in [
        (Access::ReadOnly, "dontAsk"),
        (Access::WorktreeWrite, "bypassPermissions"),
    ] {
        let plan = fixture_access_plan(access, None, false);
        let mut input = fixture_input(
            &route,
            &plan,
            &prompt,
            std::path::Path::new("/fixture/repo"),
            Some(&token),
        );
        input.host_var = &|key| (key == "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB").then(|| "1".into());
        let launch = claude::Claude.prepare(input)?;
        assert!(
            launch
                .env
                .iter()
                .all(|(key, _)| key != "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB")
        );
        assert!(
            launch
                .argv
                .windows(2)
                .any(|args| args[0] == "--permission-mode" && args[1] == mode)
        );
    }
    Ok(())
}

#[test]
fn prepare_all_adapters_and_pin_launch_controls() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let repo = std::path::Path::new("/fixture/repo");
    let token = fixture_token();
    for entry in REGISTRY.iter().filter(|entry| {
        provenant_records::AdapterSpec::get(entry.adapter.id())
            .is_some_and(|spec| spec.launch.model)
    }) {
        let id = entry.adapter.id();
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        // Codex and Agy read a credential inside the sandbox.
        let authentication = match id {
            "codex" | "cursor" | "opencode" => Some("login"),
            "agy" => Some("keychain"),
            _ => None,
        };
        let confined = read_only_plan("/fixture", authentication);
        let token = (id == "claude").then_some(&token);
        let result = entry
            .adapter
            .prepare(fixture_input(&route, &confined, &prompt, repo, token));
        let spec = result?;
        // Only Agy runs its commands on a pseudo-terminal.
        assert_eq!(spec.profile.spec.allow_pseudo_tty, id == "agy");
        // Agy keeps its prompt on argv and has no lane runtime; its own test pins its launch.
        if matches!(id, "agy" | "cursor" | "opencode") {
            continue;
        }
        let argv: Vec<_> = spec.argv.iter().map(|arg| arg.to_string_lossy()).collect();
        // Nothing is inherited: the environment is exactly the allow-list.
        let keys: Vec<_> = spec
            .env
            .iter()
            .map(|(key, _)| key.to_string_lossy())
            .collect();
        let extra: &[&str] = match id {
            "codex" => &[
                "CARGO_HOME",
                "RUSTUP_HOME",
                "RUSTUP_AUTO_INSTALL",
                "CARGO_TARGET_DIR",
                "CARGO_BUILD_BUILD_DIR",
                "CARGO_CACHE_AUTO_CLEAN_FREQUENCY",
                "CARGO_NET_OFFLINE",
                "GIT_CONFIG_GLOBAL",
                "GIT_CONFIG_NOSYSTEM",
                "CODEX_HOME",
                "SHELL",
            ],
            "claude" => &[
                "CLAUDE_CODE_TMPDIR",
                "DISABLE_AUTOUPDATER",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
                "CLAUDE_CONFIG_DIR",
                "GIT_CONFIG_GLOBAL",
                "GIT_CONFIG_NOSYSTEM",
            ],
            _ => &[],
        };
        let mut expected: Vec<_> = [
            "HOME",
            "USER",
            "PATH",
            "TMPDIR",
            "TMPPREFIX",
            "LANG",
            "PROVENANT_AGENT_LABEL",
            "PROVENANT_ROOT",
            "PROVENANT_STATE_DIR",
            "PROVENANT_LANE_DEPTH",
            "PROVENANT_PARENT_ATTEMPT_ID",
            "PROVENANT_RUN_STATE_DIR",
        ]
        .iter()
        .chain(extra)
        .copied()
        .collect();
        if cfg!(target_os = "macos")
            && id == "codex"
            && Path::new("/Library/Developer/CommandLineTools").is_dir()
        {
            let index = expected
                .iter()
                .position(|key| *key == "CARGO_NET_OFFLINE")
                .expect("cargo env")
                + 1;
            expected.splice(index..index, ["DEVELOPER_DIR", "SDKROOT"]);
        }
        if id == "codex"
            && ["/opt/homebrew", "/usr/local"]
                .iter()
                .any(|prefix| Path::new(prefix).join("bin/node").exists())
        {
            let index = expected
                .iter()
                .position(|key| *key == "GIT_CONFIG_GLOBAL")
                .expect("git env");
            expected.splice(
                index..index,
                [
                    "npm_config_cache",
                    "npm_config_cache_dir",
                    "pnpm_config_cache_dir",
                    "COREPACK_HOME",
                    "pnpm_config_verify_deps_before_run",
                    "npm_config_manage_package_manager_versions",
                    "pnpm_config_manage_package_manager_versions",
                    "npm_config_package_manager_strict",
                    "pnpm_config_package_manager_strict",
                    "COREPACK_ENABLE_PROJECT_SPEC",
                    "COREPACK_ENABLE_DOWNLOAD",
                ],
            );
        }
        // Git selection is replaced after the adapter's environment is built.
        expected.push("PROVENANT_GIT");
        assert_eq!(keys, expected, "{id}");
        if id == "codex" {
            // This fixture grants provider-only networking. Cargo must use
            // cached sources rather than attempting dependency downloads.
            assert!(spec.env.iter().any(|(key, value)| {
                key == "CARGO_NET_OFFLINE" && value == "true"
            }));
        }
        assert!(
            spec.env.iter().any(|(key, value)| {
                key == "PROVENANT_GIT" && value == "/fixture/scratch/read-only-git/git"
            }),
            "{id} must select its private Git wrapper"
        );
        assert_eq!(spec.stdin, Stdin::Bytes(b"fixture prompt".to_vec()), "{id}");
        assert!(
            !argv.iter().any(|arg| arg.contains("fixture prompt")),
            "{id}"
        );
        match id {
            "codex" => {
                assert!(argv.iter().any(|arg| arg == "service_tier=\"default\""));
                assert!(
                    !argv
                        .iter()
                        .any(|arg| arg.contains("fast") || arg.contains("priority"))
                );
            }
            "claude" => {
                assert_eq!(
                    spec.env.iter().find(|(key, _)| key == "CLAUDE_CONFIG_DIR").map(|(_, value)| Path::new(value)),
                    Some(confined.permissions().private_state.as_ref().unwrap().path.join("claude-config").as_path())
                );
                assert!(argv.iter().any(|arg| arg == "--mcp-config"));
                assert!(argv.iter().any(|arg| arg == "--settings"));
                assert!(!argv.iter().any(|arg| arg == "--safe-mode"));
                assert!(!argv.iter().any(|arg| arg == "--add-dir"));
                assert!(
                    spec.env
                        .iter()
                        .all(|(key, _)| { key != "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD" })
                );
                let sources = argv
                    .iter()
                    .position(|arg| arg == "--setting-sources")
                    .expect("isolated settings");
                assert_eq!(argv[sources + 1], "");
                let tools = argv.iter().position(|arg| arg == "--tools").expect("tools");
                assert_eq!(argv[tools + 1], "Read,Grep,Glob,Bash,Skill");
                // The host's token is the only credential: no keychain or
                // other credential path is readable. Only Provenant and its hook helpers are added.
                assert!(
                    spec.env
                        .iter()
                        .all(|(key, _)| key != "CLAUDE_CODE_OAUTH_TOKEN")
                );
                let secret = spec.secret_input.as_ref().expect("token pipe");
                assert_eq!(
                    secret.descriptor_env,
                    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR"
                );
                assert_eq!(secret.bytes, FIXTURE_TOKEN.as_bytes());
                let allowed = argv
                    .iter()
                    .position(|arg| arg == "--allowedTools")
                    .expect("allow-list");
                assert_eq!(argv[allowed + 1], "Bash,Skill,mcp__fabric__*");
                assert!(spec.profile.spec.authentication.is_empty());
                assert!(spec.profile.spec.deny_keychain_access);
                for (key, value) in [
                    ("GIT_CONFIG_GLOBAL", "/dev/null"),
                    ("GIT_CONFIG_NOSYSTEM", "1"),
                ] {
                    assert!(spec.env.iter().any(|(k, v)| k == key && v == value));
                }
                let roots: Vec<_> = spec
                    .profile
                    .spec
                    .tool_exec_roots
                    .iter()
                    .map(|grant| grant.path.as_path())
                    .collect();
                assert_eq!(
                    roots,
                    [
                        "/bin",
                        "/usr/bin",
                        "/usr/libexec",
                        "/fixture/scratch/bin",
                        "/fixture/scratch/read-only-git"
                    ]
                    .map(Path::new)
                );
                assert!(spec.setup.iter().any(|step| matches!(
                    step, crate::launch::SetupStep::WriteExecutable { path, bytes }
                        if path == Path::new("/fixture/scratch/bin/git")
                            && String::from_utf8_lossy(bytes).contains("GIT_CONFIG_NOSYSTEM=1")
                )));
                assert!(
                    spec.profile
                        .spec
                        .process_executables
                        .iter()
                        .any(|grant| grant.path
                            == std::path::Path::new("/fixture/provenant/bin/provenant"))
                );
                assert!(matches!(
                    entry
                        .adapter
                        .prepare(fixture_input(&route, &confined, &prompt, repo, None)),
                    Err(ProviderError::UnsupportedPolicy)
                ));
                let with_login = read_only_plan("/fixture", Some("login"));
                assert!(matches!(
                    entry.adapter.prepare(fixture_input(
                        &route,
                        &with_login,
                        &prompt,
                        repo,
                        Some(&fixture_token())
                    )),
                    Err(ProviderError::UnsupportedPolicy)
                ));
            }
            _ => {}
        }
        // Shared install prefixes fall back to the exact executable. Their
        // `var/` trees stay outside the lane profile.
        let shared = PrepareInput {
            executable: std::path::Path::new("/opt/homebrew/bin/provider"),
            ..fixture_input(&route, &confined, &prompt, repo, token)
        };
        let shared_spec = entry.adapter.prepare(shared)?;
        let shared_var = Path::new("/opt/homebrew/var");
        assert!(shared_spec.profile.spec.runtime_reads.iter().any(|grant| {
            grant.path == Path::new("/opt/homebrew/bin/provider")
                && grant.scope == provenant_confinement::permissions::PathScope::Exact
        }));
        assert!(shared_spec.profile.spec.runtime_reads.iter().all(|grant| {
            !grant.path.starts_with(shared_var)
                && !shared_var.starts_with(&grant.path)
                && grant.path != Path::new("/opt/homebrew")
        }));
        assert!(
            shared_spec
                .profile
                .spec
                .tool_exec_roots
                .iter()
                .all(|grant| {
                    !grant.path.starts_with(shared_var)
                        && !shared_var.starts_with(&grant.path)
                        && grant.path != Path::new("/opt/homebrew")
                })
        );
        assert!(
            provenant_confinement::write_grants(confined.permissions(), &shared_spec.profile)
                .iter()
                .all(|grant| {
                    !grant.path.starts_with(shared_var)
                        && !shared_var.starts_with(&grant.path)
                        && !Path::new("/opt/homebrew/bin/provider").starts_with(&grant.path)
                })
        );
        assert!(shared_spec.omitted_discovery.iter().any(|omitted| {
            omitted.path == Path::new("/opt/homebrew")
                && omitted.source == crate::OmissionSource::ProviderPackage
        }));
        let package = std::path::Path::new("/opt/homebrew/Cellar/provider/1.0");
        // Host-detected Homebrew Node toolchain directories (launch::node_tools).
        let homebrew_node = |path: &Path| {
            ["/opt/homebrew", "/usr/local"].iter().any(|prefix| {
                [
                    "bin",
                    "sbin",
                    "Cellar",
                    "opt",
                    "lib",
                    "libexec",
                    "etc",
                    "share",
                    "include",
                    "Frameworks",
                ]
                .iter()
                .any(|name| path == Path::new(prefix).join(name))
            })
        };
        assert!(spec.profile.spec.runtime_reads.iter().all(|grant| {
            grant.path == package
                || homebrew_node(&grant.path)
                || (id == "codex"
                    && (grant.path.starts_with("/fixture/home")
                        || grant.path == Path::new("/Library/Developer/CommandLineTools")
                        || grant.path == Path::new("/private/etc/ssl")))
                || [
                    "/fixture/provenant/bin/provenant",
                    "/fixture/git/bin/git",
                    "/fixture/provenant/release.json",
                    "/fixture/state/config.toml",
                    "/fixture/state/host-id",
                    "/fixture/state/projects",
                ]
                .iter()
                .any(|path| grant.path == std::path::Path::new(path))
                || ([
                    "/fixture/state/fabric/fabric.sqlite3",
                    "/fixture/state/fabric/fabric.sqlite3-wal",
                    "/fixture/state/fabric/fabric.sqlite3-shm",
                    "/fixture/state/fabric/fabric.sqlite3-journal",
                ]
                .iter()
                .any(|path| grant.path == Path::new(path))
                    && grant.scope == provenant_confinement::permissions::PathScope::Exact)
        }));
        assert!(
            spec.profile
                .spec
                .tool_exec_roots
                .iter()
                .all(|grant| grant.path == package
                    || homebrew_node(&grant.path)
                    || (grant.scope == provenant_confinement::permissions::PathScope::Exact
                        && grant.path.starts_with("/opt/homebrew"))
                    || !grant.path.starts_with("/opt")),
            "{id}: {:?}",
            spec.profile.spec.tool_exec_roots
        );
        // A read-only launch may only start inside a declared read root.
        assert!(matches!(
            entry.adapter.prepare(fixture_input(
                &route,
                &confined,
                &prompt,
                std::path::Path::new("/fixture/elsewhere"),
                token,
            )),
            Err(ProviderError::InvalidPath)
        ));
    }
    Ok(())
}

#[test]
fn cursor_launches_fresh_and_resume_with_edits_only_for_writers() -> Result<(), ProviderError> {
    let prompt = Prompt::new("--literal prompt".into());
    let route = route("cursor", "fixture-model");
    let continuation = Continuation::retained("cursor", "fixture-session", &route, "fixture")?;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for resume in [false, true] {
            let plan = fixture_access_plan(access, Some("login"), resume);
            let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
            input.continuation = resume.then_some(&continuation);
            let spec = cursor::Cursor.prepare(input)?;
            let mut expected = vec![
                "/opt/homebrew/Cellar/provider/1.0/bin/provider",
                "--print",
                "--output-format",
                "stream-json",
                "--workspace",
                "/fixture/repo",
                "--trust",
                "--sandbox",
                "disabled",
            ];
            match access {
                Access::ReadOnly => expected.extend(["--mode", "ask"]),
                Access::WorktreeWrite => expected.push("--force"),
            }
            if resume {
                expected.extend(["--resume", "fixture-session"]);
            }
            expected.extend(["--model", "fixture-model", "--", "--literal prompt"]);
            assert_eq!(
                spec.argv,
                expected
                    .iter()
                    .map(std::ffi::OsString::from)
                    .collect::<Vec<_>>()
            );
            assert_eq!(spec.stdin, Stdin::Null);
            assert_eq!(spec.cwd, Path::new("/fixture/repo"));
        }
    }
    let plan = read_only_plan("/fixture", Some("login"));
    let prompt = Prompt::new("x".repeat(65_537));
    assert!(matches!(
        cursor::Cursor.prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            None
        )),
        Err(ProviderError::PromptTooLarge { .. })
    ));
    Ok(())
}

/// An agy registration of the user's own login: its directory is the fixture
/// home, so lanes share the login keychain.
pub(super) fn legacy_agy_account() -> identity::RegisteredAccount {
    agy_account_at(Path::new("/fixture/home"))
}

pub(super) fn agy_account_at(directory: &Path) -> identity::RegisteredAccount {
    identity::RegisteredAccount::new("agy", "opaque-account", directory.into())
        .expect("fixture account")
}

#[test]
fn agy_registers_at_a_shallow_home_but_other_adapters_keep_the_depth_floor() {
    use identity::RegisteredAccount;
    for directory in ["/Users/me", "/home/me"] {
        assert!(RegisteredAccount::new("agy", "opaque-account", directory.into()).is_ok());
        assert!(matches!(
            RegisteredAccount::new("claude", "opaque-account", directory.into()),
            Err(ProviderError::InvalidPath)
        ));
    }
    for directory in ["/", "relative/dir", "/Users/../me"] {
        assert!(matches!(
            RegisteredAccount::new("agy", "opaque-account", directory.into()),
            Err(ProviderError::InvalidPath)
        ));
    }
}

#[test]
fn agy_includes_attempt_scratch_in_the_workspace() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("agy", "fixture-model");
    let account = legacy_agy_account();
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("keychain"), false);
        for git_context in [false, true] {
            let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
            input.git_context = git_context;
            input.account = Some(&account);
            let spec = agy::Agy.prepare(input)?;
            assert_eq!(
                spec.argv.windows(2).any(|pair| {
                    pair[0] == "--add-dir" && pair[1] == plan.permissions().scratch.path.as_os_str()
                }),
                git_context
            );
        }
    }
    Ok(())
}

#[test]
fn agy_turns_are_bounded_only_by_the_supervisor_deadline() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("agy", "fixture-model");
    let account = legacy_agy_account();
    let plan = fixture_access_plan(Access::ReadOnly, Some("keychain"), false);
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.account = Some(&account);
    let spec = agy::Agy.prepare(input)?;
    // Zero switches off agy's own print timeout, which would end a turn early.
    assert!(
        spec.argv
            .windows(2)
            .any(|pair| { pair[0] == "--print-timeout" && pair[1] == "0s" })
    );
    Ok(())
}

#[test]
fn agy_writers_exec_what_codex_writers_exec() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let repo = Path::new("/fixture/repo");
    let agy_route = route("agy", "fixture-model");
    let account = legacy_agy_account();
    let mut codex_route = route("codex", "fixture-model");
    codex_route.account_key = None;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("keychain"), false);
        let mut input = fixture_input(&agy_route, &plan, &prompt, repo, None);
        input.account = Some(&account);
        let spec = agy::Agy.prepare(input)?;
        assert!(
            spec.env
                .iter()
                .any(|(key, value)| key == "SHELL" && value == "/bin/zsh")
        );
        assert_eq!(spec.profile.spec.process_executables.len(), 4);
        let exec = &spec.profile.spec.tool_exec_roots;
        if access == Access::ReadOnly {
            assert_eq!(
                exec,
                &vec![
                    crate::launch::subtree("/bin"),
                    crate::launch::subtree("/usr/bin"),
                    crate::launch::subtree("/usr/libexec"),
                    crate::launch::subtree("/fixture/scratch/read-only-git")
                ]
            );
            assert!(spec.profile.spec.tool_writes.is_empty());
            assert!(plan.permissions().write_roots.is_empty());
            for (key, value) in [
                ("PATH", "/fixture/scratch/read-only-git:/usr/bin:/bin"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
            ] {
                assert!(spec.env.iter().any(|(k, v)| k == key && v == value));
            }
            continue;
        }
        let plan = fixture_access_plan(access, Some("login"), false);
        let codex =
            codex::Codex.prepare(fixture_input(&codex_route, &plan, &prompt, repo, None))?;
        // Everything but Codex's own package directory.
        let package = Path::new("/opt/homebrew/Cellar/provider/1.0");
        let expected: Vec<_> = codex
            .profile
            .spec
            .tool_exec_roots
            .into_iter()
            .filter(|grant| grant.path != package)
            .collect();
        assert_eq!(exec, &expected);
        assert!(exec.iter().any(|grant| grant.path == Path::new("/bin")));
    }
    Ok(())
}

#[test]
fn agy_read_only_adds_read_roots_outside_the_working_directory() -> Result<(), ProviderError> {
    use provenant_confinement::permissions::{PathGrant, PathScope};
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("agy", "fixture-model");
    let account = legacy_agy_account();
    let reader = fixture_access_plan(Access::ReadOnly, Some("keychain"), false);
    let mut permissions = reader.permissions().clone();
    permissions.read_roots.push(PathGrant {
        path: "/fixture/build".into(),
        scope: PathScope::Subtree,
    });
    let plan = ConfinedPlan::new(PlanRequest {
        scope: reader.scope(),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .expect("fixture read-only plan");
    let mut input = fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo/crates"),
        None,
    );
    input.account = Some(&account);
    let spec = agy::Agy.prepare(input)?;
    let added: Vec<_> = spec
        .argv
        .windows(2)
        .filter(|pair| pair[0] == "--add-dir")
        .map(|pair| pair[1].clone())
        .collect();
    assert_eq!(added, [OsString::from("/fixture/build")]);
    Ok(())
}

#[test]
fn claude_read_only_adds_read_roots_outside_the_working_directory() -> Result<(), ProviderError> {
    use provenant_confinement::permissions::{PathGrant, PathScope};
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let prompt = Prompt::new("fixture prompt".into());
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    let token = fixture_token();
    let reader = fixture_access_plan(Access::ReadOnly, None, false);
    let mut permissions = reader.permissions().clone();
    for path in ["/fixture/diffs", "/fixture/repo/crates/sub"] {
        permissions.read_roots.push(PathGrant {
            path: path.into(),
            scope: PathScope::Subtree,
        });
    }
    let plan = ConfinedPlan::new(PlanRequest {
        scope: reader.scope(),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .expect("fixture read-only plan");
    let spec = claude::Claude.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo/crates"),
        Some(&token),
    ))?;
    let added: Vec<_> = spec
        .argv
        .windows(2)
        .filter(|pair| pair[0] == "--add-dir")
        .map(|pair| pair[1].clone())
        .collect();
    // The fixture's own /fixture/repo root holds the working directory but
    // reaches beyond it, so it needs a flag; /fixture/repo/crates/sub sits
    // inside the working directory, so it does not.
    assert_eq!(
        added,
        [
            OsString::from("/fixture/repo"),
            OsString::from("/fixture/diffs")
        ]
    );
    Ok(())
}

#[test]
fn cursor_reads_and_execs_only_its_resolved_version_payload() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("cursor", "fixture-model");
    let payload = Path::new("/fixture/home/.local/share/cursor-agent/versions/2026.10.19-6bbf2c4");
    let launcher = payload.join("cursor-agent");
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("login"), false);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.executable = &launcher;
        let spec = cursor::Cursor.prepare(input)?.profile.spec;
        let mut executables = vec![crate::launch::exact(&launcher)];
        executables.push(crate::launch::exact("/bin/sh"));
        executables.push(crate::launch::exact("/fixture/git/bin/git"));
        assert_eq!(spec.process_executables, executables);
        assert!(
            spec.runtime_reads
                .contains(&crate::launch::subtree(payload))
        );
        assert_eq!(
            spec.tool_exec_roots,
            ["/bin", "/usr/bin"]
                .map(crate::launch::subtree)
                .into_iter()
                .chain([crate::launch::subtree(payload)])
                .chain([crate::launch::subtree(if access == Access::WorktreeWrite {
                    "/fixture/scratch/bin"
                } else {
                    "/fixture/scratch/read-only-git"
                })])
                .collect::<Vec<_>>()
        );
    }
    Ok(())
}

#[test]
fn opencode_uses_user_config_and_accounts_with_read_only_permissions() -> Result<(), ProviderError>
{
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("opencode", "configured/model");
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for git_context in [false, true] {
            let plan = fixture_access_plan(access, Some("login"), false);
            let input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
            let spec = opencode::OpenCode.prepare(PrepareInput {
                git_context,
                ..input
            })?;
            let env = |key: &str| {
                spec.env
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value)
            };
            assert_eq!(
                env("HOME"),
                Some(&std::ffi::OsString::from("/fixture/home"))
            );
            let config_home = Path::new(env("XDG_CONFIG_HOME").expect("config home"));
            let data_home = Path::new(env("XDG_DATA_HOME").expect("data home"));
            assert_eq!(config_home, Path::new("/fixture/state/config"));
            assert_eq!(data_home, Path::new("/fixture/state/data"));
            if config_home.join("opencode/skills").is_dir() {
                assert!(
                    spec.profile
                        .spec
                        .runtime_reads
                        .contains(&crate::launch::subtree(config_home.join("opencode/skills")))
                );
            }
            assert!(spec.profile.spec.authentication[0].roots.contains(&crate::launch::exact("/fixture/home/.local/share/opencode/auth.json")));
            let auth = data_home.join("opencode/auth.json");
            assert!(spec.profile.spec.state_writes.iter().all(|grant| {
                match grant.scope {
                    provenant_confinement::permissions::PathScope::Exact => grant.path != auth,
                    provenant_confinement::permissions::PathScope::Subtree => {
                        !auth.starts_with(&grant.path)
                    }
                }
            }));
            let config: serde_json::Value = serde_json::from_str(
                &env("OPENCODE_CONFIG_CONTENT")
                    .expect("inline config")
                    .to_string_lossy(),
            )
            .expect("JSON config");
            if access == Access::WorktreeWrite {
                assert_eq!(config["permission"], serde_json::json!({"*": "allow"}));
            } else {
                assert_eq!(config["permission"]["edit"], "deny");
                assert_eq!(config["permission"]["bash"], "deny");
                let scratch = format!("{}/**", plan.permissions().scratch.path.display());
                if git_context {
                    assert_eq!(
                        config["permission"]["external_directory"],
                        serde_json::json!({ scratch.clone(): "allow" })
                    );
                    assert_eq!(config["permission"]["read"][&scratch], "allow");
                } else {
                    assert!(config["permission"]["external_directory"].is_null());
                    assert!(config["permission"]["read"].is_null());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn cursor_and_claude_keep_sessions_private_with_or_without_skills() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    for id in ["cursor", "claude"] {
        for skills in [false, true] {
            for access in [Access::ReadOnly, Access::WorktreeWrite] {
                let mut route = route(id, "fixture-model");
                route.account_key = None;
                route.set_user_skills(skills);
                let plan = fixture_access_plan(access, (id == "cursor").then_some("login"), false);
                let input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), (id == "claude").then_some(&token));
                let adapter = REGISTRY.iter().find(|entry| entry.adapter.id() == id).unwrap().adapter;
                let spec = adapter.prepare(input)?;
                let key = if id == "claude" { "CLAUDE_CONFIG_DIR" } else { "HOME" };
                let expected = if id == "claude" { "/fixture/state/claude-config" } else { "/fixture/state" };
                assert!(spec.env.iter().any(|(name, value)| name == key && value == expected));
                assert!(spec.profile.spec.state_writes.iter().all(|grant| !grant.path.starts_with("/fixture/home")));
                if id == "cursor" {
                    assert!(spec.setup.contains(&SetupStep::CreateDir("/fixture/state/.cursor/chats".into())));
                }
            }
        }
    }
    Ok(())
}
