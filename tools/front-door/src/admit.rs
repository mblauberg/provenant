//! `provenant admit CLASS -- CMD ARGS...`: runs a command under a named admission class slot.
//!
//! A command acquires one host admission slot in the named class first, then this process
//! runs the child command and waits, so the slot is held until the child exits and not a
//! moment longer. Test runs and builds share the host-wide slot budget. The slot descriptor
//! is close-on-exec: a daemon or orphaned grandchild that outlives the command never keeps it.
//!
//! Re-entrancy: If a command is already inside an admitted slot (nested cargo from a build
//! script, nextest spawning cargo), it does not deadlock; the live ancestor check detects
//! re-entrancy and passes through.
use provenant_core::consumer_env::{
    ADMISSION_BACKEND, ADMISSION_CLASS, ADMISSION_DISK_LIMITS, ADMISSION_SLOT,
    ADMISSION_SLOT_STATE, ADMISSION_SLOTS, ADMISSION_SOURCE, ADMISSION_WAIT_DIR,
    BUILD_ADMISSION_BACKEND, BUILD_ADMISSION_SOURCE, BUILD_DISK_LIMITS, BUILD_SLOT,
    BUILD_SLOT_STATE, BUILD_SLOTS, BUILD_WAIT_DIR, CARGO_SOURCE_WRITES, CARGO_SOURCE_WRITES_SOURCE,
    LOCAL_CARGO,
};
use provenant_os::admission::{
    AdmissionSlots, HeldGuard, HostHeldGuard, mark_held, mark_host_held, mark_host_waiting,
    mark_waiting,
};
use provenant_supervisor::census::{ProcessObservation, ProcessObserver, SystemProcessObserver};
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
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

const VALUED: &[&str] = &["-C", "-Z", "--config", "--color"];
const GATED: &[&str] = &[
    "build", "b", "check", "c", "clippy", "test", "t", "bench", "doc", "d", "run", "r", "rustc",
    "rustdoc", "fix", "install", "package", "publish", "nextest", "llvm-cov", "miri",
];

#[derive(Debug, PartialEq, Eq)]
pub struct Holder {
    pub pid: i64,
    pub start: u64,
}

impl Holder {
    pub fn marker(&self) -> String {
        format!("{}:{}", self.pid, self.start)
    }
    pub fn parse(value: &str) -> Option<Self> {
        let (pid, start) = value.split_once(':')?;
        Some(Self {
            pid: pid.parse().ok().filter(|pid| *pid > 1)?,
            start: start.parse().ok()?,
        })
    }
}

