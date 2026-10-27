//! Cold-path OS effects. Every process command has a bounded supervised wait.
use super::{Host, Process, Signal};
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use std::{
    ffi::OsString,
    io,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) struct System {
    pub free: u64,
    root: PathBuf,
    executor: SystemExecutor,
}

impl System {
    pub fn new(root: &Path, free: u64) -> io::Result<Self> {
        Ok(Self {
            free,
            root: root.into(),
            executor: SystemExecutor::new(2).map_err(io::Error::other)?,
        })
    }
    fn command(&self, argv: &[OsString]) -> io::Result<(bool, Vec<u8>)> {
        let environment = vec![
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("LC_ALL".into(), "C".into()),
            ("TZ".into(), "UTC0".into()),
            (
                "HOME".into(),
                std::env::var_os("HOME").ok_or_else(|| io::Error::other("missing HOME"))?,
            ),
            (
                "PROVENANT_STATE_DIR".into(),
                self.root.as_os_str().to_owned(),
            ),
        ];
        let mut options = ExecOptions::new(argv, &self.root, Duration::from_secs(10));
        options.env = Some(&environment);
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture {
            limit: 4 * 1024 * 1024,
        };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        let output = self.executor.execute(options).map_err(io::Error::other)?;
        if output.timed_out
            || output.stdout.truncated
            || output.drain_truncated
            || output.overflow_stopped
        {
            return Err(io::Error::other("disk guard host command incomplete"));
        }
        Ok((
            output.termination == Termination::Exit(0),
            output.stdout.bytes,
        ))
    }
    fn ps(&self, selector: &str, value: String) -> io::Result<Vec<Process>> {
        let (ok, bytes) = self.command(&[
            "/bin/ps".into(),
            "-ww".into(),
            selector.into(),
            value.into(),
            "-o".into(),
            "pid=,ppid=,uid=,lstart=,comm=".into(),
        ])?;
        if !ok && selector != "-p" {
            return Err(io::Error::other("could not list build processes"));
        }
        parse_processes(&String::from_utf8_lossy(&bytes))
    }
    pub(super) fn log(&self, message: &str) -> io::Result<()> {
        use provenant_core::Clock;
        let now = provenant_os::clock::SystemClock::new()
            .wall_now()
            .map_err(io::Error::other)?;
        let directory = super::guard_directory(&self.root)?;
        let mut file = directory
            .open_append("log".as_ref())
            .map_err(io::Error::other)?;
        writeln!(file, "{} {message}", now.as_micros() / 1_000_000)
    }
}

impl Host for System {
    fn pid(&self) -> u32 {
        std::process::id()
    }
    fn boot_unix_seconds(&mut self) -> io::Result<i64> {
        provenant_os::clock::boot_unix_seconds()
    }
    fn log(&mut self, message: &str) -> io::Result<()> {
        System::log(self, message)
    }
    fn available(&mut self) -> io::Result<u64> {
        Ok(self.free)
    }
    fn processes(&mut self) -> io::Result<Vec<Process>> {
        self.ps("-U", provenant_os::fs::effective_uid().to_string())
    }
    fn process(&mut self, pid: u32) -> io::Result<Option<Process>> {
        Ok(self.ps("-p", pid.to_string())?.into_iter().next())
    }
    fn signal(&mut self, process: &Process, signal: Signal) -> io::Result<bool> {
        if process.pid <= 1 || process.uid != provenant_os::fs::effective_uid() {
            return Ok(false);
        }
        provenant_os::pid::signal_process(
            i64::from(process.pid),
            match signal {
                Signal::Stop => provenant_os::pid::ProcessSignal::Stop,
                Signal::Continue => provenant_os::pid::ProcessSignal::Continue,
            },
        )
        .map_err(io::Error::other)
    }
    fn notify(&mut self, message: &str) -> io::Result<()> {
        self.log(message)?;
        // Notification text is an argv value, never AppleScript source.
        let script = "on run argv\ndisplay notification (item 1 of argv) with title \"Provenant disk guard\" sound name \"Basso\"\nend run";
        self.command(&[
            "/usr/bin/osascript".into(),
            "-e".into(),
            script.into(),
            message.into(),
        ])?;
        Ok(())
    }
    fn gc(&mut self) -> io::Result<()> {
        self.log("gc --scheduled --apply")?;
        // The bounded shell exits after starting GC, with all inherited pipes
        // redirected. Arguments, including paths, never become shell source.
        let program = std::env::current_exe()?;
        let log = self.root.join("disk-guard/gc.log");
        let (ok, _) = self.command(&gc_arguments(program, log))?;
        if ok {
            Ok(())
        } else {
            Err(io::Error::other("could not launch disk guard gc"))
        }
    }
}

