//! `provenant internal build-slot`: the lane's `cargo` shim runs this. Cargo
//! commands that compile take one host build slot first, then this process
//! runs cargo as its child and waits, so the slot is held until cargo exits
//! and not a moment longer. Nextest runs that reuse both Cargo and binary
//! metadata do not compile and take no slot. The slot descriptor is
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
//! `max_concurrent_builds` observes occupancy across the host. Busy slots warn
//! and proceed immediately; Cargo owns its own build-lock queue.
use provenant_os::build_slots::BuildSlots;
use provenant_supervisor::census::{ProcessObservation, ProcessObserver, SystemProcessObserver};
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
};

/// Set in the environment of a gated build to `pid:start`, the holder shim's
/// pid and native process start time. A cargo run nested inside it (a build
/// script, a test that spawns cargo) skips the gate only while that exact
/// process is a live ancestor of the nested shim and is a provenant process,
/// so it does not queue behind the slot its own parent holds. Naming any
/// other process, even a live ancestor, is ignored.
pub(crate) const HELD: &str = "PROVENANT_BUILD_SLOT";
/// Host state root that holds the slot directory.
pub(crate) const STATE_ENV: &str = "PROVENANT_BUILD_SLOT_STATE";
/// How many builds the host runs at once.
pub(crate) const SLOTS_ENV: &str = "PROVENANT_BUILD_SLOTS";
/// Which cargo subcommands the host lets this lane run, written into the shim
/// script from the `lanes.local_cargo` knob and never from a lane's arguments.
pub(crate) const LOCAL_ENV: &str = "PROVENANT_LOCAL_CARGO";