pub fn own(observer: &dyn ProcessObserver) -> Option<(Holder, String)> {
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

fn ancestor_of(start: i64, holder: &Holder, command: &str, observer: &dyn ProcessObserver) -> bool {
    let mut current = start;
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

pub fn nested_for_class(
    class: &str,
    env: &[(OsString, OsString)],
    observer: &dyn ProcessObserver,
) -> bool {
    let class_key_upper = format!("PROVENANT_ADMISSION_SLOT_{}", class.to_ascii_uppercase());
    let class_key_lower = format!("PROVENANT_ADMISSION_SLOT_{}", class.to_ascii_lowercase());
    let holder = env
        .iter()
        .find(|(name, _)| {
            name == OsStr::new(&class_key_upper)
                || name == OsStr::new(&class_key_lower)
                || name == OsStr::new(ADMISSION_SLOT)
                || (class == "build" && name == OsStr::new(BUILD_SLOT))
        })
        .and_then(|(_, value)| {
            let s = value.to_str()?;
            // Handle optional "class:pid:start" format
            let s = if let Some((c, rest)) = s.split_once(':') {
                if rest.contains(':') && c == class {
                    rest
                } else {
                    s
                }
            } else {
                s
            };
            Holder::parse(s)
        });

    let Some(holder) = holder else {
        return false;
    };
    let Some((me, command)) = own(observer) else {
        return false;
    };
    ancestor_of(me.pid, &holder, &command, observer)
}

#[allow(dead_code)]
enum HeldGuardVariant {
    Scratch(HeldGuard),
    Host(HostHeldGuard),
}

fn sample_rss_bytes() -> Option<u64> {
    if let Some(path) = std::env::var_os(provenant_core::consumer_env::PROCESS_TREE)
        && let Ok(sample) = provenant_supervisor::process_tree::read(Path::new(&path), None)
    {
        let total: u64 = sample.processes.iter().map(|p| p.rss_bytes).sum();
        if total > 0 {
            return Some(total);
        }
    }
    None
}

fn check_disk_and_trim(
    disk_floor: Option<u64>,
    trim_action: Option<&str>,
    env: &[(OsString, OsString)],
) -> Result<(), String> {
    let value = |name: &str| {
        env.iter()
            .find(|(key, _)| key == OsStr::new(name))
            .and_then(|(_, value)| value.to_str())
    };

    let target = value("CARGO_TARGET_DIR").map_or_else(|| Path::new("."), Path::new);
    let mut root = target;
    while !root.exists() {
        root = root.parent().unwrap_or(Path::new("."));
    }

    if let Some(floor) = disk_floor {
        let free = provenant_os::fs::available_bytes_at(root).map_err(|e| e.to_string())?;
        if free < floor {
            if let Some(action) = trim_action {
                say(&format!(
                    "available disk ({free} B) is below floor ({floor} B); running trim action: {action}"
                ));
                let _ = provenant_os::sys::run_wait(
                    Path::new("/bin/sh"),
                    &[OsString::from("-c"), OsString::from(action)],
                    &[],
                );
            }
            let free_after =
                provenant_os::fs::available_bytes_at(root).map_err(|e| e.to_string())?;
            if free_after < floor {
                say(&format!(
                    "warning: available disk ({free_after} B) remains below floor ({floor} B)"
                ));
            }
        }
    }

    // Also check BUILD_DISK_LIMITS / ADMISSION_DISK_LIMITS policy gate if present
    let gate_raw = value(ADMISSION_DISK_LIMITS).or_else(|| value(BUILD_DISK_LIMITS));
    if let Some(encoded_gate) = gate_raw {
        let gate: Result<provenant_config::DiskGate, _> = serde_json::from_str(encoded_gate);
        if let Ok(gate) = gate {
            let free = provenant_os::fs::available_bytes_at(root).map_err(|e| e.to_string())?;
            match gate.decide(free) {
                provenant_records::policy::Effect::Proceed => {}
                provenant_records::policy::Effect::Warn(message) => {
                    say(&format!("warning: {message}; proceeding"));
                }
                provenant_records::policy::Effect::Refuse(diagnostic) => {
                    return Err(format!(
                        "{}; refusing to start command",
                        diagnostic.detail.unwrap_or_default()
                    ));
                }
            }
        }
    }
    Ok(())
}

fn check_memory_and_trim(memory_limit: Option<u64>, trim_action: Option<&str>, class: &str) {
    let Some(limit) = memory_limit else {
        return;
    };
    let Some(rss) = sample_rss_bytes() else {
        return;
    };
    if rss > limit {
        if let Some(action) = trim_action {
            say(&format!(
                "process resident memory ({rss} B) exceeds class '{class}' limit ({limit} B); running trim action: {action}"
            ));
            let _ = provenant_os::sys::run_wait(
                Path::new("/bin/sh"),
                &[OsString::from("-c"), OsString::from(action)],
                &[],
            );
        }
        if let Some(rss_after) = sample_rss_bytes()
            && rss_after > limit
        {
            say(&format!(
                "warning: process resident memory ({rss_after} B) exceeds class '{class}' limit ({limit} B)"
            ));
        }
    }
}

pub fn run(class: &str, command: &[OsString], env: &[(OsString, OsString)]) -> i32 {
    if let Err(error) = provenant_os::admission::validate_class_name(class) {
        return report(&error.to_string());
    }
    let Some((program, args)) = command.split_first() else {
        return report("no command to run");
    };

    let mut env: Vec<(OsString, OsString)> = env.to_vec();
    let get_env = |name: &str| {
        env.iter()
            .find(|(key, _)| key == OsStr::new(name))
            .and_then(|(_, v)| v.to_str())
    };

    // State root
    let state_buf = get_env(ADMISSION_SLOT_STATE)
        .or_else(|| get_env(BUILD_SLOT_STATE))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("PROVENANT_STATE_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));

    // Resolve class configuration
    let resolved = get_env("PROVENANT_ADMISSION_CONFIG")
        .and_then(|value| {
            serde_json::from_str::<provenant_config::ResolvedAdmissionClass>(value).ok()
        })
        .filter(|config| config.name == class)
        .unwrap_or_else(|| {
            provenant_config::ResolvedAdmissionClass::resolve(class, &state_buf, None)
        });

    // Override slots from environment if explicitly provided by host shim
    let slots_count = (class == "build")
        .then(|| get_env(ADMISSION_SLOTS).or_else(|| get_env(BUILD_SLOTS)))
        .flatten()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(resolved.slots);

    let wait_seconds = std::env::var("PROVENANT_ADMISSION_WAIT_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(resolved.wait_seconds);
    let wait_duration = Duration::from_secs(wait_seconds);

    // Admission backend and source
    let raw_backend = get_env(ADMISSION_BACKEND).or_else(|| get_env(BUILD_ADMISSION_BACKEND));
    let raw_source = get_env(ADMISSION_SOURCE).or_else(|| get_env(BUILD_ADMISSION_SOURCE));

    let (backend, source) = match (raw_backend, raw_source) {
        (Some(b), Some(s)) => {
            let parsed_b = serde_json::from_str::<provenant_config::PortBackend>(b);
            let parsed_s = serde_json::from_str::<provenant_config::PortSource>(s);
            match (parsed_b, parsed_s) {
                (Ok(backend), Ok(source)) => (backend, source),
                _ => return report("build admission port metadata is unavailable"),
            }
        }
        (Some(_), None) | (None, Some(_)) => {
            return report("build admission port metadata is unavailable");
        }
        (None, None) => (
            provenant_config::PortBackend::Builtin,
            provenant_config::PortSource::Builtin,
        ),
    };

    let is_nested = nested_for_class(class, &env, &SystemProcessObserver);

    // Local cargo warning if class == "build"
    if class == "build" && !is_nested {
        let local = Local::from_env(&env);
        if let Some(msg) = local.warning(args) {
            say(&format!("warning: {msg}"));
        }
    }

    // Disk & Memory checks with trim
    let requires_disk_check =
        class != "build" || subcommand(args).is_some_and(|cmd| GATED.contains(&cmd));
    if !is_nested && requires_disk_check {
        if let Err(e) = check_disk_and_trim(
            resolved.disk_floor_bytes,
            resolved.trim_action.as_deref(),
            &env,
        ) {
            return report(&e);
        }
        check_memory_and_trim(
            resolved.memory_per_slot_bytes,
            resolved.trim_action.as_deref(),
            class,
        );
    }

    let scratch_wait_dir = get_env(ADMISSION_WAIT_DIR).or_else(|| get_env(BUILD_WAIT_DIR));

    let mut held_guard: Option<provenant_os::admission::SlotGuard> = None;
    let mut _holder_tracking: Option<HeldGuardVariant> = None;

    let warn_slot_unavailable = |err: &dyn std::fmt::Display| {
        let slot_desc = if class == "build" {
            "build slot"
        } else {
            "admission slot"
        };
        let proceed_desc = if class == "build" {
            "building without one"
        } else {
            "proceeding without one"
        };
        say(&format!("{slot_desc} unavailable, {proceed_desc}: {err}"));
    };

    if !is_nested && slots_count > 0 {
        match backend {
            provenant_config::PortBackend::Builtin => {
                let slots = match AdmissionSlots::open(&state_buf, class) {
                    Ok(s) => s,
                    Err(_) => {
                        let _ = AdmissionSlots::provision(&state_buf, class, slots_count);
                        match AdmissionSlots::open(&state_buf, class) {
                            Ok(s) => s,
                            Err(e) => {
                                warn_slot_unavailable(&e);
                                return execute_and_forward(program, args, &env);
                            }
                        }
                    }
                };

                match slots.try_acquire(slots_count) {
                    Ok(Some(guard)) => {
                        held_guard = Some(guard);
                    }
                    Ok(None) => {
                        // Busy: wait up to wait_duration
                        let status_msg = format!(
                            "waiting for a host {class} slot ({} held of {})",
                            slots.held(slots_count),
                            slots_count
                        );
                        let my_pid = i64::from(std::process::id());
                        let attempt_id = get_env("PROVENANT_ATTEMPT_ID")
                            .or_else(|| get_env("PROVENANT_PARENT_ATTEMPT_ID"));

                        let _wait_guard = if let Some(scratch) = scratch_wait_dir {
                            mark_waiting(Path::new(scratch), class, my_pid, attempt_id).ok()
                        } else {
                            None
                        };
                        let _host_wait_guard = if scratch_wait_dir.is_none() {
                            mark_host_waiting(&state_buf, class, my_pid).ok()
                        } else {
                            None
                        };

                        say(&status_msg);

                        match slots.acquire_within(slots_count, wait_duration) {
                            Ok(Some(guard)) => {
                                held_guard = Some(guard);
                            }
                            Ok(None) => {
                                let msg = if class == "build" {
                                    "bounded wait ended with every build slot busy; building without one. Cargo manages its own build locks"
                                } else {
                                    "bounded wait ended with every slot busy; proceeding without one"
                                };
                                warn_slot_unavailable(&msg);
                            }
                            Err(e) => {
                                warn_slot_unavailable(&e);
                            }
                        }
                    }
                    Err(e) => {
                        warn_slot_unavailable(&e);
                    }
                }
            }
            provenant_config::PortBackend::Command {
                command: ref repo_cmd,
            } => {
                return execute_command_port(repo_cmd, program, args, &env, source);
            }
            provenant_config::PortBackend::None => {}
        }
    }

    // If held, track holding metadata
    if let Some(ref guard) = held_guard {
        let my_pid = i64::from(std::process::id());
        let attempt_id =
            get_env("PROVENANT_ATTEMPT_ID").or_else(|| get_env("PROVENANT_PARENT_ATTEMPT_ID"));
        if let Some(scratch) = scratch_wait_dir {
            if let Ok(hg) = mark_held(Path::new(scratch), class, guard.index(), my_pid, attempt_id)
            {
                _holder_tracking = Some(HeldGuardVariant::Scratch(hg));
            }
        } else if let Ok(hg) = mark_host_held(&state_buf, class, guard.index(), my_pid) {
            _holder_tracking = Some(HeldGuardVariant::Host(hg));
        }
    }

    // Set re-entrancy markers
    if (held_guard.is_some()
        || is_nested
        || matches!(backend, provenant_config::PortBackend::Command { .. }))
        && let Some((me, _)) = own(&SystemProcessObserver)
    {
        let marker = me.marker();
        let class_key_upper = format!("PROVENANT_ADMISSION_SLOT_{}", class.to_ascii_uppercase());
        let class_key_lower = format!("PROVENANT_ADMISSION_SLOT_{}", class.to_ascii_lowercase());
        env.retain(|(k, _)| {
            k != OsStr::new(&class_key_upper)
                && k != OsStr::new(&class_key_lower)
                && k != OsStr::new(ADMISSION_SLOT)
                && (class != "build" || k != OsStr::new(BUILD_SLOT))
        });
        env.push((OsString::from(&class_key_upper), OsString::from(&marker)));
        env.push((OsString::from(&class_key_lower), OsString::from(&marker)));
        env.push((
            OsString::from(ADMISSION_SLOT),
            OsString::from(format!("{class}:{marker}")),
        ));
        env.push((OsString::from(ADMISSION_CLASS), OsString::from(class)));
        if class == "build" {
            env.push((OsString::from(BUILD_SLOT), OsString::from(&marker)));
        }
    }

    let code = match execute(Path::new(program), args, &env) {
        Ok(code) => code,
        Err(e) => report(&format!("cannot start {}: {e}", program.to_string_lossy())),
    };

    drop(held_guard);
    drop(_holder_tracking);

    if code != 0
        && env
            .iter()
            .any(|(key, value)| key == CARGO_SOURCE_WRITES && value == "false")
    {
        let source_str = env
            .iter()
            .find(|(key, _)| key == CARGO_SOURCE_WRITES_SOURCE)
            .and_then(|(_, value)| value.to_str())
            .unwrap_or("compiled default");
        say(&format!(
            "Cargo sources are read-only ({} = false; source: {source_str}). If a dependency is missing offline, fetch it on the host or enable this setting with network access for the lane.",
            provenant_confinement::cargo::SOURCE_WRITES_KNOB
        ));
    }

    code
}

fn execute(program: &Path, args: &[OsString], env: &[(OsString, OsString)]) -> Result<i32, String> {
    provenant_os::sys::run_wait(program, args, env).map_err(|error| error.to_string())
}

fn execute_and_forward(program: &OsStr, args: &[OsString], env: &[(OsString, OsString)]) -> i32 {
    match execute(Path::new(program), args, env) {
        Ok(code) => code,
        Err(e) => report(&format!("cannot start {}: {e}", program.to_string_lossy())),
    }
}

fn execute_command_port(
    command: &provenant_config::PortCommand,
    program: &OsStr,
    args: &[OsString],
    env: &[(OsString, OsString)],
    source: provenant_config::PortSource,
) -> i32 {
    let Some(wrapper) = command.argv.first() else {
        return report("repository build wrapper has no executable");
    };
    let mut arguments: Vec<OsString> = command
        .argv
        .iter()
        .skip(1)
        .map(|argument| OsString::from(argument.as_str()))
        .collect();
    arguments.push(program.to_os_string());
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
        ADMISSION_SLOT,
        BUILD_SLOT,
    ] {
        if let Some((_, value)) = env.iter().find(|(key, _)| key == OsStr::new(name)) {
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
    let wrapper_env = provenant_config::port_command_environment(env.iter().cloned(), &explicit);
    match provenant_os::sys::run_wait(Path::new(wrapper), &arguments, &wrapper_env) {
        Ok(code) => {
            if command.exit_codes.success.contains(&code) {
                0
            } else {
                if command.exit_codes.unknown.contains(&code)
                    || command.exit_codes.other == provenant_config::ExitDisposition::Unknown
                {
                    say(&format!(
                        "provenant: repository build admission returned an unknown outcome (source: {:?})",
                        source
                    ));
                } else {
                    say(&format!(
                        "provenant: repository build admission refused cargo (source: {:?})",
                        source
                    ));
                }
                if code == 0 { 1 } else { code }
            }
        }
        Err(e) => report(&format!("cannot start wrapper {wrapper}: {e}")),
    }
}

fn source_name(source: provenant_config::PortSource) -> &'static str {
    match source {
        provenant_config::PortSource::Builtin => "default",
        provenant_config::PortSource::Host => "host",
        provenant_config::PortSource::Project => "project",
    }
}

fn say(message: &str) {
    let _ = writeln!(std::io::stderr().lock(), "provenant: {message}");
}

fn report(message: &str) -> i32 {
    say(message);
    1
}
