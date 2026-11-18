//! Named admission classes end to end: `provenant admit <CLASS>` takes a host
//! admission slot if free, runs its child and keeps the slot until
//! that exits. A fake binary stands in for heavy commands; nothing here needs a
//! sandbox. The slot directory and count reach the shim through its
//! environment, as the host's shim script supplies them.
#![allow(clippy::disallowed_methods)]
use provenant_os::admission::{AdmissionSlots, DIRECTORY};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
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
        let slot_dir = state.join(DIRECTORY.join("/")).join("build");
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
                 test) echo \"outer $$\" >> '{log}'; '{binary}' admit build -- \"$0\" build ;;\n\
                 nested_class) echo \"outer $$\" >> '{log}'; '{binary}' admit \"$2\" -- \"$0\" build ;;\n\
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
        AdmissionSlots::provision(&self.state, "build", slots)?;
        Ok(())
    }

    fn provision_class(&self, class: &str, slots: u64) -> Result {
        AdmissionSlots::provision(&self.state, class, slots)?;
        Ok(())
    }

    fn command(&self, slots: u64, args: &[&str]) -> Result<Command> {
        self.command_class("build", slots, args)
    }

    fn command_class(&self, class: &str, slots: u64, args: &[&str]) -> Result<Command> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_provenant"));
        command
            .args(["admit", class, "--"])
            .arg(&self.cargo)
            .args(args)
            .env("PROVENANT_ADMISSION_SLOT_STATE", &self.state)
            .env("PROVENANT_ADMISSION_SLOTS", slots.to_string())
            .env_remove(format!("PROVENANT_ADMISSION_SLOT_{class}"))
            .env_remove(format!(
                "PROVENANT_ADMISSION_SLOT_{}",
                class.to_ascii_uppercase()
            ))
            .env_remove(provenant_core::consumer_env::ADMISSION_SLOT)
            .env_remove(provenant_core::consumer_env::BUILD_SLOT)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        set_builtin_admission(&mut command)?;
        Ok(command)
    }

    fn shim(&self, slots: u64, args: &[&str]) -> Result<Child> {
        Ok(self.command(slots, args)?.spawn()?)
    }

    fn shim_class(&self, class: &str, slots: u64, args: &[&str]) -> Result<Child> {
        Ok(self.command_class(class, slots, args)?.spawn()?)
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

fn set_builtin_admission(command: &mut Command) -> Result {
    command
        .env(
            provenant_core::consumer_env::ADMISSION_BACKEND,
            serde_json::to_string(&provenant_config::PortBackend::Builtin)?,
        )
        .env(
            provenant_core::consumer_env::ADMISSION_SOURCE,
            serde_json::to_string(&provenant_config::PortSource::Builtin)?,
        );
    Ok(())
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
    let slots = AdmissionSlots::open(&rig.state, "build")?;
    assert_eq!(slots.held(1), 0);
    let mut second = rig.shim(1, &["build"])?;
    assert!(finish(&mut second, Duration::from_secs(10))?);
    assert!(alive(&pid), "still running after the next build finished");
    Command::new("/bin/kill").arg(&pid).status()?;
    Ok(())
}

#[test]
fn a_terminated_shim_passes_the_signal_to_its_build() -> Result {
    for signal in ["TERM", "HUP", "INT"] {
        let rig = Rig::new()?;
        rig.provision(1)?;
        let mut shim = rig.shim(1, &["check"])?;
        assert!(provenant_test_support::wait_for(
            "admission slot readiness",
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
                "admission slot readiness",
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
fn nested_cargo_does_not_queue_behind_its_parent() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    // A command that itself runs the same admission class must not wait for a second slot:
    // re-entrancy is detected and passes through.
    let mut outer = rig.shim(1, &["test"])?;
    assert!(finish(&mut outer, Duration::from_secs(10))?);
    assert!(rig.started("outer"));
    assert!(rig.started("end"));
    Ok(())
}

#[test]
fn concurrent_cargo_commands_wait_for_the_single_host_slot() -> Result {
    use std::io::BufRead;
    let rig = Rig::new()?;
    rig.provision(1)?;
    let scratch = rig.state.join("scratch");
    fs::create_dir_all(scratch.join(provenant_os::admission::SHIM_DIRECTORY))?;
    let shim = scratch
        .join(provenant_os::admission::SHIM_DIRECTORY)
        .join("cargo");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\nexec '{}' admit build -- '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_provenant"),
            rig.cargo.display()
        ),
    )?;
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o700))?;
    let agent_command = |args: &str, cwd: &std::path::Path| -> Result<Command> {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", args])
            .current_dir(cwd)
            .env(
                "PATH",
                scratch.join(provenant_os::admission::SHIM_DIRECTORY),
            )
            .env("PROVENANT_ADMISSION_SLOT_STATE", &rig.state)
            .env("PROVENANT_ADMISSION_SLOTS", "1")
            .env(provenant_core::consumer_env::ADMISSION_WAIT_DIR, &scratch)
            .env_remove("PROVENANT_ADMISSION_SLOT_build")
            .env_remove("PROVENANT_ADMISSION_SLOT_BUILD")
            .env_remove(provenant_core::consumer_env::ADMISSION_SLOT)
            .env_remove(provenant_core::consumer_env::BUILD_SLOT)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        set_builtin_admission(&mut command)?;
        Ok(command)
    };
    let mut first = agent_command("exec cargo check", &rig.state)?.spawn()?;
    assert!(provenant_test_support::wait_for(
        "first build started",
        Duration::from_secs(20),
        || rig.started("hung"),
        |ready| *ready
    )?);
    let mut second = agent_command("exec cargo build", &scratch)?.spawn()?;
    let mut stderr = std::io::BufReader::new(second.stderr.take().ok_or("stderr")?);
    let mut line = String::new();
    loop {
        line.clear();
        if stderr.read_line(&mut line)? == 0 {
            break;
        }
        if line.contains("waiting for a host build slot") {
            break;
        }
    }
    assert!(line.contains("waiting for a host build slot"), "{line}");
    assert!(provenant_os::admission::is_waiting(&scratch).is_some());
    assert!(!rig.started("start"), "the second build must wait");
    assert!(
        Command::new("/bin/kill")
            .args(["-TERM", &first.id().to_string()])
            .status()?
            .success()
    );
    first.wait()?;
    assert!(finish(&mut second, Duration::from_secs(10))?);
    assert!(provenant_os::admission::is_waiting(&scratch).is_none());
    assert!(rig.started("end"));
    Ok(())
}

