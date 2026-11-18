//! `provenant internal build-slot`: the lane's `cargo` shim runs this. Cargo
//! commands that compile and test runs take one host build slot first, then this process
//! runs cargo as its child and waits, so the slot is held until cargo exits
//! and not a moment longer. Test runs hold a slot even when they reuse build metadata. The slot descriptor is
//! close-on-exec: a daemon or
//! orphaned grandchild that outlives the build never keeps it.
//!
//! This is a build throttle for cooperative fair sharing of CPU and memory
//! among our own lanes, not a security boundary and not a hard cap. A lane
//! that calls `~/.cargo/bin/cargo`, rustc or a non-cargo build directly is not
//! gated, and a lane can hold a slot by locking its file on purpose. For that
//! reason an unexpected error opening or locking a slot lets the build run
//! without one, with a single warning.
//!
//! The slot directory and count come from the environment the host
//! wrote into the shim script, never from arguments a lane chooses. Slot files
//! live under the host state directory shared by every project, so
//! `max_concurrent_builds` observes occupancy across the host. Busy slots wait
//! a bounded time, then warn and proceed; `0` disables the cap. Cargo owns
//! its own build-lock queue.
use provenant_os::build_slots::BuildSlots;
use provenant_supervisor::census::{ProcessObservation, ProcessObserver, SystemProcessObserver};
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

// `PROVENANT_BUILD_SLOT` is set in a gated build to `pid:start`, the holder shim's
// pid and native process start time. A cargo run nested inside it (a build
// script, a test that spawns cargo) skips the gate only while that exact
// process is a live ancestor of the nested shim and is a provenant process,
// so it does not queue behind the slot its own parent holds. Naming any
// other process, even a live ancestor, is ignored.
// The shim's other variables (slot directory, slot count and the cargo
// subcommands the host lets this lane run) come from the host, written into
// the shim script from settings and never from a lane's arguments.
use provenant_core::consumer_env::{
    BUILD_ADMISSION_BACKEND, BUILD_ADMISSION_SOURCE, BUILD_DISK_LIMITS, BUILD_SLOT,
    BUILD_SLOT_STATE, BUILD_SLOTS, CARGO_SOURCE_WRITES, CARGO_SOURCE_WRITES_SOURCE, LOCAL_CARGO,
};

/// The compiling cargo commands a lane may run locally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Local {
    #[default]
    All,
    /// Warn on compiling commands other than `cargo check`.
    Check,
    None,
}
impl Local {
    /// An unset or unrecognised value is the permissive default, so a host
    /// that predates the setting behaves as before.
    pub(crate) fn from_env(env: &[(OsString, OsString)]) -> Self {
        match env
            .iter()
            .find(|(key, _)| key == OsStr::new(LOCAL_CARGO))
            .and_then(|(_, value)| value.to_str())
        {
            Some("check") => Self::Check,
            Some("none") => Self::None,
            _ => Self::All,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Check => "check",
            Self::None => "none",
        }
    }
    /// A one-line warning for commands outside the preferred scope.
    /// Subcommands that do not compile (`fmt`, `metadata`, `tree`) always may.
    pub(crate) fn warning(self, args: &[OsString]) -> Option<String> {
        let command = subcommand(args)?;
        let allowed = match self {
            Self::All => true,
            Self::Check => matches!(command, "check" | "c") || !GATED.contains(&command),
            Self::None => !GATED.contains(&command),
        };
        (!allowed).then(|| {
            format!(
                "cargo {command} exceeds lanes.local_cargo = {}; continuing. Set lanes.local_cargo or attempt.local_cargo to change this advisory policy",
                self.name()
            )
        })
    }
}

/// The first argument that is not an option or a toolchain selector.
fn subcommand(args: &[OsString]) -> Option<&str> {
    let mut args = args.iter().map(OsString::as_os_str);
    while let Some(arg) = args.next() {
        let arg = arg.to_str()?;
        if arg.starts_with('+') {
            continue;
        }
        if VALUED.contains(&arg) {
            args.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg);
    }
    None
}

