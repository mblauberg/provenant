//! What happens once a lane has settled, beyond its own outcome: the notice to
//! the chair that dispatched it and the chair log entry.
//!
//! Reactions are at-least-once and the stores make them exactly-once. The
//! first reaction for an attempt freezes its payload in a receipt under the
//! state root; every later pass, including crash recovery, replays that
//! payload, so a chain extended after the fact changes nothing. A pass drains
//! every settled attempt of a run that has no completed receipt, from both the
//! normal path and the recovery worker.
use provenant_core::{
    ActorId, AttemptId, CancelToken, MessageId, OperationId, ProjectId, Roots, RunId, WaitBudget,
};
use provenant_fabric_store::{HostStore, Message, MessageKind, Request, Store, ThreadWait};
use provenant_os::clock::SystemClock;
use provenant_records::run::{AttemptState, ChairBinding, WriterDeliveryState};
use provenant_workflow::{
    log::{Append, LogStore, Retention},
    session::{HostSessionId, chair_session_for},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

type Error = Box<dyn std::error::Error>;

/// A notice is an outcome for the chair to read, never an instruction.
pub(crate) const NOTICE_BYTES: usize = 300;

/// Run the launch and the reactions in the documented order. `launch` calls
/// its argument at most once: a delayed retry calls it before its wait, so the
/// reactions are not held behind the wait. If it never does, the reactions run
/// after it, whether or not it failed. The first error in time order is
/// reported, and neither step prevents the other.
pub(crate) fn seam(
    launch: impl FnOnce(&mut dyn FnMut()) -> Result<(), String>,
    react: impl FnOnce() -> Result<(), Error>,
) -> Result<(), Error> {
    let mut react = Some(react);
    let mut early = None;
    let launched = launch(&mut || {
        if let Some(react) = react.take() {
            early = Some(react());
        }
    });
    let late = react.map(|react| react());
    if let Some(Err(error)) = early {
        return Err(error);
    }
    launched.map_err(std::io::Error::other)?;
    late.unwrap_or(Ok(()))
}

/// Who settled, and where its state lives.
pub(crate) struct Subject {
    pub home: PathBuf,
    pub checkout: PathBuf,
    pub state_root: PathBuf,
    pub project: ProjectId,
    pub actor: ActorId,
    pub run: RunId,
}

impl Subject {
    pub(crate) fn roots(&self) -> Result<Roots, Error> {
        Ok(crate::roots::child(
            &self.home,
            &self.checkout,
            self.state_root.clone(),
        )?)
    }
}

/// The payload frozen when an attempt first reacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Frozen {
    pub notice: String,
    pub writer: bool,
    pub finished_micros: i64,
    pub chair: Option<ChairBinding>,
    pub pr_ids: Vec<String>,
    pub issue_ids: Vec<String>,
    #[serde(default)]
    pub done: bool,
}

/// What the notice line is built from.
pub(crate) struct Line<'a> {
    pub attempt: &'a str,
    pub outcome: &'a str,
    pub partial: bool,
    pub model: Option<&'a str>,
    pub staged: Option<&'a str>,
    pub head: Option<&'a str>,
    pub pull_request: Option<&'a str>,
}

fn clip(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Each variable field has its own budget, so a long model ID cannot push the
/// recovery, head or pull request out. The fixed head of the line and every
/// budget together stay within `NOTICE_BYTES`.
pub(crate) fn notice_line(line: &Line<'_>) -> String {
    let mut out = format!(
        "settled {} {}",
        clip(line.attempt, 32),
        clip(line.outcome, 16)
    );
    if line.partial {
        out.push_str(" (partial)");
    }
    for (label, value, budget) in [
        ("model", line.model, 56),
        ("next", line.staged, 40),
        ("head", line.head, 12),
        ("pr", line.pull_request, 90),
    ] {
        if let Some(value) = value {
            out.push_str(&format!(" {label}={}", clip(value, budget)));
        }
    }
    debug_assert!(out.len() <= NOTICE_BYTES);
    out
}

fn number(text: &str) -> Option<String> {
    let digits = text.trim_start_matches('#');
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.to_owned())
}

fn receipt_path(roots: &Roots, project: ProjectId, attempt: AttemptId) -> PathBuf {
    roots
        .state
        .as_path()
        .join("settlement-reactions")
        .join(project.to_string())
        .join(format!("{attempt}.json"))
}

