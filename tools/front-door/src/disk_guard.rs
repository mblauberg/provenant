//! A cheap pressure check. All host observations and effects are injected.
use provenant_config::DiskGuardSettings;
use provenant_core::Clock;
use provenant_os::fs::{Directory, LockMode, PublishMode};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
#[path = "disk_guard_host.rs"]
mod system;

/// Recognise the scheduled form before clap builds its command catalogue.
pub fn early() -> Option<u8> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("disk-guard")) {
        return None;
    }
    let (parsed, globals) = early_arguments(args)?;
    Some(run(parsed, &globals))
}

fn early_arguments(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Option<(provenant_cli::DiskGuard, provenant_cli::Globals)> {
    let mut parsed = provenant_cli::DiskGuard {
        warn: None,
        pause: None,
        resume: None,
        processes: None,
    };
    let mut globals = provenant_cli::Globals::default();
    while let Some(arg) = args.next() {
        let target = match arg.to_str()? {
            "--warn" => &mut parsed.warn,
            "--pause" => &mut parsed.pause,
            "--resume" => &mut parsed.resume,
            "--state-dir" => {
                globals.state_dir = Some(PathBuf::from(args.next()?));
                continue;
            }
            "--processes" => {
                let names: Vec<String> = args
                    .map(|arg| arg.into_string())
                    .collect::<Result<_, _>>()
                    .ok()?;
                // Let clap handle globals and help following the process list.
                if names.iter().any(|name| name.starts_with('-')) {
                    return None;
                }
                parsed.processes = Some(names);
                break;
            }
            _ => return None,
        };
        if target.is_some() {
            return None;
        }
        *target = Some(args.next()?.into_string().ok()?);
    }
    Some((parsed, globals))
}

pub fn run(args: provenant_cli::DiskGuard, globals: &provenant_cli::Globals) -> u8 {
    match run_check(args, globals) {
        Ok(()) => 0,
        Err(error) => {
            let _ = std::io::Write::write_fmt(
                &mut io::stderr().lock(),
                format_args!("disk-guard: {error}\n"),
            );
            1
        }
    }
}

fn state_root(globals: &provenant_cli::Globals) -> io::Result<PathBuf> {
    let state = globals
        .state_dir
        .clone()
        .or_else(|| std::env::var_os("PROVENANT_STATE_DIR").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".local/share/provenant/v1"))
        })
        .ok_or_else(|| io::Error::other("HOME or an absolute state directory is required"))?;
    if !state.is_absolute() {
        return Err(io::Error::other("state directory must be absolute"));
    }
    Ok(state)
}

fn run_check(args: provenant_cli::DiskGuard, globals: &provenant_cli::Globals) -> io::Result<()> {
    let root = state_root(globals)?;
    // The installed plist supplies every threshold, so no config is read here.
    let mut settings = if args.warn.is_some() && args.pause.is_some() && args.resume.is_some() {
        DiskGuardSettings::default()
    } else {
        DiskGuardSettings::read(&root.join("config.toml"))?
    };
    if let Some(warn) = args.warn {
        settings.warn = warn;
    }
    if let Some(pause) = args.pause {
        settings.pause = pause;
    }
    if let Some(resume) = args.resume {
        settings.resume = resume;
    }
    if let Some(processes) = args.processes {
        settings.processes = processes;
    }
    settings.validate().map_err(io::Error::other)?;
    if !settings.enabled {
        return Ok(());
    }
    probe(
        size(&settings.warn)?,
        || provenant_confinement::disk::available_bytes_at(&root),
        || match std::fs::metadata(root.join("disk-guard/paused.json")) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        },
        |free| {
            let directory = guard_directory(&root)?;
            let _lock = directory
                .try_lock("guard.lock".as_ref(), LockMode::Exclusive)
                .map_err(io::Error::other)?;
            let mut state = load(&directory)?;
            let mut host = system::System::new(&root, free)?;
            let uid = provenant_os::fs::effective_uid();
            let clock = provenant_os::clock::SystemClock::new();
            let mut observation = Ok(free);
            loop {
                let result = observation.and_then(|free| {
                    host.free = free;
                    check(
                        &settings,
                        &mut state,
                        clock.wall_now().map_err(io::Error::other)?.as_micros() / 1_000_000,
                        uid,
                        &mut host,
                        &mut |state| save(&directory, state),
                    )
                });
                if let Err(error) = result {
                    if state.paused.is_empty() {
                        return Err(error);
                    }
                    // Retain recovery custody across transient observation/effect
                    // failures. Never use stale free-space data to signal a PID.
                    let _ = host.log(&format!("retrying while builds are paused: {error}"));
                }
                if state.paused.is_empty() {
                    return Ok(());
                }
                // Keep custody of recovery while paused. If this checker exits,
                // the marker sends the next scheduled invocation to recovery.
                clock
                    .wait_until(
                        clock
                            .mono_now()
                            .checked_add(provenant_core::DurationMicros::from_micros(120_000_000))
                            .map_err(io::Error::other)?,
                    )
                    .map_err(io::Error::other)?;
                observation = provenant_confinement::disk::available_bytes_at(&root);
            }
        },
    )
}

