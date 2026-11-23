use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, package_grant, subtree,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use provenant_records::{Cost, CostOrigin, Tokens};
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
                // A retry past the adapter's bound emits one of these, so a
                // provider loop cannot hold the lane to its wall timeout.
                events.iter().any(|event| {
                    matches!(event, E::QuotaLimited { .. } | E::ModelOverloaded { .. })
                })
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
    reconcile_account: None,
    model_offers_sink: None,
};

crate::adapter!(OpenCode, "opencode", prepare, failure_diagnostic);
/// The JSON run stream carries no answering-model position.
const MODEL_ON_WIRE: bool = false;

/// Retry bounds declared by the OpenCode adapter. A 429 carries a reset
/// instant and never recovers inside one attempt, so one retry is already past
/// the bound; a 5xx retry loop is bounded by the ten attempts OpenCode itself
/// makes before it stops (interface study, 20 Nov 2026). These are the only
/// source of the bound: no caller guesses it from a provider name or a report.
/// Spec: docs/specs/routing.md#capacity-is-not-quota
const RATE_LIMIT_RETRY_BOUND: u64 = 1;
const CAPACITY_RETRY_BOUND: u64 = 10;

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

/// Resolve the XDG homes used by an interactive OpenCode process.
pub fn host_xdg_homes(
    home: &std::path::Path,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> [PathBuf; 4] {
    [
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_DATA_HOME", ".local/share"),
        ("XDG_STATE_HOME", ".local/state"),
        ("XDG_CACHE_HOME", ".cache"),
    ]
    .map(|(key, fallback)| {
        lookup(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(fallback))
    })
}

fn service_reachable(path: &std::path::Path) -> bool {
    let Some(url) = std::fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
        .and_then(|v| v.get("url")?.as_str().map(str::to_owned))
    else {
        return false;
    };
    let Some(authority) = url
        .strip_prefix("http://")
        .and_then(|s| s.split('/').next())
    else {
        return false;
    };
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    let Ok(port) = port.parse::<u16>() else {
        return false;
    };
    if host != "127.0.0.1" && host != "localhost" {
        return false;
    }
    use std::io::{Read, Write};
    let timeout = std::time::Duration::from_millis(250);
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        timeout,
    ) else {
        return false;
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
        || stream
            .write_all(b"GET /global/health HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .is_err()
    {
        return false;
    }
    let mut response = [0u8; 128];
    stream.read(&mut response).is_ok_and(|n| {
        let line = &response[..n];
        line.starts_with(b"HTTP/1.1 200 ")
            || line.starts_with(b"HTTP/1.0 200 ")
            || line.starts_with(b"HTTP/1.1 401 ")
            || line.starts_with(b"HTTP/1.0 401 ")
    })
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
    // Resolve the lane access once. An isolated lane refuses a provider config
    // directory that overlaps a credential root; a host lane includes it and
    // reports the overlap through the optional-grant filter below.
    let lane_access = plan
        .policy
        .as_ref()
        .map(|policy| policy.mandatory().lane_access());
    let config_isolated = lane_access.is_none_or(|receipt| receipt.access.is_isolated());
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
    // OpenCode's profile follows lane access: host access uses the host XDG
    // profile and service; isolated access uses a private per-attempt profile.
    let host_access = !config_isolated;
    let mut argv: Vec<OsString> = [
        input.executable.as_os_str(),
        "run".as_ref(),
        // Lane sessions must not appear in the owner's OpenCode session lists.
        "--archived".as_ref(),
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
    let [config_home, data_home, state_home, cache_home] =
        host_xdg_homes(input.home, input.host_var);
    let service_available =
        host_access && service_reachable(&state_home.join("opencode/service.json"));
    if !service_available {
        argv.push("--standalone".into());
    }
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
    let defaults = crate::launch::tool_search_path(input.home, true, input.policy)?;
    let mut env = base_env(input, &plan.scratch.path, defaults)?;
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
            lane_access,
            input.policy,
            crate::launch::OmissionSource::Toolchain,
            &mut omitted_discovery,
        )?;
    }
    // Preserve ordinary provider configuration without importing user plugins
    // or skills into the lane. Do not follow unrelated config links.
    let config_dir = config_home.join("opencode");
    let optional_start = runtime_reads.len();
    // Host access reads the native config home, so a directory that overlaps a
    // credential root stays in its runtime reads with a warning from the
    // optional-grant filter below. Isolated lanes keep the credential refusal.
    if let Ok(config_dir) = std::fs::canonicalize(&config_dir)
        && (!config_isolated
            || !provenant_confinement::refuses_lane_root(
                &config_dir,
                true,
                &input.credential_roots,
            ))
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
    if config_isolated
        && let Ok(config_dir) = std::fs::canonicalize(&config_dir)
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
        lane_access,
        input.policy,
        crate::launch::OmissionSource::ProviderConfig,
        &mut omitted_discovery,
    )?;
    let authentication_roots = if host_access {
        Vec::new()
    } else {
        vec![exact(host_data.join("auth.json"))]
    };
    runtime_reads.extend(skill_reads);
    let mut setup = if host_access {
        [&config_home, &data_home, &state_home, &cache_home]
            .into_iter()
            .map(|home| SetupStep::CreateDir(home.join("opencode")))
            .collect()
    } else {
        vec![
            SetupStep::CreateDir(data.clone()),
            SetupStep::Symlink {
                link: data.join("auth.json"),
                target: host_data.join("auth.json"),
            },
        ]
    };
    // Mirror ordinary provider settings as individual links. Lane-owned
    // instructions, hooks and skills use their own private paths.
    let private_config = private.join("config");
    if !host_access {
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
    for name in if host_access {
        &[][..]
    } else {
        &["log", "storage", "snapshot", "shell"][..]
    } {
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
        let rg = if host_access {
            cache_home.join("opencode/bin/rg")
        } else {
            private.join("cache/opencode/bin/rg")
        };
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
        (
            "XDG_CONFIG_HOME".into(),
            if host_access {
                config_home.clone()
            } else {
                private_config
            }
            .into_os_string(),
        ),
        (
            "XDG_DATA_HOME".into(),
            if host_access {
                data_home.clone()
            } else {
                private_data
            }
            .into_os_string(),
        ),
        (
            "XDG_CACHE_HOME".into(),
            if host_access {
                cache_home.clone()
            } else {
                private.join("cache")
            }
            .into_os_string(),
        ),
        (
            "XDG_STATE_HOME".into(),
            if host_access {
                state_home.clone()
            } else {
                private.join("state")
            }
            .into_os_string(),
        ),
    ]);
    if !host_access {
        env.push(("OPENCODE_CONFIG_CONTENT".into(), config.into()));
    }
    // No repository configuration or Claude Code files. Provenant's global
    // plugin is loaded from the private OpenCode config home.
    for key in if host_access {
        &[][..]
    } else {
        &[
            "OPENCODE_DISABLE_AUTOUPDATE",
            "OPENCODE_DISABLE_PROJECT_CONFIG",
            "OPENCODE_DISABLE_CLAUDE_CODE",
            "OPENCODE_DISABLE_FILEWATCHER",
        ][..]
    } {
        env.push(((*key).into(), "1".into()));
    }
    let mut spec = LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup,
        omitted_discovery,
        warnings: if host_access && !service_available {
            vec![
                "OpenCode host service unavailable; running standalone with the host profile"
                    .into(),
            ]
        } else {
            Vec::new()
        },
        user_skills: staged_skills,
        tool_requirements: Default::default(),
        git_wrapper: Default::default(),
        policy: input.policy.clone(),
        credential_roots: input.credential_roots.clone(),
        session_state: if host_access {
            crate::SessionStateLocation::Native {
                root: host_data.clone(),
            }
        } else {
            crate::SessionStateLocation::AttemptPrivate
        },
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                confinement:
                    provenant_confinement::permissions::ResolvedConfinementPolicy::default()
                        .resolve_for(input.confined.permissions()),
                process_executables,
                // The standalone server is a child copy of the same program.
                allow_process_fork: true,
                runtime_reads,
                state_writes: if host_access {
                    [
                        config_home.join("opencode"),
                        host_data.clone(),
                        state_home.join("opencode"),
                        cache_home.clone().join("opencode"),
                    ]
                    .into_iter()
                    .map(subtree)
                    .collect()
                } else {
                    Vec::new()
                },
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
        let cause = provenant_core::bounded_line(&printable, SERVER_FAILURE_CAUSE_LIMIT);
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

/// A variant the model does not offer is a route configuration refusal, not a
/// model outage: the provider's own name for it is `"Variant unavailable"`.
/// Spec: docs/specs/routing.md#exact-selection-no-inference
fn variant_unavailable(message: &str) -> bool {
    message.to_ascii_lowercase().contains("variant unavailable")
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
                || variant_unavailable(&message)
                || ((message.contains("variant") || message.contains("effort"))
                    && (message.contains("not supported") || message.contains("not found")))
        });
        let guidance = if invalid_reference { "; choose a supported model and variant (check the route's effort)" } else { "" };
        Some(provenant_core::bounded_line(
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
        (number > 0).then(|| number.saturating_mul(scale))
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
    /// A provider retry: unlike `error` this never ends the CLI process, so
    /// without it a 429 loop runs until the lane's wall timeout.
    #[serde(rename = "retry")]
    Retry {
        #[serde(default)]
        attempt: Option<u64>,
        error: RetryError,
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
struct RetryError {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    status: Option<u16>,
    message: Option<String>,
    #[serde(default, rename = "retryAfterMs")]
    retry_after_ms: Option<serde_json::Value>,
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
    let seconds = if let Some(n) = value.as_u64() {
        if n == 0 {
            return None;
        }
        n.div_ceil(1000)
    } else if let Some(i) = value.as_i64() {
        if i <= 0 {
            return None;
        }
        (i as u64).div_ceil(1000)
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
    Some(seconds)
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
    /// The step's own id, used as the usage report's key so a repeated
    /// step_finish replaces that step instead of double-counting it.
    #[serde(default)]
    id: Option<String>,
    reason: String,
    #[serde(default)]
    tokens: Option<StepTokens>,
    #[serde(default)]
    cost: Option<f64>,
}
#[derive(Deserialize)]
struct StepTokens {
    #[serde(default)]
    input: Option<u64>,
    #[serde(default)]
    output: Option<u64>,
    #[serde(default)]
    reasoning: Option<u64>,
    #[serde(default)]
    cache: Option<StepCache>,
}
#[derive(Deserialize)]
struct StepCache {
    #[serde(default)]
    read: Option<u64>,
    #[serde(default)]
    write: Option<u64>,
}

/// OpenCode states cost as USD. Keep it in integer micros, as the shared
/// `Cost` contract requires; an absent, negative or non-finite figure is not
/// a stated cost and stays unknown.
fn stated_cost(dollars: Option<f64>) -> Option<Cost> {
    let dollars = dollars?;
    if !dollars.is_finite() || dollars < 0.0 {
        return None;
    }
    Some(Cost {
        micros: (dollars * 1_000_000.0).round() as u64,
        currency: "USD".into(),
        stated_by: CostOrigin::Provider,
    })
}

/// Turn one `step_finish` into a typed usage report. Only buckets the step
/// actually stated are present; a stream with no tokens leaves usage unknown,
/// never zero. Spec: docs/specs/telemetry.md#usage-merges-by-declared-semantics
fn step_usage(finish: Finish) -> Option<crate::usage::UsageReport> {
    let tokens = finish.tokens?;
    let buckets = Tokens {
        input: tokens.input,
        output: tokens.output,
        reasoning: tokens.reasoning,
        cache_read: tokens.cache.as_ref().and_then(|cache| cache.read),
        cache_write: tokens.cache.and_then(|cache| cache.write),
        ..Tokens::default()
    };
    let mut report = crate::usage::UsageReport::step(buckets);
    if let Some(id) = finish.id {
        report = report.key(id);
    }
    if let Some(cost) = stated_cost(finish.cost) {
        report = report.cost(cost);
    }
    Some(report)
}

/// Decide what a provider retry means. A rate-limit retry carries a reset and
/// cannot recover in this attempt, so the first one is already past its bound
/// and ends the attempt as usage-limited. A capacity retry is transient until
/// it reaches OpenCode's own ten-attempt bound, or names an endpoint that is
/// unavailable. Spec: docs/specs/routing.md#capacity-is-not-quota
fn retry_events(error: RetryError, attempt: Option<u64>) -> Vec<E> {
    let message = error
        .message
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| "provider retry".into());
    let is_rate_limit = error.kind == "provider.rate-limit" || error.status == Some(429);
    let is_capacity = error.kind == "provider.internal"
        && error
            .status
            .is_some_and(|status| (500..600).contains(&status));
    let past = |bound| attempt.unwrap_or(1).max(1) >= bound;
    if is_rate_limit {
        if past(RATE_LIMIT_RETRY_BOUND) {
            // The same typed signal the final-error path emits, carrying the
            // provider's reset, so settlement sees a usage-limited attempt.
            vec![E::QuotaLimited {
                message,
                pool: None,
                resets_in_seconds: parse_retry_after(error.retry_after_ms.as_ref()),
            }]
        } else {
            vec![E::RateLimited { message }]
        }
    } else if is_capacity {
        let endpoint_unavailable = message
            .to_ascii_lowercase()
            .contains("endpoint is unavailable");
        if endpoint_unavailable || past(CAPACITY_RETRY_BOUND) {
            vec![E::ModelOverloaded { message }]
        } else {
            vec![E::TransientError]
        }
    } else {
        // An unclassified retry is not evidence of a refusal; keep waiting.
        vec![E::TransientError]
    }
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
            let stop = part.reason == "stop";
            let mut events = vec![E::Session(session)];
            if let Some(report) = step_usage(part) {
                events.push(E::Usage(report));
            }
            if stop {
                events.push(E::Terminal);
            }
            events
        }
        Event::Retry { attempt, error } => retry_events(error, attempt),
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
            } else if error.message.as_deref().is_some_and(variant_unavailable) {
                // A variant the model does not offer is a route refusal. The
                // diagnostic names the variants to fix the route; treating it
                // as a model outage would send settlement down the wrong path.
                E::Refusal
            } else if error.kind == "provider.no-route" {
                // OpenCode 2.2.0 maps ModelUnavailableError to this type.
                E::ModelUnavailable {
                    message: error
                        .message
                        .unwrap_or_else(|| "provider model unavailable".into()),
                }
            } else if error.kind == "provider.internal"
                && error
                    .status
                    .is_some_and(|status| (500..600).contains(&status))
            {
                // Any upstream 5xx is capacity, not a generic error, and never
                // a quota hold: the account may still have allowance.
                E::ModelOverloaded {
                    message: error
                        .message
                        .filter(|message| !message.is_empty())
                        .unwrap_or_else(|| "provider overloaded (HTTP 529)".into()),
                }
            } else if (error.kind == "unknown" || error.kind.is_empty())
                && error.status.is_none()
                && !auth
                && !limited
            {
                let message = error
                    .message
                    .unwrap_or_else(|| "unclassified provider error".into());
                E::Unclassified { message }
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
#[path = "opencode_tests.rs"]
mod tests;
