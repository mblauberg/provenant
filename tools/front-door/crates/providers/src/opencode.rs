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
    let mut env = base_env(input, &plan.scratch.path, "/usr/bin:/bin");
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
    let mut setup = vec![SetupStep::CreateDir(data.clone())];
    for name in ["log", "storage", "snapshot"] {
        let directory = data.join(name);
        state_writes.push(subtree(&directory));
        setup.push(SetupStep::CreateDir(directory));
    }
    env.extend([
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
                process_executables: vec![exact(input.executable)],
                // The standalone server is a child copy of the same program.
                allow_process_fork: true,
                runtime_reads,
                state_writes,
                tool_exec_roots: if access == Access::WorktreeWrite {
                    vec![subtree("/bin"), subtree("/usr/bin"), subtree(package)]
                } else {
                    Vec::new()
                },
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
    },
    #[serde(rename = "provider.quota")]
    Quota {
        message: Option<String>,
        response: Option<Response>,
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
}
#[derive(Deserialize)]
struct LimitError {
    #[serde(default, rename = "type", alias = "name")]
    kind: String,
    message: Option<String>,
}
fn quota(message: Option<String>, response: Option<Response>) -> E {
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
    let pool = body
        .as_ref()
        .filter(|body| body.error.kind == "FreeUsageLimitError")
        .map(|_| "opencode-free".into());
    let message = body
        .and_then(|body| body.error.message)
        .or(message)
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| "provider quota exhausted".into());
    E::QuotaLimited {
        message,
        pool,
        resets_in_seconds: None,
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
        Event::Quota { message, response } => vec![quota(message, response)],
        Event::Error { error } if error.kind == "provider.quota" => {
            vec![quota(error.message, error.response)]
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
        use provenant_confinement::permissions::snapshot::compile;
        use provenant_confinement::permissions::{AnchoredDirectory, Anchors, required_anchors};
        use provenant_supervisor::fs::LocalFs;
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
        // The project sits under the home, as lane worktrees do.
        let home = root.clone();
        fs::create_dir_all(home.join(".config/opencode")).unwrap();
        fs::create_dir(root.join("repo")).unwrap();
        // Compilation refuses private directories other users can reach.
        for name in ["scratch", "state"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let plan = crate::tests::read_only_plan(root.to_str().unwrap(), None);
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let cwd = root.join("repo");
        let input = PrepareInput {
            executable: &executable,
            home: &home,
            ..crate::tests::fixture_input(&route, &plan, &prompt, &cwd, None)
        };
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            let spec = prepare(
                &input,
                access,
                Session::Fresh,
                OpenCode.stream(None).unwrap(),
            )
            .unwrap();
            for step in &spec.setup {
                let SetupStep::CreateDir(path) = step else {
                    panic!("unexpected setup step: {step:?}");
                };
                fs::create_dir_all(path).unwrap();
            }
            assert!(spec.profile.spec.runtime_reads.contains(&subtree(&package)));
            let directories = required_anchors(plan.permissions(), &spec.profile, None)
                .into_iter()
                .map(|path| {
                    AnchoredDirectory::open_canonical(&LocalFs, &path).unwrap_or_else(|error| {
                        panic!("{access:?} anchor {} refused: {error}", path.display())
                    })
                })
                .collect::<Vec<_>>();
            let anchors = Anchors {
                home: &home,
                directories: &directories,
                safe_fs: &LocalFs,
                worktree: None,
            };
            let launch = compile(plan.permissions(), &spec.profile, &anchors)
                .unwrap_or_else(|error| panic!("{access:?} profile refused: {error}"));
            let profile = String::from_utf8(launch.profile).unwrap();
            assert!(profile.contains(&format!("(literal \"{}\")", cwd.join(".claude").display())));
            for name in [".agents", ".claude", ".codex"] {
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