fn probe(
    warn: u64,
    mut available: impl FnMut() -> io::Result<u64>,
    pending: impl FnOnce() -> io::Result<bool>,
    pressured: impl FnOnce(u64) -> io::Result<()>,
) -> io::Result<()> {
    let free = available()?;
    if free >= warn && !pending()? {
        return Ok(());
    }
    pressured(free)
}

fn guard_directory(root: &Path) -> io::Result<Directory> {
    provenant_config::private_directory(&root.join("disk-guard"), true).map_err(io::Error::other)
}
fn load(directory: &Directory) -> io::Result<State> {
    match directory.read_bounded("paused.json".as_ref(), 1024 * 1024) {
        Ok(bytes) => return serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(io::Error::other(error)),
    }
    match directory.read_bounded("state.json".as_ref(), 1024 * 1024) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.is_not_found() => Ok(State::default()),
        Err(error) => Err(io::Error::other(error)),
    }
}
fn save(directory: &Directory, state: &State) -> io::Result<()> {
    // The paused-state file is also the marker. Publish recovery custody before
    // STOP, and remove it only after publishing the empty set. A crash between
    // the empty write and unlink can only cause a harmless repeated CONT.
    let name = if state.paused.is_empty() {
        "state.json"
    } else {
        "paused.json"
    };
    directory
        .atomic_write(
            name.as_ref(),
            ".disk-guard-",
            &serde_json::to_vec(state)?,
            PublishMode::Replace,
        )
        .map(|_| ())
        .map_err(io::Error::other)?;
    if state.paused.is_empty() {
        match directory.unlink("paused.json".as_ref()) {
            Ok(()) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    Ok(())
}

pub fn resume_saved(root: &Path) -> io::Result<()> {
    if !root.join("disk-guard").exists() {
        return Ok(());
    }
    let directory = guard_directory(root)?;
    let clock = provenant_os::clock::SystemClock::new();
    let _lock = directory
        .lock(
            "guard.lock".as_ref(),
            LockMode::Exclusive,
            provenant_core::WaitBudget::cli(clock.mono_now(), 5).map_err(io::Error::other)?,
            &provenant_core::CancelToken::new(),
            &clock,
        )
        .map_err(io::Error::other)?;
    let mut state = load(&directory)?;
    if !state.paused.is_empty() {
        resume(
            &mut state,
            provenant_os::fs::effective_uid(),
            &mut system::System::new(root, 0)?,
            &mut |state| save(&directory, state),
        )?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Process {
    pid: u32,
    #[serde(default)]
    ppid: u32,
    uid: u32,
    name: String,
    started: String,
    #[serde(default)]
    started_unix: Option<i64>,
}

#[derive(Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    paused: Vec<Process>,
    warned_at: Option<i64>,
    paused_at: Option<i64>,
    // What `lanes` shows while builds are held: free bytes last seen and the resume level.
    #[serde(default)]
    free: Option<u64>,
    #[serde(default)]
    resume_at: Option<u64>,
    gc_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signal {
    Stop,
    Continue,
}

trait Host {
    fn pid(&self) -> u32;
    fn boot_unix_seconds(&mut self) -> io::Result<i64>;
    fn log(&mut self, message: &str) -> io::Result<()>;
    fn available(&mut self) -> io::Result<u64>;
    fn processes(&mut self) -> io::Result<Vec<Process>>;
    fn process(&mut self, pid: u32) -> io::Result<Option<Process>>;
    // False means the saved identity has exited or its PID was reused.
    fn signal(&mut self, process: &Process, signal: Signal) -> io::Result<bool>;
    fn notify(&mut self, message: &str) -> io::Result<()>;
    fn gc(&mut self) -> io::Result<()>;
}

fn check(
    settings: &DiskGuardSettings,
    state: &mut State,
    now: i64,
    uid: u32,
    host: &mut impl Host,
    save: &mut impl FnMut(&State) -> io::Result<()>,
) -> io::Result<()> {
    let free = host.available()?;
    discard_prior_boot(state, host, save)?;
    let pause = size(&settings.pause)?;
    let resume_bytes = size(&settings.resume)?;
    let warn = size(&settings.warn)?;
    if free < pause {
        state.free = Some(free);
        state.resume_at = Some(resume_bytes);
        let processes = host.processes()?;
        let mut protected = std::collections::HashSet::from([host.pid()]);
        loop {
            let before = protected.len();
            for process in &processes {
                if protected.contains(&process.ppid) {
                    protected.insert(process.pid);
                }
            }
            if protected.len() == before {
                break;
            }
        }
        let mut stopped = false;
        for process in processes {
            if process.uid != uid
                || process.pid <= 1
                || !settings.processes.contains(&process.name)
                || process.name == "provenant"
                || protected.contains(&process.pid)
            {
                continue;
            }
            // Publish intent before STOP so interruption never loses a paused PID.
            if !state.paused.contains(&process) {
                state.paused.push(process.clone());
                if let Err(error) = save(state) {
                    state.paused.pop();
                    return Err(error);
                }
            }
            // STOP is idempotent. Retry a recorded intent even if the previous
            // checker was interrupted after saving it but before signalling.
            match signal_matching(&process, Signal::Stop, uid, host) {
                Ok(true) => stopped = true,
                Ok(false) => {
                    state.paused.retain(|saved| saved != &process);
                    save(state)?;
                }
                Err(error) => return Err(error),
            }
        }
        if stopped && due(state.paused_at, now, 300) {
            let _ = host.notify(&format!(
                "Disk almost full: {} GiB free. Builds paused until {} free.",
                free / GIB,
                settings.resume
            ));
            state.paused_at = Some(now);
            save(state)?;
        }
    } else if free >= resume_bytes && !state.paused.is_empty() {
        resume(state, uid, host, save)?;
    }
    if free < warn {
        if free >= pause && due(state.warned_at, now, 1800) {
            let _ = host.notify(&format!(
                "Disk space low: {} GiB free. Running Provenant gc.",
                free / GIB
            ));
            state.warned_at = Some(now);
            save(state)?;
        }
        if due(state.gc_at, now, 1800) {
            host.gc()?;
            state.gc_at = Some(now);
            save(state)?;
        }
    }
    Ok(())
}

const GIB: u64 = 1024_u64.pow(3);
fn size(text: &str) -> io::Result<u64> {
    provenant_config::parse_cache_size(text).map_err(io::Error::other)
}
fn due(last: Option<i64>, now: i64, interval: i64) -> bool {
    last.is_none_or(|last| now.saturating_sub(last) >= interval)
}
fn signal_matching(
    process: &Process,
    signal: Signal,
    uid: u32,
    host: &mut impl Host,
) -> io::Result<bool> {
    let current = host.process(process.pid)?;
    let matches = current.as_ref().is_some_and(|current| {
        current.pid == process.pid
            && current.uid == process.uid
            && current.name == process.name
            && (signal == Signal::Continue || current.started == process.started)
    });
    if process.pid <= 1 || process.uid != uid || !matches {
        host.log(&format!(
            "skipping {signal:?} for saved PID {}: identity mismatch or process exited",
            process.pid
        ))?;
        return Ok(false);
    }
    host.signal(process, signal)
}
fn resume(
    state: &mut State,
    uid: u32,
    host: &mut impl Host,
    save: &mut impl FnMut(&State) -> io::Result<()>,
) -> io::Result<()> {
    discard_prior_boot(state, host, save)?;
    let mut resumed = false;
    while let Some(process) = state.paused.last() {
        resumed |= signal_matching(process, Signal::Continue, uid, host)?;
        state.paused.pop();
        save(state)?;
    }
    if resumed {
        let _ = host.notify("Disk space recovered: paused builds resumed.");
    }
    Ok(())
}

fn started_unix_seconds(started: &str) -> io::Result<i64> {
    jiff::Timestamp::strptime("%a %b %e %T %Y %z", format!("{started} +0000"))
        .map(|started| started.as_second())
        .map_err(io::Error::other)
}

fn discard_prior_boot(
    state: &mut State,
    host: &mut impl Host,
    save: &mut impl FnMut(&State) -> io::Result<()>,
) -> io::Result<()> {
    if state.paused.is_empty() {
        return Ok(());
    }
    let boot = host.boot_unix_seconds()?;
    for index in (0..state.paused.len()).rev() {
        let process = &state.paused[index];
        // Old records have local-time text without an offset. Use the current
        // UTC observation for those, rather than falsely ageing a live build
        // across the boot boundary. CONT still checks PID, owner and name.
        let started = match process.started_unix {
            Some(started) => Some(started),
            None => host
                .process(process.pid)?
                .and_then(|current| current.started_unix),
        };
        if started.is_some_and(|started| started < boot) {
            host.log(&format!(
                "dropping saved PID {}: start predates current boot",
                process.pid
            ))?;
            state.paused.remove(index);
            save(state)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(dead_code, unused_imports)] // The worktree driver includes this module without libtest.
mod tests {
    use super::*;
    const GIB: u64 = 1024_u64.pow(3);
    fn process(pid: u32, uid: u32, name: &str) -> Process {
        Process {
            pid,
            ppid: 1,
            uid,
            name: name.into(),
            started: "Mon Oct 26 12:00:00 2026".into(),
            started_unix: Some(started_unix_seconds("Mon Oct 26 12:00:00 2026").unwrap()),
        }
    }
    #[derive(Default)]
    struct Fake {
        free: u64,
        processes: Vec<Process>,
        signals: Vec<(u32, Signal)>,
        notifications: Vec<String>,
        gc: Vec<String>,
        logs: Vec<String>,
        boot: i64,
        fail_free: bool,
        fail_signal: bool,
        lists: usize,
    }
    impl Host for Fake {
        fn pid(&self) -> u32 {
            99
        }
        fn boot_unix_seconds(&mut self) -> io::Result<i64> {
            Ok(self.boot)
        }
        fn log(&mut self, message: &str) -> io::Result<()> {
            self.logs.push(message.into());
            Ok(())
        }
        fn available(&mut self) -> io::Result<u64> {
            if self.fail_free {
                Err(io::Error::other("unavailable"))
            } else {
                Ok(self.free)
            }
        }
        fn processes(&mut self) -> io::Result<Vec<Process>> {
            self.lists += 1;
            Ok(self.processes.clone())
        }
        fn process(&mut self, pid: u32) -> io::Result<Option<Process>> {
            Ok(self
                .processes
                .iter()
                .find(|process| process.pid == pid)
                .cloned())
        }
        fn signal(&mut self, process: &Process, signal: Signal) -> io::Result<bool> {
            if self.fail_signal {
                return Err(io::Error::other("signal failed"));
            }
            self.signals.push((process.pid, signal));
            Ok(true)
        }
        fn notify(&mut self, message: &str) -> io::Result<()> {
            self.notifications.push(message.into());
            Ok(())
        }
        fn gc(&mut self) -> io::Result<()> {
            self.gc.push("configured floor".into());
            Ok(())
        }
    }
    fn tick(host: &mut Fake, state: &mut State, now: i64) {
        check(
            &DiskGuardSettings::default(),
            state,
            now,
            501,
            host,
            &mut |_| Ok(()),
        )
        .unwrap();
    }
    #[test]
    fn healthy_probe_performs_one_free_space_read_and_one_marker_probe() {
        // With no paused-state marker, a healthy scheduled check skips the
        // cold closure and its state/process/log/GC effects.
        for free in [60 * GIB, 100 * GIB] {
            let mut reads = 0;
            let mut probes = 0;
            probe(
                60 * GIB,
                || {
                    reads += 1;
                    Ok(free)
                },
                || {
                    probes += 1;
                    Ok(false)
                },
                |_| panic!("unexpected cold-path I/O"),
            )
            .unwrap();
            assert_eq!(reads, 1);
            assert_eq!(probes, 1);
        }
    }
    #[test]
    fn next_healthy_tick_recovers_after_the_paused_checker_dies() {
        let tmp = tempfile::tempdir().unwrap();
        let directory = guard_directory(&tmp.path().canonicalize().unwrap()).unwrap();
        let original = process(10, 501, "cargo");
        let mut host = Fake {
            free: 24 * GIB,
            processes: vec![original.clone()],
            ..Fake::default()
        };
        let mut state = State::default();
        check(
            &DiskGuardSettings::default(),
            &mut state,
            0,
            501,
            &mut host,
            &mut |state| save(&directory, state),
        )
        .unwrap();
        assert_eq!(host.signals, [(10, Signal::Stop)]);
        assert!(tmp.path().join("disk-guard/paused.json").exists());
        // Lose all checker memory. The next scheduled tick has only disk state.
        drop(state);
        drop(host);
        let mut next = Fake {
            free: 100 * GIB,
            processes: vec![original],
            ..Fake::default()
        };
        probe(
            60 * GIB,
            || Ok(100 * GIB),
            || Ok(tmp.path().join("disk-guard/paused.json").exists()),
            |_| {
                let mut state = load(&directory)?;
                check(
                    &DiskGuardSettings::default(),
                    &mut state,
                    120,
                    501,
                    &mut next,
                    &mut |state| save(&directory, state),
                )
            },
        )
        .unwrap();
        assert_eq!(next.signals, [(10, Signal::Continue)]);
        assert_eq!(next.notifications.len(), 1);
        assert!(load(&directory).unwrap().paused.is_empty());
        assert!(!tmp.path().join("disk-guard/paused.json").exists());
    }
    #[test]
    fn guard_and_descendants_are_excluded_even_with_custom_process_names() {
        let mut child = process(100, 501, "sh");
        child.ppid = 99;
        let mut grandchild = process(101, 501, "cargo");
        grandchild.ppid = 100;
        let mut host = Fake {
            free: 24 * GIB,
            // The grandchild precedes its parent to exercise transitive protection.
            processes: vec![
                grandchild,
                child,
                process(99, 501, "cargo"),
                process(102, 501, "provenant"),
                process(10, 501, "cargo"),
            ],
            ..Fake::default()
        };
        let mut settings = DiskGuardSettings::default();
        settings.processes.extend(["sh".into(), "provenant".into()]);
        let mut state = State::default();
        check(&settings, &mut state, 0, 501, &mut host, &mut |_| Ok(())).unwrap();
        assert_eq!(host.signals, [(10, Signal::Stop)]);
        assert_eq!(state.paused, [process(10, 501, "cargo")]);
    }
    #[test]
    fn records_from_before_boot_are_discarded_without_a_resume_notification() {
        let original = process(10, 501, "cargo");
        for free in [24 * GIB, 100 * GIB] {
            let mut host = Fake {
                free,
                boot: started_unix_seconds("Tue Oct 27 00:00:00 2026").unwrap(),
                processes: if free >= 50 * GIB {
                    vec![process(10, 501, "cargo")]
                } else {
                    vec![]
                },
                ..Fake::default()
            };
            let mut state = State {
                paused: vec![original.clone()],
                ..State::default()
            };
            tick(&mut host, &mut state, 0);
            assert!(state.paused.is_empty());
            assert!(host.signals.is_empty());
            assert!(host.notifications.is_empty());
            assert_eq!(host.logs.len(), 1);
        }
    }
    #[test]
    fn fast_arguments_accept_the_plist_and_defer_globals_after_processes_to_clap() {
        let arguments = |args: &[&str]| {
            args.iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
                .into_iter()
        };
        let (args, globals) = early_arguments(arguments(&[
            "--warn",
            "60G",
            "--pause",
            "25G",
            "--resume",
            "50G",
            "--processes",
            "cargo",
            "clang++",
        ]))
        .unwrap();
        assert_eq!(args.warn.as_deref(), Some("60G"));
        assert_eq!(args.processes.unwrap(), ["cargo", "clang++"]);
        assert!(globals.state_dir.is_none());
        assert!(
            early_arguments(arguments(&[
                "--processes",
                "cargo",
                "--state-dir",
                "/tmp/state"
            ]))
            .is_none()
        );
        assert!(early_arguments(arguments(&["--help"])).is_none());
    }
    #[test]
    fn baked_settings_skip_config_and_create_no_state_on_the_healthy_path() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("config.toml"), b"this is invalid toml").unwrap();
        let args = provenant_cli::DiskGuard {
            warn: Some("3B".into()),
            pause: Some("1B".into()),
            resume: Some("2B".into()),
            processes: Some(vec![]),
        };
        run_check(
            args,
            &provenant_cli::Globals {
                state_dir: Some(tmp.path().into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!tmp.path().join("disk-guard").exists());
    }
    #[test]
    fn unchanged_ticks_do_not_write_state_or_list_processes() {
        let mut host = Fake {
            free: 40 * GIB,
            ..Fake::default()
        };
        let mut state = State::default();
        tick(&mut host, &mut state, 0);
        check(
            &DiskGuardSettings::default(),
            &mut state,
            120,
            501,
            &mut host,
            &mut |_| panic!("unchanged state must not be written"),
        )
        .unwrap();
        assert_eq!(host.lists, 0);
        host.free = 24 * GIB;
        tick(&mut host, &mut state, 240);
        assert_eq!(host.lists, 1);
        check(
            &DiskGuardSettings::default(),
            &mut state,
            360,
            501,
            &mut host,
            &mut |_| panic!("unchanged critical state must not be written"),
        )
        .unwrap();
    }
    #[test]
    fn failed_intent_write_never_stops_a_process_and_can_be_retried() {
        let mut host = Fake {
            free: 24 * GIB,
            processes: vec![process(10, 501, "cargo")],
            ..Default::default()
        };
        let mut state = State::default();
        assert!(
            check(
                &DiskGuardSettings::default(),
                &mut state,
                0,
                501,
                &mut host,
                &mut |_| Err(io::Error::other("disk full"))
            )
            .is_err()
        );
        assert!(host.signals.is_empty());
        assert!(state.paused.is_empty());
        tick(&mut host, &mut state, 120);
        assert_eq!(host.signals, [(10, Signal::Stop)]);
    }
    #[test]
    fn saved_intent_retries_a_failed_stop_without_duplicate_records() {
        let mut host = Fake {
            free: 24 * GIB,
            processes: vec![process(10, 501, "cargo")],
            fail_signal: true,
            ..Default::default()
        };
        let mut state = State::default();
        let mut saved = Vec::new();
        assert!(
            check(
                &DiskGuardSettings::default(),
                &mut state,
                0,
                501,
                &mut host,
                &mut |state| {
                    saved.push(serde_json::to_vec(state)?);
                    Ok(())
                }
            )
            .is_err()
        );
        state = serde_json::from_slice(saved.last().unwrap()).unwrap();
        host.fail_signal = false;
        tick(&mut host, &mut state, 120);
        assert_eq!(state.paused.len(), 1);
        assert_eq!(host.signals, [(10, Signal::Stop)]);
        // Repeated STOP is harmless and must not republish unchanged state.
        check(
            &DiskGuardSettings::default(),
            &mut state,
            240,
            501,
            &mut host,
            &mut |_| panic!("unchanged state was rewritten"),
        )
        .unwrap();
        assert_eq!(host.signals, [(10, Signal::Stop), (10, Signal::Stop)]);
    }
    #[test]
    fn uninstall_resumes_only_recorded_identities_and_persists_the_empty_set() {
        let tmp = tempfile::tempdir().unwrap();
        let directory = guard_directory(&tmp.path().canonicalize().unwrap()).unwrap();
        let original = process(10, 501, "cargo");
        save(
            &directory,
            &State {
                paused: vec![original.clone()],
                ..State::default()
            },
        )
        .unwrap();
        let mut state = load(&directory).unwrap();
        let mut host = Fake {
            processes: vec![original, process(11, 501, "rustc")],
            ..Fake::default()
        };
        resume(&mut state, 501, &mut host, &mut |state| {
            save(&directory, state)
        })
        .unwrap();
        assert_eq!(host.signals, [(10, Signal::Continue)]);
        assert_eq!(host.lists, 0);
        assert!(load(&directory).unwrap().paused.is_empty());
    }
    #[test]
    fn pauses_exact_names_for_this_user_and_resumes_only_at_resume_level() {
        let mut host = Fake {
            free: 25 * GIB,
            processes: vec![
                process(10, 501, "cargo"),
                process(11, 502, "rustc"),
                process(12, 501, "cargo-helper"),
            ],
            ..Fake::default()
        };
        let mut state = State::default();
        tick(&mut host, &mut state, 0);
        assert!(host.signals.is_empty());
        host.free -= 1;
        let mut saved = Vec::new();
        check(
            &DiskGuardSettings::default(),
            &mut state,
            1,
            501,
            &mut host,
            &mut |state| {
                saved.push(serde_json::to_vec(state)?);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(host.signals, [(10, Signal::Stop)]);
        assert_eq!(
            serde_json::from_slice::<State>(&saved[0]).unwrap().paused,
            [process(10, 501, "cargo")]
        );
        let saved_state = serde_json::from_slice::<State>(&saved[0]).unwrap();
        assert_eq!(saved_state.free, Some(25 * GIB - 1));
        assert_eq!(saved_state.resume_at, Some(50 * GIB));
        state = serde_json::from_slice(saved.last().unwrap()).unwrap();
        host.free = 49 * GIB;
        tick(&mut host, &mut state, 120);
        assert_eq!(host.signals.len(), 1);
        host.processes.push(process(13, 501, "rustc"));
        host.free = 50 * GIB;
        tick(&mut host, &mut state, 240);
        assert_eq!(host.signals, [(10, Signal::Stop), (10, Signal::Continue)]);
        assert!(state.paused.is_empty());
    }
    #[test]
    fn warning_gc_and_pause_notifications_have_independent_rate_limits() {
        let mut host = Fake {
            free: 60 * GIB,
            ..Fake::default()
        };
        let mut state = State::default();
        tick(&mut host, &mut state, 0);
        assert!(host.notifications.is_empty());
        host.free -= 1;
        tick(&mut host, &mut state, 0);
        tick(&mut host, &mut state, 1799);
        assert_eq!(host.notifications.len(), 1);
        assert_eq!(host.gc, ["configured floor"]);
        tick(&mut host, &mut state, 1800);
        assert_eq!(host.notifications.len(), 2);
        assert_eq!(host.gc.len(), 2);
        host.free = 24 * GIB;
        host.processes.push(process(10, 501, "cargo"));
        tick(&mut host, &mut state, 1801);
        tick(&mut host, &mut state, 2100);
        assert_eq!(host.notifications.len(), 3);
        tick(&mut host, &mut state, 2101);
        assert_eq!(host.notifications.len(), 4);
        assert_eq!(host.gc.len(), 2);
    }
    #[test]
    fn recovery_continues_matching_pid_uid_and_name_after_start_text_changes() {
        let original = process(10, 501, "cargo");
        let mut current = original.clone();
        current.started = "Mon Oct 26 22:00:00 2026".into();
        let mut state = State {
            paused: vec![original],
            ..State::default()
        };
        let mut host = Fake {
            free: 60 * GIB,
            processes: vec![current],
            ..Fake::default()
        };
        tick(&mut host, &mut state, 0);
        assert_eq!(host.signals, [(10, Signal::Continue)]);
        assert!(state.paused.is_empty());
    }
    #[test]
    fn legacy_local_time_records_resume_when_their_text_looks_older_than_boot() {
        let mut original = process(10, 501, "cargo");
        original.started = "Mon Oct 26 05:10:00 2026".into();
        original.started_unix = None;
        let mut current = process(10, 501, "cargo");
        current.started = "Mon Oct 26 12:10:00 2026".into();
        current.started_unix = Some(started_unix_seconds(&current.started).unwrap());
        let mut host = Fake {
            free: 100 * GIB,
            boot: started_unix_seconds("Mon Oct 26 12:00:00 2026").unwrap(),
            processes: vec![current],
            ..Fake::default()
        };
        let mut state = State {
            paused: vec![original],
            ..State::default()
        };
        resume(&mut state, 501, &mut host, &mut |_| Ok(())).unwrap();
        assert_eq!(host.signals, [(10, Signal::Continue)]);
        assert!(state.paused.is_empty());
    }
    #[test]
    fn no_builds_stopped_means_no_pause_notification() {
        let mut host = Fake {
            free: 24 * GIB,
            ..Fake::default()
        };
        tick(&mut host, &mut State::default(), 0);
        assert!(host.notifications.is_empty());
    }
    #[test]
    fn failed_observations_and_reused_pids_never_signal_unrelated_processes() {
        let original = process(10, 501, "cargo");
        let mut state = State {
            paused: vec![original.clone()],
            ..State::default()
        };
        let mut host = Fake {
            free: 100 * GIB,
            fail_free: true,
            ..Fake::default()
        };
        assert!(
            check(
                &DiskGuardSettings::default(),
                &mut state,
                0,
                501,
                &mut host,
                &mut |_| Ok(())
            )
            .is_err()
        );
        assert_eq!(state.paused, std::slice::from_ref(&original));
        host.fail_free = false;
        let mut reused = original;
        reused.name = "unrelated".into();
        host.processes = vec![reused, process(11, 501, "cargo")];
        tick(&mut host, &mut state, 1);
        assert!(host.signals.is_empty());
        assert!(state.paused.is_empty());
        assert!(host.notifications.is_empty());
        assert_eq!(host.logs.len(), 1);
    }
}
