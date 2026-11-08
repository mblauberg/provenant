//! The lane's `cargo` shim end to end: `internal build-slot` takes a host build
//! slot, runs the command it was given as its child and keeps the slot until
//! that exits. A fake cargo stands in for the compiler; nothing here needs a
//! sandbox. The slot directory, count and wait reach the shim through its
//! environment, as the host's shim script supplies them.
#![allow(clippy::disallowed_methods)]
use provenant_os::build_slots::{BuildSlots, DIRECTORY};
use std::{
    fs,
    io::Write,
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
    release: fs::File,
    /// Prefix of where a waiting build tells the host it waits; each command
    /// adds its pid.
    marker: PathBuf,
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
        let fifo = root.join("build-release");
        assert!(Command::new("mkfifo").arg(&fifo).status()?.success());
        let release = fs::OpenOptions::new().read(true).write(true).open(&fifo)?;
        let binary = env!("CARGO_BIN_EXE_provenant");
        let slot_dir = state.join(DIRECTORY.join("/"));
        fs::write(
            &cargo,
            format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                 build) echo \"start $$\" >> '{log}'; if [ \"${{PROVENANT_TEST_HELD_BUILD:-}}\" = 1 ]; then IFS= read -r release < '{fifo}'; fi; echo \"end $$\" >> '{log}' ;;\n\
                 fmt) echo \"fmt $$\" >> '{log}' ;;\n\
                 check) echo \"hung $$\" >> '{log}'; exec /bin/sleep 60 ;;\n\
                 bg) /bin/sleep 60 >/dev/null 2>&1 </dev/null & echo \"bg $!\" >> '{log}' ;;\n\
                 tamper) rm -f '{slots}/slot-0.lock' 2>/dev/null; echo \"rm $?\" >> '{log}'; /usr/bin/touch '{slots}/slot-9.lock' 2>/dev/null; echo \"create $?\" >> '{log}'; echo \"start $$\" >> '{log}'; echo \"end $$\" >> '{log}' ;;\n\
                 test) echo \"outer $$\" >> '{log}'; '{binary}' internal build-slot -- \"$0\" build ;;\n\
                 esac\n",
                log = log.display(),
                fifo = fifo.display(),
                slots = slot_dir.display(),
            ),
        )?;
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            marker: root.join("build-slot-wait"),
            _temp: temp,
            state,
            cargo,
            log,
            release,
        })
    }

    /// Host side: create the slot files, as `provenant run` does at launch.
    fn provision(&self, slots: u64) -> Result {
        BuildSlots::provision(&self.state, slots)?;
        Ok(())
    }

    fn command(&self, slots: u64, wait_secs: u64, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_provenant"));
        command
            .args(["internal", "build-slot", "--"])
            .arg(&self.cargo)
            .args(args)
            .env("PROVENANT_BUILD_SLOT_STATE", &self.state)
            .env("PROVENANT_BUILD_SLOTS", slots.to_string())
            .env("PROVENANT_BUILD_WAIT_SECS", wait_secs.to_string())
            .env("PROVENANT_BUILD_WAIT_MARKER", &self.marker)
            .env_remove("PROVENANT_BUILD_SLOT")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command
    }

    fn shim(&self, slots: u64, args: &[&str]) -> Result<Child> {
        Ok(self.command(slots, 120, args).spawn()?)
    }

    fn held_build(&self, slots: u64) -> Result<Child> {
        Ok(self
            .command(slots, 120, &["build"])
            .env("PROVENANT_TEST_HELD_BUILD", "1")
            .spawn()?)
    }

    fn log(&self) -> Result<Vec<String>> {
        Ok(fs::read_to_string(&self.log)?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// Wait markers now present, one per waiting command.
    fn markers(&self) -> Vec<PathBuf> {
        let dir = self.marker.parent().map(PathBuf::from).unwrap_or_default();
        let prefix = self
            .marker
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&format!("{prefix}.")))
            })
            .collect()
    }

    fn release(&self) -> Result {
        writeln!(&self.release, "continue")?;
        Ok(())
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
fn more_builds_than_slots_compile_serially() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let mut children = (0..4)
        .map(|_| rig.held_build(1))
        .collect::<Result<Vec<_>>>()?;
    for started in 1..=4 {
        let _observation = provenant_test_support::wait_for(
            &format!("{started} builds started"),
            Duration::from_secs(30),
            || rig.log().map_err(|error| error.to_string()),
            |log| {
                log.as_ref().is_ok_and(|log| {
                    log.iter().filter(|line| line.starts_with("start ")).count() >= started
                })
            },
        )?;
        rig.release()?;
    }
    for child in &mut children {
        assert!(finish(child, Duration::from_secs(30))?);
    }
    let log = rig.log()?;
    assert_eq!(log.len(), 8);
    // One slot: every start is followed by its own end before the next start.
    for pair in log.chunks(2) {
        let (start, end) = (&pair[0], &pair[1]);
        assert_eq!(
            start.strip_prefix("start "),
            end.strip_prefix("end "),
            "{log:?}"
        );
    }
    Ok(())
}

