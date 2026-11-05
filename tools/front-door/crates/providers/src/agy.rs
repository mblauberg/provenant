use crate::launch::{Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, subtree};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use serde::Deserialize;
use std::ffi::OsString;
pub struct Agy;

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &Agy,
    quota_source: Some(&Agy),
    stdout_decoder: None,
    stderr_decoder: None,
    display_name: "Agy",
    quota: provenant_quota::QuotaPolicy {
        adapter: "agy",
        timer_seconds: 1200,
        panel_seconds: 900,
        probe_seconds: 30,
        low_power: false,
        solo: true,
    },
};

crate::adapter!(Agy, "agy", prepare, failure_diagnostic);
const MODEL_ON_WIRE: bool = true;

/// The CLI's own turn limit, off at zero; the supervisor's deadline is the real
/// bound.
const PRINT_TIMEOUT: &str = "0s";
const SECURITY: &str = "/usr/bin/security";
/// Its `run_command` tool runs commands through `SHELL`.
const SHELL: &str = "/bin/zsh";
const KEYCHAIN: &str = "Library/Keychains/login.keychain-db";

/// A short-lived, non-secret identity observation shared by quota and dispatch.
/// Keychain metadata invalidates it on a normal sign-in change before expiry.
const IDENTITY_TTL: i64 = 30;

#[derive(serde::Serialize, serde::Deserialize)]
struct IdentityProbe {
    key: String,
    observed: i64,
    stamp: Option<(u64, u64, u64, i64, i64)>,
}

fn identity_file(directory: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let digest = provenant_records::CanonicalV1::file_digest(directory.as_os_str().as_bytes());
    format!(
        "agy-identity-{}.json",
        digest.to_string().trim_start_matches("sha256:")
    )
}

pub(crate) type KeychainStamp = (u64, u64, u64, i64, i64);

pub(crate) fn keychain_stamp(directory: &std::path::Path) -> Option<KeychainStamp> {
    use std::os::unix::fs::MetadataExt;
    let stat = std::fs::metadata(directory.join(KEYCHAIN)).ok()?;
    Some((
        stat.dev(),
        stat.ino(),
        stat.len(),
        stat.mtime(),
        stat.mtime_nsec(),
    ))
}

pub(crate) fn cached_identity(
    state: &std::path::Path,
    directory: &std::path::Path,
    now: i64,
) -> Option<String> {
    let root = provenant_os::fs::Directory::open_anchor(state).ok()?;
    let bytes = root
        .read_bounded(identity_file(directory).as_ref(), 4096)
        .ok()?;
    let probe: IdentityProbe = serde_json::from_slice(&bytes).ok()?;
    (probe.observed <= now
        && now < probe.observed.saturating_add(IDENTITY_TTL)
        && probe.stamp == keychain_stamp(directory)
        && probe.key.strip_prefix("agy-google-").is_some_and(|hash| {
            hash.len() == 12 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        }))
    .then_some(probe.key)
}

pub(crate) fn remember_identity(
    state: &std::path::Path,
    directory: &std::path::Path,
    key: &str,
    now: i64,
    expected_stamp: Option<KeychainStamp>,
) -> bool {
    let current_stamp = keychain_stamp(directory);
    if current_stamp != expected_stamp {
        return false;
    }
    let publish = || -> Result<(), Box<dyn std::error::Error>> {
        let root = provenant_os::fs::Directory::open_anchor(state)?;
        let bytes = serde_json::to_vec(&IdentityProbe {
            key: key.into(),
            observed: now,
            stamp: current_stamp,
        })?;
        root.atomic_write(
            identity_file(directory).as_ref(),
            ".agy-identity-",
            &bytes,
            provenant_os::fs::PublishMode::Replace,
        )?
        .require_durable()?;
        Ok(())
    };
    publish().is_ok()
}

pub(crate) fn forget_identity(state: &std::path::Path, directory: &std::path::Path) {
    if let Ok(root) = provenant_os::fs::Directory::open_anchor(state) {
        let _ = root.unlink(identity_file(directory).as_ref());
    }
}