fn read_receipt(path: &std::path::Path) -> Result<Option<Frozen>, Error> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_receipt(path: &std::path::Path, frozen: &Frozen) -> Result<(), Error> {
    let parent = path.parent().ok_or("receipt path")?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec(frozen)?)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// The frozen payload for an attempt: the one on disk if there is one, else
/// the one `make` computes, written before any reaction uses it.
pub(crate) fn frozen_or(
    path: &std::path::Path,
    make: impl FnOnce() -> Result<Option<Frozen>, Error>,
) -> Result<Option<Frozen>, Error> {
    if let Some(found) = read_receipt(path)? {
        return Ok(Some(found));
    }
    let Some(made) = make()? else {
        return Ok(None);
    };
    write_receipt(path, &made)?;
    Ok(Some(made))
}

fn compute(subject: &Subject, roots: &Roots, attempt: AttemptId) -> Result<Option<Frozen>, Error> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(provenant_core::Clock::mono_now(&clock), 30)?;
    let call = provenant_run::Request {
        clock: &clock,
        budget,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: subject.project,
        actor_id: subject.actor,
        read_prompts: true,
    };
    let store = provenant_run::RunStore::open(&context, &call)?;
    let record = store.record(attempt, &call)?;
    let AttemptState::Terminal {
        outcome,
        finished_at,
        ..
    } = &record.state
    else {
        return Ok(None);
    };
    let request = store.request(record.run_id, &call)?;
    let writer = request
        .tasks
        .iter()
        .find(|task| task.task_id == record.task_id)
        .is_some_and(|task| task.writer.is_some());
    let tail = store.policy_tail(record.attempt_id, &call)?;
    let staged = (tail.attempt_id != record.attempt_id).then(|| {
        tail.continuation
            .clone()
            .unwrap_or_else(|| "successor".into())
    });
    let backoff = ThreadWait;
    let store_call = Request {
        clock: &clock,
        budget,
        cancel: &cancel,
        backoff: &backoff,
    };
    // Lane-dispatched lanes carry no chair binding, which keeps their
    // settlements quiet.
    let chair = match open_host(roots) {
        Ok(Some(host)) => host
            .run_chair(record.project_id, record.run_id, &store_call)?
            .and_then(|found| found.chair),
        Ok(None) | Err(provenant_fabric_store::Error::NotFound) => None,
        Err(error) => return Err(error.into()),
    };
    let (head, pull_request) = record.delivery.as_ref().map_or((None, None), |receipt| {
        let pull_request = match &receipt.state {
            WriterDeliveryState::Delivered { pull_request, .. } => pull_request.clone(),
            _ => None,
        };
        (receipt.head.map(|oid| oid.to_string()), pull_request)
    });
    let head = head.or_else(|| record.head.map(|oid| oid.to_string()));
    let route = record
        .observed_route
        .as_ref()
        .or(record.applied_route.as_ref());
    let outcome = serde_json::to_value(outcome)?;
    let attempt_text = record.attempt_id.to_string();
    let notice = notice_line(&Line {
        attempt: &attempt_text,
        outcome: outcome.as_str().unwrap_or("unknown"),
        partial: !record.partial_signals.is_empty(),
        model: route.and_then(|route| route.model.as_deref()),
        staged: staged.as_deref(),
        head: head.as_deref().map(|head| clip(head, 12)),
        pull_request: pull_request.as_deref(),
    });
    Ok(Some(Frozen {
        notice,
        writer,
        finished_micros: finished_at.as_micros(),
        chair,
        pr_ids: pull_request
            .as_deref()
            .and_then(|url| url.rsplit('/').next())
            .and_then(number)
            .into_iter()
            .collect(),
        issue_ids: record
            .context
            .as_ref()
            .and_then(|context| context.work_item.as_deref())
            .and_then(number)
            .into_iter()
            .collect(),
        done: false,
    }))
}

