//! `provenant wait`: block until graph targets or pull requests settle. A wait
//! is a reader: neither expiry nor cancellation stops the work. Local graphs
//! share one event source; pull requests remain on forge observation.
// This edge owns the real deadline for channel receives and source I/O.
#![allow(clippy::disallowed_methods)]
use provenant_cli::{
    Globals,
    wait::{LocalCadence, PrHeadSource, PrHeadUnpinned},
};
use provenant_core::{CancelToken, Clock as _, GitOid, WallTime};
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use provenant_records::{
    Outcome,
    subject::{HeadCheck, PinnedSubject, SubjectOrigin, SubjectPolicy, head_check},
    warning::{self, WarningCode},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    time::{Duration, Instant, SystemTime},
};

mod stall;
use stall::{local_stall_wait, timeout_wake};

#[derive(Clone, Debug)]
enum Source {
    Graph(provenant_cli::Target),
    UnreadableGraph(provenant_cli::Target, String),
    Pr {
        number: u64,
        repository: Result<String, String>,
        /// The head and policy fixed once at admission. `None` until the forge
        /// reports a head; the first successful sample pins it then.
        subject: Option<Box<PinnedSubject>>,
        /// Whether the pull request has reported the pinned head at least once.
        /// Only after that does a different head count as a move.
        pinned_head_seen: bool,
        /// The effective `domains.wait.pr_head_policy` (or its command
        /// override) this wait applies to every sample, one-off reads included.
        policy: SubjectPolicy,
        /// The typed source of the policy that fixed this head, carried on
        /// every sample so a first pin reports the same source as admission.
        policy_source: provenant_records::Source,
        complete: bool,
    },
}
impl Source {
    fn label(&self) -> String {
        match self {
            Self::Graph(target) | Self::UnreadableGraph(target, _) => target.to_string(),
            Self::Pr { number, .. } => format!("#{number}"),
        }
    }
}

/// Where one target stands for a waiter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Settle {
    Pending,
    NeedsInput,
    Succeeded,
    Unsuccessful,
    /// A live attempt silent past its threshold. The wait ends, but the work
    /// continues: this is a report, not a settlement verdict.
    Stalled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitMode {
    All,
    Any,
    FailFast,
}

impl WaitMode {
    fn from_command(command: &provenant_cli::Wait) -> Self {
        if command.any {
            Self::Any
        } else if command.fail_fast {
            Self::FailFast
        } else {
            Self::All
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Any => "any",
            Self::FailFast => "fail-fast",
        }
    }
}

fn mode_heading(mode: WaitMode) -> String {
    format!("wait mode: {}\n", mode.as_str())
}

fn terminal_data(
    mode: WaitMode,
    targets: Vec<Value>,
    timed_out: bool,
    local_observation: Option<&str>,
) -> Value {
    json!({
        "mode": mode.as_str(),
        "targets": targets,
        "timed_out": timed_out,
        "local_observation": local_observation,
    })
}

/// The settle phrase a chair reads in the final line. An unfinished target at
/// expiry is reported as still running at timeout, never as settled.
fn settle_phrase(settle: Settle, done: bool) -> &'static str {
    match settle {
        Settle::Succeeded => "settled ok",
        Settle::Unsuccessful => "failed",
        Settle::NeedsInput => "needs input",
        Settle::Stalled => "stalled",
        Settle::Pending if done => "still running",
        Settle::Pending => "still running at timeout",
    }
}

/// The one next command after a wait. Targets still running at timeout are the
/// ones a chair resumes; once every target has settled, a status re-read is the
/// neutral next step. The command is derived only from states just observed.
fn next_command(sources: &[Source], latest: &[Option<Reading>], superseding: &[String]) -> String {
    // A superseded name is still read as asked; the next command points at the
    // newer handle instead of re-reading the old one.
    if !superseding.is_empty() {
        return format!("provenant wait {}", superseding.join(" "));
    }
    let pending: Vec<_> = sources
        .iter()
        .zip(latest)
        .filter(|(_, reading)| {
            reading
                .as_ref()
                .is_some_and(|reading| reading.settle == Settle::Pending)
        })
        .map(|(source, _)| source.label())
        .collect();
    if !pending.is_empty() {
        return format!("provenant wait {}", pending.join(" "));
    }
    let targets: Vec<_> = sources.iter().map(Source::label).collect();
    if targets.is_empty() {
        "provenant status".to_owned()
    } else {
        format!("provenant status {}", targets.join(" "))
    }
}

/// Every target's settle state, in target order. Shared by the full human
/// line and the compact summary's `next` so both last lines agree.
fn settle_states(sources: &[Source], latest: &[Option<Reading>], done: bool) -> String {
    let states: Vec<_> = sources
        .iter()
        .zip(latest)
        .map(|(source, reading)| {
            let settle = reading
                .as_ref()
                .map_or(Settle::Pending, |reading| reading.settle);
            format!("{} {}", source.label(), settle_phrase(settle, done))
        })
        .collect();
    if states.is_empty() {
        "nothing to wait for".to_owned()
    } else {
        states.join(", ")
    }
}

/// The one next command, paired with the settle states as its `why`. The edge
/// renders this typed step once for human output; JSON carries the same argv.
fn final_step(
    sources: &[Source],
    latest: &[Option<Reading>],
    done: bool,
    superseding: &[String],
) -> provenant_records::command_result::NextStep {
    provenant_records::command_result::NextStep::from_command(
        &next_command(sources, latest, superseding),
        settle_states(sources, latest, done),
    )
}

/// The last known check summary and next step that `pr watch` used to print at
/// a timeout. `wait --pr` keeps them as an extra diagnostic line so the final
/// line stays owned by the wait final-line format.
fn pr_timeout_details(sources: &[Source], latest: &[Option<Reading>]) -> Vec<String> {
    sources
        .iter()
        .zip(latest)
        .filter_map(|(source, reading)| match (source, reading) {
            (Source::Pr { complete, .. }, Some(reading)) => {
                serde_json::from_value::<crate::pr::status::Line>(reading.data.clone())
                    .ok()
                    .filter(|line| line.wait_verdict(*complete).is_none())
                    .map(|line| {
                        let next = if line.checks_unreported() {
                            format!(
                                "; cause=checks_not_scheduled; next: {}",
                                crate::pr::status::NO_CHECKS_NEXT
                            )
                        } else {
                            String::new()
                        };
                        format!("last known {}{next}", line.text())
                    })
            }
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reading {
    pub(crate) settle: Settle,
    /// `TARGET STATE [cause]`, bounded; equal lines are one observation.
    pub(crate) line: String,
    /// The full observation this reading was derived from. Crate-visible so a
    /// journal producer can prove its projected status survives into the
    /// reading that the wait's human and summary output render.
    pub(crate) data: Value,
}
impl Reading {
    fn done(&self) -> bool {
        self.settle != Settle::Pending
    }
    fn unavailable(label: &str, error: &str) -> Self {
        Self {
            settle: Settle::Pending,
            line: provenant_cli::bounded_text(
                &format!("{label} unavailable: {error}"),
                provenant_cli::limits::SUMMARY_ROW_BYTES,
            ),
            data: Value::Null,
        }
    }
}

/// Holds a settling PR reading pending, for a bounded period, while Actions has
/// reported no run for its head. A push's checks start after the push, so the
/// first reading of a new head can look settled before its workflows exist.
struct ActionsGrace {
    limit: Duration,
    /// The head last read and when it was first read.
    head: Option<(String, Instant)>,
    /// When the last reading was held, the moment the hold ends. The waiter
    /// must read again then: no event or timer of its own would wake it.
    held_until: Option<Instant>,
}
impl ActionsGrace {
    fn new(limit: Duration) -> Self {
        Self {
            limit,
            head: None,
            held_until: None,
        }
    }

    /// How long until the hold on the last reading ends; `None` when the last
    /// reading was not held.
    fn due_in(&self) -> Option<Duration> {
        self.held_until
            .map(|end| end.saturating_duration_since(Instant::now()))
    }

    /// Whether a held reading's grace period has ended and it needs a fresh read.
    fn is_due(&self) -> bool {
        self.due_in().is_some_and(|left| left.is_zero())
    }

    fn apply(&mut self, reading: Reading, label: &str, complete: bool) -> Reading {
        self.held_until = None;
        let Ok(mut line) = serde_json::from_value::<crate::pr::status::Line>(reading.data.clone())
        else {
            return reading;
        };
        // A head that moved past the pinned subject is a terminal decision,
        // not a passing verdict that might still gain Actions runs.
        if line.cause.as_deref() == Some(crate::pr::status::PR_HEAD_MOVED) {
            return reading;
        }
        let first_read = match &self.head {
            Some((head, at)) if *head == line.head => *at,
            _ => {
                let now = Instant::now();
                self.head = Some((line.head.clone(), now));
                now
            }
        };
        if first_read.elapsed() >= self.limit || !line.awaits_actions(complete) {
            return reading;
        }
        line.hold_for_actions();
        self.held_until = Some(first_read + self.limit);
        Reading {
            settle: Settle::Pending,
            line: format!(
                "{label} {}{} head={}",
                line.class(),
                line.cause_suffix(),
                line.awaiting.as_deref().unwrap_or(&line.head)
            ),
            data: json!(line),
        }
    }
}

/// A PR reading is only a verdict about the head being watched. While GitHub
/// reports a different head, keep the reading pending whatever its checks say.
fn gate_line(line: &mut crate::pr::status::Line, watch_head: Option<&str>) {
    if let Some(head) = watch_head
        && line.awaiting.is_none()
        && line.head != head
    {
        line.await_head(head);
    }
}

/// The head a PR reading is watching, always shown in its status line. The
/// pinned subject is authoritative: a head the pull request moved to is never
/// reported as the thing the wait is judging.
fn pinned_head(subject: &Option<Box<PinnedSubject>>, line: &crate::pr::status::Line) -> String {
    subject.as_ref().map_or_else(
        || line.awaiting.as_deref().unwrap_or(&line.head).to_owned(),
        |subject| subject.head.to_string(),
    )
}

/// The observation time for a subject pinned at admission or first sample.
fn wall_now() -> WallTime {
    provenant_os::clock::SystemClock::new()
        .wall_now()
        .unwrap_or(WallTime::UNIX_EPOCH)
}

/// Build the subject a `wait --pr` pins at admission. The invariant: exactly
/// one head is fixed per wait and `--head` always pins, whatever the forge
/// later reports. `auto` prefers a local tip that differs from the forge,
/// `forge` always takes the reported head, and `local` takes the branch tip
/// when there is one. A head without a chosen source leaves the subject unset.
#[allow(
    clippy::too_many_arguments,
    reason = "the admission inputs stay explicit at this boundary"
)]
fn pr_subject(
    number: u64,
    repository: &Result<String, String>,
    policy: SubjectPolicy,
    source: PrHeadSource,
    policy_source: &provenant_records::Source,
    now: WallTime,
    command_head: Option<&str>,
    forge: Option<(String, String)>,
    local_tip: Option<String>,
) -> Option<PinnedSubject> {
    let make = |head: GitOid,
                origin: SubjectOrigin,
                policy: SubjectPolicy,
                source: &provenant_records::Source| PinnedSubject {
        extra: Default::default(),
        base: None,
        head,
        origin,
        pinned_at: now,
        policy,
        repository: repository.as_ref().ok().cloned(),
        base_ref: None,
        policy_source: Some(source.clone()),
        followed_from: Vec::new(),
    };
    if let Some(head) = command_head {
        return GitOid::parse(head).ok().map(|head| {
            make(
                head,
                SubjectOrigin::Explicit,
                SubjectPolicy::Pinned,
                &provenant_records::Source::at(provenant_records::Layer::Launch, "--head"),
            )
        });
    }
    let (sha, branch) = forge?;
    if source != PrHeadSource::Forge
        && let Some(local) = local_tip
        && (source == PrHeadSource::Local || local != sha)
        && let Ok(head) = GitOid::parse(&local)
    {
        return Some(make(
            head,
            SubjectOrigin::LocalRef { name: branch },
            policy,
            policy_source,
        ));
    }
    GitOid::parse(&sha).ok().map(|head| {
        make(
            head,
            SubjectOrigin::ForgePr {
                repository: repository.as_ref().cloned().unwrap_or_default(),
                number,
            },
            policy,
            policy_source,
        )
    })
}

/// The terminal reading for a wait that refuses to proceed without a head.
/// The invariant: the cause is the declared `head_unpinned` and the next step
/// names the key that chose refusal, so the operator can change one setting.
fn unpinned_refusal(number: u64, source: &str) -> Reading {
    Reading {
        settle: Settle::Unsuccessful,
        line: format!("#{number} head_unpinned ({source})"),
        data: json!({
            "number": number,
            "cause": crate::pr::status::HEAD_UNPINNED,
            "next": format!("set {source} to first_sample, or rerun with --head"),
        }),
    }
}

/// The fixed context of one PR observation: which pull request, the policy a
/// first pin takes and the key that chose it, and when it was observed.
#[derive(Clone, Copy)]
pub(crate) struct PrSample<'a> {
    pub(crate) number: u64,
    pub(crate) repository: &'a Result<String, String>,
    pub(crate) policy: SubjectPolicy,
    pub(crate) policy_source: &'a provenant_records::Source,
    pub(crate) now: WallTime,
}