/// The keychain an account signs in with, from its registered directory.
///
/// A directory that is the user's real `home` is the shared login keychain
/// (`Ok(None)`). Any other directory is an isolated account whose own `HOME`
/// holds its keychain, and that mode never depends on whether the file exists:
/// a missing keychain, or one that is not a regular file (a symlink would
/// point at a file elsewhere), is an error naming the path, so a keychain that
/// vanishes after registration or verification fails closed instead of
/// falling back to the shared login keychain. That holds for a keychain that
/// is gone when it is checked; see [`ProbeHome`] for the window while a probe
/// runs.
///
/// This only selects which keychain a lane links. The guard against a wrong
/// sign-in is the identity pinned at registration and re-verified on launch.
pub fn account_keychain(
    directory: &std::path::Path,
    home: &std::path::Path,
) -> Result<Option<std::path::PathBuf>, crate::ProviderError> {
    let canonical = |path: &std::path::Path| std::fs::canonicalize(path).ok();
    if directory == home
        || canonical(directory)
            .zip(canonical(home))
            .is_some_and(|(directory, home)| directory == home)
    {
        return Ok(None);
    }
    isolated_keychain(directory, home).map(|(keychain, _)| Some(keychain))
}

/// The one check that an account directory holds its own keychain, shared by
/// registration, both probes and launch preparation: the keychain path, with
/// its identity, or the error naming it. The file must be a regular file and
/// its fully resolved path must be exactly `<resolved directory>/Library/
/// Keychains/login.keychain-db`, so a symlink anywhere along it (the file, or
/// `Library` or `Library/Keychains` pointing at another home's) is refused.
/// A hard link is no better: the file must have a single link and must not be
/// the same device and inode as the real `home`'s login keychain, which is only
/// ever stat'ed, never opened.
fn isolated_keychain(
    directory: &std::path::Path,
    home: &std::path::Path,
) -> Result<(std::path::PathBuf, std::fs::Metadata), crate::ProviderError> {
    use std::os::unix::fs::MetadataExt;
    let keychain = directory.join(KEYCHAIN);
    let own = std::fs::canonicalize(directory)
        .ok()
        .zip(std::fs::canonicalize(&keychain).ok())
        .is_some_and(|(directory, resolved)| resolved == directory.join(KEYCHAIN));
    match std::fs::symlink_metadata(&keychain) {
        Ok(metadata)
            if own
                && metadata.file_type().is_file()
                && metadata.nlink() == 1
                && !std::fs::metadata(home.join(KEYCHAIN))
                    .is_ok_and(|real| identity(&real) == identity(&metadata)) =>
        {
            Ok((keychain, metadata))
        }
        _ => Err(crate::ProviderError::IsolatedKeychainUnavailable(
            keychain.display().to_string(),
        )),
    }
}

/// The `ExecError::Io` operation of a probe refused because its isolated
/// keychain was no longer the one selected.
pub const KEYCHAIN_OPERATION: &str = "isolated keychain";

/// The `HOME` an agy identity probe runs under, with the isolated keychain that
/// selected it.
///
/// The probe itself is unconfined, and `HOME` is all that makes the agent use
/// the account's keychain: if that file vanished, macOS could fall through to
/// the shared login keychain and the probe would record the wrong account.
/// [`ProbeHome::guard`] therefore re-checks the keychain (same device and
/// inode, still a regular file inside the account directory) immediately
/// before spawning and again once the probe returns, and refuses the result if
/// either check fails.
///
/// Residual race: the agent opens the keychain at some point between the two
/// checks, so any temporary replacement, absence or symlink swap during the
/// probe that is undone, with the original inode restored, before the second
/// check is not observable. The probe can then have read another keychain
/// without the result being refused. Closing it needs a seatbelt profile
/// denying the real home's `Library/Keychains` to the probe, which cannot be
/// verified against a real agy without touching a real keychain.
#[derive(Debug)]
pub struct ProbeHome {
    home: std::path::PathBuf,
    held: Option<Held>,
}

