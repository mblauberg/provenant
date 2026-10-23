mod common;
use common::{Sandbox, BINARY};
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

fn invoke(spec: &str) -> std::process::Output {
    let mut child = Command::new(BINARY)
        .args(["lane", "run"])
        .env_remove("PROVENANT_FRONT_DOOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{spec}\n").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn unknown_launch_major_refuses_without_starting() {
    let output = invoke(r#"{"schema":"provenant.lane.launch.v2"}"#);
    assert_eq!(
        output.status.code(),
        Some(64),
        "{}",
        common::stderr(&output)
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn normal_exit_publishes_provider_status_and_redacts_environment() {
    let sandbox = Sandbox::new("lane-normal");
    let dir = sandbox.dir.display();
    let output = invoke(&format!(
        r#"{{"schema":"provenant.lane.launch.v1","argv":["/bin/sh","-c","printf 'hello\\n'; printf 'diagnostic\\n' >&2; exit 7"],"cwd":"{dir}","attempt_dir":"{dir}","run_token":"test","env":{{"PATH":"/usr/bin:/bin","SECRET":"private-value"}},"stdin":{{}},"capture":{{"events":"events.jsonl","stderr":"stderr.log","max_bytes":1024}},"silence_observation_ms":1000,"timeouts_ms":{{"startup":1000,"wall":2000,"settle":20,"child_grace":20,"root_flush":50}},"cancel_markers":[],"observer":{{"argv":[]}},"spare":{{"code_mode_host":true}}}}"#
    ));
    assert_eq!(output.status.code(), Some(0), "{}", common::stderr(&output));
    let record = fs::read_to_string(sandbox.dir.join("supervisor.exit.json")).unwrap();
    assert!(record.contains("\"exit\":7"), "{record}");
    assert!(record.contains("provenant.lane.exit.v1"));
    assert_eq!(
        fs::read_to_string(sandbox.dir.join("events.jsonl")).unwrap(),
        "hello\n"
    );
    let launch = fs::read_to_string(sandbox.dir.join("launch.json")).unwrap();
    assert!(!launch.contains("private-value"));
    assert!(launch.contains("SECRET"));
    assert!(launch.contains("\"timeouts_ms\""), "{launch}");
    assert!(launch.contains("\"capture\""), "{launch}");
}

fn quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}
struct Fixture {
    sandbox: Sandbox,
    script: String,
    startup: u64,
    wall: u64,
    cap: usize,
    observer: String,
    markers: String,
    silence: u64,
    writer: bool,
}
impl Fixture {
    fn new(name: &str, script: &str) -> Self {
        Self {
            sandbox: Sandbox::new(name),
            script: script.into(),
            startup: 2000,
            wall: 3000,
            cap: 1024,
            observer: "[]".into(),
            markers: "[]".into(),
            silence: 1000,
            writer: false,
        }
    }
    fn spec(&self) -> String {
        let dir = quote(&self.sandbox.dir.to_string_lossy());
        let helper = quote(&std::env::current_exe().unwrap().to_string_lossy());
        format!(concat!("{{\"schema\":\"provenant.lane.launch.v1\",\"argv\":[\"/bin/sh\",\"-c\",{}],\"cwd\":{},\"attempt_dir\":{},\"run_token\":\"test\",",
            "\"env\":{{\"PATH\":\"/usr/bin:/bin\",\"S2_HELPER\":{},\"S2_DIR\":{}}},\"stdin\":{{}},\"capture\":{{\"events\":\"events.jsonl\",\"stderr\":\"stderr.log\",\"max_bytes\":{}}},",
            "\"silence_observation_ms\":{},\"timeouts_ms\":{{\"startup\":{},\"wall\":{},\"settle\":40,\"child_grace\":40,\"root_flush\":150}},",
            "\"progress\":{},\"cancel_markers\":{},\"observer\":{{\"argv\":{}}},\"spare\":{{\"code_mode_host\":true}}}}"),
            quote(&self.script),dir,dir,helper,dir,self.cap,self.silence,self.startup,self.wall,
            if self.writer {format!("{{\"writer_tree\":{{\"root\":{},\"exclude\":[\"ignored\"]}}}}",dir)}else {"{}".into()},self.markers,self.observer)
    }
    fn run(&self) -> String {
        let output = invoke(&self.spec());
        assert_eq!(output.status.code(), Some(0), "{}", common::stderr(&output));
        let stdout = common::stdout(&output);
        assert!(stdout.contains("\"event\":\"started\""), "{stdout}");
        assert!(stdout.contains("\"event\":\"exited\""), "{stdout}");
        fs::read_to_string(self.sandbox.dir.join("supervisor.exit.json")).unwrap()
    }
}
#[test]
fn startup_deadline_counts_stdout_not_stderr() {
    let mut f = Fixture::new("lane-startup", "printf 'banner\\n' >&2; exec /bin/sleep 10");
    f.startup = 80;
    let record = f.run();
    assert!(
        record.contains("\"forced\":\"startup_timeout\""),
        "{record}"
    );
}
#[test]
fn wall_deadline_stops_a_started_provider() {
    let mut f = Fixture::new("lane-wall", "printf 'ready\\n'; exec /bin/sleep 10");
    f.wall = 100;
    let record = f.run();
    assert!(record.contains("\"forced\":\"wall_timeout\""), "{record}");
}
#[test]
fn cancel_marker_stops_a_live_provider() {
    let mut f = Fixture::new(
        "lane-cancel",
        "printf 'ready\\n'; : > cancel.request; exec /bin/sleep 10",
    );
    f.markers = "[\"cancel.request\"]".into();
    let record = f.run();
    assert!(record.contains("\"forced\":\"cancelled\""), "{record}");
}
#[test]
fn cancel_before_launch_never_starts_provider() {
    let mut f = Fixture::new("lane-pre-cancel", ": > should-not-exist");
    f.markers = "[\"cancel.request\"]".into();
    fs::write(f.sandbox.dir.join("cancel.request"), b"").unwrap();
    let output = invoke(&f.spec());
    assert_eq!(output.status.code(), Some(0));
    assert!(!f.sandbox.dir.join("should-not-exist").exists());
    assert!(!common::stdout(&output).contains("\"event\":\"started\""));
}
#[test]
fn sigterm_ignoring_provider_escalates_to_sigkill() {
    let mut f = Fixture::new(
        "lane-ignore",
        "trap '' TERM; printf 'ready\\n'; while :; do /bin/sleep 10; done",
    );
    f.wall = 80;
    let record = f.run();
    assert!(record.contains("\"signal\":9"), "{record}");
}
#[test]
fn fast_exit_before_process_watch_is_still_published() {
    let f = Fixture::new("lane-fast", "exit 0");
    let record = f.run();
    assert!(record.contains("\"exit\":0"), "{record}");
    assert!(record.contains("\"forced\":null"));
}
#[test]
fn capture_over_cap_retains_fixed_head_and_rolling_tail() {
    let mut f=Fixture::new("lane-cap","printf HEAD; i=0; while [ $i -lt 1000 ]; do printf abcdefgh; i=$((i+1)); done; printf TAIL");
    f.cap = 128;
    let record = f.run();
    let capture = fs::read(f.sandbox.dir.join("events.jsonl")).unwrap();
    assert_eq!(capture.len(), 128);
    assert!(capture.starts_with(b"HEAD"));
    assert!(capture.ends_with(b"TAIL"));
    assert!(
        record.contains("\"bytes\":8008,\"truncated\":true"),
        "{record}"
    );
}
#[test]
fn observer_death_leaves_deadlines_in_charge() {
    let mut f = Fixture::new(
        "lane-observer-dead",
        "printf 'ready\\n'; exec /bin/sleep 10",
    );
    f.wall = 100;
    f.observer = "[\"/bin/sh\",\"-c\",\"exit 1\"]".into();
    let record = f.run();
    assert!(record.contains("\"observer_lost\":true"), "{record}");
    assert!(record.contains("\"forced\":\"wall_timeout\""), "{record}");
}
#[test]
fn observer_terminal_closes_stdin_and_sets_terminal_without_forcing() {
    let mut f = Fixture::new("lane-terminal", "printf 'ready\\n'; cat >/dev/null; exit 0");
    f.observer = format!(
        "[\"/bin/sh\",\"-c\",{}]",
        quote("IFS= read -r line; printf '{\"terminal\":true}\\n'; cat >/dev/null")
    );
    let record = f.run();
    assert!(record.contains("\"terminal_event_seen\":true"), "{record}");
    assert!(record.contains("\"forced\":null"), "{record}");
}
#[test]
fn observer_stalled_request_is_rejected() {
    let mut f = Fixture::new("lane-stalled", "printf 'ready\\n'; exec /bin/sleep 10");
    f.wall = 100;
    f.observer = format!(
        "[\"/bin/sh\",\"-c\",{}]",
        quote("IFS= read -r line; printf '{\"stop\":\"stalled\"}\\n'; cat >/dev/null")
    );
    let record = f.run();
    assert!(record.contains("\"forced\":\"wall_timeout\""), "{record}");
    assert!(record.contains("\"observer_lost\":true"));
}
#[test]
fn silence_records_observation_without_stopping() {
    let mut f = Fixture::new("lane-silent", "printf 'ready\\n'; /bin/sleep 0.2; exit 0");
    f.silence = 40;
    f.writer = true;
    let record = f.run();
    assert!(record.contains("\"exit\":0"), "{record}");
    assert!(record.contains("\"forced\":null"));
    let watchdog = fs::read_to_string(f.sandbox.dir.join("watchdog.json")).unwrap();
    assert!(watchdog.contains("fabric.watchdog.v1"));
    assert!(watchdog.contains("\"process_alive\":true"));
}
// This test executable supplies a portable setsid helper to the /bin/sh fake.
#[test]
fn escaped_child_helper() {
    if std::env::var("S2_ESCAPE").as_deref() != Ok("1") {
        return;
    }
    assert!(unsafe { libc::setsid() } > 0);
    fs::write(
        std::path::Path::new(&std::env::var("S2_DIR").unwrap()).join("escaped.pid"),
        std::process::id().to_string(),
    )
    .unwrap();
    let status = Command::new("/bin/sh")
        .args(["-c", "trap '' TERM; exec /bin/sleep 10"])
        .status()
        .unwrap();
    std::process::exit(status.code().unwrap_or(1));
}
#[test]
fn new_session_grandchild_is_cleaned_up() {
    let mut f=Fixture::new("lane-escaped","S2_ESCAPE=1 \"$S2_HELPER\" --exact escaped_child_helper --nocapture & while [ ! -f escaped.pid ]; do :; done; printf ready; /bin/sleep 0.15; exit 0");
    f.wall = 1500;
    let record = f.run();
    let pid = fs::read_to_string(f.sandbox.dir.join("escaped.pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert!(record.contains(&format!("\"pid\":{pid}")), "{record}");
    let exists = unsafe { libc::kill(pid, 0) } == 0;
    #[cfg(target_os = "macos")]
    assert!(!exists, "escaped child {pid} survived: {record}");
    #[cfg(target_os = "linux")]
    if exists {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        assert!(stat.contains(") Z "), "{stat}");
    }
}

#[cfg(feature = "fault-injection")]
fn fault_run(site: &str, name: &str) -> (std::process::Output, Fixture) {
    let f = Fixture::new(name, "printf 'healthy\\n'; /bin/sleep 0.08; exit 0");
    let mut child = Command::new(BINARY)
        .args(["lane", "run"])
        .env("PROVENANT_LANE_FAULT", site)
        .env_remove("PROVENANT_FRONT_DOOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{}\n", f.spec()).as_bytes())
        .unwrap();
    (child.wait_with_output().unwrap(), f)
}
#[cfg(feature = "fault-injection")]
#[test]
fn full_disk_capture_and_progress_do_not_stop_a_healthy_provider() {
    for site in ["capture", "progress"] {
        let (output, f) = fault_run(site, &format!("lane-enospc-{site}"));
        assert_eq!(output.status.code(), Some(0), "{}", common::stderr(&output));
        let record = fs::read_to_string(f.sandbox.dir.join("supervisor.exit.json")).unwrap();
        assert!(record.contains("\"exit\":0"), "{record}");
        assert!(record.contains("\"forced\":null"));
        assert!(!record.contains("\"io_errors\":[]"));
    }
}
#[cfg(feature = "fault-injection")]
#[test]
fn full_disk_publication_releases_reserve_and_retries_once() {
    let (output, f) = fault_run("exit-once", "lane-reserve");
    assert_eq!(output.status.code(), Some(0), "{}", common::stderr(&output));
    assert!(f.sandbox.dir.join("supervisor.exit.json").exists());
    assert!(!f.sandbox.dir.join("supervisor.reserve").exists());
}
#[cfg(feature = "fault-injection")]
#[test]
fn persistent_full_disk_returns_record_on_stdout_with_75() {
    let (output, f) = fault_run("exit", "lane-publish-full");
    assert_eq!(
        output.status.code(),
        Some(75),
        "{}",
        common::stderr(&output)
    );
    assert!(!f.sandbox.dir.join("supervisor.exit.json").exists());
    assert!(!f.sandbox.dir.join("supervisor.reserve").exists());
    let stdout = common::stdout(&output);
    assert!(stdout.contains("provenant.lane.exit.v1"));
    assert!(stdout.contains("\"exit\":0"));
}

#[test]
fn observer_overflow_revokes_late_stop_requests() {
    let mut f=Fixture::new("lane-overflow","i=0; while [ $i -lt 8000 ]; do printf 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\\n'; i=$((i+1)); done; : > producer.done; /bin/sleep 0.1; exit 0");
    f.cap = 128;
    f.observer=format!("[\"/bin/sh\",\"-c\",{}]",quote("while [ ! -f producer.done ]; do :; done; printf '{\"stop\":\"usage_limited\"}\\n'; cat >/dev/null"));
    let record = f.run();
    assert!(record.contains("\"exit\":0"), "{record}");
    assert!(record.contains("\"forced\":null"), "{record}");
    assert!(record.contains("\"observer_lost\":true"));
    assert!(!record.contains("\"observer_dropped\":0"));
}
#[test]
fn inherited_non_cloexec_descriptors_do_not_reach_the_provider() {
    use std::os::fd::AsRawFd;
    let mut f = Fixture::new("lane-fds", "");
    let path = f.sandbox.dir.join("inherited.file");
    fs::write(&path, b"private").unwrap();
    let file = fs::File::open(&path).unwrap();
    let fd = file.as_raw_fd();
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, 0);
    }
    f.script = format!("if [ /dev/fd/{fd} -ef inherited.file ]; then printf leaked; fi; exit 0");
    f.run();
    assert!(fs::read(f.sandbox.dir.join("events.jsonl"))
        .unwrap()
        .is_empty());
}
#[test]
fn supervisor_signal_becomes_a_staged_stop() {
    use std::io::{BufRead, BufReader, Read};
    let f = Fixture::new("lane-signal", "printf ready; exec /bin/sleep 10");
    let mut child = Command::new(BINARY)
        .args(["lane", "run"])
        .env_remove("PROVENANT_FRONT_DOOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{}\n", f.spec()).as_bytes())
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert!(first.contains("\"event\":\"started\""));
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();
    assert!(child.wait().unwrap().success());
    let record = fs::read_to_string(f.sandbox.dir.join("supervisor.exit.json")).unwrap();
    assert!(
        record.contains("\"forced\":\"supervisor_signal\""),
        "{record}"
    );
}
#[test]
fn inherited_ignored_signals_and_mask_are_reset_before_provider_exec() {
    use std::os::unix::process::CommandExt;
    let f = Fixture::new("lane-inherited-signals", "kill -TERM $$; exit 88");
    let mut command = Command::new(BINARY);
    command
        .args(["lane", "run"])
        .env_remove("PROVENANT_FRONT_DOOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGCHLD, libc::SIG_IGN);
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGTERM);
            libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{}\n", f.spec()).as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", common::stderr(&output));
    let record = fs::read_to_string(f.sandbox.dir.join("supervisor.exit.json")).unwrap();
    assert!(record.contains("\"signal\":15"), "{record}");
}

#[cfg(feature = "fault-injection")]
#[test]
fn directory_sync_failure_after_link_does_not_report_a_lost_exit() {
    let (output, f) = fault_run("exit-sync", "lane-exit-sync");
    assert_eq!(output.status.code(), Some(0), "{}", common::stderr(&output));
    assert!(f.sandbox.dir.join("supervisor.exit.json").exists());
}
#[test]
fn descendant_cpu_activity_advances_progress_without_output() {
    let f=Fixture::new("lane-child-cpu","printf ready; (while :; do :; done) & work=$!; /bin/sleep 1.2; kill $work; wait $work; exit 0");
    let record = f.run();
    let value = |key: &str| {
        record
            .split(&format!("\"{key}\":\""))
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string()
    };
    assert!(value("last_progress_at") > value("started_at"), "{record}");
}
