//! S2 attempt supervisor. Python decides; this process holds and publishes.
mod files;
mod group;
mod observer;
mod platform;
mod progress;
mod spec;
mod state;

use files::{Attempt, Capture, ExitRecord, Notes};
use group::ProviderGroup;
use observer::{Lines, Observer};
use platform::Reactor;
use spec::Spec;
use state::{Action, Event, Phase, State, StopReason, step};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Read};
use std::marker::PhantomData;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Exited;
struct Lane<S> {
    files: Attempt,
    record: ExitRecord,
    _stage: PhantomData<S>,
}
impl Lane<Exited> {
    /// The terminal state is consumed; an error returns custody for retry/report.
    fn publish(self) -> Result<(), (Box<Self>, io::Error)> {
        let data = self.record.encode();
        match self.files.publish(data.as_bytes()) {
            Ok(()) => Ok(()),
            Err(e) => Err((Box::new(self), e)),
        }
    }
}
fn emit(output: &mut platform::EventOutput, kind: &str, fields: &str) {
    let line = format!(
        "{{\"event\":{},{}\n",
        files::quote(kind),
        fields.trim().strip_prefix('{').unwrap_or(fields)
    );
    // Nonblocking, bounded writes. A disconnected or stalled owner has no
    // influence over the provider's lifetime.
    output.emit(line.as_bytes());
}
fn apply(s: &mut State, event: Event) -> Vec<Action> {
    let (next, a) = step(std::mem::take(s), event);
    *s = next;
    a
}
fn add_deadline(now: Instant, millis: u64) -> Instant {
    now.checked_add(Duration::from_millis(millis))
        .unwrap_or(now + Duration::from_secs(100 * 365 * 86400))
}

