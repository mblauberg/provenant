#[test]
// Spec: docs/specs/agent-native.md#peer-projection
fn unlocatable_peer_project_is_a_typed_operation_error_with_setting_and_next()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{CancelToken, Clock, RootInputs, WaitBudget};
    use provenant_fabric_runtime::{
        config::Peer,
        hosts::{PeerAction, mirror::ProjectionMode},
    };
    use provenant_records::AbsolutePath;

    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(&project)?;
    let mut inputs = RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(root.path().join("state"));
    let mut context = super::Context::open(
        inputs.resolve(&provenant_os::fs::SystemPathResolver)?,
        &home,
        &project,
    )?;
    let host = context.identity.host;
    context.config.peers.insert(
        "worker".into(),
        Peer {
            host,
            destination: "worker".into(),
            remote_executable: None,
            connect_timeout_seconds: 10,
        },
    );
    // `locate` never mirrors, so a project outside the peer's home is an
    // operation error that names the setting and a way out, before any
    // transport is opened.
    context.projection.value = ProjectionMode::Locate;
    let outside = root.path().join("scratch").join("project");
    context.identity.common_dir = AbsolutePath::new(&outside)?;
    let clock = provenant_os::clock::SystemClock;
    let budget = WaitBudget::cli(clock.mono_now(), 30)?;
    let failure = context
        .peer(
            &clock,
            host,
            PeerAction::Health {
                extra: Default::default(),
            },
            budget,
            &CancelToken::new(),
        )
        .expect_err("a project outside home must fail before peer transport");
    match &*failure.error {
        super::FacadeError::RunKind {
            run_code,
            next,
            detail,
            ..
        } => {
            assert_eq!(run_code.as_str(), super::PEER_PROJECT_UNLOCATABLE);
            assert_eq!(next.as_str(), "run without --host");
            let detail = detail.as_ref().expect("operation error names the setting");
            assert!(detail.contains(super::PROJECTION_KEY), "{detail}");
            assert!(detail.contains("locate"), "{detail}");
        }
        other => panic!("expected an unlocatable operation error, got {other:?}"),
    }
    let (outcome, kind) = super::facade_outcome(&failure.error);
    assert_eq!(outcome, provenant_records::Outcome::Error);
    assert_eq!(kind.code(), super::PEER_PROJECT_UNLOCATABLE);
    Ok(())
}

#[test]
// Spec: docs/specs/agent-native.md#peer-projection
fn projection_setting_uses_the_resolved_layer_and_origin() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_fabric_runtime::hosts::mirror::ProjectionMode;
    use provenant_records::{Layer, Source, Sourced};

    let mut knobs = std::collections::BTreeMap::new();
    knobs.insert(
        super::PROJECTION_KEY.to_owned(),
        Sourced {
            value: serde_json::json!("mirror"),
            source: Source {
                layer: Layer::Project,
                origin: Some(".provenant/config.toml:12".to_owned()),
            },
        },
    );
    let sourced = super::projection_from_knobs(&knobs);
    assert_eq!(sourced.value, ProjectionMode::Mirror);
    assert_eq!(sourced.source.layer, Layer::Project);
    assert_eq!(
        sourced.source.origin.as_deref(),
        Some(".provenant/config.toml:12")
    );

    let empty = std::collections::BTreeMap::new();
    let default = super::projection_from_knobs(&empty);
    assert_eq!(default.value, ProjectionMode::Auto);
    assert_eq!(default.source.layer, Layer::Default);
    Ok(())
}