#[test]
fn two_slots_allow_two_builds_at_once_and_no_more() -> Result {
    let rig = Rig::new()?;
    rig.provision(2)?;
    let mut children = (0..4)
        .map(|_| rig.held_build(2))
        .collect::<Result<Vec<_>>>()?;
    for started in 2..=4 {
        let _observation = provenant_test_support::wait_for(
            &format!("{started} builds started"),
            Duration::from_secs(30),
            || rig.log().map_err(|error| error.to_string()),
            |log| {
                log.as_ref().is_ok_and(|log| {
                    log.iter().filter(|line| line.starts_with("start ")).count() >= started
                })
            },
        )?;
        rig.release()?;
    }
    rig.release()?;
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

/// Two commands wait at once. Each has its own marker, so one that gets a slot
/// and removes its marker cannot hide the wait of the other.
#[test]
fn waiting_commands_keep_separate_markers() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let mut holder = rig.shim(1, &["check"])?;
    assert!(provenant_test_support::wait_for(
        "holder started",
        Duration::from_secs(20),
        || rig.started("hung"),
        |ready| *ready
    )?);
    let mut first = rig.held_build(1)?;
    let mut second = rig.held_build(1)?;
    provenant_test_support::wait_for(
        "two waiting markers",
        Duration::from_secs(20),
        || rig.markers(),
        |markers| markers.len() == 2,
    )?;
    holder.kill()?;
    holder.wait()?;
    // The first to finish removes only its own marker.
    let _observation = provenant_test_support::wait_for(
        "first build starts and one marker remains",
        Duration::from_secs(30),
        || (rig.log().map_err(|error| error.to_string()), rig.markers()),
        |(log, markers)| {
            log.as_ref()
                .is_ok_and(|log| log.iter().any(|line| line.starts_with("start")))
                && markers.len() == 1
        },
    )?;
    rig.release()?;
    let _observation = provenant_test_support::wait_for(
        "second build starts",
        Duration::from_secs(30),
        || rig.log().map_err(|error| error.to_string()),
        |log| {
            log.as_ref()
                .is_ok_and(|log| log.iter().filter(|line| line.starts_with("start ")).count() == 2)
        },
    )?;
    rig.release()?;
    assert!(finish(&mut first, Duration::from_secs(30))?);
    assert!(finish(&mut second, Duration::from_secs(30))?);
    assert!(rig.markers().is_empty());
    Ok(())
}

