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

pub(super) fn fixture_policy() -> &'static provenant_records::policy::Policy {
    static POLICY: std::sync::OnceLock<provenant_records::policy::Policy> =
        std::sync::OnceLock::new();
    POLICY.get_or_init(provenant_records::policy::Policy::default)
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
        provider_model: None,
        confined,
        prompt,
        continuation: None,
        grant_digest: "fixture",
        policy: fixture_policy(),
        credential_roots: provenant_core::CredentialRoots::for_home(std::path::Path::new(
            "/fixture/home",
        )),
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
    assert!(!spec.profile.spec.confinement.keychain_permitted());
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
            if id != "claude" || access == Access::WorktreeWrite {
                assert!(!spec.argv.iter().any(|arg| arg == "--add-dir"));
            }
        }
    }
    Ok(())
}

#[test]
fn environment_inheritance_follows_access_and_sandbox() -> Result<(), ProviderError> {
    use provenant_records::LaneAccess;
    let prompt = Prompt::new("fixture".into());
    let plan = fixture_access_plan(Access::WorktreeWrite, None, false);
    let host = || {
        [
            ("AWS_PROFILE".into(), "fixture-profile".into()),
            ("AWS_ACCESS_KEY_ID".into(), "fixture-key".into()),
            ("GITHUB_TOKEN".into(), "fixture-token".into()),
            ("JAVA_HOME".into(), "/fixture/java".into()),
            ("TMPDIR".into(), "/host/tmp".into()),
            ("TMPPREFIX".into(), "/host/zsh".into()),
        ]
        .into_iter()
    };
    for sandboxed in [false, true] {
        for access in [LaneAccess::Host, LaneAccess::Isolated] {
            let mut route = route("codex", "fixture-model");
            route.set_access(access);
            let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
            let host_var = |name: &str| {
                (name == provenant_core::consumer_env::SANDBOX)
                    .then(|| if sandboxed { "on" } else { "off" }.into())
            };
            input.host_var = &host_var;
            let env = crate::launch::base_env_with_host(
                &input,
                Path::new("/fixture/tmp"),
                "/bin",
                host(),
            )?;
            for (key, value) in host().take(4) {
                assert_eq!(
                    env.iter().any(|entry| entry.0 == key && entry.1 == value),
                    !(sandboxed && access == LaneAccess::Isolated),
                    "{access:?} sandboxed={sandboxed} {key:?}"
                );
            }
            let value = |key: &str| {
                env.iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value.as_os_str())
            };
            if sandboxed && access == LaneAccess::Isolated {
                for (key, expected) in [
                    ("HOME", "/fixture/home"),
                    ("USER", "fixture"),
                    ("PATH", "/bin"),
                    ("LANG", "en_US.UTF-8"),
                ] {
                    assert_eq!(value(key), Some(std::ffi::OsStr::new(expected)));
                }
            }
            for (key, native, confined) in [
                ("TMPDIR", "/host/tmp", "/fixture/tmp"),
                ("TMPPREFIX", "/host/zsh", "/fixture/tmp/zsh"),
                (provenant_core::consumer_env::SANDBOX, "off", "on"),
            ] {
                assert_eq!(
                    value(key),
                    Some(std::ffi::OsStr::new(if sandboxed { confined } else { native }))
                );
            }
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
            let declaration = provenant_records::AdapterSpec::get(id).expect("adapter declaration");
            let authentication = declaration.authentication;
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
fn claude_native_permission_controls_ignore_host_scrub_overrides() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let token = fixture_token();
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
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
                .windows(crate::launch::CLAUDE_NATIVE_ARGUMENTS.len())
                .any(|args| {
                    args.iter().zip(crate::launch::CLAUDE_NATIVE_ARGUMENTS).all(
                        |(actual, expected)| actual.as_os_str() == std::ffi::OsStr::new(expected),
                    )
                })
        );
    }
    Ok(())
}

#[test]
fn model_adapters_follow_declared_prompt_transport_without_exposing_tokens()
-> Result<(), ProviderError> {
    let prompt = Prompt::new("  to make a test pass: fix the code\npassword=fixture-token\nghp_abcdefghijklmnopqrstuvwxyz0123456789\n".into());
    let token = fixture_token();
    let repo = Path::new("/fixture/repo");
    for entry in REGISTRY.iter().filter(|entry| {
        provenant_records::AdapterSpec::get(entry.adapter.id())
            .is_some_and(|spec| spec.launch.model)
    }) {
        let id = entry.adapter.id();
        let declaration = provenant_records::AdapterSpec::get(id).expect("adapter declaration");
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        let plan = read_only_plan("/fixture", declaration.authentication);
        let prepared = entry.adapter.prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            repo,
            (id == "claude").then_some(&token),
        ))?;

        assert_eq!(
            cargo_home_path(&prepared),
            Path::new("/fixture/home/.cargo")
        );
        if declaration.argv_prompt {
            assert!(prepared.argv.iter().any(|arg| arg == prompt.text()));
        } else {
            assert_eq!(
                prepared.stdin,
                Stdin::Bytes(prompt.text().as_bytes().to_vec())
            );
            assert!(
                prepared
                    .argv
                    .iter()
                    .all(|arg| !arg.to_string_lossy().contains(prompt.text()))
            );
        }
        assert!(prepared.env.iter().all(|(_, value)| value != FIXTURE_TOKEN));
        if let Some(secret) = &prepared.secret_input {
            assert_eq!(secret.bytes, FIXTURE_TOKEN.as_bytes());
            assert!(
                prepared
                    .env
                    .iter()
                    .all(|(key, _)| key.as_os_str() != secret.descriptor_env.as_str())
            );
        }
    }
    Ok(())
}