#[test]
// Spec: docs/specs/cli.md#every-name-refusal-lists-close-matches
fn a_one_edit_peer_label_typo_ranks_the_configured_label_first() {
    let close = super::close_host_names("worksttion", ["workstation", "laptop"]);
    assert_eq!(close.first().map(String::as_str), Some("workstation"));
    assert!(super::close_host_names("zzzzzz", ["workstation"]).is_empty());
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "An isolated child process supplies the harness markers to project context."
)]
// Spec: docs/specs/fabric.md#identity-and-store
fn project_context_uses_the_active_harness_actor() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;

    const FIXTURE: &str = "PROVENANT_TEST_PROJECT_CONTEXT_IDENTITY";
    if let Some(root) = std::env::var_os(FIXTURE) {
        use provenant_core::RootInputs;
        use std::fs;

        let root = PathBuf::from(root);
        let home = root.join("home");
        let project = root.join("project");
        let state = root.join("state");
        let config = root.join("config");
        for directory in [&home, &project, &state, &config] {
            fs::create_dir_all(directory)?;
        }
        fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        fs::write(
            state.join("config.toml"),
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nclient_label='terminal'\n",
        )?;
        let mut inputs = RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(state);
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let location = provenant_config::ProjectLocation::new(fs::canonicalize(&project)?)?;
        let binding = provenant_config::bind_project(&roots, location)?;
        provenant_config::bind_host(&roots)?;

        let context = super::Context::open_for_project(roots, &home, &binding)?;
        let expected = context.inbox_address(None)?;
        assert_eq!(context.identity.inbox.actor, expected.actor);
        let expected_label = std::env::var("PROVENANT_TEST_EXPECTED_LABEL")?;
        assert_eq!(context.identity.inbox.agent, expected_label);
        let chair = super::current_chair(&context)?;
        assert_eq!(chair.actor, context.identity.inbox.actor);
        assert_eq!(chair.host, context.identity.host);
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    for (client, label) in [
        ("codex", "codex"),
        ("claude", "claude"),
        ("labelled", "reviewer"),
    ] {
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "fabric::context::tests::project_context_uses_the_active_harness_actor",
                "--nocapture",
            ])
            .env(FIXTURE, root.join(client))
            .env("HOME", root.join(client).join("home"))
            .env("XDG_CONFIG_HOME", root.join(client).join("config"))
            .env("XDG_CACHE_HOME", root.join(client).join("cache"))
            .env("PROVENANT_ROOT", root.join(client).join("project"))
            .env("PROVENANT_STATE_DIR", root.join(client).join("state"))
            .env("PROVENANT_TEST_EXPECTED_LABEL", label)
            .env_remove("PROVENANT_AGENT_LABEL")
            .env_remove("PROVENANT_SESSION_ID")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_SESSION_ID");
        if client == "codex" {
            command.env("CODEX_THREAD_ID", "fixture");
        } else {
            command.env("CLAUDECODE", "1");
            if client == "labelled" {
                command.env("PROVENANT_AGENT_LABEL", "reviewer");
            }
        }
        let output = command.output()?;
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[test]
fn registered_project_root_uses_external_common_hint_and_keeps_bare_projects_host_only()
-> Result<(), Box<dyn std::error::Error>> {
    use super::{registered_project_roots_for_repository, registered_project_roots_from_binding};
    use provenant_core::{CancelToken, RootInputs};
    use std::path::Path;
    use std::time::Duration;

    fn git(cwd: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
        let output = provenant_os::fixture::git_command()
            .current_dir(cwd)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }
        Ok(())
    }

    // Spec: docs/specs/run-git.md#project-layout-ownership
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let assets = base.join("assets");
    let state = base.join("state");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&assets)?;
    std::fs::create_dir_all(&state)?;
    std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let mut inputs = RootInputs::new(home, assets);
    inputs.state_dir = Some(state);
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;

    let checkout = base.join("project");
    let common = base.join("metadata").join("common-store");
    std::fs::create_dir_all(&checkout)?;
    std::fs::create_dir_all(common.parent().ok_or("metadata parent")?)?;
    git(
        &base,
        &[
            "init",
            "-q",
            "--separate-git-dir",
            common.to_str().ok_or("common path")?,
            checkout.to_str().ok_or("checkout path")?,
        ],
    )?;
    let location =
        provenant_config::ProjectLocation::from_verified_primary_checkout(&common, &checkout)?;
    let binding = provenant_config::bind_project(&roots, location)?;
    assert_eq!(binding.primary_checkout_hint(), Some(checkout.as_path()));
    let git_program = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    std::fs::create_dir(base.join("utility-home"))?;
    std::fs::set_permissions(
        base.join("utility-home"),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    let utility = provenant_confinement::UtilityRuntime::new(
        &git_program,
        base.join("utility-home"),
        roots.state.as_path(),
    )?;
    let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
    let primary_repo = utility.open_repo(&executor, &checkout, budget, &CancelToken::new())?;
    let (root, project) = registered_project_roots_for_repository(
        &binding,
        &primary_repo,
        &executor,
        &utility,
        &CancelToken::new(),
    )?;
    assert_eq!(root, checkout);
    assert_eq!(project.as_deref(), Some(checkout.as_path()));

    // A hint is evidence only while Git re-proves the checkout/common pair.
    std::fs::remove_dir_all(&checkout)?;
    assert!(
        provenant_confinement::worktrees::project_layout_for_binding(
            &executor,
            &binding,
            &git_program,
            &provenant_confinement::worktrees::project_layout_git_environment(),
            Duration::from_secs(5),
            &CancelToken::new(),
        )
        .is_err()
    );

    let source = base.join("source");
    std::fs::create_dir(&source)?;
    git(&source, &["init", "-q", "--initial-branch=main"])?;
    git(&source, &["config", "user.name", "Fixture"])?;
    git(
        &source,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    std::fs::write(source.join("README.md"), "bare owner fixture\n")?;
    git(&source, &["add", "README.md"])?;
    git(&source, &["commit", "-qm", "initial"])?;
    let bare = base.join("metadata").join("bare-store");
    git(
        &base,
        &[
            "clone",
            "--bare",
            "-q",
            source.to_str().ok_or("source path")?,
            bare.to_str().ok_or("bare path")?,
        ],
    )?;
    let linked = base.join("linked");
    git(
        &base,
        &[
            "--git-dir",
            bare.to_str().ok_or("bare path")?,
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().ok_or("linked path")?,
            "HEAD",
        ],
    )?;
    let bare_binding =
        provenant_config::bind_project(&roots, provenant_config::ProjectLocation::new(&bare)?)?;
    assert_eq!(bare_binding.primary_checkout_hint(), None);
    let (bare_root, bare_project) = registered_project_roots_from_binding(
        &bare_binding,
        &executor,
        &utility,
        &CancelToken::new(),
    )?;
    assert_eq!(bare_root, bare);
    assert_eq!(bare_project, None);
    assert_ne!(bare_root, linked);
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)] // Dedicated fixture subprocess isolates lane-only environment variables.
fn lane_publication_uses_private_scratch() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::RootInputs;
    const FIXTURE: &str = "PROVENANT_TEST_LANE_PUBLICATION";
    if let Some(base) = std::env::var_os(FIXTURE) {
        let base = std::path::PathBuf::from(base);
        let mut inputs = RootInputs::new(base.join("home"), base.join("assets"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let result = provenant_records::CommandResult::new(
            "fixture",
            provenant_records::Outcome::Ok,
            "result",
        );
        let path = super::publish(&result, &roots)?;
        assert!(
            path.to_path_buf()?
                .starts_with(base.join("scratch/results"))
        );
        let bytes = std::fs::read(path.to_path_buf()?)?;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes)?["data"],
            "result"
        );
        assert!(!base.join("state").exists());
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    for name in ["home", "assets", "scratch"] {
        std::fs::create_dir(base.join(name))?;
        std::fs::set_permissions(
            base.join(name),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )?;
    }
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "fabric::context::tests::lane_publication_uses_private_scratch",
            "--nocapture",
        ])
        .env(FIXTURE, &base)
        .env(provenant_core::RUN_STATE_DIRECTORY_ENV, base.join("state"))
        .env("TMPDIR", base.join("scratch"))
        .output()?;
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!base.join("state").exists());
    Ok(())
}