/// What the host's shim script supplies, read from the environment.
pub(crate) struct Settings {
    pub state: PathBuf,
    pub slots: u64,
    pub wait: Duration,
}

const DEFAULT_WAIT: Duration = Duration::from_secs(15);
impl Settings {
    /// None when the host did not configure slots; the build then runs
    /// ungated.
    pub(crate) fn from_env(env: &[(OsString, OsString)]) -> Option<Self> {
        let get = |name: &str| {
            env.iter()
                .find(|(key, _)| key == OsStr::new(name))
                .map(|(_, value)| value)
        };
        let state = PathBuf::from(get(BUILD_SLOT_STATE)?);
        let number = |name: &str| get(name)?.to_str()?.parse::<u64>().ok();
        Some(Self {
            state: state.is_absolute().then_some(state)?,
            slots: number(BUILD_SLOTS).filter(|n| *n <= 64)?,
            wait: DEFAULT_WAIT,
        })
    }
}

/// The real cargo with its arguments, and the host's settings if any.
pub(crate) struct Gate<'a> {
    pub settings: Option<Settings>,
    pub local: Local,
    pub command: &'a [OsString],
}

/// Cargo subcommands that compile. Aliases and subcommands not named here run
/// without a slot. `run` is gated because it compiles first.
const GATED: &[&str] = &[
    "build", "b", "check", "c", "clippy", "test", "t", "bench", "doc", "d", "run", "r", "rustc",
    "rustdoc", "fix", "install", "package", "publish", "nextest", "llvm-cov", "miri",
];
/// Cargo options that take their value as the next argument.
const VALUED: &[&str] = &["-C", "-Z", "--config", "--color"];