/// Apply one PR observation to the wait's pinned subject. Updates `subject`
/// and `pinned_head_seen` in place; returns `true` only when a previously pinned_head_seen head moved
/// past a pinned subject. The invariant: a verdict is drawn only from the head
/// pinned at admission (or the first sample when the forge was unreadable),
/// never from a later head whose checks belong to different work.
fn apply_subject_sample(
    subject: &mut Option<Box<PinnedSubject>>,
    pinned_head_seen: &mut bool,
    head_check_seen: &mut Option<HeadCheck>,
    line: &mut crate::pr::status::Line,
    sample: &PrSample<'_>,
) -> bool {
    let PrSample {
        number,
        repository,
        policy,
        policy_source,
        now,
    } = *sample;
    let Some(subject_ref) = subject.as_deref_mut() else {
        // The forge was unreadable at admission: the first successful sample
        // pins its head, with the `ForgePr` origin and the configured policy.
        if let Ok(head) = GitOid::parse(&line.head) {
            *subject = Some(Box::new(PinnedSubject {
                extra: Default::default(),
                base: None,
                head,
                origin: SubjectOrigin::ForgePr {
                    repository: repository.as_ref().cloned().unwrap_or_default(),
                    number,
                },
                pinned_at: now,
                policy,
                repository: repository.as_ref().ok().cloned(),
                base_ref: None,
                policy_source: Some(policy_source.clone()),
                followed_from: Vec::new(),
            }));
        }
        *pinned_head_seen = true;
        return false;
    };
    let (pinned, policy) = (subject_ref.head, subject_ref.policy);
    let pinned_text = pinned.to_string();
    let observed = head_check(
        &pinned,
        GitOid::parse(&line.head).map_err(|error| error.to_string()),
    );
    *head_check_seen = Some(observed.clone());
    match observed {
        HeadCheck::Unmoved => *pinned_head_seen = true,
        HeadCheck::Moved { current, .. } => {
            if *pinned_head_seen {
                match policy {
                    SubjectPolicy::Pinned => {
                        line.cause = Some(crate::pr::status::PR_HEAD_MOVED.to_owned());
                        line.next = Some(format!(
                            "rerun provenant wait --pr {number} for the new head, or set domains.wait.pr_head_policy=follow_latest"
                        ));
                        return true;
                    }
                    SubjectPolicy::FollowLatest => {
                        warn(
                            warning::PR_HEAD_FOLLOWED,
                            format!(
                                "#{number} head moved from {pinned} to {current}; following the new head"
                            ),
                        );
                        subject_ref.followed_from.push(pinned);
                        subject_ref.head = current;
                        subject_ref.pinned_at = now;
                        *pinned_head_seen = false;
                    }
                }
            }
        }
        HeadCheck::Unavailable { .. } => {}
    }
    if !*pinned_head_seen {
        gate_line(line, Some(&pinned_text));
    }
    false
}

fn warn(code: WarningCode, message: String) {
    warning::warn(warning::Warning::new(
        code,
        message,
        provenant_records::warning::detected_source(code),
    ));
}

/// Report that a local head was requested but the branch has no local tip.
/// The source names the key that chose `local`, so a caller can change it.
fn warn_local_head_unavailable(number: u64, source: &str) {
    warn(
        warning::PR_HEAD_LOCAL_UNAVAILABLE,
        format!(
            "#{number}: no local branch tip for this pull request; using the forge head ({source})"
        ),
    );
}

/// The tip of a branch known locally, checked in the local repository only.
/// Both the checked-out branch and its tracking ref under the configured forge
/// remote count; a missing repository, remote or unsafe branch name simply
/// means no local tip is known.
fn local_branch_tip(state: &Path, cwd: &Path, branch: &str) -> Option<String> {
    let binary = provenant_os::git::git_executable()?;
    let mut refs = vec![provenant_confinement::repo::GitRefName::branch(branch).ok()?];
    if let Some(remote) = provenant_cli::run::resolved_forge_remote(state, cwd) {
        refs.push(
            provenant_confinement::repo::GitRefName::remote_tracking(format!(
                "{}/{branch}",
                remote.as_str()
            ))
            .ok()?,
        );
    }
    let environment: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            !key.to_str()
                .is_some_and(|key| crate::fabric::GIT_REDIRECTS.contains(&key))
        })
        .chain([("GIT_TERMINAL_PROMPT".into(), "0".into())])
        .collect();
    let executor = SystemExecutor::new(1).ok()?;
    for reference in refs {
        let argv: Vec<OsString> = [
            binary.clone().into_os_string(),
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(reference.as_str()),
        ]
        .into();
        let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(10));
        options.env = Some(&environment);
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture { limit: 128 };
        options.stderr = StreamPolicy::Capture { limit: 128 };
        let Ok(out) = executor.execute(options) else {
            continue;
        };
        if out.termination != Termination::Exit(0) {
            continue;
        }
        let tip = String::from_utf8_lossy(&out.stdout.bytes).trim().to_owned();
        if provenant_core::GitOid::parse(&tip).is_ok() {
            return Some(tip);
        }
    }
    None
}

/// A graph target settles by its own row: the graph, else its node, else its
/// newest attempt. A complete graph or a settled attempt succeeds only with a
/// succeeded outcome; a paused graph needs input.
fn classify_rows(label: &str, rows: &[Value]) -> Reading {
    let pick = |kind: &str| rows.iter().rfind(|row| row["kind"] == kind);
    let mut data = json!({ "rows": rows, "digests": crate::fabric::node_digests(rows) });
    // The wait is a stream of one-line events, not a detail view: bound both
    // the target line and each digest line here so a whole reason from a row
    // builder cannot grow --follow transitions or JSONL payloads. `--full`
    // status keeps the whole text; a wait never re-renders it.
    let bound = provenant_cli::limits::SUMMARY_ROW_BYTES;
    if let Some(digests) = data["digests"].as_array_mut() {
        for digest in digests {
            if let Some(line) = digest["line"].as_str() {
                digest["line"] = json!(provenant_cli::bounded_text(line, bound));
            }
        }
    }
    let Some(row) = pick("graph")
        .or_else(|| pick("node"))
        .or_else(|| pick("attempt"))
    else {
        return Reading {
            settle: Settle::Pending,
            line: format!("{label} pending"),
            data,
        };
    };
    let state = row["state"].as_str().unwrap_or("");
    let succeeded = row["outcome"] == "succeeded" || state == "succeeded";
    let settle = match row["kind"].as_str() {
        Some("graph") => match state {
            "complete" if succeeded => Settle::Succeeded,
            "complete" | "cancelled" | "unreadable" => Settle::Unsuccessful,
            "paused" => Settle::NeedsInput,
            _ => Settle::Pending,
        },
        Some("node") => match state {
            "succeeded" => Settle::Succeeded,
            "failed" | "blocked" | "skipped" => Settle::Unsuccessful,
            _ => Settle::Pending,
        },
        _ if state == "terminal" || !row["finished_at"].is_null() => {
            if succeeded {
                Settle::Succeeded
            } else {
                Settle::Unsuccessful
            }
        }
        _ if state == "unreadable" => Settle::Unsuccessful,
        _ if row["stalled"] == true => Settle::Stalled,
        _ => Settle::Pending,
    };
    // A graph or node row stays running while its attempt is silent, so the
    // stall fact lives on the attempt row even when a parent row was picked.
    let settle = if settle == Settle::Pending
        && rows
            .iter()
            .any(|row| row["kind"] == "attempt" && row["stalled"] == true)
    {
        Settle::Stalled
    } else {
        settle
    };
    let mut line = crate::fabric::status_line(label, row, None)
        .trim_end()
        .to_owned();
    if settle == Settle::Stalled {
        let last = rows
            .iter()
            .rfind(|row| row["kind"] == "attempt" && row["stalled"] == true)
            .map(|row| &row["last_event_at"])
            .unwrap_or(&row["last_event_at"]);
        if let Some(at) = last.as_str() {
            line.push_str(&format!(" last event {at}"));
        }
    }
    Reading {
        settle,
        line: provenant_cli::bounded_text(&line, bound),
        data,
    }
}

