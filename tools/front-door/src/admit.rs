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
    ADMISSION_CLASS, ADMISSION_DISK_LIMITS, ADMISSION_SLOT, ADMISSION_SLOT_STATE, ADMISSION_SLOTS,
    ADMISSION_WAIT_DIR, CARGO_SOURCE_WRITES, CARGO_SOURCE_WRITES_SOURCE, LOCAL_CARGO,
};
use provenant_os::admission::{
    HeldGuard, HostHeldGuard, mark_held_with_command, mark_host_held_with_command,
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

/// How much of the writer's current child command a status reader may see.
const MAX_CHILD_COMMAND_CHARS: usize = 512;

/// Render the command running under the slot for a live status row. Masking and
/// bounding happen here, before the argv reaches a record or another process:
/// readers never handle the raw arguments. A value that cannot be filtered is
/// replaced wholesale rather than published.
fn masked_child_command(program: &OsStr, args: &[OsString]) -> String {
    let joined = std::iter::once(program)
        .chain(args.iter().map(OsString::as_os_str))
        .map(|part| {
            part.to_string_lossy()
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ");
    provenant_redact::bounded_line(&joined, MAX_CHILD_COMMAND_CHARS)
}

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

    // Also check ADMISSION_DISK_LIMITS policy gate if present
    let gate_raw = value(ADMISSION_DISK_LIMITS);
    if let Some(encoded_gate) = gate_raw {
        let gate: Result<provenant_config::DiskGate, _> = serde_json::from_str(encoded_gate);
        if let Ok(gate) = gate {
            let free = provenant_os::fs::available_bytes_at(root).map_err(|e| e.to_string())?;
            match gate.decide(free) {
                provenant_records::policy::Effect::Proceed => {}
                provenant_records::policy::Effect::Warn(message) => {
                    say(&format!("warning: {message}; proceeding"));
                }
                provenant_records::policy::Effect::Refuse(diagnostic)
                | provenant_records::policy::Effect::Stop(diagnostic) => {
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
        say(&format!(
            "warning: class '{class}' sets a memory limit but no process-tree sample is available; limit not checked"
        ));
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
    let child_command = masked_child_command(program, args);

    let mut env: Vec<(OsString, OsString)> = env.to_vec();
    let get_env = |name: &str| {
        env.iter()
            .find(|(key, _)| key == OsStr::new(name))
            .and_then(|(_, v)| v.to_str())
    };

    // State root
    let state_buf = get_env(ADMISSION_SLOT_STATE)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("PROVENANT_STATE_DIR").map(PathBuf::from));

    // Resolve class configuration
    let resolved = get_env("PROVENANT_ADMISSION_CONFIG")
        .and_then(|value| {
            serde_json::from_str::<provenant_config::ResolvedAdmissionClass>(value).ok()
        })
        .filter(|config| config.name == class)
        .unwrap_or_else(|| {
            provenant_config::ResolvedAdmissionClass::resolve(
                class,
                state_buf.as_deref().unwrap_or(Path::new("")),
                None,
                None,
            )
        });

    // Override slots from environment if explicitly provided by host shim
    let slots_count = (class == "build")
        .then(|| get_env(ADMISSION_SLOTS))
        .flatten()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(resolved.slots);

    let wait_seconds = std::env::var("PROVENANT_ADMISSION_WAIT_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(resolved.wait_seconds);
    let wait_duration = Duration::from_secs(wait_seconds);

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

    if class == "build" {
        let raw_context = get_env(provenant_core::consumer_env::BUILD_CONTEXT);
        let parsed_context = raw_context
            .and_then(|value| {
                serde_json::from_str::<provenant_run::build::BuildContext>(value).ok()
            })
            .filter(|context| context.schema == 1);
        let context_was_invalid = raw_context.is_some() && parsed_context.is_none();
        let context = match parsed_context {
            Some(context) => context,
            None => match host_build_context(&env, state_buf.as_deref()) {
                Ok(context) => context,
                Err(error) => {
                    say(&format!(
                        "warning: build context unavailable: {error}; running command locally"
                    ));
                    let code = execute_and_forward(program, args, &env);
                    if code != 0 {
                        warn_readonly_sources(&env);
                    }
                    return code;
                }
            },
        };
        if context_was_invalid {
            say(
                "warning: PROVENANT_BUILD_CONTEXT was invalid; resolving build context on the host",
            );
        }
        return run_build_job(
            context,
            program,
            args,
            &env,
            is_nested,
            state_buf.as_deref(),
            subcommand(args).unwrap_or_else(|| {
                Path::new(program)
                    .file_name()
                    .and_then(OsStr::to_str)
                    .unwrap_or("cargo")
            }),
        );
    }

    let scratch_wait_dir = get_env(ADMISSION_WAIT_DIR);

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
        let Some(state_ref) = state_buf.as_ref() else {
            warn_slot_unavailable(&"state directory is unavailable");
            return execute_and_forward(program, args, &env);
        };
        use provenant_run::build::{
            BuildError, Priority,
            queue::{self, AdmissionRequest, Admitted, OverflowPolicy},
        };
        use sha2::{Digest, Sha256};
        let cancel = provenant_core::CancelToken::new();
        let id = format!(
            "{:032x}",
            (u128::from(std::process::id()) << 96)
                | u128::from(provenant_run::build::ledger::now_unix_ms())
        );
        let owner = get_env("PROVENANT_ATTEMPT_ID")
            .or_else(|| get_env("PROVENANT_PARENT_ATTEMPT_ID"))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("chair-{}", std::process::id()));
        let mut digest = Sha256::new();
        if let Ok(cwd) = std::env::current_dir() {
            let bytes = cwd.as_os_str().as_encoded_bytes();
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        for argument in
            std::iter::once(program.as_os_str()).chain(args.iter().map(OsString::as_os_str))
        {
            let bytes = argument.as_encoded_bytes();
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        let request = AdmissionRequest {
            state: state_ref,
            class,
            limit: slots_count,
            job_id: &id,
            owner: &owner,
            project_key: "",
            priority: Priority::Interactive,
            aging: Duration::from_secs(120),
            command_digest: digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            overflow: OverflowPolicy::Local {
                after: wait_duration,
            },
            affinity: vec![],
            excluded: vec![],
            cancel: &cancel,
            scratch: scratch_wait_dir.map(Path::new),
            attempt_id: get_env("PROVENANT_ATTEMPT_ID")
                .or_else(|| get_env("PROVENANT_PARENT_ATTEMPT_ID")),
            command: Some(child_command.clone()),
            wait_seconds,
        };
        match queue::admit(&request) {
            Ok(Admitted::Held { guard, .. }) => held_guard = Some(guard),
            Ok(Admitted::Overflow { .. }) => warn_slot_unavailable(
                &"bounded wait ended with every slot busy; proceeding without one",
            ),
            Err(BuildError::Cancelled) => return 130,
            Err(BuildError::CancelledSignal(signal)) => return 128 + signal,
            Err(BuildError::Superseded) => {
                say("admission request superseded by a newer identical request");
                return 75;
            }
            Err(error) => warn_slot_unavailable(&error),
        }
    }

    // If held, track holding metadata
    if let Some(ref guard) = held_guard {
        let my_pid = i64::from(std::process::id());
        let attempt_id =
            get_env("PROVENANT_ATTEMPT_ID").or_else(|| get_env("PROVENANT_PARENT_ATTEMPT_ID"));
        if let Some(scratch) = scratch_wait_dir {
            if let Ok(hg) = mark_held_with_command(
                Path::new(scratch),
                class,
                guard.index(),
                my_pid,
                attempt_id,
                Some(child_command.clone()),
            ) {
                _holder_tracking = Some(HeldGuardVariant::Scratch(hg));
            }
        } else if let Some(state_ref) = state_buf.as_ref()
            && let Ok(hg) = mark_host_held_with_command(
                state_ref,
                class,
                guard.index(),
                my_pid,
                Some(child_command.clone()),
            )
        {
            _holder_tracking = Some(HeldGuardVariant::Host(hg));
        }
    }

    // Set re-entrancy markers
    if (held_guard.is_some() || is_nested)
        && let Some((me, _)) = own(&SystemProcessObserver)
    {
        let marker = me.marker();
        let class_key_upper = format!("PROVENANT_ADMISSION_SLOT_{}", class.to_ascii_uppercase());
        let class_key_lower = format!("PROVENANT_ADMISSION_SLOT_{}", class.to_ascii_lowercase());
        env.retain(|(k, _)| {
            k != OsStr::new(&class_key_upper)
                && k != OsStr::new(&class_key_lower)
                && k != OsStr::new(ADMISSION_SLOT)
        });
        env.push((OsString::from(&class_key_upper), OsString::from(&marker)));
        env.push((OsString::from(&class_key_lower), OsString::from(&marker)));
        env.push((
            OsString::from(ADMISSION_SLOT),
            OsString::from(format!("{class}:{marker}")),
        ));
        env.push((OsString::from(ADMISSION_CLASS), OsString::from(class)));
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
        warn_readonly_sources(&env);
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

fn run_build_job(
    context: provenant_run::build::BuildContext,
    program: &OsStr,
    args: &[OsString],
    env: &[(OsString, OsString)],
    nested: bool,
    state: Option<&Path>,
    subcommand: &str,
) -> i32 {
    use provenant_run::build::{
        GitBinding, Job, JobKind, Origin, Output, Priority, SnapshotSource, Stack, Step,
    };

    let state = state
        .map(Path::to_path_buf)
        .unwrap_or_else(|| context.host_home.join(provenant_core::DEFAULT_STATE_SUFFIX));
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => return report(&format!("cannot resolve build working directory: {error}")),
    };
    let cwd_rel = relative_path(&context.worktree, &cwd);
    let mut step_argv = Vec::with_capacity(args.len() + 1);
    step_argv.push(program.to_os_string());
    step_argv.extend_from_slice(args);
    let priority_class = match subcommand {
        "check" | "c" => "check",
        "clippy" => "clippy",
        "test" | "t" => "test",
        "nextest" => "nextest",
        _ => "other",
    };
    let resolved_priority = context.port.priority_for(priority_class);
    for warning in &resolved_priority.warnings {
        say(warning);
    }
    let priority = match resolved_priority.value {
        provenant_config::BuildPriority::Interactive => Priority::Interactive,
        provenant_config::BuildPriority::Test => Priority::Test,
        provenant_config::BuildPriority::Train => Priority::Train,
        provenant_config::BuildPriority::Prewarm => Priority::Prewarm,
    };
    let now_ms = provenant_run::build::ledger::now_unix_ms();
    let job_id = format!(
        "{:032x}",
        (u128::from(std::process::id()) << 96) | u128::from(now_ms)
    );
    let owner = context
        .attempt
        .clone()
        .unwrap_or_else(|| format!("chair-{}", std::process::id()));
    let origin = if context.attempt.is_some() {
        Origin::Lane
    } else {
        Origin::Chair
    };
    let git = context.git.clone().unwrap_or_else(|| {
        say("warning: build Git binding is unavailable; slot snapshotting will fall back locally");
        let git_dir = context.worktree.join(".git");
        GitBinding {
            git: provenant_os::git::git_executable().unwrap_or_else(|| PathBuf::from("git")),
            git_dir: git_dir.clone(),
            common_dir: git_dir.clone(),
            object_dir: None,
            index_file: git_dir.join("index"),
            config_global: None,
        }
    });
    let source = SnapshotSource::WorkingTree {
        worktree: context.worktree.clone(),
        git,
    };
    let mut job_env = env.to_vec();
    job_env.retain(|(key, _)| key != OsStr::new("PROVENANT_BUILD_NESTED"));
    if nested {
        job_env.push((
            OsString::from("PROVENANT_BUILD_NESTED"),
            OsString::from("1"),
        ));
    }
    let job = Job {
        id: job_id,
        project_key: context.project_key.clone(),
        project: context.project.clone(),
        origin,
        owner,
        kind: JobKind::Intercepted {
            stack: Stack::Rust,
            subcommand: subcommand.to_owned(),
        },
        priority,
        source,
        verify_base: context.verify_base.clone(),
        steps: vec![Step {
            cwd_rel,
            argv: step_argv,
        }],
        stop_on_failure: true,
        env: job_env,
        output: Output::Stream,
        timeout: None,
        sandboxed: context.sandboxed,
        prelude: None,
    };
    let resolved = provenant_run::build::strategy(&context.port);
    let run_context = provenant_run::build::RunContext {
        state,
        home: context.host_home,
        strategy: resolved.value,
        cancel: provenant_core::CancelToken::new(),
    };
    let code = match provenant_run::build::run(job, &run_context) {
        Ok(result) => {
            for warning in &result.outcome.warnings {
                say(warning);
            }
            if let Some(exit) = result.outcome.exit {
                exit.code
                    .or_else(|| exit.signal.map(|signal| 128 + signal))
                    .unwrap_or(1)
            } else if result.outcome.cancelled {
                130
            } else {
                result
                    .step_exits
                    .last()
                    .and_then(Option::as_ref)
                    .and_then(|exit| exit.code.or_else(|| exit.signal.map(|signal| 128 + signal)))
                    .unwrap_or(1)
            }
        }
        Err(provenant_run::build::BuildError::Cancelled) => 130,
        Err(provenant_run::build::BuildError::CancelledSignal(signal)) => 128 + signal,
        Err(provenant_run::build::BuildError::Superseded) => {
            say("build request superseded by a newer identical request");
            75
        }
        Err(error) => {
            say(&format!("build job could not run: {error}"));
            1
        }
    };
    if code != 0 {
        warn_readonly_sources(env);
    }
    code
}

pub(crate) fn host_build_context(
    env: &[(OsString, OsString)],
    state: Option<&Path>,
) -> Result<provenant_run::build::BuildContext, String> {
    use provenant_run::build::{BaseRef, BuildContext, GitBinding};

    let value = |name: &str| {
        env.iter()
            .find(|(key, _)| key == OsStr::new(name))
            .map(|(_, value)| value.clone())
    };
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let home = value("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| cwd.clone());
    let state = state
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(provenant_core::DEFAULT_STATE_SUFFIX));
    let git = provenant_os::git::git_executable();
    let resolved_project = git.as_ref().and_then(|_| {
        provenant_os::git::repository_path(&cwd, provenant_os::git::RepositoryPath::Toplevel).ok()
    });
    let is_repository = resolved_project.is_some();
    let project = resolved_project.unwrap_or_else(|| cwd.clone());
    if !is_repository {
        say("warning: Git repository is unavailable; build will run locally");
    }
    let git_dir = git
        .as_ref()
        .and_then(|_| {
            provenant_os::git::repository_path(&cwd, provenant_os::git::RepositoryPath::GitDir).ok()
        })
        .unwrap_or_else(|| project.join(".git"));
    let common_dir = git
        .as_ref()
        .and_then(|_| {
            provenant_os::git::repository_path(&cwd, provenant_os::git::RepositoryPath::CommonDir)
                .ok()
        })
        .unwrap_or_else(|| git_dir.clone());
    let project_key = if is_repository {
        provenant_config::project_key_for(&project)
            .unwrap_or_else(|| provenant_config::project_key(&common_dir))
    } else {
        provenant_config::project_key(&project)
    };
    let table = match if is_repository {
        provenant_config::PortTable::read_for(Some(&project), &state)
    } else {
        provenant_config::PortTable::read_for(None, &state)
    } {
        Ok(table) => table,
        Err(error) => {
            say(&format!(
                "warning: build ports failed validation: {error}; reading configured values for build"
            ));
            match if is_repository {
                provenant_config::PortTable::read_for_inspection(Some(&project), &state)
            } else {
                provenant_config::PortTable::read_for_inspection(None, &state)
            } {
                Ok(table) => table,
                Err(inspection_error) => {
                    say(&format!(
                        "warning: build ports unavailable: {inspection_error}; using builtin build configuration"
                    ));
                    provenant_config::PortTable::default()
                }
            }
        }
    };
    let strategy_source = table
        .provenance()
        .get("build.strategy")
        .copied()
        .unwrap_or(table.build.source);
    let index_file = git_dir.join("index");
    let global_config = home.join(".gitconfig");
    let binding = GitBinding {
        git: git.clone().unwrap_or_else(|| PathBuf::from("git")),
        git_dir,
        common_dir,
        object_dir: None,
        index_file,
        config_global: global_config.is_file().then_some(global_config),
    };
    let verify_base = if !is_repository || table.verify.base.trim().is_empty() {
        None
    } else {
        let remote_ref = table.verify.base.clone();
        git.as_ref()
            .and_then(|git| resolve_git_commit(git, &project, &remote_ref))
            .map(|commit| BaseRef { remote_ref, commit })
    };
    if is_repository && !table.verify.base.trim().is_empty() && verify_base.is_none() {
        say(&format!(
            "warning: verify.base {:?} could not be resolved; build context has no verify base",
            table.verify.base
        ));
    }
    let mut host_path = value("PATH").unwrap_or_default();
    if host_path.is_empty() {
        host_path = std::env::var_os("PATH").unwrap_or_default();
    }
    let sandboxed = value(provenant_core::consumer_env::SANDBOX)
        .and_then(|value| value.into_string().ok())
        .is_some_and(|value| value == "on");
    Ok(BuildContext {
        schema: 1,
        project: project.clone(),
        project_key,
        worktree: project,
        attempt: None,
        sandboxed,
        git: Some(binding),
        port: table.build,
        strategy_source,
        host_home: home.clone(),
        rustup_home: value("RUSTUP_HOME").map(PathBuf::from),
        cargo_home: value("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cargo")),
        host_path,
        verify_base,
    })
}

fn resolve_git_commit(git: &Path, project: &Path, reference: &str) -> Option<String> {
    use provenant_os::exec::{ExecOptions, StreamPolicy, SystemExecutor, Termination, run_text};

    let argv = vec![
        git.as_os_str().to_owned(),
        OsString::from("rev-parse"),
        OsString::from("--verify"),
        OsString::from("--end-of-options"),
        OsString::from(format!("{reference}^{{commit}}")),
    ];
    let clean_env: Vec<(OsString, OsString)> = [
        ("HOME", "/nonexistent"),
        ("XDG_CONFIG_HOME", "/nonexistent"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect();
    let executor = SystemExecutor::new(1).ok()?;
    let mut options = ExecOptions::new(&argv, project, Duration::from_secs(10));
    options.env = Some(&clean_env);
    options.stdout = StreamPolicy::Capture { limit: 4096 };
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    let result = run_text(&executor, options).ok()?;
    if result.outcome.termination != Termination::Exit(0) || result.outcome.timed_out {
        return None;
    }
    let commit = result.stdout.trim();
    (!commit.is_empty()).then(|| commit.to_owned())
}

fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from = std::fs::canonicalize(from).unwrap_or_else(|_| from.to_path_buf());
    let to = std::fs::canonicalize(to).unwrap_or_else(|_| to.to_path_buf());
    let from_components = from.components().collect::<Vec<_>>();
    let to_components = to.components().collect::<Vec<_>>();
    let shared = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in &from_components[shared..] {
        relative.push("..");
    }
    for component in &to_components[shared..] {
        relative.push(component.as_os_str());
    }
    if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative
    }
}

fn warn_readonly_sources(env: &[(OsString, OsString)]) {
    if !env
        .iter()
        .any(|(key, value)| key == CARGO_SOURCE_WRITES && value == "false")
    {
        return;
    }
    if let Some(source_str) = env
        .iter()
        .find(|(key, _)| key == CARGO_SOURCE_WRITES_SOURCE)
        .and_then(|(_, value)| value.to_str())
    {
        say(&format!(
            "Cargo sources are read-only ({} = false; source: {source_str}). If a dependency is missing offline, fetch it on the host or enable this setting with network access for the lane.",
            provenant_confinement::cargo::SOURCE_WRITES_KNOB
        ));
    } else {
        say(&format!(
            "Cargo sources are read-only ({} = false; source: compiled default). If a dependency is missing offline, fetch it on the host or enable this setting with network access for the lane.",
            provenant_confinement::cargo::SOURCE_WRITES_KNOB
        ));
    }
}

fn say(message: &str) {
    let _ = writeln!(std::io::stderr().lock(), "provenant: {message}");
}

fn report(message: &str) -> i32 {
    say(message);
    1
}
