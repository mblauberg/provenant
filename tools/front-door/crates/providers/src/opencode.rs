use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, package_grant, subtree,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use serde::Deserialize;
use std::ffi::OsString;
use std::path::PathBuf;
pub struct OpenCode;

/// Only a typed quota refusal ends a live attempt; ordinary text and malformed
/// frames retain the existing exit-time decoding policy.
impl provenant_os::capture::FrameDecoder for OpenCode {
    fn decode(
        &self,
        frame: &[u8],
    ) -> Result<provenant_os::capture::FrameEvent, provenant_os::capture::DecodeFailure> {
        use provenant_os::capture::FrameEvent;
        Ok(
            if decode(frame).is_ok_and(|events| {
                events
                    .iter()
                    .any(|event| matches!(event, E::QuotaLimited { .. }))
            }) {
                FrameEvent::Terminal
            } else {
                FrameEvent::Progress
            },
        )
    }
}

/// Only a line beginning with the provider's error class is live evidence.
pub struct StderrQuota;
impl provenant_os::capture::FrameDecoder for StderrQuota {
    fn decode(
        &self,
        frame: &[u8],
    ) -> Result<provenant_os::capture::FrameEvent, provenant_os::capture::DecodeFailure> {
        use provenant_os::capture::FrameEvent;
        Ok(if self.partial_terminal(frame) {
            FrameEvent::Terminal
        } else {
            FrameEvent::Progress
        })
    }

    fn partial_terminal(&self, frame: &[u8]) -> bool {
        let Ok(mut line) = std::str::from_utf8(frame) else {
            return false;
        };
        loop {
            line = line.trim_start();
            let Some(rest) = line.strip_prefix("\x1b[") else {
                break;
            };
            let Some(end) = rest.find(|c: char| ('@'..='~').contains(&c)) else {
                return false;
            };
            line = &rest[end + 1..];
        }
        line.starts_with("AI.Error.QuotaExceeded:")
    }
}

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &OpenCode,
    quota_source: Some(&OpenCode),
    stdout_decoder: Some(&OpenCode),
    stderr_decoder: Some(&StderrQuota),
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

crate::adapter!(OpenCode, "opencode", prepare, failure_diagnostic);
/// The JSON run stream carries no answering-model position.
const MODEL_ON_WIRE: bool = false;

