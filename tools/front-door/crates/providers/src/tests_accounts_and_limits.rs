#[test]
fn opencode_free_limit_preserves_provider_message() -> Result<(), Box<dyn std::error::Error>> {
    // Synthetic protocol regression from #1216, not an activation recording.
    let message = "Free tier exhausted. See https://opencode.ai/go\\quota";
    for wrapped in [false, true] {
        for string_body in [false, true] {
            let body =
                serde_json::json!({"error":{"type":"FreeUsageLimitError","message":message}});
            let body = if string_body {
                serde_json::Value::String(body.to_string())
            } else {
                body
            };
            let event =
                serde_json::json!({"type":"provider.quota","status":429,"response":{"body":body}});
            let event = if wrapped {
                serde_json::json!({"type":"error","error":event})
            } else {
                event
            };
            let (events, outcome) = replay(
                &opencode::OpenCode,
                &(event.to_string() + "\n"),
                ExitObservation {
                    code: Some(1),
                    timed_out: false,
                },
            )?;
            assert_eq!(outcome.status(), OutcomeStatus::Refused);
            assert!(events.contains(&ProviderEvent::QuotaLimited {
                message: message.into(),
                pool: Some("opencode-free".into()),
                resets_in_seconds: None,
            }));
        }
    }
    for response in [
        serde_json::json!({}),
        serde_json::json!({"body":{"error":{"code":429,"message":"provider quota message"}}}),
    ] {
        let event = serde_json::json!({"type":"provider.quota","message":"provider quota message","response":response});
        let (events, outcome) = replay(&opencode::OpenCode, &(event.to_string() + "\n"), EXIT)?;
        assert_eq!(outcome.status(), OutcomeStatus::Refused);
        assert!(events.contains(&ProviderEvent::QuotaLimited {
            message: "provider quota message".into(),
            pool: None,
            resets_in_seconds: None,
        }));
    }
    Ok(())
}

#[test]
fn opencode_free_limit_decodes_retry_after_ms() -> Result<(), Box<dyn std::error::Error>> {
    let message = "Free tier exhausted. See https://opencode.ai/go\\quota";
    for (retry_after_val, expected_resets) in [
        (serde_json::json!(240_000), Some(240)),
        (serde_json::json!(240_000.0), Some(240)),
        (serde_json::json!(1_500), Some(2)),
        (serde_json::json!(999_999_999), Some(86_400)),
        (serde_json::json!(null), None),
        (serde_json::json!(-1000), None),
        (serde_json::json!(0), None),
        (serde_json::json!("soon"), None),
        (serde_json::json!(true), None),
    ] {
        for wrapped in [false, true] {
            let body = serde_json::json!({
                "error": {
                    "type": "FreeUsageLimitError",
                    "message": message,
                }
            });
            let mut event = serde_json::json!({
                "type": "provider.quota",
                "status": 429,
                "response": {"body": body},
                "retryAfterMs": retry_after_val,
            });
            if wrapped {
                event = serde_json::json!({
                    "type": "error",
                    "error": event,
                });
            }
            let (events, outcome) = replay(
                &opencode::OpenCode,
                &(event.to_string() + "\n"),
                ExitObservation {
                    code: Some(1),
                    timed_out: false,
                },
            )?;
            assert_eq!(outcome.status(), OutcomeStatus::Refused);
            assert!(
                events.contains(&ProviderEvent::QuotaLimited {
                    message: message.into(),
                    pool: Some("opencode-free".into()),
                    resets_in_seconds: expected_resets,
                }),
                "failed for retry_after_val: {retry_after_val:?}, wrapped: {wrapped}"
            );
        }
    }

    let body = serde_json::json!({
        "error": {
            "type": "FreeUsageLimitError",
            "message": message,
            "retryAfterMs": 120_000,
        }
    });
    let event = serde_json::json!({
        "type": "provider.quota",
        "status": 429,
        "response": {"body": body},
    });
    let (events, outcome) = replay(
        &opencode::OpenCode,
        &(event.to_string() + "\n"),
        ExitObservation {
            code: Some(1),
            timed_out: false,
        },
    )?;
    assert_eq!(outcome.status(), OutcomeStatus::Refused);
    assert!(events.contains(&ProviderEvent::QuotaLimited {
        message: message.into(),
        pool: Some("opencode-free".into()),
        resets_in_seconds: Some(120),
    }));

    let body = serde_json::json!({
        "error": {
            "type": "FreeUsageLimitError",
            "message": message,
        }
    });
    let event = serde_json::json!({
        "type": "error",
        "retryAfterMs": 180_000,
        "error": {
            "type": "provider.quota",
            "status": 429,
            "response": {"body": body},
        }
    });
    let (events, outcome) = replay(
        &opencode::OpenCode,
        &(event.to_string() + "\n"),
        ExitObservation {
            code: Some(1),
            timed_out: false,
        },
    )?;
    assert_eq!(outcome.status(), OutcomeStatus::Refused);
    assert!(events.contains(&ProviderEvent::QuotaLimited {
        message: message.into(),
        pool: Some("opencode-free".into()),
        resets_in_seconds: Some(180),
    }));

    let (events, outcome) = replay(
        &opencode::OpenCode,
        fixture!("opencode/free-limit.jsonl"),
        ExitObservation {
            code: Some(1),
            timed_out: false,
        },
    )?;
    assert_eq!(outcome.status(), OutcomeStatus::Refused);
    assert!(events.contains(&ProviderEvent::QuotaLimited {
        message: "FIXTURE_USER_OR_PROVIDER_TEXT_021".into(),
        pool: Some("opencode-free".into()),
        resets_in_seconds: Some(240),
    }));

    Ok(())
}

