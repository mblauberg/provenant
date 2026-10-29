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
use provenant_os::build_slots::{BuildSlots, SlotError, Wait, wait_for_cargo_lock};
use provenant_supervisor::census::{ProcessObservation, ProcessObserver, SystemProcessObserver};
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

/// Set in the environment of a gated build to the holder shim's pid. A cargo
/// run nested inside it (a build script, a test that spawns cargo) skips the
/// gate only while that pid is a live ancestor of the nested shim, so it does
/// not queue behind the slot its own parent holds. A value naming anything
/// else is ignored.
pub(crate) const HELD: &str = "PROVENANT_BUILD_SLOT";
/// Host state root that holds the slot directory.
pub(crate) const STATE_ENV: &str = "PROVENANT_BUILD_SLOT_STATE";
/// How many builds the host runs at once.
pub(crate) const SLOTS_ENV: &str = "PROVENANT_BUILD_SLOTS";
/// Seconds a build waits for a slot or a Cargo lock before giving up.
pub(crate) const WAIT_ENV: &str = "PROVENANT_BUILD_WAIT_SECS";

/// What the host's shim script supplies, read from the environment.
pub(crate) struct Settings {
    pub state: PathBuf,
    pub slots: u64,
    pub wait: Duration,
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

/// Whether `pid` is a live ancestor of this process.
pub(crate) fn live_ancestor(pid: i64) -> bool {
    ancestor_of(std::process::id().into(), pid, &SystemProcessObserver)
}

fn ancestor_of(start: i64, wanted: i64, observer: &dyn ProcessObserver) -> bool {
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
        if parent == wanted {
            return match observer.observe(parent) {
                ProcessObservation::Present(parent) => !parent.zombie,
                _ => false,
            };
        }
        current = parent;
    }
    false
}

/// Whether the marker in `env` names a live ancestor shim.
fn nested(env: &[(OsString, OsString)]) -> bool {
    env.iter()
        .find(|(name, _)| name == HELD)
        .and_then(|(_, value)| value.to_str()?.parse::<i64>().ok())
        .is_some_and(|pid| pid > 1 && live_ancestor(pid))
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
        if let Err(message) = wait_for_lock(&env, settings) {
            return report(&message);
        }
        match BuildSlots::open(&settings.state) {
            Ok(slots) => match slots.acquire(
                settings.slots,
                Wait::new(settings.wait),
                &CancelToken::new(),
                &mut |busy, waited| waiting(busy, settings.slots, waited),
            ) {
                Ok(slot) => held = Some(slot),
                Err(SlotError::Timeout { held, waited, .. }) => {
                    return report(&format!(
                        "gave up waiting for a build slot after {} min: all {} are held ({held} in use). Run `provenant lanes` to find the lanes holding them, or raise domains.fabric.lanes.max_concurrent_builds or build_wait_minutes",
                        waited.as_secs().div_ceil(60),
                        settings.slots
                    ));
                }
                Err(error) => warn(&error),
            },
            Err(error) => warn(&error),
        }
    }
    if held.is_some() {
        env.push((HELD.into(), std::process::id().to_string().into()));
    }
    // `held` stays open while the child runs; the descriptor is the slot.
    let code = match provenant_os::sys::run_wait(Path::new(program), args, &env) {
        Ok(code) => code,
        Err(error) => report(&format!("cannot start cargo: {error}")),
    };
    drop(held);
    code
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
    struct Tree(HashMap<i64, (i64, bool)>);
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
                Some((parent, zombie)) => ProcessObservation::Present(ProcessFact {
                    identity: ProcessIdentity {
                        boot: "boot".into(),
                        pid,
                        native_start: 1,
                        group_id: pid,
                        session_id: pid,
                    },
                    parent_pid: *parent,
                    zombie: *zombie,
                    cpu_micros: None,
                    command: None,
                }),
                None => ProcessObservation::Absent,
            }
        }
    }

    #[test]
    fn only_a_live_ancestor_counts_as_the_holder() {
        // 500 (a lane process) -> 400 (cargo) -> 300 (holder shim) -> 200 -> 1
        let tree = Tree(HashMap::from([
            (500, (400, false)),
            (400, (300, false)),
            (300, (200, false)),
            (200, (1, false)),
            (900, (1, false)),
        ]));
        assert!(ancestor_of(500, 300, &tree));
        assert!(ancestor_of(500, 400, &tree));
        assert!(!ancestor_of(500, 900, &tree), "an unrelated live pid");
        assert!(!ancestor_of(500, 1, &tree), "init is never the holder");
        assert!(!ancestor_of(500, 777, &tree), "a pid that does not exist");
        let dead = Tree(HashMap::from([(500, (300, false)), (300, (200, true))]));
        assert!(!ancestor_of(500, 300, &dead), "a zombie holder is gone");
    }
}
