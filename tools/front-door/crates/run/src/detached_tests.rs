use super::*;
use crate::oracle::CommandOutput;
use std::collections::BTreeSet;
use std::fs::File;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};

#[derive(Default)]
struct Probe(BTreeSet<String>);
impl ProcessProbe for Probe {
    fn process_exists(&mut self, pid: &str) -> bool {
        self.0.contains(pid)
    }
}
fn alive(pid: &str) -> Probe {
    Probe([pid.to_owned()].into())
}
fn complete() -> Evidence {
    Evidence {
        wrapper: Some("999999\n".into()),
        worker: Some("888888\n".into()),
        done: Some("wrapper_pid=999999\nworker_pid=888888\nexit=7\n".into()),
        done_exists: true,
        done_size: Some(49),
        directory_age_seconds: Some(60.0),
    }
}

#[test]
fn complete_evidence_outputs_worker_wrapper_and_status() {
    let result = validate(&complete(), &mut Probe::default()).unwrap();
    assert_eq!(result.exit_code(), 0);
    assert_eq!(result.stdout(), "888888 999999 7\n");
}
#[test]
fn new_claim_has_bounded_startup_grace_with_no_sleeps() {
    for (age, expected) in [
        (Some(0.0), true),
        (Some(2.0), true),
        (Some(2.0001), false),
        (Some(-0.1), false),
        (None, false),
        (Some(f64::NAN), false),
    ] {
        let evidence = Evidence {
            directory_age_seconds: age,
            ..Evidence::default()
        };
        assert_eq!(validate(&evidence, &mut Probe::default()).is_ok(), expected);
    }
    let evidence = Evidence {
        done_exists: true,
        directory_age_seconds: Some(0.0),
        ..Evidence::default()
    };
    assert_eq!(
        validate(&evidence, &mut Probe::default()),
        Err(EvidenceError::MissingWrapper)
    );
}
#[test]
fn missing_worker_is_starting_only_while_wrapper_lives_and_marker_empty() {
    let evidence = Evidence {
        wrapper: Some("999999\n".into()),
        ..Evidence::default()
    };
    assert_eq!(
        validate(&evidence, &mut alive("999999")),
        Ok(Validation::Starting)
    );
    assert_eq!(
        validate(&evidence, &mut Probe::default()),
        Err(EvidenceError::MissingWorker)
    );
    assert_eq!(
        validate(
            &Evidence {
                done_size: Some(1),
                ..evidence
            },
            &mut alive("999999")
        ),
        Err(EvidenceError::MissingWorker)
    );
}
#[test]
fn wrapper_exit_without_marker_is_a_failure() {
    let evidence = Evidence {
        done: None,
        done_size: None,
        ..complete()
    };
    assert_eq!(
        validate(&evidence, &mut alive("999999")),
        Ok(Validation::Starting)
    );
    assert_eq!(
        validate(&evidence, &mut Probe::default()),
        Err(EvidenceError::MissingMarker)
    );
}
#[test]
fn valid_marker_waits_for_wrapper_but_refuses_live_worker() {
    assert_eq!(
        validate(&complete(), &mut alive("999999")),
        Ok(Validation::Starting)
    );
    assert_eq!(
        validate(&complete(), &mut alive("888888")),
        Err(EvidenceError::LiveWorker)
    );
}
#[test]
fn persisted_pids_require_single_value_and_decimal_digits() {
    assert_eq!(single_value("1\nignored"), Some("1".into()));
    assert_eq!(single_value("1\r\n"), Some("1".into()));
    for text in ["", "1", "\n", "1\n2\n"] {
        assert!(single_value(text).is_none());
    }
    let evidence = Evidence {
        wrapper: Some("bad\n".into()),
        ..complete()
    };
    assert_eq!(
        validate(&evidence, &mut Probe::default()),
        Err(EvidenceError::InvalidWrapper)
    );
    let evidence = Evidence {
        worker: Some("-2\n".into()),
        ..complete()
    };
    assert_eq!(
        validate(&evidence, &mut Probe::default()),
        Err(EvidenceError::InvalidWorker)
    );
}
#[test]
fn malformed_markers_fail_before_process_liveness() {
    for marker in [
        "wrapper_pid=999999\nworker_pid=888888\nexit=wat\n",
        "wrapper_pid=999999\nworker_pid=888888\nexit=-1\n",
        "wrapper_pid=999999\nworker_pid=888888\nexit=+7\n",
    ] {
        assert_eq!(
            validate(
                &Evidence {
                    done: Some(marker.into()),
                    ..complete()
                },
                &mut alive("999999")
            ),
            Err(EvidenceError::InvalidExit)
        );
    }
    for marker in [
        "wrapper_pid=999999\nworker_pid=888888\nexit=7",
        "wrapper_pid=999999\nworker_pid=888888\nexit=7\n\n",
        "wrapper_pid=999999\nworker_pid=888888\nexit=7\nextra=x\n",
        "wrapper_pid=000999999\nworker_pid=888888\nexit=7\n",
        "wrapper_pid=999999\nworker_pid=888889\nexit=7\n",
        "exit=7\nworker_pid=888888\nexit=7\n",
    ] {
        assert_eq!(
            validate(
                &Evidence {
                    done: Some(marker.into()),
                    ..complete()
                },
                &mut alive("999999")
            ),
            Err(EvidenceError::Malformed),
            "{marker:?}"
        );
    }
}
#[test]
fn marker_order_and_leading_zero_exit_are_preserved() {
    let evidence = Evidence {
        done: Some("exit=007\r\nworker_pid=888888\r\nwrapper_pid=999999\r\n".into()),
        ..complete()
    };
    assert_eq!(
        validate(&evidence, &mut Probe::default()).unwrap().stdout(),
        "888888 999999 007\n"
    );
}
#[test]
fn cli_errors_flags_and_separator_match_entry_point_policy() {
    let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(
        parse_args(&args(&["--run-dir"])).unwrap_err().to_string(),
        "--run-dir needs a value"
    );
    assert_eq!(
        parse_args(&args(&["--wat"])).unwrap_err().to_string(),
        "unknown argument: --wat"
    );
    assert!(matches!(
        parse_args(&[]).unwrap().check(),
        Err(Error::MissingDirectory)
    ));
    let parsed = parse_args(&args(&[
        "--run-dir",
        "one",
        "--validate",
        "--run-dir",
        "two",
        "--",
        "--worker-flag",
    ]))
    .unwrap();
    assert_eq!(parsed.run_dir, Some("two".into()));
    assert!(parsed.validate);
    assert_eq!(parsed.command, [OsString::from("--worker-flag")]);
    assert!(matches!(
        parse_args(&args(&["--run-dir", "r", "--"]))
            .unwrap()
            .check(),
        Err(Error::MissingCommand)
    ));
    let dir = tempfile::tempdir().unwrap();
    let parsed = Arguments {
        validate: true,
        run_dir: Some(dir.path().as_os_str().into()),
        command: vec!["worker".into()],
    };
    assert!(matches!(parsed.check(), Err(Error::ValidationCommand)));
}

