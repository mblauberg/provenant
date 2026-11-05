//! Bounded command transport for installed cloud clients.
use provenant_os::exec::{ExecOptions, Executor, Termination, run_text};
use std::{ffi::OsString, path::PathBuf, time::Duration};

/// The only process boundary used by cloud adapters. Arguments must not contain credentials.
pub trait CloudCli {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<String, String>;
}

pub struct InstalledCloudCli<'a> {
    pub executor: &'a dyn Executor,
    pub cwd: PathBuf,
    pub timeout: Duration,
}

impl CloudCli for InstalledCloudCli<'_> {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
        let argv: Vec<OsString> = std::iter::once(program)
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let mut options = ExecOptions::new(&argv, &self.cwd, self.timeout);
        options.process_group = true;
        options.stdin = stdin.map(str::as_bytes);
        let output = run_text(self.executor, options).map_err(|e| e.to_string())?;
        if output.outcome.timed_out
            || output.outcome.cancelled
            || output.outcome.drain_truncated
            || output.outcome.stdout.truncated
            || output.outcome.stderr.truncated
            || output.outcome.stdout.read_failed
            || output.outcome.stderr.read_failed
        {
            return Err(format!("{program} returned incomplete output or timed out"));
        }
        if output.outcome.termination != Termination::Exit(0) {
            return Err(format!("{program} failed: {}", output.stderr.trim()));
        }
        Ok(output.stdout)
    }
}