/// Whether `args` (the arguments after `cargo`) name a compiling subcommand.
pub(crate) fn gated(args: &[OsString]) -> bool {
    let mut args = args.iter().map(OsString::as_os_str);
    while let Some(arg) = args.next() {
        let Some(arg) = arg.to_str() else {
            return false;
        };
        if arg.starts_with('+') {
            continue;
        }
        if VALUED.contains(&arg) {
            args.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return GATED.contains(&arg);
    }
    false
}

/// The process a nested build's marker must name.
#[derive(Debug, PartialEq, Eq)]
struct Holder {
    pid: i64,
    start: u64,
}
impl Holder {
    fn marker(&self) -> String {
        format!("{}:{}", self.pid, self.start)
    }
    fn parse(value: &str) -> Option<Self> {
        let (pid, start) = value.split_once(':')?;
        Some(Self {
            pid: pid.parse().ok().filter(|pid| *pid > 1)?,
            start: start.parse().ok()?,
        })
    }
}

/// This process's identity as a holder, and its observed command name. The
/// holder shim and a nested shim are the same binary, so the command name of
/// one is the command a holder must have.
fn own(observer: &dyn ProcessObserver) -> Option<(Holder, String)> {
    let pid = i64::from(std::process::id());
    match observer.observe(pid) {
        ProcessObservation::Present(fact) => Some((
            Holder {
                pid,
                start: fact.identity.native_start,
            },
            fact.command?,
        )),
        _ => None,
    }
}

/// Whether `holder` is a live ancestor of `start` that began when the marker
/// says and runs `command`: a pid reused by another process, a live ancestor
/// that is not a shim, and a zombie all fail.
fn ancestor_of(start: i64, holder: &Holder, command: &str, observer: &dyn ProcessObserver) -> bool {
    let mut current = start;
    // A process tree is shallow; the bound only stops a cycle.
    for _ in 0..256 {
        let ProcessObservation::Present(fact) = observer.observe(current) else {
            return false;
        };
        let parent = fact.parent_pid;
        if parent <= 1 {
            return false;
        }
        if parent == holder.pid {
            return match observer.observe(parent) {
                ProcessObservation::Present(found) => {
                    !found.zombie
                        && found.identity.native_start == holder.start
                        && found.command.as_deref() == Some(command)
                }
                _ => false,
            };
        }
        current = parent;
    }
    false
}

/// Whether the marker in `env` names a live ancestor shim.
fn nested(env: &[(OsString, OsString)]) -> bool {
    nested_with(env, &SystemProcessObserver)
}

fn nested_with(env: &[(OsString, OsString)], observer: &dyn ProcessObserver) -> bool {
    let Some(holder) = env
        .iter()
        .find(|(name, _)| name == BUILD_SLOT)
        .and_then(|(_, value)| Holder::parse(value.to_str()?))
    else {
        return false;
    };
    let Some((me, command)) = own(observer) else {
        return false;
    };
    ancestor_of(me.pid, &holder, &command, observer)
}

struct Taken {
    guard: Option<provenant_os::build_slots::SlotGuard>,
    #[allow(dead_code)] // Read by tests; the harness-free worktree driver omits them.
    status: Option<String>,
}

/// Take one slot, waiting up to `settings.wait` when every slot is busy.
/// `slots == 0` is not handled here; the caller skips the gate.
fn take_slot(settings: &Settings) -> Result<Taken, String> {
    let slots = BuildSlots::open(&settings.state).map_err(|error| error.to_string())?;
    if let Some(guard) = slots
        .try_acquire(settings.slots)
        .map_err(|error| error.to_string())?
    {
        return Ok(Taken {
            guard: Some(guard),
            status: None,
        });
    }
    let status = format!(
        "waiting for a host build slot ({} held of {})",
        slots.held(settings.slots),
        settings.slots
    );
    say(&status);
    let guard = slots
        .acquire_within(settings.slots, settings.wait)
        .map_err(|error| error.to_string())?;
    Ok(Taken {
        guard,
        status: Some(status),
    })
}

/// Take a slot when `gate.command` compiles, run it and return its exit
/// code. Busy slots wait a bounded time, then warn and the build proceeds.
pub(crate) fn run(gate: Gate<'_>) -> i32 {
    let env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let backend = env
        .iter()
        .find(|(name, _)| name == OsStr::new(BUILD_ADMISSION_BACKEND))
        .and_then(|(_, value)| value.to_str())
        .and_then(|value| serde_json::from_str::<provenant_config::PortBackend>(value).ok());
    let source = env
        .iter()
        .find(|(name, _)| name == OsStr::new(BUILD_ADMISSION_SOURCE))
        .and_then(|(_, value)| value.to_str())
        .and_then(|value| serde_json::from_str::<provenant_config::PortSource>(value).ok());
    let (backend, source) = match (backend, source) {
        (Some(backend), Some(source)) => (backend, source),
        _ => {
            return report("build admission port metadata is unavailable");
        }
    };
    let nested = nested(&env);
    let execute_backend = backend.clone();
    run_with(
        gate,
        env,
        nested,
        backend,
        |settings| take_slot(settings).map(|taken| taken.guard),
        move |program, args, env| {
            if !gated(args) || nested {
                return provenant_os::sys::run_wait(program, args, env)
                    .map_err(|error| error.to_string());
            }
            provenant_config::dispatch_port_backend(
                &execute_backend,
                || {
                    provenant_os::sys::run_wait(program, args, env)
                        .map_err(|error| error.to_string())
                },
                |command| {
                    let Some(wrapper) = command.argv.first() else {
                        return Err("repository build wrapper has no executable".into());
                    };
                    let mut arguments: Vec<OsString> = command
                        .argv
                        .iter()
                        .skip(1)
                        .map(|argument| OsString::from(argument.as_str()))
                        .collect();
                    arguments.push(program.as_os_str().to_owned());
                    arguments.extend_from_slice(args);
                    let mut explicit = Vec::new();
                    for name in [
                        "CARGO_TARGET_DIR",
                        "CARGO_BUILD_JOBS",
                        "CARGO_ENCODED_RUSTFLAGS",
                        "RUSTFLAGS",
                        "RUSTDOCFLAGS",
                        "RUSTC",
                        "RUSTDOC",
                        "RUSTC_WRAPPER",
                        "RUSTC_WORKSPACE_WRAPPER",
                        provenant_core::consumer_env::BUILD_SLOT,
                    ] {
                        if let Some((_, value)) =
                            env.iter().find(|(key, _)| key == OsStr::new(name))
                        {
                            explicit.push((OsString::from(name), value.clone()));
                        }
                    }
                    explicit.push((
                        OsString::from(provenant_core::consumer_env::PORT_PHASE),
                        OsString::from("build"),
                    ));
                    explicit.push((
                        OsString::from(provenant_core::consumer_env::PORT_SOURCE),
                        OsString::from(source_name(source)),
                    ));
                    let wrapper_env =
                        provenant_config::port_command_environment(env.iter().cloned(), &explicit);
                    let code =
                        provenant_os::sys::run_wait(Path::new(wrapper), &arguments, &wrapper_env)
                            .map_err(|error| error.to_string())?;
                    if command.exit_codes.success.contains(&code) {
                        return Ok(0);
                    }
                    if command.exit_codes.unknown.contains(&code)
                        || command.exit_codes.other == provenant_config::ExitDisposition::Unknown
                    {
                        warn(&format!(
                            "repository build admission returned an unknown outcome (source: {:?})",
                            source
                        ));
                    } else {
                        warn(&format!(
                            "repository build admission refused cargo (source: {:?})",
                            source
                        ));
                    }
                    Ok(if code == 0 { 1 } else { code })
                },
                || {
                    provenant_os::sys::run_wait(program, args, env)
                        .map_err(|error| error.to_string())
                },
            )
        },
        |message| warn(&message),
    )
}

fn source_name(source: provenant_config::PortSource) -> &'static str {
    match source {
        provenant_config::PortSource::Builtin => "builtin",
        provenant_config::PortSource::Host => "host",
        provenant_config::PortSource::Project => "project",
    }
}