// This fixture exercises wait/reap and descriptor effects. It deliberately does
// not certify setsid or wrapper-death behaviour: that belongs to the pending
// supervisor implementation and its process conformance gate.
struct FixtureHost;
struct FixtureWorker(Child);
impl SessionWorker for FixtureWorker {
    fn pid(&self) -> u32 {
        self.0.id()
    }
    fn wait_for(&mut self, timeout: std::time::Duration) -> io::Result<Option<WorkerExit>> {
        let deadline = std::time::Instant::now() + timeout;
        let status = loop {
            if let Some(status) = self.0.try_wait()? {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::yield_now();
        };
        match (status.code(), status.signal()) {
            (Some(code), _) => Ok(Some(WorkerExit::Exited(code))),
            (_, Some(signal)) => Ok(Some(WorkerExit::Signalled(signal))),
            _ => Err(io::Error::other("missing exit status")),
        }
    }
    fn terminate_and_wait(&mut self, timeout: std::time::Duration) -> io::Result<()> {
        // An unreaped owned child retains its PID; this fixture cannot target a
        // recycled process. Production signalling belongs to supervisor::sys.
        let signal = Command::new("/bin/kill")
            .args(["-TERM", &self.0.id().to_string()])
            .bounded_output()?;
        if self.wait_for(timeout)?.is_none() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if !signal.status.success() && !signal.stderr.ends_with(b"No such process\n") {
            return Err(io::Error::other("fixture SIGTERM failed"));
        }
        Ok(())
    }
}
impl Drop for FixtureWorker {
    fn drop(&mut self) {
        // Ensure a failed fixture assertion cannot leave an unreaped child.
        let _ = self.0.kill();
        let _ = self.wait_for(std::time::Duration::from_secs(1));
    }
}
impl SessionHost for FixtureHost {
    type Worker = FixtureWorker;
    fn writable(&mut self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|m| !m.permissions().readonly())
    }
    fn spawn(&mut self, command: &[OsString], transcript: &File) -> io::Result<FixtureWorker> {
        Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::null())
            .stdout(transcript.try_clone()?)
            .stderr(transcript.try_clone()?)
            .spawn()
            .map(FixtureWorker)
    }
    fn spawn_error_text(&self, error: &io::Error) -> String {
        let message = error.to_string();
        message
            .split(" (os error ")
            .next()
            .unwrap_or(&message)
            .into()
    }
}
fn shell(code: &str) -> Vec<OsString> {
    vec!["/bin/sh".into(), "-c".into(), code.into()]
}