#[test]
fn selected_accounts_control_provider_environment_and_exact_authentication_grants()
-> Result<(), Box<dyn std::error::Error>> {
    use std::path::Path;
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    let temp = tempfile::tempdir().unwrap();
    for adapter in ["claude", "codex"] {
        for key in ["fixture-one", "fixture-two"] {
            let mut route = route(adapter, "fixture-model");
            let email = format!("{key}@example.test");
            let account_key = if adapter == "claude" {
                identity::fingerprint(&email).unwrap()
            } else {
                key.into()
            };
            route.account_key = Some(account_key.clone());
            let dir = temp.path().join(key);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join(".credentials.json"),
                br#"{"claudeAiOauth":{"accessToken":"fixture-token","expiresAt":4000000000000}}"#,
            )?;
            let account = identity::RegisteredAccount::new(adapter, &account_key, dir.clone())?;
            let selected_token;
            let token = if adapter == "claude" {
                let status =
                    serde_json::to_vec(&serde_json::json!({"loggedIn":true,"email":email}))?;
                let script = KeychainScript {
                    items: std::sync::Mutex::new([br#"{"claudeAiOauth":{"accessToken":"fixture-token","expiresAt":4000000000000}}"#.to_vec()].into()),
                    statuses: std::sync::Mutex::new([status.clone(), status].into()),
                    calls: Default::default(),
                };
                selected_token = claude::refresh_for_account(
                    &script,
                    &claude::Login {
                        executable: Path::new("/fixture/bin/claude"),
                        home: Path::new("/fixture/home"),
                        user: "fixture".as_ref(),
                        config_dir: &dir,
                    },
                    &account,
                    &|| 1000,
                    10000,
                )?;
                &selected_token
            } else {
                &token
            };
            let confined = read_only_plan("/fixture", (adapter == "codex").then_some("login"));
            let mut input = fixture_input(
                &route,
                &confined,
                &prompt,
                Path::new("/fixture/repo"),
                Some(token),
            );
            input.account = Some(&account);
            let unplaced = fixture_input(
                &route,
                &confined,
                &prompt,
                Path::new("/fixture/repo"),
                Some(token),
            );
            assert!(
                REGISTRY
                    .iter()
                    .find(|r| r.adapter.id() == adapter)
                    .unwrap()
                    .adapter
                    .prepare(unplaced)
                    .is_err()
            );
            let spec = REGISTRY
                .iter()
                .find(|r| r.adapter.id() == adapter)
                .unwrap()
                .adapter
                .prepare(input)?;

            // Provider runtime grants must exclude the host association
            // database and every SQLite companion.
            let grants = &spec.profile.spec.state_writes;
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let host_index =
                    Path::new("/fixture/state/fabric").join(format!("host.sqlite3{suffix}"));
                assert!(
                    grants.iter().all(|grant| match grant.scope {
                        provenant_records::run::PathScope::Exact => grant.path != host_index,
                        provenant_records::run::PathScope::Subtree =>
                            !host_index.starts_with(&grant.path),
                    }),
                    "{adapter} can write {}",
                    host_index.display()
                );
            }

            if adapter == "claude" {
                let mut unattributed_route = route.clone();
                unattributed_route.account_key = None;
                assert!(matches!(
                    claude::Claude.prepare(fixture_input(
                        &unattributed_route,
                        &confined,
                        &prompt,
                        Path::new("/fixture/repo"),
                        Some(token)
                    )),
                    Err(ProviderError::IdentityMismatch)
                ));
                let other_key = identity::fingerprint("other@example.test").unwrap();
                let other = identity::RegisteredAccount::new(
                    "claude",
                    &other_key,
                    Path::new("/fixture/identities/other").into(),
                )?;
                let mut other_route = route.clone();
                other_route.account_key = Some(other_key);
                let mut wrong_token = fixture_input(
                    &other_route,
                    &confined,
                    &prompt,
                    Path::new("/fixture/repo"),
                    Some(token),
                );
                wrong_token.account = Some(&other);
                assert!(matches!(
                    claude::Claude.prepare(wrong_token),
                    Err(ProviderError::IdentityMismatch)
                ));
                assert!(
                    spec.env
                        .iter()
                        .any(|(k, v)| k == "CLAUDE_CONFIG_DIR"
                            && v == "/fixture/state/claude-config")
                );
                assert!(
                    spec.profile
                        .spec
                        .runtime_reads
                        .iter()
                        .all(|g| !g.path.starts_with(&dir))
                );
                assert!(
                    spec.profile
                        .spec
                        .state_writes
                        .iter()
                        .filter(|g| g.path.starts_with(&dir))
                        .all(|g| g.path.starts_with(dir.join("projects")))
                );
                assert!(
                    spec.profile
                        .spec
                        .state_writes
                        .iter()
                        .filter(|g| !g.path.starts_with("/fixture/state"))
                        .all(|g| g.path.starts_with(&dir))
                );
                let lane = spec
                    .profile
                    .spec
                    .state_writes
                    .iter()
                    .filter(|g| g.path.starts_with("/fixture/state"))
                    .cloned()
                    .collect::<Vec<_>>();
                // Child admission crosses the relay; no child-store write grant remains.
                assert_eq!(lane, [crate::launch::subtree("/fixture/state/workflow")]);
            } else {
                // The accepted design keeps runtime state per attempt. Only the
                // selected identity's auth.json is linked into CODEX_HOME.
                let auth = dir.join("auth.json");
                assert_eq!(spec.profile.spec.authentication[0].roots[0].path, auth);
                assert_eq!(
                    spec.profile.spec.authentication[0].in_place_writes[0].path,
                    auth
                );
                assert!(
                    matches!(&spec.setup[0], SetupStep::Symlink { target, .. } if target == &auth)
                );
            }
        }
    }
    Ok(())
}

