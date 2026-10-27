use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, package_root, subtree,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{NetworkEndpoint, ProviderProfile, ProviderProfileSpec};
use serde::Deserialize;
use std::ffi::OsString;
use std::path::PathBuf;
pub struct OpenCode;

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &OpenCode,
    quota_source: Some(&OpenCode),
    display_name: "OpenCode",
    quota: provenant_quota::QuotaPolicy {
        adapter: "opencode",
        timer_seconds: 300,
        panel_seconds: 120,
        probe_seconds: 14,
        low_power: true,
        solo: false,
    },
};

crate::adapter!(OpenCode, "opencode", prepare);
/// The JSON run stream carries no answering-model position.
const MODEL_ON_WIRE: bool = false;

/// OpenCode uses the user's configuration and stored provider accounts.
/// Inline permissions deny edits and shell commands for read-only runs.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    if plan.selected_authentication.is_some() {
        return Err(ProviderError::UnsupportedPolicy);
    }
    let private = &plan
        .private_state
        .as_ref()
        .ok_or(ProviderError::UnsupportedPolicy)?
        .path;
    let mut argv: Vec<OsString> = [
        input.executable.as_os_str(),
        "run".as_ref(),
        "--standalone".as_ref(),
        "--format".as_ref(),
        "json".as_ref(),
        "--log-level".as_ref(),
        "error".as_ref(),
    ]
    .map(OsString::from)
    .to_vec();
    if session == Session::Resume {
        let continuation = input.continuation.ok_or(ProviderError::UnknownIdentity)?;
        argv.extend(["-s".into(), continuation.session().into()]);
    }
    let model = match &input.route.effort {
        Some(effort) => format!("{}#{effort}", input.route.model),
        None => input.route.model.clone(),
    };
    argv.extend(["-m".into(), model.into()]);
    let edit = match access {
        Access::ReadOnly => "deny",
        Access::WorktreeWrite => "allow",
    };
    // Scratch is outside the workspace; headless runs cannot answer the
    // external-directory prompt. Keep the exception inside its existing grant.
    // https://opencode.ai/docs/permissions/#external-directories
    let scratch = format!(
        "{}/**",
        plan.scratch
            .path
            .to_str()
            .ok_or(ProviderError::InvalidPath)?
    );
    let mut permission = serde_json::json!({
        "edit": edit,
        "bash": edit,
        "webfetch": "deny",
    });
    if input.git_context {
        permission["external_directory"] = serde_json::json!({scratch.clone(): "allow"});
        permission["read"] = serde_json::json!({scratch: "allow"});
    }
    let config = serde_json::json!({
        "autoupdate": false,
        "share": "disabled",
        "mcp": {},
        "plugin": [],
        "permission": permission,
    })
    .to_string();
    let package = package_root(input.executable)?;
    // Bypass Apple's Git shim, including when the selected binary is Homebrew's.
    let bin = plan.scratch.path.join("bin");
    let path = std::env::join_paths([
        bin.as_path(),
        std::path::Path::new("/usr/bin"),
        std::path::Path::new("/bin"),
    ])
    .map_err(|_| ProviderError::InvalidPath)?;
    let mut env = base_env(
        input,
        &plan.scratch.path,
        path.to_str().ok_or(ProviderError::InvalidPath)?,
    );
    let xdg = |key, fallback| {
        (input.host_var)(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| input.home.join(fallback))
    };
    let config_home = xdg("XDG_CONFIG_HOME", ".config");
    let data_home = xdg("XDG_DATA_HOME", ".local/share");
    let data = data_home.join("opencode");
    let mut runtime_reads = vec![subtree(package), subtree(&data)];
    runtime_reads.extend(
        crate::dylib::linked_libraries(input.provenant.git)
            .into_iter()
            .map(exact),
    );
    if access == Access::WorktreeWrite {
        runtime_reads.extend(crate::launch::git_global_files(&config_home));
    }
    let config_dir = config_home.join("opencode");
    if config_dir.is_dir() {
        runtime_reads.push(subtree(config_dir));
    }
    // Keep the account store readable but outside all writable state grants.
    let mut state_writes = ["opencode.db", "opencode.db-shm", "opencode.db-wal"]
        .map(|name| exact(data.join(name)))
        .to_vec();
    let mut setup = vec![
        SetupStep::CreateDir(data.clone()),
        SetupStep::CreateDir(bin.clone()),
        SetupStep::Symlink {
            link: bin.join("git"),
            target: input.provenant.git.to_owned(),
        },
    ];
    for name in ["log", "storage", "snapshot"] {
        let directory = data.join(name);
        state_writes.push(subtree(&directory));
        setup.push(SetupStep::CreateDir(directory));
    }
    // Writers' system roots do not cover a host-selected Homebrew Git.
    let process_executables = vec![exact(input.executable), exact(input.provenant.git)];
    let tool_exec_roots = if access == Access::WorktreeWrite {
        vec![subtree("/bin"), subtree("/usr/bin"), subtree(package)]
    } else {
        Vec::new()
    };
    env.extend([
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        (
            "XDG_CONFIG_HOME".into(),
            config_home.clone().into_os_string(),
        ),
        ("XDG_DATA_HOME".into(), data_home.clone().into_os_string()),
        (
            "XDG_CACHE_HOME".into(),
            private.join("cache").into_os_string(),
        ),
        (
            "XDG_STATE_HOME".into(),
            private.join("state").into_os_string(),
        ),
    ]);
    env.push(("OPENCODE_CONFIG_CONTENT".into(), config.into()));
    // No repository configuration or plugins, and no Claude Code files.
    for key in [
        "OPENCODE_DISABLE_AUTOUPDATE",
        "OPENCODE_DISABLE_PROJECT_CONFIG",
        "OPENCODE_DISABLE_CLAUDE_CODE",
        "OPENCODE_DISABLE_FILEWATCHER",
    ] {
        env.push((key.into(), "1".into()));
    }
    Ok(LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup,
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables,
                // The standalone server is a child copy of the same program.
                allow_process_fork: true,
                runtime_reads,
                state_writes,
                tool_exec_roots,
                discovery_reads: input
                    .cwd
                    .ancestors()
                    // Root-level probes would require a forbidden `/` anchor,
                    // and probes at the home would list `~/.claude` and
                    // `~/.codex`. Stop below both.
                    .take_while(|parent| {
                        parent.parent().is_some() && !input.home.starts_with(parent)
                    })
                    .flat_map(|parent| {
                        [".agents", ".claude", ".codex"].map(|name| exact(parent.join(name)))
                    })
                    .collect(),
                provider_api: vec![NetworkEndpoint {
                    address: "*".into(),
                    port: 443,
                }],
                loopback_server: true,
                ..ProviderProfileSpec::default()
            },
            observations: Default::default(),
        },
        stream,
    })
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    Text {
        #[serde(rename = "sessionID")]
        session: String,
        part: Text,
    },
    StepFinish {
        #[serde(rename = "sessionID")]
        session: String,
        part: Finish,
    },
    Error {
        error: Failure,
        #[serde(rename = "retryAfterMs", default)]
        retry_after_ms: Option<serde_json::Value>,
    },
    #[serde(rename = "provider.quota")]
    Quota {
        message: Option<String>,
        response: Option<Response>,
        #[serde(rename = "retryAfterMs", default)]
        retry_after_ms: Option<serde_json::Value>,
    },
    #[serde(rename = "provider.free-limit")]
    FreeLimit {
        message: Option<String>,
        response: Option<Response>,
        #[serde(rename = "retryAfterMs", default)]
        retry_after_ms: Option<serde_json::Value>,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Failure {
    #[serde(rename = "type")]
    kind: String,
    message: Option<String>,
    response: Option<Response>,
    #[serde(rename = "retryAfterMs", default)]
    retry_after_ms: Option<serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Response {
    Body { body: Option<Body> },
    Other(serde::de::IgnoredAny),
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Body {
    Json(ErrorBody),
    Text(String),
    Other(serde::de::IgnoredAny),
}
#[derive(Deserialize)]
struct ErrorBody {
    error: LimitError,
    #[serde(rename = "retryAfterMs", default)]
    retry_after_ms: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct LimitError {
    #[serde(default, rename = "type", alias = "name")]
    kind: String,
    message: Option<String>,
    #[serde(rename = "retryAfterMs", default)]
    retry_after_ms: Option<serde_json::Value>,
}
fn parse_retry_after(value: Option<&serde_json::Value>) -> Option<u64> {
    let value = value?;
    const MAX_SECONDS: u64 = 86_400;
    let seconds = if let Some(n) = value.as_u64() {
        if n == 0 {
            return None;
        }
        n.saturating_add(999) / 1000
    } else if let Some(i) = value.as_i64() {
        if i <= 0 {
            return None;
        }
        (i as u64).saturating_add(999) / 1000
    } else {
        let f = value.as_f64()?;
        if !f.is_finite() || f <= 0.0 {
            return None;
        }
        (f / 1000.0).ceil() as u64
    };
    if seconds == 0 {
        return None;
    }
    Some(seconds.min(MAX_SECONDS))
}
fn quota(
    message: Option<String>,
    response: Option<Response>,
    retry_after: Option<serde_json::Value>,
    is_free_kind: bool,
) -> E {
    let body = response
        .and_then(|response| match response {
            Response::Body { body } => body,
            Response::Other(_) => None,
        })
        .and_then(|body| match body {
            Body::Json(body) => Some(body),
            Body::Text(text) => wire::<ErrorBody>(text.as_bytes()).ok(),
            Body::Other(_) => None,
        });
    let is_free = is_free_kind
        || body
            .as_ref()
            .is_some_and(|body| body.error.kind == "FreeUsageLimitError");
    let pool = is_free.then(|| "opencode-free".into());
    let retry_val = retry_after
        .as_ref()
        .or_else(|| body.as_ref().and_then(|b| b.retry_after_ms.as_ref()))
        .or_else(|| body.as_ref().and_then(|b| b.error.retry_after_ms.as_ref()));
    let resets_in_seconds = parse_retry_after(retry_val);
    let message = body
        .and_then(|body| body.error.message)
        .or(message)
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| "provider quota exhausted".into());
    E::QuotaLimited {
        message,
        pool,
        resets_in_seconds,
    }
}
#[derive(Deserialize)]
struct Text {
    text: String,
}
#[derive(Deserialize)]
struct Finish {
    reason: String,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        Event::Text { session, part } => vec![E::Session(session), E::Text(part.text)],
        Event::StepFinish { session, part } => {
            let mut events = vec![E::Session(session)];
            if part.reason == "stop" {
                events.push(E::Terminal);
            }
            events
        }
        Event::Quota {
            message,
            response,
            retry_after_ms,
        } => vec![quota(message, response, retry_after_ms, false)],
        Event::FreeLimit {
            message,
            response,
            retry_after_ms,
        } => vec![quota(message, response, retry_after_ms, true)],
        Event::Error {
            error,
            retry_after_ms,
        } if error.kind == "provider.quota" || error.kind == "provider.free-limit" => {
            let is_free_kind = error.kind == "provider.free-limit";
            vec![quota(
                error.message,
                error.response,
                error.retry_after_ms.or(retry_after_ms),
                is_free_kind,
            )]
        }
        Event::Error { .. } => vec![E::Error],
        _ => vec![],
    })
}

crate::quota_source!(OpenCode, "opencode");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Adapter;

    #[test]
    fn discovery_reads_are_literal_along_cwd_ancestry() {
        let plan = crate::tests::read_only_plan("/fixture", None);
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let input = crate::tests::fixture_input(
            &route,
            &plan,
            &prompt,
            std::path::Path::new("/fixture/repo"),
            None,
        );
        let spec = prepare(
            &input,
            Access::ReadOnly,
            Session::Fresh,
            OpenCode.stream(None).unwrap(),
        )
        .unwrap();
        for parent in input.cwd.ancestors() {
            // `/fixture` holds the fixture home, so the walk stops below it.
            let probed = parent == input.cwd;
            for name in [".agents", ".claude", ".codex"] {
                assert_eq!(
                    spec.profile
                        .spec
                        .discovery_reads
                        .contains(&exact(parent.join(name))),
                    probed,
                    "{}",
                    parent.display()
                );
            }
        }
        assert!(
            spec.profile
                .spec
                .discovery_reads
                .iter()
                .all(|g| g.scope == provenant_confinement::permissions::PathScope::Exact)
        );
    }

    #[test]
    fn git_discovery_exec_is_exact_in_both_access_modes() {
        let plan = crate::tests::read_only_plan("/fixture", None);
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let input = crate::tests::fixture_input(
            &route,
            &plan,
            &prompt,
            std::path::Path::new("/fixture/repo"),
            None,
        );
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            let spec = prepare(
                &input,
                access,
                Session::Fresh,
                OpenCode.stream(None).unwrap(),
            )
            .unwrap();
            let profile = provenant_confinement::permissions::snapshot::render(
                plan.permissions(),
                &spec.profile,
                input.home,
            )
            .unwrap();
            let git = input.provenant.git.display();
            let literal = format!("(allow process-exec (literal \"{git}\"))");
            assert!(profile.contains(&literal), "{access:?} lacks {literal}");
            let subtree_rule = format!("(allow process-exec (subpath \"{git}\"))");
            assert!(
                !profile.contains(&subtree_rule),
                "{access:?} grants {subtree_rule}"
            );
            for shim in ["/usr/bin/git", "/usr/bin/xcode-select"] {
                assert!(
                    !profile.contains(&format!("(allow process-exec (literal \"{shim}\"))")),
                    "{access:?} grants shim {shim}"
                );
            }
            let path = spec.env.iter().find(|(key, _)| key == "PATH").unwrap();
            let bin = plan.permissions().scratch.path.join("bin");
            assert_eq!(std::env::split_paths(&path.1).next(), Some(bin.clone()));
            assert!(spec.setup.contains(&SetupStep::Symlink {
                link: bin.join("git"),
                target: input.provenant.git.to_owned(),
            }));
            // Read-only Git discovery must not enable other commands.
            assert_eq!(
                spec.profile.spec.tool_exec_roots,
                if access == Access::ReadOnly {
                    Vec::new()
                } else {
                    vec![
                        subtree("/bin"),
                        subtree("/usr/bin"),
                        subtree(package_root(input.executable).unwrap()),
                    ]
                }
            );
        }
    }

    #[test]
    fn git_discovery_ignores_host_config_in_both_access_modes() {
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            let plan = crate::tests::fixture_access_plan(access, None, false);
            let spec = OpenCode
                .prepare(crate::tests::fixture_input(
                    &route,
                    &plan,
                    &prompt,
                    std::path::Path::new("/fixture/repo"),
                    None,
                ))
                .unwrap();
            for (key, value) in [
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
            ] {
                let actual = spec.env.iter().find(|(name, _)| name == key);
                assert_eq!(
                    actual.map(|(_, value)| value.as_os_str()),
                    Some(value.as_ref()),
                    "{access:?} {key}"
                );
            }
        }
    }

    /// A literal on the home's `.claude` or `.codex` would list their entries.
    #[test]
    fn seatbelt_profile_for_a_project_under_home_lists_no_home_probes() {
        let home = std::path::Path::new("/fixture/home");
        let plan = crate::tests::read_only_plan("/fixture/home", None);
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let cwd = home.join("repo/crate");
        let input = crate::tests::fixture_input(&route, &plan, &prompt, &cwd, None);
        assert_eq!(input.home, home);
        let spec = prepare(
            &input,
            Access::ReadOnly,
            Session::Fresh,
            OpenCode.stream(None).unwrap(),
        )
        .unwrap();
        let profile = provenant_confinement::permissions::snapshot::render(
            plan.permissions(),
            &spec.profile,
            home,
        )
        .unwrap();
        let literal = |parent: &std::path::Path, name| {
            format!("(literal \"{}\")", parent.join(name).display())
        };
        for name in [".agents", ".claude", ".codex"] {
            for parent in [&cwd, &home.join("repo")] {
                assert!(profile.contains(&literal(parent, name)), "{name}");
            }
            for parent in [home, std::path::Path::new("/fixture")] {
                let literal = literal(parent, name);
                assert!(!profile.contains(&literal), "{literal}");
            }
        }
    }

    // Anchors are real host paths; on Linux `/bin` links to `/usr/bin`, and
    // only the macOS backend confines OpenCode.
    #[cfg(target_os = "macos")]
    #[test]
    fn homebrew_launch_plan_anchors_with_default_xdg_homes() {
        homebrew_discovery_fixture(Access::ReadOnly);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn homebrew_writer_launch_plan_anchors_with_discovery_directories() {
        homebrew_discovery_fixture(Access::WorktreeWrite);
    }

    #[cfg(target_os = "macos")]
    fn homebrew_discovery_fixture(access: Access) {
        use provenant_confinement::permissions::snapshot::compile;
        use provenant_confinement::permissions::{AnchoredDirectory, Anchors, required_anchors};
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let prefix = root.join("opt/homebrew");
        let package = prefix.join("Cellar/opencode-v2/2.0.32");
        let bin = prefix.join("bin");
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        fs::write(package.join("bin/opencode"), "fixture").unwrap();
        let link = bin.join("opencode");
        std::os::unix::fs::symlink("../Cellar/opencode-v2/2.0.32/bin/opencode", &link).unwrap();
        // Dispatch resolves the CLI before passing it to the provider.
        let executable = link.canonicalize().unwrap();
        // The selected Git can be outside both system and provider roots,
        // and its executable need not be named `git`.
        let git = prefix.join("Cellar/git/1.0/bin/selected-git");
        fs::create_dir_all(git.parent().unwrap()).unwrap();
        fs::write(&git, "fixture git").unwrap();
        // The project sits under the home, as lane worktrees do.
        let home = root.clone();
        fs::create_dir_all(home.join(".config/opencode")).unwrap();
        let project_parent = root.join("projects");
        fs::create_dir_all(project_parent.join("repo")).unwrap();
        // Both the read root and an ancestor contain discovery directories.
        for parent in [&project_parent, &project_parent.join("repo")] {
            for name in [".agents", ".claude", ".codex"] {
                fs::create_dir(parent.join(name)).unwrap();
                fs::write(parent.join(name).join("config.json"), "private config").unwrap();
            }
        }
        // Compilation refuses private directories other users can reach.
        for name in ["scratch", "state"] {
            fs::create_dir(project_parent.join(name)).unwrap();
            fs::set_permissions(project_parent.join(name), fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let plan = crate::tests::read_only_plan(project_parent.to_str().unwrap(), None);
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let cwd = project_parent.join("repo");
        let mut input = PrepareInput {
            executable: &executable,
            home: &home,
            ..crate::tests::fixture_input(&route, &plan, &prompt, &cwd, None)
        };
        input.provenant.git = &git;
        {
            let spec = prepare(
                &input,
                access,
                Session::Fresh,
                OpenCode.stream(None).unwrap(),
            )
            .unwrap();
            for step in &spec.setup {
                match step {
                    SetupStep::CreateDir(path) => fs::create_dir_all(path).unwrap(),
                    SetupStep::Symlink { link, target } => {
                        std::os::unix::fs::symlink(target, link).unwrap();
                    }
                    _ => panic!("unexpected setup step: {step:?}"),
                }
            }
            assert_eq!(
                project_parent
                    .join("scratch/bin/git")
                    .canonicalize()
                    .unwrap(),
                git
            );
            assert!(spec.profile.spec.runtime_reads.contains(&subtree(&package)));
            let directories = required_anchors(plan.permissions(), &spec.profile, None)
                .into_iter()
                .map(|path| {
                    AnchoredDirectory::open_canonical(&path).unwrap_or_else(|error| {
                        panic!("{access:?} anchor {} refused: {error}", path.display())
                    })
                })
                .collect::<Vec<_>>();
            let anchors = Anchors {
                home: &home,
                directories: &directories,
                worktree: None,
            };
            let launch = compile(plan.permissions(), &spec.profile, &anchors)
                .unwrap_or_else(|error| panic!("{access:?} profile refused: {error}"));
            let profile = String::from_utf8(launch.profile).unwrap();
            assert!(profile.contains(&format!(
                "(allow process-exec (literal \"{}\"))",
                git.display()
            )));
            assert!(!profile.contains(&format!(
                "(allow process-exec (subpath \"{}\"))",
                git.display()
            )));
            assert!(profile.contains(&format!("(literal \"{}\")", cwd.join(".claude").display())));
            for name in [".agents", ".claude", ".codex"] {
                for parent in [&project_parent, &cwd] {
                    let directory = parent.join(name);
                    assert!(profile.contains(&format!("(literal \"{}\")", directory.display())));
                    assert!(!profile.contains(&format!("(subpath \"{}\")", directory.display())));
                    assert!(!profile.contains(directory.join("config.json").to_str().unwrap()));
                }
                // Ancestor configuration has no enclosing data-read grant.
                assert!(!profile.contains(&format!("(subpath \"{}\")", project_parent.display())));
                let literal = format!("(literal \"{}\")", home.join(name).display());
                assert!(!profile.contains(&literal), "{access:?} grants {literal}");
            }
        }
    }

    #[test]
    fn xdg_homes_follow_absolute_host_variables() {
        let plan = crate::tests::read_only_plan("/fixture", None);
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let lookup = |key: &str| match key {
            "XDG_CONFIG_HOME" => Some("/xdg/config".into()),
            "XDG_DATA_HOME" => Some("relative/data".into()),
            _ => None,
        };
        let input = PrepareInput {
            host_var: &lookup,
            ..crate::tests::fixture_input(
                &route,
                &plan,
                &prompt,
                std::path::Path::new("/fixture/repo"),
                None,
            )
        };
        let spec = prepare(
            &input,
            Access::ReadOnly,
            Session::Fresh,
            OpenCode.stream(None).unwrap(),
        )
        .unwrap();
        let env = |key: &str| {
            spec.env
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(env("XDG_CONFIG_HOME"), Some("/xdg/config".into()));
        // A relative XDG path is ignored, as the XDG base directory spec says.
        assert_eq!(
            env("XDG_DATA_HOME"),
            Some("/fixture/home/.local/share".into())
        );
    }
}