fn run_with(
    gate: Gate<'_>,
    mut env: Vec<(OsString, OsString)>,
    nested: bool,
    backend: provenant_config::PortBackend,
    mut acquire: impl FnMut(&Settings) -> Result<Option<provenant_os::build_slots::SlotGuard>, String>,
    mut execute: impl FnMut(&Path, &[OsString], &[(OsString, OsString)]) -> Result<i32, String>,
    mut warning: impl FnMut(&str),
) -> i32 {
    let Some((program, args)) = gate.command.split_first() else {
        return report("no command to run");
    };
    // A nested cargo (a build script, a test) runs under a parent the policy
    // already let through.
    if !nested && let Some(message) = gate.local.warning(args) {
        warning(&message);
    }
    let gate_this = gated(args) && !nested;
    if gate_this
        && matches!(&backend, provenant_config::PortBackend::Builtin)
        && let Err(message) = check_disk(&env)
    {
        return report(&message);
    }
    let mut held = None;
    let disabled = gate
        .settings
        .as_ref()
        .is_some_and(|settings| settings.slots == 0);
    if gate_this && !disabled {
        let admission = provenant_config::dispatch_port_backend(
            &backend,
            || match &gate.settings {
                Some(settings) => acquire(settings),
                None => Err("the host's build slot settings are missing or invalid".into()),
            },
            |_| Ok(None),
            || Ok(None),
        );
        match admission {
            Ok(Some(slot)) => held = Some(slot),
            Ok(None) if matches!(&backend, provenant_config::PortBackend::Builtin) => warning(
                "bounded wait ended with every build slot busy; building without one. Cargo manages its own build locks",
            ),
            Err(error) if matches!(&backend, provenant_config::PortBackend::Builtin) => {
                warning(&error)
            }
            _ => {}
        }
    }
    if (held.is_some() || matches!(&backend, provenant_config::PortBackend::Command { .. }))
        && gate_this
        && let Some((me, _)) = own(&SystemProcessObserver)
    {
        env.retain(|(name, _)| name != BUILD_SLOT);
        env.push((BUILD_SLOT.into(), me.marker().into()));
    }
    // `held` stays open while the child runs; the descriptor is the slot.
    let code = match execute(Path::new(program), args, &env) {
        Ok(code) => code,
        Err(error) => report(&format!("cannot start cargo: {error}")),
    };
    drop(held);
    if code != 0
        && env
            .iter()
            .any(|(key, value)| key == CARGO_SOURCE_WRITES && value == "false")
    {
        let source = env
            .iter()
            .find(|(key, _)| key == CARGO_SOURCE_WRITES_SOURCE)
            .and_then(|(_, value)| value.to_str())
            .unwrap_or("compiled default");
        warning(&format!(
            "Cargo sources are read-only ({} = false; source: {source}). If a dependency is missing offline, fetch it on the host or enable this setting with network access for the lane.",
            provenant_confinement::cargo::SOURCE_WRITES_KNOB
        ));
    }
    code
}