/// Render one PR observation as a reading. `head_moved` forces an
/// unsuccessful settlement for a pinned subject whose head moved after it was
/// pinned_head_seen. The subject is carried in the data so status and the wait envelope
/// show exactly what the wait pinned.
fn pr_reading(
    label: &str,
    subject: &Option<Box<PinnedSubject>>,
    head_check: &Option<HeadCheck>,
    line: crate::pr::status::Line,
    complete: bool,
    head_moved: bool,
) -> Reading {
    let head = pinned_head(subject, &line);
    let settle = if head_moved {
        Settle::Unsuccessful
    } else {
        match line.wait_verdict(complete) {
            Some(true) => Settle::Succeeded,
            Some(false) => Settle::Unsuccessful,
            None => Settle::Pending,
        }
    };
    let mut data = json!(line);
    data["subject"] = json!(subject);
    data["head_check"] = json!(head_check);
    Reading {
        settle,
        line: format!(
            "{label} {}{} head={head}",
            line.class(),
            line.cause_suffix()
        ),
        data,
    }
}

/// Read one PR source, applying the wait's subject policy to the observation.
/// `source` is owned by the polling worker so `subject` and `pinned_head_seen` persist
/// across samples: a verdict is only ever about the pinned head.
fn read_pr(
    source: &mut Source,
    edge: &provenant_cli::run::Edge<'_>,
    _session: &mut Option<provenant_cli::fabric::Context>,
    deadline: Instant,
    pr_read_timeout: Duration,
    cancel: &CancelToken,
) -> Reading {
    let label = source.label();
    let (number, repository, complete, policy, policy_source) = match &*source {
        Source::Pr {
            number,
            repository,
            complete,
            policy,
            policy_source,
            ..
        } => (
            *number,
            repository.clone(),
            *complete,
            *policy,
            policy_source.clone(),
        ),
        _ => return Reading::unavailable(&label, "not a pull request source"),
    };
    let Ok(repository) = repository else {
        return Reading::unavailable(&label, "forge context unavailable");
    };
    let mut line = match crate::pr::status::wait_line_observed(
        edge.cwd,
        edge.roots.state.as_path(),
        &repository,
        number,
        &forge_request(deadline, cancel, pr_read_timeout),
        // Spec: docs/specs/cli.md#ci-observations — the PR path reads
        // through the one observer and its one sourced retry family.
        crate::pr::observer::retry(edge.roots).poll.value,
    ) {
        Ok(line) => line,
        Err(error) => return Reading::unavailable(&label, &error.to_string()),
    };
    let mut head_check_seen = None;
    let head_moved = match &mut *source {
        Source::Pr {
            subject,
            pinned_head_seen,
            ..
        } => apply_subject_sample(
            subject,
            pinned_head_seen,
            &mut head_check_seen,
            &mut line,
            &PrSample {
                number,
                repository: &Ok(repository),
                policy,
                policy_source: &policy_source,
                now: wall_now(),
            },
        ),
        _ => unreachable!("checked above"),
    };
    line.attach_failure_digest(
        edge.roots.state.as_path(),
        edge.cwd,
        &forge_request(deadline, cancel, pr_read_timeout),
    );
    let subject = match &*source {
        Source::Pr { subject, .. } => subject.clone(),
        _ => unreachable!("checked above"),
    };
    pr_reading(
        &label,
        &subject,
        &head_check_seen,
        line,
        complete,
        head_moved,
    )
}

/// Read one source after a native event or a configured fallback tick. A graph
/// read also re-drives the journal-locked advance when its remote reading is
/// terminal, so a settled peer need not wait for the next host tick.
fn read(
    source: &Source,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<provenant_cli::fabric::Context>,
    deadline: Instant,
    pr_read_timeout: Duration,
    cancel: &CancelToken,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
) -> Reading {
    let label = source.label();
    let left = deadline.saturating_duration_since(Instant::now());
    if cancel.is_cancelled() || left.is_zero() {
        return Reading::unavailable(&label, "wait deadline or cancellation reached");
    }

    match source {
        Source::Pr { .. } => {
            // A one-off read has no state to carry forward; the polling worker
            // owns the persistent subject for the wait loop.
            let mut owned = source.clone();
            read_pr(&mut owned, edge, session, deadline, pr_read_timeout, cancel)
        }
        Source::UnreadableGraph(_, error) => classify_rows(
            &label,
            &[json!({
                "kind":"graph", "state":"unreadable", "reason":error,
            })],
        ),
        Source::Graph(target) => {
            match provenant_sqlite::with_busy_timeout(Duration::ZERO, || {
                crate::fabric::read_status_rows_with_recovery(
                    target, edge, session, deadline, cancel, workers,
                )
            }) {
                Ok(rows) => classify_rows(&label, &rows),
                Err(error) => Reading::unavailable(&label, &error.to_string()),
            }
        }
    }
}

/// Render a PR journal event as a reading, applying the same subject policy
/// as a forge read. The label is derived from the PR number so event readings
/// agree with direct readings in the wait's per-target rows.
pub(crate) fn reading_pr_event(
    subject: &mut Option<Box<PinnedSubject>>,
    pinned_head_seen: &mut bool,
    sample: &PrSample<'_>,
    complete: bool,
    detail: Value,
) -> Reading {
    let label = format!("#{}", sample.number);
    let status = &detail["status"];
    let malformed: Vec<_> = [
        "draft",
        "checks_passed",
        "checks_total",
        "checks_failed",
        "checks_pending",
        "failing_checks",
        "pending_checks",
        "merge",
        "review",
        "error",
    ]
    .into_iter()
    .filter(|field| {
        status.get(*field).is_some_and(|value| match *field {
            "draft" => !value.is_boolean(),
            "checks_passed" | "checks_total" | "checks_failed" | "checks_pending" => {
                !value.is_null()
                    && value
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .is_none()
            }
            "failing_checks" | "pending_checks" => !value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            "error" => !value.is_null() && !value.is_string(),
            _ => !value.is_string(),
        })
    })
    .collect();
    match serde_json::from_value::<crate::pr::status::Line>(status.clone()) {
        Ok(mut line) => {
            let mut head_check_seen = None;
            let head_moved = apply_subject_sample(
                subject,
                pinned_head_seen,
                &mut head_check_seen,
                &mut line,
                sample,
            );
            let mut reading = pr_reading(
                &label,
                subject,
                &head_check_seen,
                line,
                complete,
                head_moved,
            );
            if !malformed.is_empty() {
                reading.line = format!(
                    "{} warning: invalid display fields {}",
                    reading.line,
                    malformed.join(",")
                );
            }
            reading
        }
        Err(error) => Reading::unavailable(&label, &format!("invalid PR journal status: {error}")),
    }
}

/// Cancellation wakes source pauses and interrupts in-flight GitHub reads.
#[derive(Default)]
struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
    cancel: CancelToken,
    event_wake: Mutex<Option<provenant_os::changes::watch::WakeHandle>>,
}
impl Stop {
    fn install_event_wake(&self, wake: provenant_os::changes::watch::WakeHandle) {
        if let Ok(mut slot) = self.event_wake.lock() {
            *slot = Some(wake.clone());
        }
        if self.cancel.is_cancelled() {
            let _ = wake.wake();
        }
    }
    fn stop(&self) {
        self.cancel.cancel();
        if let Ok(slot) = self.event_wake.lock()
            && let Some(wake) = slot.as_ref()
        {
            let _ = wake.wake();
        }
        if let Ok(mut stopped) = self.stopped.lock() {
            *stopped = true;
        }
        self.wake.notify_all();
    }
    fn pause(&self, duration: Duration) -> bool {
        let Ok(stopped) = self.stopped.lock() else {
            return false;
        };
        self.wake
            .wait_timeout_while(stopped, duration, |stopped| !*stopped)
            .is_ok_and(|(stopped, _)| !*stopped)
    }
}

struct Update {
    target: usize,
    reading: Reading,
}

fn graph_observation_paths(
    state: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    graph: &str,
) -> Result<Vec<PathBuf>, String> {
    provenant_fabric_runtime::graph::FileJournal::observation_paths(state, project, graph)
        .map_err(|error| error.to_string())
}

pub(crate) fn observation_text(
    watcher: Option<&provenant_os::changes::watch::Watcher>,
    cadence: &LocalCadence,
    fallback: Option<&str>,
) -> String {
    match watcher {
        Some(watcher) => format!(
            "filesystem events via {}",
            match watcher.backend() {
                provenant_os::changes::watch::Backend::Inotify => "inotify",
                provenant_os::changes::watch::Backend::Kqueue => "kqueue",
                provenant_os::changes::watch::Backend::Unsupported => "unsupported",
            }
        ),
        None => format!(
            "paced reread {}; event source unavailable: {}",
            cadence.every(),
            fallback.unwrap_or("no event source")
        ),
    }
}

fn rows_local<H: std::str::FromStr + PartialEq>(rows: &[Value], host: Option<&H>) -> bool {
    host.is_some_and(|host| {
        rows.iter().all(|row| {
            row["kind"] != "attempt"
                || row["attempt_id"]
                    .as_str()
                    .and_then(|id| id.split_once('/'))
                    .is_none_or(|(owner, _)| owner.parse::<H>().is_ok_and(|owner| &owner == host))
        })
    })
}

fn reading_locally_represented(reading: &Reading, host: Option<&provenant_core::HostId>) -> bool {
    reading.data["rows"]
        .as_array()
        .is_some_and(|rows| rows_local(rows, host))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraphWake {
    Event,
    NativeTimeout,
    StallDeadline,
    PacedFallback,
}

/// Distinguishes native notifications from cancellation checkpoints and paced
/// fallback ticks. Only `PacedFallback` authorizes a state reread without an
/// event; native timeouts merely let callers check cancellation and deadlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationWake {
    Changed,
    NativeTimeout,
    Woken,
    PacedFallback,
    Cancelled,
}

/// One host event source for all local observation paths, with an explicit
/// configured-interval fallback when native registration is unavailable.
pub(crate) struct LocalObservation {
    watcher: Option<provenant_os::changes::watch::Watcher>,
    cadence: LocalCadence,
    fallback: Option<String>,
    has_local_paths: bool,
    next_paced: Option<provenant_core::MonoTime>,
}
impl LocalObservation {
    pub(crate) fn subscribe(paths: &[PathBuf], cadence: LocalCadence) -> Self {
        if paths.is_empty() {
            return Self {
                watcher: None,
                cadence,
                fallback: Some("no local event targets".into()),
                has_local_paths: false,
                next_paced: None,
            };
        }
        match provenant_os::changes::watch::Watcher::subscribe(paths) {
            Ok(watcher) => Self {
                watcher: Some(watcher),
                cadence,
                fallback: None,
                has_local_paths: true,
                next_paced: None,
            },
            Err(error) => Self {
                watcher: None,
                cadence,
                fallback: Some(error.to_string()),
                has_local_paths: true,
                next_paced: None,
            },
        }
    }

    pub(crate) fn paced(cadence: LocalCadence, reason: impl Into<String>) -> Self {
        Self {
            watcher: None,
            cadence,
            fallback: Some(reason.into()),
            has_local_paths: true,
            next_paced: None,
        }
    }

    pub(crate) fn interval(&self) -> Duration {
        self.cadence.interval
    }

