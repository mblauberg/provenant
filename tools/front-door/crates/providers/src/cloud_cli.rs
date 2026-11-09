//! Bounded command transport for installed cloud clients.
use provenant_config::{CodexCloudTransport, ProviderCloudSettings};
use provenant_os::exec::{ExecOptions, Executor, Termination, run_text};
use std::{ffi::OsString, path::PathBuf, time::Duration};

#[path = "cloud_thread.rs"]
mod thread;
#[cfg(test)]
#[path = "cloud_thread_tests.rs"]
mod thread_tests;

/// The only process boundary used by cloud adapters. Arguments must not contain credentials.
const REFUSAL: &str = "\u{1e}cloud-refusal:";

pub trait CloudCli {
    fn run(&mut self, program: &str, args: &[&str], stdin: Option<&str>) -> Result<String, String>;
    /// Submit errors are uncertain unless the transport proves a refusal.
    fn submit(
        &mut self,
        program: &str,
        args: &[&str],
        stdin: Option<&str>,
        tty: bool,
    ) -> Result<String, String> {
        if tty {
            self.run_tty(program, args)
        } else {
            self.run(program, args, stdin)
        }
    }
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
    fn submit(
        &mut self,
        program: &str,
        args: &[&str],
        stdin: Option<&str>,
        tty: bool,
    ) -> Result<String, String> {
        let argv = if tty {
            pty_argv(program, args)
                .into_iter()
                .map(OsString::from)
                .collect()
        } else {
            std::iter::once(program)
                .chain(args.iter().copied())
                .map(OsString::from)
                .collect()
        };
        self.exec_inner(program, argv, stdin, true)
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
        self.exec_inner(program, argv, stdin, false)
    }
    fn exec_inner(
        &self,
        program: &str,
        argv: Vec<OsString>,
        stdin: Option<&str>,
        submission: bool,
    ) -> Result<String, String> {
        let mut options = ExecOptions::new(&argv, &self.cwd, self.timeout);
        options.process_group = true;
        options.stdin = stdin.map(str::as_bytes);
        let output = run_text(self.executor, options).map_err(|e| e.to_string())?;
        if output.outcome.timed_out
            || output.outcome.cancelled
            || output.outcome.drain_truncated
            || output.outcome.overflow_stopped
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
            let detail = failure_excerpt(text);
            let refusal = submission
                && matches!(output.outcome.termination, Termination::Exit(code) if (1..128).contains(&code))
                && !detail.is_empty()
                && ![&output.stdout, &output.stderr].iter().any(|text| {
                    // The transport is unknown here, so any transport's identity
                    // proves the remote may hold the submission.
                    parse_identity(text).is_ok() || thread::submission_id(text).is_ok()
                });
            return Err(format!(
                "{}{program} failed: {detail}",
                if refusal { REFUSAL } else { "" }
            ));
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

enum ResultChannel {
    ClaudeBranch,
    CodexDiff,
    CodexThread,
}

/// The commit-message line that carries a Claude job's result. Claude has no
/// status command, so this pushed marker is the job's completion signal.
pub const RESULT_TRAILER: &str = "Provenant-Cloud-Result:";

fn result_prompt(request: crate::cloud::CloudSubmission<'_>, channel: ResultChannel) -> String {
    let job = request.job;
    let claude = matches!(channel, ResultChannel::ClaudeBranch);
    let work = match &job.work_branch {
        Some(work) => format!(
            "You are a writer. Use repository {:?}. Fetch origin. If origin has branch {work:?}, check it out and confirm it contains commit {}; otherwise create {work:?} at commit {}. You may merge origin/{} into it. Commit your changes and push them to {work:?} without force (git push origin HEAD:refs/heads/{work}); never force-push, rewrite published history or push to another branch.{} Run the proof the task asks for on that branch; heavy builds and tests are allowed.",
            job.repository,
            job.commit,
            job.commit,
            job.branch,
            if claude {
                " If the platform refuses that branch name, push to the working branch it allows (for example your claude/ branch) and use that branch throughout."
            } else {
                ""
            }
        ),
        None => format!(
            "Use repository {:?}, start from commit {} on branch {:?} and run the requested verification there.",
            job.repository, job.commit, job.branch
        ),
    };
    let fields = format!(
        "job_id {:?}, nonce {:?}, commit {:?} (the full commit you started from), exit_status (integer), tests_run and tests_failed (arrays of strings), log_location (string) and observed_model (the model you are running as, or null if you cannot tell)",
        job.job_id, job.nonce, job.commit
    );
    let pushed = if job.work_branch.is_some() && !claude {
        ", plus branch (the branch you pushed) and head (the full commit ID of its pushed head)"
    } else {
        ""
    };
    let report = match channel {
        ResultChannel::ClaudeBranch => format!(
            "When you have finished, push one more commit to the branch you used, which may be empty (git commit --allow-empty), whose message ends with the single line `{RESULT_TRAILER} ` followed by the result as one-line JSON with {fields}. {}",
            if job.work_branch.is_some() {
                "The owner reads the branch and head from the remote."
            } else {
                "Push it to your claude/ working branch on top of the commit; the owner deletes that branch after reading it."
            }
        ),
        ResultChannel::CodexDiff => format!(
            "Write the result to .provenant-cloud-result.json as JSON with {fields}{pushed}. Leave that file uncommitted so it appears in your cloud diff."
        ),
        ResultChannel::CodexThread => format!(
            "End with a final message that contains the result as JSON with {fields}{pushed}."
        ),
    };
    format!(
        "{}\n\n{work} {report} Do not include credentials in the result, commits or messages. Requested model: {:?}; requested effort: {:?}. Report the model you actually run as observed_model; never copy the requested model.",
        request.task, job.requested_model, job.requested_effort
    )
}

fn full_oid(oid: &str) -> bool {
    crate::cloud::full_oid(oid)
}

/// Parse one `git ls-remote` line, refusing anything but a full ID and branch ref.
fn remote_ref(line: &str) -> Result<(&str, &str), String> {
    match line.split_once('\t') {
        Some((oid, reference))
            if full_oid(oid)
                && reference
                    .strip_prefix("refs/heads/")
                    .is_some_and(crate::cloud::valid_branch) =>
        {
            Ok((oid, reference))
        }
        _ => Err(format!("unrecognised git ls-remote output: {line}")),
    }
}

fn fetch<C: CloudCli>(cli: &mut C, reference: &str) -> Result<(), String> {
    cli.run(
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
    )
    .map(|_| ())
}

/// True when the job's starting commit is in the pushed head's history. A
/// failed check, including Git's "not an ancestor" exit, is not proof.
fn descends<C: CloudCli>(cli: &mut C, commit: &str, head: &str) -> bool {
    cli.run("git", &["merge-base", "--is-ancestor", commit, head], None)
        .is_ok()
}

/// Confirm a writer's reported push on the remote: the named branch exists,
/// its head is the reported head, and that head descends from the job's
/// starting commit. `Ok(Some(reason))` refuses the evidence; `Err` is a
/// transport failure that settles nothing and is observed again later.
pub fn verify_push<C: CloudCli>(
    cli: &mut C,
    job: &crate::cloud::RemoteJob,
    result: &crate::cloud::CloudResult,
) -> Result<Option<String>, String> {
    let (Some(branch), Some(head)) = (result.branch.as_deref(), result.head.as_deref()) else {
        return Ok(Some(
            "the writer did not report its pushed branch and head".into(),
        ));
    };
    if job.work_branch.as_deref() != Some(branch) {
        return Ok(Some(format!(
            "the writer reported branch {branch:?}, not the named branch {:?}",
            job.work_branch.as_deref().unwrap_or_default()
        )));
    }
    if !crate::cloud::valid_branch(branch) || !full_oid(head) {
        return Ok(Some("the writer reported an invalid branch or head".into()));
    }
    let reference = format!("refs/heads/{branch}");
    let listed = cli.run("git", &["ls-remote", "origin", &reference], None)?;
    let mut remote = None;
    for line in listed.lines().filter(|line| !line.trim().is_empty()) {
        let (oid, name) = remote_ref(line)?;
        if name == reference {
            remote = Some(oid.to_owned());
        }
    }
    match remote {
        Some(oid) if oid.eq_ignore_ascii_case(head) => {}
        Some(oid) => {
            return Ok(Some(format!(
                "origin {branch} is at {oid}, not the reported head {head}"
            )));
        }
        None => return Ok(Some(format!("origin has no branch {branch}"))),
    }
    fetch(cli, &reference)?;
    if !descends(cli, &job.commit, head) {
        return Ok(Some(format!(
            "pushed head {head} does not descend from commit {}",
            job.commit
        )));
    }
    Ok(None)
}

/// Settle a finished writer only on a verified push; otherwise fail with why.
fn settle_writer<C: CloudCli>(
    cli: &mut C,
    job: &crate::cloud::RemoteJob,
    result: crate::cloud::CloudResult,
) -> Result<crate::cloud::RemoteState, String> {
    if job.work_branch.is_none() {
        return Ok(crate::cloud::RemoteState::Finished { result });
    }
    Ok(match verify_push(cli, job, &result)? {
        None => crate::cloud::RemoteState::Finished { result },
        Some(detail) => crate::cloud::RemoteState::Failed {
            detail: format!("cloud writer push not verified: {detail}"),
        },
    })
}

/// The result JSON on a commit message's trailer line, if any.
fn trailer_result(message: &str) -> Option<crate::cloud::CloudResult> {
    message
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(RESULT_TRAILER))
        .and_then(|json| serde_json::from_str(json.trim()).ok())
}

/// The claude.ai page for a session ID, or the URL the CLI printed.
pub fn claude_session_url(remote_id: &str) -> Option<String> {
    let id = remote_id.trim();
    if id.starts_with("https://") {
        Some(id.to_owned())
    } else if !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        Some(format!("https://claude.ai/code/{id}"))
    } else {
        None
    }
}

