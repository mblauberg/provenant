use super::observe::*;
use super::{LivenessError, Process};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

#[derive(Default)]
struct FixtureFs {
    linux: bool,
    exists: BTreeSet<PathBuf>,
    resolved: BTreeMap<PathBuf, PathBuf>,
    home: Option<PathBuf>,
    directories: BTreeSet<PathBuf>,
    paths: Vec<PathBuf>,
    lines: BTreeMap<PathBuf, Vec<u8>>,
    metadata: BTreeMap<PathBuf, ObservedMetadata>,
}

impl ObservationFs for FixtureFs {
    fn current_dir(&self) -> io::Result<PathBuf> {
        Ok(PathBuf::from("/fixture"))
    }

    fn is_linux(&self) -> bool {
        self.linux
    }

    fn exists(&self, path: &Path) -> bool {
        self.exists.contains(path)
    }

    fn resolve_non_strict(&self, path: &Path) -> io::Result<PathBuf> {
        self.resolved
            .get(path)
            .cloned()
            .or_else(|| path.is_absolute().then(|| path.to_owned()))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unmapped path"))
    }

    fn lookup_home(&self, _user: Option<&OsStr>) -> io::Result<Option<PathBuf>> {
        Ok(self.home.clone())
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.directories.contains(path)
    }

    fn walk_session_logs(&self, _root: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(self.paths.clone())
    }

    fn read_first_line(&self, path: &Path, _limit: u64) -> io::Result<Vec<u8>> {
        self.lines
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing fixture line"))
    }

    fn metadata(&self, path: &Path) -> io::Result<ObservedMetadata> {
        self.metadata
            .get(path)
            .copied()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing fixture metadata"))
    }
}

#[derive(Default)]
struct FixtureRunner {
    calls: Mutex<Vec<Vec<OsString>>>,
    outputs: Mutex<VecDeque<CommandOutput>>,
}

impl FixtureRunner {
    fn with_outputs(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            outputs: Mutex::new(outputs.into_iter().collect()),
        }
    }

    fn calls(&self) -> Vec<Vec<OsString>> {
        self.calls.lock().expect("fixture calls lock").clone()
    }
}

impl CommandRunner for FixtureRunner {
    fn run(&self, args: &[OsString]) -> Result<CommandOutput, ObserveError> {
        self.calls
            .lock()
            .expect("fixture calls lock")
            .push(args.to_vec());
        self.outputs
            .lock()
            .expect("fixture output lock")
            .pop_front()
            .ok_or_else(|| ObserveError::TimedOut {
                program: "missing fixture result".into(),
            })
    }
}

fn command_output(returncode: i32, stdout: &str) -> CommandOutput {
    CommandOutput {
        returncode,
        stdout: stdout.into(),
    }
}

#[test]
fn process_cwd_uses_procfs_on_linux_and_lsof_elsewhere() {
    let proc_path = PathBuf::from("/proc/48/cwd");
    let mut linux_fs = FixtureFs {
        linux: true,
        ..FixtureFs::default()
    };
    linux_fs.exists.insert(proc_path.clone());
    linux_fs
        .resolved
        .insert(proc_path, PathBuf::from("/repo/worktree"));
    let unused_runner = FixtureRunner::default();
    assert_eq!(
        process_cwd(&linux_fs, &unused_runner, 48).unwrap(),
        "/repo/worktree"
    );
    assert!(unused_runner.calls().is_empty());

    let runner =
        FixtureRunner::with_outputs([command_output(0, "p48\nn/repo/.worktrees/impl-421\n")]);
    let cwd = process_cwd(&FixtureFs::default(), &runner, 48).unwrap();
    assert_eq!(cwd, "/repo/.worktrees/impl-421");
    assert_eq!(
        runner.calls(),
        [vec![
            OsString::from("lsof"),
            OsString::from("-a"),
            OsString::from("-p"),
            OsString::from("48"),
            OsString::from("-d"),
            OsString::from("cwd"),
            OsString::from("-Fn"),
        ]]
    );
}

