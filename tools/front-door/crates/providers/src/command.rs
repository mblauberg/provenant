//! Configured commands share the provider launch and settlement machinery.
use crate::launch::{Access, Session, base_env, exact, subtree};
use crate::{LaunchSpec, PrepareInput, ProviderError, ProviderEvent, Stdin, StreamState};
use provenant_confinement::permissions::{ProviderProfile, ProviderProfileSpec};

pub struct Command;

/// Trusted argv and its host-resolved invocation path. Keeping the final
/// symlink name preserves argv[0] for rustup, corepack and other multicall tools.
#[derive(Clone, Copy)]
pub struct Invocation<'a> {
    pub argv: &'a [String],
    pub program: &'a std::path::Path,
    /// Host-resolved runtimes granted execution without file or write access.
    pub runtimes: &'a [std::path::PathBuf],
}

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &Command,
    quota_source: None,
    stdout_decoder: None,
    stderr_decoder: None,
    display_name: "Command",
    quota: provenant_quota::QuotaPolicy {
        adapter: "command",
        timer_seconds: 0,
        panel_seconds: 0,
        probe_seconds: 0,
        low_power: true,
        solo: false,
    },
};

crate::adapter!(Command, "command", prepare);
const MODEL_ON_WIRE: bool = false;

// Command streams are reduced as raw bounded bytes, rather than JSON lines.
fn decode(_: &[u8]) -> Result<Vec<ProviderEvent>, ProviderError> {
    Ok(Vec::new())
}

fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    if access != Access::ReadOnly {
        return Err(ProviderError::CapabilityUnavailable(
            "command adapter cannot enforce writer access",
        ));
    }
    if session != Session::Fresh || input.route.account_key.is_some() {
        return Err(input.boundary_error(
            "COMMAND_PREPARE_AUTHORITY",
            "command adapter cannot ignore the selected session or account identity",
            ProviderError::UnsupportedPolicy,
        ));
    }
    if !input.route.model.is_empty() || input.route.effort.is_some() {
        return Err(ProviderError::CapabilityUnavailable(
            "command adapter does not support model or effort selection",
        ));
    }
    if input
        .confined
        .permissions()
        .selected_authentication
        .is_some()
    {
        return Err(input.boundary_error(
            "COMMAND_PREPARE_CREDENTIALS",
            "command adapter cannot accept a provider authentication grant",
            ProviderError::UnsupportedPolicy,
        ));
    }
    let command = input
        .command
        .filter(|command| {
            !command.argv.is_empty() && !command.argv[0].is_empty() && command.program.is_absolute()
        })
        .ok_or(ProviderError::InvalidInvocation(
            "configured command needs non-empty argv and an absolute executable",
        ))?;
    // The fixed script merges stderr into the captured output. Positional
    // arguments stay literal; no configured argument is interpreted as shell text.
    let mut argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        "exec \"$@\" 2>&1".into(),
        "command".into(),
        command.program.into(),
    ];
    argv.extend(command.argv.iter().skip(1).map(Into::into));
    let scratch = &input.confined.permissions().scratch.path;
    let mut env = base_env(
        input,
        scratch,
        crate::launch::tool_search_path(input.home, true, input.policy)?,
    )?;
    if input.sandboxed() {
        env.extend([
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ]);
    }
    let mut profile = ProviderProfileSpec {
        process_executables: vec![exact("/bin/sh"), exact(input.executable)],
        allow_process_fork: true,
        deny_keychain_access: true,
        deny_credential_access: true,
        deny_network: input.confined.permissions().network
            == provenant_records::run::NetworkAccess::Provider,
        runtime_reads: Vec::new(),
        tool_exec_roots: vec![subtree("/bin"), subtree("/usr/bin")],
        ..Default::default()
    };
    profile
        .tool_exec_roots
        .extend(command.runtimes.iter().map(exact));
    let setup = Vec::new();
    let omitted_discovery = Vec::new();
    Ok(LaunchSpec {
        argv,
        env,
        stdin: Stdin::Null,
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup,
        omitted_discovery,
        user_skills: Default::default(),
        tool_requirements: Default::default(),
        git_wrapper: Default::default(),
        policy: input.policy.clone(),
        credential_roots: input.credential_roots.clone(),
        session_state: crate::SessionStateLocation::AttemptPrivate,
        profile: ProviderProfile {
            spec: profile,
            observations: Default::default(),
        },
        stream,
    })
}
