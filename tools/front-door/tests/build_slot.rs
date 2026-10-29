//! The lane's `cargo` shim end to end: `internal build-slot` takes a host build
//! slot, becomes the command it was given and keeps the slot until that exits.
//! A fake cargo stands in for the compiler; nothing here needs a sandbox.
#![allow(clippy::disallowed_methods)]
use provenant_os::build_slots::BuildSlots;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const ATTEMPT: &str = "0b0e1d54-3c1e-4a53-8f0b-1d7c2a9a4f10";

struct Rig {
    _temp: tempfile::TempDir,
    state: PathBuf,
    cargo: PathBuf,
    log: PathBuf,
}
impl Rig {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?;
        let (state, log, cargo) = (
            root.join("state"),
            root.join("log"),
            root.join("fake-cargo"),
        );
        fs::create_dir(&state)?;
        fs::write(&log, "")?;
        let binary = env!("CARGO_BIN_EXE_provenant");
        fs::write(
            &cargo,
            format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                 build) echo \"start $$\" >> '{log}'; /bin/sleep 0.3; echo \"end $$\" >> '{log}' ;;\n\
                 fmt) echo \"fmt $$\" >> '{log}' ;;\n\
                 check) echo \"hung $$\" >> '{log}'; exec /bin/sleep 60 ;;\n\
                 test) echo \"outer $$\" >> '{log}'; '{binary}' internal build-slot --state-dir '{state}' --slots 1 -- \"$0\" build ;;\n\
                 esac\n",
                log = log.display(),
                state = state.display(),
            ),
        )?;
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            _temp: temp,
            state,
            cargo,
            log,
        })
    }

    fn shim(&self, slots: u64, args: &[&str]) -> Result<Child> {
        Ok(Command::new(env!("CARGO_BIN_EXE_provenant"))
            .args(["internal", "build-slot", "--state-dir"])
            .arg(&self.state)
            .arg("--slots")
            .arg(slots.to_string())
            .args(["--attempt", ATTEMPT, "--"])
            .arg(&self.cargo)
            .args(args)
            .env_remove("PROVENANT_BUILD_SLOT")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?)
    }

    fn log(&self) -> Result<Vec<String>> {
        Ok(fs::read_to_string(&self.log)?
            .lines()
            .map(str::to_owned)
            .collect())
    }
}

fn until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    condition()
}

fn finish(child: &mut Child, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    child.kill()?;
    child.wait()?;
    Err("shim did not finish in time".into())
}

#[test]
fn more_builds_than_slots_compile_serially() -> Result {
    let rig = Rig::new()?;
    let mut children = (0..4)
        .map(|_| rig.shim(1, &["build"]))
        .collect::<Result<Vec<_>>>()?;
    for child in &mut children {
        assert!(finish(child, Duration::from_secs(30))?);
    }
    let log = rig.log()?;
    assert_eq!(log.len(), 8);
    // One slot: every start is followed by its own end before the next start.
    for pair in log.chunks(2) {
        let (start, end) = (&pair[0], &pair[1]);
        assert_eq!(start.strip_prefix("start "), end.strip_prefix("end "));
    }
    Ok(())
}

#[test]
fn two_slots_allow_two_builds_at_once_and_no_more() -> Result {
    let rig = Rig::new()?;
    let mut children = (0..4)
        .map(|_| rig.shim(2, &["build"]))
        .collect::<Result<Vec<_>>>()?;
    for child in &mut children {
        assert!(finish(child, Duration::from_secs(30))?);
    }
    let (mut running, mut peak) = (0_i32, 0_i32);
    for line in rig.log()? {
        running += if line.starts_with("start") { 1 } else { -1 };
        peak = peak.max(running);
    }
    assert_eq!(peak, 2);
    Ok(())
}

#[test]
fn a_killed_build_releases_its_slot_and_a_waiter_is_visible_meanwhile() -> Result {
    let rig = Rig::new()?;
    let mut holder = rig.shim(1, &["check"])?;
    assert!(until(Duration::from_secs(20), || rig.log().is_ok_and(
        |log| log.iter().any(|line| line.starts_with("hung"))
    )));
    let mut waiter = rig.shim(1, &["build"])?;
    let slots = BuildSlots::observe(&rig.state)?;
    assert!(
        until(Duration::from_secs(20), || slots.waiting(ATTEMPT)),
        "the blocked command registers as waiting"
    );
    assert_eq!(slots.held(1), 1);
    assert!(
        !rig.log()?.iter().any(|line| line.starts_with("start")),
        "the second build waits while the first holds the slot"
    );
    // SIGKILL leaves no chance to release anything: the kernel does.
    holder.kill()?;
    holder.wait()?;
    assert!(finish(&mut waiter, Duration::from_secs(30))?);
    assert!(rig.log()?.iter().any(|line| line.starts_with("end")));
    assert!(!slots.waiting(ATTEMPT));
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut waiter.stderr.take().ok_or("stderr")?, &mut stderr)?;
    assert!(
        stderr.contains("waiting for a build slot (1 of 1 in use)"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn commands_that_do_not_compile_and_nested_cargo_never_queue() -> Result {
    let rig = Rig::new()?;
    let mut holder = rig.shim(1, &["check"])?;
    assert!(until(Duration::from_secs(20), || rig.log().is_ok_and(
        |log| log.iter().any(|line| line.starts_with("hung"))
    )));
    // Not a compiling subcommand: runs at once although the only slot is held.
    let mut format = rig.shim(1, &["fmt"])?;
    assert!(finish(&mut format, Duration::from_secs(10))?);
    holder.kill()?;
    holder.wait()?;
    // A build that itself runs cargo must not wait for a second slot.
    let mut outer = rig.shim(1, &["test"])?;
    assert!(finish(&mut outer, Duration::from_secs(10))?);
    let log = rig.log()?;
    assert!(log.iter().any(|line| line.starts_with("outer")));
    assert!(log.iter().any(|line| line.starts_with("end")));
    Ok(())
}

#[test]
fn an_unusable_slot_directory_does_not_stop_the_build() -> Result {
    let rig = Rig::new()?;
    // A regular file where the slot directory belongs.
    let fabric = rig.state.join("fabric");
    fs::write(&fabric, b"not a directory")?;
    let mut child = rig.shim(1, &["build"])?;
    assert!(finish(&mut child, Duration::from_secs(10))?);
    assert!(rig.log()?.iter().any(|line| line.starts_with("end")));
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().ok_or("stderr")?, &mut stderr)?;
    assert!(stderr.contains("build slot unavailable"), "{stderr}");
    Ok(())
}