#[test]
fn canonical_and_worker_cwd_expand_home_and_resolve_missing_tails() {
    let mut fs = FixtureFs {
        home: Some(PathBuf::from("/Users/example")),
        ..FixtureFs::default()
    };
    fs.resolved.insert(
        PathBuf::from("/Users/example/launch/../repo"),
        PathBuf::from("/Users/example/repo"),
    );
    assert_eq!(
        canonical_cwd(&fs, "~/repo/missing").unwrap(),
        "/Users/example/repo/missing"
    );
    assert_eq!(canonical_cwd(&fs, "?").unwrap(), "?");
    let empty_home = FixtureFs {
        home: Some(PathBuf::new()),
        resolved: BTreeMap::from([(PathBuf::from("/x"), PathBuf::from("/x"))]),
        ..FixtureFs::default()
    };
    assert_eq!(canonical_cwd(&empty_home, "~/x").unwrap(), "/x");
    let runner = FixtureRunner::with_outputs([command_output(0, "n/Users/example/launch\n")]);
    let process = Process {
        pid: 48,
        ppid: 1,
        elapsed: "00:10".into(),
        cpu: "0:00.01".into(),
        command: "codex exec --cd ../repo task".into(),
    };
    assert_eq!(
        worker_cwd(&fs, &runner, &process).unwrap(),
        "/Users/example/repo"
    );
}

#[test]
fn worktree_fence_runs_before_git_and_git_status_matches_source_arguments() {
    let runner = FixtureRunner::with_outputs([command_output(0, "")]);
    assert!(matches!(
        worktree_state(&runner, "/repo/.worktrees/lane", Some(84), Some(&BTreeSet::from([84]))),
        Err(ObserveError::Liveness(LivenessError::LiveWriter {
            pid: 84,
            action,
        })) if action == "inspection"
    ));
    assert!(runner.calls().is_empty());
    assert!(matches!(
        worktree_state(&runner, "/repo/.worktrees/lane", Some(84), None),
        Err(ObserveError::Liveness(LivenessError::MissingPidSet))
    ));
    assert!(runner.calls().is_empty());

    assert_eq!(
        worktree_state(&runner, "/repo/.worktrees/lane", None, None).unwrap(),
        "clean"
    );
    assert_eq!(
        runner.calls(),
        [vec![
            OsString::from("git"),
            OsString::from("--no-optional-locks"),
            OsString::from("-C"),
            OsString::from("/repo/.worktrees/lane"),
            OsString::from("status"),
            OsString::from("--porcelain=v1"),
            OsString::from("--untracked-files=all"),
        ]]
    );
}

#[test]
fn repository_identity_uses_common_git_directory_and_python_strip() {
    let runner = FixtureRunner::with_outputs([
        command_output(0, "\u{1c}/repo/.git\u{1f}\n"),
        command_output(1, "fatal: not a repository\n"),
    ]);
    assert_eq!(
        repository_id(&FixtureFs::default(), &runner, Path::new("/repo"))
            .unwrap()
            .as_deref(),
        Some("/repo/.git")
    );
    assert_eq!(
        repository_id(&FixtureFs::default(), &runner, Path::new("/other")).unwrap(),
        None
    );
    assert_eq!(
        runner.calls(),
        vec![
            vec![
                OsString::from("git"),
                OsString::from("--no-optional-locks"),
                OsString::from("-C"),
                OsString::from("/repo"),
                OsString::from("rev-parse"),
                OsString::from("--path-format=absolute"),
                OsString::from("--git-common-dir"),
            ],
            vec![
                OsString::from("git"),
                OsString::from("--no-optional-locks"),
                OsString::from("-C"),
                OsString::from("/other"),
                OsString::from("rev-parse"),
                OsString::from("--path-format=absolute"),
                OsString::from("--git-common-dir"),
            ],
        ]
    );
}