#[test]
fn claude_credentials_require_a_host_token_without_profile_authentication() {
    let prompt = Prompt::new("fixture prompt".into());
    let token = fixture_token();
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    for (authentication, provide_token) in [(None, false), (Some("login"), true)] {
        let plan = read_only_plan("/fixture", authentication);
        let mut input = fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            provide_token.then_some(&token),
        );
        input.account = None;
        assert!(matches!(
            claude::Claude.prepare(input),
            Err(ProviderError::UnsupportedPolicy)
        ));
    }
}

#[test]
fn read_only_launch_requires_cwd_inside_a_declared_read_root() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let token = fixture_token();
    for entry in REGISTRY.iter().filter(|entry| {
        provenant_records::AdapterSpec::get(entry.adapter.id())
            .is_some_and(|declaration| declaration.launch.model)
    }) {
        let id = entry.adapter.id();
        let declaration = provenant_records::AdapterSpec::get(id).expect("adapter declaration");
        let plan = read_only_plan("/fixture", declaration.authentication);
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        assert!(matches!(
            entry.adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/elsewhere"),
                (id == "claude").then_some(&token),
            )),
            Err(ProviderError::InvalidPath)
        ));
    }
    Ok(())
}

#[test]
fn cursor_uses_outer_confinement_for_fresh_and_resumed_tool_access() -> Result<(), ProviderError> {
    let prompt = Prompt::new("--literal prompt".into());
    let route = route("cursor", "grok-4.7-high");
    let continuation = Continuation::retained("cursor", "fixture-session", &route, "fixture")?;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for resume in [false, true] {
            let plan = fixture_access_plan(access, Some("login"), resume);
            let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
            input.continuation = resume.then_some(&continuation);
            let spec = cursor::Cursor.prepare(input)?;
            let has_flag = |flag: &str| spec.argv.iter().any(|arg| arg == flag);
            let has_pair = |flag: &str, value: &str| {
                spec.argv
                    .windows(2)
                    .any(|pair| pair[0] == flag && pair[1] == value)
            };
            assert!(has_flag("--print"));
            assert!(has_pair("--output-format", "stream-json"));
            assert!(has_pair("--workspace", "/fixture/repo"));
            assert!(has_flag("--trust"));
            assert!(has_pair("--sandbox", "disabled"));
            assert!(has_flag("--force"));
            if resume {
                assert!(has_pair("--resume", "fixture-session"));
            } else {
                assert!(!has_flag("--resume"));
            }
            assert!(has_pair("--model", "grok-4.7-high"));
            assert!(spec.argv.iter().any(|arg| arg == prompt.text()));
            assert_eq!(spec.stdin, Stdin::Null);
            assert_eq!(spec.cwd, Path::new("/fixture/repo"));
        }
    }
    let plan = read_only_plan("/fixture", Some("login"));
    let prompt = Prompt::new("x".repeat(65_537));
    let mut strict = provenant_records::policy::Policy::default();
    strict.overlay(
        &std::collections::BTreeMap::from([(
            "ARGV_PROMPT".into(),
            provenant_records::policy::Strength::Refuse,
        )]),
        provenant_records::policy::Source::Project,
    );
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.policy = &strict;
    assert!(matches!(
        cursor::Cursor.prepare(input),
        Err(ProviderError::PromptTooLarge { .. })
    ));
    Ok(())
}

// Spec: docs/specs/routing.md#provider-model-scope
#[test]
fn cursor_effort_launch_spelling_keeps_canonical_continuation_identity()
-> Result<(), ProviderError> {
    let prompt = Prompt::new("resume this session".into());
    let mut route = route("cursor", "grok-4.7");
    route.effort = Some("high".into());
    let continuation = Continuation::retained("cursor", "fixture-session", &route, "fixture")?;
    let plan = fixture_access_plan(Access::ReadOnly, Some("login"), true);
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.provider_model = Some("grok-4.7-high");
    input.continuation = Some(&continuation);

    let spec = cursor::Cursor.prepare(input)?;
    assert_eq!(route.model, "grok-4.7");
    assert_eq!(route.effort.as_deref(), Some("high"));
    assert!(spec.argv.windows(2).any(|pair| {
        pair[0] == "--model" && pair[1] == "grok-4.7-high"
    }));
    assert!(spec.argv.windows(2).any(|pair| {
        pair[0] == "--resume" && pair[1] == "fixture-session"
    }));
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
    assert_eq!(
        added,
        [
            OsString::from("/fixture/state"),
            OsString::from("/fixture/build")
        ]
    );
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
            OsString::from("/fixture/state"),
            OsString::from("/fixture/repo"),
            OsString::from("/fixture/diffs")
        ]
    );
    Ok(())
}