#[cfg(feature = "recording")]
#[test]
fn claude_writer_shell_has_private_config_and_rust_tools() -> Result<(), ProviderError> {
    let plan = fixture_access_plan(Access::WorktreeWrite, None, false);
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture prompt".into());
    let token = fixture_token();
    let launch = claude::Claude.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        std::path::Path::new("/fixture/repo"),
        Some(&token),
    ))?;
    assert!(
        launch
            .argv
            .windows(2)
            .any(|pair| pair[0] == "--tools" && pair[1] == "default")
    );
    assert!(!launch.argv.iter().any(|arg| arg == "--allowedTools"));
    assert!(
        launch
            .env
            .iter()
            .all(|(key, _)| key != "CLAUDE_CODE_OAUTH_TOKEN")
    );
    assert!(launch.profile.spec.deny_keychain_access);
    assert!(launch.profile.spec.authentication.is_empty());
    for (key, value) in [
        (
            "CARGO_HOME",
            "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/cargo-home",
        ),
        (
            "CARGO_TARGET_DIR",
            "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/target",
        ),
        (
            "CARGO_BUILD_BUILD_DIR",
            "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/target",
        ),
        ("GIT_CONFIG_GLOBAL", "/fixture/scratch/gitconfig"),
    ] {
        assert!(
            launch.env.iter().any(|(k, v)| k == key && v == value),
            "{key}"
        );
    }
    assert!(
        launch
            .env
            .iter()
            .all(|(key, _)| key != "GIT_CONFIG_NOSYSTEM")
    );
    for path in [
        "/fixture/home/.cargo/bin",
        "/fixture/home/.rustup/toolchains",
        "/fixture/home/Library/Caches/provenant-lane-cargo/fixture",
    ] {
        assert!(
            launch
                .profile
                .spec
                .tool_exec_roots
                .iter()
                .any(|g| g.path == Path::new(path)),
            "{path}"
        );
    }
    // Only lane downloads are shared for writes. Host toolchains, trusted
    // sources and Cargo credentials remain protected.
    let shared = Path::new("/fixture/home/Library/Caches/provenant-cargo-downloads");
    assert!(
        launch
            .profile
            .spec
            .tool_writes
            .iter()
            .all(|grant| grant.path == shared)
    );
    // Claude's env scrub drops GIT_CONFIG_GLOBAL from Bash, so scratch git is a
    // wrapper that sets it, and its directory is executable.
    assert!(launch.setup.iter().any(|step| matches!(
        step,
        crate::launch::SetupStep::WriteExecutable { path, .. } if path == Path::new("/fixture/scratch/bin/git")
    )));
    assert!(
        launch
            .profile
            .spec
            .tool_exec_roots
            .iter()
            .any(|g| g.path == Path::new("/fixture/scratch/bin"))
    );
    // Runtime roots are readable; no grant names a credentials file, and the
    // compiler hard-denies Cargo credentials inside them.
    assert!(
        launch
            .profile
            .spec
            .runtime_reads
            .iter()
            .any(|grant| grant.path == Path::new("/fixture/home/.cargo"))
    );
    assert!(
        launch
            .profile
            .spec
            .runtime_reads
            .iter()
            .all(|grant| !grant.path.to_string_lossy().contains("credentials"))
    );
    assert!(launch.profile.spec.state_writes.iter().all(|grant| {
        !grant.path.starts_with("/fixture/home/.cargo")
            && !grant.path.starts_with("/fixture/home/.rustup")
    }));
    // Only lane grants are writable: never the private config or the user's home.
    assert!(launch.profile.spec.state_writes.iter().all(|grant| {
        !grant.path.starts_with("/fixture/state/claude-config")
            && !grant.path.starts_with("/fixture/home")
    }));
    assert!(
        launch
            .env
            .iter()
            .any(|(key, value)| key == "CLAUDE_CONFIG_DIR"
                && value == "/fixture/state/claude-config")
    );
    let secret = launch.secret_input.as_ref().expect("token pipe");
    assert_eq!(
        secret.descriptor_env,
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR"
    );
    assert_eq!(secret.bytes, FIXTURE_TOKEN.as_bytes());
    Ok(())
}