#[test]
fn session_logs_choose_latest_canonical_cwd_and_follow_file_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let cwd = root.path().join("repo");
    std::fs::create_dir(&cwd).unwrap();
    let older = root.path().join("rollout-old.jsonl");
    let latest = nested.join("rollout-latest.jsonl");
    let alias_target = nested.join("session-source.jsonl");
    let alias = nested.join("rollout-alias.jsonl");
    let cr_log = root.path().join("rollout-carriage-return.jsonl");
    std::fs::write(
        &older,
        format!("{{\"payload\":{{\"cwd\":\"{}/\"}}}}\n{{}}\n", cwd.display()),
    )
    .unwrap();
    std::fs::write(
        &latest,
        format!("{{\"payload\":{{\"cwd\":\"{}\"}}}}\n", cwd.display()),
    )
    .unwrap();
    let alias_cwd = root.path().join("alias-repo");
    std::fs::create_dir(&alias_cwd).unwrap();
    std::fs::write(
        &alias_target,
        format!("{{\"payload\":{{\"cwd\":\"{}\"}}}}\n", alias_cwd.display()),
    )
    .unwrap();
    let cr_cwd = root.path().join("cr-repo");
    std::fs::create_dir(&cr_cwd).unwrap();
    std::fs::write(
        &cr_log,
        format!(
            "{{\"payload\":{{\"cwd\":\"{}\"}}}}\r{{\"payload\":{{\"cwd\":\"/ignored\"}}}}\n",
            cr_cwd.display()
        ),
    )
    .unwrap();
    set_mtime(&older, 1_700_000_000);
    set_mtime(&latest, 1_700_000_001);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&alias_target, &alias).unwrap();

    let logs = session_logs(&SystemObservationFs, root.path()).unwrap();
    let canonical_cwd = std::fs::canonicalize(cwd).unwrap();
    assert_eq!(
        logs.get(&canonical_cwd.to_string_lossy().to_string()),
        Some(&latest)
    );
    let canonical_alias_cwd = std::fs::canonicalize(alias_cwd).unwrap();
    assert_eq!(
        logs.get(&canonical_alias_cwd.to_string_lossy().to_string()),
        Some(&alias)
    );
    let canonical_cr_cwd = std::fs::canonicalize(cr_cwd).unwrap();
    assert_eq!(
        logs.get(&canonical_cr_cwd.to_string_lossy().to_string()),
        Some(&cr_log)
    );
}

#[test]
fn log_metadata_uses_shared_cpython_timestamp_and_explicit_now() {
    let path = PathBuf::from("/log");
    let fs = FixtureFs {
        metadata: BTreeMap::from([(
            path.clone(),
            ObservedMetadata {
                modified_seconds: 1_700_000_000.123456,
                size: 37,
            },
        )]),
        ..FixtureFs::default()
    };
    assert_eq!(
        log_metadata(&fs, Some(&path), 1_700_000_100.0).unwrap(),
        (
            "2023-11-14T22:13:20.123456+00:00".into(),
            Some(99.87654399871826)
        )
    );
    assert_eq!(log_metadata(&fs, None, 0.0).unwrap(), ("-".into(), None));
    assert_eq!(output_size(&fs, Some(&path)).unwrap(), Some(37));
    assert_eq!(output_size(&fs, None).unwrap(), None);
}

#[test]
fn session_log_number_range_is_a_typed_refusal_and_invalid_json_is_skipped() {
    let root = PathBuf::from("/sessions");
    let bad = PathBuf::from("/sessions/rollout-bad.jsonl");
    let huge = PathBuf::from("/sessions/rollout-huge.jsonl");
    let fs = FixtureFs {
        directories: BTreeSet::from([root.clone()]),
        paths: vec![bad.clone(), huge.clone()],
        lines: BTreeMap::from([
            (bad, b"not json\n".to_vec()),
            (
                huge.clone(),
                b"{\"payload\":{\"cwd\":\"/repo\"},\"unused\":18446744073709551616}\n".to_vec(),
            ),
        ]),
        metadata: BTreeMap::from([(
            huge.clone(),
            ObservedMetadata {
                modified_seconds: 1.0,
                size: 10,
            },
        )]),
        ..FixtureFs::default()
    };
    assert!(matches!(
        session_logs(&fs, &root),
        Err(ObserveError::NumberOutOfRange { path, value })
            if path == huge && value == "18446744073709551616"
    ));
}