#[test]
fn opencode_profile_follows_lane_access() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path();
    let state = home.join("state");
    std::fs::create_dir_all(state.join("opencode"))?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    std::fs::write(state.join("opencode/service.json"), format!(r#"{{"url":"http://{}"}}"#, listener.local_addr()?))?;
    let lookup = |key: &str| match key {
        "XDG_CONFIG_HOME" => Some(home.join("config").into_os_string()),
        "XDG_DATA_HOME" => Some(home.join("data").into_os_string()),
        "XDG_STATE_HOME" => Some(state.clone().into_os_string()),
        "XDG_CACHE_HOME" => Some(home.join("cache").into_os_string()),
        _ => None,
    };
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().expect("health connection");
        let mut request = [0; 256];
        let _ = stream.read(&mut request);
        stream.write_all(b"HTTP/1.1 401 Unauthorized\r\n\r\n").expect("health response");
    });
    let prompt = Prompt::new("fixture prompt".into());
    let base = fixture_access_plan(Access::ReadOnly, Some("login"), false);
    let plan_for = |access: provenant_records::LaneAccess| {
        use provenant_records::{LaneAccessReceipt, LaneAccessSource};
        let mut permissions = base.permissions().clone();
        let mut policy = provenant_confinement::run_config::RunConfig::default().policy_for_launch(
            &state,
            provenant_confinement::permissions::MandatoryRoots::new(
                home,
                &state,
                Path::new("/fixture/repo"),
            )?,
        );
        policy.mandatory_mut().set_lane_access(LaneAccessReceipt {
            access,
            source: LaneAccessSource::Default,
            key: "lanes.access".into(),
            extra: Default::default(),
        });
        permissions.policy = Some(policy);
        provenant_confinement::ConfinedPlan::new(provenant_confinement::PlanRequest {
            scope: base.scope(),
            permissions,
            codex: provenant_confinement::CodexPolicy::Clean,
            extensions: provenant_confinement::ExtensionPolicy::Deny,
            agy_state: None,
        })
    };
    let mut route = route("opencode", "configured/model");
    route.set_access(provenant_records::LaneAccess::Host);
    let host_plan = plan_for(provenant_records::LaneAccess::Host)?;
    let mut input = fixture_input(
        &route,
        &host_plan,
        &prompt,
        Path::new("/fixture/repo"),
        None,
    );
    input.home = home;
    input.host_var = &lookup;
    let attached = opencode::OpenCode.prepare(input)?;
    server.join().expect("health server");
    for (key, name) in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache")] {
        assert_eq!(attached.env.iter().find(|(k, _)| k == key).map(|(_, v)| Path::new(v)), Some(home.join(name).as_path()));
        assert!(attached.profile.spec.state_writes.contains(&crate::launch::subtree(home.join(name).join("opencode"))));
    }
    assert!(!attached.argv.iter().any(|arg| arg == "--standalone"));
    assert!(attached.profile.spec.loopback_server);
    std::fs::remove_file(state.join("opencode/service.json"))?;
    let fallback_plan = plan_for(provenant_records::LaneAccess::Host)?;
    let mut input = fixture_input(
        &route,
        &fallback_plan,
        &prompt,
        Path::new("/fixture/repo"),
        None,
    );
    input.home = home;
    input.host_var = &lookup;
    let fallback = opencode::OpenCode.prepare(input)?;
    assert!(fallback.argv.iter().any(|arg| arg == "--standalone"));
    assert!(fallback.warnings.iter().any(|warning| warning.contains("host service unavailable")));
    assert_eq!(fallback.session_state, crate::SessionStateLocation::Native { root: home.join("data/opencode") });
    route.set_access(provenant_records::LaneAccess::Isolated);
    let lane_plan = plan_for(provenant_records::LaneAccess::Isolated)?;
    let mut input = fixture_input(&route, &lane_plan, &prompt, Path::new("/fixture/repo"), None);
    input.home = home;
    input.host_var = &lookup;
    let lane = opencode::OpenCode.prepare(input)?;
    assert!(lane.argv.iter().any(|arg| arg == "--standalone"));
    assert_eq!(lane.session_state, crate::SessionStateLocation::AttemptPrivate);
    let private = &lane_plan.permissions().private_state.as_ref().expect("private state").path;
    assert_eq!(lane.env.iter().find(|(k, _)| k == "XDG_DATA_HOME").map(|(_, v)| Path::new(v)), Some(private.join("data").as_path()));
    Ok(())
}