#[test]
fn claude_account_probe_carries_user_for_keychain_selection() {
    let probe_root = tempfile::tempdir().unwrap();
    let script = KeychainScript {
        items: Default::default(),
        statuses: std::sync::Mutex::new(
            vec![br#"{"loggedIn":true,"email":"fixture@example.test"}"#.to_vec()].into(),
        ),
        calls: std::sync::Mutex::new(vec![]),
    };
    let key = crate::accounts::detect(
        &script,
        "claude",
        Path::new("/opt/homebrew/bin/claude"),
        Path::new("/home/u"),
        OsStr::new("u"),
        Path::new("/home/u/.claude-pro"),
        probe_root.path(),
    );
    assert!(key.is_ok());
    let calls = script.calls.into_inner().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1.iter().any(|entry| entry == "USER=u"));
}

#[test]
fn claude_account_probe_names_the_missing_login() {
    let probe_root = tempfile::tempdir().unwrap();
    let script = KeychainScript {
        items: Default::default(),
        statuses: std::sync::Mutex::new(
            vec![br#"{"loggedIn":false,"authMethod":"none"}"#.to_vec()].into(),
        ),
        calls: std::sync::Mutex::new(vec![]),
    };
    let error = crate::accounts::detect(
        &script,
        "claude",
        Path::new("/opt/homebrew/bin/claude"),
        Path::new("/home/u"),
        OsStr::new("u"),
        Path::new("/home/u/.claude-pro"),
        probe_root.path(),
    )
    .unwrap_err();
    assert!(matches!(error, ProviderError::LoginRequired));
    assert!(error.to_string().contains("unlock the login keychain"));
}

#[test]
fn codex_rollout_skips_long_unparsable_and_unrelated_lines() {
    let home = std::env::temp_dir().join(format!("provenant-rollout-{}", std::process::id()));
    let day = home.join("sessions/2026/10/24");
    std::fs::create_dir_all(&day).unwrap();
    let thread = "01234567-89ab-cdef-0123-456789abcdef";
    let long = format!(
        "{{\"type\":\"response_item\",\"payload\":{{\"output\":\"{}\"}}}}\n",
        "x".repeat(3 * 1024 * 1024)
    );
    let rollout = format!(
        "{}\n{long}not json\n{{\"type\":\"event_msg\",\"payload\":{{\"model\":7}}}}\n{}\n",
        serde_json::json!({"type":"session_meta","payload":{"id":thread}}),
        serde_json::json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}),
    );
    std::fs::write(
        day.join(format!("rollout-2026-10-24T00-00-00-{thread}.jsonl")),
        rollout,
    )
    .unwrap();
    let observed = crate::codex_rollout::observe(&home, thread);
    std::fs::remove_dir_all(&home).unwrap();
    let turn = observed.unwrap();
    assert_eq!(turn.model, "gpt-6-luna");
    assert_eq!(turn.effort.as_deref(), Some("low"));
}

/// A real Codex home holds more rollouts than the full-scan bound; a UUIDv7
/// thread is found from its creation date, filed under the local date.
#[test]
fn codex_rollout_finds_a_v7_thread_by_date_in_a_large_home() {
    let home = std::env::temp_dir().join(format!("provenant-rollout-v7-{}", std::process::id()));
    for year in 0..4100 {
        std::fs::create_dir_all(home.join(format!("sessions/{:04}", 3000 + year))).unwrap();
    }
    // Created 2026-11-13T06:45Z and filed under the next day, the window's edge.
    let thread = "01a1d3a7-96ef-7032-a58f-5d1770a43b6e";
    let day = home.join("sessions/2026/11/14");
    std::fs::create_dir_all(&day).unwrap();
    let rollout = format!(
        "{}\n{}\n",
        serde_json::json!({"type":"session_meta","payload":{"id":thread}}),
        serde_json::json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}),
    );
    std::fs::write(
        day.join(format!("rollout-2026-11-14T00-00-00-{thread}.jsonl")),
        rollout,
    )
    .unwrap();
    let observed = crate::codex_rollout::observe(&home, thread);
    let missing = crate::codex_rollout::observe(&home, "01a1d3a7-96ef-7032-a58f-000000000000");
    std::fs::remove_dir_all(&home).unwrap();
    assert_eq!(observed.unwrap().model, "gpt-6-luna");
    assert!(matches!(missing, Err(ProviderError::UnknownIdentity)));
}

#[test]
fn rust_tools_exec_repository_scripts_in_the_working_directory() {
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let plan = read_only_plan("/fixture", Some("login"));
    let prompt = Prompt::new("fixture".into());
    let spec = crate::codex::Codex
        .prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            None,
        ))
        .unwrap();
    let exec = &spec.profile.spec.tool_exec_roots;
    assert!(exec.iter().any(|g| g.path == Path::new("/fixture/repo")
        && g.scope == provenant_confinement::permissions::PathScope::Subtree));
    // Exec does not widen reads: the worktree was already readable.
    assert!(!exec.iter().any(|g| g.path == Path::new("/fixture/home")));
}

/// `/usr/bin/python3` is a shim that runs the Command Line Tools' python3, a
/// symlink into its Python framework, which lies outside `usr/bin`.
#[test]
fn shell_lanes_exec_the_command_line_tools_python() {
    let framework =
        Path::new("/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework");
    if !cfg!(target_os = "macos") || !framework.is_dir() {
        return;
    }
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("login"), false);
        let spec = crate::codex::Codex
            .prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                None,
            ))
            .unwrap();
        let profile = &spec.profile.spec;
        let subtree = |grants: &[provenant_confinement::permissions::PathGrant]| {
            grants.iter().any(|grant| {
                grant.path == framework
                    && grant.scope == provenant_confinement::permissions::PathScope::Subtree
            })
        };
        assert!(subtree(&profile.tool_exec_roots), "{access:?}");
        assert!(
            !profile
                .tool_writes
                .iter()
                .any(|g| g.path.starts_with(framework))
        );
    }
}