#[test]
fn submit_preflight_refuses_old_or_incapable_stub_before_launch()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::HostId;
    use provenant_fabric_runtime::{
        hosts::{PROTOCOL, PeerHealth, PeerResponse, PeerResult},
        identity::CrossHostProjectKey,
        runs::RunCapabilities,
    };
    use std::{cell::Cell, path::Path};
    let host = HostId::from_bytes([2; 16]);
    let project = CrossHostProjectKey::from_home(Path::new("/home/a"), Path::new("/home/a/repo"))?;
    for (submit, code) in [(false, Some("dispatch_refused")), (true, None)] {
        let calls = Cell::new(0);
        let stub = || {
            calls.set(calls.get() + 1);
            Ok(PeerResponse {
                extra: Default::default(),
                version: 1,
                protocol: PROTOCOL,
                host,
                project: project.clone(),
                result: PeerResult::Health {
                    extra: Default::default(),
                    data: PeerHealth {
                        extra: Default::default(),
                        host,
                        available: true,
                        execution: submit,
                        release: env!("CARGO_PKG_VERSION").into(),
                        run_capabilities: RunCapabilities {
                            submit,
                            list: true,
                            status: true,
                            wait: true,
                            cancel: true,
                            resume: true,
                            output: true,
                            events: true,
                        },
                        readiness: provenant_fabric_runtime::hosts::PeerReadiness::reporting(),
                    },
                },
            })
        };
        let result = super::submit_after_probe("worker", stub, stub);
        if let Some(code) = code {
            let error = result.unwrap_err();
            assert_eq!(error.error.code(), code);
            assert_eq!(calls.get(), 1);
        } else {
            assert!(result.is_ok());
            assert_eq!(calls.get(), 2);
        }
    }
    Ok(())
}