/// New builds sample their output volume once. Existing builds are never signalled.
fn check_disk(env: &[(OsString, OsString)]) -> Result<(), String> {
    let value = |name: &str| {
        env.iter()
            .find(|(key, _)| key == OsStr::new(name))
            .and_then(|(_, value)| value.to_str())
    };
    let Some(encoded_gate) = value(BUILD_DISK_LIMITS) else {
        return Ok(());
    };
    let observed = (|| {
        let gate: provenant_config::DiskGate =
            serde_json::from_str(encoded_gate).map_err(|error| error.to_string())?;
        let target = value("CARGO_TARGET_DIR").map_or_else(|| Path::new("."), Path::new);
        let mut root = target;
        while !root.exists() {
            root = root.parent().unwrap_or(Path::new("."));
        }
        let free = provenant_os::fs::available_bytes_at(root).map_err(|error| error.to_string())?;
        Ok::<_, String>((gate, free))
    })();
    match observed {
        // The host's declared decision, shared with submission and admission.
        Ok((gate, free)) => match gate.decide(free) {
            provenant_records::policy::Effect::Proceed => Ok(()),
            provenant_records::policy::Effect::Warn(message) => {
                say(&format!("warning: {message}; building"));
                Ok(())
            }
            provenant_records::policy::Effect::Refuse(diagnostic)
            | provenant_records::policy::Effect::Stop(diagnostic) => Err(format!(
                "{}; refusing to start a new build",
                diagnostic.detail.unwrap_or_default()
            )),
        },
        Err(error) => {
            say(&format!(
                "build disk limits are invalid or the disk observation is unavailable ({error}); the build may proceed without the disk gate"
            ));
            Ok(())
        }
    }
}

/// The lane's own stderr is the only place a wait shows; a closed stream is
/// not an error.
fn say(message: &str) {
    let _ = writeln!(std::io::stderr().lock(), "provenant: {message}");
}

fn report(message: &str) -> i32 {
    say(message);
    1
}

/// Slots are a fairness aid, not a boundary, so a slot that cannot be opened
/// or locked does not stop the build.
fn warn(error: &dyn std::fmt::Display) {
    say(&format!(
        "build slot unavailable, building without one: {error}"
    ));
}