#[test]
fn a_killed_build_releases_its_slot_and_the_wait_is_reported() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let mut holder = rig.shim(1, &["check"])?;
    assert!(provenant_test_support::wait_for(
        "holder started",
        Duration::from_secs(20),
        || rig.started("hung"),
        |ready| *ready
    )?);
    let mut waiter = rig.shim(1, &["build"])?;
    let slots = BuildSlots::open(&rig.state)?;
    provenant_test_support::wait_for(
        "slot held",
        Duration::from_secs(20),
        || slots.held(1),
        |held| *held == 1,
    )?;
    assert!(provenant_test_support::wait_for(
        "build slot readiness",
        Duration::from_secs(20),
        || {
            rig.markers()
                .iter()
                .any(|path| fs::read_to_string(path).is_ok_and(|text| text.starts_with("slot ")))
        },
        |ready| *ready
    )?);
    assert!(
        !rig.started("start"),
        "the second build waits while the first holds the slot"
    );
    // The host reads this to show the lane as waiting in `lanes` and `status`.
    assert!(provenant_test_support::wait_for(
        "build slot readiness",
        Duration::from_secs(20),
        || {
            rig.markers()
                .iter()
                .any(|path| fs::read_to_string(path).is_ok_and(|text| text.starts_with("slot ")))
        },
        |ready| *ready
    )?);
    // SIGKILL leaves no chance to release anything: the kernel does.
    holder.kill()?;
    holder.wait()?;
    // The orphaned fake cargo (`sleep 60`) does not keep the slot: the
    // descriptor is close-on-exec, so only the dead shim held it.
    assert!(finish(&mut waiter, Duration::from_secs(30))?);
    assert!(rig.started("end"));
    assert!(
        rig.markers().is_empty(),
        "the marker goes once the wait ends"
    );
    Ok(())
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

#[test]
fn a_wait_gives_up_after_the_configured_time_naming_the_slots() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let mut holder = rig.shim(1, &["check"])?;
    assert!(provenant_test_support::wait_for(
        "holder started",
        Duration::from_secs(20),
        || rig.started("hung"),
        |ready| *ready
    )?);
    let begun = Instant::now();
    let mut waiter = rig.command(1, 1, &["build"]).spawn()?;
    assert!(
        !finish(&mut waiter, Duration::from_secs(20))?,
        "a build that never got a slot fails"
    );
    assert!(begun.elapsed() < Duration::from_secs(15));
    assert!(!rig.started("start"), "the build never ran");
    holder.kill()?;
    holder.wait()?;
    Ok(())
}

#[test]
fn a_held_cargo_lock_wait_is_bounded_too() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let profile = rig.state.join("target/debug");
    fs::create_dir_all(&profile)?;
    // Another cargo holds the target directory's build lock.
    let lock = fs::File::create(profile.join(".cargo-lock"))?;
    lock.try_lock()?;
    let mut waiter = rig
        .command(1, 1, &["build"])
        .env("CARGO_TARGET_DIR", rig.state.join("target"))
        .spawn()?;
    assert!(!finish(&mut waiter, Duration::from_secs(20))?);
    assert!(!rig.started("start"));
    // Released, the same command runs.
    drop(lock);
    let mut again = rig
        .command(1, 5, &["build"])
        .env("CARGO_TARGET_DIR", rig.state.join("target"))
        .spawn()?;
    assert!(finish(&mut again, Duration::from_secs(20))?);
    Ok(())
}

#[test]
fn a_spoofed_marker_does_not_skip_the_gate_unless_it_names_a_live_ancestor() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let mut holder = rig.shim(1, &["check"])?;
    assert!(provenant_test_support::wait_for(
        "holder started",
        Duration::from_secs(20),
        || rig.started("hung"),
        |ready| *ready
    )?);
    let holder_pid = holder.id().to_string();
    // The holder is live but not an ancestor of these shims; nor is init, a
    // pid that does not exist, or text.
    for marker in [holder_pid.as_str(), "1", "999999", "not-a-pid", "0", "-1"] {
        let mut spoof = rig
            .command(1, 1, &["build"])
            .env("PROVENANT_BUILD_SLOT", marker)
            .spawn()?;
        assert!(
            !finish(&mut spoof, Duration::from_secs(20))?,
            "marker {marker} must not skip the gate"
        );
        assert!(
            stderr_of(&mut spoof)?.contains("gave up waiting"),
            "{marker}"
        );
    }
    assert!(!rig.started("start"));
    holder.kill()?;
    holder.wait()?;
    Ok(())
}