#[test]
fn python_differential_covers_canonical_paths_and_recursive_session_logs() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("repo");
    std::fs::create_dir(&cwd).unwrap();
    let sessions = root.path().join("sessions");
    let nested = sessions.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let log = nested.join("rollout-1.jsonl");
    std::fs::write(
        &log,
        format!("{{\"payload\":{{\"cwd\":\"{}/\"}}}}\n", cwd.display()),
    )
    .unwrap();
    // Equal mtimes pin glob traversal order. A root-level match must win over
    // a nested one even when the nested directory was created first.
    let direct = sessions.join("rollout-direct.jsonl");
    std::fs::copy(&log, &direct).unwrap();
    let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for path in [&log, &direct] {
        std::fs::File::open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(
        outside.join("rollout-excluded.jsonl"),
        "{\"payload\":{\"cwd\":\"/excluded-directory-link\"}}\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, sessions.join("directory-link")).unwrap();
    let path = serde_json::to_string(cwd.to_str().unwrap()).unwrap();
    let sessions_arg = serde_json::to_string(sessions.to_str().unwrap()).unwrap();
    let script = format!(
        r#"
import importlib.util, json, sys
from pathlib import Path
source = Path('skills/orchestrate/scripts/worker_liveness.py')
spec = importlib.util.spec_from_file_location('worker_liveness', source)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
cwd = {path}
logs = module.session_logs(Path({sessions_arg}))
print(json.dumps({{'canonical': module.canonical_cwd(cwd), 'logs': {{key: str(value) for key, value in logs.items()}}}}))
"#
    );
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let Some(expected) = crate::test_support::python(&repo_root, &script) else {
        return;
    };
    let logs = session_logs(&SystemObservationFs, &sessions).unwrap();
    let actual = serde_json::json!({
        "canonical": canonical_cwd(&SystemObservationFs, cwd.to_str().unwrap()).unwrap(),
        "logs": logs
            .into_iter()
            .map(|(key, path)| (key, path.to_string_lossy().into_owned()))
            .collect::<BTreeMap<_, _>>(),
    });
    assert_eq!(actual, expected);
}

#[test]
fn python_differential_pins_lsof_and_worktree_command_policy() {
    let script = r#"
import importlib.util, json, subprocess, sys
from pathlib import Path
source = Path('skills/orchestrate/scripts/worker_liveness.py')
spec = importlib.util.spec_from_file_location('worker_liveness', source)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
calls = []
def run(command, **kwargs):
    calls.append(command)
    if command[0] == 'lsof':
        return subprocess.CompletedProcess(command, 0, 'p48\nn/repo/lane\n', '')
    return subprocess.CompletedProcess(command, 0, '', '')
module.sys.platform = 'darwin'
module.subprocess.run = run
cwd = module.process_cwd(48)
state = module.worktree_state('/repo/lane')
print(json.dumps([cwd, state, calls]))
"#;
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let Some(expected) = crate::test_support::python(&repo_root, script) else {
        return;
    };
    let runner = FixtureRunner::with_outputs([
        command_output(0, "p48\nn/repo/lane\n"),
        command_output(0, ""),
    ]);
    let fs = FixtureFs::default();
    let cwd = process_cwd(&fs, &runner, 48).unwrap();
    let state = worktree_state(&runner, "/repo/lane", None, None).unwrap();
    let calls = runner
        .calls()
        .into_iter()
        .map(|args| {
            args.into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(serde_json::json!([cwd, state, calls]), expected);
}

fn set_mtime(path: &Path, seconds: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    let modified = UNIX_EPOCH + Duration::from_secs(seconds);
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
}

#[test]
fn session_log_rotation_keeps_the_first_selected_path_when_metadata_is_gone() {
    let root = PathBuf::from("/sessions");
    let path = root.join("rollout-rotated.jsonl");
    let fs = FixtureFs {
        directories: BTreeSet::from([root.clone()]),
        paths: vec![path.clone()],
        lines: BTreeMap::from([(path.clone(), br#"{"payload":{"cwd":"/repo"}}"#.to_vec())]),
        ..FixtureFs::default()
    };
    let logs = session_logs(&fs, &root).unwrap();
    assert_eq!(logs.get("/repo"), Some(&path));
    assert_eq!(
        log_metadata(&fs, logs.get("/repo").map(PathBuf::as_path), 0.0).unwrap(),
        ("-".into(), None)
    );
}

#[test]
fn diagnostic_capture_refuses_truncated_stdout_instead_of_using_it_as_a_path() {
    let runner = SystemCommandRunner {
        timeout: Duration::from_secs(5),
        output_limit_bytes: 3,
    };
    let result = runner.run(&["/bin/sh".into(), "-c".into(), "printf 1234".into()]);
    assert!(matches!(
        result,
        Err(ObserveError::CommandOutputLimit { limit: 3, .. })
    ));
}