#[derive(Debug)]
struct Held {
    directory: std::path::PathBuf,
    home: std::path::PathBuf,
    id: (u64, u64),
}

fn identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

impl ProbeHome {
    /// The `HOME` for `directory`, refusing an isolated account whose keychain
    /// is not available. The user's own home needs no guard.
    pub fn select(
        directory: &std::path::Path,
        home: &std::path::Path,
    ) -> Result<Self, crate::ProviderError> {
        if account_keychain(directory, home)?.is_none() {
            return Ok(Self {
                home: home.to_owned(),
                held: None,
            });
        }
        let (_, metadata) = isolated_keychain(directory, home)?;
        Ok(Self {
            home: directory.to_owned(),
            held: Some(Held {
                directory: directory.to_owned(),
                home: home.to_owned(),
                id: identity(&metadata),
            }),
        })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.home
    }

    /// Fails unless the isolated keychain is still the selected file.
    pub fn verify(&self) -> Result<(), crate::ProviderError> {
        let Some(held) = &self.held else {
            return Ok(());
        };
        let (keychain, metadata) = isolated_keychain(&held.directory, &held.home)?;
        if identity(&metadata) == held.id {
            Ok(())
        } else {
            Err(crate::ProviderError::IsolatedKeychainUnavailable(
                keychain.display().to_string(),
            ))
        }
    }

    /// Runs probes through `executor` under this home's keychain check.
    pub fn guard<'a>(
        &'a self,
        executor: &'a dyn provenant_os::exec::Executor,
    ) -> GuardedExecutor<'a> {
        GuardedExecutor {
            executor,
            home: self,
        }
    }
}

/// An executor that checks the keychain before spawning and after the probe
/// returns, failing with [`KEYCHAIN_OPERATION`] when it changed.
pub struct GuardedExecutor<'a> {
    executor: &'a dyn provenant_os::exec::Executor,
    home: &'a ProbeHome,
}

impl provenant_os::exec::Executor for GuardedExecutor<'_> {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
        let refused = |error: crate::ProviderError| provenant_os::exec::ExecError::Io {
            operation: KEYCHAIN_OPERATION,
            source: std::io::Error::other(match error {
                crate::ProviderError::IsolatedKeychainUnavailable(path) => path,
                other => other.to_string(),
            }),
        };
        self.home.verify().map_err(refused)?;
        let outcome = self.executor.execute(options)?;
        self.home.verify().map_err(refused)?;
        Ok(outcome)
    }
}