/// The marker names a live ancestor with its true pid and start time, but that
/// ancestor is a shell, not a provenant shim, so the gate still applies.
#[test]
fn a_marker_naming_a_live_ancestor_that_is_not_a_shim_does_not_skip_the_gate() -> Result {
    use provenant_supervisor::census::{ProcessObservation, SystemProcessObserver};
    let rig = Rig::new()?;
    rig.provision(1)?;
    let mut holder = rig.shim(1, &["check"])?;
    assert!(provenant_test_support::wait_for(
        "holder started",
        Duration::from_secs(20),
        || rig.started("hung"),
        |ready| *ready
    )?);
    let result = rig.log.with_extension("result");
    // The shell stays the shim's parent: the shim is not its last command.
    let script = format!(
        "IFS= read -r marker; PROVENANT_BUILD_SLOT=$marker '{bin}' internal build-slot -- '{cargo}' build; echo \"exit $?\" > '{r}'; exit 0",
        bin = env!("CARGO_BIN_EXE_provenant"),
        cargo = rig.cargo.display(),
        r = result.display(),
    );
    let mut shell = Command::new("/bin/sh")
        .args(["-c", &script])
        .env("PROVENANT_BUILD_SLOT_STATE", &rig.state)
        .env("PROVENANT_BUILD_SLOTS", "1")
        .env("PROVENANT_BUILD_WAIT_SECS", "1")
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let pid = i64::from(shell.id());
    let ProcessObservation::Present(fact) = SystemProcessObserver.observe(pid) else {
        return Err("shell not observable".into());
    };
    writeln!(
        shell.stdin.take().ok_or("shell stdin")?,
        "{pid}:{}",
        fact.identity.native_start
    )?;
    assert!(provenant_test_support::wait_for(
        "shell result written",
        Duration::from_secs(30),
        || result.exists(),
        |ready| *ready
    )?);
    assert!(
        fs::read_to_string(&result)?.contains("exit 1"),
        "the build waited for the held slot and gave up"
    );
    assert!(!rig.started("start"), "the build never ran");
    shell.wait()?;
    holder.kill()?;
    holder.wait()?;
    Ok(())
}

#[test]
fn cargo_lock_wait_transitions_to_a_bounded_slot_wait() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let mut holder = rig.shim(1, &["check"])?;
    assert!(provenant_test_support::wait_for(
        "holder started",
        Duration::from_secs(20),
        || rig.started("hung"),
        |ready| *ready
    )?);
    let profile = rig.state.join("target/debug");
    fs::create_dir_all(&profile)?;
    let lock = fs::File::create(profile.join(".cargo-lock"))?;
    lock.try_lock()?;
    let mut waiter = rig
        .command(1, 1, &["build"])
        .env("CARGO_TARGET_DIR", rig.state.join("target"))
        .spawn()?;
    // Observe the Cargo-lock wait before releasing it. Exact shared-budget
    // subtraction and expiry are covered without sleeping by
    // build_slot::tests::cargo_lock_time_reduces_slot_budget.
    assert!(provenant_test_support::wait_for(
        "build slot readiness",
        Duration::from_secs(20),
        || {
            rig.markers()
                .iter()
                .any(|path| fs::read_to_string(path).is_ok_and(|text| text.starts_with("lock ")))
        },
        |ready| *ready
    )?);
    drop(lock);
    assert!(!finish(&mut waiter, Duration::from_secs(30))?);
    assert!(stderr_of(&mut waiter)?.contains("gave up waiting for a build slot"));
    assert!(!rig.started("start"), "the build never ran without a slot");
    holder.kill()?;
    holder.wait()?;
    Ok(())
}

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
    // marker or slot settings would decide this test, and a held slot would
    // turn the probe into a wait up to `build_wait_minutes`.
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
        .command(1, 5, &["no-such-subcommand-exits-zero"])
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