#[test]
fn read_only_slot_files_cannot_be_unlinked_or_created_by_the_build() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    // The lane's access: read the directory and its files, write neither.
    let dir = rig.state.join(DIRECTORY.join("/")).join("build");
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
    let slots = rig.state.join(DIRECTORY.join("/")).join("build");
    fs::create_dir_all(slots.parent().unwrap())?;
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
fn missing_builtin_slot_settings_still_run_the_build() -> Result {
    let rig = Rig::new()?;
    // Absent slot settings run without slot acquisition.
    let mut command = Command::new(env!("CARGO_BIN_EXE_provenant"));
    command
        .args(["admit", "build", "--"])
        .arg(&rig.cargo)
        .arg("build")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    set_builtin_admission(&mut command)?;
    let mut child = command.spawn()?;
    assert!(finish(&mut child, Duration::from_secs(10))?);
    assert!(rig.started("end"), "Command ran without a slot grant");
    Ok(())
}

#[test]
fn absent_or_malformed_port_metadata_does_not_start_cargo() -> Result {
    let rig = Rig::new()?;
    let builtin = serde_json::to_string(&provenant_config::PortBackend::Builtin)?;
    let source = serde_json::to_string(&provenant_config::PortSource::Builtin)?;
    let cases = [
        (None, Some(source.as_str())),
        (Some("{"), Some(source.as_str())),
        (Some(builtin.as_str()), None),
        (Some(builtin.as_str()), Some("\"unknown\"")),
    ];
    for (backend, source) in cases {
        let mut command = Command::new(env!("CARGO_BIN_EXE_provenant"));
        command
            .args(["admit", "build", "--"])
            .arg(&rig.cargo)
            .arg("build")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(backend) = backend {
            command.env(provenant_core::consumer_env::ADMISSION_BACKEND, backend);
        }
        if let Some(source) = source {
            command.env(provenant_core::consumer_env::ADMISSION_SOURCE, source);
        }
        assert!(
            !command.status()?.success(),
            "invalid metadata must fail closed"
        );
        assert!(!rig.started("end"), "invalid metadata must not run Cargo");
    }
    Ok(())
}

#[test]
fn the_shims_exit_code_is_the_builds() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;
    let status = rig
        .command(1, &["no-such-subcommand-exits-zero"])?
        .status()?;
    assert!(status.success());
    let mut failing_command = Command::new(env!("CARGO_BIN_EXE_provenant"));
    failing_command
        .args(["admit", "build", "--", "/bin/sh", "-c", "exit 7"])
        .env_remove("PROVENANT_ADMISSION_SLOT_STATE")
        .env_remove(provenant_core::consumer_env::BUILD_SLOT)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    set_builtin_admission(&mut failing_command)?;
    let failing = failing_command.status()?;
    assert_eq!(failing.code(), Some(7));
    Ok(())
}