#[test]
fn opencode_uses_user_config_and_accounts_with_read_only_permissions() -> Result<(), ProviderError>
{
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("opencode", "configured/model");
    let home = Path::new("/fixture/home");
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for (git_context, sandboxed) in [(false, true), (true, true), (false, false), (true, false)]
        {
            let plan = fixture_access_plan(access, Some("login"), false);
            let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
            input.home = home;
            let sandbox_mode = |key: &str| {
                (key == provenant_core::consumer_env::SANDBOX)
                    .then(|| if sandboxed { "on" } else { "off" }.into())
            };
            let spec = opencode::OpenCode.prepare(PrepareInput {
                host_var: &sandbox_mode,
                git_context,
                ..input
            })?;
            assert_eq!(
                spec.session_state,
                crate::SessionStateLocation::AttemptPrivate,
                "OpenCode keeps its session database in attempt state even on native launches"
            );
            let env = |key: &str| {
                spec.env
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value)
            };
            assert_eq!(Path::new(env("HOME").expect("home")), home);
            let state_root = &plan
                .permissions()
                .private_state
                .as_ref()
                .expect("fixture private state")
                .path;
            for (key, suffix) in [
                ("XDG_CONFIG_HOME", "config"),
                ("XDG_DATA_HOME", "data"),
                ("XDG_CACHE_HOME", "cache"),
                ("XDG_STATE_HOME", "state"),
            ] {
                let expected = state_root.join(suffix);
                assert_eq!(
                    env(key).map(Path::new),
                    Some(expected.as_path()),
                    "{key}"
                );
            }
            // Repository context may omit this optional selector. When present,
            // it must be unambiguous.
            assert!(
                spec.env
                    .iter()
                    .filter(|(key, _)| key == "GIT_CONFIG_NOSYSTEM")
                    .count()
                    <= 1
            );
            let globals: Vec<_> = spec
                .env
                .iter()
                .filter(|(key, _)| key == "GIT_CONFIG_GLOBAL")
                .map(|(_, value)| Path::new(value))
                .collect();
            assert_eq!(globals.len(), 1, "one unambiguous Git config selector");
            assert!(
                globals[0] == Path::new("/dev/null")
                    || spec.setup.iter().any(|step| {
                        matches!(step, SetupStep::WriteFile { path, .. } if path == globals[0])
                    }),
                "Git config must be disabled or supplied by launch setup"
            );
            let config_home = Path::new(env("XDG_CONFIG_HOME").expect("config home"));
            let data_home = Path::new(env("XDG_DATA_HOME").expect("data home"));
            assert_eq!(config_home, state_root.join("config"));
            assert_eq!(data_home, state_root.join("data"));
            if config_home.join("opencode/skills").is_dir() {
                assert!(
                    spec.profile
                        .spec
                        .runtime_reads
                        .contains(&crate::launch::subtree(config_home.join("opencode/skills")))
                );
            }
            assert!(spec.profile.spec.authentication.iter().any(|grant| {
                grant.roots.contains(&crate::launch::exact(
                    home.join(".local/share/opencode/auth.json"),
                ))
            }));
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
            let permissions = config["permission"]
                .as_object()
                .expect("OpenCode permission entries");
            let native = crate::launch::OPENCODE_NATIVE_PERMISSIONS;
            assert_eq!(
                permissions
                    .get(native.default_key)
                    .and_then(serde_json::Value::as_str),
                Some(native.default_value)
            );
            if access == Access::ReadOnly {
                let scratch = format!("{}/**", plan.permissions().scratch.path.display());
                let state = format!("{}/**", state_root.display());
                let external = config["permission"][native.external_directory_key]
                    .as_object()
                    .expect("external directory permissions");
                let reads = config["permission"][native.read_key]
                    .as_object()
                    .expect("read permissions");
                assert_eq!(
                    external.get(&state).and_then(serde_json::Value::as_str),
                    Some(native.path_value)
                );
                assert_eq!(
                    reads.get(&state).and_then(serde_json::Value::as_str),
                    Some(native.path_value)
                );
                if git_context {
                    assert_eq!(
                        external.get(&scratch).and_then(serde_json::Value::as_str),
                        Some(native.path_value)
                    );
                    assert_eq!(
                        reads.get(&scratch).and_then(serde_json::Value::as_str),
                        Some(native.path_value)
                    );
                } else {
                    assert!(!external.contains_key(&scratch));
                    assert!(!reads.contains_key(&scratch));
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
                let input = fixture_input(
                    &route,
                    &plan,
                    &prompt,
                    Path::new("/fixture/repo"),
                    (id == "claude").then_some(&token),
                );
                let adapter = REGISTRY
                    .iter()
                    .find(|entry| entry.adapter.id() == id)
                    .unwrap()
                    .adapter;
                let spec = adapter.prepare(input)?;
                assert_eq!(
                    spec.session_state,
                    crate::SessionStateLocation::AttemptPrivate,
                    "sandboxed {id} state stays in the attempt"
                );
                let key = if id == "claude" {
                    "CLAUDE_CONFIG_DIR"
                } else {
                    "HOME"
                };
                let expected = if id == "claude" {
                    "/fixture/state/claude-config"
                } else {
                    "/fixture/state"
                };
                assert!(
                    spec.env
                        .iter()
                        .any(|(name, value)| name == key && value == expected)
                );
                assert!(
                    spec.profile
                        .spec
                        .state_writes
                        .iter()
                        .all(|grant| !grant.path.starts_with("/fixture/home"))
                );
                if id == "cursor" {
                    assert!(
                        spec.setup
                            .contains(&SetupStep::CreateDir("/fixture/state/.cursor/chats".into()))
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn sandboxed_host_access_uses_native_homes_and_authentication() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture".into());
    for id in ["codex", "cursor", "claude", "agy"] {
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        let plan = fixture_access_plan(Access::WorktreeWrite, None, false);
        let adapter = REGISTRY.iter().find(|entry| entry.adapter.id() == id).unwrap().adapter;
        assert!(matches!(adapter.prepare(fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None)), Err(ProviderError::UnsupportedPolicy)));
        route.set_access(provenant_records::LaneAccess::Host);
        let spec = adapter.prepare(fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None))?;
        let expected = match id {
            "codex" => Path::new("/fixture/home/.codex"),
            "claude" => Path::new("/fixture/home/.claude"),
            _ => Path::new("/fixture/home"),
        };
        assert_eq!(spec.session_state, crate::SessionStateLocation::Native { root: expected.into() });
        assert!(spec.user_skills.root.is_none());
        assert!(spec.secret_input.is_none());
        assert!(!spec.env.iter().any(|(key, value)| key == "GIT_CONFIG_GLOBAL" && value == "/dev/null"));
        let state_root = if id == "agy" { Path::new("/fixture/home/.gemini") } else if id == "cursor" { Path::new("/fixture/home/.cursor") } else { expected };
        assert!(spec.profile.spec.state_writes.iter().any(|grant| grant.path == state_root));
        // A host lane reads its native provider home, but run-owned setup may
        // not create or rewrite it: those paths are refused before launch.
        let native_home = Path::new("/fixture/home");
        for step in &spec.setup {
            let path = match step {
                SetupStep::CreateDir(path)
                | SetupStep::WriteFile { path, .. }
                | SetupStep::WriteExecutable { path, .. }
                | SetupStep::CreateLock(path) => path,
                SetupStep::Symlink { link, .. } => link,
            };
            assert!(
                !path.starts_with(native_home),
                "{id}: host setup must not write the native home: {step:?}"
            );
        }
        assert!(!spec.env.iter().any(|(key, value)| (key == "HOME" || key == "CODEX_HOME" || key == "CLAUDE_CONFIG_DIR") && Path::new(value).starts_with("/fixture/state")));
        if id == "claude" {
            assert!(!spec.argv.iter().any(|arg| arg == "--strict-mcp-config" || arg == "--setting-sources"));
        }
        if id == "codex" {
            assert!(!spec.argv.iter().any(|arg| arg == "--ignore-user-config"));
        }
    }
    Ok(())
}

#[test]
fn native_adapters_preserve_the_host_environment_and_sandbox_tool_preparation()
-> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    let host: Vec<_> = std::env::vars_os().collect();
    for id in ["codex", "claude", "agy", "cursor"] {
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        let auth = match id {
            "codex" | "cursor" => Some("login"),
            "agy" => Some("keychain"),
            _ => None,
        };
        let plan = fixture_access_plan(Access::WorktreeWrite, auth, false);
        let adapter = REGISTRY
            .iter()
            .find(|entry| entry.adapter.id() == id)
            .unwrap()
            .adapter;
        let mut input = fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            Some(&token),
        );
        input.host_var =
            &|name| (name == provenant_core::consumer_env::SANDBOX).then(|| "off".into());
        let native = adapter.prepare(input)?;
        let crate::SessionStateLocation::Native { root: session_root } = &native.session_state
        else {
            return Err(ProviderError::UnknownIdentity);
        };
        assert!(session_root.is_absolute());
        match id {
            "agy" | "cursor" => assert_eq!(session_root, Path::new("/fixture/home")),
            "claude" => assert_eq!(session_root, Path::new("/fixture/home/.claude")),
            "codex" => {}
            _ => unreachable!("only native session-state adapters are listed"),
        }
        for (key, value) in &host {
            // These selectors bind the child to its provider, Fabric context
            // and private Cargo configuration/target with shared downloads.
            if key.to_str().is_some_and(|key| {
                key.starts_with("PROVENANT_")
                    || matches!(
                        key,
                        "PATH"
                            | "HOME"
                            | "TMPDIR"
                            | "TMPPREFIX"
                            | "CARGO_HOME"
                            | "RUSTUP_HOME"
                            | "RUSTUP_AUTO_INSTALL"
                            | "CARGO_TARGET_DIR"
                            | "CARGO_BUILD_BUILD_DIR"
                            | "CARGO_CACHE_AUTO_CLEAN_FREQUENCY"
                            | "CODEX_HOME"
                            | "CLAUDE_CONFIG_DIR"
                            | "CLAUDE_CODE_TMPDIR"
                            | "DISABLE_AUTOUPDATER"
                            | "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"
                    )
            }) {
                continue;
            }
            assert!(
                native
                    .env
                    .iter()
                    .any(|entry| &entry.0 == key && &entry.1 == value),
                "native adapter dropped a host environment binding"
            );
        }
        let homes: Vec<_> = native
            .env
            .iter()
            .filter(|(key, _)| key == "CARGO_HOME")
            .collect();
        assert_eq!(homes.len(), 1);
        assert_eq!(homes[0].1, "/fixture/home/.cargo");
        assert_eq!(cargo_home_path(&native), Path::new("/fixture/home/.cargo"));
        if let Some((_, host_path)) = host.iter().find(|(key, _)| key == "PATH") {
            let child_path = &native.env.iter().find(|(key, _)| key == "PATH").unwrap().1;
            let child_paths: Vec<_> = std::env::split_paths(child_path).collect();
            assert!(std::env::split_paths(host_path).all(|path| child_paths.contains(&path)));
        }
        assert!(native.user_skills.root.is_none());
        assert!(native.tool_requirements.reads.is_empty());
        assert!(native.tool_requirements.exec.is_empty());
        if matches!(id, "codex" | "claude" | "agy") {
            let confined = adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                Some(&token),
            ))?;
            assert_eq!(
                confined.session_state,
                crate::SessionStateLocation::AttemptPrivate,
                "sandboxed {id} state stays in the attempt"
            );
            assert!(!confined.tool_requirements.exec.is_empty());
        }
    }
    Ok(())
}