pub fn run(args: &[OsString]) -> u8 {
    if args != ["run"] {
        eprintln!("provenant lane: expected run");
        return 64;
    }
    let mut line = String::new();
    let result = std::io::stdin()
        .lock()
        .take(2 * 1024 * 1024 + 1)
        .read_line(&mut line);
    if result.is_err() || line.len() > 2 * 1024 * 1024 {
        eprintln!("provenant lane: invalid or oversized launch line");
        return 64;
    }
    let spec = match Spec::parse(&line) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("provenant lane: {e}");
            return 64;
        }
    };
    // Credentials only lived in the input string; never persist that string.
    let redacted = match spec.redacted(&line) {
        Ok(redacted) => redacted,
        Err(_) => {
            eprintln!("provenant lane: invalid launch record");
            return 64;
        }
    };
    drop(line);
    let input = match &spec.input {
        Some(path) => match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("provenant lane: stdin: {e}");
                return 64;
            }
        },
        None => None,
    };
    let attempt = match Attempt::open(&spec.attempt) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("provenant lane: supervisor custody: {e}");
            return 75;
        }
    };
    if attempt.exists("supervisor.exit.json") {
        eprintln!("provenant lane: attempt already terminal");
        return 64;
    }
    let mut io = Notes::default();
    let reactor = match Reactor::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("provenant lane: event loop: {e}");
            return 75;
        }
    };
    let _ = platform::nonblocking(std::io::stdout().as_raw_fd());
    let mut output = platform::EventOutput::new(std::io::stdout().as_raw_fd());
    io.result(
        "launch",
        attempt.atomic("launch.json", redacted.as_bytes(), false),
    );
    let mut out = Capture::new(attempt.create(&spec.events), spec.cap);
    let mut err = Capture::new(attempt.create(&spec.stderr), spec.cap);
    let started = match files::timestamp() {
        Ok(started) => started,
        Err(error) => {
            eprintln!("provenant lane: system clock: {error}");
            return 75;
        }
    };
    let mut record = ExitRecord {
        pid: None,
        started: started.clone(),
        ended: String::new(),
        progress: started.clone(),
        exit: None,
        signal: None,
        forced: None,
        terminal: false,
        reaped: vec![],
        spared: vec![],
        warnings: Notes::default(),
        io: Notes::default(),
        out: String::new(),
        err: String::new(),
        observer_lost: false,
        observer_dropped: 0,
    };
    let streams = format!(
        "{{\"events.jsonl\":{},\"stderr.log\":{},\"started_at\":{}}}\n",
        files::quote(&spec.events.to_string_lossy()),
        files::quote(&spec.stderr.to_string_lossy()),
        files::quote(&started)
    );
    io.result(
        "capture paths",
        attempt.atomic("watchdog-streams.json", streams.as_bytes(), false),
    );
    if spec.cancelled() {
        record.forced = Some("cancelled".into());
    } else {
        let previous = io
            .result("subreaper", ProviderGroup::enable_subreaper())
            .unwrap_or(0);
        let mut command = Command::new(&spec.argv[0]);
        command
            .args(&spec.argv[1..])
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(&spec.env)
            .stdin(input.map(Stdio::from).unwrap_or_else(Stdio::piped))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let spawned = platform::prepare_child(&mut command).and_then(|_| command.spawn());
        match spawned {
            Err(e) => io.add(format!("spawn: {e}")),
            Ok(mut child) => {
                let mut stdout = child.stdout.take();
                let mut stderr = child.stderr.take();
                let mut stdin = child.stdin.take();
                // Acquire the RAII owner immediately after spawn, before any
                // fallible I/O. Its Drop covers unwinding as well as normal exit.
                let mut group = ProviderGroup::new(
                    child,
                    spec.attempt.clone(),
                    spec.spare_host,
                    spec.child_grace,
                    spec.root_flush,
                    previous,
                );
                record.pid = Some(group.pid);
                io.result("child watch", reactor.watch_child(group.pid));
                for fd in [
                    stdout.as_ref().unwrap().as_raw_fd(),
                    stderr.as_ref().unwrap().as_raw_fd(),
                ] {
                    io.result("pipe nonblocking", platform::nonblocking(fd));
                }
                io.result("reserve", attempt.reserve());
                let pid = format!("{}\n", group.pid);
                io.result("pgid", attempt.atomic("pgid", pid.as_bytes(), false));
                let canonical =
                    platform::process(group.pid).and_then(|r| files::canonical_start(r.epoch));
                let start = format!(
                    "{{\"pid\":{},\"pgid\":{},\"started_at\":{}}}",
                    group.pid,
                    group.pid,
                    files::nullable(canonical.as_deref())
                );
                emit(&mut output, "started", &start);
                let mut observer = Observer::start(&spec);
                if let Some(pid) = observer.pid() {
                    group.exclude_owned_child(pid);
                }
                let mut out_lines = Lines::new();
                let mut err_lines = Lines::new();
                let began = Instant::now();
                let mut last_output = began;
                let mut last_progress = began;
                let mut last_event_at = None;
                let mut last_output_at = None;
                let mut next_watch = add_deadline(began, spec.silence);
                let mut next_progress = began;
                let mut grace = None;
                let mut settle = None;
                let mut finished = None;
                let mut cpu = group.cpu_stamp();
                let mut workspace = spec.writer.as_ref().map(|(root, exclude)| {
                    progress::Progress::new(root.clone(), exclude.clone(), &spec.attempt)
                });
                let mut state = State::default();
                let mut signals = Vec::new();
                loop {
                    // Observe the zombie before applying requests from this tick;
                    // natural exit wins a cancel marker or signal delivered late.
                    if group.observed_exit()
                        && !matches!(
                            state.phase,
                            Phase::Settling | Phase::Stopping | Phase::Exited
                        )
                    {
                        let actions = apply(&mut state, Event::ChildExit);
                        if actions.contains(&Action::StartSettle) {
                            settle = Some(add_deadline(Instant::now(), spec.settle));
                        }
                    }
                    let mut events = Vec::new();
                    // Each stream gets a bounded read budget per turn.
                    for is_out in [true, false] {
                        let fd = if is_out {
                            stdout.as_ref().map(|s| s.as_raw_fd())
                        } else {
                            stderr.as_ref().map(|s| s.as_raw_fd())
                        };
                        let Some(fd) = fd else {
                            continue;
                        };
                        let mut data = [0u8; 65536];
                        let mut eof = false;
                        for _ in 0..4 {
                            let n = unsafe { libc::read(fd, data.as_mut_ptr().cast(), data.len()) };
                            if n == 0 {
                                eof = true;
                                break;
                            }
                            if n < 0 {
                                let e = io::Error::last_os_error();
                                if e.kind() == io::ErrorKind::Interrupted {
                                    continue;
                                }
                                if e.kind() != io::ErrorKind::WouldBlock {
                                    io.add(format!("capture read: {e}"));
                                    events.push(Event::IoError);
                                    eof = true;
                                }
                                break;
                            }
                            let bytes = &data[..n as usize];
                            if is_out {
                                out.write(bytes);
                                out_lines.write(bytes, "out", &mut observer);
                                events.push(Event::Stdout);
                                if let Ok(stamp) = files::timestamp() {
                                    last_event_at = Some(stamp);
                                }
                            } else {
                                err.write(bytes);
                                err_lines.write(bytes, "err", &mut observer);
                            }
                            last_output = Instant::now();
                            last_progress = last_output;
                            if let Ok(stamp) = files::timestamp() {
                                record.progress = stamp;
                            }
                            last_output_at = Some(record.progress.clone());
                            next_watch = add_deadline(last_output, spec.silence);
                        }
                        if eof {
                            if is_out {
                                stdout = None;
                                out_lines.finish("out", &mut observer);
                            } else {
                                stderr = None;
                                err_lines.finish("err", &mut observer);
                            }
                        }
                    }
                    if group.observed_exit()
                        && !matches!(
                            state.phase,
                            Phase::Settling | Phase::Stopping | Phase::Exited
                        )
                    {
                        apply(&mut state, Event::ChildExit);
                        settle = Some(add_deadline(Instant::now(), spec.settle));
                    }
                    events.extend(observer.pump());
                    let current = Instant::now();
                    if state.phase != Phase::Exited {
                        if signals.iter().any(|s| *s != libc::SIGCHLD) {
                            events.push(Event::Stop(StopReason::SupervisorSignal));
                        }
                        signals.clear();
                        if spec.cancelled() {
                            events.push(Event::Stop(StopReason::Cancel));
                        }
                        if current.duration_since(began) >= Duration::from_millis(spec.wall) {
                            events.push(Event::WallDeadline);
                        }
                        if current.duration_since(began) >= Duration::from_millis(spec.startup) {
                            events.push(Event::StartupDeadline);
                        }
                        if grace.is_some_and(|at| current >= at) {
                            events.push(Event::GraceDeadline);
                        }
                        if settle.is_some_and(|at| current >= at) {
                            events.push(Event::SettleDeadline);
                        }
                        if current >= next_watch {
                            events.push(Event::Silence);
                            next_watch = add_deadline(current, spec.silence);
                        }
                    }
                    for event in events {
                        for action in apply(&mut state, event) {
                            match action {
                                Action::CloseStdin => {
                                    drop(stdin.take());
                                }
                                Action::StartGrace => {
                                    grace = Some(add_deadline(current, spec.settle))
                                }
                                Action::StartSettle => {
                                    settle = Some(add_deadline(current, spec.settle))
                                }
                                Action::Stop(reason) => {
                                    drop(stdin.take());
                                    group.stop(reason);
                                }
                                Action::Observe => {
                                    let alive = !group.observed_exit();
                                    let w = format!(
                                        concat!(
                                            "{{\"schema\":\"fabric.watchdog.v1\",\"checked_at\":{},\"pid\":{},\"process_alive\":{},",
                                            "\"silent_seconds\":{},\"last_event_at\":{},\"last_output_at\":{},\"last_worktree_write_at\":{},\"worktree_scan_complete\":{}}}\n"
                                        ),
                                        files::quote(
                                            &files::timestamp()
                                                .unwrap_or_else(|_| record.progress.clone())
                                        ),
                                        group.pid,
                                        alive,
                                        last_output.elapsed().as_secs_f64(),
                                        files::nullable(last_event_at.as_deref()),
                                        files::nullable(last_output_at.as_deref()),
                                        files::nullable(
                                            workspace.as_ref().and_then(|w| w.latest.as_deref())
                                        ),
                                        workspace.as_ref().is_some_and(|w| w.scan_complete())
                                    );
                                    io.result(
                                        "watchdog",
                                        attempt.atomic("watchdog.json", w.as_bytes(), false),
                                    );
                                }
                                Action::Publish => {}
                            }
                        }
                    }
                    if current >= next_progress && state.phase != Phase::Exited {
                        next_progress = add_deadline(current, 1000);
                        let changed = workspace.as_mut().is_some_and(|w| w.probe());
                        let new_cpu = group.cpu_stamp();
                        if changed || (!cpu.is_empty() && !new_cpu.is_empty() && cpu != new_cpu) {
                            last_progress = current;
                            if let Ok(stamp) = files::timestamp() {
                                record.progress = stamp;
                            }
                        }
                        cpu = new_cpu;
                        let live = format!(
                            "{{\"schema\":\"provenant.lane.supervisor.v1\",\"pid\":{},\"pgid\":{},\"started_at\":{},\"last_progress_at\":{},\"run_token\":{},\"descendants\":{},\"descendants_truncated\":{}}}\n",
                            group.pid,
                            group.pid,
                            files::quote(&started),
                            files::quote(&record.progress),
                            files::quote(&spec.token),
                            group.census_json(),
                            group.census_omitted()
                        );
                        io.result(
                            "supervisor",
                            attempt.atomic("supervisor.json", live.as_bytes(), false),
                        );
                        emit(
                            &mut output,
                            "progress",
                            &format!(
                                "{{\"last_progress_at\":{}}}",
                                files::quote(&record.progress)
                            ),
                        );
                    }
                    group.tick();
                    if group.done && state.phase != Phase::Exited {
                        apply(&mut state, Event::CleanupDone);
                        finished = Some(current);
                        drop(stdin.take());
                    }
                    if state.phase == Phase::Exited {
                        // Spared descendants can keep pipes open. Drain only up
                        // to root_flush, then let the observer settle on EOF.
                        if stdout.is_none() && stderr.is_none()
                            || finished.is_some_and(|at| {
                                current.duration_since(at) >= Duration::from_millis(spec.root_flush)
                            })
                        {
                            stdout = None;
                            stderr = None;
                            out_lines.finish("out", &mut observer);
                            err_lines.finish("err", &mut observer);
                            observer.close();
                        }
                        if observer.finished() && stdout.is_none() && stderr.is_none() {
                            break;
                        }
                        if finished.is_some_and(|at| {
                            current.duration_since(at)
                                >= Duration::from_millis(
                                    spec.root_flush.saturating_add(spec.settle),
                                )
                        }) {
                            record.observer_lost = true;
                            break;
                        }
                    }
                    let mut fds = observer.fds();
                    if let Some(s) = &stdout {
                        fds.push((s.as_raw_fd(), false));
                    }
                    if let Some(s) = &stderr {
                        fds.push((s.as_raw_fd(), false));
                    }
                    output.flush();
                    match reactor.wait(&fds, 20) {
                        Ok(s) => signals = s,
                        Err(e) => io.add(format!("event loop: {e}")),
                    }
                    let _ = last_progress;
                }
                record.exit = group.status.and_then(|s| s.0);
                record.signal = group.status.and_then(|s| s.1);
                record.forced = state
                    .forced
                    .as_ref()
                    .and_then(|r| r.forced().map(str::to_string));
                record.terminal = state.terminal;
                record.observer_lost |=
                    state.observer_lost || observer.lost || observer.dropped > 0;
                record.observer_dropped = observer.dropped;
                record.reaped = group.reaped();
                record.spared = group.spared();
                record.warnings = std::mem::take(&mut group.warnings);
            }
        }
    }
    out.finish();
    err.finish();
    if let Some(e) = &out.error {
        io.add(format!("events: {e}"));
    }
    if let Some(e) = &err.error {
        io.add(format!("stderr: {e}"));
    }
    record.out = out.json();
    record.err = err.json();
    record.io = io;
    record.ended = files::timestamp().unwrap_or_else(|_| record.progress.clone());
    let encoded = record.encode();
    let lane = Lane::<Exited> {
        files: attempt,
        record,
        _stage: PhantomData,
    };
    let status = match lane.publish() {
        Ok(()) => 0,
        Err((_lane, e)) => {
            eprintln!("provenant lane: exit publication: {e}");
            75
        }
    };
    emit(&mut output, "exited", &encoded);
    output.finish(Duration::from_millis(50));
    status
}

#[cfg(feature = "test-fixtures")]
#[doc(hidden)]
pub mod fixtures;