#[cfg(test)]
#[allow(
    unused_imports,
    dead_code,
    reason = "the custom worktree test driver includes this library without the test harness"
)]
mod tests {
    fn args(list: &[&str]) -> Vec<std::ffi::OsString> {
        list.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn local_cargo_policy_warns_by_subcommand() {
        use super::Local;
        for command in [
            &["test"][..],
            &["+stable", "build", "--release"],
            &["nextest", "run"],
        ] {
            assert_eq!(Local::All.warning(&args(command)), None);
            let message = Local::Check.warning(&args(command)).unwrap();
            assert!(message.contains("continuing") && message.contains("lanes.local_cargo"));
            assert!(!message.contains('\n'));
            assert!(Local::None.warning(&args(command)).is_some());
        }
        for command in [
            &["check", "-p", "x"][..],
            &["c"],
            &["--color", "never", "check"],
        ] {
            assert_eq!(Local::Check.warning(&args(command)), None);
            assert!(Local::None.warning(&args(command)).is_some());
        }
        for command in [&["fmt"][..], &["metadata"], &[]] {
            assert_eq!(Local::None.warning(&args(command)), None);
        }
    }

    use super::*;
    use provenant_supervisor::census::{ProcessFact, ProcessIdentity};
    use std::collections::HashMap;

    #[test]
    fn cargo_failures_keep_exit_codes_and_explain_read_only_sources() {
        let mut binding =
            provenant_confinement::cargo::CargoHome::resolve(Path::new("/fixture/home"), |_| None)
                .unwrap();
        for setting in [None, Some(false), Some(true)] {
            binding.configure_source_writes(setting);
            for exit in [0, 101] {
                let command = [OsString::from("cargo"), OsString::from("metadata")];
                let gate = Gate {
                    settings: None,
                    local: Local::All,
                    command: &command,
                };
                let mut env = Vec::new();
                binding.source_environment(
                    provenant_confinement::permissions::NetworkAccess::Open,
                    &mut env,
                );
                let mut warnings = Vec::new();
                let actual = run_with(
                    gate,
                    env,
                    false,
                    provenant_config::PortBackend::Builtin,
                    |_| unreachable!(),
                    |_, _, _| Ok(exit),
                    |message| warnings.push(message.to_owned()),
                );
                assert_eq!(actual, exit);
                let diagnostic = warnings.iter().find(|message| {
                    message.contains(provenant_confinement::cargo::SOURCE_WRITES_KNOB)
                });
                assert_eq!(diagnostic.is_some(), exit != 0 && !binding.source_writes);
                if let Some(diagnostic) = diagnostic {
                    assert!(diagnostic.contains(binding.source_writes_source));
                }
            }
        }
    }

    // Spec: docs/specs/run.md#lane-verification-and-the-host-build-cap
    #[test]
    fn local_cargo_scope_warns_and_runs_the_command() {
        let command = [OsString::from("cargo"), OsString::from("test")];
        let gate = Gate {
            settings: None,
            local: Local::Check,
            command: &command,
        };
        let mut warnings = Vec::new();
        let mut ran = false;
        let code = run_with(
            gate,
            Vec::new(),
            false,
            provenant_config::PortBackend::Builtin,
            |_| Ok(None),
            |_, _, _| {
                ran = true;
                Ok(0)
            },
            |message| warnings.push(message.to_owned()),
        );
        assert_eq!(code, 0);
        assert!(ran);
        assert!(
            warnings
                .iter()
                .any(|message| message.contains("lanes.local_cargo"))
        );
    }

    #[test]
    fn all_busy_build_slots_warn_once_and_still_run_the_child() {
        let state = tempfile::tempdir().unwrap();
        BuildSlots::provision(state.path(), 1).unwrap();
        let slots = BuildSlots::open(state.path()).unwrap();
        let held = slots.try_acquire(1).unwrap().unwrap();
        let command = [OsString::from("cargo"), OsString::from("check")];
        let gate = Gate {
            settings: Some(Settings {
                state: state.path().to_owned(),
                slots: 1,
                wait: std::time::Duration::ZERO,
            }),
            local: Local::All,
            command: &command,
        };
        let mut warnings = Vec::new();
        let mut ran = false;
        let code = run_with(
            gate,
            Vec::new(),
            false,
            provenant_config::PortBackend::Builtin,
            |settings| {
                let slots = BuildSlots::open(&settings.state).map_err(|error| error.to_string())?;
                slots
                    .try_acquire(settings.slots)
                    .map_err(|error| error.to_string())
            },
            |_, _, _| {
                ran = true;
                Ok(0)
            },
            |message| warnings.push(message.to_owned()),
        );
        assert_eq!(code, 0);
        assert!(ran);
        assert_eq!(warnings.len(), 1);
        drop(held);
    }

    #[test]
    fn six_lane_builds_on_two_slots_each_report_a_wait() {
        let state = tempfile::tempdir().unwrap();
        BuildSlots::provision(state.path(), 2).unwrap();
        let slots = BuildSlots::open(state.path()).unwrap();
        let _held = [
            slots.try_acquire(2).unwrap().unwrap(),
            slots.try_acquire(2).unwrap().unwrap(),
        ];
        let mut threads = Vec::new();
        for _ in 0..4 {
            let path = state.path().to_owned();
            threads.push(std::thread::spawn(move || {
                super::take_slot(&super::Settings {
                    state: path,
                    slots: 2,
                    wait: std::time::Duration::from_millis(200),
                })
                .unwrap()
            }));
        }
        for thread in threads {
            let taken = thread.join().unwrap();
            assert!(taken.guard.is_none());
            let status = taken.status.expect("status");
            assert!(status.contains("waiting for a host build slot"));
            assert!(status.contains("2 held of 2"));
        }
    }

    #[test]
    fn a_disabled_cap_does_not_take_a_slot() {
        let command = [OsString::from("cargo"), OsString::from("check")];
        let gate = Gate {
            settings: Some(Settings {
                state: std::path::PathBuf::from("/unused"),
                slots: 0,
                wait: std::time::Duration::ZERO,
            }),
            local: Local::All,
            command: &command,
        };
        let mut warnings = Vec::new();
        let code = run_with(
            gate,
            Vec::new(),
            false,
            provenant_config::PortBackend::Builtin,
            |_| unreachable!("disabled cap does not acquire"),
            |_, _, _| Ok(0),
            |message| warnings.push(message.to_owned()),
        );
        assert_eq!(code, 0);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn compiling_subcommands_are_gated_whatever_their_options() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        for list in [
            &["build"][..],
            &["+nightly", "test", "--workspace"],
            &["--locked", "clippy"],
            &["-C", "tools/front-door", "check"],
            &["-Z", "unstable-options", "build"],
            &["--config", "build.jobs=2", "nextest", "run"],
            &["t"],
            &["run", "--", "build"],
            &["r"],
        ] {
            assert!(gated(&args(list)), "{list:?}");
        }
    }

    #[test]
    fn nextest_run_with_reused_build_metadata_holds_a_slot() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(gated(&args(&[
            "nextest",
            "run",
            "--cargo-metadata",
            "cargo.json",
            "--binaries-metadata",
            "bins.json"
        ])));
        assert!(gated(&args(&[
            "nextest",
            "run",
            "--cargo-metadata",
            "cargo.json"
        ])));
        assert!(gated(&args(&[
            "nextest",
            "list",
            "--cargo-metadata",
            "cargo.json",
            "--binaries-metadata",
            "bins.json"
        ])));
        assert!(gated(&args(&[
            "nextest",
            "run",
            "--",
            "--cargo-metadata",
            "--binaries-metadata"
        ])));
    }