// Spec: docs/specs/supervisor.md#native-environment-and-sandbox-tool-stores
#[test]
fn sandbox_path_keeps_requester_env_interpreter_ahead_of_fallbacks()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let requester_bin = temp.path().join("requester-bin");
    std::fs::create_dir(&requester_bin)?;
    let preferred_shell = requester_bin.join("sh");
    std::fs::write(&preferred_shell, "#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(&preferred_shell, std::fs::Permissions::from_mode(0o700))?;
    let requester_path = std::env::join_paths([requester_bin.as_path()])?;
    let host_var = |name: &str| match name {
        "PATH" => Some(requester_path.clone()),
        name if name == provenant_core::consumer_env::SANDBOX => Some("on".into()),
        _ => None,
    };
    let prompt = Prompt::new("fixture".into());
    let mut route = route("codex", "fixture-model");
    // This fixture uses local login; it has no registered Codex identity.
    route.account_key = None;
    let plan = fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
    let private_wrapper = plan.permissions().scratch.path.join("bin");
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.host_var = &host_var;
    let spec = codex::Codex.prepare(input)?;
    let path = spec
        .env
        .iter()
        .find(|(key, _)| key == "PATH")
        .map(|(_, path)| path)
        .expect("sandbox PATH");
    let entries = std::env::split_paths(path).collect::<Vec<_>>();
    assert!(entries.contains(&std::path::PathBuf::from("/fixture/home/.cargo/bin")));
    assert!(entries.contains(&std::path::PathBuf::from("/usr/bin")));
    assert!(entries.contains(&std::path::PathBuf::from("/bin")));
    assert!(entries.contains(&requester_bin));
    let requester_index = entries.iter().position(|entry| entry == &requester_bin).unwrap();
    let cargo_index = entries
        .iter()
        .position(|entry| entry == Path::new("/fixture/home/.cargo/bin"))
        .unwrap();
    let system_index = entries
        .iter()
        .position(|entry| entry == Path::new("/usr/bin"))
        .unwrap();
    let bin_index = entries
        .iter()
        .position(|entry| entry == Path::new("/bin"))
        .unwrap();
    assert_eq!(entries.first(), Some(&private_wrapper));
    assert!(requester_index < cargo_index && cargo_index < system_index && system_index < bin_index);
    let env_shell = entries
        .iter()
        .map(|directory| directory.join("sh"))
        .find(|candidate| candidate.is_file());
    assert_eq!(env_shell.as_deref(), Some(preferred_shell.as_path()));
    assert!(spec.profile.spec.tool_exec_roots.iter().all(|grant| {
        !grant.path.starts_with(&requester_bin)
    }));
    Ok(())
}