#[test]
fn records_child_merged_transcript_and_exit_then_validates_reentry() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("run");
    assert_eq!(
        execute(
            &path,
            &shell("printf 'out\n'; printf 'err\n' >&2; exit 7"),
            999999,
            OsStr::new("helper"),
            &mut FixtureHost
        )
        .unwrap(),
        7
    );
    assert_eq!(
        fs::read_to_string(path.join("transcript.txt")).unwrap(),
        "out\nerr\n"
    );
    let worker = fs::read_to_string(path.join("worker.pid")).unwrap();
    assert_ne!(worker, "999999\n");
    assert_eq!(
        fs::read_to_string(path.join("done")).unwrap(),
        format!("wrapper_pid=999999\nworker_pid={worker}exit=7\n")
    );
    assert_eq!(
        validate_run(&path, SystemTime::now(), &mut Probe::default())
            .unwrap()
            .stdout(),
        format!("{} 999999 7\n", worker.trim())
    );
}
#[test]
fn same_directory_claim_launches_only_one_worker() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("run");
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = &path;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    execute(
                        path,
                        &shell("exit 7"),
                        999999,
                        OsStr::new("helper"),
                        &mut FixtureHost,
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| matches!(r, Ok(7))).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::Claim { .. })))
            .count(),
        1
    );
}
#[test]
fn worker_stdin_is_null() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("run");
    assert_eq!(
        execute(
            &path,
            &shell("cat"),
            999999,
            OsStr::new("helper"),
            &mut FixtureHost
        )
        .unwrap(),
        0
    );
    assert_eq!(fs::read(path.join("transcript.txt")).unwrap(), b"");
}
#[test]
fn signal_exit_is_128_plus_signal() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("run");
    assert_eq!(
        execute(
            &path,
            &shell("kill -TERM $$"),
            999999,
            OsStr::new("helper"),
            &mut FixtureHost
        )
        .unwrap(),
        143
    );
    assert!(
        fs::read_to_string(path.join("done"))
            .unwrap()
            .ends_with("exit=143\n")
    );
}
#[test]
fn invalid_parent_is_refused_without_a_worker() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("file");
    fs::write(&parent, "file").unwrap();
    assert!(matches!(
        execute(
            &parent.join("run"),
            &shell("exit 7"),
            999999,
            OsStr::new("helper"),
            &mut FixtureHost
        ),
        Err(Error::Parent { .. })
    ));
}
#[test]
fn failed_exec_diagnostic_and_fallback_worker_are_durable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("run");
    let missing = root.path().join("absent");
    assert_eq!(
        execute(
            &path,
            &[missing.as_os_str().into()],
            999999,
            OsStr::new("helper"),
            &mut FixtureHost
        )
        .unwrap(),
        127
    );
    assert_eq!(
        fs::read_to_string(path.join("transcript.txt")).unwrap(),
        format!(
            "helper: line 172: {}: No such file or directory\n",
            missing.display()
        )
    );
    assert!(
        fs::read_to_string(path.join("done"))
            .unwrap()
            .ends_with("exit=127\n")
    );
}
#[test]
fn non_executable_worker_uses_126() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("run");
    let blocked = root.path().join("blocked");
    fs::write(&blocked, "not executable").unwrap();
    assert_eq!(
        execute(
            &path,
            &[blocked.as_os_str().into()],
            999999,
            OsStr::new("helper"),
            &mut FixtureHost
        )
        .unwrap(),
        126
    );
}
#[test]
fn differential_python_validation_matrix() {
    let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../skills/orchestrate/scripts/run_worker_detached.py");
    let fixtures = [
        complete(),
        Evidence {
            wrapper: Some("١٢٣\n".into()),
            worker: Some("²\n".into()),
            done: Some("wrapper_pid=١٢٣\nworker_pid=²\nexit=7\n".into()),
            ..complete()
        },
        Evidence {
            done: Some("wrapper_pid=999999\nworker_pid=888888\nexit=٧\n".into()),
            ..complete()
        },
        Evidence::default(),
        Evidence {
            directory_age_seconds: Some(2.0),
            ..Evidence::default()
        },
        Evidence {
            worker: None,
            done_size: None,
            ..complete()
        },
        Evidence {
            done: None,
            ..complete()
        },
        Evidence {
            done: Some("exit=wat\n".into()),
            ..complete()
        },
        Evidence {
            done: Some("wrapper_pid=999999\nworker_pid=888888\nexit=7".into()),
            ..complete()
        },
        Evidence {
            wrapper: Some("n\n".into()),
            ..complete()
        },
    ];
    for (index, fixture) in fixtures.iter().enumerate() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("run");
        fs::create_dir(&dir).unwrap();
        for (name, text) in [
            ("wrapper.pid", &fixture.wrapper),
            ("worker.pid", &fixture.worker),
            ("done", &fixture.done),
        ] {
            if let Some(text) = text {
                fs::write(dir.join(name), text).unwrap();
            }
        }
        let age = fixture.directory_age_seconds.unwrap_or(60.0);
        let Some(mut python) = crate::oracle::python() else {
            return;
        };
        let result = python.args(["-c", r#"
import importlib.util,sys,os,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('helper',sys.argv[1]);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
m.process_exists=lambda pid:False
# Fix both wall clock and mtime; no assertion depends on elapsed time.
m.time.time=lambda:1000000000.0
os.utime(sys.argv[2],(1000000000.0-float(sys.argv[3]),)*2)
raise SystemExit(m.validate_run(Path(sys.argv[2])))
"#]).arg(&helper).arg(&dir).arg(age.to_string()).bounded_output();
        let result = match result {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                eprintln!("SKIP: python3 absent");
                return;
            }
            other => other.unwrap(),
        };
        let actual = validate(fixture, &mut Probe::default());
        let (code, stdout, stderr) = match actual {
            Ok(v) => (v.exit_code(), v.stdout(), String::new()),
            Err(e) => (2, String::new(), format!("{e}\n")),
        };
        assert_eq!(
            (result.status.code(), result.stdout, result.stderr),
            (Some(code), stdout.into_bytes(), stderr.into_bytes()),
            "fixture {index}"
        );
    }
}

