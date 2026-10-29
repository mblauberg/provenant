//! Configured commands share the provider launch and settlement machinery.
use crate::launch::{Access, Session, base_env, exact, node_tools, rust_tools, subtree};
use crate::{LaunchSpec, PrepareInput, ProviderError, ProviderEvent, Stdin, StreamState};
use provenant_confinement::permissions::{ProviderProfile, ProviderProfileSpec};

pub struct Command;

/// Trusted argv and its host-resolved invocation path. Keeping the final
/// symlink name preserves argv[0] for rustup, corepack and other multicall tools.
#[derive(Clone, Copy)]
pub struct Invocation<'a> {
    pub argv: &'a [String],
    pub program: &'a std::path::Path,
}

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &Command,
    quota_source: None,
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
    if access != Access::ReadOnly
        || session != Session::Fresh
        || !input.route.model.is_empty()
        || input.route.effort.is_some()
        || input.route.account_key.is_some()
        || input
            .confined
            .permissions()
            .selected_authentication
            .is_some()
    {
        return Err(ProviderError::UnsupportedPolicy);
    }
    let command = input
        .command
        .filter(|command| {
            !command.argv.is_empty() && !command.argv[0].is_empty() && command.program.is_absolute()
        })
        .ok_or(ProviderError::UnsupportedPolicy)?;
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
        &format!("{}:/usr/bin:/bin", input.home.join(".cargo/bin").display()),
    );
    env.extend([
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]);
    let mut profile = ProviderProfileSpec {
        process_executables: vec![exact("/bin/sh"), exact(input.executable)],
        allow_process_fork: true,
        deny_keychain_access: true,
        deny_network: input.confined.permissions().network
            == provenant_records::run::NetworkAccess::Provider,
        runtime_reads: crate::dylib::linked_libraries(input.executable)
            .into_iter()
            .map(exact)
            .collect(),
        tool_exec_roots: vec![subtree("/bin"), subtree("/usr/bin")],
        ..Default::default()
    };
    let mut setup = Vec::new();
    if [".cargo/bin", ".rustup/toolchains"]
        .iter()
        .all(|path| input.home.join(path).is_dir())
    {
        rust_tools(
            input,
            access,
            &mut profile.runtime_reads,
            &mut profile.tool_exec_roots,
            &mut env,
            &mut setup,
        );
    } else {
        crate::launch::system_tools(
            access,
            &mut profile.runtime_reads,
            &mut profile.tool_exec_roots,
            &mut env,
        );
        profile.tool_exec_roots.push(subtree(input.cwd));
    }
    node_tools(
        input.home,
        access,
        scratch,
        &mut profile.runtime_reads,
        &mut profile.tool_exec_roots,
        &mut env,
    );
    Ok(LaunchSpec {
        argv,
        env,
        stdin: Stdin::Null,
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup,
        git_wrapper: Default::default(),
        profile: ProviderProfile {
            spec: profile,
            observations: Default::default(),
        },
        stream,
    })
}