#[test]
fn native_provider_preparation_uses_requester_path_without_supervisor_rustup()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    const CHILD: &str = "PROVENANT_TEST_REQUESTER_PATH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let argv = [
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "tests::native_provider_preparation_uses_requester_path_without_supervisor_rustup"
                .into(),
            "--nocapture".into(),
        ];
        let env = [
            (CHILD.into(), "1".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ];
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(60));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65536 };
        options.stderr = StreamPolicy::Capture { limit: 65536 };
        let output = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(output.termination, Termination::Exit(0), "{output:?}");
        assert!(!output.timed_out, "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout.bytes).contains("1 passed"), "{output:?}");
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let requester_bin = temp.path().join("requester/.cargo/bin");
    std::fs::create_dir_all(&requester_bin)?;
    std::fs::write(requester_bin.join("rustup"), b"requester rustup shim")?;
    std::fs::set_permissions(
        requester_bin.join("rustup"),
        std::fs::Permissions::from_mode(0o755),
    )?;
    std::os::unix::fs::symlink("rustup", requester_bin.join("rustc"))?;
    let requester_path = std::env::join_paths([
        requester_bin.as_os_str(),
        std::ffi::OsStr::new("/usr/bin"),
        std::ffi::OsStr::new("/bin"),
    ])?;
    let supervisor_path = std::env::var_os("PATH").unwrap_or_default();
    assert!(
        !std::env::split_paths(&supervisor_path).any(|entry| entry == requester_bin),
        "fixture rustup shim unexpectedly appears on the supervisor PATH"
    );
    let host_var = |name: &str| match name {
        "PATH" => Some(requester_path.clone()),
        name if name == provenant_core::consumer_env::SANDBOX => Some("off".into()),
        _ => None,
    };

    let prompt = Prompt::new("fixture".into());
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let plan = fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
    let mut input = fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo"),
        None,
    );
    input.host_var = &host_var;

    let prepared = codex::Codex.prepare(input)?;
    assert!(prepared.env.iter().any(|(key, value)| {
        key == provenant_core::consumer_env::SANDBOX && value == "off"
    }));
    let paths = prepared
        .env
        .iter()
        .filter(|(key, _)| key == "PATH")
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 1);
    let lane_path = std::env::split_paths(paths[0]).collect::<Vec<_>>();
    assert!(
        lane_path.contains(&requester_bin),
        "prepared native lane PATH omitted the requester's rustup directory: {lane_path:?}"
    );
    Ok(())
}

/// Small: all adapters use the same identity/cache bindings, including a
/// sparse native environment. No process-global environment mutation is needed.
#[test]
fn every_adapter_finalises_missing_lane_environment() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    let argv = vec!["/bin/echo".to_owned(), "fixture".to_owned()];
    for entry in REGISTRY {
        let id = entry.adapter.id();
        for native in [false, true] {
            let auth = match id {
                "codex" | "cursor" | "opencode" => Some("login"),
                "agy" => Some("keychain"),
                _ => None,
            };
            let plan = fixture_access_plan(Access::ReadOnly, auth, false);
            let mut route = route(id, if id == "command" { "" } else { "fixture-model" });
            route.account_key = None;
            let host_var = |name: &str| {
                (native && name == provenant_core::consumer_env::SANDBOX).then(|| "off".into())
            };
            let mut input = fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                (id == "claude").then_some(&token),
            );
            input.host_var = &host_var;
            if id == "command" {
                input.command = Some(command::Invocation {
                    argv: &argv,
                    program: Path::new("/bin/echo"),
                    runtimes: &[],
                });
            }
            let mut spec = entry.adapter.prepare(input)?;
            let selected_home = spec
                .env
                .iter()
                .find(|(key, _)| key == "HOME")
                .map(|(_, value)| value.clone())
                .expect("adapter HOME");
            if native {
                assert_eq!(selected_home, std::ffi::OsString::from("/fixture/home"));
            }
            let assert_bindings = |spec: &LaunchSpec| {
                for (key, expected) in [
                    ("HOME", Path::new(&selected_home).to_path_buf()),
                    ("TMPDIR", plan.permissions().scratch.path.clone()),
                    ("CARGO_HOME", cargo_home_path(spec)),
                ] {
                    let values: Vec<_> = spec
                        .env
                        .iter()
                        .filter(|(name, _)| name == key)
                        .map(|(_, value)| value.as_os_str())
                        .collect();
                    assert_eq!(
                        values,
                        vec![expected.as_os_str()],
                        "{id} native={native} {key}"
                    );
                }
            };
            assert_bindings(&spec);
            spec.env
                .retain(|(key, _)| !matches!(key.to_str(), Some("TMPDIR" | "CARGO_HOME")));
            let mut input = fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                (id == "claude").then_some(&token),
            );
            input.host_var = &host_var;
            crate::launch::finalise_provider_env(&input, &mut spec)?;
            assert_bindings(&spec);
            spec.env.retain(|(key, _)| key != "HOME");
            crate::launch::finalise_provider_env(&input, &mut spec)?;
            let homes: Vec<_> = spec
                .env
                .iter()
                .filter(|(key, _)| key == "HOME")
                .map(|(_, value)| value.as_os_str())
                .collect();
            assert_eq!(homes, [input.home.as_os_str()]);
        }
    }
    Ok(())
}