/// The answering model from OpenCode's own session record, keyed by the
/// stream's session id. Missing, ambiguous or unreadable records are unknown.
pub fn observe_model(data: &std::path::Path, session: &str) -> Result<String, ProviderError> {
    use rusqlite::{Connection, OpenFlags};
    let db = Connection::open_with_flags(
        data.join("opencode.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| ProviderError::UnknownIdentity)?;
    db.busy_timeout(std::time::Duration::from_millis(250))
        .map_err(|_| ProviderError::UnknownIdentity)?;
    let mut statement = db
        .prepare(
            "SELECT DISTINCT json_extract(data,'$.model.providerID'), json_extract(data,'$.model.id') \
             FROM session_message WHERE session_id=? AND type='assistant' LIMIT 2",
        )
        .map_err(|_| ProviderError::UnknownIdentity)?;
    let models = statement
        .query_map([session], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(|_| ProviderError::UnknownIdentity)?;
    match models.as_slice() {
        [] => Err(ProviderError::UnknownIdentity),
        [(Some(provider), Some(model))] => Ok(format!("opencode/{provider}/{model}")),
        _ => Err(ProviderError::IdentityMismatch),
    }
}

/// OpenCode uses the user's configuration and stored provider accounts.
/// Inline permissions deny edits and shell commands for read-only runs. Its
/// built-in search tools still run ripgrep, which the profile grants by path.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    let mut tool_requirements = crate::launch::ToolRequirements::default();
    if plan.selected_authentication.as_deref() != Some("login") {
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
        // The server reports its own HTTP failures only in its log, and the
        // JSON stream carries just "UnexpectedStatus: 500". Keep that log in
        // the attempt's stderr so `failure_diagnostic` can recover the cause.
        "--print-logs".as_ref(),
        "--log-level".as_ref(),
        "info".as_ref(),
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
    let staged_skills = input.stage_user_skills();
    let user_skills = staged_skills.root.clone();
    let skill_reads = user_skills.iter().map(subtree).collect::<Vec<_>>();
    let mut omitted_discovery = Vec::new();
    let permission = if access == Access::WorktreeWrite {
        // Confinement enforces path and credential access. Do not add headless
        // approval prompts for writer tools or directories outside the cwd.
        serde_json::json!({"*": "allow"})
    } else {
        let mut permission = serde_json::json!({
            "edit": "deny",
            "bash": "deny",
            "webfetch": "deny",
        });
        let mut external = serde_json::Map::new();
        let mut reads = serde_json::Map::new();
        if input.git_context {
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
            external.insert(scratch.clone(), "allow".into());
            reads.insert(scratch, "allow".into());
        }
        if let Some(skills) = &user_skills {
            // Keep native paths usable even when the skills root is itself a link.
            let pattern = format!("{}/**", skills.display());
            external.insert(pattern.clone(), "allow".into());
            reads.insert(pattern, "allow".into());
        }
        for grant in &skill_reads {
            let pattern = format!("{}/**", grant.path.display());
            external.insert(pattern.clone(), "allow".into());
            reads.insert(pattern, "allow".into());
        }
        if !external.is_empty() {
            permission["external_directory"] = external.into();
            permission["read"] = reads.into();
        }
        permission
    };
    let config = serde_json::json!({
        "autoupdate": false,
        "share": "disabled",
        "mcp": {},
        "plugin": [],
        "permission": permission,
    })
    .to_string();
    let package = package_grant(input.executable, &mut omitted_discovery);
    // Bypass Apple's Git shim, including when the selected binary is Homebrew's.
    let bin = plan.scratch.path.join("bin");
    let shell = access == Access::WorktreeWrite;
    let defaults = crate::launch::tool_search_path(input.home, shell);
    let path = match std::env::join_paths(
        std::iter::once(bin.clone()).chain(std::env::split_paths(&defaults)),
    ) {
        Ok(path) => path,
        Err(_) if !shell => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: the OpenCode Git launcher cannot be included in PATH; continuing without it"
            );
            defaults
        }
        Err(_) => return Err(ProviderError::InvalidPath),
    };
    let mut env = base_env(input, &plan.scratch.path, path);
    let xdg = |key, fallback| {
        (input.host_var)(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| input.home.join(fallback))
    };
    let config_home = xdg("XDG_CONFIG_HOME", ".config");
    let data_home = xdg("XDG_DATA_HOME", ".local/share");
    let host_data = data_home.join("opencode");
    let private_data = private.join("data");
    let data = private_data.join("opencode");
    let mut runtime_reads = vec![package.clone()];
    if access == Access::WorktreeWrite {
        let start = runtime_reads.len();
        runtime_reads.extend(crate::launch::git_global_files(&config_home));
        crate::launch::filter_optional_grants(
            &mut runtime_reads,
            start,
            input.home,
            crate::launch::OmissionSource::Toolchain,
            &mut omitted_discovery,
        );
    }
    // Preserve the existing provider configuration without making skills
    // readable when discovery is off. Do not follow unrelated config links.
    let config_dir = config_home.join("opencode");
    let optional_start = runtime_reads.len();
    if let Ok(config_dir) = std::fs::canonicalize(&config_dir)
        && !provenant_confinement::refuses_lane_root(&config_dir, true, Some(input.home))
        && let Ok(entries) = std::fs::read_dir(&config_dir)
    {
        runtime_reads.push(exact(&config_dir));
        for entry in entries.flatten() {
            if entry.file_name() == "skills" {
                continue;
            }
            if let Ok(kind) = entry.file_type() {
                if kind.is_dir() {
                    runtime_reads.push(subtree(entry.path()));
                } else if kind.is_file() {
                    runtime_reads.push(exact(entry.path()));
                }
            }
        }
    }
    if let Ok(config_dir) = std::fs::canonicalize(&config_dir)
        && provenant_confinement::refuses_lane_root(&config_dir, true, Some(input.home))
        && !omitted_discovery.iter().any(|entry| {
            entry.path == config_dir
                && entry.source == crate::launch::OmissionSource::ProviderConfig
        })
    {
        omitted_discovery.push(crate::launch::OmittedGrant {
            path: config_dir,
            source: crate::launch::OmissionSource::ProviderConfig,
        });
    }
    crate::launch::filter_optional_grants(
        &mut runtime_reads,
        optional_start,
        input.home,
        crate::launch::OmissionSource::ProviderConfig,
        &mut omitted_discovery,
    );
    let mut authentication_roots = runtime_reads.split_off(optional_start);
    authentication_roots.push(exact(host_data.join("auth.json")));
    runtime_reads.extend(skill_reads);
    let mut setup = vec![
        SetupStep::CreateDir(data.clone()),
        SetupStep::Symlink {
            link: data.join("auth.json"),
            target: host_data.join("auth.json"),
        },
        SetupStep::CreateDir(bin.clone()),
        SetupStep::Symlink {
            link: bin.join("git"),
            target: input.provenant.git.to_owned(),
        },
    ];
    // Mirror provider configuration as individual links; staged skills have their own tree.
    let private_config = private.join("config");
    {
        let private_opencode = private_config.join("opencode");
        setup.push(SetupStep::CreateDir(private_opencode.clone()));
        if let Ok(canonical) = std::fs::canonicalize(&config_dir)
            && !provenant_confinement::refuses_lane_root(&canonical, true, Some(input.home))
            && let Ok(entries) = std::fs::read_dir(&config_dir)
        {
            for entry in entries.flatten() {
                if entry.file_name() != "skills"
                    && entry.file_type().is_ok_and(|kind| !kind.is_symlink())
                {
                    setup.push(SetupStep::Symlink {
                        link: private_opencode.join(entry.file_name()),
                        target: entry.path(),
                    });
                }
            }
        }
        if let Some(skills) = &staged_skills.root {
            setup.push(SetupStep::Symlink {
                link: private_opencode.join("skills"),
                target: skills.clone(),
            });
        }
    }
    // `shell` holds per-project shell state; the server creates its entry
    // while resolving the location, and refuses every request (HTTP 500)
    // when that mkdir is denied.
    for name in ["log", "storage", "snapshot", "shell"] {
        let directory = data.join(name);
        setup.push(SetupStep::CreateDir(directory));
    }
    // Writers' system roots do not cover a host-selected Homebrew Git.
    let process_executables = vec![exact(input.executable), exact(input.provenant.git)];
    let mut tool_exec_roots = if shell {
        vec![subtree("/bin"), subtree("/usr/bin"), package.clone()]
    } else {
        Vec::new()
    };
    if access == Access::ReadOnly {
        // OpenCode's grep, glob and list tools execute ripgrep. It resolves
        // `rg` from PATH, then from its cache bin ($XDG_CACHE_HOME/opencode/bin),
        // and installs it there when absent. The lane sets XDG_CACHE_HOME to
        // its private state, whose grant already covers the read, so grant
        // exactly that binary for execution, alongside system grep. No shell is enabled.
        let rg = private.join("cache/opencode/bin/rg");
        // Resolve install-name symlinks to exact files, accepting libraries by
        // content regardless of the host's package manager or PATH ordering.
        tool_exec_roots.push(exact(rg));
        tool_exec_roots.push(exact("/usr/bin/grep"));
    }
    env.extend([
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("XDG_CONFIG_HOME".into(), private_config.into_os_string()),
        ("XDG_DATA_HOME".into(), private_data.into_os_string()),
        (
            "XDG_CACHE_HOME".into(),
            private.join("cache").into_os_string(),
        ),
        (
            "XDG_STATE_HOME".into(),
            private.join("state").into_os_string(),
        ),
    ]);
    // Only lanes whose permissions allow bash can run the toolchain.
    if shell {
        crate::launch::rust_tools(input, access, &mut tool_requirements, &mut env, &mut setup);
        crate::launch::node_tools(
            input.home,
            access,
            crate::launch::lane_cache(input),
            &mut tool_requirements,
            &mut env,
        );
    }
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
    let mut spec = LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup,
        omitted_discovery,
        user_skills: staged_skills,
        tool_requirements,
        git_wrapper: Default::default(),
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables,
                // The standalone server is a child copy of the same program.
                allow_process_fork: true,
                runtime_reads,
                authentication: vec![AuthenticationProfile {
                    mechanism: "login".into(),
                    roots: authentication_roots,
                    in_place_writes: Vec::new(),
                    keychain: None,
                }],
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
    };
    crate::launch::prepare_tool_requirements(input, &mut spec);
    Ok(spec)
}

