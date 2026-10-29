//! `provenant internal build-slot`: the lane's `cargo` shim runs this. Cargo
//! commands that compile take one host build slot first, then this process
//! runs cargo as its child and waits, so the slot is held until cargo exits
//! and not a moment longer. The slot descriptor is close-on-exec: a daemon or
//! orphaned grandchild that outlives the build never keeps it.
//!
//! This is a build throttle for cooperative fair sharing of CPU and memory
//! among our own lanes, not a security boundary and not a hard cap. A lane
//! that calls `~/.cargo/bin/cargo`, rustc or a non-cargo build directly is not
//! gated, and a lane can hold a slot by locking its file on purpose. For that
//! reason an unexpected error opening or locking a slot lets the build run
//! without one, with a single warning.
//!
//! The slot directory, count and wait come from the environment the host
//! wrote into the shim script, never from arguments a lane chooses. Slot files
//! live under the host state directory shared by every project, so
//! `max_concurrent_builds` bounds builds across the host, not per project.
use provenant_core::CancelToken;
use provenant_os::build_slots::{BuildSlots, SlotError, Wait, WaitKind, wait_for_cargo_lock};
use provenant_supervisor::census::{ProcessObservation, ProcessObserver, SystemProcessObserver};
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
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
/// Seconds a build waits for a slot or a Cargo lock before giving up.
pub(crate) const WAIT_ENV: &str = "PROVENANT_BUILD_WAIT_SECS";
/// Prefix of the file where a waiting build says so, in the lane's scratch
/// directory; the build adds `.pid`.
pub(crate) const MARKER_ENV: &str = "PROVENANT_BUILD_WAIT_MARKER";

/// What the host's shim script supplies, read from the environment.
pub(crate) struct Settings {
    pub state: PathBuf,
    pub slots: u64,
    pub wait: Duration,
    /// Where to publish a wait for the host's `lanes` and `status`; none means
    /// the wait shows on stderr only.
    pub marker: Option<PathBuf>,
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
            wait: Duration::from_secs(number(WAIT_ENV).filter(|n| *n >= 1)?),
            marker: get(MARKER_ENV)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
        })
    }
}

/// The real cargo with its arguments, and the host's settings if any.
pub(crate) struct Gate<'a> {
    pub settings: Option<Settings>,
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
/// code. A slot that cannot be had in time is an error; one that cannot be
/// opened or locked is a warning and the build runs without it.
pub(crate) fn run(gate: Gate<'_>) -> i32 {
    let Some((program, args)) = gate.command.split_first() else {
        return report("no command to run");
    };
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let mut held = None;
    let gate_this = gated(args) && !nested(&env);
    if gate_this && gate.settings.is_none() {
        warn(&"the host's build slot settings are missing or invalid");
    }
    if gate_this && let Some(settings) = &gate.settings {
        // One deadline covers the Cargo-lock wait and the slot wait.
        #[allow(clippy::disallowed_methods)]
        let began = Instant::now();
        let locked = wait_for_lock(&env, settings);
        unmark(settings);
        if let Err(message) = locked {
            return report(&message);
        }
        match BuildSlots::open(&settings.state) {
            Ok(slots) => match acquire(&slots, settings, began) {
                Ok(slot) => held = Some(slot),
                Err(SlotError::Timeout { held, .. }) => {
                    return report(&format!(
                        "gave up waiting for a build slot after {} min: all {} are held ({held} in use). Run `provenant lanes` to find the lanes holding them, or raise domains.fabric.lanes.max_concurrent_builds or build_wait_minutes",
                        began.elapsed().as_secs().div_ceil(60),
                        settings.slots
                    ));
                }
                Err(error) => warn(&error),
            },
            Err(error) => warn(&error),
        }
    }
    if held.is_some()
        && let Some((me, _)) = own(&SystemProcessObserver)
    {
        env.retain(|(name, _)| name != HELD);
        env.push((HELD.into(), me.marker().into()));
    }
    // `held` stays open while the child runs; the descriptor is the slot.
    let code = match provenant_os::sys::run_wait(Path::new(program), args, &env) {
        Ok(code) => code,
        Err(error) => report(&format!("cannot start cargo: {error}")),
    };
    drop(held);
    code
}