// Spec: docs/specs/routing.md#provider-configuration-and-local-diagnostics
#[test]
fn codex_configuration_is_a_writer_preference_and_read_only_authority()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let prompt = Prompt::new("fixture".into());
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for configuration in [CodexPolicy::Clean, CodexPolicy::Inherited] {
            if access == Access::ReadOnly && configuration == CodexPolicy::Inherited {
                continue; // The plan owner rejects unconfined read-only extensions.
            }
            let base = fixture_access_plan(access, Some("login"), false);
            let plan = ConfinedPlan::new(PlanRequest {
                scope: base.scope(),
                permissions: base.permissions().clone(),
                codex: configuration,
                extensions: ExtensionPolicy::Deny,
                agy_state: None,
            })?;
            for sandboxed in [false, true] {
                let lookup = |key: &str| {
                    (key == provenant_core::consumer_env::SANDBOX)
                        .then(|| if sandboxed { "on" } else { "off" }.into())
                };
                let mut input =
                    fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
                input.host_var = &lookup;
                let launch = codex::Codex.prepare(input)?;
                assert_eq!(
                    launch.argv.iter().any(|arg| arg == "--ignore-user-config"),
                    sandboxed && configuration == CodexPolicy::Clean
                );
            }
        }
    }
    Ok(())
}

/// A sandboxed writer includes the host's global Git configuration in full and
/// receives a read grant on the host file so the include resolves. Host values
/// stay in the included file; only the token helper is appended to the lane's
/// command-scope configuration.
#[test]
fn sandboxed_private_git_config_includes_host_config() -> Result<(), Box<dyn std::error::Error>> {
    const TOKEN: &str = "ghp_fixtureLaneConfigToken";
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo)?;
    let host_config = home.join(".gitconfig");
    std::fs::write(
        &host_config,
        format!(
            "[user]\n name = Host Name\n email = host@example.com\n[alias]\n st = status --short\n\
             pub = !git push https://x-access-token:{TOKEN}@github.com/o/r\n\
             [http \"https://github.com/\"]\n extraHeader = AUTHORIZATION: bearer {TOKEN}\n"
        ),
    )?;
    let git = provenant_os::git::git_executable()
        .ok_or("Git unavailable")?
        .canonicalize()?;
    let prompt = Prompt::new("fixture".into());
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let plan = fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
    let mut input = fixture_input(&route, &plan, &prompt, &repo, None);
    input.home = &home;
    input.provenant.git = &git;
    input.credential_roots = provenant_core::CredentialRoots::for_home(&home);
    let spec = codex::Codex.prepare(input)?;
    let config = spec
        .setup
        .iter()
        .find_map(|step| match step {
            SetupStep::WriteFile { path, bytes } if path.ends_with("gitconfig") => {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
            _ => None,
        })
        .ok_or("private gitconfig")?;
    let canonical = host_config.canonicalize()?;
    assert!(
        config.contains(&format!(
            "[include]\n\tpath = \"{}\"\n",
            canonical.to_str().ok_or("config path")?
        )),
        "{config}"
    );
    assert!(config.contains("[user]\n\tuseConfigOnly = true\n"), "{config}");
    // The private file names the host file; it does not copy its values.
    assert!(!config.contains(TOKEN), "{config}");
    assert!(
        spec.profile.spec.runtime_reads.iter().any(|grant| {
            grant.path == canonical
                && grant.scope == provenant_confinement::permissions::PathScope::Exact
        }),
        "the included host file must be granted to the lane"
    );
    assert_eq!(
        spec.env
            .iter()
            .filter(|(key, _)| key == "GIT_CONFIG_NOSYSTEM")
            .count(),
        0,
        "the lane does not force system Git configuration off"
    );
    let get = |name: &str| {
        spec.env
            .iter()
            .find_map(|(key, value)| (key == name).then(|| value.to_string_lossy().into_owned()))
    };
    assert_eq!(get("GIT_CONFIG_COUNT").as_deref(), Some("2"));
    assert_eq!(get("GIT_CONFIG_KEY_0").as_deref(), Some("credential.helper"));
    assert_eq!(get("GIT_CONFIG_VALUE_0").as_deref(), Some(""));
    assert_eq!(
        get("GIT_CONFIG_KEY_1").as_deref(),
        Some("credential.https://github.com.helper")
    );
    Ok(())
}

// Spec: docs/specs/run-launch.md#lane-access
#[test]
fn ssh_agent_follows_lane_access() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::LaneAccess;
    let temp = tempfile::tempdir()?;
    let socket = temp.path().join("agent.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket)?;
    let canonical = socket.canonicalize()?;
    let host_var = |name: &str| (name == "SSH_AUTH_SOCK").then(|| socket.clone().into_os_string());
    let prompt = Prompt::new("fixture".into());
    let plan = fixture_access_plan(Access::ReadOnly, Some("login"), false);
    for access in [LaneAccess::Host, LaneAccess::Isolated] {
        let mut route = route("codex", "fixture-model");
        route.account_key = None;
        route.set_access(access);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.host_var = &host_var;
        let spec = codex::Codex.prepare(input)?;
        let agent = spec.env.iter().find(|(key, _)| key == "SSH_AUTH_SOCK");
        if access == LaneAccess::Host {
            assert_eq!(agent.map(|(_, value)| value), Some(&socket.clone().into_os_string()));
            let grant = spec.profile.spec.ssh_agent_socket.as_ref().expect("agent grant");
            assert_eq!(grant.path, canonical);
            assert_eq!(grant.scope, provenant_confinement::permissions::PathScope::Exact);
        } else {
            assert!(agent.is_none());
            assert!(spec.profile.spec.ssh_agent_socket.is_none());
        }
    }
    Ok(())
}