#[test]
fn lane_git_exec_path_grants_follow_the_reported_symlink() -> Result<(), Box<dyn std::error::Error>>
{
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    // The host resolves Homebrew's opt/git path into its versioned keg.
    let keg = temp.path().join("Cellar/git/1.0");
    let developer = keg.join("Contents/Developer");
    let git = temp.path().join("opt/git/bin/git");
    let linked_core = temp
        .path()
        .join("opt/git/Contents/Developer/usr/libexec/git-core");
    let core = developer.join("usr/libexec/git-core");
    let developer_git = developer.join("usr/bin/git");
    std::fs::create_dir_all(&core)?;
    std::fs::create_dir_all(developer_git.parent().unwrap())?;
    std::fs::write(&developer_git, "#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(&developer_git, std::fs::Permissions::from_mode(0o700))?;
    let homebrew_link = temp.path().join("opt/git");
    std::fs::create_dir_all(homebrew_link.parent().unwrap())?;
    std::os::unix::fs::symlink(&keg, &homebrew_link)?;
    let host_git = keg.join("bin/git");
    std::fs::create_dir_all(host_git.parent().unwrap())?;
    std::fs::write(
        &host_git,
        format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", linked_core.display()),
    )?;
    std::fs::set_permissions(&host_git, std::fs::Permissions::from_mode(0o700))?;
    let canonical_developer = std::fs::canonicalize(&developer)?;
    let canonical_core = std::fs::canonicalize(&core)?;
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("login"), false);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.provenant.git = &git;
        let launch = crate::codex::Codex.prepare(input)?;
        let profile = &launch.profile.spec;
        assert!(
            profile
                .tool_exec_roots
                .iter()
                .any(|grant| grant.path == canonical_core
                    && grant.scope == provenant_confinement::permissions::PathScope::Subtree)
        );
        assert!(
            profile
                .tool_exec_roots
                .iter()
                .any(|grant| grant.path == canonical_developer.join("usr/bin")
                    && grant.scope == provenant_confinement::permissions::PathScope::Subtree)
        );
        assert!(
            profile
                .runtime_reads
                .iter()
                .any(|grant| grant.path == canonical_developer.join("usr/bin")
                    && grant.scope == provenant_confinement::permissions::PathScope::Subtree)
        );
        assert!(profile.process_executables.iter().any(|grant| {
            grant.path == std::fs::canonicalize(&developer_git).unwrap()
                && grant.scope == provenant_confinement::permissions::PathScope::Exact
        }));
        assert!(
            !profile
                .tool_writes
                .iter()
                .any(|grant| grant.path.starts_with(&canonical_developer))
        );
        assert!(
            profile
                .runtime_reads
                .iter()
                .any(|grant| grant.path == canonical_core
                    && grant.scope == provenant_confinement::permissions::PathScope::Subtree)
        );
        let wrapper = launch
            .setup
            .iter()
            .find_map(|step| match step {
                SetupStep::WriteExecutable { path, bytes }
                    if path.file_name().is_some_and(|name| name == "git") =>
                {
                    Some(String::from_utf8(bytes.clone()).unwrap())
                }
                _ => None,
            })
            .expect("prepared Git wrapper");
        assert!(wrapper.contains(&format!(
            "GIT_EXEC_PATH='{}'",
            std::fs::canonicalize(&core)?.display()
        )));
    }
    Ok(())
}

#[test]
fn lanes_read_git_global_ignore_and_attributes_only_when_present()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::PathScope;
    let temp = tempfile::tempdir()?;
    let home = temp.path();
    let git = home.join(".config/git");
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let granted = |access: Access| -> Result<Vec<std::path::PathBuf>, ProviderError> {
        let plan = fixture_access_plan(access, Some("login"), false);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.home = home;
        let launch = crate::codex::Codex.prepare(input)?;
        Ok(launch
            .profile
            .spec
            .runtime_reads
            .iter()
            .filter(|grant| grant.path.starts_with(&git))
            .inspect(|grant| assert_eq!(grant.scope, PathScope::Exact))
            .map(|grant| grant.path.clone())
            .collect())
    };
    // Absent files are not granted, so a missing path cannot break a launch.
    assert!(granted(Access::WorktreeWrite)?.is_empty());
    std::fs::create_dir_all(&git)?;
    std::fs::write(git.join("ignore"), "")?;
    std::fs::write(git.join("attributes"), "")?;
    std::fs::write(git.join("config"), "")?;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let mut paths = granted(access)?;
        paths.sort();
        let mut expected = vec![git.join("attributes"), git.join("ignore")];
        if access == Access::WorktreeWrite {
            expected.push(git.join("config"));
            expected.sort();
        }
        assert_eq!(paths, expected);
    }
    Ok(())
}