/// The server's own cause for a failed HTTP request, from its log lines
/// (`... cause="..." ... http.status=500`). Only a bounded, printable cause on
/// a 5xx line is kept; no other stderr prose is copied.
fn failure_diagnostic(stderr: &[u8]) -> Option<String> {
    const LIMIT: usize = 240;
    let stderr = std::str::from_utf8(stderr).ok()?;
    stderr.lines().find_map(|line| {
        let status = line.split(" http.status=").nth(1)?;
        let digits = status.split(' ').next()?;
        if digits.len() != 3
            || !digits.starts_with('5')
            || !digits.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let cause = line.split(" cause=\"").nth(1)?;
        let end = cause.find("\" ").or_else(|| cause.rfind('"'))?;
        // The log escapes newlines, so the first stack frame ends the cause.
        let first = cause[..end].split("\\n").next()?.trim();
        let printable: String = first
            .chars()
            .map(|c| {
                if c.is_ascii_graphic() || c == ' ' {
                    c
                } else {
                    '?'
                }
            })
            .collect();
        let first = provenant_confinement::redact::bounded_line(&printable, LIMIT);
        (!first.is_empty()).then(|| format!("opencode server failed with HTTP {digits}: {first}"))
    })
}

/// Recognise OpenCode's unstructured quota errors without treating arbitrary
/// provider output as protocol events. Pool and retry evidence come only from
/// the matched line and the one after it, so unrelated stderr cannot steer them.
pub fn stderr_quota(stderr: &[u8]) -> Option<E> {
    let text = std::str::from_utf8(stderr).ok()?;
    let mut lines = text.lines();
    let matched = lines.find(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("ai.error.quotaexceeded") || lower.contains("usage limit exceeded")
    })?;
    let scope = format!("{matched}\n{}", lines.next().unwrap_or_default()).to_ascii_lowercase();
    let pool = if scope.contains("free tier") || scope.contains("free usage") {
        "opencode-free"
    } else {
        "opencode-go"
    };
    // A number after `key`, with its unit word when one follows.
    let number_after = |key: &str| {
        let at = scope.find(key)?;
        let value = scope[at + key.len()..]
            .trim_start_matches(|c: char| c.is_ascii_whitespace() || matches!(c, '"' | ':' | '='));
        let digits = value.len() - value.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let number = value[..digits].parse::<u64>().ok()?;
        let unit: String = value[digits..]
            .trim_start()
            .chars()
            .take_while(char::is_ascii_alphabetic)
            .collect();
        Some((number, unit))
    };
    let seconds = |number: u64, unit: &str, bare_ok: bool| -> Option<u64> {
        let scale = match unit {
            "" if bare_ok => 1,
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
            _ => return None,
        };
        (number > 0).then(|| number.saturating_mul(scale).min(86_400))
    };
    let retry = number_after("retryafterms")
        .and_then(|(n, _)| parse_retry_after(Some(&serde_json::Value::from(n))))
        .or_else(|| {
            number_after("retry-after")
                .and_then(|(n, unit)| seconds(n, &unit, true))
                .or_else(|| {
                    number_after("retry after").and_then(|(n, unit)| seconds(n, &unit, false))
                })
        });
    Some(E::QuotaLimited {
        message: format!("OpenCode quota exhausted ({pool})"),
        pool: Some(pool.into()),
        resets_in_seconds: retry,
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
        #[serde(rename = "sessionID", default)]
        session: Option<String>,
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
    #[serde(default)]
    status: Option<u16>,
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
        // OpenCode emits completed text parts, but can drop the final
        // step_finish while reconciling at idle. Success still requires exit 0
        // and no errors in StreamState::finish.
        Event::Text { session, part } => vec![
            E::Session(session),
            E::MessageStart,
            E::Text(part.text),
            E::Terminal,
        ],
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
            session,
        } => {
            let mut events = Vec::new();
            if let Some(session) = session.filter(|session| !session.is_empty()) {
                events.push(E::Session(session));
            }
            let event = if error.kind == "provider.quota" || error.kind == "provider.free-limit" {
                let is_free_kind = error.kind == "provider.free-limit";
                quota(
                    error.message,
                    error.response,
                    error.retry_after_ms.or(retry_after_ms),
                    is_free_kind,
                )
            } else if error.kind == "provider.no-route" {
                // OpenCode 2.2.0 maps ModelUnavailableError to this type.
                E::ModelUnavailable {
                    message: error
                        .message
                        .unwrap_or_else(|| "provider model unavailable".into()),
                }
            } else if error.kind == "provider.internal"
                && (error.status == Some(529)
                    || (error.status == Some(500)
                        && error.message.as_deref().is_some_and(|message| {
                            message.to_ascii_lowercase().contains("overloaded")
                        })))
            {
                E::ModelOverloaded {
                    message: error
                        .message
                        .filter(|message| !message.is_empty())
                        .unwrap_or_else(|| "provider overloaded (HTTP 529)".into()),
                }
            } else {
                E::Error
            };
            events.push(event);
            events
        }
        _ => vec![],
    })
}

