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

/// Recognise provider quota errors and its rate-limit retry notice live.
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
        line.starts_with("AI.Error.QuotaExceeded:") || rate_limit_retry(line)
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

/// Confirm that the retained database contains the exact session identity.
/// OpenCode 2.x stores sessions in `session_v2`; a missing table or session is
/// an unknown identity.
pub fn session_exists(data: &std::path::Path, session: &str) -> Result<(), ProviderError> {
    use rusqlite::{Connection, OpenFlags};
    let db = Connection::open_with_flags(
        data.join("opencode.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| ProviderError::UnknownIdentity)?;
    db.busy_timeout(std::time::Duration::from_millis(250))
        .map_err(|_| ProviderError::UnknownIdentity)?;
    let found = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM session_v2 WHERE id=?)",
            [session],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|_| ProviderError::UnknownIdentity)?;
    if found {
        Ok(())
    } else {
        Err(ProviderError::UnknownIdentity)
    }
}

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
/// Native tools remain available while the outer profile grants their paths
/// and bounds filesystem and network access. Search tools run ripgrep, which
/// the profile grants by path.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    if plan.selected_authentication.as_deref() != Some("login") {
        return Err(input.boundary_error(
            "OPENCODE_PREPARE_CREDENTIALS",
            "OpenCode requires its selected login authentication",
            ProviderError::UnsupportedPolicy,
        ));
    }
    let private = crate::launch::private_state_or_scratch(
        plan.private_state
            .as_ref()
            .map(|state| state.path.as_path()),
        &plan.scratch.path,
        input.policy,
        "OPENCODE_PREPARE_NONE",
    )?;
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
        let continuation = input.continuation.ok_or_else(|| {
            input.boundary_error(
                "OPENCODE_PREPARE_AUTHORITY",
                "resumed OpenCode invocation has no validated continuation identity",
                ProviderError::UnknownIdentity,
            )
        })?;
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
    let permission = {
        // The provider's own prompt layer cannot answer in a headless lane.
        // The outer profile remains the source of path and network authority.
        let native = crate::launch::OPENCODE_NATIVE_PERMISSIONS;
        let mut permission = serde_json::Map::new();
        permission.insert(native.default_key.into(), native.default_value.into());
        let mut external = serde_json::Map::new();
        let mut reads = serde_json::Map::new();
        // The pinned cwd and candidate state are siblings. Headless tools must
        // not prompt for the private directory where progress is published.
        let progress = format!("{}/**", private.to_str().ok_or(ProviderError::InvalidPath)?);
        external.insert(progress.clone(), native.path_value.into());
        reads.insert(progress, native.path_value.into());
        if input.git_context {
            // Scratch is outside the workspace; headless runs cannot answer the
            // external-directory prompt. Keep the exception inside its existing grant.
            // https://opencode.ai/docs/permissions/#external-directories
            let scratch = format!(
                "{}/**",
                plan.scratch.path.to_str().ok_or_else(|| {
                    input.boundary_error(
                        "OPENCODE_PREPARE_AUTHORITY_2",
                        "Git-context scratch path cannot be represented by OpenCode",
                        ProviderError::InvalidPath,
                    )
                })?
            );
            external.insert(scratch.clone(), native.path_value.into());
            reads.insert(scratch, native.path_value.into());
        }
        if let Some(skills) = &user_skills {
            // Keep native paths usable even when the skills root is itself a link.
            let pattern = format!("{}/**", skills.display());
            external.insert(pattern.clone(), native.path_value.into());
            reads.insert(pattern, native.path_value.into());
        }
        for grant in &skill_reads {
            let pattern = format!("{}/**", grant.path.display());
            external.insert(pattern.clone(), native.path_value.into());
            reads.insert(pattern, native.path_value.into());
        }
        if access == Access::ReadOnly {
            for grant in &plan.read_roots {
                let pattern = match grant.scope {
                    provenant_confinement::permissions::PathScope::Exact => {
                        grant.path.display().to_string()
                    }
                    provenant_confinement::permissions::PathScope::Subtree => {
                        format!("{}/**", grant.path.display())
                    }
                };
                external.insert(pattern.clone(), native.path_value.into());
                reads.insert(pattern, native.path_value.into());
            }
        }
        if !external.is_empty() {
            permission.insert(native.external_directory_key.into(), external.into());
            permission.insert(native.read_key.into(), reads.into());
        }
        serde_json::Value::Object(permission)
    };
    let config = serde_json::json!({
        "autoupdate": false,
        "share": "disabled",
        "mcp": {},
        "permission": permission,
    })
    .to_string();
    let package = package_grant(input.executable, &mut omitted_discovery, input.policy)?;
    // Bypass Apple's Git shim, including when the selected binary is Homebrew's.
    let bin = plan.scratch.path.join("bin");
    let writer = access == Access::WorktreeWrite;
    let defaults = crate::launch::tool_search_path(input.home, true, input.policy)?;
    let path = match std::env::join_paths(
        std::iter::once(bin.clone()).chain(std::env::split_paths(&defaults)),
    ) {
        Ok(path) => path,
        Err(_) if !writer => {
            crate::launch::policy_warning(
                input.policy,
                "OPENCODE_PREPARE_NONE_2",
                "the optional read-only Git launcher cannot be encoded in PATH; the selected Git remains explicitly granted",
            )?;
            defaults
        }
        Err(_) => {
            return Err(input.policy.boundary_error(
                "OPENCODE_PREPARE_GIT_ISOLATION",
                provenant_records::policy::Facts::violation(
                    "the private Git launcher cannot be placed first in PATH",
                ),
                ProviderError::InvalidPath,
            ));
        }
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
            &input.credential_roots,
            input.policy,
            crate::launch::OmissionSource::Toolchain,
            &mut omitted_discovery,
        )?;
    }
    // Preserve ordinary provider configuration without importing user plugins
    // or skills into the lane. Do not follow unrelated config links.
    let config_dir = config_home.join("opencode");
    let optional_start = runtime_reads.len();
    if let Ok(config_dir) = std::fs::canonicalize(&config_dir)
        && !provenant_confinement::refuses_lane_root(&config_dir, true, &input.credential_roots)
        && let Ok(entries) = std::fs::read_dir(&config_dir)
    {
        runtime_reads.push(exact(&config_dir));
        for entry in entries.flatten() {
            if matches!(entry.file_name().to_str(), Some("skills" | "plugins")) {
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
        && provenant_confinement::refuses_lane_root(&config_dir, true, &input.credential_roots)
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
        &input.credential_roots,
        input.policy,
        crate::launch::OmissionSource::ProviderConfig,
        &mut omitted_discovery,
    )?;
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
    // Mirror ordinary provider settings as individual links. Lane-owned
    // instructions, hooks and skills use their own private paths.
    let private_config = private.join("config");
    {
        let private_opencode = private_config.join("opencode");
        setup.push(SetupStep::CreateDir(private_opencode.clone()));
        if let Ok(canonical) = std::fs::canonicalize(&config_dir)
            && !provenant_confinement::refuses_lane_root(&canonical, true, &input.credential_roots)
            && let Ok(entries) = std::fs::read_dir(&config_dir)
        {
            for entry in entries.flatten() {
                if !matches!(entry.file_name().to_str(), Some("skills" | "plugins"))
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
    // Shell tools run under the outer lane profile. Its write grants and
    // protected checkouts, not OpenCode's prompt config, set write authority.
    let mut tool_exec_roots = vec![subtree("/bin"), subtree("/usr/bin"), package.clone()];
    if access == Access::ReadOnly {
        // OpenCode's grep, glob and list tools execute ripgrep. It resolves
        // `rg` from PATH, then from its cache bin ($XDG_CACHE_HOME/opencode/bin),
        // and installs it there when absent. The lane sets XDG_CACHE_HOME to
        // its private state, whose grant already covers the read, so grant
        // exactly that binary for execution, alongside system grep.
        let rg = private.join("cache/opencode/bin/rg");
        // Resolve install-name symlinks to exact files, accepting libraries by
        // content regardless of the host's package manager or PATH ordering.
        tool_exec_roots.push(exact(rg));
        tool_exec_roots.push(exact("/usr/bin/grep"));
    }
    // Native environments already contain host selectors. Replace them so the
    // child cannot observe an earlier duplicate instead of its private state.
    env.retain(|(key, _)| {
        ![
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
        ]
        .iter()
        .any(|name| key == *name)
    });
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
    env.push(("OPENCODE_CONFIG_CONTENT".into(), config.into()));
    // No repository configuration or Claude Code files. Provenant's global
    // plugin is loaded from the private OpenCode config home.
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
        tool_requirements: Default::default(),
        git_wrapper: Default::default(),
        policy: input.policy.clone(),
        credential_roots: input.credential_roots.clone(),
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
    crate::launch::prepare_tool_requirements(input, &mut spec)?;
    Ok(spec)
}

/// The server's own cause for a failed HTTP request, from its log lines
/// (`... cause="..." ... http.status=500`). Only a bounded, printable cause on
/// a 5xx line is kept; no other stderr prose is copied.
const SERVER_FAILURE_CAUSE_LIMIT: usize = 240;

#[derive(Debug, PartialEq, Eq)]
struct ServerFailureDiagnostic {
    status: u16,
    cause: String,
}

fn parse_failure_diagnostic(stderr: &[u8]) -> Option<ServerFailureDiagnostic> {
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
        let cause =
            provenant_confinement::redact::bounded_line(&printable, SERVER_FAILURE_CAUSE_LIMIT);
        let status = digits.parse().ok()?;
        (!cause.is_empty()).then_some(ServerFailureDiagnostic { status, cause })
    })
}

fn failure_diagnostic(stderr: &[u8]) -> Option<String> {
    let failure = parse_failure_diagnostic(stderr)?;
    Some(format!(
        "opencode server failed with HTTP {}: {}",
        failure.status, failure.cause
    ))
}

/// Parse the error envelope, projecting identifiers and the requested route,
/// never arbitrary provider messages or response bodies.
pub fn stdout_error_diagnostic(
    stdout: &[u8],
    model: Option<&str>,
    variant: Option<&str>,
) -> Option<String> {
    fn identifier(value: Option<&str>) -> &str {
        value
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 64
                    && s.bytes().all(|b| {
                        b.is_ascii_lowercase()
                            || b.is_ascii_digit()
                            || matches!(b, b'.' | b'_' | b'-')
                    })
            })
            .unwrap_or("unknown")
    }
    fn selector(value: Option<&str>) -> &str {
        value
            .filter(|text| {
                !text.is_empty()
                    && text.len() <= 128
                    && text.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
                    })
            })
            .unwrap_or("unknown")
    }
    stdout.split(|b| *b == b'\n').find_map(|line| {
        let value: serde_json::Value = serde_json::from_slice(line).ok()?;
        if value.get("type")?.as_str()? != "error" {
            return None;
        }
        let error = value.get("error")?;
        let body = error
            .pointer("/response/body")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .or_else(|| error.pointer("/response/body").cloned());
        let kind = identifier(error.get("type").and_then(|v| v.as_str()));
        let code = identifier(
            body.as_ref()
                .and_then(|v| v.pointer("/error/code"))
                .and_then(|v| v.as_str()),
        );
        let status = error
            .get("status")
            .and_then(|v| v.as_u64())
            .filter(|n| (100..=599).contains(n))
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unknown".into());
        let invalid_reference = [
            error.get("message"), error.pointer("/data/message"),
            body.as_ref().and_then(|body| body.pointer("/error/message")),
            error.get("type"),
        ].into_iter().flatten().filter_map(serde_json::Value::as_str).any(|message| {
            let message = message.to_ascii_lowercase();
            message.contains("invalid model reference")
                || message.contains("unsupported variant")
                || message.contains("unknown variant")
                || message.contains("modelnotfound")
                || message.contains("variantnotfound")
                || ((message.contains("variant") || message.contains("effort"))
                    && (message.contains("not supported") || message.contains("not found")))
        });
        let guidance = if invalid_reference { "; choose a supported model and variant (check the route's effort)" } else { "" };
        Some(provenant_confinement::redact::bounded_line(
            &format!("opencode provider error: model={}, variant={}, type={kind}, status={status}, code={code}{guidance}", selector(model), selector(variant.or(Some("default")))),
            640,
        ))
    })
}