#[test]
fn published_files_use_default_creation_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let baseline = root.path().join("baseline");
    fs::write(&baseline, "").unwrap();
    let expected = fs::metadata(baseline).unwrap().permissions().mode() & 0o777;
    let run = root.path().join("run");
    execute(
        &run,
        &shell("exit 0"),
        999999,
        OsStr::new("helper"),
        &mut FixtureHost,
    )
    .unwrap();
    for name in ["wrapper.pid", "worker.pid", "transcript.txt", "done"] {
        assert_eq!(
            fs::metadata(run.join(name)).unwrap().permissions().mode() & 0o777,
            expected,
            "{name}"
        );
    }
}

struct ObstructingHost {
    run: PathBuf,
    name: &'static str,
    ready: Option<PathBuf>,
}
impl SessionHost for ObstructingHost {
    type Worker = FixtureWorker;
    fn writable(&mut self, path: &Path) -> bool {
        FixtureHost.writable(path)
    }
    fn spawn(&mut self, command: &[OsString], transcript: &File) -> io::Result<Self::Worker> {
        fs::create_dir(self.run.join(self.name))?;
        let worker = FixtureHost.spawn(command, transcript)?;
        if let Some(ready) = &self.ready {
            // A FIFO read blocks until the worker has installed its SIGTERM trap.
            if fs::read(ready)? != b"ready" {
                return Err(io::Error::other("fixture handshake failed"));
            }
        }
        Ok(worker)
    }
    fn spawn_error_text(&self, error: &io::Error) -> String {
        FixtureHost.spawn_error_text(error)
    }
}

