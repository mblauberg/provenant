//! Bounded command transport for installed cloud clients.
use provenant_os::exec::{ExecOptions, Executor, Termination, run_text};
use std::{ffi::OsString, path::PathBuf, time::Duration};

/// The only process boundary used by cloud adapters. Arguments must not contain credentials.
pub trait CloudCli {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<String, String>;
    /// Run with a pseudo-terminal and no stdin, for CLIs that refuse a plain pipe.
    fn run_tty(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
        self.run(program, args, None)
    }
}

const FAILURE_LINES: usize = 3;
const FAILURE_BYTES: usize = 600;

/// The first lines of a CLI failure, without terminal controls, bounded.
pub fn failure_excerpt(text: &str) -> String {
    let clean = strip_ansi(text).replace('\r', "");
    let mut out = clean
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(FAILURE_LINES)
        .collect::<Vec<_>>()
        .join(" / ");
    if out.len() > FAILURE_BYTES {
        let mut end = FAILURE_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    out
}

/// Wrap `program args` so it runs under a pseudo-terminal via `script(1)`.
fn pty_argv(program: &str, args: &[&str]) -> Vec<String> {
    if cfg!(target_os = "macos") {
        // BSD script: `script -q /dev/null command args...`.
        let mut argv = vec![
            "script".to_owned(),
            "-q".into(),
            "/dev/null".into(),
            program.into(),
        ];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        argv
    } else {
        // util-linux script runs one shell command line.
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let line = std::iter::once(program)
            .chain(args.iter().copied())
            .map(quote)
            .collect::<Vec<_>>()
            .join(" ");
        vec!["script".into(), "-qec".into(), line, "/dev/null".into()]
    }
}

pub struct InstalledCloudCli<'a> {
    pub executor: &'a dyn Executor,
    pub cwd: PathBuf,
    pub timeout: Duration,
}

impl CloudCli for InstalledCloudCli<'_> {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
        self.exec(
            program,
            std::iter::once(program)
                .chain(args.iter().copied())
                .map(OsString::from)
                .collect(),
            stdin,
        )
    }
    fn run_tty(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
        let argv = pty_argv(program, args)
            .into_iter()
            .map(OsString::from)
            .collect();
        self.exec(program, argv, None)
    }
}

impl InstalledCloudCli<'_> {
    fn exec(
        &self,
        program: &str,
        argv: Vec<OsString>,
        stdin: Option<&str>,
    ) -> Result<String, String> {
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
            let text = if output.stderr.trim().is_empty() {
                &output.stdout
            } else {
                &output.stderr
            };
            return Err(format!("{program} failed: {}", failure_excerpt(text)));
        }
        Ok(output.stdout)
    }
}