fn now_unix() -> Result<u64, String> {
    let now = provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new())
        .map_err(|error| error.to_string())?
        .unix_seconds();
    u64::try_from(now).map_err(|error| error.to_string())
}

/// Claude's cloud command has no status or cancellation operation. A job's
/// completion signal is the result trailer it pushes to its branch.
pub struct ClaudeCloud<C> {
    pub cli: C,
    pub pending_cleanup: Option<(String, String)>,
}

/// Branches a Claude cloud session may push when it cannot use the named one.
const CLAUDE_BRANCHES: &str = "refs/heads/claude/*";

impl<C: CloudCli> crate::cloud::CloudProvider for ClaudeCloud<C> {
    type Error = String;
    fn check_environment(&mut self, _: crate::cloud::CloudSubmission<'_>) -> Result<(), String> {
        Ok(())
    }
    fn submit(&mut self, request: crate::cloud::CloudSubmission<'_>) -> Result<String, String> {
        let prompt = result_prompt(request, ResultChannel::ClaudeBranch);
        // Without an environment the CLI uses the hosted Claude cloud.
        let environment = request.job.environment.trim();
        let mut args = vec!["--cloud", prompt.as_str()];
        if !environment.is_empty() {
            args.extend(["--environment", environment]);
        }
        // Ask the platform for the model and effort; the record still compares
        // the requested model with the one the job reports.
        let model = request.job.requested_model.trim();
        if !model.is_empty() {
            args.extend(["--model", model]);
        }
        if let Some(effort) = request
            .job
            .requested_effort
            .as_deref()
            .map(str::trim)
            .filter(|effort| !effort.is_empty())
        {
            args.extend(["--effort", effort]);
        }
        // `claude --cloud` refuses a non-terminal, so only this call needs a PTY.
        let out = self.cli.submit("claude", &args, None, true)?;
        parse_identity(&out).map_err(|_| {
            format!(
                "claude --cloud returned no session identity: {}",
                failure_excerpt(&out)
            )
        })
    }
    fn session_url(&self, remote_id: &str) -> Option<String> {
        claude_session_url(remote_id)
    }
    fn failure_detail(&self, error: &String) -> Option<String> {
        Some(failure_excerpt(
            error.strip_prefix(REFUSAL).unwrap_or(error),
        ))
    }
    fn submission_refused(&self, error: &String) -> bool {
        error.starts_with(REFUSAL)
    }
    fn observe(
        &mut self,
        job: &crate::cloud::RemoteJob,
    ) -> Result<crate::cloud::RemoteState, String> {
        self.pending_cleanup = None;
        let mut patterns = vec![CLAUDE_BRANCHES.to_owned()];
        if let Some(work) = &job.work_branch {
            patterns.push(format!("refs/heads/{work}"));
        }
        let mut args = vec!["ls-remote", "origin"];
        args.extend(patterns.iter().map(String::as_str));
        let refs = self.cli.run("git", &args, None)?;
        for line in refs.lines().filter(|line| !line.trim().is_empty()) {
            let (oid, reference) = remote_ref(line)?;
            let branch = &reference["refs/heads/".len()..];
            if self
                .cli
                .run(
                    "git",
                    &["cat-file", "-e", &format!("{oid}^{{commit}}")],
                    None,
                )
                .is_err()
            {
                fetch(&mut self.cli, reference)?;
            }
            let message = self
                .cli
                .run("git", &["log", "-1", "--format=%B", oid], None)?;
            let Some(mut result) = trailer_result(&message) else {
                continue;
            };
            // The remote ref, not the job's report, names the branch and head.
            result.branch = Some(branch.to_owned());
            result.head = Some(oid.to_owned());
            if job.check(&result).is_err() || !descends(&mut self.cli, &job.commit, oid) {
                continue;
            }
            if job.work_branch.is_none() {
                // A verification job's result-only branch is not a deliverable.
                self.pending_cleanup = Some((reference.to_owned(), oid.to_owned()));
            }
            return Ok(crate::cloud::RemoteState::Finished { result });
        }
        if now_unix()? >= job.deadline_unix_seconds {
            let session = job
                .session_url
                .clone()
                .or_else(|| claude_session_url(&job.remote_id))
                .unwrap_or_else(|| job.remote_id.clone());
            return Ok(crate::cloud::RemoteState::Unknown {
                detail: format!(
                    "Cloud result did not arrive before the deadline; open {session} to read the session"
                ),
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
    pub settings: ProviderCloudSettings,
}

impl<C: CloudCli> CodexCloud<C> {
    pub fn new(cli: C, settings: ProviderCloudSettings) -> Self {
        Self { cli, settings }
    }

    /// Apply the transport saved at submission, not the current host config.
    fn adopt(&mut self, job: &crate::cloud::RemoteJob) {
        self.settings = ProviderCloudSettings {
            transport: job.cloud_transport,
            command: job.cloud_command.clone(),
        };
    }

    fn program(&self) -> String {
        self.settings
            .command
            .clone()
            .unwrap_or_else(|| match self.settings.transport {
                CodexCloudTransport::Official => "codex".into(),
                CodexCloudTransport::Thread => "codex-cloud-task".into(),
            })
    }

    /// Recover only an unambiguous list entry carrying the caller's correlation marker.
    /// A list without that marker cannot safely distinguish concurrent submissions.
    pub fn recover(&mut self, marker: &str) -> Result<Option<String>, String> {
        if marker.trim().is_empty() {
            return Err("empty cloud recovery marker".into());
        }
        // Thread `list` enumerates environments, not submitted jobs. Never use
        // one of those IDs to recover or retry an uncertain submission.
        if self.settings.transport == CodexCloudTransport::Thread {
            return Ok(None);
        }
        let out = self
            .cli
            .run(&self.program(), &["cloud", "list", "--json"], None)?;
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
        if self.settings.transport == CodexCloudTransport::Thread {
            let prompt = result_prompt(request, ResultChannel::CodexThread);
            // No read-only sandbox: writers push, and heavy proof writes
            // build output even in a verification job.
            let out = self.cli.submit(
                &self.program(),
                &["submit", "--env", request.job.environment.trim()],
                Some(&prompt),
                false,
            )?;
            return thread::submission_id(&out);
        }
        let prompt = result_prompt(request, ResultChannel::CodexDiff);
        let out = self.cli.submit(
            &self.program(),
            &[
                "cloud",
                "exec",
                "--env",
                request.job.environment.trim(),
                "--branch",
                &request.job.branch,
            ],
            Some(&prompt),
            false,
        )?;
        parse_identity(&out)
    }
    fn failure_detail(&self, error: &String) -> Option<String> {
        Some(failure_excerpt(
            error.strip_prefix(REFUSAL).unwrap_or(error),
        ))
    }
    fn submission_refused(&self, error: &String) -> bool {
        error.starts_with(REFUSAL)
    }
    fn stamp(&self, job: &mut crate::cloud::RemoteJob) {
        job.cloud_transport = self.settings.transport;
        job.cloud_command = self.settings.command.clone();
    }
    fn recover(&mut self, job: &crate::cloud::RemoteJob) -> Result<Option<String>, String> {
        self.adopt(job);
        CodexCloud::recover(self, &job.nonce)
    }
    fn observe(
        &mut self,
        job: &crate::cloud::RemoteJob,
    ) -> Result<crate::cloud::RemoteState, String> {
        self.adopt(job);
        if self.settings.transport == CodexCloudTransport::Thread {
            let out = self
                .cli
                .run(&self.program(), &["status", &job.remote_id], None)?;
            return Ok(match thread::observe(&out, job)? {
                crate::cloud::RemoteState::Finished { result } => {
                    settle_writer(&mut self.cli, job, result)?
                }
                state => state,
            });
        }
        let out = self
            .cli
            .run(&self.program(), &["cloud", "status", &job.remote_id], None)?;
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
                .run(&self.program(), &["cloud", "diff", &job.remote_id], None)?;
            let mut result = crate::cloud_result::result_from_diff(&diff)?;
            // Codex cannot honour a requested model; do not infer one from the request.
            result.observed_model = None;
            job.check(&result).map_err(|e| e.to_string())?;
            settle_writer(&mut self.cli, job, result)
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
            work_branch: None,
            remote_id: "remote".into(),
            session_url: None,
            commit: "a".repeat(40),
            requested_model: "model".into(),
            requested_effort: None,
            state: RemoteState::Running,
            submit_error: None,
            submit_refused: false,
            cloud_transport: Default::default(),
            cloud_command: None,
        }
    }
    /// Replies keyed by the first matching argument prefix; records every call.
    struct Routes {
        routes: Vec<(&'static str, Result<String, String>)>,
        calls: Vec<String>,
    }
    impl CloudCli for Routes {
        fn run(&mut self, program: &str, args: &[&str], _: Option<&str>) -> Result<String, String> {
            let call = format!("{program} {}", args.join(" "));
            self.calls.push(call.clone());
            self.routes
                .iter()
                .find(|(prefix, _)| call.starts_with(prefix))
                .map(|(_, reply)| reply.clone())
                .unwrap_or_else(|| Err(format!("unexpected command {call}")))
        }
    }
    fn trailer(commit: &str) -> String {
        format!(
            "Finish the work\n\n{RESULT_TRAILER} {{\"job_id\":\"job\",\"nonce\":\"nonce\",\"commit\":\"{commit}\",\"exit_status\":0,\"tests_run\":[\"tier\"],\"tests_failed\":[],\"log_location\":\"log\",\"observed_model\":\"claude-opus-5-5\"}}\n"
        )
    }
    #[test]
    fn cloud_claude_verification_reads_trailer_and_deletes_after_save() {
        let oid = "b".repeat(40);
        let commit = "a".repeat(40);
        let mut adapter = ClaudeCloud {
            cli: Routes {
                routes: vec![
                    (
                        "git ls-remote origin refs/heads/claude/*",
                        Ok(format!("{oid}\trefs/heads/claude/result\n")),
                    ),
                    ("git cat-file", Err("missing".into())),
                    ("git -c fetch.fsckObjects=true fetch", Ok(String::new())),
                    ("git log -1", Ok(trailer(&commit))),
                    ("git merge-base --is-ancestor", Ok(String::new())),
                    ("git push", Ok(String::new())),
                ],
                calls: Vec::new(),
            },
            pending_cleanup: None,
        };
        let job = job();
        let RemoteState::Finished { result } = adapter.observe(&job).unwrap() else {
            panic!("expected a result")
        };
        assert_eq!(result.branch.as_deref(), Some("claude/result"));
        assert_eq!(result.head.as_deref(), Some(oid.as_str()));
        assert_eq!(result.observed_model.as_deref(), Some("claude-opus-5-5"));
        assert!(
            adapter
                .cli
                .calls
                .iter()
                .any(|c| c.contains("fetch.fsckObjects=true"))
        );
        adapter.after_save(&job);
        assert!(adapter.cli.calls.last().unwrap().contains("--delete"));
    }
    #[test]
    fn cloud_claude_writer_settles_on_the_real_branch_and_keeps_it() {
        let oid = "c".repeat(40);
        let commit = "a".repeat(40);
        let mut job = job();
        job.work_branch = Some("lane/cloud".into());
        let mut adapter = ClaudeCloud {
            cli: Routes {
                routes: vec![
                    (
                        "git ls-remote origin refs/heads/claude/* refs/heads/lane/cloud",
                        Ok(format!(
                            "{}\trefs/heads/claude/other\n{oid}\trefs/heads/claude/lane-cloud-x1\n",
                            "d".repeat(40)
                        )),
                    ),
                    ("git cat-file", Ok(String::new())),
                    (
                        Box::leak(
                            format!("git log -1 --format=%B {}", "d".repeat(40)).into_boxed_str(),
                        ),
                        Ok("unrelated\n".into()),
                    ),
                    ("git log -1", Ok(trailer(&commit))),
                    ("git merge-base --is-ancestor", Ok(String::new())),
                ],
                calls: Vec::new(),
            },
            pending_cleanup: None,
        };
        let RemoteState::Finished { result } = adapter.observe(&job).unwrap() else {
            panic!("expected a result")
        };
        // The platform's branch is reported as it is, never as the named one.
        assert_eq!(result.branch.as_deref(), Some("claude/lane-cloud-x1"));
        assert_eq!(result.head.as_deref(), Some(oid.as_str()));
        assert!(job.check(&result).unwrap().passed);
        assert!(adapter.pending_cleanup.is_none());
        adapter.after_save(&job);
        assert!(!adapter.cli.calls.iter().any(|c| c.contains("--delete")));
    }
    #[test]
    fn cloud_claude_unfinished_past_deadline_names_the_session() {
        let mut job = job();
        job.deadline_unix_seconds = 0;
        job.remote_id = "session_01ABC".into();
        let mut adapter = ClaudeCloud {
            cli: Routes {
                routes: vec![("git ls-remote", Ok(String::new()))],
                calls: Vec::new(),
            },
            pending_cleanup: None,
        };
        let RemoteState::Unknown { detail } = adapter.observe(&job).unwrap() else {
            panic!("expected unknown")
        };
        assert!(
            detail.contains("https://claude.ai/code/session_01ABC"),
            "{detail}"
        );
    }
    #[test]
    fn cloud_writer_push_is_verified_on_the_remote() {
        let head = "e".repeat(40);
        let mut job = job();
        job.work_branch = Some("lane/cloud".into());
        let mut result = crate::cloud::CloudResult {
            job_id: "job".into(),
            nonce: "nonce".into(),
            commit: job.commit.clone(),
            exit_status: 0,
            tests_run: vec!["tier".into()],
            tests_failed: vec![],
            log_location: "log".into(),
            observed_model: None,
            branch: Some("lane/cloud".into()),
            head: Some(head.clone()),
        };
        let routes = |listed: String| Routes {
            routes: vec![
                ("git ls-remote origin refs/heads/lane/cloud", Ok(listed)),
                ("git -c fetch.fsckObjects=true fetch", Ok(String::new())),
                ("git merge-base --is-ancestor", Ok(String::new())),
            ],
            calls: Vec::new(),
        };
        let mut cli = routes(format!("{head}\trefs/heads/lane/cloud\n"));
        assert_eq!(verify_push(&mut cli, &job, &result), Ok(None));
        let refused = |cli: &mut Routes, result: &crate::cloud::CloudResult| {
            verify_push(cli, &job, result).unwrap().unwrap()
        };
        let mut moved = routes(format!("{}\trefs/heads/lane/cloud\n", "f".repeat(40)));
        assert!(refused(&mut moved, &result).contains("not the reported head"));
        let mut absent = routes(String::new());
        assert!(refused(&mut absent, &result).contains("no branch"));
        let mut lost = Routes {
            routes: vec![("git ls-remote", Err("network down".into()))],
            calls: Vec::new(),
        };
        assert!(verify_push(&mut lost, &job, &result).is_err());
        result.branch = Some("main".into());
        assert!(refused(&mut cli, &result).contains("not the named branch"));
        let mut unrelated = Routes {
            routes: vec![
                (
                    "git ls-remote",
                    Ok(format!("{head}\trefs/heads/lane/cloud\n")),
                ),
                ("git -c", Ok(String::new())),
                ("git merge-base", Err("git failed: ".into())),
            ],
            calls: Vec::new(),
        };
        result.branch = Some("lane/cloud".into());
        assert!(refused(&mut unrelated, &result).contains("does not descend"));
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
            settings: ProviderCloudSettings::default(),
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
    fn cloud_codex_finished_result_is_checked_and_accepts_changed_files() {
        use std::collections::VecDeque;
        struct Script(VecDeque<String>);
        impl CloudCli for Script {
            fn run(&mut self, _: &str, _: &[&str], _: Option<&str>) -> Result<String, String> {
                self.0.pop_front().ok_or_else(|| "missing response".into())
            }
        }
        let job = job();
        let diff = format!(
            "diff --git a/.provenant-cloud-result.json b/.provenant-cloud-result.json\nnew file mode 100644\n--- /dev/null\n+++ b/.provenant-cloud-result.json\n@@ -0,0 +1 @@\n+{{\"job_id\":\"job\",\"nonce\":\"nonce\",\"commit\":\"{}\",\"exit_status\":0,\"tests_run\":[\"test\"],\"tests_failed\":[],\"log_location\":\"log\",\"observed_model\":\"claimed\"}}\ndiff --git a/src/lib.rs b/src/lib.rs\nindex 1234567..7654321 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
            job.commit
        );
        let mut adapter = CodexCloud {
            settings: ProviderCloudSettings::default(),
            cli: Script(["completed".into(), diff.clone()].into()),
        };
        let RemoteState::Finished { result } = adapter.observe(&job).unwrap() else {
            panic!("expected result")
        };
        let checked = job.check(&result).unwrap();
        assert!(checked.passed);
        assert_eq!(checked.model, crate::cloud::ModelObservation::Unknown);
        let mut wrong = job;
        wrong.nonce = "other".into();
        let mut adapter = CodexCloud {
            settings: ProviderCloudSettings::default(),
            cli: Script(["completed".into(), diff].into()),
        };
        assert!(adapter.observe(&wrong).is_err());
    }
    const CAPTURED: &str = "\u{1b}[>4m\u{1b}]0;claude\u{7}\u{1b}[?25lCreated cloud session: Fix the thing\r\nView: https://claude.ai/code/session_01ABC?from=cli&m=0\r\nResume with: claude --teleport session_01ABC\r\n\u{1b}[?25h";
    #[test]
    fn cloud_submission_classifies_only_complete_nonzero_refusals() {
        use provenant_os::exec::{
            CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
        };
        struct FakeExec {
            termination: Termination,
            stdout: &'static str,
            stderr: &'static str,
            timed_out: bool,
        }
        impl Executor for FakeExec {
            fn execute(&self, _: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                Ok(ExecOutcome {
                    termination: self.termination,
                    stdout: CapturedStream {
                        bytes: self.stdout.as_bytes().to_vec(),
                        ..Default::default()
                    },
                    stderr: CapturedStream {
                        bytes: self.stderr.as_bytes().to_vec(),
                        ..Default::default()
                    },
                    timed_out: self.timed_out,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 0,
                })
            }
        }
        for (termination, stdout, stderr, timed_out, refused) in [
            (
                Termination::Exit(1),
                "",
                "no cloud environments are available",
                false,
                true,
            ),
            (
                Termination::Exit(1),
                "",
                "--cloud requires an interactive terminal",
                false,
                true,
            ),
            (
                Termination::Exit(1),
                "session_id: remote-1",
                "error",
                false,
                false,
            ),
            (
                Termination::Exit(1),
                "Created cloud thread thread-1\nSubmitting message ...",
                "",
                false,
                false,
            ),
            (
                Termination::Exit(1),
                "",
                "{\"threadId\":\"thread-1\"}",
                false,
                false,
            ),
            (
                Termination::Exit(1),
                "{\"threadId\":\"thread-2\"}\nwarning: monitor not started",
                "",
                false,
                false,
            ),
            (Termination::Signal(9), "", "error", false, false),
            (Termination::Exit(137), "", "error", false, false),
            (Termination::Exit(1), "", "error", true, false),
            (Termination::Exit(1), "", "", false, false),
        ] {
            let executor = FakeExec {
                termination,
                stdout,
                stderr,
                timed_out,
            };
            let mut cli = InstalledCloudCli {
                executor: &executor,
                cwd: PathBuf::from("."),
                timeout: Duration::from_secs(1),
            };
            let error = CloudCli::submit(&mut cli, "fixture", &[], None, false).unwrap_err();
            assert_eq!(
                error.starts_with(REFUSAL),
                refused,
                "{termination:?} {stdout} {stderr}"
            );
        }
    }

    #[test]
    fn cloud_adapters_settle_refusals_and_keep_unparseable_success_uncertain() {
        use crate::cloud::{CloudJobError, CloudJobStore};
        #[derive(Default)]
        struct Store(Option<RemoteJob>);
        impl CloudJobStore for Store {
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
        struct Response(bool);
        impl CloudCli for Response {
            fn run(&mut self, _: &str, _: &[&str], _: Option<&str>) -> Result<String, String> {
                if self.0 {
                    Err(format!("{REFUSAL}no cloud environments are available"))
                } else {
                    Ok("unparseable".into())
                }
            }
        }
        for refused in [true, false] {
            let mut claude = ClaudeCloud {
                cli: Response(refused),
                pending_cleanup: None,
            };
            let mut codex = CodexCloud::new(Response(refused), ProviderCloudSettings::default());
            let mut a = Store::default();
            let mut b = Store::default();
            let first = crate::cloud::submit(&mut claude, &mut a, job(), "test");
            let second = crate::cloud::submit(&mut codex, &mut b, job(), "test");
            if refused {
                for saved in [first.unwrap(), second.unwrap()] {
                    assert_eq!(
                        saved.state,
                        RemoteState::Failed {
                            detail: "no cloud environments are available".into()
                        }
                    );
                }
            } else {
                assert!(matches!(first, Err(CloudJobError::SubmitUnknown(_))));
                assert!(matches!(second, Err(CloudJobError::SubmitUnknown(_))));
                assert_eq!(a.0.unwrap().state, RemoteState::Submitting);
                assert_eq!(b.0.unwrap().state, RemoteState::Submitting);
            }
        }
    }

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
        assert_eq!(
            &claude.cli.calls[0].1[2..],
            ["--environment", "env", "--model", "model"]
        );
        assert!(claude.cli.calls[0].1[1].contains("Requested model: \"model\""));
        let mut hosted = job.clone();
        hosted.environment.clear();
        claude
            .submit(CloudSubmission {
                job: &hosted,
                task: "test task",
            })
            .unwrap();
        assert_eq!(&claude.cli.calls[1].1[2..], ["--model", "model"]);
        assert!(!claude.cli.calls[1].1.iter().any(|a| a == "--environment"));
        let mut writer = hosted.clone();
        writer.work_branch = Some("lane/cloud".into());
        writer.requested_effort = Some("high".into());
        claude
            .submit(CloudSubmission {
                job: &writer,
                task: "test task",
            })
            .unwrap();
        assert_eq!(
            &claude.cli.calls[2].1[2..],
            ["--model", "model", "--effort", "high"]
        );
        let prompt = &claude.cli.calls[2].1[1];
        assert!(prompt.contains("without force"), "{prompt}");
        assert!(prompt.contains("lane/cloud"), "{prompt}");
        assert!(prompt.contains(RESULT_TRAILER), "{prompt}");
        assert_eq!(
            claude.session_url("session_01ABC").as_deref(),
            Some("https://claude.ai/code/session_01ABC")
        );
        let mut codex_without = CodexCloud {
            settings: ProviderCloudSettings::default(),
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
            settings: ProviderCloudSettings::default(),
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
