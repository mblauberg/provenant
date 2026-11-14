//! The lane's `cargo` shim end to end: `internal build-slot` takes a host build
//! advisory slot if free, runs its child and keeps the slot until
//! that exits. A fake cargo stands in for the compiler; nothing here needs a
//! sandbox. The slot directory and count reach the shim through its
//! environment, as the host's shim script supplies them.
#![allow(clippy::disallowed_methods)]
use provenant_os::build_slots::{BuildSlots, DIRECTORY};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

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
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        fs::write(&log, "")?;
        let binary = env!("CARGO_BIN_EXE_provenant");
        let slot_dir = state.join(DIRECTORY.join("/"));
        fs::write(
            &cargo,
            format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                 build) echo \"start $$\" >> '{log}'; echo \"end $$\" >> '{log}' ;;\n\
                 fmt) echo \"fmt $$\" >> '{log}' ;;\n\
                 check) echo \"hung $$\" >> '{log}'; exec /bin/sleep 60 ;;\n\
                 bg) /bin/sleep 60 >/dev/null 2>&1 </dev/null & echo \"bg $!\" >> '{log}' ;;\n\
                 tamper) rm -f '{slots}/slot-0.lock' 2>/dev/null; echo \"rm $?\" >> '{log}'; /usr/bin/touch '{slots}/slot-9.lock' 2>/dev/null; echo \"create $?\" >> '{log}'; echo \"start $$\" >> '{log}'; echo \"end $$\" >> '{log}' ;;\n\
                 test) echo \"outer $$\" >> '{log}'; '{binary}' internal build-slot -- \"$0\" build ;;\n\
                 esac\n",
                log = log.display(),
                slots = slot_dir.display(),
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

    /// Host side: create the slot files, as `provenant run` does at launch.
    fn provision(&self, slots: u64) -> Result {
        BuildSlots::provision(&self.state, slots)?;
        Ok(())
    }

    fn command(&self, slots: u64, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_provenant"));
        command
            .args(["internal", "build-slot", "--"])
            .arg(&self.cargo)
            .args(args)
            .env("PROVENANT_BUILD_SLOT_STATE", &self.state)
            .env("PROVENANT_BUILD_SLOTS", slots.to_string())
            .env_remove("PROVENANT_BUILD_SLOT")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command
    }

    fn shim(&self, slots: u64, args: &[&str]) -> Result<Child> {
        Ok(self.command(slots, args).spawn()?)
    }

    fn log(&self) -> Result<Vec<String>> {
        Ok(fs::read_to_string(&self.log)?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    fn started(&self, word: &str) -> bool {
        self.log()
            .is_ok_and(|log| log.iter().any(|line| line.starts_with(word)))
    }
}

fn stderr_of(child: &mut Child) -> Result<String> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().ok_or("stderr")?, &mut text)?;
    Ok(text)
}

fn finish(child: &mut Child, timeout: Duration) -> Result<bool> {
    Ok(until_exit(child, timeout)?.success())
}

fn until_exit(child: &mut Child, timeout: Duration) -> Result<std::process::ExitStatus> {
    match provenant_test_support::wait_for(
        "shim exit",
        timeout,
        || child.try_wait(),
        |status| !matches!(status, Ok(None)),
    ) {
        Ok(status) => Ok(status?.ok_or("shim exit observation missing")?),
        Err(error) => {
            child.kill()?;
            child.wait()?;
            Err(error.into())
        }
    }
}

fn alive(pid: &str) -> bool {
    Command::new("/bin/kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn a_backgrounded_grandchild_that_outlives_the_build_does_not_keep_the_slot() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let mut first = rig.shim(1, &["bg"])?;
    assert!(finish(&mut first, Duration::from_secs(20))?);
    let pid = rig
        .log()?
        .iter()
        .find_map(|line| line.strip_prefix("bg ").map(str::to_owned))
        .ok_or("the grandchild started")?;
    assert!(alive(&pid), "the grandchild outlives the build");
    // The slot is free the moment the direct child exits.
    let slots = BuildSlots::open(&rig.state)?;
    assert_eq!(slots.held(1), 0);
    let mut second = rig.shim(1, &["build"])?;
    assert!(finish(&mut second, Duration::from_secs(10))?);
    assert!(alive(&pid), "still running after the next build finished");
    Command::new("/bin/kill").arg(&pid).status()?;
    Ok(())
}

/// The marker names a live ancestor with its true pid and start time, but that
/// ancestor is a shell, not a provenant shim, so the gate still applies.

#[test]
fn a_terminated_shim_passes_the_signal_to_its_build() -> Result {
    for signal in ["TERM", "HUP", "INT"] {
        let rig = Rig::new()?;
        rig.provision(1)?;
        let mut shim = rig.shim(1, &["check"])?;
        assert!(provenant_test_support::wait_for(
            "build slot readiness",
            Duration::from_secs(20),
            || rig.started("hung"),
            |ready| *ready
        )?);
        let pid = rig
            .log()?
            .iter()
            .find_map(|line| line.strip_prefix("hung ").map(str::to_owned))
            .ok_or("no pid")?;
        assert!(alive(&pid));
        Command::new("/bin/kill")
            .args([format!("-{signal}"), shim.id().to_string()])
            .status()?;
        let status = until_exit(&mut shim, Duration::from_secs(20))?;
        assert!(!status.success(), "{signal}: the build was interrupted");
        assert!(
            provenant_test_support::wait_for(
                "build slot readiness",
                Duration::from_secs(10),
                || !alive(&pid),
                |ready| *ready
            )?,
            "{signal}: the build kept running after the shim was signalled"
        );
    }
    Ok(())
}

#[test]
fn commands_that_do_not_compile_and_nested_cargo_never_queue() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let mut holder = rig.shim(1, &["check"])?;
    assert!(provenant_test_support::wait_for(
        "holder started",
        Duration::from_secs(20),
        || rig.started("hung"),
        |ready| *ready
    )?);
    // Not a compiling subcommand: runs at once although the only slot is held.
    let mut format = rig.shim(1, &["fmt"])?;
    assert!(finish(&mut format, Duration::from_secs(10))?);
    holder.kill()?;
    holder.wait()?;
    // A build that itself runs cargo must not wait for a second slot: the
    // outer shim is a live ancestor of the inner one.
    let mut outer = rig.shim(1, &["test"])?;
    assert!(finish(&mut outer, Duration::from_secs(10))?);
    assert!(rig.started("outer"));
    assert!(rig.started("end"));
    Ok(())
}