/// Remove CSI and OSC terminal escape sequences, keeping the visible text.
fn strip_ansi(text: &str) -> String {
    // `script` echoes a closed stdin's EOF as `^D` plus two backspaces.
    let text = text.replace("^D\u{8}\u{8}", "");
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        chars.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// The session ID in Claude's `Resume with: claude --teleport <id>` line.
fn teleport_id(line: &str) -> Option<&str> {
    let (label, command) = line.split_once(':')?;
    if !label.trim().eq_ignore_ascii_case("resume with") {
        return None;
    }
    let mut words = command.split_whitespace();
    words.find(|word| *word == "--teleport")?;
    words.next().filter(|id| !id.starts_with('-'))
}

/// Parse an explicitly labelled ID or session URL; never guess from arbitrary prose.
pub fn parse_identity(output: &str) -> Result<String, String> {
    let output = strip_ansi(output).replace('\r', "");
    let output = output.as_str();
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
    // The resume line is the stable ID; the View URL is only a fallback.
    if let Some(id) = output.lines().find_map(|line| teleport_id(line.trim())) {
        return Ok(id.to_owned());
    }
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some((label, value)) = trimmed.split_once(':') {
            let label = label.trim().to_ascii_lowercase();
            if matches!(
                label.as_str(),
                "id" | "session id"
                    | "session_id"
                    | "task id"
                    | "task_id"
                    | "url"
                    | "session url"
                    | "view"
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
        let claude = "Created cloud session: Measurement task\nView: https://claude.ai/code/session_01EXAMPLEEXAMPLEEXAMPLE?from=cli&m=0\nResume with: claude --teleport session_01EXAMPLEEXAMPLEEXAMPLE\n";
        assert_eq!(
            parse_identity(claude).unwrap(),
            "session_01EXAMPLEEXAMPLEEXAMPLE"
        );
        let coloured = "\u{1b}[2K\u{1b}[1mCreated cloud session: Measurement task\u{1b}[0m\n\u{1b}]8;;https://claude.ai/x\u{7}View\u{1b}]8;;\u{7}: https://claude.ai/code/session_01EXAMPLEEXAMPLEEXAMPLE?from=cli&m=0\n\u{1b}[32mResume with:\u{1b}[0m claude --teleport \u{1b}[1msession_01EXAMPLEEXAMPLEEXAMPLE\u{1b}[0m\r\n";
        assert_eq!(
            parse_identity(coloured).unwrap(),
            "session_01EXAMPLEEXAMPLEEXAMPLE"
        );
        assert!(
            parse_identity("unrecognised response")
                .unwrap_err()
                .contains("unrecognised response")
        );
    }
}

fn result_prompt(request: crate::cloud::CloudSubmission<'_>, claude: bool) -> String {
    let job = request.job;
    let channel = if claude {
        "Commit only the result file as a single commit on top of the tested commit and push your claude working branch to origin."
    } else {
        "Include the result file as a new file in your cloud diff."
    };
    let model = if claude {
        format!(
            " Requested model: {:?}; requested effort: {:?}.",
            job.requested_model, job.requested_effort
        )
    } else {
        String::new()
    };
    format!(
        "{}\n\nUse repository source {:?} and branch {:?}, and test commit {}. Record your result in .provenant-cloud-result.json as JSON with job_id {:?}, nonce {:?}, commit (the full tested commit), exit_status (integer), tests_run and tests_failed (arrays of strings), log_location (string), tracked_files_changed (boolean, excluding the result file), and observed_model (string or null). Do not include credentials in the result. {channel}{model}",
        request.task, job.repository, job.branch, job.commit, job.job_id, job.nonce
    )
}

/// Claude's cloud command does not expose status or cancellation operations.
pub struct ClaudeCloud<C> {
    pub cli: C,
    pub pending_cleanup: Option<(String, String)>,
}

impl<C: CloudCli> crate::cloud::CloudProvider for ClaudeCloud<C> {
    type Error = String;
    fn check_environment(&mut self, _: crate::cloud::CloudSubmission<'_>) -> Result<(), String> {
        Ok(())
    }
    fn submit(&mut self, request: crate::cloud::CloudSubmission<'_>) -> Result<String, String> {
        let prompt = result_prompt(request, true);
        // Without an environment the CLI uses the hosted Claude cloud.
        let environment = request.job.environment.trim();
        let mut args = vec!["--cloud", prompt.as_str()];
        if !environment.is_empty() {
            args.extend(["--environment", environment]);
        }
        // `claude --cloud` refuses a non-terminal, so only this call needs a PTY.
        let out = self
            .cli
            .run_tty("claude", &args)
            .map_err(|error| format!("claude --cloud refused: {error}"))?;
        parse_identity(&out).map_err(|_| {
            format!(
                "claude --cloud returned no session identity: {}",
                failure_excerpt(&out)
            )
        })
    }
    fn failure_detail(&self, error: &String) -> Option<String> {
        Some(failure_excerpt(error))
    }
    fn observe(
        &mut self,
        job: &crate::cloud::RemoteJob,
    ) -> Result<crate::cloud::RemoteState, String> {
        self.pending_cleanup = None;
        let refs = self
            .cli
            .run("git", &["ls-remote", "origin", "refs/heads/claude/*"], None)?;
        for line in refs.lines() {
            let Some((oid, reference)) = line.split_once('\t') else {
                return Err(format!("unrecognised git ls-remote output: {line}"));
            };
            if !reference.starts_with("refs/heads/claude/")
                || !matches!(oid.len(), 40 | 64)
                || !oid.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(format!("unrecognised git ls-remote output: {line}"));
            }
            self.cli.run(
                "git",
                &[
                    "-c",
                    "fetch.fsckObjects=true",
                    "fetch",
                    "--no-tags",
                    "origin",
                    reference,
                ],
                None,
            )?;
            let parents = self
                .cli
                .run("git", &["rev-list", "--parents", "-n", "1", oid], None)?;
            let parts: Vec<_> = parents.split_whitespace().collect();
            if parts.as_slice() != [oid, job.commit.as_str()] {
                continue;
            }
            let diff = self.cli.run(
                "git",
                &["diff", "--no-ext-diff", "--no-renames", &job.commit, oid],
                None,
            )?;
            let Ok(result) = crate::cloud_result::result_from_diff(&diff) else {
                continue;
            };
            if result
                .clone()
                .check(&job.job_id, &job.nonce, &job.commit, &job.requested_model)
                .is_err()
            {
                continue;
            }
            self.pending_cleanup = Some((reference.to_owned(), oid.to_owned()));
            return Ok(crate::cloud::RemoteState::Finished { result });
        }
        let now = provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new())
            .map_err(|error| error.to_string())?
            .unix_seconds();
        let now = u64::try_from(now).map_err(|error| error.to_string())?;
        if now >= job.deadline_unix_seconds {
            return Ok(crate::cloud::RemoteState::Unknown {
                detail: "Cloud result did not arrive before the deadline".into(),
            });
        }
        Ok(crate::cloud::RemoteState::Running)
    }
    #[allow(clippy::print_stderr)] // A failed best-effort remote cleanup must be visible.
    fn after_save(&mut self, _: &crate::cloud::RemoteJob) {
        if let Some((reference, oid)) = self.pending_cleanup.take() {
            let lease = format!("--force-with-lease={reference}:{oid}");
            if let Err(error) = self.cli.run(
                "git",
                &["push", &lease, "origin", "--delete", &reference],
                None,
            ) {
                eprintln!("warning: could not delete cloud result branch {reference}: {error}");
            }
        }
    }
    fn cancel(&mut self, _: &crate::cloud::RemoteJob) -> Result<bool, String> {
        Ok(false)
    }
}