    #[test]
    fn other_subcommands_and_bare_options_are_not_gated() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        for list in [
            &[][..],
            &["fmt", "--check"],
            &["metadata"],
            &["--version"],
            &["--list"],
            &["-C", "dir"],
            &["xtask", "build"],
        ] {
            assert!(!gated(&args(list)), "{list:?}");
        }
    }

    #[test]
    fn settings_come_from_the_host_environment_and_are_validated() {
        let env = |list: &[(&str, &str)]| {
            list.iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v)))
                .collect::<Vec<_>>()
        };
        let good = env(&[(BUILD_SLOT_STATE, "/s"), (BUILD_SLOTS, "2")]);
        let settings = Settings::from_env(&good).unwrap();
        assert_eq!(settings.slots, 2);
        assert_eq!(
            Settings::from_env(&env(&[(BUILD_SLOT_STATE, "/s"), (BUILD_SLOTS, "0")]))
                .unwrap()
                .slots,
            0
        );
        for bad in [
            env(&[]),
            env(&[(BUILD_SLOT_STATE, "relative"), (BUILD_SLOTS, "2")]),
            env(&[(BUILD_SLOT_STATE, "/s"), (BUILD_SLOTS, "65")]),
            env(&[(BUILD_SLOT_STATE, "/s"), (BUILD_SLOTS, "x")]),
        ] {
            assert!(Settings::from_env(&bad).is_none());
        }
    }

    #[allow(
        dead_code,
        reason = "the custom worktree test driver includes this library without the test harness"
    )]
    struct Tree(HashMap<i64, Node>);
    #[allow(
        dead_code,
        reason = "the custom worktree test driver includes this library without the test harness"
    )]
    #[derive(Clone, Copy)]
    struct Node {
        parent: i64,
        zombie: bool,
        start: u64,
        command: &'static str,
    }
    #[allow(
        dead_code,
        reason = "the custom worktree test driver includes this library without the test harness"
    )]
    fn node(parent: i64, start: u64, command: &'static str) -> Node {
        Node {
            parent,
            zombie: false,
            start,
            command,
        }
    }
    impl ProcessObserver for Tree {
        fn boot_identity(&self) -> Result<String, provenant_supervisor::census::CensusError> {
            Ok("boot".into())
        }
        fn census(
            &self,
            _: usize,
        ) -> Result<provenant_supervisor::census::Census, provenant_supervisor::census::CensusError>
        {
            Err(provenant_supervisor::census::CensusError::InvalidRecord)
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            match self.0.get(&pid) {
                Some(node) => ProcessObservation::Present(ProcessFact {
                    identity: ProcessIdentity {
                        boot: "boot".into(),
                        pid,
                        native_start: node.start,
                        group_id: pid,
                        session_id: pid,
                    },
                    parent_pid: node.parent,
                    zombie: node.zombie,
                    cpu_micros: None,
                    command: Some(node.command.into()),
                }),
                None => ProcessObservation::Absent,
            }
        }
    }

    #[allow(
        dead_code,
        reason = "the custom worktree test driver includes this library without the test harness"
    )]
    fn holder(pid: i64, start: u64) -> Holder {
        Holder { pid, start }
    }

    #[test]
    fn only_the_exact_live_shim_ancestor_counts_as_the_holder() {
        // 500 (nested shim) -> 400 (cargo) -> 300 (holder shim) -> 200 (sh) -> 1
        let tree = Tree(HashMap::from([
            (500, node(400, 50, "provenant")),
            (400, node(300, 40, "cargo")),
            (300, node(200, 30, "provenant")),
            (200, node(1, 20, "sh")),
            (900, node(1, 90, "provenant")),
        ]));
        assert!(ancestor_of(500, &holder(300, 30), "provenant", &tree));
        assert!(
            !ancestor_of(500, &holder(300, 31), "provenant", &tree),
            "a pid reused after the holder: another start time"
        );
        assert!(
            !ancestor_of(500, &holder(400, 40), "provenant", &tree),
            "a live ancestor that is not a shim"
        );
        assert!(
            !ancestor_of(500, &holder(200, 20), "provenant", &tree),
            "the shell above the shim"
        );
        assert!(!ancestor_of(500, &holder(900, 90), "provenant", &tree));
        assert!(!ancestor_of(500, &holder(777, 1), "provenant", &tree));
        let mut dead = tree.0.clone();
        dead.insert(
            300,
            Node {
                zombie: true,
                ..node(200, 30, "provenant")
            },
        );
        assert!(
            !ancestor_of(500, &holder(300, 30), "provenant", &Tree(dead)),
            "a zombie holder is gone"
        );
    }
}