fn gc_arguments(program: PathBuf, log: PathBuf) -> [OsString; 9] {
    [
        "/bin/sh".into(),
        "-c".into(),
        "log=$1; shift; \"$@\" >>\"$log\" 2>&1 </dev/null &".into(),
        "disk-guard-gc".into(),
        log.into_os_string(),
        program.into_os_string(),
        "gc".into(),
        "--scheduled".into(),
        "--apply".into(),
    ]
}

fn parse_processes(text: &str) -> io::Result<Vec<Process>> {
    Ok(text
        .lines()
        .filter_map(|line| parse_process(line).ok())
        .collect())
}

fn parse_process(line: &str) -> io::Result<Process> {
    let mut fields = line.split_whitespace();
    let mut next = || {
        fields
            .next()
            .ok_or_else(|| io::Error::other("short ps record"))
    };
    let pid = next()?.parse().map_err(io::Error::other)?;
    let ppid = next()?.parse().map_err(io::Error::other)?;
    let uid = next()?.parse().map_err(io::Error::other)?;
    let started = [next()?, next()?, next()?, next()?, next()?].join(" ");
    let started_unix = Some(super::started_unix_seconds(&started)?);
    let executable = fields.collect::<Vec<_>>().join(" ");
    let name = Path::new(&executable)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("missing ps executable name"))?
        .into();
    Ok(Process {
        pid,
        ppid,
        uid,
        name,
        started,
        started_unix,
    })
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // The worktree driver includes this module without libtest.
    use super::*;
    #[test]
    fn pressure_gc_leaves_the_configured_free_floor_in_effect() {
        let argv = gc_arguments("/bin/provenant".into(), "/state/gc.log".into());
        let arguments = argv
            .iter()
            .skip(6)
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(arguments, ["gc", "--scheduled", "--apply"]);
    }
    #[test]
    fn malformed_records_do_not_hide_valid_builds() {
        let processes = parse_processes("bad pid\n42 1 501 Mon Oct 26 12:00:00 2026 cargo\n43 no-parent 501 Mon Oct 26 12:00:00 2026 rustc\n44 1 501 invalid start time here now cargo\n45 1 501 Tue Oct 27 12:00:00 2026 rustc\n46 1 501 Tue Oct 27 12:00:00 2026\n").unwrap();
        assert_eq!(
            processes
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>(),
            [42, 45]
        );
    }
    #[test]
    fn process_records_preserve_identity_and_exact_basename() {
        let processes = parse_processes(" 42 1 501 Mon Oct 26 12:00:00 2026 /build dir/bin/clang++\n 43 1 502 Tue Oct 27 12:00:00 2026 cargo-helper\n").unwrap();
        assert_eq!(processes[0].name, "clang++");
        assert_eq!(processes[0].pid, 42);
        assert_eq!(processes[0].uid, 501);
        assert_ne!(processes[0].started, processes[1].started);
        assert_eq!(processes[1].name, "cargo-helper");
        assert!(parse_processes("42 501 cargo").unwrap().is_empty());
    }
}