/// Codex cloud transport.
pub struct CodexCloud<C> {
    pub cli: C,
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
                    if row
                        .values()
                        .any(|value| value.as_str().is_some_and(|text| text.contains(marker)))
                    {
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
    fn check_environment(
        &mut self,
        request: crate::cloud::CloudSubmission<'_>,
    ) -> Result<(), String> {
        if request.job.environment.trim().is_empty() {
            return Err("codex cloud targets require an environment".into());
        }
        Ok(())
    }
    fn submit(&mut self, request: crate::cloud::CloudSubmission<'_>) -> Result<String, String> {
        let prompt = result_prompt(request, false);
        let out = self.cli.run(
            "codex",
            &[
                "cloud",
                "exec",
                "--env",
                request.job.environment.trim(),
                "--branch",
                &request.job.branch,
            ],
            Some(&prompt),
        )?;
        parse_identity(&out)
    }
    fn recover(&mut self, job: &crate::cloud::RemoteJob) -> Result<Option<String>, String> {
        CodexCloud::recover(self, &job.nonce)
    }
    fn observe(
        &mut self,
        job: &crate::cloud::RemoteJob,
    ) -> Result<crate::cloud::RemoteState, String> {
        let out = self
            .cli
            .run("codex", &["cloud", "status", &job.remote_id], None)?;
        let lower = out.to_ascii_lowercase();
        if [
            "environment setup failed",
            "setup_failed",
            "environment_missing",
            "environment not found",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            Ok(crate::cloud::RemoteState::SetupFailed { detail: out })
        } else if lower.contains("running") || lower.contains("pending") || lower.contains("queued")
        {
            Ok(crate::cloud::RemoteState::Running)
        } else if ["completed", "finished", "succeeded", "failed"]
            .iter()
            .any(|state| lower.contains(state))
        {
            let diff = self
                .cli
                .run("codex", &["cloud", "diff", &job.remote_id], None)?;
            let mut result = crate::cloud_result::result_from_diff(&diff)?;
            // Codex cannot honour a requested model; do not infer one from the request.
            result.observed_model = None;
            result
                .clone()
                .check(&job.job_id, &job.nonce, &job.commit, &job.requested_model)
                .map_err(|e| e.to_string())?;
            Ok(crate::cloud::RemoteState::Finished { result })
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
            job_id: "job".into(),
            nonce: "nonce".into(),
            deadline_unix_seconds: u64::MAX,
            provider: "fake".into(),
            target: "target".into(),
            environment: "env".into(),
            repository: "repo".into(),
            branch: "main".into(),
            remote_id: "remote".into(),
            commit: "a".repeat(40),
            requested_model: "model".into(),
            requested_effort: None,
            state: RemoteState::Running,
            submit_error: None,
        }
    }
    #[test]
    fn cloud_claude_reads_matching_result_and_deletes_after_save() {
        use std::collections::VecDeque;
        struct Script {
            replies: VecDeque<String>,
            calls: Vec<String>,
        }
        impl CloudCli for Script {
            fn run(
                &mut self,
                program: &str,
                args: &[&str],
                _: Option<&str>,
            ) -> Result<String, String> {
                self.calls.push(format!("{program} {}", args.join(" ")));
                self.replies
                    .pop_front()
                    .ok_or_else(|| "missing response".into())
            }
        }
        let oid = "b".repeat(40);
        let commit = "a".repeat(40);
        let diff = format!(
            "diff --git a/.provenant-cloud-result.json b/.provenant-cloud-result.json\nnew file mode 100644\n--- /dev/null\n+++ b/.provenant-cloud-result.json\n@@ -0,0 +1 @@\n+{{\"job_id\":\"job\",\"nonce\":\"nonce\",\"commit\":\"{commit}\",\"exit_status\":0,\"tests_run\":[],\"tests_failed\":[],\"log_location\":\"log\",\"tracked_files_changed\":false,\"observed_model\":null}}\n"
        );
        let script = Script {
            replies: [
                format!("{oid}\trefs/heads/claude/result\n"),
                String::new(),
                format!("{oid} {commit}"),
                diff,
                String::new(),
            ]
            .into(),
            calls: Vec::new(),
        };
        let mut adapter = ClaudeCloud {
            cli: script,
            pending_cleanup: None,
        };
        let job = job();
        assert!(matches!(
            adapter.observe(&job).unwrap(),
            RemoteState::Finished { .. }
        ));
        assert_eq!(adapter.cli.calls.len(), 4);
        assert!(adapter.cli.calls[1].contains("fetch.fsckObjects=true"));
        assert!(adapter.cli.calls[2].contains(&oid));
        assert!(adapter.cli.calls[3].contains(&oid));
        adapter.after_save(&job);
        assert!(adapter.cli.calls[4].contains("--delete"));
    }
    #[test]
    fn cloud_codex_recovers_lost_submit_response_without_retry() {
        struct LostCli {
            execs: usize,
        }
        impl CloudCli for LostCli {
            fn run(&mut self, _: &str, args: &[&str], _: Option<&str>) -> Result<String, String> {
                match args {
                    ["cloud", "exec", ..] => {
                        self.execs += 1;
                        Err("response lost".into())
                    }
                    ["cloud", "list", "--json"] => {
                        Ok(r#"[{"id":"recovered","prompt":"nonce"}]"#.into())
                    }
                    ["cloud", "status", "recovered"] => Ok("running".into()),
                    _ => Err(format!("unexpected command {args:?}")),
                }
            }
        }
        #[derive(Default)]
        struct Store(Option<RemoteJob>);
        impl crate::cloud::CloudJobStore for Store {
            type Error = ();
            fn begin(&mut self, job: &RemoteJob) -> Result<(), ()> {
                self.0 = Some(job.clone());
                Ok(())
            }
            fn save(&mut self, job: &RemoteJob) -> Result<(), ()> {
                self.0 = Some(job.clone());
                Ok(())
            }
        }
        let mut adapter = CodexCloud {
            cli: LostCli { execs: 0 },
        };
        let mut store = Store::default();
        assert!(matches!(
            crate::cloud::submit(&mut adapter, &mut store, job(), "test"),
            Err(crate::cloud::CloudJobError::SubmitUnknown(_))
        ));
        let mut job = store.0.clone().unwrap();
        crate::cloud::reconcile(&mut adapter, &mut store, &mut job).unwrap();
        assert_eq!(job.remote_id, "recovered");
        assert_eq!(store.0.unwrap().remote_id, "recovered");
        assert_eq!(adapter.cli.execs, 1);
    }

    #[test]
    fn cloud_codex_finished_result_is_checked_and_reports_changed_files() {
        use std::collections::VecDeque;
        struct Script(VecDeque<String>);
        impl CloudCli for Script {
            fn run(&mut self, _: &str, _: &[&str], _: Option<&str>) -> Result<String, String> {
                self.0.pop_front().ok_or_else(|| "missing response".into())
            }
        }
        let job = job();
        let diff = format!(
            "diff --git a/.provenant-cloud-result.json b/.provenant-cloud-result.json\nnew file mode 100644\n--- /dev/null\n+++ b/.provenant-cloud-result.json\n@@ -0,0 +1 @@\n+{{\"job_id\":\"job\",\"nonce\":\"nonce\",\"commit\":\"{}\",\"exit_status\":0,\"tests_run\":[\"test\"],\"tests_failed\":[],\"log_location\":\"log\",\"tracked_files_changed\":true,\"observed_model\":\"claimed\"}}\n",
            job.commit
        );
        let mut adapter = CodexCloud {
            cli: Script(["completed".into(), diff.clone()].into()),
        };
        let RemoteState::Finished { result } = adapter.observe(&job).unwrap() else {
            panic!("expected result")
        };
        let checked = result
            .check(&job.job_id, &job.nonce, &job.commit, &job.requested_model)
            .unwrap();
        assert!(!checked.passed);
        assert_eq!(checked.model, crate::cloud::ModelObservation::Unknown);
        let mut wrong = job;
        wrong.nonce = "other".into();
        let mut adapter = CodexCloud {
            cli: Script(["completed".into(), diff].into()),
        };
        assert!(adapter.observe(&wrong).is_err());
    }
    const CAPTURED: &str = "\u{1b}[>4m\u{1b}]0;claude\u{7}\u{1b}[?25lCreated cloud session: Fix the thing\r\nView: https://claude.ai/code/session_01ABC?from=cli&m=0\r\nResume with: claude --teleport session_01ABC\r\n\u{1b}[?25h";
    #[test]
    fn cloud_claude_parses_pty_capture() {
        assert_eq!(parse_identity(CAPTURED).unwrap(), "session_01ABC");
        let view_only = "\u{1b}[>4mView: https://claude.ai/code/session_01ABC?from=cli&m=0\r\n";
        assert_eq!(
            parse_identity(view_only).unwrap(),
            "https://claude.ai/code/session_01ABC?from=cli&m=0"
        );
        let job = job();
        let mut claude = ClaudeCloud {
            pending_cleanup: None,
            cli: FakeCli {
                response: CAPTURED.into(),
                ..Default::default()
            },
        };
        let request = CloudSubmission {
            job: &job,
            task: "t",
        };
        assert_eq!(claude.submit(request).unwrap(), "session_01ABC");
        assert!(claude.cli.calls[0].2.is_none());
    }
    #[test]
    fn cloud_claude_non_tty_refusal_quotes_first_error_line() {
        struct Refuses;
        impl CloudCli for Refuses {
            fn run(&mut self, _: &str, _: &[&str], _: Option<&str>) -> Result<String, String> {
                Err("claude failed: Error: --cloud requires an interactive terminal.\nNon-interactive invocations run locally".into())
            }
        }
        let job = job();
        let mut claude = ClaudeCloud {
            pending_cleanup: None,
            cli: Refuses,
        };
        let error = claude
            .submit(CloudSubmission {
                job: &job,
                task: "t",
            })
            .unwrap_err();
        assert!(error.contains("refused"));
        assert!(error.contains("--cloud requires an interactive terminal."));
        let detail = claude.failure_detail(&error).unwrap();
        assert!(detail.contains("--cloud requires an interactive terminal."));
        assert!(failure_excerpt(&"x\n".repeat(1000)).len() < 700);
    }
    #[test]
    fn cloud_adapters_submit_and_cancel_unconfirmed() {
        let job = job();
        let mut claude = ClaudeCloud {
            pending_cleanup: None,
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
        assert_eq!(claude.cli.calls[0].1[0], "--cloud");
        assert!(claude.cli.calls[0].1[1].contains("test task"));
        assert!(claude.cli.calls[0].1[1].contains(&job.nonce));
        assert_eq!(&claude.cli.calls[0].1[2..], ["--environment", "env"]);
        let mut hosted = job.clone();
        hosted.environment.clear();
        claude
            .submit(CloudSubmission {
                job: &hosted,
                task: "test task",
            })
            .unwrap();
        assert_eq!(claude.cli.calls[1].1.len(), 2);
        assert!(!claude.cli.calls[1].1.iter().any(|a| a == "--environment"));
        let mut codex_without = CodexCloud {
            cli: FakeCli::default(),
        };
        assert!(
            codex_without
                .check_environment(CloudSubmission {
                    job: &hosted,
                    task: "test task"
                })
                .unwrap_err()
                .contains("require an environment")
        );
        claude.cli.response.clear();
        assert_eq!(claude.observe(&job).unwrap(), RemoteState::Running);
        let mut expired = job.clone();
        expired.deadline_unix_seconds = 0;
        assert!(matches!(
            claude.observe(&expired).unwrap(),
            RemoteState::Unknown { .. }
        ));
        assert!(!claude.cancel(&job).unwrap());
        let mut codex = CodexCloud {
            cli: FakeCli {
                response: "{\"id\":\"task-1\"}".into(),
                ..Default::default()
            },
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
        assert!(
            codex.cli.calls[0]
                .2
                .as_deref()
                .unwrap()
                .contains("test task")
        );
        assert!(
            codex.cli.calls[0]
                .2
                .as_deref()
                .unwrap()
                .contains(&job.nonce)
        );
        codex.cli.response = r#"[{"id":"task-1","marker":"unique"}]"#.into();
        assert_eq!(codex.recover("unique").unwrap().as_deref(), Some("task-1"));
        assert_eq!(codex.recover("different").unwrap(), None);
        codex.cli.response =
            r#"[{"id":"one","marker":"unique"},{"id":"two","marker":"unique"}]"#.into();
        assert!(codex.recover("unique").is_err());
        codex.cli.response = "status: running".into();
        assert_eq!(codex.observe(&job).unwrap(), RemoteState::Running);
        assert!(!codex.cancel(&job).unwrap());
        codex.cli.response = "environment setup failed".into();
        let mut diagnosed = job.clone();
        diagnosed.state = codex.observe(&job).unwrap();
        let diagnostic = diagnosed.environment_diagnostic().unwrap();
        assert_eq!(diagnostic.target, "target");
        assert_eq!(diagnostic.environment, "env");
        assert!(diagnostic.remedy.contains("repair"));
        codex.cli.response = "unexpected".into();
        assert!(codex.observe(&job).unwrap_err().contains("unexpected"));
    }
}