#[test]
fn disk_policy_applies_before_starting_a_new_build() -> Result {
    use provenant_records::diagnostic::SettingSource;
    use provenant_records::policy::{Policy, Source, Strength};
    for strength in Strength::ALL {
        let mut policy = Policy::default();
        policy.overlay(
            &std::collections::BTreeMap::from([(
                provenant_config::DISK_FLOOR.to_owned(),
                strength,
            )]),
            Source::Host,
        );
        let effective = policy.rule(provenant_config::DISK_FLOOR).rule.strength;
        for (critical, below) in [(0, false), (u64::MAX, true)] {
            let rig = Rig::new()?;
            rig.provision(1)?;
            let gate = provenant_config::DiskGate::new(
                provenant_config::DiskLimits {
                    free_floor: u64::MAX,
                    free_floor_source: SettingSource::Configuration,
                    critical_free: critical,
                    critical_free_source: SettingSource::ShippedDefault,
                },
                &policy,
            );
            let mut child = rig
                .command(1, &["build"])?
                .env(
                    "PROVENANT_ADMISSION_DISK_LIMITS",
                    serde_json::to_string(&gate)?,
                )
                .spawn()?;
            let starts = !(below && effective == Strength::Refuse);
            let completed = finish(&mut child, Duration::from_secs(10))?;
            assert_eq!(completed, starts);
            assert_eq!(rig.started("start"), starts);
            // Whatever is reported names the figures and their sources.
            let message = stderr_of(&mut child)?;
            if !(below && effective == Strength::Off) {
                for fact in [
                    "gc.free_floor",
                    "gc.critical_free",
                    "host configuration",
                    "shipped default",
                ] {
                    assert!(message.contains(fact), "{fact}: {message}");
                }
            }
        }
    }
    Ok(())
}

#[test]
fn disk_refusal_allows_non_build_commands() -> Result {
    use provenant_records::diagnostic::SettingSource;
    use provenant_records::policy::{Policy, Source, Strength};
    let mut policy = Policy::default();
    policy.overlay(
        &std::collections::BTreeMap::from([(
            provenant_config::DISK_FLOOR.to_owned(),
            Strength::Refuse,
        )]),
        Source::Host,
    );
    let gate = provenant_config::DiskGate::new(
        provenant_config::DiskLimits {
            free_floor: u64::MAX,
            free_floor_source: SettingSource::Configuration,
            critical_free: u64::MAX,
            critical_free_source: SettingSource::Configuration,
        },
        &policy,
    );
    let rig = Rig::new()?;
    rig.provision(1)?;
    for command in ["clean", "metadata", "fmt", "--version"] {
        let mut child = rig
            .command(1, &[command])?
            .env(
                "PROVENANT_ADMISSION_DISK_LIMITS",
                serde_json::to_string(&gate)?,
            )
            .spawn()?;
        assert!(
            finish(&mut child, Duration::from_secs(10))?,
            "{command}: {}",
            stderr_of(&mut child)?
        );
    }
    Ok(())
}

#[test]
fn distinct_admission_classes_are_isolated() -> Result {
    let rig = Rig::new()?;
    rig.provision_class("build", 1)?;
    rig.provision_class("heavy", 1)?;

    // Hold the "build" class slot
    let mut build_hold = rig.shim_class("build", 1, &["check"])?;
    assert!(provenant_test_support::wait_for(
        "build class slot held",
        Duration::from_secs(20),
        || rig.started("hung"),
        |ready| *ready
    )?);

    // "heavy" class slot is free and starts immediately despite "build" being occupied
    let mut heavy = rig.shim_class("heavy", 1, &["build"])?;
    assert!(finish(&mut heavy, Duration::from_secs(10))?);
    assert!(rig.started("start"));
    assert!(rig.started("end"));

    // Clean up held process
    Command::new("/bin/kill")
        .args(["-TERM", &build_hold.id().to_string()])
        .status()?;
    build_hold.wait()?;
    Ok(())
}

#[test]
fn bounded_wait_warns_and_proceeds() -> Result {
    let rig = Rig::new()?;
    rig.provision(1)?;

    // Occupy the only slot
    let mut hold = rig.shim(1, &["check"])?;
    assert!(provenant_test_support::wait_for(
        "slot held",
        Duration::from_secs(20),
        || rig.started("hung"),
        |ready| *ready
    )?);

    // Run second command with a 1-second timeout
    let mut waiting_cmd = rig.command(1, &["build"])?;
    waiting_cmd.env("PROVENANT_ADMISSION_WAIT_SECONDS", "1");
    let mut child = waiting_cmd.spawn()?;

    assert!(finish(&mut child, Duration::from_secs(15))?);
    let stderr = stderr_of(&mut child)?;
    assert!(
        stderr.contains("build slot unavailable, building without one")
            || stderr.contains("admission slot unavailable, proceeding without one"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("bounded wait ended with every build slot busy"),
        "stderr: {stderr}"
    );

    // Clean up held process
    Command::new("/bin/kill")
        .args(["-TERM", &hold.id().to_string()])
        .status()?;
    hold.wait()?;
    Ok(())
}