/// The compiling cargo commands a lane may run locally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Local {
    #[default]
    All,
    /// Only `cargo check`; CI runs builds, tests, lints and the rest.
    Check,
    None,
}
impl Local {
    /// An unset or unrecognised value is the permissive default, so a host
    /// that predates the setting behaves as before.
    pub(crate) fn from_env(env: &[(OsString, OsString)]) -> Self {
        match env
            .iter()
            .find(|(key, _)| key == OsStr::new(LOCAL_ENV))
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
    /// The one-line refusal for `args`, or none when the command may run.
    /// Subcommands that do not compile (`fmt`, `metadata`, `tree`) always may.
    pub(crate) fn refusal(self, args: &[OsString]) -> Option<String> {
        let command = subcommand(args)?;
        let allowed = match self {
            Self::All => true,
            Self::Check => matches!(command, "check" | "c") || !GATED.contains(&command),
            Self::None => !GATED.contains(&command),
        };
        (!allowed).then(|| {
            format!(
                "cargo {command} is not run in lanes here (lanes.local_cargo = {}); CI runs it. Set the lanes.local_cargo knob (or the node's attempt.local_cargo) to all to run it locally",
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
}
impl Settings {
    /// None when the host did not configure slots; the build then runs
    /// ungated.
    pub(crate) fn from_env(env: &[(OsString, OsString)]) -> Option<Self> {
        let get = |name: &str| {
            env.iter()
                .find(|(key, _)| key == OsStr::new(name))
                .map(|(_, value)| value)
        };
        let state = PathBuf::from(get(STATE_ENV)?);
        let number = |name: &str| get(name)?.to_str()?.parse::<u64>().ok();
        Some(Self {
            state: state.is_absolute().then_some(state)?,
            slots: number(SLOTS_ENV).filter(|n| (1..=64).contains(n))?,
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
        if arg == "nextest" {
            let remaining = args
                .take_while(|arg| *arg != OsStr::new("--"))
                .collect::<Vec<_>>();
            let has_option = |name: &str| {
                remaining.iter().any(|arg| {
                    arg.to_str()
                        .is_some_and(|arg| arg == name || arg.starts_with(&format!("{name}=")))
                })
            };
            if remaining
                .first()
                .is_some_and(|arg| *arg == OsStr::new("run"))
                && has_option("--cargo-metadata")
                && has_option("--binaries-metadata")
            {
                return false;
            }
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
        .find(|(name, _)| name == HELD)
        .and_then(|(_, value)| Holder::parse(value.to_str()?))
    else {
        return false;
    };
    let Some((me, command)) = own(observer) else {
        return false;
    };
    ancestor_of(me.pid, &holder, &command, observer)
}

/// Take a slot when `gate.command` compiles, run it and return its exit
/// code. Busy or unavailable slots warn and the build proceeds immediately.
pub(crate) fn run(gate: Gate<'_>) -> i32 {
    let env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let nested = nested(&env);
    run_with(
        gate,
        env,
        nested,
        |settings| {
            BuildSlots::open(&settings.state)
                .and_then(|slots| slots.try_acquire(settings.slots))
                .map_err(|error| error.to_string())
        },
        |program, args, env| {
            provenant_os::sys::run_wait(program, args, env).map_err(|error| error.to_string())
        },
        |message| warn(&message),
    )
}

fn run_with(
    gate: Gate<'_>,
    mut env: Vec<(OsString, OsString)>,
    nested: bool,
    mut acquire: impl FnMut(&Settings) -> Result<Option<provenant_os::build_slots::SlotGuard>, String>,
    mut execute: impl FnMut(&Path, &[OsString], &[(OsString, OsString)]) -> Result<i32, String>,
    mut warning: impl FnMut(&str),
) -> i32 {
    let Some((program, args)) = gate.command.split_first() else {
        return report("no command to run");
    };
    // A nested cargo (a build script, a test) runs under a parent the policy
    // already let through.
    if !nested && let Some(message) = gate.local.refusal(args) {
        return report(&message);
    }
    let gate_this = gated(args) && !nested;
    if gate_this && let Err(message) = check_disk(&env) {
        return report(&message);
    }
    let mut held = None;
    if gate_this && let Some(settings) = &gate.settings {
        match acquire(settings) {
            Ok(Some(slot)) => held = Some(slot),
            Ok(None) => warning("all configured slots are busy; Cargo manages its own build locks"),
            Err(error) => warning(&error),
        }
    } else if gate_this {
        warning("the host's build slot settings are missing or invalid");
    }
    if held.is_some()
        && let Some((me, _)) = own(&SystemProcessObserver)
    {
        env.retain(|(name, _)| name != HELD);
        env.push((HELD.into(), me.marker().into()));
    }
    // `held` stays open while the child runs; the descriptor is the slot.
    let code = match execute(Path::new(program), args, &env) {
        Ok(code) => code,
        Err(error) => report(&format!("cannot start cargo: {error}")),
    };
    drop(held);
    code
}

/// New builds sample their output volume once. Existing builds are never signalled.
fn check_disk(env: &[(OsString, OsString)]) -> Result<(), String> {
    let value = |name: &str| {
        env.iter()
            .find(|(key, _)| key == OsStr::new(name))
            .and_then(|(_, value)| value.to_str())
    };
    let Some(limits) = value("PROVENANT_BUILD_DISK_LIMITS") else {
        return Ok(());
    };
    let observed = (|| {
        let limits: provenant_config::DiskLimits =
            serde_json::from_str(limits).map_err(|error| error.to_string())?;
        let target = value("CARGO_TARGET_DIR").map_or_else(|| Path::new("."), Path::new);
        let mut root = target;
        while !root.exists() {
            root = root.parent().unwrap_or(Path::new("."));
        }
        let free = provenant_os::fs::available_bytes_at(root).map_err(|error| error.to_string())?;
        Ok::<_, String>((limits.level(free), limits.detail(free)))
    })();
    use provenant_config::DiskLevel;
    match observed {
        Ok((DiskLevel::Critical, detail)) => Err(detail),
        Ok((DiskLevel::Warning, detail)) => {
            say(&format!("warning: {detail}; building"));
            Ok(())
        }
        Ok((DiskLevel::Ready, _)) => Ok(()),
        Err(error) => {
            say(&format!(
                "disk policy observation failed: {error}; building"
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
    fn local_cargo_policy_refuses_by_subcommand() {
        use super::Local;
        for command in [
            &["test"][..],
            &["+stable", "build", "--release"],
            &["nextest", "run"],
        ] {
            assert_eq!(Local::All.refusal(&args(command)), None);
            let message = Local::Check.refusal(&args(command)).unwrap();
            assert!(message.contains("CI runs it") && message.contains("lanes.local_cargo"));
            assert!(!message.contains('\n'));
            assert!(Local::None.refusal(&args(command)).is_some());
        }
        for command in [
            &["check", "-p", "x"][..],
            &["c"],
            &["--color", "never", "check"],
        ] {
            assert_eq!(Local::Check.refusal(&args(command)), None);
            assert!(Local::None.refusal(&args(command)).is_some());
        }
        for command in [&["fmt"][..], &["metadata"], &[]] {
            assert_eq!(Local::None.refusal(&args(command)), None);
        }
    }

    use super::*;
    use provenant_supervisor::census::{ProcessFact, ProcessIdentity};
    use std::collections::HashMap;

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
    fn nextest_run_with_reused_build_metadata_does_not_hold_a_build_slot() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(!gated(&args(&[
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
        let good = env(&[(STATE_ENV, "/s"), (SLOTS_ENV, "2")]);
        let settings = Settings::from_env(&good).unwrap();
        assert_eq!(settings.slots, 2);
        for bad in [
            env(&[]),
            env(&[(STATE_ENV, "relative"), (SLOTS_ENV, "2")]),
            env(&[(STATE_ENV, "/s"), (SLOTS_ENV, "0")]),
            env(&[(STATE_ENV, "/s"), (SLOTS_ENV, "65")]),
            env(&[(STATE_ENV, "/s"), (SLOTS_ENV, "x")]),
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