#[test]
fn homebrew_node_tools_grant_the_prefix_but_not_var() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let prefix = temp.path();
    for name in ["bin", "Cellar", "etc", "var"] {
        std::fs::create_dir(prefix.join(name))?;
    }
    let mut requirements = crate::launch::ToolRequirements::default();
    let mut env = vec![(
        std::ffi::OsString::from("PATH"),
        std::ffi::OsString::from("/usr/bin:/bin"),
    )];
    crate::launch::homebrew_tools(prefix, &mut requirements, &mut env);
    let paths = |grants: &[provenant_confinement::permissions::PathGrant]| {
        grants
            .iter()
            .map(|grant| grant.path.clone())
            .collect::<Vec<_>>()
    };
    // etc/ holds OpenSSL's configuration; var/ holds service data.
    assert_eq!(
        paths(&requirements.reads),
        ["bin", "Cellar", "etc"].map(|name| prefix.join(name))
    );
    assert_eq!(
        paths(&requirements.exec),
        ["bin", "Cellar"].map(|name| prefix.join(name))
    );
    assert_eq!(
        env[0].1,
        std::ffi::OsString::from(format!("/usr/bin:/bin:{}", prefix.join("bin").display()))
    );
    Ok(())
}

#[test]
fn resume_argv_carries_the_recorded_session() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let account_dir = std::path::Path::new("/fixture/identities/opaque");
    for (id, session, flag) in [
        ("claude", "5e55-claude-session", "--resume"),
        ("codex", "5e55-codex-thread", "resume"),
    ] {
        let adapter = REGISTRY
            .iter()
            .find(|entry| entry.adapter.id() == id)
            .expect("provider")
            .adapter;
        let route = route(id, "fixture-model");
        let continuation = Continuation::retained(id, session, &route, "fixture")?;
        let account = identity::RegisteredAccount::new(id, "opaque-account", account_dir.into())?;
        let token =
            (id == "claude").then(|| fixture_token().for_account("opaque-account", account_dir));
        let plan = fixture_access_plan(Access::ReadOnly, (id == "codex").then_some("login"), true);
        let mut input = fixture_input(
            &route,
            &plan,
            &prompt,
            std::path::Path::new("/fixture/repo"),
            token.as_ref(),
        );
        input.account = Some(&account);
        input.continuation = Some(&continuation);
        let launch = adapter.prepare(input)?;
        let argv: Vec<_> = launch
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect();
        let at = argv.iter().position(|arg| arg == flag).expect(flag);
        assert_eq!(argv[at + 1], session, "{id}");
    }
    Ok(())
}

#[test]
fn shared_provider_package_prefix_falls_back_to_exact_executable() -> Result<(), ProviderError> {
    use provenant_confinement::permissions::PathScope;
    let prefixes = ["/usr/local", "/opt/homebrew"];
    let token = fixture_token();
    let prompt = Prompt::new("fixture".into());
    for adapter in ["claude", "codex", "cursor", "opencode"] {
        let auth = match adapter {
            "codex" | "cursor" | "opencode" => Some("login"),
            _ => None,
        };
        let plan = fixture_access_plan(Access::ReadOnly, auth, false);
        let mut route = route(adapter, "fixture-model");
        route.account_key = None;
        let provider = REGISTRY
            .iter()
            .find(|entry| entry.adapter.id() == adapter)
            .unwrap()
            .adapter;
        for prefix in prefixes {
            let executable = Path::new(prefix).join("bin/provider");
            let shared_var = Path::new(prefix).join("var");
            let mut input = fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                Some(&token),
            );
            input.executable = &executable;
            let spec = provider.prepare(input)?;
            assert!(
                spec.profile
                    .spec
                    .runtime_reads
                    .iter()
                    .any(|grant| grant.path == executable && grant.scope == PathScope::Exact),
                "{adapter}: {prefix}"
            );
            assert!(
                spec.profile.spec.runtime_reads.iter().all(|grant| {
                    grant.path != Path::new(prefix)
                        && !grant.path.starts_with(&shared_var)
                        && !shared_var.starts_with(&grant.path)
                }),
                "{adapter}: {prefix}"
            );
            assert!(
                spec.profile.spec.tool_exec_roots.iter().all(|grant| {
                    grant.path != Path::new(prefix)
                        && !grant.path.starts_with(&shared_var)
                        && !shared_var.starts_with(&grant.path)
                }),
                "{adapter}: {prefix}"
            );
            assert!(
                provenant_confinement::write_grants(plan.permissions(), &spec.profile)
                    .iter()
                    .all(|grant| {
                        !grant.path.starts_with(&shared_var)
                            && !shared_var.starts_with(&grant.path)
                            && !Path::new(prefix)
                                .join("bin/provider")
                                .starts_with(&grant.path)
                    }),
                "{adapter}: {prefix}"
            );
            assert!(
                spec.omitted_discovery.iter().any(|omitted| {
                    omitted.path == Path::new(prefix)
                        && omitted.source == crate::OmissionSource::ProviderPackage
                }),
                "{adapter}: {prefix}"
            );
            let receipt = spec.applied_access().receipt(Path::new("/fixture/home"));
            assert!(
                receipt["omitted_grants"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["reason"] == "shared_package_prefix"),
                "{adapter}: {prefix}"
            );
        }
    }
    Ok(())
}

