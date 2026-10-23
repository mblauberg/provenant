//! Typed command boundary. Argv parsing remains with the CLI owner; no command
//! is switched to this inert library.
use super::*;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Collect {
        sessions_root: PathBuf,
        repository: PathBuf,
        json: bool,
    },
    TerminalReport {
        pid: i64,
        classification: String,
        exit_status: Option<i64>,
    },
}
#[derive(Debug, Error)]
pub enum MainError {
    #[error(transparent)]
    Policy(#[from] LivenessError),
    #[error("cannot inspect processes: {0}")]
    Census(#[source] crate::process_info::ProcessError),
    #[error(transparent)]
    Observation(#[from] super::observe::ObserveError),
}
/// Inject the complete observation boundary so the CLI domain rules have no
/// process-global test seams or process-control authority.
pub trait LivenessSource {
    fn live(&self) -> Result<Vec<Process>, MainError>;
    fn workers(&self, sessions_root: &Path, repository: &Path) -> Result<Vec<Worker>, MainError>;
}
/// One explicit-clock collection adapter; no filesystem inspection of live
/// workers is performed (their worktree label is always `writer-live`).
pub struct ObservedSource<'a> {
    pub filesystem: &'a dyn ObservationFs,
    pub commands: &'a dyn CommandRunner,
    pub census: &'a dyn ProcessCensus,
    pub now_seconds: f64,
}
impl LivenessSource for ObservedSource<'_> {
    fn live(&self) -> Result<Vec<Process>, MainError> {
        Ok(live_processes(&self.census.processes()?))
    }
    fn workers(&self, sessions_root: &Path, repository: &Path) -> Result<Vec<Worker>, MainError> {
        self.collect_observed(sessions_root, Some(repository), None)
    }
}
impl ObservedSource<'_> {
    pub fn collect_observed(
        &self,
        sessions_root: &Path,
        repository: Option<&Path>,
        previous_sizes: Option<&BTreeMap<i64, i64>>,
    ) -> Result<Vec<Worker>, MainError> {
        let paths = session_logs(self.filesystem, sessions_root)?;
        let scope_id = match repository {
            Some(path) => Some(
                repository_id(self.filesystem, self.commands, path)?
                    .ok_or_else(|| LivenessError::Repository(path.display().to_string()))?,
            ),
            None => None,
        };
        let mut located = Vec::new();
        for process in self.live()? {
            let cwd = worker_cwd(self.filesystem, self.commands, &process)?;
            if scope_id.is_none()
                || repository_id(self.filesystem, self.commands, Path::new(&cwd))? == scope_id
            {
                located.push(LocatedProcess {
                    process,
                    cwd,
                    repository_id: None,
                });
            }
        }
        let mut counts = BTreeMap::new();
        for row in &located {
            *counts.entry(&row.cwd).or_insert(0_usize) += 1;
        }
        let mut workers = Vec::new();
        for row in &located {
            // Python validates elapsed before observing log metadata.
            parse_elapsed(&row.process.elapsed)?;
            let path = paths.get(&row.cwd).map(PathBuf::as_path);
            let (modified, age_seconds) = log_metadata(self.filesystem, path, self.now_seconds)?;
            let size = if counts.get(&row.cwd).copied().unwrap_or(0) > 1 {
                None
            } else {
                output_size(self.filesystem, path)?
            };
            let logs = path
                .map(|path| {
                    BTreeMap::from([(
                        row.cwd.clone(),
                        SessionLog {
                            path: path.display().to_string(),
                            modified,
                            age_seconds,
                            output_size: size,
                        },
                    )])
                })
                .unwrap_or_default();
            workers.extend(collect(
                std::slice::from_ref(row),
                &logs,
                None,
                previous_sizes,
            )?);
        }
        Ok(workers)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
}

pub fn execute_request(
    source: &dyn LivenessSource,
    request: &Request,
) -> Result<MainOutput, MainError> {
    let result = match request {
        Request::TerminalReport {
            pid,
            classification,
            exit_status,
        } => source.live().and_then(|processes| {
            terminal_report(
                classification,
                *pid,
                processes.iter().any(|p| p.pid == *pid),
                exit_status.map(|exit_status| WorkerExit {
                    pid: *pid,
                    exit_status,
                }),
            )
            .map(|report| terminal_json(&report))
            .map_err(Into::into)
        }),
        Request::Collect {
            sessions_root,
            repository,
            json,
        } => source.workers(sessions_root, repository).map(|rows| {
            if *json {
                workers_json(&rows)
            } else {
                render(&rows)
            }
        }),
    };
    match result {
        Ok(mut stdout) => {
            stdout.push('\n');
            Ok(MainOutput {
                stdout,
                stderr: String::new(),
                exit_code: 0,
            })
        }
        Err(
            error @ (MainError::Policy(
                LivenessError::Repository(_)
                | LivenessError::Classification(_)
                | LivenessError::LiveWriter { .. }
                | LivenessError::MissingExit,
            )
            | MainError::Census(_)
            | MainError::Observation(ObserveError::ProcessCensus(_))),
        ) => Ok(MainOutput {
            stdout: String::new(),
            stderr: format!("worker-liveness: {error}\n"),
            exit_code: 2,
        }),
        Err(error) => Err(error),
    }
}

pub fn terminal_json(report: &TerminalReport) -> String {
    format!(
        "{{\"classification\": {}, \"exit_status\": {}, \"observed_exit\": {}, \"pid\": {}}}",
        crate::pending::json_ascii(&report.classification),
        report.exit_status,
        report.observed_exit,
        report.pid
    )
}

/// Dataclass declaration order is the retained worker JSON field order.
pub fn workers_json(workers: &[Worker]) -> String {
    if workers.is_empty() {
        return "[]".into();
    }
    let rows: Vec<_> = workers
        .iter()
        .map(|worker| {
            let text = crate::pending::json_ascii;
            let float = crate::pending::json_float;
            let fields = [
                ("pid", worker.pid.to_string()),
                ("elapsed", text(&worker.elapsed)),
                ("elapsed_seconds", float(worker.elapsed_seconds)),
                ("cpu", text(&worker.cpu)),
                ("cpu_seconds", float(worker.cpu_seconds)),
                ("cwd", text(&worker.cwd)),
                ("session_log", text(&worker.session_log)),
                ("session_log_mtime", text(&worker.session_log_mtime)),
                (
                    "session_log_age_seconds",
                    worker.session_log_age_seconds.map_or("null".into(), float),
                ),
                (
                    "output_size",
                    worker.output_size.map_or("null".into(), |v| v.to_string()),
                ),
                ("worktree", text(&worker.worktree)),
                ("stalled", worker.stalled.to_string()),
            ];
            format!(
                "  {{\n{}\n  }}",
                fields
                    .into_iter()
                    .map(|(key, value)| format!("    \"{key}\": {value}"))
                    .collect::<Vec<_>>()
                    .join(",\n")
            )
        })
        .collect();
    format!("[\n{}\n]", rows.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Source {
        live: bool,
    }
    impl LivenessSource for Source {
        fn live(&self) -> Result<Vec<Process>, MainError> {
            Ok(if self.live {
                vec![Process {
                    pid: 83,
                    ppid: 1,
                    elapsed: "1".into(),
                    cpu: "0".into(),
                    command: "codex exec".into(),
                }]
            } else {
                vec![]
            })
        }
        fn workers(&self, _: &Path, repository: &Path) -> Result<Vec<Worker>, MainError> {
            if repository == Path::new("/missing") {
                return Err(LivenessError::Repository("/missing".into()).into());
            }
            Ok(vec![])
        }
    }
    #[test]
    fn main_fences_live_workers_requires_exit_and_returns_source_output() {
        for (live, exit_status, code) in [(true, Some(0), 2), (false, None, 2), (false, Some(7), 0)]
        {
            let out = execute_request(
                &Source { live },
                &Request::TerminalReport {
                    pid: 83,
                    classification: "complete".into(),
                    exit_status,
                },
            )
            .unwrap();
            assert_eq!(out.exit_code, code);
            if code == 0 {
                assert_eq!(out.stdout, "{\"classification\": \"complete\", \"exit_status\": 7, \"observed_exit\": true, \"pid\": 83}\n");
            } else {
                assert!(out.stderr.starts_with("worker-liveness: "));
                assert!(out.stdout.is_empty());
            }
        }
        let out = execute_request(
            &Source { live: false },
            &Request::Collect {
                sessions_root: "/sessions".into(),
                repository: "/missing".into(),
                json: true,
            },
        )
        .unwrap();
        assert_eq!(
            out.stderr,
            "worker-liveness: not a git repository: /missing\n"
        );
        let out = execute_request(
            &Source { live: false },
            &Request::Collect {
                sessions_root: "/sessions".into(),
                repository: "/repo".into(),
                json: true,
            },
        )
        .unwrap();
        assert_eq!(out.stdout, "[]\n");
    }
    #[test]
    fn worker_json_bytes_match_python_dataclass_order_unicode_and_float_policy() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(
            &root,
            r#"
import importlib.util, sys, json
from dataclasses import asdict
spec=importlib.util.spec_from_file_location('worker_liveness', 'skills/orchestrate/scripts/worker_liveness.py')
m=importlib.util.module_from_spec(spec); sys.modules[spec.name]=m; spec.loader.exec_module(m)
rows=[m.Worker(9007199254740993,'λ\n',1e16,'0',-0.0,'💩é\x7f','-', '-', None,None,'writer-live',False),m.Worker(2,'2',float('inf'),'0',float('nan'),'r','-', '-', 1e-5,0,'writer-live',True)]
print(json.dumps(json.dumps([asdict(row) for row in rows],indent=2)))
"#,
        ) else {
            return;
        };
        let rows = [
            Worker {
                pid: 9007199254740993,
                elapsed: "λ\n".into(),
                elapsed_seconds: 1e16,
                cpu: "0".into(),
                cpu_seconds: -0.0,
                cwd: "💩é\u{7f}".into(),
                session_log: "-".into(),
                session_log_mtime: "-".into(),
                session_log_age_seconds: None,
                output_size: None,
                worktree: "writer-live".into(),
                stalled: false,
            },
            Worker {
                pid: 2,
                elapsed: "2".into(),
                elapsed_seconds: f64::INFINITY,
                cpu: "0".into(),
                cpu_seconds: f64::NAN,
                cwd: "r".into(),
                session_log: "-".into(),
                session_log_mtime: "-".into(),
                session_log_age_seconds: Some(1e-5),
                output_size: Some(0),
                worktree: "writer-live".into(),
                stalled: true,
            },
        ];
        assert_eq!(workers_json(&rows), expected.as_str().unwrap());
    }
}

#[cfg(test)]
mod collection_tests {
    use super::*;
    use std::ffi::OsString;
    struct Census(Vec<Process>);
    impl ProcessCensus for Census {
        fn processes(&self) -> Result<Vec<Process>, ObserveError> {
            Ok(self.0.clone())
        }
    }
    struct Commands;
    impl CommandRunner for Commands {
        fn run(&self, args: &[OsString]) -> Result<CommandOutput, ObserveError> {
            assert!(
                !args.iter().any(|arg| arg == "status"),
                "must not inspect a live writer"
            );
            Ok(CommandOutput {
                returncode: 0,
                stdout: if args[0] == "lsof" {
                    "n/launch\n"
                } else {
                    "/repo/.git\n"
                }
                .into(),
            })
        }
    }
    #[test]
    fn observed_collection_applies_cwd_ownership_and_shared_log_ambiguity() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let log = temp.path().join("rollout-one.jsonl");
        std::fs::write(&log, serde_json::json!({"payload":{"cwd":cwd}}).to_string()).unwrap();
        let size = std::fs::metadata(&log).unwrap().len() as i64;
        let row = |pid| Process {
            pid,
            ppid: 1,
            elapsed: "1:00".into(),
            cpu: "0".into(),
            command: format!(
                "codex exec --cd {} task",
                crate::process_info::shell_join([cwd.to_str().unwrap()])
            ),
        };
        let census = Census(vec![row(2_000_000_000)]);
        let source = ObservedSource {
            filesystem: &SystemObservationFs,
            commands: &Commands,
            census: &census,
            now_seconds: 2_000_000_000.0,
        };
        let previous = BTreeMap::from([(2_000_000_000, size)]);
        let workers = source
            .collect_observed(temp.path(), Some(&cwd), Some(&previous))
            .unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].cwd, cwd.to_str().unwrap());
        assert_eq!(workers[0].output_size, Some(size));
        assert!(workers[0].stalled);
        assert_eq!(workers[0].worktree, "writer-live");
        let census = Census(vec![row(2_000_000_000), row(2_000_000_001)]);
        let source = ObservedSource {
            census: &census,
            ..source
        };
        let workers = source
            .collect_observed(temp.path(), Some(&cwd), Some(&previous))
            .unwrap();
        assert_eq!(workers.len(), 2);
        assert!(workers
            .iter()
            .all(|worker| worker.output_size.is_none() && !worker.stalled));
    }
}
