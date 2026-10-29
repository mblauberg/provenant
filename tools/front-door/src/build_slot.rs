//! `provenant internal build-slot`: the lane's `cargo` shim runs this. Cargo
//! commands that compile take one host build slot first, then this process
//! becomes cargo with the slot's descriptor inherited, so the slot is held
//! until cargo and its children exit, however they end.
//!
//! Slot files live under the host state directory shared by every project, so
//! `max_concurrent_builds` bounds builds across the host, not per project.
use provenant_core::CancelToken;
use provenant_os::build_slots::{BuildSlots, cargo_lock_held};
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::Path,
    time::Duration,
};

/// Set in the environment of a gated build. A cargo run nested inside it
/// (a build script, a test that spawns cargo) must not queue for a second
/// slot behind the one its own parent holds, which could deadlock.
pub(crate) const HELD: &str = "PROVENANT_BUILD_SLOT";

/// What the shim passes: the host state root, the slot count, the lane's
/// attempt (named while it waits) and the real cargo with its arguments.
pub(crate) struct Gate<'a> {
    pub state: &'a Path,
    pub slots: u64,
    pub attempt: Option<&'a str>,
    pub command: &'a [OsString],
}

/// Cargo subcommands that compile. Aliases and subcommands not named here run
/// without a slot: `run` stays out because it may serve for as long as it likes.
const GATED: &[&str] = &[
    "build", "b", "check", "c", "clippy", "test", "t", "bench", "doc", "d", "rustc", "rustdoc",
    "fix", "install", "package", "publish", "nextest", "llvm-cov", "miri",
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

/// Take a slot when `gate.command` compiles, then replace this process with
/// it. Returns only the error that kept cargo from starting.
#[allow(clippy::print_stderr)]
pub(crate) fn run(gate: Gate<'_>) -> std::io::Error {
    let Some((program, args)) = gate.command.split_first() else {
        return std::io::Error::from(std::io::ErrorKind::InvalidInput);
    };
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let nested = env.iter().any(|(name, _)| name == HELD);
    let mut held = None;
    if !nested && gated(args) {
        wait_for_cargo_lock(&env);
        match BuildSlots::open(gate.state) {
            Ok(slots) => {
                let slot =
                    slots.acquire(gate.slots, gate.attempt, &CancelToken::new(), &mut |busy| {
                        let mut stderr = std::io::stderr().lock();
                        // The lane's own stderr is the only place this shows
                        // while it waits; a closed stream is not an error.
                        let _ = writeln!(
                            stderr,
                            "provenant: waiting for a build slot ({busy} of {} in use)",
                            gate.slots
                        );
                    });
                match slot {
                    Ok(slot) => match slot.inherit() {
                        Ok(()) => held = Some(slot),
                        Err(error) => warn(&error),
                    },
                    Err(error) => warn(&error),
                }
            }
            Err(error) => warn(&error),
        }
    }
    if let Some(slot) = &held {
        env.push((HELD.into(), slot.index().to_string().into()));
    }
    // `held` stays open through exec; the descriptor is the slot.
    let error = provenant_os::sys::replace_process(Path::new(program), args, &env);
    drop(held);
    error
}

/// Cargo queues a second build behind the first one's `.cargo-lock`. Waiting
/// for that lock before taking a slot keeps the slot free for a build that
/// can use it.
fn wait_for_cargo_lock(env: &[(OsString, OsString)]) {
    let Some(target) = env
        .iter()
        .find(|(name, _)| name == OsStr::new("CARGO_TARGET_DIR"))
        .map(|(_, value)| Path::new(value))
    else {
        return;
    };
    while cargo_lock_held(target) {
        for _ in 0..5 {
            provenant_os::sys::wait_pause(Duration::from_millis(50));
        }
    }
}

/// Slots are a fairness aid, not a boundary, so a slot that cannot be taken
/// does not stop the build.
#[allow(clippy::print_stderr)]
fn warn(error: &dyn std::fmt::Display) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "provenant: build slot unavailable, building without one: {error}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn compiling_subcommands_are_gated_whatever_their_options() {
        for list in [
            &["build"][..],
            &["+nightly", "test", "--workspace"],
            &["--locked", "clippy"],
            &["-C", "tools/front-door", "check"],
            &["-Z", "unstable-options", "build"],
            &["--config", "build.jobs=2", "nextest", "run"],
            &["t"],
        ] {
            assert!(gated(&args(list)), "{list:?}");
        }
    }

    #[test]
    fn other_subcommands_and_bare_options_are_not_gated() {
        for list in [
            &[][..],
            &["fmt", "--check"],
            &["metadata"],
            &["run", "--", "build"],
            &["--version"],
            &["--list"],
            &["-C", "dir"],
            &["xtask", "build"],
        ] {
            assert!(!gated(&args(list)), "{list:?}");
        }
    }
}