/// Agy runs with the attempt's private state as `HOME`, so it sees none of the
/// user's Gemini or Antigravity configuration, MCP servers, plugins, hooks,
/// sidecars or knowledge: its state lives and dies in the private home. Its
/// sign-in is in a keychain, so the private home links only that one file,
/// read-only: an isolated account's own keychain, otherwise the user's login
/// keychain. Headless mode auto-denies every
/// tool permission it cannot prompt for, so lanes auto-approve tools: our
/// confinement profile, not agy's prompts, bounds what a lane can do. Its own
/// sandbox cannot nest inside ours.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    let mut tool_requirements = crate::launch::ToolRequirements::default();
    if plan.selected_authentication.as_deref() != Some("keychain") {
        return Err(ProviderError::UnsupportedPolicy);
    }
    input.prompt.check_argv()?;
    let home = &plan
        .private_state
        .as_ref()
        .ok_or(ProviderError::UnsupportedPolicy)?
        .path;
    let keychain = match input.account {
        Some(account) => account_keychain(account.directory(), input.home)?,
        None => None,
    }
    .unwrap_or_else(|| input.home.join(KEYCHAIN));
    let staged_skills = input.stage_user_skills();
    let user_skills = staged_skills.root.clone();
    let skill_reads = user_skills.iter().map(subtree).collect::<Vec<_>>();
    let omitted_discovery = Vec::new();
    let mut argv: Vec<OsString> = [
        input.executable.as_os_str(),
        "--output-format".as_ref(),
        "stream-json".as_ref(),
        "--print-timeout".as_ref(),
        PRINT_TIMEOUT.as_ref(),
    ]
    .map(OsString::from)
    .to_vec();
    if input.git_context {
        // Headless reads are auto-approved inside workspace directories.
        // https://www.agy.dev/docs/cli/reference/ (--add-dir)
        argv.extend(["--add-dir".into(), plan.scratch.path.as_os_str().to_owned()]);
    }
    argv.push("--dangerously-skip-permissions".into());
    if access == Access::ReadOnly {
        if let Some(user_skills) = &user_skills {
            // The skill loader reports paths through the private home link.
            argv.extend([
                "--add-dir".into(),
                home.join(".gemini/skills").into_os_string(),
            ]);
            argv.extend(["--add-dir".into(), user_skills.as_os_str().to_owned()]);
        }
        for grant in &skill_reads {
            argv.extend(["--add-dir".into(), grant.path.as_os_str().to_owned()]);
        }
        // Headless mode auto-denies reads outside the workspace, which holds
        // only the working directory, so every other read root joins it.
        for root in &plan.read_roots {
            if !input.cwd.starts_with(&root.path) {
                argv.extend(["--add-dir".into(), root.path.as_os_str().to_owned()]);
            }
        }
    }
    if let Some(parent) = input.prompt.file().and_then(std::path::Path::parent) {
        argv.extend(["--add-dir".into(), parent.as_os_str().to_owned()]);
    }
    if session == Session::Resume {
        let continuation = input.continuation.ok_or(ProviderError::UnknownIdentity)?;
        argv.extend(["--conversation".into(), continuation.session().into()]);
    }
    argv.extend(["--model".into(), input.route.model.as_str().into()]);
    if let Some(effort) = &input.route.effort {
        argv.extend(["--effort".into(), effort.into()]);
    }
    argv.extend(["--print".into(), input.prompt.text().into()]);

    let shell = access == Access::WorktreeWrite;
    let path = if shell {
        format!("{}:/usr/bin:/bin", input.home.join(".cargo/bin").display())
    } else {
        "/usr/bin:/bin".into()
    };
    let mut env = base_env(input, &plan.scratch.path, &path);
    for (key, value) in &mut env {
        if key == "HOME" {
            *value = home.into();
        }
    }
    env.push(("SHELL".into(), SHELL.into()));
    let keychains = home.join("Library/Keychains");
    let mut setup = vec![
        SetupStep::CreateDir(keychains.clone()),
        SetupStep::Symlink {
            link: keychains.join("login.keychain-db"),
            target: keychain.clone(),
        },
    ];
    if let Some(user_skills) = &user_skills {
        setup.push(SetupStep::CreateDir(home.join(".gemini")));
        setup.push(SetupStep::Symlink {
            link: home.join(".gemini/skills"),
            target: user_skills.clone(),
        });
    }
    // Both lanes run system commands; only writers get the additional tools.
    let runtime_reads = skill_reads;
    let tool_exec_roots = vec![
        subtree("/bin"),
        subtree("/usr/bin"),
        subtree("/usr/libexec"),
    ];
    env.extend([
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]);
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
    Ok(LaunchSpec {
        argv,
        env,
        stdin: Stdin::Null,
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup,
        omitted_discovery,
        user_skills: staged_skills,
        tool_requirements,
        git_wrapper: Default::default(),
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                // The keyring library reads its token through Apple's `security`.
                process_executables: vec![exact(input.executable), exact(SECURITY)],
                allow_process_fork: true,
                // Its `run_command` tool fails unless it can allocate a PTY.
                allow_pseudo_tty: true,
                authentication: vec![AuthenticationProfile {
                    mechanism: "keychain".into(),
                    roots: vec![exact(&keychain)],
                    in_place_writes: Vec::new(),
                }],
                runtime_reads,
                tool_exec_roots,
                provider_api: vec![NetworkEndpoint {
                    address: "*".into(),
                    port: 443,
                }],
                // Its agent runs behind a local server on a loopback port.
                loopback_server: true,
                ..ProviderProfileSpec::default()
            },
            observations: Default::default(),
        },
        stream,
    })
}
crate::quota_source!(Agy, "agy");
#[derive(Deserialize)]
#[serde(tag = "event")]
enum Event {
    #[serde(rename = "init")]
    Init {
        conversation_id: String,
        init: InitWire,
    },
    #[serde(rename = "step_update")]
    Step { step_update: Step },
    #[serde(rename = "result")]
    Result { result: ResultWire },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct InitWire {
    model: String,
}
#[derive(Deserialize)]
struct Step {
    conversation_id: String,
    step_type: String,
    text_delta: Option<String>,
}
#[derive(Deserialize)]
struct ResultWire {
    conversation_id: String,
    status: String,
    response: String,
    error: Option<String>,
    #[serde(default)]
    denied_actions: serde_json::Value,
    usage: Usage,
}
#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}
/// Only the CLI's recorded headless auto-denial is diagnostic evidence. Do not
/// copy arbitrary stderr into results or infer completion or identity from it.
fn failure_diagnostic(stderr: &[u8]) -> Option<String> {
    let stderr = std::str::from_utf8(stderr).ok()?;
    stderr.lines().find_map(|line| {
        let permission = line
            .strip_prefix("jetski: no output produced \u{2014} a tool required the \"")?
            .strip_suffix(
                "\" permission that headless mode cannot prompt for, so it was auto-denied.",
            )?;
        // Bound and constrain the only provider-supplied part of the message.
        (1..=64)
            .contains(&permission.len())
            .then_some(permission)
            .filter(|name| {
                name.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
            .map(|name| format!("agy denied the \"{name}\" permission in headless mode"))
    })
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        Event::Init {
            conversation_id,
            init,
        } => vec![E::Session(conversation_id), E::Model(init.model)],
        Event::Step { step_update } => {
            let mut events = vec![E::Session(step_update.conversation_id)];
            if step_update.step_type == "agent_response"
                && let Some(text) = step_update.text_delta
            {
                events.push(E::Text(text));
            }
            events
        }
        Event::Result { result } => {
            let mut events = vec![E::Usage {
                input: result.usage.input_tokens,
                output: result.usage.output_tokens,
                // Agy's `cache_read_tokens` is not recorded: nothing shows whether
                // `input_tokens` already contains it, so splitting would guess.
                cache_read: None,
                cache_write: None,
                cache_write_1h: None,
            }];
            if !result.conversation_id.is_empty() {
                events.push(E::Session(result.conversation_id));
            }
            if let Some(message) = result
                .error
                .as_deref()
                .filter(|message| quota_reached(message))
            {
                events.push(E::QuotaLimited {
                    message: message.to_owned(),
                    pool: None,
                    resets_in_seconds: resets_in(message),
                });
            } else if let Some(message) = result
                .error
                .as_deref()
                .filter(|message| model_overloaded(message))
            {
                events.push(E::ModelOverloaded {
                    message: message.to_owned(),
                });
            } else if let Some(denied) = result.denied_actions.as_array().and_then(|a| a.first()) {
                if !result.response.is_empty() {
                    events.push(E::Text(result.response.clone()));
                }
                let name = denied
                    .get("display_name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let action = if (1..=64).contains(&name.len())
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    name
                } else {
                    "unknown action"
                };
                if result.status == "SUCCESS"
                    && result.error.is_none()
                    && result.response.is_empty()
                {
                    events.push(E::Denied {
                        action: action.into(),
                    });
                } else {
                    events.push(E::DeniedNote {
                        action: action.into(),
                    });
                    events.push(if result.error.is_some() || result.status != "SUCCESS" {
                        E::Error
                    } else {
                        E::Terminal
                    });
                }
            } else if result.error.is_some() || result.status != "SUCCESS" {
                if !result.response.is_empty() {
                    events.push(E::Text(result.response));
                }
                events.push(E::Error);
            } else {
                events.push(E::Text(result.response));
                events.push(E::Terminal);
            }
            events
        }
        _ => vec![],
    })
}
/// The CLI's own exhausted-quota refusal, `Individual quota reached ... Resets
/// in 7h23m`, possibly behind its 429 status. The error must open with it, so
/// an echoed prompt or another error quoting it does not count. A bare 429 or
/// `RESOURCE_EXHAUSTED` may be a per-minute rate limit, so neither counts.
fn quota_reached(message: &str) -> bool {
    const REFUSAL: &str = "individual quota reached";
    let message = message.trim_start();
    let message = message
        .strip_prefix("429")
        .filter(|rest| rest.starts_with([':', ' ']))
        .map_or(message, |rest| rest.trim_start_matches([':', ' ']));
    message
        .get(..REFUSAL.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(REFUSAL))
}
/// The model's capacity refusal, `The model is overloaded. Please try again
/// later.`, possibly behind its 503 or 529 status. As with [`quota_reached`],
/// the error must open with it.
fn model_overloaded(message: &str) -> bool {
    let mut message = message.trim_start();
    for status in ["503", "529"] {
        if let Some(rest) = message
            .strip_prefix(status)
            .filter(|rest| rest.starts_with([':', ' ']))
        {
            message = rest.trim_start_matches([':', ' ']);
        }
    }
    for lead in ["service unavailable", "unavailable"] {
        if message
            .get(..lead.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(lead))
        {
            message = message[lead.len()..].trim_start_matches([':', ' ', '-']);
        }
    }
    let message = message
        .strip_prefix("The ")
        .or_else(|| message.strip_prefix("the "))
        .unwrap_or(message);
    const REFUSAL: &str = "model is overloaded";
    message
        .get(..REFUSAL.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(REFUSAL))
}
/// The refusal's stated wait, only from its closing `Resets in 7h23m`, within
/// one weekly window.
fn resets_in(message: &str) -> Option<u64> {
    const MARKER: &str = "resets in ";
    let lower = message
        .trim_end()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let rest = lower[lower.rfind(MARKER)? + MARKER.len()..].trim_start();
    let total = crate::duration::suffix_seconds(rest)?;
    (1..=7 * 86_400).contains(&total).then_some(total)
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn agy_identity_cache_expires_and_invalidates_when_keychain_changes() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("home");
        let keychain = directory.join(KEYCHAIN);
        std::fs::create_dir_all(keychain.parent().unwrap()).unwrap();
        std::fs::write(&keychain, "synthetic").unwrap();
        let key = "agy-google-0123456789ab";
        let stamp = keychain_stamp(&directory);
        assert!(remember_identity(temp.path(), &directory, key, 100, stamp));
        assert_eq!(
            cached_identity(temp.path(), &directory, 100).as_deref(),
            Some(key)
        );
        assert_eq!(
            cached_identity(temp.path(), &directory, 129).as_deref(),
            Some(key)
        );
        assert!(cached_identity(temp.path(), &directory, 99).is_none());
        assert!(cached_identity(temp.path(), &directory, 130).is_none());
        assert!(cached_identity(temp.path(), &temp.path().join("other"), 101).is_none());
        std::fs::write(&keychain, "changed synthetic keychain").unwrap();
        assert!(cached_identity(temp.path(), &directory, 101).is_none());
        let new_stamp = keychain_stamp(&directory);
        assert!(remember_identity(
            temp.path(),
            &directory,
            key,
            102,
            new_stamp
        ));
        forget_identity(temp.path(), &directory);
        assert!(cached_identity(temp.path(), &directory, 103).is_none());
    }
}