/// Wait for a slot, telling the host while it does.
fn acquire(
    slots: &BuildSlots,
    settings: &Settings,
    began: Instant,
) -> Result<provenant_os::build_slots::SlotGuard, SlotError> {
    let acquired = slots.acquire(
        settings.slots,
        Wait::new(settings.wait.saturating_sub(began.elapsed())),
        &CancelToken::new(),
        &mut |busy, waited| {
            mark(settings, WaitKind::Slot);
            waiting(busy, settings.slots, waited);
        },
    );
    unmark(settings);
    acquired
}

/// Cargo queues a second build behind the first one's `.cargo-lock`. Waiting
/// for that lock before taking a slot keeps the slot free for a build that
/// can use it. The wait is bounded and reported like the slot wait.
fn wait_for_lock(env: &[(OsString, OsString)], settings: &Settings) -> Result<(), String> {
    let Some(target) = env
        .iter()
        .find(|(name, _)| name == OsStr::new("CARGO_TARGET_DIR"))
        .map(|(_, value)| Path::new(value))
    else {
        return Ok(());
    };
    wait_for_cargo_lock(target, Wait::new(settings.wait), &mut |waited| {
        mark(settings, WaitKind::CargoLock);
        say(&format!(
            "waiting for another cargo build to release its lock{}",
            since(waited)
        ));
    })
    .map_err(|error| {
        format!(
            "gave up after {error}. Run `provenant lanes` to find the lane building in the same target directory, or raise domains.fabric.lanes.build_wait_minutes"
        )
    })
}

/// Tell the host this build waits, now. Best effort: the wait shows on
/// stderr whatever happens here.
#[allow(clippy::disallowed_methods)]
fn mark(settings: &Settings, kind: WaitKind) {
    let Some(marker) = &settings.marker else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let _ = std::fs::write(own_marker(marker), kind.marker(now));
}

/// One marker per command, so a sibling that gets a slot cannot remove the
/// wait of one that still waits.
fn own_marker(base: &std::path::Path) -> PathBuf {
    let mut name = base.as_os_str().to_owned();
    name.push(format!(".{}", std::process::id()));
    name.into()
}

fn unmark(settings: &Settings) {
    if let Some(marker) = &settings.marker {
        let _ = std::fs::remove_file(own_marker(marker));
    }
}

fn since(waited: Duration) -> String {
    if waited.is_zero() {
        String::new()
    } else {
        format!(", waited {} s", waited.as_secs())
    }
}

fn waiting(busy: u64, slots: u64, waited: Duration) {
    say(&format!(
        "waiting for a build slot ({busy} of {slots} in use){}",
        since(waited)
    ));
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
    reason = "the custom worktree test driver includes this library without the test harness"
)]
mod tests {
    use super::*;
    use provenant_supervisor::census::{ProcessFact, ProcessIdentity};
    use std::collections::HashMap;

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
        let good = env(&[(STATE_ENV, "/s"), (SLOTS_ENV, "2"), (WAIT_ENV, "90")]);
        let settings = Settings::from_env(&good).unwrap();
        assert_eq!(
            (settings.slots, settings.wait),
            (2, Duration::from_secs(90))
        );
        for bad in [
            env(&[]),
            env(&[(STATE_ENV, "relative"), (SLOTS_ENV, "2"), (WAIT_ENV, "9")]),
            env(&[(STATE_ENV, "/s"), (SLOTS_ENV, "0"), (WAIT_ENV, "9")]),
            env(&[(STATE_ENV, "/s"), (SLOTS_ENV, "65"), (WAIT_ENV, "9")]),
            env(&[(STATE_ENV, "/s"), (SLOTS_ENV, "2"), (WAIT_ENV, "0")]),
            env(&[(STATE_ENV, "/s"), (SLOTS_ENV, "x"), (WAIT_ENV, "9")]),
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

    #[test]
    fn markers_round_trip_and_reject_anything_malformed() {
        let marker = holder(300, 30).marker();
        assert_eq!(marker, "300:30");
        assert_eq!(Holder::parse(&marker), Some(holder(300, 30)));
        for bad in [
            "300", "300:", ":30", "1:30", "0:30", "-5:30", "x:30", "300:y", "",
        ] {
            assert_eq!(Holder::parse(bad), None, "{bad:?}");
        }
    }
}