fn rate_limit_retry(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("rate limit exceeded") && lower.contains("retrying")
}

/// Recognise OpenCode's unstructured quota errors without treating arbitrary
/// provider output as protocol events. Pool and retry evidence come only from
/// the matched line and the one after it, so unrelated stderr cannot steer them.
pub fn stderr_quota(stderr: &[u8]) -> Option<E> {
    let text = std::str::from_utf8(stderr).ok()?;
    let mut lines = text.lines();
    let matched = lines.find(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("ai.error.quotaexceeded")
            || lower.contains("usage limit exceeded")
            || rate_limit_retry(line)
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
    // Provider HTTP errors may report only a code; absence is current wire data.
    #[serde(default, rename = "type")]
    kind: String,
    message: Option<String>,
    #[serde(default)]
    code: Option<String>,
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
            let body = error.response.as_ref().and_then(|response| match response {
                Response::Body {
                    body: Some(Body::Json(body)),
                } => body.error.code.clone(),
                Response::Body {
                    body: Some(Body::Text(text)),
                } => wire::<ErrorBody>(text.as_bytes())
                    .ok()
                    .and_then(|body| body.error.code),
                _ => None,
            });
            let auth = error.kind == "provider.auth"
                || matches!(error.status, Some(401 | 403))
                || body.as_deref() == Some("api_key_expired");
            let limited = error.status == Some(429)
                || error.message.as_deref().is_some_and(|message| {
                    message
                        .to_ascii_lowercase()
                        .contains("insufficient balance")
                })
                || body.as_deref().is_some_and(|code| {
                    matches!(
                        code,
                        "insufficient_balance" | "insufficient_quota" | "rate_limit_exceeded"
                    )
                });
            let event = if auth {
                E::AuthRequired
            } else if error.kind == "provider.quota"
                || error.kind == "provider.free-limit"
                || limited
            {
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

crate::quota_source!(
    OpenCode,
    "opencode",
    crate::quota_live::opencode::capture_source,
    crate::quota::opencode::decode_source
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Adapter;

    #[test]
    fn structured_errors_classify_auth_and_quota() {
        for (error, auth) in [
            (serde_json::json!({"type":"provider.auth"}), true),
            (serde_json::json!({"type":"unknown", "status":401}), true),
            (serde_json::json!({"type":"unknown", "status":403}), true),
            (
                serde_json::json!({"type":"unknown", "response":{"body":"{\"error\":{\"code\":\"api_key_expired\"}}"}}),
                true,
            ),
            (serde_json::json!({"type":"unknown", "status":429}), false),
            (
                serde_json::json!({"type":"unknown", "response":{"body":{"error":{"code":"insufficient_balance"}}}}),
                false,
            ),
        ] {
            let event = serde_json::json!({"type":"error", "error":error}).to_string();
            let events = decode(event.as_bytes()).unwrap();
            assert!(events.iter().any(|event| if auth {
                matches!(event, E::AuthRequired)
            } else {
                matches!(event, E::QuotaLimited { .. })
            }));
        }
    }

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
        for line in [
            "Rate limit exceeded, retrying in 5 seconds",
            "Rate limit exceeded, retrying in 10 seconds",
        ] {
            assert_eq!(
                decoder.decode(line.as_bytes()).unwrap(),
                FrameEvent::Terminal
            );
            assert!(matches!(
                stderr_quota(line.as_bytes()),
                Some(E::QuotaLimited { .. })
            ));
        }
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

    #[derive(Clone, Copy)]
    enum ModelSource {
        Stream(&'static [u8]),
        SessionV2,
    }

    struct ModelCase {
        session: &'static str,
        model: &'static str,
        expected: &'static str,
        source: ModelSource,
        live_wal: bool,
    }

    /// Seed only the current session and assistant-turn tables.
    fn seed_v2_only(dir: &std::path::Path, session: &str, provider: &str, model: &str) {
        let db = rusqlite::Connection::open(dir.join("opencode.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_v2 (id TEXT PRIMARY KEY); \
             CREATE TABLE IF NOT EXISTS session_message (session_id TEXT, type TEXT, data TEXT)",
        )
        .unwrap();
        db.execute("INSERT INTO session_v2 VALUES (?)", [session])
            .unwrap();
        db.execute(
            "INSERT INTO session_message VALUES (?, 'assistant', ?)",
            (
                session,
                format!(
                    r#"{{"model":{{"id":"{model}","providerID":"{provider}","variant":"default"}}}}"#
                ),
            ),
        )
        .unwrap();
    }

    #[test]
    fn model_observation_follows_each_session_source() {
        let cases = [
            ModelCase {
                session: "ses_x",
                model: r#"{"model":{"id":"beam-free","providerID":"opencode-go"},"finish":"stop"}"#,
                expected: "opencode/opencode-go/beam-free",
                source: ModelSource::Stream(include_bytes!(
                    "../../../tests/fixtures/providers/opencode/v2.2.0-success.jsonl"
                )),
                live_wal: true,
            },
            ModelCase {
                session: "FIXTURE_SESSION_IDENTIFIER_007",
                model: r#"{"model":{"id":"glm-5.3-flash-free","providerID":"opencode"}}"#,
                expected: "opencode/opencode/glm-5.3-flash-free",
                source: ModelSource::Stream(include_bytes!(
                    "../../../tests/fixtures/providers/opencode/unversioned-success.jsonl"
                )),
                live_wal: false,
            },
            ModelCase {
                session: "ses_zen",
                model: r#"{"model":{"id":"deepseek-v4.1-flash-free","providerID":"opencode","variant":"default"}}"#,
                expected: "opencode/opencode/deepseek-v4.1-flash-free",
                source: ModelSource::SessionV2,
                live_wal: false,
            },
        ];

        for case in cases {
            let dir = tempfile::tempdir().unwrap();
            let db = rusqlite::Connection::open(dir.path().join("opencode.db")).unwrap();
            if case.live_wal {
                db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
                    .unwrap();
            }
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS session_message (session_id TEXT, type TEXT, data TEXT)",
            )
            .unwrap();
            if matches!(case.source, ModelSource::SessionV2) {
                db.execute_batch("CREATE TABLE session_v2 (id TEXT PRIMARY KEY)")
                    .unwrap();
                db.execute("INSERT INTO session_v2 VALUES (?)", [case.session])
                    .unwrap();
            }
            db.execute(
                "INSERT INTO session_message VALUES (?, 'assistant', ?)",
                (case.session, case.model),
            )
            .unwrap();
            if case.live_wal {
                assert!(dir.path().join("opencode.db-wal").is_file());
            }

            let observed_session = match case.source {
                ModelSource::Stream(bytes) => {
                    let mut stream = OpenCode.stream(None).unwrap();
                    for chunk in bytes.chunks(17) {
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
                    outcome.session().unwrap().to_owned()
                }
                ModelSource::SessionV2 => {
                    session_exists(dir.path(), case.session).unwrap();
                    case.session.to_owned()
                }
            };
            assert_eq!(observed_session, case.session);
            assert_eq!(
                observe_model(dir.path(), &observed_session).unwrap(),
                case.expected
            );
        }
    }

    #[test]
    fn model_observation_rejects_unknown_sessions_and_conflicting_models() {
        let dir = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(dir.path().join("opencode.db")).unwrap();
        db.execute_batch("CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT)")
            .unwrap();
        db.execute(
            "INSERT INTO session_message VALUES (?, 'assistant', ?)",
            (
                "fixture-session",
                r#"{"model":{"id":"glm-5.3-flash-free","providerID":"opencode"}}"#,
            ),
        )
        .unwrap();
        assert!(matches!(
            observe_model(dir.path(), "unknown-session"),
            Err(ProviderError::UnknownIdentity)
        ));
        db.execute(
            "INSERT INTO session_message VALUES (?, 'assistant', ?)",
            (
                "fixture-session",
                r#"{"model":{"id":"other","providerID":"opencode"}}"#,
            ),
        )
        .unwrap();
        assert!(matches!(
            observe_model(dir.path(), "fixture-session"),
            Err(ProviderError::IdentityMismatch)
        ));
    }

    #[test]
    fn go_session_recorded_only_in_session_v2_names_the_go_provider() {
        let dir = tempfile::tempdir().unwrap();
        seed_v2_only(dir.path(), "ses_go", "opencode-go", "mimo-v2.6-flash");
        session_exists(dir.path(), "ses_go").unwrap();
        // The route requested a Go model: the session must name the Go provider,
        // so a session that ran on the free provider cannot stand in for it.
        assert_eq!(
            observe_model(dir.path(), "ses_go").unwrap(),
            "opencode/opencode-go/mimo-v2.6-flash"
        );
        seed_v2_only(dir.path(), "ses_free", "opencode", "mimo-v2.6-flash");
        assert_ne!(
            observe_model(dir.path(), "ses_free").unwrap(),
            observe_model(dir.path(), "ses_go").unwrap()
        );
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
    fn completion_status_follows_answer_stop_and_exit_state() {
        use crate::{ExitObservation, OutcomeStatus};

        let stop =
            "{\"type\":\"step_finish\",\"sessionID\":\"fixture\",\"part\":{\"reason\":\"stop\"}}\n";
        let answerless = FINAL_TEXT_WITHOUT_FINISH
            .rsplit_once("{\"type\":\"text\"")
            .unwrap()
            .0;
        for (answer, stop_present, code, timed_out, expected) in [
            (true, false, Some(0), false, OutcomeStatus::Success),
            (true, true, Some(0), false, OutcomeStatus::Success),
            (false, false, Some(0), false, OutcomeStatus::Partial),
            (false, true, Some(0), false, OutcomeStatus::Success),
            (true, false, Some(1), false, OutcomeStatus::Failed),
            (true, false, None, false, OutcomeStatus::Failed),
            (true, false, Some(0), true, OutcomeStatus::Partial),
        ] {
            let mut stream = OpenCode.stream(None).unwrap();
            let mut events = Vec::new();
            let raw = if answer {
                FINAL_TEXT_WITHOUT_FINISH
            } else {
                answerless
            };
            let mut input = raw.to_owned();
            if stop_present {
                input.push_str(stop);
            }
            for chunk in input.as_bytes().chunks(17) {
                events.extend(OpenCode.observe(&mut stream, chunk).unwrap());
            }
            assert_eq!(events.contains(&E::Text("Provenant".into())), answer);
            let outcome = OpenCode
                .finish(stream, ExitObservation { code, timed_out })
                .unwrap();
            assert_eq!(outcome.status(), expected, "{answer}, {stop_present}");
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
        let spec = OpenCode.prepare(input).unwrap();
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
    fn read_only_tools_keep_shell_and_declared_reads_under_outer_confinement() {
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
        permissions.read_roots.push(PathGrant {
            path: "/fixture/shared-read".into(),
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
        let spec = OpenCode
            .prepare(crate::tests::fixture_input(
                &route,
                &plan,
                &prompt,
                std::path::Path::new("/fixture/repo"),
                None,
            ))
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
        let native = crate::launch::OPENCODE_NATIVE_PERMISSIONS;
        assert_eq!(
            config["permission"][native.default_key],
            native.default_value
        );
        let declared_pattern = "/fixture/shared-read/**";
        assert_eq!(
            config["permission"][native.external_directory_key][declared_pattern],
            native.path_value
        );
        assert_eq!(
            config["permission"][native.read_key][declared_pattern],
            native.path_value
        );

        // OpenCode resolves ripgrep from PATH and then its cache bin, which is
        // XDG_CACHE_HOME/opencode/bin under the lane's private state.
        let rg = exact(std::path::Path::new(
            "/fixture/provider-state/cache/opencode/bin/rg",
        ));
        assert_eq!(
            spec.profile.spec.process_executables,
            [exact(input.executable), exact(input.provenant.git)]
        );
        assert!(spec.profile.spec.tool_exec_roots.contains(&rg.clone()));
        assert!(
            spec.profile
                .spec
                .tool_exec_roots
                .contains(&exact("/usr/bin/grep"))
        );
        // Shell and toolchain access stay available; the outer profile still
        // blocks writes to the read-only worktree.
        let path: Vec<_> = std::env::split_paths(&env("PATH").expect("PATH")).collect();
        assert!(path.contains(&input.home.join(".cargo/bin")), "{path:?}");
        assert!(
            spec.profile
                .spec
                .tool_exec_roots
                .contains(&subtree(input.home.join(".cargo/bin")))
        );

        let profile = provenant_confinement::permissions::snapshot::render(
            plan.permissions(),
            &spec.profile,
            input.home,
        )
        .unwrap();
        let exec = format!("(allow process-exec (literal \"{}\"))", rg.path.display());
        assert!(profile.contains(&exec), "{exec}");
        assert!(
            provenant_confinement::write_grants(plan.permissions(), &spec.profile)
                .iter()
                .all(|grant| !grant.path.starts_with(input.cwd))
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
        #[derive(Clone, Copy)]
        enum Coverage {
            Library,
            AlreadyCovered,
            CredentialOmitted,
            Ineligible,
        }
        struct DependencyFixture {
            path: PathBuf,
            coverage: Coverage,
        }

        let home_text = home.join("Documents/plain.txt");
        let host_text = root.join("host/Cellar/plain.txt");
        let home_library = home.join("fixture.dylib");
        let state_library = lane.join("state/fixture.dylib");
        let scratch_library = lane.join("scratch/fixture.dylib");
        let worktree_library = cwd.join("fixture.dylib");
        let credential_library = root.join("host/.ssh/fixture.dylib");
        let executable = root.join("host/executable");
        let directory = root.join("host/directory");
        // Already-covered images still contribute their own dependencies.
        let hidden = root.join("host/hidden.dylib");
        image(&hidden, 6, &[])?;
        for path in [
            &home_text,
            &host_text,
            &home_library,
            &state_library,
            &scratch_library,
            &worktree_library,
            &credential_library,
            &executable,
            &directory,
        ] {
            image(path, 6, &[&hidden])?;
        }
        std::fs::write(&home_text, "plain text")?;
        std::fs::write(&host_text, "plain text")?;
        image(&executable, 2, &[&hidden])?;
        std::fs::remove_file(&directory)?;
        std::fs::create_dir(&directory)?;
        let cached = lane.join("state/cache/opencode/bin/rg");
        let mut dependencies = vec![alias.as_path()];
        dependencies.extend(
            [
                &home_text,
                &host_text,
                &home_library,
                &state_library,
                &scratch_library,
                &worktree_library,
                &credential_library,
                &executable,
                &directory,
            ]
            .iter()
            .map(|path| path.as_path()),
        );
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
        // Credential stores are declared paths; the host's store is declared
        // here as host configuration would declare it.
        let mut credential_roots = provenant_core::CredentialRoots::for_home(&home);
        credential_roots.add(
            root.join("host/.ssh"),
            provenant_core::CredentialScope::Subtree,
            "fixture host declaration",
            provenant_core::CredentialKind::Store,
        );
        input.credential_roots = credential_roots;
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
        let denied = [
            home_text.clone(),
            host_text.clone(),
            home_library.clone(),
            state_library.clone(),
            scratch_library.clone(),
            worktree_library.clone(),
            credential_library.clone(),
            executable.clone(),
            directory.clone(),
        ];
        let dependency_fixtures = [
            DependencyFixture {
                path: first.clone(),
                coverage: Coverage::Library,
            },
            DependencyFixture {
                path: second.clone(),
                coverage: Coverage::Library,
            },
            DependencyFixture {
                path: home_library,
                coverage: Coverage::Library,
            },
            DependencyFixture {
                path: hidden.clone(),
                coverage: Coverage::Library,
            },
            DependencyFixture {
                path: state_library,
                coverage: Coverage::AlreadyCovered,
            },
            DependencyFixture {
                path: scratch_library,
                coverage: Coverage::AlreadyCovered,
            },
            DependencyFixture {
                path: worktree_library,
                coverage: Coverage::AlreadyCovered,
            },
            DependencyFixture {
                path: credential_library,
                coverage: Coverage::CredentialOmitted,
            },
            DependencyFixture {
                path: home_text,
                coverage: Coverage::Ineligible,
            },
            DependencyFixture {
                path: host_text,
                coverage: Coverage::Ineligible,
            },
            DependencyFixture {
                path: executable,
                coverage: Coverage::Ineligible,
            },
            DependencyFixture {
                path: directory,
                coverage: Coverage::Ineligible,
            },
        ];
        for fixture in dependency_fixtures {
            let libraries = tool.dependency_closure.libraries.contains(&fixture.path);
            let covered = tool
                .dependency_closure
                .already_covered
                .contains(&fixture.path);
            let omitted = tool.dependency_closure.omitted.contains(&fixture.path);
            match fixture.coverage {
                Coverage::Library => {
                    assert!(libraries, "{}", fixture.path.display());
                    assert_eq!(std::fs::canonicalize(&fixture.path)?, fixture.path);
                    assert!(spec.profile.spec.runtime_reads.iter().all(|grant| {
                        grant.scope != provenant_confinement::permissions::PathScope::Subtree
                            || !fixture.path.starts_with(&grant.path)
                    }));
                    assert!(
                        provenant_confinement::write_grants(plan.permissions(), &spec.profile)
                            .iter()
                            .all(|grant| !fixture.path.starts_with(&grant.path)
                                && !grant.path.starts_with(&fixture.path))
                    );
                }
                Coverage::AlreadyCovered => assert!(covered, "{}", fixture.path.display()),
                Coverage::CredentialOmitted => {
                    assert!(omitted, "{}", fixture.path.display());
                    assert!(tool.dependency_closure.reasons.iter().any(|reason| matches!(
                        reason,
                        crate::IncompleteReason::OmittedCredential(path) if path == &fixture.path
                    )));
                }
                Coverage::Ineligible => assert!(!libraries && !covered && !omitted),
            }
        }
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
        for exec in profile
            .lines()
            .filter_map(|line| line.strip_prefix("(allow process-exec (subpath \""))
            .filter_map(|line| line.strip_suffix("\"))"))
        {
            for lane_path in [
                &cwd,
                &home,
                &plan.permissions().scratch.path,
                &plan
                    .permissions()
                    .private_state
                    .as_ref()
                    .ok_or("private state")?
                    .path,
            ] {
                assert!(
                    !Path::new(exec).starts_with(lane_path),
                    "lane exec subtree: {exec}"
                );
            }
        }
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
        // `repo` is the read root the renderer takes as the lane's source and
        // `other-member` a protected checkout, the two roots a read-only
        // shell must not write.
        let plan = crate::tests::read_only_plan_protecting(
            root.to_str().unwrap(),
            Some("login"),
            &["other-member"],
        );
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
        // The same launch must support the brief's temporary-file + rename
        // protocol while the shell it grants cannot write the source or a
        // protected checkout. The profile is `allow default`; these refusals
        // come from the read-only source and protected-root denials in
        // `provenant_confinement::permissions::render::push_boundaries`, not
        // from OpenCode's permission config.
        let private = &plan.permissions().private_state.as_ref().unwrap().path;
        std::fs::create_dir_all(private)?;
        let source = root.join("repo");
        let protected = root.join("other-member");
        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(&protected)?;
        let output = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "/bin/sh", "-c"])
            .arg(r#"set -eu
printf '%s' '{"schema_version":2,"status":"done","note":"progress","remaining":[],"decisions":[],"verify":[]}' > "$1/handoff.tmp"
/bin/mv "$1/handoff.tmp" "$1/handoff.json"
if (printf forbidden > "$2/forbidden") 2>/dev/null; then exit 91; fi
if (printf forbidden > "$3/forbidden") 2>/dev/null; then exit 92; fi
if /bin/mv "$1/handoff.json" "$2/handoff.json" 2>/dev/null; then exit 93; fi
"#)
            .arg("handoff-probe")
            .arg(private)
            .arg(&source)
            .arg(&protected)
            .output()?;
        assert!(
            output.status.success(),
            "probe exit {:?} (91: source written, 92: protected checkout written, \
             93: handoff moved into source): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        let progress: serde_json::Value =
            serde_json::from_slice(&std::fs::read(private.join("handoff.json"))?)?;
        assert_eq!(progress["status"], "done");
        assert!(!private.join("handoff.tmp").exists());
        assert!(!source.join("forbidden").exists());
        assert!(!source.join("handoff.json").exists());
        assert!(!protected.join("forbidden").exists());
        Ok(())
    }

    #[test]
    fn server_http_failure_diagnostic_carries_its_typed_cause_and_status() {
        let log = concat!(
            "timestamp=t level=INFO run=a message=\"watcher stopped\" path=/x http.span=1\n",
            "timestamp=t level=INFO run=a cause=\"Error: EPERM: operation not permitted, ",
            "mkdir '/h/.local/share/opencode/shell/abc'\\n    at ServerProcess.start\" ",
            "http.span=68 role=server http.method=GET http.url=/api/location http.status=500\n",
        );
        let failure = parse_failure_diagnostic(log.as_bytes()).unwrap();
        assert_eq!(failure.status, 500);
        assert!(failure.cause.contains("EPERM"));
        assert!(failure.cause.contains("mkdir"));
        let rendered = failure_diagnostic(log.as_bytes()).unwrap();
        assert!(rendered.contains(&failure.status.to_string()));
        assert!(rendered.contains(&failure.cause));
        let long = format!(
            "level=INFO cause=\"{}\" http.status=503\n",
            "x".repeat(1000)
        );
        let diagnostic = parse_failure_diagnostic(long.as_bytes()).unwrap();
        assert_eq!(diagnostic.status, 503);
        assert!(diagnostic.cause.chars().count() <= SERVER_FAILURE_CAUSE_LIMIT);
        let secret =
            "level=INFO cause=\"fetch https://user:secret@example.invalid/x\" http.status=500\n";
        let redacted = parse_failure_diagnostic(secret.as_bytes()).unwrap();
        assert!(!redacted.cause.contains("secret"));
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
    fn discovery_reads_are_exact_below_the_home_for_each_cwd_shape() {
        let cases = [
            ("/fixture", "/fixture/repo", &["/fixture/repo"][..]),
            (
                "/fixture/home",
                "/fixture/home/repo/crate",
                &["/fixture/home/repo/crate", "/fixture/home/repo"][..],
            ),
        ];
        for (root, cwd, expected_parents) in cases {
            let plan = crate::tests::read_only_plan(root, Some("login"));
            let route = crate::tests::route("opencode", "fixture-model");
            let prompt = crate::Prompt::new("fixture".into());
            let cwd = std::path::Path::new(cwd);
            let input = crate::tests::fixture_input(&route, &plan, &prompt, cwd, None);
            let spec = prepare(
                &input,
                Access::ReadOnly,
                Session::Fresh,
                OpenCode.stream(None).unwrap(),
            )
            .unwrap();
            let expected = expected_parents
                .iter()
                .flat_map(|parent| {
                    [".agents", ".claude", ".codex"]
                        .map(|name| exact(PathBuf::from(*parent).join(name)))
                })
                .collect::<Vec<_>>();
            let mut actual = spec.profile.spec.discovery_reads;
            actual.sort_by(|left, right| left.path.cmp(&right.path));
            let mut expected = expected;
            expected.sort_by(|left, right| left.path.cmp(&right.path));
            assert_eq!(actual, expected);
            assert!(
                actual.iter().all(
                    |grant| grant.scope == provenant_confinement::permissions::PathScope::Exact
                )
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_only_search_path_skips_unencodable_git_shim_directory() {
        use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
        let base = crate::tests::read_only_plan("/fixture", Some("login"));
        let scratch = tempfile::tempdir().unwrap();
        let scratch_path = scratch.path().canonicalize().unwrap().join("scratch:other");
        std::fs::create_dir_all(scratch_path.join("bin")).unwrap();
        let mut permissions = base.permissions().clone();
        permissions.scratch.path = scratch_path;
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
        let defaults = crate::launch::tool_search_path(input.home, true, input.policy).unwrap();
        assert_eq!(path, &defaults);
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
        input.credential_roots = provenant_core::CredentialRoots::for_home(&home);
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
        input.credential_roots = provenant_core::CredentialRoots::for_home(home);
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
        let route = crate::tests::route("opencode", "fixture-model");
        let prompt = crate::Prompt::new("fixture".into());
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            let plan = crate::tests::fixture_access_plan(access, Some("login"), false);
            let input = crate::tests::fixture_input(
                &route,
                &plan,
                &prompt,
                std::path::Path::new("/fixture/repo"),
                None,
            );
            let git = input.provenant.git;
            let git_path = git.to_owned();
            let cwd = input.cwd;
            let home = input.home;
            let spec = OpenCode.prepare(input).unwrap();
            let profile = provenant_confinement::permissions::snapshot::render(
                plan.permissions(),
                &spec.profile,
                home,
            )
            .unwrap();
            let git = git.display();
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
                target: git_path,
            }));
            let roots = &spec.profile.spec.tool_exec_roots;
            for required in [
                subtree("/bin"),
                subtree("/usr/bin"),
                subtree(home.join(".cargo/bin")),
                subtree(cwd),
            ] {
                assert!(roots.contains(&required), "{access:?} lacks {required:?}");
            }
            if access == Access::ReadOnly {
                for required in [
                    exact(
                        plan.permissions()
                            .private_state
                            .as_ref()
                            .unwrap()
                            .path
                            .join("cache/opencode/bin/rg"),
                    ),
                    exact("/usr/bin/grep"),
                ] {
                    assert!(roots.contains(&required), "{access:?} lacks {required:?}");
                }
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

    // Anchors are real host paths; on Linux `/bin` links to `/usr/bin`, and
    // only the macOS backend confines OpenCode.
    #[cfg(target_os = "macos")]
    #[test]
    fn homebrew_launch_plan_anchors_with_discovery_directories_for_each_access_mode() {
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            homebrew_discovery_fixture(access);
        }
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
            credential_roots: provenant_core::CredentialRoots::for_home(&home),
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
            credential_roots: {
                let mut roots = provenant_core::CredentialRoots::for_home(std::path::Path::new(
                    "/fixture/home",
                ));
                roots.add(
                    PathBuf::from("/xdg/config/opencode"),
                    provenant_core::CredentialScope::Subtree,
                    "provider declaration: opencode XDG config",
                    provenant_core::CredentialKind::Store,
                );
                roots.add(
                    PathBuf::from("/xdg/data/opencode"),
                    provenant_core::CredentialScope::Subtree,
                    "provider declaration: opencode XDG data",
                    provenant_core::CredentialKind::Store,
                );
                roots
            },
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