#[test]
fn package_root_never_grants_a_hidden_home_tool_tree() {
    use crate::launch::package_root;
    let direct = Path::new("/home/u/.cargo/bin/codex");
    assert_eq!(
        package_root(direct).unwrap(),
        Path::new("/home/u/.cargo/bin")
    );
    let versioned = Path::new("/home/u/.local/share/claude/versions/2.1.310");
    assert_eq!(
        package_root(versioned).unwrap(),
        Path::new("/home/u/.local/share/claude/versions")
    );
    let cask = Path::new("/opt/homebrew/Caskroom/codex/0.159.0/bin/codex");
    assert_eq!(
        package_root(cask).unwrap(),
        Path::new("/opt/homebrew/Caskroom/codex/0.159.0")
    );
}

#[test]
fn registration_org_fingerprint_pins_on_first_available_probe() {
    let mut entry = accounts::Registration {
        adapter: "claude".into(),
        label: "pro".into(),
        key: "k".into(),
        org_fingerprint: None,
        directory: "/home/u/.claude-pro".into(),
        plan: None,
        priority: None,
        billing_anchor: None,
        plans: Vec::new(),
        directories: Vec::new(),
        limits: None,
        display_name: None,
        hidden: false,
        order: None,
        headline: None,
        settings: Default::default(),
    };
    assert_eq!(entry.check_org(Some("a")).unwrap().as_deref(), Some("a"));
    assert_eq!(entry.check_org(None).unwrap(), None);
    entry.org_fingerprint = Some("a".into());
    assert_eq!(entry.check_org(Some("a")).unwrap(), None);
    assert_eq!(entry.check_org(None).unwrap(), None);
    assert!(entry.check_org(Some("b")).is_err());
}
#[test]
fn codex_ignores_web_search_items_with_repeated_members() -> Result<(), ProviderError> {
    let mut stream = codex::Codex.stream(None)?;
    let raw = b"{\"type\":\"item.started\",\"item\":{\"id\":\"item_1\",\"type\":\"web_search\",\"id\":\"exec-1\",\"query\":\"\"}}\n{\"type\":\"item.completed\",\"item\":{\"id\":\"item_1\",\"type\":\"web_search\",\"id\":\"exec-1\",\"query\":\"q\"}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n";
    codex::Codex.observe(&mut stream, raw)?;
    assert_eq!(
        codex::Codex.finish(stream, EXIT)?.status(),
        OutcomeStatus::Success
    );
    Ok(())
}

#[test]
fn account_display_fields_validate() {
    use provenant_config::DomainConfig;
    let base = "[entries.claude-work]\nadapter = \"claude\"\nlabel = \"work\"\nkey = \"k\"\ndirectory = \"/home/u/.claude-work\"\n";
    let parse = |extra: &str| -> accounts::Settings {
        toml_edit::de::from_str(&format!("{base}{extra}")).unwrap()
    };
    assert!(
        parse("display_name = \"Work\"\nhidden = true\norder = 999\nheadline = \"5h\"\n")
            .validate()
            .is_empty()
    );
    for extra in [
        "display_name = \"\"\n",
        "display_name = \" pad\"\n",
        "display_name = \"a\\u0007b\"\n",
        "order = 0\n",
        "order = 1000\n",
    ] {
        let errors = parse(extra).validate();
        assert_eq!(errors.len(), 1, "{extra}");
        assert_eq!(errors[0].code, "display", "{extra}");
    }
    assert!(
        parse(&format!("display_name = \"{}\"\n", "x".repeat(65)))
            .validate()
            .len()
            == 1
    );
    assert!(
        toml_edit::de::from_str::<accounts::Settings>(&format!("{base}headline = \"daily\"\n"))
            .is_err()
    );
}

#[test]
fn shared_settings_refuse_identity_credential_and_hook_keys() {
    let mut settings = accounts::Settings::default();
    assert!(settings.settings_error().is_none());
    settings.shared_settings = Some(accounts::SharedSettings {
        source: None,
        keys: Some(vec!["outputStyle".into(), "includeCoAuthoredBy".into()]),
    });
    assert!(settings.settings_error().is_none());
    for key in [
        "apiKeyHelper",
        "forceLoginMethod",
        "oauthAccount",
        "hooks",
        "authtoken",
        "AUTHTOKEN",
        "refreshtoken",
        "token2",
        "oauth2",
    ] {
        settings.shared_settings = Some(accounts::SharedSettings {
            source: None,
            keys: Some(vec!["outputStyle".into(), key.into()]),
        });
        assert!(settings.settings_error().is_some(), "{key}");
    }
    settings.shared_settings = Some(accounts::SharedSettings {
        source: None,
        keys: Some(vec!["refreshtoken".into()]),
    });
    let (message, code) = settings.settings_error().unwrap();
    assert_eq!(code, Some("SHARED_SETTINGS_NEVER_SYNCED"));
    assert!(
        message.contains("`refreshtoken`") && message.contains("`token`"),
        "{message}"
    );
    let errors = provenant_config::DomainConfig::validate(&settings);
    let error = errors
        .iter()
        .find(|error| error.field == "shared_settings")
        .unwrap();
    assert_eq!(error.code, "SHARED_SETTINGS_NEVER_SYNCED");
    assert_eq!(error.detail, Some(message));
    assert!(error.to_string().contains("is never synced"));
    settings.shared_settings = None;
    let mut entry = accounts::Registration {
        adapter: "claude".into(),
        label: "work".into(),
        key: "k".into(),
        org_fingerprint: None,
        directory: "/home/u/.claude-work".into(),
        plan: None,
        priority: None,
        billing_anchor: None,
        plans: Vec::new(),
        directories: Vec::new(),
        limits: None,
        display_name: None,
        hidden: false,
        order: None,
        headline: None,
        settings: Default::default(),
    };
    entry.settings.insert("outputStyle".into(), "x".into());
    entry.settings.insert("model".into(), "y".into());
    settings.entries.insert("claude-work".into(), entry);
    // `model` is not on the allow-list.
    assert!(settings.settings_error().is_some());
    settings
        .entries
        .get_mut("claude-work")
        .unwrap()
        .settings
        .remove("model");
    assert!(settings.settings_error().is_none());
    settings
        .entries
        .get_mut("claude-work")
        .unwrap()
        .settings
        .insert(
            "env".into(),
            serde_json::json!({"ANTHROPIC_AUTH_TOKEN": "x"}),
        );
    assert!(settings.settings_error().is_some());
}

