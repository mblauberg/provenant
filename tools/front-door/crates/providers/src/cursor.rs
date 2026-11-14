use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, package_grant, subtree,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use std::{io::Read, path::Path, time::Duration};

const IDENTITY_LIMIT: usize = 1_048_576;

/// One Cursor identity observation shared by registration and quota capture.
/// The access token is private and intentionally omitted from Debug.
pub struct CursorIdentity {
    pub account_key: String,
    access_token: Option<String>,
}
impl CursorIdentity {
    pub(crate) fn access_token(&self) -> Option<&str> {
        self.access_token.as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CursorIdentityError {
    #[error("Cursor credentials were not found")]
    CredentialNotFound,
    #[error("Cursor identity is unavailable")]
    Unavailable,
    #[error("Cursor identity is malformed")]
    Malformed,
}

pub fn read_identity(directory: &Path) -> Result<CursorIdentity, CursorIdentityError> {
    if let Some(token) = read_access_token(directory)? {
        if token.len() > IDENTITY_LIMIT {
            return Err(CursorIdentityError::Malformed);
        }
        let token = token.trim().to_owned();
        let account_key =
            crate::identity::cursor_account(&token).ok_or(CursorIdentityError::Malformed)?;
        return Ok(CursorIdentity {
            account_key,
            access_token: Some(token),
        });
    }

    let mut bytes = Vec::new();
    std::fs::File::open(directory.join("cli-config.json"))
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CursorIdentityError::CredentialNotFound
            } else {
                CursorIdentityError::Unavailable
            }
        })?
        .take(IDENTITY_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CursorIdentityError::Unavailable)?;
    if bytes.len() > IDENTITY_LIMIT {
        return Err(CursorIdentityError::Malformed);
    }
    let config: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| CursorIdentityError::Malformed)?;
    let account_key = config["authInfo"]["authId"]
        .as_str()
        .and_then(crate::identity::digest)
        .map(|key| format!("cursor-{key}"))
        .ok_or(CursorIdentityError::Malformed)?;
    Ok(CursorIdentity {
        account_key,
        access_token: None,
    })
}

fn read_access_token(directory: &Path) -> Result<Option<String>, CursorIdentityError> {
    let path = directory.join("state.vscdb");
    if !path
        .try_exists()
        .map_err(|_| CursorIdentityError::Unavailable)?
    {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| CursorIdentityError::Unavailable)?;
    connection
        .busy_timeout(Duration::from_millis(250))
        .map_err(|_| CursorIdentityError::Unavailable)?;
    let has_item_table: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='ItemTable')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| CursorIdentityError::Unavailable)?;
    if !has_item_table {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT substr(value,1,1048577) FROM ItemTable WHERE key='cursorAuth/accessToken' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| CursorIdentityError::Unavailable)
}

pub struct Cursor;

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &Cursor,
    quota_source: Some(&Cursor),
    stdout_decoder: None,
    stderr_decoder: None,
    display_name: "Cursor",
    quota: provenant_quota::QuotaPolicy {
        adapter: "cursor",
        timer_seconds: 1800,
        panel_seconds: 600,
        probe_seconds: 14,
        low_power: true,
        solo: false,
    },
};

/// Project the CLI's current `status` output immediately to a non-secret
/// account fingerprint. Display names and model selectors are not identities.
pub fn status_account(output: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(output).ok()?;
    let account = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("✓ Logged in as "))?;
    if account.split_whitespace().count() != 1 || !account.contains('@') {
        return None;
    }
    crate::identity::fingerprint(account)
}

crate::adapter!(Cursor, "cursor", prepare);
// The stream reports a display name (including Auto), not an exact model id.
const MODEL_ON_WIRE: bool = false;

fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    input.prompt.check_argv(input.policy)?;
    let plan = input.confined.permissions();
    if plan.selected_authentication.as_deref() != Some("login") {
        return Err(input.boundary_error(
            "CURSOR_PREPARE_CREDENTIALS",
            "Cursor requires its selected login authentication",
            ProviderError::UnsupportedPolicy,
        ));
    }
    let mut argv = vec![
        input.executable.into(),
        "--print".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--workspace".into(),
        input.cwd.into(),
        "--trust".into(),
        // Cursor's own sandbox does not nest inside the confinement profile.
        "--sandbox".into(),
        "disabled".into(),
    ];
    match access {
        Access::ReadOnly => argv.extend(["--mode".into(), "ask".into()]),
        Access::WorktreeWrite => argv.push("--force".into()),
    }
    if session == Session::Resume {
        let continuation = input.continuation.ok_or_else(|| {
            input.boundary_error(
                "CURSOR_PREPARE_AUTHORITY",
                "resumed Cursor invocation has no validated continuation identity",
                ProviderError::UnknownIdentity,
            )
        })?;
        argv.extend(["--resume".into(), continuation.session().into()]);
    }
    if !input.route.model.is_empty() {
        argv.extend(["--model".into(), input.route.model.as_str().into()]);
    }
    argv.extend(["--".into(), input.prompt.text().into()]);
    let mut package_omissions = Vec::new();
    let package = package_grant(input.executable, &mut package_omissions, input.policy)?;
    let mut authentication = vec![exact(input.home.join(".cursor/cli-config.json"))];
    let config = input.home.join(".config/cursor");
    if config.is_dir() {
        authentication.push(subtree(config));
    }
    let staged_skills = input.stage_user_skills();
    let skill_reads = staged_skills.root.iter().map(subtree).collect::<Vec<_>>();
    let mut omitted_discovery = package_omissions;
    let mut optional_reads = if input.sandboxed() && access == Access::WorktreeWrite {
        crate::launch::git_global_files(&input.home.join(".config"))
    } else {
        Vec::new()
    };
    crate::launch::filter_optional_grants(
        &mut optional_reads,
        0,
        input.home,
        input.policy,
        crate::launch::OmissionSource::Toolchain,
        &mut omitted_discovery,
    )?;
    let mut env = base_env(input, &plan.scratch.path, "/usr/bin:/bin");
    if input.sandboxed() {
        env.extend([
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ]);
    }
    let private = &plan
        .private_state
        .as_ref()
        .ok_or(ProviderError::UnsupportedPolicy)?
        .path;
    let chats = private.join(".cursor/chats");
    let mut setup = vec![SetupStep::CreateDir(chats.clone())];
    setup.push(SetupStep::CreateDir(private.join(".cursor")));
    setup.push(SetupStep::CreateDir(private.join(".config")));
    for (link, target) in [
        (
            private.join(".cursor/cli-config.json"),
            input.home.join(".cursor/cli-config.json"),
        ),
        (
            private.join(".config/cursor"),
            input.home.join(".config/cursor"),
        ),
    ] {
        if target.exists() {
            setup.push(SetupStep::Symlink { link, target });
        }
    }
    if let Some(skills) = &staged_skills.root {
        setup.push(SetupStep::Symlink {
            link: private.join(".cursor/skills"),
            target: skills.clone(),
        });
    }
    for (key, value) in &mut env {
        if input.sandboxed() && key == "HOME" {
            *value = private.into();
        }
    }
    Ok(LaunchSpec {
        argv,
        env,
        stdin: Stdin::Null,
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup,
        omitted_discovery,
        user_skills: staged_skills,
        tool_requirements: Default::default(),
        git_wrapper: Default::default(),
        policy: input.policy.clone(),
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![exact(input.executable)],
                allow_process_fork: true,
                runtime_reads: [package.clone()]
                    .into_iter()
                    .chain(skill_reads)
                    .chain(optional_reads)
                    .collect(),
                authentication: vec![AuthenticationProfile {
                    keychain: None,
                    mechanism: "login".into(),
                    roots: authentication,
                    in_place_writes: Vec::new(),
                }],
                // The resolved launcher is a shell script in the self-updated
                // `versions/<version>/` payload that execs its sibling `node`,
                // so every lane needs a shell and that one payload directory.
                tool_exec_roots: vec![subtree("/bin"), subtree("/usr/bin"), package.clone()],
                provider_api: vec![NetworkEndpoint {
                    address: "*".into(),
                    port: 443,
                }],
                ..ProviderProfileSpec::default()
            },
            observations: Default::default(),
        },
        stream,
    })
}
crate::quota_source!(
    Cursor,
    "cursor",
    crate::quota_live::cursor::capture_source,
    crate::quota::cursor::decode_source
);
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "system")]
    System { subtype: String, session_id: String },
    #[serde(rename = "assistant")]
    Assistant {
        message: Message,
        session_id: String,
    },
    #[serde(rename = "result")]
    Result {
        session_id: String,
        subtype: String,
        is_error: bool,
        result: Option<String>,
        usage: Option<Usage>,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Message {
    role: String,
    content: Vec<Content>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Content {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Usage {
    #[serde(rename = "inputTokens")]
    input: u64,
    #[serde(rename = "outputTokens")]
    output: u64,
    // Cursor reports cache tokens apart from `inputTokens`, as its own
    // dashboard rows do.
    #[serde(default, rename = "cacheReadTokens")]
    cache_read: Option<u64>,
    #[serde(default, rename = "cacheWriteTokens")]
    cache_write: Option<u64>,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        // Auto is a selector, not answering-model evidence.
        Event::System {
            subtype,
            session_id,
        } => {
            if subtype == "init" {
                vec![E::Session(session_id)]
            } else {
                vec![]
            }
        }
        Event::Assistant {
            message,
            session_id,
        } if message.role == "assistant" => {
            let mut events = vec![E::Session(session_id)];
            for content in message.content {
                if let Content::Text { text } = content {
                    events.push(E::Text(text));
                }
            }
            events
        }
        Event::Result {
            session_id,
            subtype,
            is_error,
            result,
            usage,
        } => {
            let mut events = vec![E::Session(session_id)];
            if let Some(usage) = usage {
                events.push(E::Usage {
                    input: usage.input,
                    output: usage.output,
                    cache_read: usage.cache_read,
                    cache_write: usage.cache_write,
                    cache_write_1h: None,
                });
            }
            if is_error || subtype != "success" {
                if let Some(result) = result.filter(|message| !message.is_empty()) {
                    events.push(E::Text(result));
                }
                events.push(E::Error);
            } else {
                if let Some(result) = result {
                    events.push(E::Text(result));
                }
                events.push(E::Terminal);
            }
            events
        }
        _ => vec![],
    })
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use rusqlite::params;

    fn seed_token(directory: &Path) -> String {
        let token = "header.eyJzdWIiOiJjdXJzb3ItdXNlciJ9.signature";
        let connection = Connection::open(directory.join("state.vscdb")).unwrap();
        connection
            .execute("CREATE TABLE ItemTable (key TEXT, value TEXT)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable (key,value) VALUES ('cursorAuth/accessToken',?1)",
                params![token],
            )
            .unwrap();
        token.to_owned()
    }

    #[test]
    fn identity_reader_returns_the_account_key_and_token_from_one_observation() {
        let directory = tempfile::tempdir().unwrap();
        let token = seed_token(directory.path());
        let identity = read_identity(directory.path()).unwrap();
        assert_eq!(
            identity.account_key,
            format!("cursor-{}", crate::identity::digest("cursor-user").unwrap())
        );
        assert_eq!(identity.access_token(), Some(token.as_str()));
    }

    #[test]
    fn database_failure_is_distinct_from_absent_desktop_identity() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("state.vscdb"), b"unavailable").unwrap();
        std::fs::write(
            directory.path().join("cli-config.json"),
            r#"{"authInfo":{"authId":"cursor-config-user"}}"#,
        )
        .unwrap();
        assert!(matches!(
            read_identity(directory.path()),
            Err(CursorIdentityError::Unavailable)
        ));
        std::fs::remove_file(directory.path().join("state.vscdb")).unwrap();
        let identity = read_identity(directory.path()).unwrap();
        assert_eq!(
            identity.account_key,
            format!(
                "cursor-{}",
                crate::identity::digest("cursor-config-user").unwrap()
            )
        );
        assert_eq!(identity.access_token(), None);
    }
}