// The read-only host store takes a real deadline.
#[allow(clippy::disallowed_methods)]
fn open_host(roots: &Roots) -> provenant_fabric_store::Result<Option<HostStore>> {
    HostStore::open_read_only(
        &roots.state,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
}

/// React for every settled attempt of the subject's run that has not finished
/// reacting. Errors do not stop the other attempts; the first is returned.
pub(crate) fn drain_run(subject: &Subject) -> Result<(), Error> {
    let roots = subject.roots()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        budget: WaitBudget::cli(provenant_core::Clock::mono_now(&clock), 30)?,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: subject.project,
        actor_id: subject.actor,
        read_prompts: false,
    };
    let attempts =
        provenant_run::RunStore::open(&context, &call)?.terminal_attempts(subject.run, &call)?;
    let config = provenant_config::read_with_domains(&roots, &provenant_cli::domains::SECTIONS)?;
    let settings = config.domains.fabric.lanes.settlement;
    let mut first_error = None;
    for attempt in attempts {
        let path = receipt_path(&roots, subject.project, attempt.attempt_id);
        let result = (|| -> Result<(), Error> {
            let Some(mut frozen) =
                frozen_or(&path, || compute(subject, &roots, attempt.attempt_id))?
            else {
                return Ok(());
            };
            if frozen.done {
                return Ok(());
            }
            let notice = post_notice(&frozen, &settings, &roots, subject, attempt.attempt_id);
            let log = append_chair_log(
                &frozen,
                &settings,
                &config.retention,
                &roots,
                subject.project,
                attempt.attempt_id,
            );
            let outcome = notice.and(log);
            if outcome.is_ok() {
                frozen.done = true;
                write_receipt(&path, &frozen)?;
            }
            outcome
        })();
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Post the frozen notice to the dispatcher's inbox. A missing store or inbox
/// drops it without error.
pub(crate) fn post_notice(
    frozen: &Frozen,
    settings: &provenant_fabric_runtime::config::Settlement,
    roots: &Roots,
    subject: &Subject,
    attempt: AttemptId,
) -> Result<(), Error> {
    let Some(chair) = &frozen.chair else {
        return Ok(());
    };
    if !settings.notice || (!frozen.writer && !settings.notice_read_only) {
        return Ok(());
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let backoff = ThreadWait;
    let request = Request {
        clock: &clock,
        budget: WaitBudget::cli(provenant_core::Clock::mono_now(&clock), 10)?,
        cancel: &cancel,
        backoff: &backoff,
    };
    let store = match Store::open(&roots.state, &request) {
        Ok(store) => store,
        Err(provenant_fabric_store::Error::NotFound) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let message = Message {
        extra: Default::default(),
        header: provenant_records::RecordHeader::V1,
        project: subject.project,
        // The attempt names the notice and the frozen body and settlement time
        // fix its content, so a replay inserts the same record.
        id: MessageId::from_bytes(*attempt.as_bytes()),
        sender: subject.actor,
        recipients: vec![chair.actor],
        conversation: None,
        kind: MessageKind::Note,
        body: frozen.notice.clone(),
        artifacts: Vec::new(),
        reply_to: None,
        task: None,
        created_at: provenant_core::WallTime::from_micros(frozen.finished_micros)?,
    };
    match store.send(&message, &request) {
        Ok(()) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Append the frozen entry to the dispatching chair's session log. The
/// session is the chair's explicit one, else the one it saved.
pub(crate) fn append_chair_log(
    frozen: &Frozen,
    settings: &provenant_fabric_runtime::config::Settlement,
    retention: &provenant_config::RetentionSettings,
    roots: &Roots,
    project: ProjectId,
    attempt: AttemptId,
) -> Result<(), Error> {
    let Some(chair) = &frozen.chair else {
        return Ok(());
    };
    if !settings.chair_log {
        return Ok(());
    }
    let Some(session) = chair
        .session
        .as_deref()
        .and_then(|session| HostSessionId::parse(session).ok())
        .or_else(|| chair_session_for(&roots.state, project, chair.actor))
    else {
        return Ok(());
    };
    let retention = Retention {
        max_age_seconds: retention.chair_log_days * 86400,
        replay_horizon_seconds: retention.chair_log_replay_seconds(),
        max_entries: retention.chair_log_entries,
        max_bytes: retention.chair_log_bytes,
    };
    let issued_at = frozen.finished_micros.div_euclid(1_000_000);
    let now = provenant_core::Clock::wall_now(&SystemClock::new())
        .map_or(issued_at, |at| at.as_micros().div_euclid(1_000_000));
    match LogStore::new(roots.state.clone(), project, session).append(
        Append {
            operation_id: OperationId::parse(&format!("settle-{attempt}"))?,
            issued_at,
            host: chair.host.to_string(),
            kind: "lane_settled".into(),
            markdown: frozen.notice.clone(),
            request_ids: Vec::new(),
            attempt_ids: vec![attempt.to_string()],
            issue_ids: frozen.issue_ids.clone(),
            pr_ids: frozen.pr_ids.clone(),
            channel: "provenant".into(),
            correction_of: None,
        },
        retention,
        now,
    ) {
        // A settlement older than the replay horizon has nothing to add.
        Ok(_) | Err(provenant_workflow::log::Error::Expired) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, dead_code)]
mod tests {
    #[allow(unused_imports)]
    use super::*;
    #[allow(unused_imports)]
    use std::cell::RefCell;

    fn line<'a>(model: &'a str) -> Line<'a> {
        Line {
            attempt: "0123456789abcdef0123456789abcdef",
            outcome: "quota_limited",
            partial: true,
            model: Some(model),
            staged: Some("policy_fallback_with_a_long_name_that_overruns_it"),
            head: Some("0123456789abcdef"),
            pull_request: Some("https://github.com/example/example/pull/2125"),
        }
    }

    #[test]
    fn required_fields_survive_a_long_model_id() {
        let model = "m".repeat(512);
        let text = notice_line(&line(&model));
        assert!(text.len() <= NOTICE_BYTES, "{}", text.len());
        for needle in [
            "(partial)",
            " next=policy_fallback",
            " head=0123456789ab",
            " pr=https://",
        ] {
            assert!(text.contains(needle), "{needle} in {text}");
        }
    }

    #[test]
    fn truncation_respects_character_boundaries() {
        let model = "\u{e9}".repeat(300);
        let text = notice_line(&line(&model));
        assert!(text.len() <= NOTICE_BYTES);
    }

    #[test]
    fn seam_runs_reactions_once_before_a_delayed_wait() {
        let calls = RefCell::new(Vec::new());
        let result = seam(
            |before| {
                calls.borrow_mut().push("launch-start");
                before();
                calls.borrow_mut().push("launch-wait");
                Ok(())
            },
            || {
                calls.borrow_mut().push("react");
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(*calls.borrow(), ["launch-start", "react", "launch-wait"]);
    }

    #[test]
    fn seam_runs_reactions_once_without_a_successor() {
        let count = RefCell::new(0);
        seam(
            |_| Ok(()),
            || {
                *count.borrow_mut() += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*count.borrow(), 1);
    }

    #[test]
    fn a_failed_launch_still_runs_reactions_and_reports_first() {
        let count = RefCell::new(0);
        let error = seam(
            |_| Err("launch".into()),
            || {
                *count.borrow_mut() += 1;
                Err("react".into())
            },
        )
        .unwrap_err();
        assert_eq!(*count.borrow(), 1);
        assert_eq!(error.to_string(), "launch");
    }

    #[test]
    fn an_early_reaction_error_is_not_replaced_by_a_later_launch_error() {
        let error = seam(
            |before| {
                before();
                Err("launch".into())
            },
            || Err("react".into()),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "react");
    }

    fn frozen(notice: &str) -> Frozen {
        Frozen {
            notice: notice.into(),
            writer: true,
            finished_micros: now() * 1_000_000,
            chair: None,
            pr_ids: vec![],
            issue_ids: vec![],
            done: false,
        }
    }

    #[test]
    fn a_receipt_freezes_the_payload_against_a_chain_extension() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("a.json");
        let first = frozen_or(&path, || Ok(Some(frozen("settled A next=B")))).unwrap();
        // The chain grew A -> B -> C; a replay must not recompute.
        let replay = frozen_or(&path, || Ok(Some(frozen("settled A next=C")))).unwrap();
        assert_eq!(first, replay);
        assert_eq!(replay.unwrap().notice, "settled A next=B");
    }

    #[test]
    fn an_unsettled_attempt_leaves_no_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("a.json");
        assert!(frozen_or(&path, || Ok(None)).unwrap().is_none());
        assert!(!path.exists());
    }

    fn roots(base: &std::path::Path) -> Roots {
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("assets"));
        inputs.state_dir = Some(base.join("state"));
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::fs::create_dir_all(base.join("assets")).unwrap();
        inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap()
    }

    fn chair(session: Option<&str>) -> ChairBinding {
        ChairBinding {
            extra: Default::default(),
            host: provenant_core::HostId::from_bytes([1; 16]),
            actor: ActorId::from_bytes([2; 16]),
            session: session.map(Into::into),
        }
    }

    fn subject() -> Subject {
        Subject {
            home: "/".into(),
            checkout: "/".into(),
            state_root: "/".into(),
            project: ProjectId::from_bytes([3; 16]),
            actor: ActorId::from_bytes([4; 16]),
            run: RunId::from_bytes([5; 16]),
        }
    }

    fn inbox(roots: &Roots) -> Vec<String> {
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let backoff = ThreadWait;
        let request = Request {
            clock: &clock,
            budget: WaitBudget::cli(provenant_core::Clock::mono_now(&clock), 10).unwrap(),
            cancel: &cancel,
            backoff: &backoff,
        };
        let store = Store::open(&roots.state, &request).unwrap();
        store
            .read_mail(
                subject().project,
                ActorId::from_bytes([2; 16]),
                Some(0),
                50,
                &request,
            )
            .unwrap()
            .items
            .into_iter()
            .map(|delivery| delivery.message.body)
            .collect()
    }

    #[test]
    fn notice_and_log_replay_to_one_each_and_honour_the_switches() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let roots = roots(&base);
        let settings = provenant_fabric_runtime::config::Settlement::default();
        let retention = provenant_config::RetentionSettings::default();
        let attempt = AttemptId::from_bytes([9; 16]);
        let mut frozen = frozen("settled one");
        frozen.chair = Some(chair(Some("session-1")));
        let subject = subject();
        for _ in 0..3 {
            post_notice(&frozen, &settings, &roots, &subject, attempt).unwrap();
            append_chair_log(
                &frozen,
                &settings,
                &retention,
                &roots,
                subject.project,
                attempt,
            )
            .unwrap();
        }
        assert_eq!(inbox(&roots), ["settled one"]);
        let page = LogStore::new(
            roots.state.clone(),
            subject.project,
            HostSessionId::parse("session-1").unwrap(),
        )
        .page(None, 50, retention_for(&retention), now())
        .unwrap();
        assert_eq!(page.entries.len(), 1);

        // Switched off, a different attempt posts and logs nothing.
        let off = provenant_fabric_runtime::config::Settlement {
            notice: false,
            notice_read_only: false,
            chair_log: false,
        };
        let other = AttemptId::from_bytes([8; 16]);
        post_notice(&frozen, &off, &roots, &subject, other).unwrap();
        append_chair_log(&frozen, &off, &retention, &roots, subject.project, other).unwrap();
        assert_eq!(inbox(&roots).len(), 1);

        // A read-only lane is quiet unless asked.
        let mut probe = frozen.clone();
        probe.writer = false;
        post_notice(&probe, &settings, &roots, &subject, other).unwrap();
        assert_eq!(inbox(&roots).len(), 1);
        let loud = provenant_fabric_runtime::config::Settlement {
            notice_read_only: true,
            ..settings.clone()
        };
        post_notice(&probe, &loud, &roots, &subject, other).unwrap();
        assert_eq!(inbox(&roots).len(), 2);

        // No chair, no reaction.
        probe.chair = None;
        post_notice(
            &probe,
            &loud,
            &roots,
            &subject,
            AttemptId::from_bytes([7; 16]),
        )
        .unwrap();
        assert_eq!(inbox(&roots).len(), 2);
    }

    fn retention_for(settings: &provenant_config::RetentionSettings) -> Retention {
        Retention {
            max_age_seconds: settings.chair_log_days * 86400,
            replay_horizon_seconds: settings.chair_log_replay_seconds(),
            max_entries: settings.chair_log_entries,
            max_bytes: settings.chair_log_bytes,
        }
    }

    fn now() -> i64 {
        provenant_core::Clock::wall_now(&SystemClock::new())
            .unwrap()
            .as_micros()
            .div_euclid(1_000_000)
    }
}