fn accounts_with_plans(plans: &str) -> Result<accounts::Settings, toml_edit::de::Error> {
    toml_edit::de::from_str(&format!(
        "[entries.claude-work]\nadapter = \"claude\"\nlabel = \"work\"\nkey = \"k1\"\ndirectory = \"/home/u/.claude-work\"\n{plans}"
    ))
}
#[test]
fn account_plan_prices_parse_validate_and_reject_unknown_fields() {
    use provenant_config::DomainConfig;
    let valid = accounts_with_plans(
        "[[entries.claude-work.plans]]\nname = \"max_5x\"\nprice = 100.0\ncurrency = \"USD\"\neffective_from = \"2026-01-01\"\n[[entries.claude-work.plans]]\nname = \"max_20x\"\nprice = 300\ncurrency = \"AUD\"\neffective_from = \"2026-06-01\"\n",
    )
    .unwrap();
    assert_eq!(valid.entries["claude-work"].plans.len(), 2);
    assert!(valid.validate().is_empty());
    // Unknown keys are refused at parse time.
    assert!(
        accounts_with_plans(
            "[[entries.claude-work.plans]]\nname = \"a\"\nprice = 1\ncurrency = \"USD\"\neffective_from = \"2026-01-01\"\nnote = \"x\"\n"
        )
        .is_err()
    );
    for bad in [
        "price = -1\ncurrency = \"USD\"\neffective_from = \"2026-01-01\"",
        "price = 1\ncurrency = \"usd\"\neffective_from = \"2026-01-01\"",
        "price = 1\ncurrency = \"USD\"\neffective_from = \"2026-02-30\"",
        "price = 1\ncurrency = \"USD\"\neffective_from = \"soon\"",
    ] {
        let parsed = accounts_with_plans(&format!(
            "[[entries.claude-work.plans]]\nname = \"a\"\n{bad}\n"
        ))
        .unwrap();
        assert_eq!(parsed.validate()[0].code, "plans", "{bad}");
    }
    let duplicate = accounts_with_plans(
        "[[entries.claude-work.plans]]\nname = \"a\"\nprice = 1\ncurrency = \"USD\"\neffective_from = \"2026-01-01\"\n[[entries.claude-work.plans]]\nname = \"b\"\nprice = 2\ncurrency = \"USD\"\neffective_from = \"2026-01-01\"\n",
    )
    .unwrap();
    assert_eq!(duplicate.validate()[0].code, "plans");
}

#[test]
fn free_plan_notional_value_and_extra_directories_parse_and_validate() {
    use provenant_config::DomainConfig;
    let ok = accounts_with_plans(
        "directories = [\"/home/u/.claude\"]\n[[entries.claude-work.plans]]\nname = \"student\"\nprice = 0\nnotional_usd = 20\neffective_from = \"2026-01-01\"\n",
    )
    .unwrap();
    let entry = &ok.entries["claude-work"];
    assert_eq!(entry.plans[0].currency, "USD");
    assert_eq!(entry.plans[0].notional_usd, Some(20.0));
    assert_eq!(entry.directories.len(), 1);
    assert!(ok.validate().is_empty());
    let relative = accounts_with_plans("directories = [\"rel/dir\"]\n").unwrap();
    assert!(!relative.validate().is_empty());
}

#[test]
fn adapter_sign_in_stderr_has_a_typed_auth_cause() {
    let fixtures: &[(&dyn Adapter, &[u8])] = &[
        (
            &cursor::Cursor,
            include_bytes!("../../../tests/fixtures/providers/cursor/auth-required.stderr"),
        ),
        (
            &codex::Codex,
            include_bytes!("../../../tests/fixtures/providers/codex/auth-required.stderr"),
        ),
        (
            &opencode::OpenCode,
            include_bytes!("../../../tests/fixtures/providers/opencode/auth-required.stderr"),
        ),
        (
            &agy::Agy,
            include_bytes!("../../../tests/fixtures/providers/agy/auth-required.stderr"),
        ),
    ];
    for (adapter, stderr) in fixtures {
        assert_eq!(
            adapter.failure_cause(stderr),
            FailureCause::AuthRequired,
            "{}",
            adapter.id()
        );
        for unrelated in [
            "connection closed",
            "tool request failed: Unauthorized",
            "application request unauthenticated",
            "tool request failed: invalid API key",
            "tool documentation mentions opencode auth login",
        ] {
            assert_eq!(
                adapter.failure_cause(unrelated.as_bytes()),
                FailureCause::Provider,
                "{}: {unrelated}",
                adapter.id()
            );
        }
    }
}