#[test]
fn graph_setting_origins_distinguish_domain_default_from_explicit_same_value()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::Layer;
    let temp = tempfile::tempdir()?;
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("repo"));
    inputs.state_dir = Some(temp.path().join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let default =
        provenant_config::load_with_domains::<&[u8], _>(&roots, None, &crate::domains::SECTIONS)?;
    assert_eq!(super::prompt_limit_source(&default.origins), Layer::Default);
    assert_eq!(
        super::decide_timeout_source(&default.origins),
        Layer::Default
    );
    let configured = b"schema_version = 1
[domains.fabric.graph]
decide_timeout_seconds = 300
[domains.fabric.graph.limits]
node_prompt_bytes = 262144
";
    let resolved = provenant_config::load_with_domains(
        &roots,
        Some(configured.as_slice()),
        &crate::domains::SECTIONS,
    )?;
    assert_eq!(
        resolved.domains.fabric.graph.limits.node_prompt_bytes,
        default.domains.fabric.graph.limits.node_prompt_bytes
    );
    assert_eq!(super::prompt_limit_source(&resolved.origins), Layer::Host);
    assert_eq!(super::decide_timeout_source(&resolved.origins), Layer::Host);
    Ok(())
}
#[test]
fn protocol_mismatch_keeps_both_versions_and_the_cli_code() {
    let failure = super::peer_failure(
        "workshop",
        &super::PeerError::ProtocolMismatch {
            peer: "workshop".into(),
            remote: 1,
            expected: provenant_fabric_runtime::hosts::PROTOCOL,
        },
    );
    assert_eq!(failure.error.code(), "peer_version");
    assert_eq!(
        failure.error.to_string(),
        format!(
            "peer workshop speaks protocol 1, this host expects protocol {}; upgrade both",
            provenant_fabric_runtime::hosts::PROTOCOL
        )
    );
    assert_eq!(
        super::facade_outcome(&failure.error).1.code(),
        "PEER_VERSION"
    );
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "Isolated child processes test chair session resolution across environment variables."
)]
fn host_chair_session_discovery() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;

    const FIXTURE: &str = "PROVENANT_TEST_HOST_CHAIR_SESSION";
    if let Some(mode) = std::env::var_os(FIXTURE) {
        use provenant_core::{Clock, RootInputs};
        use std::fs;

        let root = PathBuf::from(std::env::var("PROVENANT_TEST_ROOT")?);
        let home = root.join("home");
        let project = root.join("project");
        let state = root.join("state");
        let mut inputs = RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(state.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let location = provenant_config::ProjectLocation::new(fs::canonicalize(&project)?)?;
        let binding = provenant_config::bind_project(&roots, location)?;
        provenant_config::bind_host(&roots)?;

        let context = super::Context::open_for_project(roots, &home, &binding)?;

        if mode == "none_and_stored" {
            // 1. None fallback: without PROVENANT_SESSION_ID and without stored session.
            assert_eq!(context.host_chair_session(), None);

            // Now store a chair session for this actor/project.
            let session_id = provenant_workflow::session::HostSessionId::parse("saved-chair-456")?;
            let clock = provenant_os::clock::SystemClock::new();
            let budget = provenant_core::WaitBudget::cli(clock.mono_now(), 5)?;
            provenant_workflow::session::SessionStore::new(
                context.roots.state.clone(),
                context.identity.project,
                session_id,
            )
            .with_chair(context.identity.host, context.identity.inbox.actor)
            .with_workspace(context.project_root.clone())
            .save(
                b"checkpoint",
                budget,
                &provenant_core::CancelToken::new(),
                &clock,
            )?;

            // 2. Stored chair session fallback:
            let hs = context
                .host_chair_session()
                .expect("expected stored chair session");
            assert_eq!(hs.session, "saved-chair-456");
            assert_eq!(
                hs.source,
                format!(
                    "saved chair session for actor {}",
                    context.identity.inbox.actor
                )
            );
        } else if mode == "env_override" {
            // 3. PROVENANT_SESSION_ID override takes precedence over stored session.
            let hs = context
                .host_chair_session()
                .expect("expected session from env");
            assert_eq!(hs.session, "env-override-789");
            assert_eq!(
                hs.source,
                format!(
                    "environment variable {}",
                    provenant_core::consumer_env::SESSION_ID
                )
            );
        }
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let home = root.join("home");
    let project = root.join("project");
    let state = root.join("state");
    let config = root.join("config");
    for directory in [&home, &project, &state, &config] {
        std::fs::create_dir_all(directory)?;
    }
    std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nclient_label='terminal'\n",
    )?;

    // Phase 1: verify None fallback, then save session and verify stored fallback.
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "fabric::context::tests::host_chair_session_discovery",
            "--nocapture",
        ])
        .env(FIXTURE, "none_and_stored")
        .env("PROVENANT_TEST_ROOT", &root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("PROVENANT_ROOT", &project)
        .env("PROVENANT_STATE_DIR", &state)
        .env_remove("PROVENANT_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_SESSION_ID");
    let output = command.output()?;
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Phase 2: verify PROVENANT_SESSION_ID override over the stored session.
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "fabric::context::tests::host_chair_session_discovery",
            "--nocapture",
        ])
        .env(FIXTURE, "env_override")
        .env("PROVENANT_TEST_ROOT", &root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("PROVENANT_ROOT", &project)
        .env("PROVENANT_STATE_DIR", &state)
        .env("PROVENANT_SESSION_ID", "env-override-789")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_SESSION_ID");
    let output = command.output()?;
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}

#[test]
fn typed_run_facade_errors_map_back_to_their_run_kind() {
    use provenant_fabric_runtime::coordination::facade::FacadeError;
    use provenant_records::Outcome;
    let run_kind = FacadeError::RunKind {
        run_code: "DISK_FLOOR".into(),
        next: "run provenant gc --apply".into(),
        detail: None,
        resource: None,
    };
    assert_eq!(run_kind.code(), "DISK_FLOOR");
    assert_eq!(
        super::facade_outcome(&run_kind),
        (Outcome::Refused, provenant_run::ErrorKind::DiskFloor)
    );
    let lease = FacadeError::WriterLeaseHeld {
        holder: None,
        branch: None,
        worktree: None,
        age: None,
        live: None,
    };
    assert_eq!(
        super::facade_outcome(&lease),
        (Outcome::Refused, provenant_run::ErrorKind::WriterLeaseHeld)
    );
}