    pub(crate) fn report(&self, remote_pending: bool) -> String {
        if !self.has_local_paths {
            return if remote_pending {
                format!(
                    "peer statuses reread {}; no local event targets",
                    self.cadence.every()
                )
            } else {
                "no local event targets".into()
            };
        }
        observation_text_with_remote(
            self.watcher.as_ref(),
            &self.cadence,
            self.fallback.as_deref(),
            remote_pending,
        )
    }

    pub(crate) fn wait(
        &mut self,
        maximum: Duration,
        cancel: &CancelToken,
        clock: &(impl provenant_core::Clock + ?Sized),
    ) -> Result<ObservationWake, provenant_core::TimeError> {
        if cancel.is_cancelled() {
            return Ok(ObservationWake::Cancelled);
        }
        let slice = maximum.min(Duration::from_millis(250));
        let native_deadline = if self.watcher.is_some() {
            Some(
                clock
                    .mono_now()
                    .checked_add(provenant_core::DurationMicros::from_micros(
                        slice.as_micros() as u64,
                    ))?,
            )
        } else {
            None
        };
        if let Some(deadline) = native_deadline {
            let result = match self.watcher.as_mut() {
                Some(watcher) => watcher.wait(slice),
                None => return self.wait_paced(maximum, cancel, clock),
            };
            if cancel.is_cancelled() {
                return Ok(ObservationWake::Cancelled);
            }
            match result {
                Ok(provenant_os::changes::watch::WaitOutcome::Changed) => {
                    return Ok(ObservationWake::Changed);
                }
                Ok(provenant_os::changes::watch::WaitOutcome::TimedOut) => {
                    // Native waits use real kernel time. Advance injected clocks
                    // to the same pre-wait checkpoint without rereading state.
                    if !slice.is_zero() {
                        clock.wait_until(deadline)?;
                    }
                    return Ok(ObservationWake::NativeTimeout);
                }
                Ok(provenant_os::changes::watch::WaitOutcome::Woken) => {
                    return Ok(ObservationWake::Woken);
                }
                Err(error) => {
                    // A failed native source becomes an explicit, reported
                    // paced fallback. Do not reread until its configured tick.
                    self.watcher = None;
                    self.fallback = Some(error.to_string());
                }
            }
        }
        self.wait_paced(maximum, cancel, clock)
    }

    fn wait_paced(
        &mut self,
        maximum: Duration,
        cancel: &CancelToken,
        clock: &(impl provenant_core::Clock + ?Sized),
    ) -> Result<ObservationWake, provenant_core::TimeError> {
        let interval =
            provenant_core::DurationMicros::from_micros(self.cadence.interval.as_micros() as u64);
        let due = match self.next_paced {
            Some(due) => due,
            None => {
                let due = clock.mono_now().checked_add(interval)?;
                self.next_paced = Some(due);
                due
            }
        };
        let now = clock.mono_now();
        if now >= due {
            self.next_paced = Some(now.checked_add(interval)?);
            return Ok(ObservationWake::PacedFallback);
        }

        let requested =
            clock
                .mono_now()
                .checked_add(provenant_core::DurationMicros::from_micros(
                    maximum.as_micros().min(u64::MAX as u128) as u64,
                ))?;
        let wake_at = requested.min(due);
        while !cancel.is_cancelled() {
            let now = clock.mono_now();
            if now >= wake_at {
                break;
            }
            let remaining = wake_at.as_micros().saturating_sub(now.as_micros());
            let slice = Duration::from_micros(remaining).min(Duration::from_millis(250));
            let slice_deadline = now.checked_add(provenant_core::DurationMicros::from_micros(
                slice.as_micros() as u64,
            ))?;
            clock.wait_until(slice_deadline)?;
        }
        if cancel.is_cancelled() {
            return Ok(ObservationWake::Cancelled);
        }
        let now = clock.mono_now();
        if now >= due {
            // Start the next period at this observed tick, preserving a single
            // reread even if a remote wait shortened each preceding call.
            self.next_paced = Some(now.checked_add(interval)?);
            Ok(ObservationWake::PacedFallback)
        } else {
            Ok(ObservationWake::NativeTimeout)
        }
    }
}

fn graph_reads_for_wake(
    pending: &[usize],
    remote: &std::collections::BTreeSet<usize>,
    wake: GraphWake,
) -> Vec<usize> {
    match wake {
        GraphWake::Event | GraphWake::StallDeadline => pending
            .iter()
            .copied()
            .filter(|index| !remote.contains(index))
            .collect(),
        GraphWake::NativeTimeout => Vec::new(),
        GraphWake::PacedFallback => pending.to_vec(),
    }
}

fn include_due_remote_reads(
    reads: &mut Vec<usize>,
    pending: &[usize],
    remote: &std::collections::BTreeSet<usize>,
    remote_poll_due: bool,
) {
    if remote_poll_due {
        let due = pending
            .iter()
            .copied()
            .filter(|index| remote.contains(index) && !reads.contains(index))
            .collect::<Vec<_>>();
        reads.extend(due);
    }
}

fn observation_text_with_remote(
    watcher: Option<&provenant_os::changes::watch::Watcher>,
    cadence: &LocalCadence,
    fallback: Option<&str>,
    remote_pending: bool,
) -> String {
    let mut text = observation_text(watcher, cadence, fallback);
    if remote_pending {
        text.push_str(&format!(
            "; peer/relay or unrepresented rows reread {} because the relay has no remote event subscription",
            cadence.every()
        ));
    }
    text
}

#[derive(Default)]
struct ObservationHistory {
    remote_source_seen: bool,
}
impl ObservationHistory {
    fn record(&mut self, reading: &Reading, host: Option<&provenant_core::HostId>) {
        self.remote_source_seen |= !reading_locally_represented(reading, host);
    }

    fn render(
        &self,
        watcher: Option<&provenant_os::changes::watch::Watcher>,
        cadence: &LocalCadence,
        fallback: Option<&str>,
    ) -> String {
        observation_text_with_remote(watcher, cadence, fallback, self.remote_source_seen)
    }
}

/// Only terminal node/attempt states produce settlement events. In particular
/// pending -> running is progress, not a reason for --any to return.
fn digest_settle(digest: &Value) -> Option<Settle> {
    if digest["settled"] == true {
        return Some(if digest["outcome"] == "succeeded" {
            Settle::Succeeded
        } else {
            Settle::Unsuccessful
        });
    }
    match digest["state"].as_str()? {
        "succeeded" => Some(Settle::Succeeded),
        "failed" | "blocked" | "skipped" | "unreadable" => Some(Settle::Unsuccessful),
        "terminal" => Some(if digest["outcome"] == "succeeded" {
            Settle::Succeeded
        } else {
            Settle::Unsuccessful
        }),
        _ => None,
    }
}

fn new_settlements<'a>(previous: Option<&Reading>, reading: &'a Reading) -> Vec<&'a Value> {
    reading.data["digests"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|digest| {
            digest_settle(digest).is_some()
                && !previous.is_some_and(|previous| {
                    previous.data["digests"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|old| old["key"] == digest["key"] && digest_settle(old).is_some())
                })
        })
        .collect()
}

fn settlement_verdict(digests: &[&Value]) -> Option<Settle> {
    if digests
        .iter()
        .any(|digest| digest_settle(digest) == Some(Settle::Unsuccessful))
    {
        Some(Settle::Unsuccessful)
    } else if !digests.is_empty() {
        Some(Settle::Succeeded)
    } else {
        None
    }
}

/// Whether the wait is done: under `any`, once one target settled or needs
/// input; otherwise once every target has.
fn wait_done(latest: &[Option<Reading>], settled: &[usize], mode: WaitMode) -> bool {
    match mode {
        WaitMode::Any => !settled.is_empty(),
        WaitMode::All => latest
            .iter()
            .all(|reading| reading.as_ref().is_some_and(Reading::done)),
        WaitMode::FailFast => {
            latest
                .iter()
                .flatten()
                .any(|reading| reading.settle == Settle::Unsuccessful)
                || latest
                    .iter()
                    .all(|reading| reading.as_ref().is_some_and(Reading::done))
        }
    }
}

/// The parts of a reading that say what was observed, without its staleness.
fn observed(reading: &Reading) -> (Settle, &str, Value) {
    let mut data = reading.data.clone();
    if let Some(fields) = data.as_object_mut() {
        fields.remove("stale");
    }
    (reading.settle, reading.line.as_str(), data)
}

/// Fold one reading into the wait. A failed read keeps the last snapshot,
/// marked stale. `changed` sees each distinct observation once, and `settled`
/// records the order targets finished in.
fn absorb(
    latest: &mut [Option<Reading>],
    settled: &mut Vec<usize>,
    mode: WaitMode,
    mut update: Update,
    changed: &mut impl FnMut(usize, &Reading),
) {
    let index = update.target;
    if update.reading.data.is_null()
        && let Some(previous) = latest[index]
            .as_ref()
            .filter(|reading| reading.data.is_object())
    {
        update.reading.data = previous.data.clone();
        update.reading.data["stale"] = json!(true);
    }
    if latest[index]
        .as_ref()
        .is_some_and(|previous| observed(previous) == observed(&update.reading))
    {
        latest[index] = Some(update.reading);
        return;
    }
    let node_verdict = latest[index]
        .as_ref()
        .and_then(|previous| settlement_verdict(&new_settlements(Some(previous), &update.reading)));
    changed(index, &update.reading);
    if !update.reading.done()
        && let Some(verdict) = node_verdict
    {
        match mode {
            WaitMode::Any => update.reading.settle = verdict,
            WaitMode::FailFast if verdict == Settle::Unsuccessful => {
                update.reading.settle = verdict;
            }
            WaitMode::All | WaitMode::FailFast => {}
        }
    }
    if update.reading.done() && !settled.contains(&index) {
        settled.push(index);
    }
    latest[index] = Some(update.reading);
}

/// Fold source readings until the wait is done. Returns false when the
/// deadline passes first, leaving unsettled readings marked stale.
fn collect(
    receiver: &mpsc::Receiver<Update>,
    latest: &mut [Option<Reading>],
    settled: &mut Vec<usize>,
    deadline: Instant,
    mode: WaitMode,
    changed: &mut impl FnMut(usize, &Reading),
) -> bool {
    loop {
        if wait_done(latest, settled, mode) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(update) = receiver.recv_timeout(left) else {
            for reading in latest.iter_mut().flatten() {
                if !reading.done() && reading.data.is_object() {
                    reading.data["stale"] = json!(true);
                }
            }
            return false;
        };
        absorb(latest, settled, mode, update, changed);
    }
}