#[test]
fn pid_publication_failure_is_explicit_and_reaps_launched_worker() {
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("run");
    let mut host = ObstructingHost {
        run: run.clone(),
        name: "worker.pid",
        ready: None,
    };
    let error = execute(
        &run,
        &shell("exit 0"),
        999999,
        OsStr::new("helper"),
        &mut host,
    )
    .unwrap_err();
    assert!(matches!(error, Error::WorkerPid { .. }));
    assert_eq!(error.exit_code(), 1);
    assert!(!run.join("done").exists());
    assert!(!run.join("worker.pid.tmp.999999").exists());
}

#[test]
fn completion_publication_failure_is_explicit_after_worker_exit() {
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("run");
    let mut host = ObstructingHost {
        run: run.clone(),
        name: "done",
        ready: None,
    };
    let error = execute(
        &run,
        &shell("exit 7"),
        999999,
        OsStr::new("helper"),
        &mut host,
    )
    .unwrap_err();
    assert!(matches!(error, Error::Completion(_)));
    assert_eq!(error.exit_code(), 1);
    assert!(run.join("worker.pid").is_file());
    assert!(!run.join("done.tmp.999999").exists());
}

#[test]
fn pid_publication_failure_sends_sigterm_to_owned_worker() {
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("run");
    let ready = root.path().join("ready.fifo");
    let marker = root.path().join("terminated");
    assert!(
        Command::new("mkfifo")
            .arg(&ready)
            .status()
            .unwrap()
            .success()
    );
    let mut command = shell(
        "trap 'printf term > \"$1\"; exit 0' TERM; printf ready > \"$2\"; while :; do :; done",
    );
    command.extend([
        OsString::from("worker"),
        marker.as_os_str().into(),
        ready.as_os_str().into(),
    ]);
    let mut host = ObstructingHost {
        run: run.clone(),
        name: "worker.pid",
        ready: Some(ready),
    };
    let error = execute(&run, &command, 999999, OsStr::new("helper"), &mut host).unwrap_err();
    assert!(matches!(error, Error::WorkerPid { cleanup: None, .. }));
    assert_eq!(fs::read_to_string(marker).unwrap(), "term");
    assert!(!run.join("done").exists());
}

#[test]
fn undecodable_evidence_retains_typed_encoding_cause() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("wrapper.pid"), [255, 10]).unwrap();
    assert!(matches!(
        validate_run(root.path(), SystemTime::now(), &mut Probe::default()),
        Err(Error::Encoding { .. })
    ));
}

#[test]
fn unicode_digit_table_matches_python_for_every_scalar() {
    let Some(mut python) = crate::oracle::python() else {
        return;
    };
    let output = python
        .args([
            "-c",
            "import json; print(json.dumps([i for i in range(0x110000) if chr(i).isdigit()]))",
        ])
        .bounded_output()
        .unwrap();
    assert!(output.status.success());
    let expected: Vec<u32> = serde_json::from_slice(&output.stdout).unwrap();
    let actual: Vec<_> = (0..=0x10ffff)
        .filter(|&i| char::from_u32(i).is_some_and(|c| digits(&c.to_string())))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn later_undecodable_files_do_not_mask_earlier_pid_refusals() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("wrapper.pid"), "bad\n").unwrap();
    fs::write(root.path().join("worker.pid"), [255, 10]).unwrap();
    fs::write(root.path().join("done"), [255]).unwrap();
    assert!(matches!(
        validate_run(root.path(), SystemTime::now(), &mut Probe::default()),
        Err(Error::Evidence(EvidenceError::InvalidWrapper))
    ));
    fs::write(root.path().join("wrapper.pid"), "999999\n").unwrap();
    fs::remove_file(root.path().join("worker.pid")).unwrap();
    assert!(matches!(
        validate_run(root.path(), SystemTime::now(), &mut Probe::default()),
        Err(Error::Evidence(EvidenceError::MissingWorker))
    ));
}

#[test]
fn startup_grace_uses_python_float_epoch_subtraction() {
    use std::time::{Duration, UNIX_EPOCH};
    let root = tempfile::tempdir().unwrap();
    let modified = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    File::open(root.path())
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let now = modified + Duration::new(2, 1);
    assert!(matches!(
        validate_run(root.path(), now, &mut Probe::default()),
        Ok(Validation::Starting)
    ));
}