/// Parse an explicitly labelled ID or session URL; never guess from arbitrary prose.
pub fn parse_identity(output: &str) -> Result<String, String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(output) {
        for key in ["id", "session_id", "task_id", "url"] {
            if let Some(id) = value
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            {
                return Ok(id.to_owned());
            }
        }
    }
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some((label, value)) = trimmed.split_once(':') {
            let label = label.trim().to_ascii_lowercase();
            if matches!(
                label.as_str(),
                "id" | "session id" | "session_id" | "task id" | "task_id" | "url" | "session url"
            ) && !value.trim().is_empty()
            {
                return Ok(value.trim().to_owned());
            }
        }
        if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
            return Ok(trimmed.to_owned());
        }
    }
    Err(format!(
        "could not find a cloud identity in output: {output}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cloud_identity_accepts_label_json_and_url() {
        assert_eq!(
            parse_identity("Starting\nSession ID: abc\n").unwrap(),
            "abc"
        );
        assert_eq!(parse_identity(r#"{"task_id":"123"}"#).unwrap(), "123");
        assert_eq!(
            parse_identity("https://example.invalid/session/123").unwrap(),
            "https://example.invalid/session/123"
        );
        assert!(
            parse_identity("unrecognised response")
                .unwrap_err()
                .contains("unrecognised response")
        );
    }
}

/// Claude's cloud command does not expose status or cancellation operations.
pub struct ClaudeCloud<C> {
    pub cli: C,
}

impl<C: CloudCli> crate::cloud::CloudProvider for ClaudeCloud<C> {
    type Error = String;
    fn check_environment(&mut self, _: crate::cloud::CloudSubmission<'_>) -> Result<(), String> {
        Ok(())
    }
    fn submit(&mut self, request: crate::cloud::CloudSubmission<'_>) -> Result<String, String> {
        let out = self.cli.run(
            "claude",
            &[
                "--cloud",
                request.task,
                "--environment",
                &request.job.environment,
            ],
            None,
        )?;
        parse_identity(&out)
    }
    fn observe(
        &mut self,
        _: &crate::cloud::RemoteJob,
    ) -> Result<crate::cloud::RemoteState, String> {
        Ok(crate::cloud::RemoteState::Running)
    }
    fn cancel(&mut self, _: &crate::cloud::RemoteJob) -> Result<bool, String> {
        Ok(false)
    }
}

/// Codex cloud transport. `branch` identifies the repository ref available in the environment.
pub struct CodexCloud<C> {
    pub cli: C,
    pub branch: String,
}

impl<C: CloudCli> CodexCloud<C> {
    /// Recover only an unambiguous list entry carrying the caller's correlation marker.
    /// A list without that marker cannot safely distinguish concurrent submissions.
    pub fn recover(&mut self, marker: &str) -> Result<Option<String>, String> {
        if marker.trim().is_empty() {
            return Err("empty cloud recovery marker".into());
        }
        let out = self.cli.run("codex", &["cloud", "list", "--json"], None)?;
        let value: serde_json::Value = serde_json::from_str(&out)
            .map_err(|_| format!("could not parse codex cloud list: {out}"))?;
        fn collect(value: &serde_json::Value, marker: &str, ids: &mut Vec<String>) {
            match value {
                serde_json::Value::Array(rows) => {
                    for row in rows {
                        collect(row, marker, ids);
                    }
                }
                serde_json::Value::Object(row) => {
                    if row.values().any(|value| value.as_str() == Some(marker)) {
                        if let Some(id) = ["id", "task_id", "session_id"]
                            .iter()
                            .find_map(|key| row.get(*key).and_then(|v| v.as_str()))
                        {
                            ids.push(id.to_owned());
                        }
                    } else {
                        for value in row.values() {
                            if value.is_array() || value.is_object() {
                                collect(value, marker, ids);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let mut ids = Vec::new();
        collect(&value, marker, &mut ids);
        match ids.as_slice() {
            [] => Ok(None),
            [id] if !id.trim().is_empty() => Ok(Some(id.clone())),
            _ => Err("codex cloud list has ambiguous recovery matches".into()),
        }
    }
}

impl<C: CloudCli> crate::cloud::CloudProvider for CodexCloud<C> {
    type Error = String;
    fn check_environment(&mut self, _: crate::cloud::CloudSubmission<'_>) -> Result<(), String> {
        Ok(())
    }
    fn submit(&mut self, request: crate::cloud::CloudSubmission<'_>) -> Result<String, String> {
        let out = self.cli.run(
            "codex",
            &[
                "cloud",
                "exec",
                "--env",
                &request.job.environment,
                "--branch",
                &self.branch,
            ],
            Some(request.task),
        )?;
        parse_identity(&out)
    }
    fn observe(
        &mut self,
        job: &crate::cloud::RemoteJob,
    ) -> Result<crate::cloud::RemoteState, String> {
        let out = self
            .cli
            .run("codex", &["cloud", "status", &job.remote_id], None)?;
        let lower = out.to_ascii_lowercase();
        if lower.contains("running") || lower.contains("pending") || lower.contains("queued") {
            Ok(crate::cloud::RemoteState::Running)
        } else {
            Err(format!("unrecognised codex cloud status: {out}"))
        }
    }
    fn cancel(&mut self, _: &crate::cloud::RemoteJob) -> Result<bool, String> {
        Ok(false)
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    use crate::cloud::{CloudProvider, CloudSubmission, RemoteJob, RemoteState};
    #[derive(Default)]
    struct FakeCli {
        calls: Vec<(String, Vec<String>, Option<String>)>,
        response: String,
    }
    impl CloudCli for FakeCli {
        fn run(
            &mut self,
            program: &str,
            args: &[&str],
            stdin: Option<&str>,
        ) -> Result<String, String> {
            self.calls.push((
                program.into(),
                args.iter().map(|s| (*s).into()).collect(),
                stdin.map(str::to_owned),
            ));
            Ok(self.response.clone())
        }
    }
    fn job() -> RemoteJob {
        RemoteJob {
            provider: "fake".into(),
            target: "target".into(),
            environment: "env".into(),
            repository: "repo".into(),
            remote_id: "remote".into(),
            commit: "a".repeat(40),
            requested_model: "model".into(),
            requested_effort: None,
            state: RemoteState::Running,
        }
    }
    #[test]
    fn cloud_adapters_submit_and_cancel_unconfirmed() {
        let job = job();
        let mut claude = ClaudeCloud {
            cli: FakeCli {
                response: "Session ID: session-1".into(),
                ..Default::default()
            },
        };
        assert_eq!(
            claude
                .submit(CloudSubmission {
                    job: &job,
                    task: "test task"
                })
                .unwrap(),
            "session-1"
        );
        assert_eq!(
            claude.cli.calls[0].1,
            ["--cloud", "test task", "--environment", "env"]
        );
        assert_eq!(claude.observe(&job).unwrap(), RemoteState::Running);
        assert!(!claude.cancel(&job).unwrap());
        let mut codex = CodexCloud {
            cli: FakeCli {
                response: "{\"id\":\"task-1\"}".into(),
                ..Default::default()
            },
            branch: "main".into(),
        };
        assert_eq!(
            codex
                .submit(CloudSubmission {
                    job: &job,
                    task: "test task"
                })
                .unwrap(),
            "task-1"
        );
        assert_eq!(
            codex.cli.calls[0].1,
            ["cloud", "exec", "--env", "env", "--branch", "main"]
        );
        assert_eq!(codex.cli.calls[0].2.as_deref(), Some("test task"));
        codex.cli.response = r#"[{"id":"task-1","marker":"unique"}]"#.into();
        assert_eq!(codex.recover("unique").unwrap().as_deref(), Some("task-1"));
        assert_eq!(codex.recover("different").unwrap(), None);
        codex.cli.response =
            r#"[{"id":"one","marker":"unique"},{"id":"two","marker":"unique"}]"#.into();
        assert!(codex.recover("unique").is_err());
        codex.cli.response = "status: running".into();
        assert_eq!(codex.observe(&job).unwrap(), RemoteState::Running);
        assert!(!codex.cancel(&job).unwrap());
        codex.cli.response = "unexpected".into();
        assert!(codex.observe(&job).unwrap_err().contains("unexpected"));
    }
}