/// Expiry ends blocking observation; then each unsettled local graph target
/// gets one final nonblocking snapshot of its local store, so a settlement
/// after the last sample (event or paced tick) is reported, not a timeout.
/// Pull requests and peer or relayed rows keep their last reading: remote
/// reads use only the original budget. Returns whether the wait is now done.
fn final_snapshot(
    sources: &[Source],
    latest: &mut [Option<Reading>],
    settled: &mut Vec<usize>,
    mode: WaitMode,
    host: Option<&provenant_core::HostId>,
    mut read_local: impl FnMut(&provenant_cli::Target) -> Option<Reading>,
    changed: &mut impl FnMut(usize, &Reading),
) -> bool {
    for (index, source) in sources.iter().enumerate() {
        if wait_done(latest, settled, mode) {
            break;
        }
        let Source::Graph(target) = source else {
            continue;
        };
        let local = latest[index].as_ref().is_none_or(|reading| {
            !reading.done()
                && (!reading.data.is_object() || reading_locally_represented(reading, host))
        });
        if !local {
            continue;
        }
        if let Some(reading) = read_local(target) {
            absorb(
                latest,
                settled,
                mode,
                Update {
                    target: index,
                    reading,
                },
                changed,
            );
        }
    }
    wait_done(latest, settled, mode)
}

/// One local graph target's final snapshot; `None` when the local store cannot
/// answer without blocking.
fn final_read(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<provenant_cli::fabric::Context>,
) -> Option<Reading> {
    provenant_sqlite::with_busy_timeout(Duration::ZERO, || {
        crate::fabric::final_status_rows(target, edge, session)
    })
    .ok()
    .map(|rows| classify_rows(&target.to_string(), &rows))
}

/// One forge request for a pull request read: the wait's absolute deadline,
/// capped by `domains.wait.pr_read_timeout_seconds`, and the waiter's
/// cancellation, so a target that settles elsewhere interrupts the transport.
fn forge_request(
    deadline: Instant,
    cancel: &CancelToken,
    pr_read_timeout: Duration,
) -> provenant_config::ports::Request {
    provenant_config::ports::Request::new(deadline, cancel.clone()).capped(pr_read_timeout)
}

/// The local paced-reread cadence for owner-side waits, resolved through the
/// one wait settings policy. An unreadable or malformed configuration warns and
/// uses the compiled defaults rather than refusing the wait.
pub(crate) fn local_cadence(
    roots: &provenant_core::Roots,
) -> Result<LocalCadence, provenant_fabric_runtime::coordination::facade::FacadeError> {
    use provenant_fabric_runtime::coordination::facade::{FacadeError, InvalidInputDetail};
    let (settings, origin) = provenant_cli::wait::Settings::resolve(roots).map_err(|error| {
        FacadeError::invalid(InvalidInputDetail::Configuration(error.to_string()))
    })?;
    Ok(origin.local_cadence(&settings))
}

/// How the wait ended: a timeout, else the worst of the targets that decided
/// it. Under `any` only the first target to finish decides; fail-fast returns
/// on the first unsuccessful target, otherwise it reports all final results.
fn outcome(latest: &[Option<Reading>], settled: &[usize], done: bool, mode: WaitMode) -> Outcome {
    if !done {
        return Outcome::Timeout;
    }
    let deciding: Vec<_> = match mode {
        WaitMode::Any => settled.iter().copied().take(1).collect(),
        WaitMode::All => settled.to_vec(),
        WaitMode::FailFast => {
            let failures: Vec<_> = settled
                .iter()
                .copied()
                .filter(|index| {
                    latest[*index]
                        .as_ref()
                        .is_some_and(|reading| reading.settle == Settle::Unsuccessful)
                })
                .take(1)
                .collect();
            if failures.is_empty() {
                settled.to_vec()
            } else {
                failures
            }
        }
    };
    let settles: Vec<_> = deciding
        .iter()
        .filter_map(|index| latest[*index].as_ref().map(|reading| reading.settle))
        .collect();
    if settles.contains(&Settle::Unsuccessful) {
        Outcome::Failed
    } else if settles.contains(&Settle::Stalled) {
        Outcome::Stalled
    } else if settles.contains(&Settle::NeedsInput) {
        Outcome::Pending
    } else {
        Outcome::Ok
    }
}

/// The settle verdict for a child row from the owner relay. A settled node is
/// either `succeeded` or one of the terminal unsuccessful states; only a node
/// the relay still reports as running stays pending.
fn child_settle(child: &Value) -> Settle {
    if child["settled"] != true {
        return Settle::Pending;
    }
    if child["state"] == "succeeded" {
        Settle::Succeeded
    } else {
        Settle::Unsuccessful
    }
}

/// The verdict for a child wait is drawn from exactly the children the mode
/// decided on: the first settled child under `--any`, the first failed child
/// under `--fail-fast`, and every child otherwise. A child still running when
/// the wait ends early is not a failure.
fn child_wait_outcome(children: &[Value], mode: WaitMode, settled_order: &[usize]) -> Outcome {
    let verdicts: Vec<Settle> = match mode {
        WaitMode::Any => settled_order
            .iter()
            .take(1)
            .filter_map(|&index| children.get(index).map(child_settle))
            .collect(),
        WaitMode::FailFast => match settled_order.iter().copied().find(|&index| {
            children
                .get(index)
                .is_some_and(|child| child_settle(child) == Settle::Unsuccessful)
        }) {
            Some(index) => vec![child_settle(&children[index])],
            None => children.iter().map(child_settle).collect(),
        },
        WaitMode::All => children.iter().map(child_settle).collect(),
    };
    if verdicts.contains(&Settle::Unsuccessful) {
        Outcome::Failed
    } else {
        Outcome::Ok
    }
}

/// A wait inside a lane that names no targets observes the parent's children
/// through the owner relay. Those terminal child states are the wait's targets:
/// emit the same settlement events and terminal envelope a named-target wait
/// emits. Discarding them would read a failed child as success and leave
/// `wait --json` with no terminal result at all. Settlement events are streamed
/// by the polling callback so `--follow` sees transitions as they happen; this
/// builds only the terminal envelope and its final rows.
fn emit_child_wait(
    children: &[Value],
    cadence: &LocalCadence,
    mode: WaitMode,
    globals: &Globals,
    settled_order: &[usize],
    write_error: Option<std::io::Error>,
) -> u8 {
    let observation = provenant_cli::fabric::graph_relay::child_wait_observation(cadence);
    let mut human = mode_heading(mode);
    let mut rows = Vec::new();
    let mut summary_lines = Vec::new();
    for child in children {
        let label = child["node"].as_str().unwrap_or("child").to_owned();
        let settle = child_settle(child);
        let line = format!("{label} {}", settle_phrase(settle, true));
        human.push_str(&line);
        human.push('\n');
        summary_lines.push(line.clone());
        rows.push(json!({
            "target": label,
            "settle": settle,
            "line": line,
            "data": child,
        }));
    }
    if children.is_empty() {
        human.push_str("nothing to wait for: no child work\n");
    }
    let states = if summary_lines.is_empty() {
        "nothing to wait for".to_owned()
    } else {
        summary_lines.join(", ")
    };
    if let Some(error) = write_error {
        return crate::fail(
            "wait",
            Outcome::Error,
            "IO",
            &error.to_string(),
            "check the reported output path and its permissions, then rerun `provenant wait`",
            globals,
        );
    }
    let outcome = child_wait_outcome(children, mode, settled_order);
    let summary =
        provenant_cli::Summary::new(mode_heading(mode).trim_end().to_owned(), summary_lines);
    let step =
        provenant_records::command_result::NextStep::from_command("provenant status", states);
    crate::emit_summarised(
        &provenant_records::CommandResult::new(
            "wait",
            outcome,
            terminal_data(mode, rows, false, Some(&observation)),
        )
        .with_next(vec![step]),
        globals,
        &human,
        true,
        Some(&summary),
    )
}