#[test]
fn read_only_slot_files_cannot_be_unlinked_or_created_by_the_build() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    // The lane's access: read the directory and its files, write neither.
    let dir = rig.state.join(DIRECTORY.join("/"));
    fs::set_permissions(dir.join("slot-0.lock"), fs::Permissions::from_mode(0o400))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500))?;
    let mut build = rig.shim(1, &["tamper"])?;
    let finished = finish(&mut build, Duration::from_secs(20));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let ok = finished?;
    assert!(ok, "{}", stderr_of(&mut build)?);
    let log = rig.log()?;
    assert!(
        log.iter()
            .any(|line| line.starts_with("rm ") && line != "rm 0")
    );
    assert!(
        log.iter()
            .any(|line| line.starts_with("create ") && line != "create 0")
    );
    assert!(dir.join("slot-0.lock").exists());
    assert!(!dir.join("slot-9.lock").exists());
    // And the read-only descriptor took the slot: the build ran gated.
    assert!(rig.started("end"));
    Ok(())
}

#[test]
fn an_unusable_slot_directory_does_not_stop_the_build() -> Result {
    let rig = Rig::new()?;
    // A regular file where the slot directory belongs.
    let slots = rig.state.join(DIRECTORY.join("/"));
    fs::write(&slots, b"not a directory")?;
    let mut child = rig.shim(1, &["build"])?;
    assert!(finish(&mut child, Duration::from_secs(10))?);
    assert!(rig.started("end"));
    let stderr = stderr_of(&mut child)?;
    assert!(stderr.contains("build slot unavailable"), "{stderr}");
    assert_eq!(stderr.matches("build slot unavailable").count(), 1);
    Ok(())
}

#[test]
fn missing_settings_run_the_build_ungated_with_one_warning() -> Result {
    let rig = Rig::new()?;
    // Nothing from whoever ran the suite may reach the shim: a lane's own
    // marker or slot settings would decide this test.
    let began = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .args(["internal", "build-slot", "--"])
        .arg(&rig.cargo)
        .arg("build")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    assert!(finish(&mut child, Duration::from_secs(10))?);
    assert!(stderr_of(&mut child)?.contains("settings are missing or invalid"));
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "an ungated build took {:?}",
        began.elapsed()
    );
    Ok(())
}

#[test]
fn the_shims_exit_code_is_the_builds() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let status = rig
        .command(1, &["no-such-subcommand-exits-zero"])
        .status()?;
    assert!(status.success());
    let failing = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .args(["internal", "build-slot", "--", "/bin/sh", "-c", "exit 7"])
        .env_remove("PROVENANT_BUILD_SLOT_STATE")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    assert_eq!(failing.code(), Some(7));
    Ok(())
}

#[test]
fn busy_slot_never_blocks_a_build() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let slots = BuildSlots::open(&rig.state)?;
    let _busy = slots.try_acquire(1)?.ok_or("expected free slot")?;
    let mut child = rig.shim(1, &["build"])?;
    assert!(finish(&mut child, Duration::from_secs(10))?);
    assert!(rig.started("end"));
    assert!(stderr_of(&mut child)?.contains("building without one"));
    Ok(())
}

#[test]
fn disk_policy_applies_before_starting_a_new_build() -> Result {
    use provenant_records::policy::{Boundary, Rule, Strength};
    for strength in [Strength::Warn, Strength::Refuse, Strength::Off] {
        let rig = Rig::new()?;
        rig.provision(1)?;
        let rule = Rule {
            code: "disk_floor".into(),
            boundary: Boundary::None,
            strength,
            knob: "domains.fabric.lanes.admission_policy.disk_floor".into(),
            source: "host config".into(),
            justification: "free output space is low".into(),
        };
        let mut child = rig
            .command(1, &["build"])
            .env("PROVENANT_BUILD_DISK_FLOOR", u64::MAX.to_string())
            .env("PROVENANT_BUILD_DISK_RULE", serde_json::to_string(&rule)?)
            .spawn()?;
        let completed = finish(&mut child, Duration::from_secs(10))?;
        assert_eq!(completed, strength != Strength::Refuse);
        assert_eq!(rig.started("start"), strength != Strength::Refuse);
    }
    Ok(())
}