// Spec: docs/specs/run-launch.md#lane-access
#[test]
fn unbounded_reader_git_config_follows_lane_access() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::LaneAccess;
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let config = home.join(".gitconfig");
    std::fs::write(&config, "[alias]\n  fixture = status\n")?;
    let plan = fixture_access_plan(Access::ReadOnly, Some("login"), false);
    let prompt = Prompt::new("fixture".into());
    for access in [LaneAccess::Host, LaneAccess::Isolated] {
        let mut route = route("codex", "fixture-model");
        route.account_key = None;
        route.set_access(access);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.home = &home;
        let spec = codex::Codex.prepare(input)?;
        let global = spec.env.iter().find(|(key, _)| key == "GIT_CONFIG_GLOBAL").map(|(_, value)| value);
        let includes = spec.setup.iter().any(|step| match step {
            SetupStep::WriteFile { path, bytes } if path.ends_with("gitconfig") => String::from_utf8_lossy(bytes).contains(config.to_str().unwrap()),
            _ => false,
        });
        if access == LaneAccess::Host {
            assert!(includes);
            assert!(global.is_some_and(|value| value != "/dev/null"));
            assert!(spec.profile.spec.runtime_reads.iter().any(|grant| grant.path == config));
        } else {
            assert!(!includes);
            assert_eq!(global.map(|v| v.as_os_str()), Some(std::ffi::OsStr::new("/dev/null")));
        }
    }
    Ok(())
}

// Spec: docs/specs/run-launch.md#lane-access
#[test]
fn opencode_config_overlapping_a_credential_root_follows_lane_access()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{CredentialKind, CredentialScope};
    use provenant_records::{LaneAccess, LaneAccessReceipt, LaneAccessSource};
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let config = home.join(".config/opencode");
    std::fs::create_dir_all(&config)?;
    std::fs::write(config.join("opencode.json"), "{\"model\":\"fixture\"}")?;
    let state = home.join("state");
    let repo = home.join("repo");
    let base = read_only_plan(home.to_str().ok_or("home path")?, Some("login"));
    let mut roots = provenant_core::CredentialRoots::for_home(&home);
    roots.add(
        config.clone(),
        CredentialScope::Subtree,
        "fixture declared opencode config",
        CredentialKind::Store,
    );
    let mut permissions = base.permissions().clone();
    permissions.credential_roots = roots.clone();
    let prompt = Prompt::new("fixture".into());
    let canonical = config.canonicalize()?;
    for access in [LaneAccess::Host, LaneAccess::Isolated] {
        let mut policy = provenant_confinement::run_config::RunConfig::default().policy_for_launch(
            &state,
            provenant_confinement::permissions::MandatoryRoots::new(&home, &state, &repo)?,
        );
        policy.mandatory_mut().set_lane_access(LaneAccessReceipt {
            access,
            source: LaneAccessSource::Default,
            key: "lanes.access".into(),
            extra: Default::default(),
        });
        permissions.policy = Some(policy);
        let plan = provenant_confinement::ConfinedPlan::new(provenant_confinement::PlanRequest {
            scope: base.scope(),
            permissions: permissions.clone(),
            codex: provenant_confinement::CodexPolicy::Clean,
            extensions: provenant_confinement::ExtensionPolicy::Deny,
            agy_state: None,
        })?;
        let mut route = route("opencode", "fixture-model");
        route.account_key = None;
        route.set_access(access);
        let mut input = fixture_input(&route, &plan, &prompt, &repo, None);
        input.home = &home;
        input.credential_roots = roots.clone();
        let launch = opencode::OpenCode.prepare(input)?;
        let discovered = launch
            .profile
            .spec
            .runtime_reads
            .iter()
            .any(|grant| grant.path == canonical);
        let mirrored = launch.setup.iter().any(|step| {
            matches!(step, SetupStep::Symlink { link, target }
                if link.file_name() == Some(std::ffi::OsStr::new("opencode.json"))
                    && target == &config.join("opencode.json"))
        });
        let omitted = launch.omitted_discovery.iter().any(|entry| {
            entry.path == canonical && entry.source == crate::launch::OmissionSource::ProviderConfig
        });
        if access == LaneAccess::Host {
            let config_home = launch
                .env
                .iter()
                .find(|(key, _)| key == "XDG_CONFIG_HOME")
                .map(|(_, value)| std::path::PathBuf::from(value))
                .ok_or("host launch has no XDG_CONFIG_HOME")?;
            assert_eq!(
                config_home,
                home.join(".config"),
                "host lanes use the native XDG config home"
            );
            assert!(discovered, "host lanes read the overlapping config directory");
            assert!(
                !mirrored,
                "host lanes read the native config home instead of mirroring it into private state"
            );
            assert!(!omitted, "host lanes do not omit the overlapping config");
        } else {
            assert!(!discovered, "isolated lanes keep the credential refusal");
            assert!(!mirrored, "isolated lanes do not mirror credential material");
            assert!(omitted, "isolated lanes record the omitted config directory");
        }
    }
    Ok(())
}