fn preflight_error(
    error: &(dyn std::error::Error + 'static),
    deadline: Instant,
    cancel: &CancelToken,
    globals: &Globals,
) -> u8 {
    if cancel.is_cancelled() {
        crate::fail(
            "wait",
            Outcome::Cancelled,
            "CANCELLED",
            "wait cancelled; the work continues",
            "run `provenant status` to see the work, then rerun `provenant wait` to resume observing",
            globals,
        )
    } else if Instant::now() >= deadline {
        crate::fail(
            "wait",
            Outcome::Timeout,
            "TIMEOUT",
            "wait timed out; the work continues",
            "rerun `provenant wait` with a longer `--timeout`, or check `provenant status`; the work continues",
            globals,
        )
    } else {
        crate::fabric::command_fail("wait", error, globals)
    }
}

pub(crate) fn run(
    command: provenant_cli::Wait,
    globals: &Globals,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
) -> u8 {
    use crate::roots::{Product, Workspace};
    let mode = WaitMode::from_command(&command);
    let resolved = match crate::roots::resolve(globals, Workspace::Project, Product::Required) {
        Ok(resolved) => resolved,
        Err(error) => {
            return crate::fail(
                "wait",
                Outcome::Error,
                "ROOT",
                &error.to_string(),
                "run from a project checkout or pass `--project PATH`",
                globals,
            );
        }
    };
    let (settings, origin) = match provenant_cli::wait::Settings::resolve(&resolved.roots) {
        Ok(resolved) => resolved,
        Err(error) => {
            return crate::fail(
                "wait",
                Outcome::Error,
                "CONFIG",
                &error.to_string(),
                "run `provenant check config --json` and correct the reported wait setting",
                globals,
            );
        }
    };
    let edge = provenant_cli::run::Edge {
        roots: &resolved.roots,
        home: &resolved.home,
        assets: &resolved.assets,
        cwd: &resolved.project,
    };
    // A `--pr --until complete` wait must outlast a normal CI run, so it uses
    // its own configured default budget. An explicit --timeout still wins and
    // every budget shares the same maximum.
    let complete = matches!(command.until, Some(provenant_cli::PrWaitUntil::Complete));
    let budget = if complete {
        settings.complete_timeout(command.timeout, &origin)
    } else {
        settings.timeout(command.timeout, &origin)
    };
    let deadline = Instant::now() + Duration::from_secs(budget);
    // Preflight reads tolerate a momentarily busy store instead of failing a
    // wait the instant another writer holds a lock. Capped by the wait's own
    // deadline, and configurable through `domains.wait.preflight_busy_timeout_ms`.
    let preflight_busy = Duration::from_millis(settings.preflight_busy_timeout_ms)
        .min(deadline.saturating_duration_since(Instant::now()));
    let stop = Stop::default();
    let cadence = origin.local_cadence(&settings);
    let interval = cadence.interval;
    let mut sources: Vec<Source> = command.targets.iter().cloned().map(Source::Graph).collect();

    // Subscribe before the first status read so settlement during preflight
    // remains queued for the single local graph worker.
    let project = if provenant_cli::fabric::in_lane() {
        Err(
            "lane targets are read through their owner; local event files are unavailable"
                .to_owned(),
        )
    } else {
        provenant_cli::fabric::Context::open_read_only(
            resolved.roots.clone(),
            &resolved.home,
            &resolved.project,
        )
        .map(|context| (context.identity.project, context.identity.host))
        .map_err(|error| format!("cannot resolve project event logs: {error}"))
    };
    let local_host = project.as_ref().ok().map(|(_, host)| *host);
    let project_id = project.as_ref().ok().map(|(id, _)| *id);
    let project_error = project.as_ref().err().cloned();
    let mut pending_index_error = None;
    let mut observation_path_error = None;
    let event_paths = match project {
        Ok((project, _)) if !command.targets.is_empty() => {
            let paths = command
                .targets
                .iter()
                .map(|target| graph_observation_paths(&resolved.roots.state, project, &target.name))
                .collect::<Result<Vec<_>, _>>();
            match paths {
                Ok(paths) => paths.into_iter().flatten().collect(),
                Err(error) => {
                    observation_path_error =
                        Some(format!("cannot resolve graph observation paths: {error}"));
                    Vec::new()
                }
            }
        }
        Ok((project, _)) if command.prs.is_empty() => {
            let names = provenant_fabric_runtime::graph::FileJournal::pending_graph_ids(
                &resolved.roots.state,
                project,
            )
            .map_err(|error| format!("cannot resolve pending graph events: {error}"));
            match names {
                Ok(names) => {
                    let names = names
                        .into_iter()
                        .take(settings.max_targets as usize)
                        .collect::<Vec<_>>();
                    match names
                        .iter()
                        .map(|name| graph_observation_paths(&resolved.roots.state, project, name))
                        .collect::<Result<Vec<_>, _>>()
                    {
                        Ok(paths) => paths.into_iter().flatten().collect(),
                        Err(error) => {
                            observation_path_error =
                                Some(format!("cannot resolve graph observation paths: {error}"));
                            Vec::new()
                        }
                    }
                }
                Err(error) => {
                    pending_index_error = Some(error);
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    };
    // Capture the shared journal cursor before initial reconciliation. Only
    // values cross the worker boundary; SQLite connections remain thread local.
    let graph_subscription = project.as_ref().ok().and_then(|(project, _)| {
        crate::event_delivery::Journal::tip(&resolved.roots, *project)
            .ok()
            .map(|cursor| (resolved.roots.clone(), *project, cursor))
    });
    let event_paths = if graph_subscription.is_some() && !event_paths.is_empty() {
        vec![
            resolved
                .roots
                .state
                .as_path()
                .join(provenant_core::changes::FILE_NAME),
        ]
    } else {
        event_paths
    };
    let setup_fallback = project_error
        .or(pending_index_error)
        .or(observation_path_error);
    let (mut event_watcher, initial_fallback) = if event_paths.is_empty() {
        (
            None,
            setup_fallback.unwrap_or_else(|| "no local graph targets".to_owned()),
        )
    } else {
        match provenant_os::changes::watch::Watcher::subscribe(&event_paths) {
            Ok(watcher) => (Some(watcher), String::new()),
            Err(error) => (None, setup_fallback.unwrap_or_else(|| error.to_string())),
        }
    };
    let fallback_reason = event_watcher.is_none().then(|| initial_fallback.clone());
    if let Some(watcher) = &event_watcher {
        stop.install_event_wake(watcher.wake_handle());
    }
    let observation = Arc::new(Mutex::new(observation_text_with_remote(
        event_watcher.as_ref(),
        &cadence,
        fallback_reason.as_deref(),
        false,
    )));
    let mut validation = std::collections::BTreeMap::new();
    if let Err(error) = provenant_sqlite::with_busy_timeout(preflight_busy, || {
        crate::fabric::check_targets(
            &command.targets,
            &edge,
            deadline,
            &stop.cancel,
            |target, rows| {
                validation.insert(
                    target.to_string(),
                    classify_rows(&target.to_string(), &rows),
                );
            },
        )
    }) {
        return preflight_error(error.as_ref(), deadline, &stop.cancel, globals);
    }
    if command.targets.is_empty() && command.prs.is_empty() {
        if provenant_cli::fabric::in_lane() {
            provenant_records::warning::warn(
                provenant_records::warning::Warning::new(
                    provenant_records::warning::WAIT_TARGET_FALLBACK,
                    "[WAIT_TARGET_FALLBACK] wait inside a lane named no targets; waiting on this lane's children through the owner relay",
                 provenant_records::warning::detected_source(provenant_records::warning::WAIT_TARGET_FALLBACK))
                .once_per_change(),
            );
            // The local project journal is unavailable inside a lane and the
            // project-wide unsettled set would include this lane's own pending
            // graph, so an all-mode wait could never settle. Discover the
            // lane's children through the relay and wait for those instead,
            // ending on and reporting exactly the mode the caller selected.
            let within = deadline.saturating_duration_since(Instant::now());
            let child_mode = match mode {
                WaitMode::Any => provenant_cli::fabric::graph_relay::ChildWaitMode::Any,
                WaitMode::FailFast => provenant_cli::fabric::graph_relay::ChildWaitMode::FailFast,
                WaitMode::All => provenant_cli::fabric::graph_relay::ChildWaitMode::All,
            };
            let mut announced: std::collections::HashMap<String, Settle> = Default::default();
            let mut settled_order: Vec<usize> = Vec::new();
            let mut write_error = None;
            let mut observe = |children: &[Value]| {
                for (index, child) in children.iter().enumerate() {
                    let settle = child_settle(child);
                    // Without --follow only settlements are events; with it,
                    // every newly observed state is a transition, as for named
                    // targets.
                    if !command.follow && settle == Settle::Pending {
                        continue;
                    }
                    let label = child["node"].as_str().unwrap_or("child").to_owned();
                    if announced.get(&label) == Some(&settle) {
                        continue;
                    }
                    announced.insert(label.clone(), settle);
                    if settle != Settle::Pending && !settled_order.contains(&index) {
                        settled_order.push(index);
                    }
                    if write_error.is_some() {
                        continue;
                    }
                    let line = format!("{label} {}", settle_phrase(settle, true));
                    let event = if settle == Settle::Pending {
                        "transition"
                    } else {
                        "settled"
                    };
                    let event = if globals.json {
                        json!({"event":event,"target":label,"settle":settle,"line":line})
                            .to_string()
                    } else {
                        line
                    };
                    write_error = write_line(&event).err();
                }
            };
            let waited = provenant_cli::fabric::graph_relay::wait_for_children(
                &stop.cancel,
                within,
                cadence.interval,
                child_mode,
                &mut observe,
            );
            drop(observe);
            return match waited {
                Ok(settled_children) => {
                    let children = settled_children["children"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    emit_child_wait(
                        &children,
                        &cadence,
                        mode,
                        globals,
                        &settled_order,
                        write_error,
                    )
                }
                Err(error) => match error.code() {
                    "cancelled" => crate::fail(
                        "wait",
                        Outcome::Cancelled,
                        "CANCELLED",
                        "wait cancelled; the work continues",
                        "run `provenant status` to see the children, then rerun `provenant wait`",
                        globals,
                    ),
                    "timeout" => crate::fail(
                        "wait",
                        Outcome::Timeout,
                        "TIMEOUT",
                        "wait timed out; the work continues",
                        "rerun `provenant wait` with a longer `--timeout` or check `provenant status`",
                        globals,
                    ),
                    _ => crate::fail(
                        "wait",
                        Outcome::Error,
                        "CHILD_WAIT",
                        &format!("cannot wait on this lane's children: {error}"),
                        "check the lane relay and rerun `provenant wait`",
                        globals,
                    ),
                },
            };
        }
        match provenant_sqlite::with_busy_timeout(preflight_busy, || {
            crate::fabric::unsettled_graphs(&edge, deadline, &stop.cancel)
        }) {
            Ok(graphs) => {
                for graph in graphs {
                    if graph.error.is_none() {
                        validation.insert(
                            graph.target.to_string(),
                            classify_rows(&graph.target.to_string(), &graph.rows),
                        );
                    }
                    sources.push(match graph.error {
                        Some(error) => Source::UnreadableGraph(graph.target, error),
                        None => Source::Graph(graph.target),
                    });
                }
            }
            Err(error) => return preflight_error(error.as_ref(), deadline, &stop.cancel, globals),
        }
    }
    let mut prs = command.prs.clone();
    prs.sort_unstable();
    prs.dedup();
    let pr_read_timeout = Duration::from_secs(settings.pr_read_timeout_seconds);
    let actions_grace = Duration::from_secs(settings.actions_grace_seconds);
    // One resolution per wait: the project row the wait resolves its
    // repository from beats the host section, and every sourced report cites
    // the key that supplied the effective value.
    let project_key = project_id.map(|id| id.to_string());
    let head = settings.effective_pr_head(project_key.as_deref());
    let pr_head_policy = command.pr_head_policy.unwrap_or(head.policy);
    let pr_policy_source = if command.head.is_some() {
        provenant_records::Source::at(provenant_records::Layer::Launch, "--head")
    } else if command.pr_head_policy.is_some() {
        provenant_records::Source::at(provenant_records::Layer::Launch, "--pr-head-policy")
    } else {
        origin.source(&head.policy_key)
    };
    let head_source_source = origin.describe(&head.source_key);
    let unpinned_source = origin.describe(&head.unpinned_key);
    if sources.len() + prs.len() > settings.max_targets as usize {
        return crate::fail(
            "wait",
            Outcome::Invalid,
            "TOO_MANY_TARGETS",
            &format!("wait takes at most {} targets", settings.max_targets),
            "remove targets or raise `domains.wait.max_targets`, then rerun `provenant wait`",
            globals,
        );
    }
    if !prs.is_empty() {
        let request = forge_request(deadline, &stop.cancel, pr_read_timeout);
        let repository =
            crate::pr::status::wait_repository(edge.roots.state.as_path(), edge.cwd, &request)
                .map_err(|error| error.to_string());
        sources.extend(prs.into_iter().map(|number| {
            let forge = if command.head.is_some() {
                None
            } else {
                repository.as_ref().ok().and_then(|repository| {
                    crate::pr::status::wait_head(
                        edge.roots.state.as_path(),
                        edge.cwd,
                        repository,
                        number,
                        &request,
                    )
                    .ok()
                    .map(|reported| (reported.sha, reported.branch))
                })
            };
            let local_tip = forge
                .as_ref()
                .and_then(|(_, branch)| local_branch_tip(edge.roots.state.as_path(), edge.cwd, branch));
            if let (Some((sha, branch)), Some(local)) = (&forge, &local_tip)
                && local != sha
            {
                let action = if head.source == PrHeadSource::Forge {
                    "ignoring the local tip"
                } else {
                    "waiting for the local tip"
                };
                warning::warn(
                    warning::Warning::new(
                        warning::BRANCH_BEHIND_BASE,
                        format!("#{number}: local tip {local} differs from GitHub head {sha}; {action}"), provenant_records::warning::detected_source(warning::BRANCH_BEHIND_BASE),)
                    .subject(branch.clone()),
                );
            }
            if head.source == PrHeadSource::Local
                && command.head.is_none()
                && local_tip.is_none()
                && forge.is_some()
            {
                warn_local_head_unavailable(number, &head_source_source);
            }
            let unpinned = command.head.is_none() && forge.is_none();
            let subject = pr_subject(
                number,
                &repository,
                pr_head_policy,
                head.source,
                &pr_policy_source,
                wall_now(),
                command.head.as_deref(),
                forge,
                local_tip,
            );
            if unpinned {
                if head.unpinned == PrHeadUnpinned::Refuse {
                    validation.insert(
                        format!("#{number}"),
                        unpinned_refusal(number, &unpinned_source),
                    );
                } else {
                    warn(
                        warning::PR_HEAD_UNPINNED,
                        format!("#{number}: forge head unavailable; the first observation pins the subject ({unpinned_source})"),
                    );
                }
            }
            Source::Pr {
                number,
                repository: repository.clone(),
                subject: subject.map(Box::new),
                pinned_head_seen: false,
                policy: pr_head_policy,
                policy_source: pr_policy_source.clone(),
                complete,
            }
        }));
    }
    let mut latest: Vec<Option<Reading>> = sources
        .iter()
        .map(|source| validation.get(&source.label()).cloned())
        .collect();
    let mut settled: Vec<usize> = latest
        .iter()
        .enumerate()
        .filter_map(|(index, reading)| reading.as_ref().is_some_and(Reading::done).then_some(index))
        .collect();
    let mut write_error = None;
    let mut announced_targets = std::collections::BTreeSet::new();
    let mut announced_nodes = std::collections::BTreeSet::new();
    let mut observed_targets = std::collections::BTreeSet::new();
    let mut transition = |index: usize, reading: &Reading| {
        let target_settled = reading.done() && announced_targets.insert(index);
        let first_sample = observed_targets.insert(index);
        let node_settled: Vec<_> = new_settlements(None, reading)
            .into_iter()
            .filter(|digest| {
                let newly_observed = announced_nodes.insert((index, digest["key"].to_string()));
                newly_observed && (!first_sample || target_settled)
            })
            .collect();
        let event = if target_settled {
            "settled"
        } else if !node_settled.is_empty() {
            "node_settled"
        } else {
            "transition"
        };
        if (target_settled || !node_settled.is_empty() || command.follow) && write_error.is_none() {
            let line = if globals.json {
                json!({"event":event,"target":sources[index].label(),
                    "settle":settlement_verdict(&node_settled).unwrap_or(reading.settle),"line":reading.line,"digests":reading.data["digests"]})
                .to_string()
            } else {
                let mut line = format!("{event} {}", reading.line);
                for digest in reading.data["digests"].as_array().into_iter().flatten() {
                    if let Some(digest_line) = digest["line"].as_str() {
                        line.push('\n');
                        line.push_str(digest_line);
                    }
                }
                line
            };
            write_error = write_line(&line).err();
        }
    };
    for (index, reading) in latest.iter().enumerate() {
        if let Some(reading) = reading {
            transition(index, reading);
        }
    }
    let (sender, receiver) = mpsc::sync_channel(128);
    let graph_indices = sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| match source {
            Source::Graph(_) | Source::UnreadableGraph(..) => Some(index),
            Source::Pr { .. } => None,
        })
        .collect::<Vec<_>>();
    let done = std::thread::scope(|scope| {
        if !graph_indices.is_empty() {
            let sender = sender.clone();
            let edge = &edge;
            let stop = &stop;
            let observation = Arc::clone(&observation);
            let cadence = &cadence;
            let pending = graph_indices
                .iter()
                .copied()
                .filter(|index| {
                    latest[*index]
                        .as_ref()
                        .is_none_or(|reading| !reading.done())
                })
                .collect::<Vec<_>>();
            let initial_reads = pending
                .iter()
                .copied()
                .filter(|index| latest[*index].is_none())
                .collect::<Vec<_>>();
            let graph_sources = graph_indices
                .iter()
                .map(|index| (*index, sources[*index].clone()))
                .collect::<Vec<_>>();
            let mut watcher = event_watcher.take();
            let mut fallback_reason = fallback_reason.clone();
            let mut remote_indices = pending
                .iter()
                .copied()
                .filter(|index| {
                    latest[*index].as_ref().is_none_or(|reading| {
                        !reading_locally_represented(reading, local_host.as_ref())
                    })
                })
                .collect::<std::collections::BTreeSet<_>>();
            let mut observation_history = ObservationHistory::default();
            let mut sampled_readings = pending
                .iter()
                .filter_map(|index| latest[*index].clone().map(|reading| (*index, reading)))
                .collect::<std::collections::BTreeMap<_, _>>();
            for index in &pending {
                if let Some(reading) = latest[*index].as_ref() {
                    observation_history.record(reading, local_host.as_ref());
                }
            }
            if let Ok(mut current) = observation.lock() {
                *current = observation_history.render(
                    watcher.as_ref(),
                    cadence,
                    fallback_reason.as_deref(),
                );
            }
            scope.spawn(move || {
                let mut journal = graph_subscription.and_then(|(roots, project, cursor)|
                    crate::event_delivery::Journal::resume_graphs(roots, project, cursor).ok());
                let mut last_graph_reconcile = Instant::now();
                let mut session = None;
                let mut observation_history = observation_history;
                let mut pending = pending;
                // Read once for rows that arrived after preflight or were not
                // represented in its sample before waiting for an event.
                for index in initial_reads {
                    let Some((_, source)) = graph_sources
                        .iter()
                        .find(|(candidate, _)| *candidate == index)
                    else {
                        continue;
                    };
                    let reading = read(
                        source,
                        edge,
                        &mut session,
                        deadline,
                        pr_read_timeout,
                        &stop.cancel,
                        workers,
                    );
                    observation_history.record(&reading, local_host.as_ref());
                    sampled_readings.insert(index, reading.clone());
                    let locally_represented =
                        reading_locally_represented(&reading, local_host.as_ref());
                    if locally_represented {
                        remote_indices.remove(&index);
                    } else {
                        remote_indices.insert(index);
                    }
                    let finished = reading.done();
                    if finished {
                        remote_indices.remove(&index);
                        pending.retain(|candidate| *candidate != index);
                    }
                    if let Ok(mut current) = observation.lock() {
                        *current = observation_history.render(
                            watcher.as_ref(),
                            cadence,
                            fallback_reason.as_deref(),
                        );
                    }
                    if sender
                        .send(Update {
                            target: index,
                            reading,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                let mut next_remote_poll = Instant::now() + interval;
                while !pending.is_empty() && !stop.cancel.is_cancelled() {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    let stall_after = pending
                        .iter()
                        .filter(|index| !remote_indices.contains(*index))
                        .filter_map(|index| sampled_readings.get(index).cloned())
                        .collect::<Vec<_>>();
                    let stall_deadline = local_stall_wait(&stall_after, SystemTime::now())
                        .map(|after| Instant::now() + after);
                    let until_stall = stall_deadline
                        .map(|due| due.saturating_duration_since(Instant::now()))
                        .unwrap_or(left);
                    let wake = if let Some(source) = watcher.as_mut() {
                        let until_remote = if remote_indices.is_empty() {
                            left
                        } else {
                            next_remote_poll.saturating_duration_since(Instant::now())
                        };
                        let wait_for = left
                            .min(until_remote)
                            .min(Duration::from_secs(60).saturating_sub(last_graph_reconcile.elapsed()))
                            .min(until_stall);
                        match source.wait(wait_for) {
                            Ok(provenant_os::changes::watch::WaitOutcome::Changed) => {
                                Some(GraphWake::Event)
                            }
                            Ok(provenant_os::changes::watch::WaitOutcome::TimedOut) => {
                                if deadline.saturating_duration_since(Instant::now()).is_zero() {
                                    break;
                                }
                                Some(timeout_wake(Instant::now(), stall_deadline))
                            }
                            Ok(provenant_os::changes::watch::WaitOutcome::Woken) => break,
                            Err(error) => {
                                watcher = None;
                                fallback_reason = Some(format!("event source failed: {error}"));
                                if let Ok(mut current) = observation.lock() {
                                    *current = observation_history.render(
                                        None,
                                        cadence,
                                        fallback_reason.as_deref(),
                                    );
                                }
                                let left = deadline.saturating_duration_since(Instant::now());
                                if !stop.pause(interval.min(left).min(until_stall)) {
                                    break;
                                }
                                Some(if stall_deadline.is_some_and(|due| Instant::now() >= due) {
                                    GraphWake::StallDeadline
                                } else {
                                    GraphWake::PacedFallback
                                })
                            }
                        }
                    } else {
                        if !stop.pause(interval.min(left).min(until_stall)) {
                            break;
                        }
                        Some(if stall_deadline.is_some_and(|due| Instant::now() >= due) {
                            GraphWake::StallDeadline
                        } else {
                            GraphWake::PacedFallback
                        })
                    };
                    let Some(wake) = wake else {
                        continue;
                    };
                    // A local event may race with the remote cadence boundary.
                    // Include remote targets whenever that absolute deadline has
                    // elapsed, regardless of how often local events arrive.
                    let remote_poll_due =
                        !remote_indices.is_empty() && Instant::now() >= next_remote_poll;
                    let mut reads = graph_reads_for_wake(&pending, &remote_indices, wake);
                    let gap_due = last_graph_reconcile.elapsed() >= Duration::from_secs(60);
                    if let Some(source) = journal.as_mut() {
                        match source.drain_graphs() {
                            Ok(graphs) if !gap_due => reads.retain(|index| {
                                remote_indices.contains(index) || graph_sources.iter().any(|(candidate, source)|
                                    candidate == index && matches!(source, Source::Graph(target) if graphs.contains(&target.name)))
                            }),
                            Ok(_) => { reads = pending.clone(); last_graph_reconcile = Instant::now(); }
                            Err(_) => { journal = None; reads = pending.clone(); last_graph_reconcile = Instant::now(); }
                        }
                    } else if gap_due {
                        reads = pending.clone();
                        last_graph_reconcile = Instant::now();
                    }
                    include_due_remote_reads(
                        &mut reads,
                        &pending,
                        &remote_indices,
                        remote_poll_due,
                    );
                    let polled_remote = reads.iter().any(|index| remote_indices.contains(index));
                    for index in reads {
                        if stop.cancel.is_cancelled() {
                            break;
                        }
                        let Some((_, source)) = graph_sources
                            .iter()
                            .find(|(candidate, _)| *candidate == index)
                        else {
                            continue;
                        };
                        let reading = read(
                            source,
                            edge,
                            &mut session,
                            deadline,
                            pr_read_timeout,
                            &stop.cancel,
                            workers,
                        );
                        sampled_readings.insert(index, reading.clone());
                        observation_history.record(&reading, local_host.as_ref());
                        let locally_represented =
                            reading_locally_represented(&reading, local_host.as_ref());
                        if locally_represented {
                            remote_indices.remove(&index);
                        } else {
                            remote_indices.insert(index);
                        }
                        let finished = reading.done();
                        if finished {
                            remote_indices.remove(&index);
                            pending.retain(|candidate| *candidate != index);
                        }
                        if let Ok(mut current) = observation.lock() {
                            *current = observation_history.render(
                                watcher.as_ref(),
                                cadence,
                                fallback_reason.as_deref(),
                            );
                        }
                        if sender
                            .send(Update {
                                target: index,
                                reading,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    if polled_remote {
                        next_remote_poll = Instant::now() + interval;
                    }
                }
            });
        }
        for (index, source) in sources.iter().enumerate() {
            if !matches!(source, Source::Pr { .. })
                || latest[index].as_ref().is_some_and(Reading::done)
            {
                continue;
            }
            let sender = sender.clone();
            let edge = &edge;
            let stop = &stop;
            let interval = Duration::from_secs(60);
            scope.spawn(move || {
                // Own the source so the subject and `pinned_head_seen` survive across
                // samples: they are the wait's one pinned decision.
                let mut source = (*source).clone();
                let (number, repository, complete, policy_source) = match &source {
                    Source::Pr {
                        number,
                        repository,
                        complete,
                        policy_source,
                        ..
                    } => (
                        *number,
                        repository.clone(),
                        *complete,
                        policy_source.clone(),
                    ),
                    _ => return,
                };
                let label = source.label();
                let mut session = None;
                // Each subscription owns its replay cursor. No target can
                // consume another target's events from a shared cursor.
                let setup =
                    (|| -> Result<crate::pr::observer::journal::PrJournal, Box<dyn std::error::Error>> {
                        // The observer module owns both transports: this journal
                        // supplies a current PR observation when one exists, and
                        // `read_pr` falls back to the forge through the observer.
                        let journal = crate::pr::observer::journal::PrJournal::open(
                            edge.roots.state.as_path(),
                            edge.cwd,
                        )?;
                        if let Ok(repository) = &repository {
                            journal.register(repository, number, None)?;
                        }
                        Ok(journal)
                    })();
                let mut event_reader = match setup {
                    Ok(journal) => Some(journal),
                    Err(error) => {
                        provenant_records::warning::warn(provenant_records::warning::Warning::new(
                            provenant_records::warning::PR_JOURNAL_UNAVAILABLE,
                            format!("PR journal unavailable; reconciling after gaps: {error}"),
                         provenant_records::warning::detected_source(provenant_records::warning::PR_JOURNAL_UNAVAILABLE)));
                        None
                    }
                };
                let mut grace = ActionsGrace::new(actions_grace);
                let initial = grace.apply(
                    read_pr(
                        &mut source,
                        edge,
                        &mut session,
                        deadline,
                        pr_read_timeout,
                        &stop.cancel,
                    ),
                    &label,
                    complete,
                );
                let done = initial.done();
                if sender
                    .send(Update {
                        target: index,
                        reading: initial,
                    })
                    .is_err()
                    || done
                {
                    return;
                }
                let mut last_reconcile = Instant::now();
                loop {
                    if stop.cancel.is_cancelled()
                        || deadline.saturating_duration_since(Instant::now()).is_zero()
                    {
                        break;
                    }
                    let reading = match (event_reader.as_mut(), repository.as_ref()) {
                        (Some(journal), Ok(repository_ref)) => {
                            match journal.next(
                                repository_ref,
                                number,
                                Duration::from_secs(60)
                                    .saturating_sub(last_reconcile.elapsed())
                                    .min(deadline.saturating_duration_since(Instant::now()))
                                    .min(grace.due_in().unwrap_or(Duration::MAX)),
                                &stop.cancel,
                            ) {
                            Ok(Some(detail)) => {
                                last_reconcile = Instant::now();
                                if detail["status"].is_object() {
                                    let event = match &mut source {
                                        Source::Pr { subject, pinned_head_seen, .. } => reading_pr_event(
                                            subject,
                                            pinned_head_seen,
                                            &PrSample {
                                                number,
                                                repository: &repository,
                                                policy: pr_head_policy,
                                                policy_source: &policy_source,
                                                now: wall_now(),
                                            },
                                            complete,
                                            detail,
                                        ),
                                        _ => unreachable!("checked above"),
                                    };
                                    if complete || event.settle == Settle::Succeeded {
                                        // Journal records are bounded and omit Actions runs.
                                        // Reconcile through the observer before settling.
                                        read_pr(
                                            &mut source,
                                            edge,
                                            &mut session,
                                            deadline,
                                            pr_read_timeout,
                                            &stop.cancel,
                                        )
                                    } else {
                                        event
                                    }
                                } else {
                                    // Webhook records carry identifiers only;
                                    // fetch the authoritative PR status now.
                                    read_pr(
                                        &mut source,
                                        edge,
                                        &mut session,
                                        deadline,
                                        pr_read_timeout,
                                        &stop.cancel,
                                    )
                                }
                            }
                            Ok(None)
                                if last_reconcile.elapsed() >= Duration::from_secs(60)
                                    || grace.is_due() =>
                            {
                                last_reconcile = Instant::now();
                                // A held reading whose grace period ended needs a
                                // fresh read even when the journal is current:
                                // nothing else would settle it.
                                if !grace.is_due()
                                    && journal
                                        .source_current(repository_ref, number, None)
                                        .unwrap_or(false)
                                {
                                    continue;
                                }
                                read_pr(
                                    &mut source,
                                    edge,
                                    &mut session,
                                    deadline,
                                    pr_read_timeout,
                                    &stop.cancel,
                                )
                            }
                            Ok(None) => continue,
                            Err(error) => {
                                event_reader = None;
                                Reading::unavailable(
                                    &label,
                                    &format!("PR event journal failed: {error}"),
                                )
                            }
                            }
                        }
                        _ => {
                            if !stop.pause(
                                interval
                                    .saturating_sub(last_reconcile.elapsed())
                                    .min(deadline.saturating_duration_since(Instant::now()))
                                    .min(grace.due_in().unwrap_or(Duration::MAX)),
                            ) {
                                break;
                            }
                            last_reconcile = Instant::now();
                            read_pr(
                                &mut source,
                                edge,
                                &mut session,
                                deadline,
                                pr_read_timeout,
                                &stop.cancel,
                            )
                        }
                    };
                    let reading = grace.apply(reading, &label, complete);
                    let finished = reading.done();
                    if sender
                        .send(Update {
                            target: index,
                            reading,
                        })
                        .is_err()
                        || finished
                    {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let done = collect(
            &receiver,
            &mut latest,
            &mut settled,
            deadline,
            mode,
            &mut transition,
        );
        stop.stop();
        drop(receiver);
        done
    });
    let mut final_session = None;
    let done = done
        || final_snapshot(
            &sources,
            &mut latest,
            &mut settled,
            mode,
            local_host.as_ref(),
            |target| final_read(target, &edge, &mut final_session),
            &mut transition,
        );
    if let Some(error) = write_error {
        return crate::fail(
            "wait",
            Outcome::Error,
            "IO",
            &error.to_string(),
            "check the reported output path and its permissions, then rerun `provenant wait`",
            globals,
        );
    }
    let outcome = outcome(&latest, &settled, done, mode);
    let mut human = mode_heading(mode);
    let rows: Vec<_> = sources
        .iter()
        .zip(&latest)
        .map(|(source, reading)| {
            let label = source.label();
            let line = reading.as_ref().map_or_else(
                || format!("{label} pending"),
                |reading| reading.line.clone(),
            );
            human.push_str(&line);
            human.push('\n');
            if let (Source::Pr { .. }, Some(reading)) = (source, reading)
                && let Ok(status) =
                    serde_json::from_value::<crate::pr::status::Line>(reading.data.clone())
            {
                for failure in status.failure_lines() {
                    human.push_str(&failure);
                    human.push('\n');
                }
                for digest in status.digest_rows() {
                    human.push_str(&digest);
                    human.push('\n');
                }
            }
            if let Some(reading) = reading {
                for digest in reading.data["digests"].as_array().into_iter().flatten() {
                    if let Some(line) = digest["line"].as_str() {
                        human.push_str(line);
                        human.push('\n');
                    }
                }
            }
            json!({
                "target":label,
                "settle":reading.as_ref().map_or(Settle::Pending, |reading| reading.settle),
                "line":line,
                "data":reading.as_ref().map(|reading| &reading.data),
            })
        })
        .collect();
    if sources.is_empty() {
        human.push_str("nothing to wait for: no unsettled graphs\n");
    }
    if !done {
        for detail in pr_timeout_details(&sources, &latest) {
            human.push_str(&detail);
            human.push('\n');
        }
        human.push_str("wait timed out; the work continues\n");
    }
    let observation = observation
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|_| "event source status unavailable".into());
    // The observation method is diagnostic detail, not the answer a chair
    // reads: it belongs to --full and to the JSON envelope, never to the last
    // line of default human output.
    if globals.full && !graph_indices.is_empty() {
        human.push_str(&format!("local observation: {observation}\n"));
    }
    // A superseded name warns on stderr; next_command points at the newer handle.
    let mut superseding = Vec::new();
    if let Some(project_id) = project_id {
        for target in &command.targets {
            if let Some(newest) =
                crate::fabric::superseding_handle(&resolved.roots.state, project_id, &target.name)
            {
                crate::fabric::superseded_notice(&target.name, &newest, "wait");
                superseding.push(newest);
            }
        }
    }
    let step = final_step(&sources, &latest, done, &superseding);
    // Compact output keeps an owner summary. The settle states and the next
    // command now travel as one typed step on the envelope, not as two strings.
    let mut summary = provenant_cli::Summary::new(
        mode_heading(mode).trim_end().to_owned(),
        rows.iter()
            .map(|row| row["line"].as_str().unwrap_or("").to_owned())
            .collect(),
    );
    // Completion reports must retain every failing check and its link in compact output.
    for (source, reading) in sources.iter().zip(&latest) {
        if let (Source::Pr { .. }, Some(reading)) = (source, reading)
            && let Ok(status) =
                serde_json::from_value::<crate::pr::status::Line>(reading.data.clone())
        {
            summary.lines.extend(status.failure_lines());
            summary.lines.extend(status.digest_rows());
        }
    }
    crate::emit_summarised(
        &provenant_records::CommandResult::new(
            "wait",
            outcome,
            terminal_data(
                mode,
                rows,
                !done,
                (!graph_indices.is_empty()).then_some(observation.as_str()),
            ),
        )
        .with_next(vec![step]),
        globals,
        &human,
        true,
        Some(&summary),
    )
}

fn write_line(line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")?;
    stdout.flush()
}

/// The policy source the unit tests pin with: the compiled profile's key.
#[cfg(test)]
pub(crate) fn test_policy_source() -> provenant_records::Source {
    provenant_records::Source::at(
        provenant_records::Layer::Default,
        "test#domains.wait.pr_head_policy",
    )
}

#[cfg(test)]
#[allow(dead_code)] // Also included by the worktree driver without libtest.
mod tests;