crate::quota_source!(OpenCode, "opencode");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Adapter;

    #[test]
    fn live_stderr_quota_requires_error_class_at_line_start() {
        use provenant_os::capture::{FrameDecoder, FrameEvent};
        let decoder = super::StderrQuota;
        assert_eq!(
            decoder
                .decode(b"AI.Error.QuotaExceeded: Go usage limit exceeded")
                .unwrap(),
            FrameEvent::Terminal
        );
        assert_eq!(
            decoder
                .decode(b"\x1b[31mAI.Error.QuotaExceeded: limit")
                .unwrap(),
            FrameEvent::Terminal
        );
        for text in [
            "The documentation mentions quota and usage limit exceeded",
            "example AI.Error.QuotaExceeded: Go usage limit exceeded",
            "AI.Error.QuotaExceeded elsewhere in prose",
        ] {
            assert_eq!(
                decoder.decode(text.as_bytes()).unwrap(),
                FrameEvent::Progress
            );
        }
    }

    #[test]
    fn stderr_quota_forms_and_retry() {
        use super::{E, stderr_quota};
        for (line, pool, reset) in [
            (
                "AI.Error.QuotaExceeded: Go usage limit exceeded",
                "opencode-go",
                None,
            ),
            (
                "free tier usage limit exceeded retryAfterMs=1500",
                "opencode-free",
                Some(2),
            ),
            (
                "AI.Error.QuotaExceeded\nretry-after: 60",
                "opencode-go",
                Some(60),
            ),
        ] {
            assert!(
                matches!(stderr_quota(line.as_bytes()), Some(E::QuotaLimited { pool: Some(actual), resets_in_seconds, .. }) if actual == pool && resets_in_seconds == reset)
            );
        }
        assert!(stderr_quota(b"ordinary failure").is_none());
    }

    #[test]
    fn stderr_quota_scopes_pool_and_retry_to_the_matched_line() {
        use super::{E, stderr_quota};
        let quota = |text: &str| match stderr_quota(text.as_bytes()) {
            Some(E::QuotaLimited {
                pool,
                resets_in_seconds,
                ..
            }) => (pool, resets_in_seconds),
            other => panic!("unexpected {other:?}"),
        };
        // Unrelated "free" and retry text elsewhere must not steer the result.
        let (pool, reset) = quota(
            "using free model fallback\nretry after 5 seconds\nAI.Error.QuotaExceeded: Go usage limit exceeded\nstack\nretryAfterMs=9000\nfree tier\n",
        );
        assert_eq!(pool.as_deref(), Some("opencode-go"));
        assert_eq!(reset, None);
        // Units are honoured; a bare number is seconds only in the header form.
        assert_eq!(
            quota("usage limit exceeded, retry after 30 minutes").1,
            Some(1800)
        );
        assert_eq!(
            quota("usage limit exceeded, retry after 30 sec").1,
            Some(30)
        );
        assert_eq!(quota("usage limit exceeded, retry after 30").1, None);
        assert_eq!(
            quota("usage limit exceeded\nretry-after: 30 minutes").1,
            Some(1800)
        );
        assert_eq!(
            quota("usage limit exceeded\nretry-after: 30 fortnights").1,
            None
        );
        assert_eq!(
            quota("Free usage limit exceeded").0.as_deref(),
            Some("opencode-free")
        );
    }

    #[test]
    fn unavailable_model_is_a_typed_refusal() {
        let mut stream = OpenCode.stream(None).unwrap();
        let mut events = Vec::new();
        for chunk in
            include_bytes!("../../../tests/fixtures/providers/opencode/model-unavailable.jsonl")
                .chunks(17)
        {
            events.extend(OpenCode.observe(&mut stream, chunk).unwrap());
        }
        assert!(events.contains(&E::ModelUnavailable {
            message: "Model unavailable: opencode/misspelt-free".into(),
        }));
        assert!(events.contains(&E::Session("ses_x".into())));
        let outcome = OpenCode
            .finish(
                stream,
                crate::ExitObservation {
                    code: Some(1),
                    timed_out: false,
                },
            )
            .unwrap();
        assert_eq!(outcome.status(), crate::OutcomeStatus::Refused);
        assert_eq!(outcome.session(), Some("ses_x"));
    }

    #[test]
    fn upstream_529_is_a_typed_overload() {
        let events = decode(br#"{"type":"error","error":{"type":"provider.internal","status":529,"message":"Overloaded"}}"#).unwrap();
        assert_eq!(
            events,
            vec![E::ModelOverloaded {
                message: "Overloaded".into()
            }]
        );
        assert_eq!(
            decode(br#"{"type":"error","error":{"type":"provider.internal","status":500,"message":"Overloaded"}}"#).unwrap(),
            vec![E::ModelOverloaded { message: "Overloaded".into() }]
        );
        for raw in [
            br#"{"type":"error","error":{"type":"provider.internal","status":500,"message":"UnexpectedStatus: 500"}}"#.as_slice(),
            br#"{"type":"error","error":{"type":"unknown","status":529,"message":"Overloaded"}}"#.as_slice(),
        ] {
            assert_eq!(decode(raw).unwrap(), vec![E::Error]);
        }
        let events = decode(br#"{"type":"error","sessionID":"ses_x","error":{"type":"provider.internal","status":529,"message":"Overloaded"}}"#).unwrap();
        assert_eq!(
            events,
            vec![
                E::Session("ses_x".into()),
                E::ModelOverloaded {
                    message: "Overloaded".into()
                }
            ]
        );
    }

    #[test]
    fn v2_text_without_finish_observes_model_from_live_wal() {
        let dir = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(dir.path().join("opencode.db")).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; \
             CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT)",
        )
        .unwrap();
        db.execute(
            "INSERT INTO session_message VALUES (?, 'assistant', ?)",
            (
                "ses_x",
                r#"{"model":{"id":"beam-free","providerID":"opencode-go"},"finish":"stop"}"#,
            ),
        )
        .unwrap();
        assert!(dir.path().join("opencode.db-wal").is_file());
        let mut stream = OpenCode.stream(None).unwrap();
        for chunk in
            include_bytes!("../../../tests/fixtures/providers/opencode/v2.2.0-success.jsonl")
                .chunks(17)
        {
            OpenCode.observe(&mut stream, chunk).unwrap();
        }
        let outcome = OpenCode
            .finish(
                stream,
                crate::ExitObservation {
                    code: Some(0),
                    timed_out: false,
                },
            )
            .unwrap();
        assert_eq!(outcome.status(), crate::OutcomeStatus::Success);
        assert_eq!(outcome.session(), Some("ses_x"));
        assert_eq!(
            observe_model(dir.path(), outcome.session().unwrap()).unwrap(),
            "opencode/opencode-go/beam-free"
        );
    }

    #[test]
    fn recorded_stream_session_selects_v2_model() {
        let dir = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(dir.path().join("opencode.db")).unwrap();
        db.execute_batch("CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT)")
            .unwrap();
        db.execute(
            "INSERT INTO session_message VALUES (?, 'assistant', ?)",
            (
                "FIXTURE_SESSION_IDENTIFIER_007",
                r#"{"model":{"id":"glm-5.3-flash-free","providerID":"opencode"}}"#,
            ),
        )
        .unwrap();
        let mut stream = OpenCode.stream(None).unwrap();
        OpenCode
            .observe(
                &mut stream,
                include_bytes!(
                    "../../../tests/fixtures/providers/opencode/unversioned-success.jsonl"
                ),
            )
            .unwrap();
        let outcome = OpenCode
            .finish(
                stream,
                crate::ExitObservation {
                    code: Some(0),
                    timed_out: false,
                },
            )
            .unwrap();
        assert_eq!(
            observe_model(dir.path(), outcome.session().unwrap()).unwrap(),
            "opencode/opencode/glm-5.3-flash-free"
        );
        assert!(matches!(
            observe_model(dir.path(), "ses_none"),
            Err(ProviderError::UnknownIdentity)
        ));
        db.execute(
            "INSERT INTO session_message VALUES (?, 'assistant', ?)",
            (
                "FIXTURE_SESSION_IDENTIFIER_007",
                r#"{"model":{"id":"other","providerID":"opencode"}}"#,
            ),
        )
        .unwrap();
        assert!(matches!(
            observe_model(dir.path(), outcome.session().unwrap()),
            Err(ProviderError::IdentityMismatch)
        ));
    }

    // Reduced from #1891: three tool steps, then final text without step_finish.
    const FINAL_TEXT_WITHOUT_FINISH: &str = concat!(
        "{\"type\":\"step_start\",\"sessionID\":\"fixture\"}\n",
        "{\"type\":\"tool_use\",\"sessionID\":\"fixture\"}\n",
        "{\"type\":\"step_finish\",\"sessionID\":\"fixture\",\"part\":{\"reason\":\"tool-calls\"}}\n",
        "{\"type\":\"step_start\",\"sessionID\":\"fixture\"}\n",
        "{\"type\":\"tool_use\",\"sessionID\":\"fixture\"}\n",
        "{\"type\":\"step_finish\",\"sessionID\":\"fixture\",\"part\":{\"reason\":\"tool-calls\"}}\n",
        "{\"type\":\"step_start\",\"sessionID\":\"fixture\"}\n",
        "{\"type\":\"tool_use\",\"sessionID\":\"fixture\"}\n",
        "{\"type\":\"step_finish\",\"sessionID\":\"fixture\",\"part\":{\"reason\":\"tool-calls\"}}\n",
        "{\"type\":\"step_start\",\"sessionID\":\"fixture\"}\n",
        "{\"type\":\"text\",\"sessionID\":\"fixture\",\"part\":{\"text\":\"Provenant\"}}\n",
    );

    #[test]
    fn final_text_without_step_finish_succeeds_only_on_clean_exit() {
        use crate::{ExitObservation, OutcomeStatus};

        for (code, timed_out, expected) in [
            (Some(0), false, OutcomeStatus::Success),
            (Some(1), false, OutcomeStatus::Failed),
            (None, false, OutcomeStatus::Failed),
            (Some(0), true, OutcomeStatus::Partial),
        ] {
            let mut stream = OpenCode.stream(None).unwrap();
            let mut events = Vec::new();
            for chunk in FINAL_TEXT_WITHOUT_FINISH.as_bytes().chunks(17) {
                events.extend(OpenCode.observe(&mut stream, chunk).unwrap());
            }
            assert!(events.contains(&E::Text("Provenant".into())));
            let outcome = OpenCode
                .finish(stream, ExitObservation { code, timed_out })
                .unwrap();
            assert_eq!(outcome.status(), expected, "{code:?}, {timed_out}");
        }
    }

    #[test]
    fn final_text_does_not_hide_error_events() {
        let error = b"{\"type\":\"error\",\"error\":{\"type\":\"APIError\"}}\n";
        for error_first in [true, false] {
            let mut stream = OpenCode.stream(None).unwrap();
            let text = FINAL_TEXT_WITHOUT_FINISH.as_bytes();
            let parts = if error_first {
                [error.as_slice(), text]
            } else {
                [text, error.as_slice()]
            };
            for part in parts {
                OpenCode.observe(&mut stream, part).unwrap();
            }
            let outcome = OpenCode
                .finish(
                    stream,
                    crate::ExitObservation {
                        code: Some(0),
                        timed_out: false,
                    },
                )
                .unwrap();
            assert_eq!(outcome.status(), crate::OutcomeStatus::Failed);
        }
    }

    #[test]
    fn clean_exit_without_answer_or_stop_remains_partial() {
        let raw = FINAL_TEXT_WITHOUT_FINISH
            .rsplit_once("{\"type\":\"text\"")
            .unwrap()
            .0;
        let mut stream = OpenCode.stream(None).unwrap();
        OpenCode.observe(&mut stream, raw.as_bytes()).unwrap();
        let outcome = OpenCode
            .finish(
                stream,
                crate::ExitObservation {
                    code: Some(0),
                    timed_out: false,
                },
            )
            .unwrap();
        assert_eq!(outcome.status(), crate::OutcomeStatus::Partial);
    }

    #[test]
    fn server_shell_state_is_created_and_writable() {
        let plan = crate::tests::read_only_plan("/fixture", Some("login"));
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
        let shell = plan
            .permissions()
            .private_state
            .as_ref()
            .unwrap()
            .path
            .join("data/opencode/shell");
        assert!(spec.setup.iter().any(|step| matches!(
            step, SetupStep::CreateDir(path) if *path == shell)));
        assert!(shell.starts_with(&plan.permissions().private_state.as_ref().unwrap().path));
        let argv: Vec<_> = spec.argv.iter().map(|a| a.to_string_lossy()).collect();
        assert!(argv.iter().any(|a| a == "--print-logs"));
    }

    #[test]
    fn read_only_search_grants_cached_ripgrep_and_system_grep() {
        use provenant_confinement::permissions::{PathGrant, PathScope};
        use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};

        let base = crate::tests::read_only_plan("/fixture", Some("login"));
        let mut permissions = base.permissions().clone();
        // Provider state is attempt-private and separate from the lane state
        // root, as in the Seatbelt snapshot fixture.
        permissions.private_state = Some(PathGrant {
            path: "/fixture/provider-state".into(),
            scope: PathScope::Subtree,
        });
        let plan = ConfinedPlan::new(PlanRequest {
            scope: base.scope(),
            permissions,
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        })
        .unwrap();
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
        let env = |key: &str| {
            spec.env
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        let config: serde_json::Value = serde_json::from_str(
            &env("OPENCODE_CONFIG_CONTENT")
                .expect("inline config")
                .to_string_lossy(),
        )
        .unwrap();
        assert_eq!(config["permission"]["bash"], "deny");
        assert_eq!(config["permission"]["edit"], "deny");

        // OpenCode resolves ripgrep from PATH and then its cache bin, which is
        // XDG_CACHE_HOME/opencode/bin under the lane's private state.
        let rg = exact(std::path::Path::new(
            "/fixture/provider-state/cache/opencode/bin/rg",
        ));
        assert_eq!(
            spec.profile.spec.process_executables,
            [exact(input.executable), exact(input.provenant.git)]
        );
        assert_eq!(
            spec.profile.spec.tool_exec_roots,
            [rg.clone(), exact("/usr/bin/grep")]
        );
        // No shell toolchain leaks into a read-only lane.
        let path: Vec<_> = std::env::split_paths(&env("PATH").expect("PATH")).collect();
        assert!(!path.contains(&input.home.join(".cargo/bin")), "{path:?}");
        assert!(spec.env.iter().all(|(name, _)| name != "CARGO_HOME"));

        let profile = provenant_confinement::permissions::snapshot::render(
            plan.permissions(),
            &spec.profile,
            input.home,
        )
        .unwrap();
        let exec = format!("(allow process-exec (literal \"{}\"))", rg.path.display());
        assert!(profile.contains(&exec), "{exec}");
        assert!(
            !profile.contains("(allow process-exec (subpath"),
            "read-only search must not grant an exec subtree"
        );
        // The private state grant is the read access ripgrep needs.
        assert!(profile.contains("(allow file-read* (subpath \"/fixture/provider-state\"))"));
    }

    #[cfg(unix)]
    #[test]
    fn cached_search_library_grants_follow_content_and_exclude_lane_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;
        use std::path::Path;

        fn image(path: &Path, file_type: u32, dependencies: &[&Path]) -> std::io::Result<()> {
            let mut bytes = vec![0; 32];
            bytes[..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
            bytes[12..16].copy_from_slice(&file_type.to_le_bytes());
            bytes[16..20].copy_from_slice(&(dependencies.len() as u32).to_le_bytes());
            for dependency in dependencies {
                let name = dependency.to_str().unwrap().as_bytes();
                let start = bytes.len();
                let size = (24 + name.len() + 1).next_multiple_of(8);
                bytes.extend(0xc_u32.to_le_bytes());
                bytes.extend((size as u32).to_le_bytes());
                bytes.extend(24_u32.to_le_bytes());
                bytes.resize(start + 24, 0);
                bytes.extend(name);
                bytes.resize(start + size, 0);
            }
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, bytes)
        }

        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let lane = root.join("lane");
        let cwd = lane.join("repo");
        let home = lane.join("home");
        let first = root.join("host/arbitrary/first.dylib");
        let second = root.join("host/other/second.dylib");
        image(&first, 6, &[&second])?;
        image(&second, 6, &[])?;
        let alias = root.join("host/alias.dylib");
        symlink(&first, &alias)?;
        let denied = [
            home.join("Documents/plain.txt"),
            root.join("host/Cellar/plain.txt"),
            home.join("fixture.dylib"),
            lane.join("state/fixture.dylib"),
            lane.join("scratch/fixture.dylib"),
            cwd.join("fixture.dylib"),
            root.join("host/.ssh/fixture.dylib"),
            root.join("host/executable"),
            root.join("host/directory"),
        ];
        // Already-covered images still contribute their own dependencies.
        let hidden = root.join("host/hidden.dylib");
        image(&hidden, 6, &[])?;
        for path in &denied {
            image(path, 6, &[&hidden])?;
        }
        std::fs::write(&denied[0], "plain text")?;
        std::fs::write(&denied[1], "plain text")?;
        image(&denied[7], 2, &[&hidden])?;
        std::fs::remove_file(&denied[8])?;
        std::fs::create_dir(&denied[8])?;
        let cached = lane.join("state/cache/opencode/bin/rg");
        let mut dependencies = vec![alias.as_path()];
        dependencies.extend(denied.iter().map(|path| path.as_path()));
        image(&cached, 2, &dependencies)?;
        // A non-executable, unrelated rg earlier on PATH cannot suppress grants.
        let host_bin = root.join("host/bin");
        std::fs::create_dir_all(&host_bin)?;
        std::fs::write(host_bin.join("rg"), "unrelated")?;
        let plan = crate::tests::read_only_plan(lane.to_str().unwrap(), Some("login"));
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let mut input = crate::tests::fixture_input(&route, &plan, &prompt, &cwd, None);
        input.home = &home;
        let host_var = |name: &str| (name == "PATH").then(|| host_bin.clone().into_os_string());
        input.host_var = &host_var;
        let spec = prepare(
            &input,
            Access::ReadOnly,
            Session::Fresh,
            OpenCode.stream(None)?,
        )?;
        let tool = spec
            .prepared_tools()
            .iter()
            .find(|tool| tool.executable == cached)
            .expect("cached rg has prepared closure facts");
        assert_eq!(
            tool.dependency_closure.outcome,
            crate::ClosureOutcome::Incomplete
        );
        assert!(
            tool.dependency_closure
                .reasons
                .iter()
                .any(|reason| matches!(reason, crate::IncompleteReason::Malformed(_)))
        );
        for path in &tool.dependency_closure.libraries {
            assert_eq!(std::fs::canonicalize(path)?, *path);
            assert!(spec.profile.spec.runtime_reads.iter().any(|grant| {
                grant.path == *path
                    && grant.scope == provenant_confinement::permissions::PathScope::Exact
            }));
            assert!(spec.profile.spec.runtime_reads.iter().all(|grant| {
                grant.scope != provenant_confinement::permissions::PathScope::Subtree
                    || !path.starts_with(&grant.path)
            }));
            assert!(
                provenant_confinement::write_grants(plan.permissions(), &spec.profile)
                    .iter()
                    .all(|grant| !path.starts_with(&grant.path))
            );
        }
        for path in [&first, &second, &denied[2], &hidden] {
            assert!(
                tool.dependency_closure.libraries.contains(path),
                "{}",
                path.display()
            );
            assert_eq!(std::fs::canonicalize(path)?, *path);
            assert!(
                spec.profile
                    .spec
                    .runtime_reads
                    .iter()
                    .all(|grant| grant.scope
                        != provenant_confinement::permissions::PathScope::Subtree
                        || !path.starts_with(&grant.path))
            );
            assert!(
                provenant_confinement::write_grants(plan.permissions(), &spec.profile)
                    .iter()
                    .all(|grant| !path.starts_with(&grant.path) && !grant.path.starts_with(path))
            );
        }
        for path in [&denied[3], &denied[4], &denied[5]] {
            assert!(
                tool.dependency_closure.already_covered.contains(path),
                "{}",
                path.display()
            );
        }
        assert!(
            tool.dependency_closure
                .omitted
                .contains(&root.join("host/.ssh/fixture.dylib"))
        );
        assert!(
            tool.dependency_closure
                .reasons
                .iter()
                .any(|reason| matches!(
                    reason, crate::IncompleteReason::OmittedCredential(path)
                        if path == &root.join("host/.ssh/fixture.dylib")
                ))
        );
        assert!(
            !tool
                .receipt()
                .to_string()
                .contains(&root.display().to_string())
        );
        let profile = provenant_confinement::permissions::snapshot::render(
            plan.permissions(),
            &spec.profile,
            &home,
        )?;
        for path in [&first, &second, &denied[2], &hidden] {
            assert!(
                profile.contains(&format!(
                    "(allow file-read* (literal \"{}\"))",
                    path.display()
                )),
                "{}",
                path.display()
            );
        }
        for path in denied
            .iter()
            .enumerate()
            .filter_map(|(index, path)| (index != 2).then_some(path))
            .chain([&alias])
        {
            assert!(
                !profile.contains(&format!(
                    "(allow file-read* (literal \"{}\"))",
                    path.display()
                )),
                "{}",
                path.display()
            );
        }
        assert!(!profile.contains("(allow process-exec (subpath"));
        Ok(())
    }

    fn search_tool(path: &std::ffi::OsStr, required: bool) -> Result<Option<PathBuf>, String> {
        let rg = std::env::split_paths(path)
            .map(|dir| dir.join("rg"))
            .find(|path| path.is_file());
        if rg.is_none() {
            provenant_test_support::missing_prerequisite(required, "rg for native search")?;
        }
        Ok(rg)
    }

    #[test]
    fn missing_native_search_tool_obeys_required_control() {
        let path = tempfile::tempdir().unwrap();
        let error = search_tool(path.path().as_os_str(), true).unwrap_err();
        assert!(error.contains("rg"));
        assert_eq!(search_tool(path.path().as_os_str(), false).unwrap(), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[allow(clippy::disallowed_methods)]
    fn read_only_search_runs_in_native_profile() -> Result<(), Box<dyn std::error::Error>> {
        let Some(rg) = search_tool(
            &std::env::var_os("PATH").unwrap_or_default(),
            std::env::var_os("PROVENANT_CONFINEMENT_REQUIRED").is_some(),
        )?
        else {
            return Ok(());
        };
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let cached = root.join("state/cache/opencode/bin/rg");
        std::fs::create_dir_all(cached.parent().unwrap())?;
        std::fs::copy(rg, &cached)?;
        let plan = crate::tests::read_only_plan(root.to_str().unwrap(), Some("login"));
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let mut input = crate::tests::fixture_input(&route, &plan, &prompt, &root, None);
        input.host_var = &|name| std::env::var_os(name);
        let spec = prepare(
            &input,
            Access::ReadOnly,
            Session::Fresh,
            OpenCode.stream(None)?,
        )?;
        let requirement = spec
            .prepared_tools()
            .iter()
            .find(|tool| tool.executable == cached)
            .expect("cached rg has prepared closure facts");
        for library in &requirement.dependency_closure.libraries {
            assert!(spec.profile.spec.runtime_reads.contains(&exact(library)));
        }
        let profile = provenant_confinement::permissions::snapshot::render(
            plan.permissions(),
            &spec.profile,
            input.home,
        )?;
        for program in [cached.as_path(), std::path::Path::new("/usr/bin/grep")] {
            let output = std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &profile])
                .arg(program)
                .arg("--version")
                .output()?;
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            if diagnostic.contains("sandbox_apply: Operation not permitted") {
                provenant_test_support::skip_sandbox_dependent_test(diagnostic.to_string())?;
                return Ok(());
            }
            assert!(
                output.status.success(),
                "{}: {diagnostic}",
                program.display()
            );
        }
        Ok(())
    }

    #[test]
    fn server_http_failure_cause_is_a_bounded_diagnostic() {
        let log = concat!(
            "timestamp=t level=INFO run=a message=\"watcher stopped\" path=/x http.span=1\n",
            "timestamp=t level=INFO run=a cause=\"Error: EPERM: operation not permitted, ",
            "mkdir '/h/.local/share/opencode/shell/abc'\\n    at ServerProcess.start\" ",
            "http.span=68 role=server http.method=GET http.url=/api/location http.status=500\n",
        );
        assert_eq!(
            failure_diagnostic(log.as_bytes()).as_deref(),
            Some(
                "opencode server failed with HTTP 500: Error: EPERM: operation not permitted, \
                 mkdir '/h/.local/share/opencode/shell/abc'"
            )
        );
        let long = format!(
            "level=INFO cause=\"{}\" http.status=503\n",
            "x".repeat(1000)
        );
        let diagnostic = failure_diagnostic(long.as_bytes()).unwrap();
        assert_eq!(
            diagnostic.chars().count(),
            240 + "opencode server failed with HTTP 503: ".len()
        );
        assert!(diagnostic.ends_with('…'));
        let secret =
            "level=INFO cause=\"fetch https://user:secret@example.invalid/x\" http.status=500\n";
        let redacted = failure_diagnostic(secret.as_bytes()).unwrap();
        assert!(!redacted.contains("secret"), "{redacted}");
        for unrelated in [
            "level=INFO cause=\"boom\" http.status=404\n",
            "level=INFO message=\"ok\" http.status=500\n",
            "level=INFO cause=\"\" http.status=500\n",
            "garbage",
        ] {
            assert_eq!(
                failure_diagnostic(unrelated.as_bytes()),
                None,
                "{unrelated}"
            );
        }
    }

    #[test]
    fn discovery_reads_are_literal_along_cwd_ancestry() {
        let plan = crate::tests::read_only_plan("/fixture", Some("login"));
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
    fn read_only_search_path_skips_unencodable_git_directory() {
        use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
        let base = crate::tests::read_only_plan("/fixture", Some("login"));
        let mut permissions = base.permissions().clone();
        permissions.scratch.path = "/fixture/scratch:other".into();
        let plan = ConfinedPlan::new(PlanRequest {
            scope: base.scope(),
            permissions,
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        })
        .unwrap();
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
        let path = &spec.env.iter().find(|(key, _)| key == "PATH").unwrap().1;
        assert_eq!(
            std::env::split_paths(path).collect::<Vec<_>>(),
            [PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
        );
        assert!(matches!(
            prepare(
                &input,
                Access::WorktreeWrite,
                Session::Fresh,
                OpenCode.stream(None).unwrap()
            ),
            Err(ProviderError::InvalidPath)
        ));
    }

    #[test]
    fn writer_search_path_preserves_non_utf8_home() {
        use std::os::unix::ffi::OsStringExt;

        let plan = crate::tests::read_only_plan("/fixture", Some("login"));
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let home = PathBuf::from(OsString::from_vec(b"/fixture/home-\xff".to_vec()));
        let mut input = crate::tests::fixture_input(
            &route,
            &plan,
            &prompt,
            std::path::Path::new("/fixture/repo"),
            None,
        );
        input.home = &home;
        let spec = prepare(
            &input,
            Access::WorktreeWrite,
            Session::Fresh,
            OpenCode.stream(None).unwrap(),
        )
        .unwrap();
        let path = spec.env.iter().find(|(key, _)| key == "PATH").unwrap();
        assert!(std::env::split_paths(&path.1).any(|entry| entry == home.join(".cargo/bin")));
    }

    #[test]
    fn writer_search_path_skips_home_with_path_separator() {
        let plan = crate::tests::read_only_plan("/fixture", Some("login"));
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        let home = std::path::Path::new("/fixture/home:with-separator");
        let mut input = crate::tests::fixture_input(
            &route,
            &plan,
            &prompt,
            std::path::Path::new("/fixture/repo"),
            None,
        );
        input.home = home;
        let spec = prepare(
            &input,
            Access::WorktreeWrite,
            Session::Fresh,
            OpenCode.stream(None).unwrap(),
        )
        .unwrap();
        let path = spec.env.iter().find(|(key, _)| key == "PATH").unwrap();
        assert!(!std::env::split_paths(&path.1).any(|entry| entry == home.join(".cargo/bin")));
    }

    #[test]
    fn git_discovery_exec_is_exact_in_both_access_modes() {
        let plan = crate::tests::read_only_plan("/fixture", Some("login"));
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
            // Read-only discovery enables OpenCode's cached search binary and system grep; a
            // writer's toolchain roots follow the system and package roots.
            let roots = &spec.profile.spec.tool_exec_roots;
            if access == Access::ReadOnly {
                assert_eq!(
                    roots,
                    &[
                        exact(
                            plan.permissions()
                                .private_state
                                .as_ref()
                                .unwrap()
                                .path
                                .join("cache/opencode/bin/rg")
                        ),
                        exact("/usr/bin/grep"),
                    ]
                );
            } else {
                assert_eq!(
                    roots[..3],
                    [
                        subtree("/bin"),
                        subtree("/usr/bin"),
                        subtree(crate::launch::package_root(input.executable).unwrap()),
                    ]
                );
            }
        }
    }

    #[test]
    fn git_discovery_preserves_writer_config_and_isolates_reader() {
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            let plan = crate::tests::fixture_access_plan(access, Some("login"), false);
            let spec = OpenCode
                .prepare(crate::tests::fixture_input(
                    &route,
                    &plan,
                    &prompt,
                    std::path::Path::new("/fixture/repo"),
                    None,
                ))
                .unwrap();
            let global = spec
                .env
                .iter()
                .find(|(name, _)| name == "GIT_CONFIG_GLOBAL");
            let expected = if access == Access::WorktreeWrite {
                "/fixture/scratch/gitconfig"
            } else {
                "/dev/null"
            };
            assert_eq!(
                global.map(|(_, value)| value.as_os_str()),
                Some(expected.as_ref())
            );
            assert_eq!(
                spec.env
                    .iter()
                    .any(|(name, _)| name == "GIT_CONFIG_NOSYSTEM"),
                access == Access::ReadOnly
            );
        }
    }

    /// A literal on the home's `.claude` or `.codex` would list their entries.
    #[test]
    fn seatbelt_profile_for_a_project_under_home_lists_no_home_probes() {
        let home = std::path::Path::new("/fixture/home");
        let plan = crate::tests::read_only_plan("/fixture/home", Some("login"));
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
        let cached = project_parent.join("state/cache/opencode/bin/rg");
        if access == Access::ReadOnly && std::path::Path::new("/opt/homebrew/bin/rg").is_file() {
            fs::create_dir_all(cached.parent().unwrap()).unwrap();
            fs::copy("/opt/homebrew/bin/rg", &cached).unwrap();
        }
        let plan = crate::tests::read_only_plan(project_parent.to_str().unwrap(), Some("login"));
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
    fn host_account_uses_xdg_fallback_while_session_data_is_private() {
        let plan = crate::tests::read_only_plan("/fixture", Some("login"));
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
        assert_eq!(env("XDG_CONFIG_HOME"), Some("/fixture/state/config".into()));
        assert_eq!(env("XDG_DATA_HOME"), Some("/fixture/state/data".into()));
        let auth = exact("/fixture/home/.local/share/opencode/auth.json");
        assert_eq!(spec.profile.spec.authentication[0].mechanism, "login");
        assert!(spec.profile.spec.authentication[0].roots.contains(&auth));
        assert!(spec.setup.contains(&SetupStep::Symlink {
            link: "/fixture/state/data/opencode/auth.json".into(),
            target: auth.path
        }));
        let lookup = |key: &str| match key {
            "XDG_DATA_HOME" => Some("/xdg/data".into()),
            _ => None,
        };
        let custom = PrepareInput {
            host_var: &lookup,
            ..input
        };
        let custom = prepare(
            &custom,
            Access::ReadOnly,
            Session::Fresh,
            OpenCode.stream(None).unwrap(),
        )
        .unwrap();
        assert!(
            custom.profile.spec.authentication[0]
                .roots
                .contains(&exact("/xdg/data/opencode/auth.json"))
        );
        assert!(custom.setup.contains(&SetupStep::Symlink {
            link: "/fixture/state/data/opencode/auth.json".into(),
            target: "/xdg/data/opencode/auth.json".into()
        }));
        assert!(
            spec.profile
                .spec
                .state_writes
                .iter()
                .all(|grant| grant.path.starts_with("/fixture/state/data/opencode"))
        );
    }
}
