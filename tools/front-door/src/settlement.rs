//! What happens once a lane has settled, beyond its own outcome: the notice to
//! the chair that dispatched it and the chair log entry. Both read one frozen
//! view of the attempt and are idempotent by attempt ID, so a repeated
//! settlement leaves one notice and one entry. The ordered list of reactions
//! lives in `fabric::advance_settled_graphs`; this module holds the reactions
//! that need no graph.
use provenant_core::{CancelToken, MessageId, OperationId, WaitBudget};
use provenant_fabric_store::{HostStore, Message, MessageKind, Request, Store, ThreadWait};
use provenant_os::clock::SystemClock;
use provenant_records::run::{AttemptState, ChairBinding, WriterDeliveryState};
use provenant_run::HolderContext;
use provenant_workflow::{
    log::{Append, LogStore, Retention},
    session::HostSessionId,
};

type Error = Box<dyn std::error::Error>;

/// A notice is an outcome for the chair to read, never an instruction.
pub(crate) const NOTICE_BYTES: usize = 300;

/// The settled attempt as the reactions see it.
pub(crate) struct Facts {
    attempt: String,
    outcome: String,
    model: Option<String>,
    partial: bool,
    head: Option<String>,
    pull_request: Option<String>,
    work_item: Option<String>,
    /// How the host staged the lane's remaining work, if it did.
    staged: Option<String>,
    writer: bool,
    finished_micros: i64,
    chair: Option<ChairBinding>,
    pub(crate) settings: provenant_fabric_runtime::config::Settlement,
    retention: provenant_config::RetentionSettings,
    state: provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    actor: provenant_core::ActorId,
    attempt_id: provenant_core::AttemptId,
}

impl Facts {
    /// `None` when the attempt is not terminal yet, which a reaction treats as
    /// nothing to report.
    // The read-only host store takes a real deadline.
    #[allow(clippy::disallowed_methods)]
    pub(crate) fn read(
        settled: &HolderContext,
        roots: &provenant_core::Roots,
    ) -> Result<Option<Self>, Error> {
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
            project_id: settled.project_id,
            actor_id: settled.actor_id,
            read_prompts: true,
        };
        let store = provenant_run::RunStore::open(&context, &call)?;
        let record = store.record(settled.attempt_id, &call)?;
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
        let config = provenant_config::read_with_domains(roots, &provenant_cli::domains::SECTIONS)?;
        let backoff = ThreadWait;
        let store_call = Request {
            clock: &clock,
            budget,
            cancel: &cancel,
            backoff: &backoff,
        };
        // Lane-dispatched lanes carry no chair binding, which is what keeps
        // their settlements quiet.
        let chair = match HostStore::open_read_only(
            &roots.state,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        ) {
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
        let route = record
            .observed_route
            .as_ref()
            .or(record.applied_route.as_ref());
        Ok(Some(Self {
            attempt: record.attempt_id.to_string(),
            outcome: serde_json::to_value(outcome)?
                .as_str()
                .unwrap_or("unknown")
                .to_owned(),
            model: route.and_then(|route| route.model.clone()),
            partial: !record.partial_signals.is_empty(),
            head: head.or_else(|| record.head.map(|oid| oid.to_string())),
            pull_request,
            work_item: record
                .context
                .as_ref()
                .and_then(|context| context.work_item.clone()),
            staged,
            writer,
            finished_micros: finished_at.as_micros(),
            chair,
            settings: config.domains.fabric.lanes.settlement,
            retention: config.retention,
            state: roots.state.clone(),
            project: record.project_id,
            actor: settled.actor_id,
            attempt_id: record.attempt_id,
        }))
    }

    /// One line, at most `NOTICE_BYTES`: attempt, outcome, the partial flag,
    /// model, staged recovery, head, then the pull request as the part that
    /// yields first to the limit.
    pub(crate) fn notice(&self) -> String {
        let mut line = format!("settled {} {}", self.attempt, self.outcome);
        if self.partial {
            line.push_str(" (partial)");
        }
        for (label, value) in [
            ("model", &self.model),
            ("next", &self.staged),
            (
                "head",
                &self
                    .head
                    .as_ref()
                    .map(|head| head[..head.len().min(12)].to_owned()),
            ),
            ("pr", &self.pull_request),
        ] {
            if let Some(value) = value {
                line.push_str(&format!(" {label}={value}"));
            }
        }
        truncate(&mut line, NOTICE_BYTES);
        line
    }

    fn quiet(&self) -> bool {
        self.chair.is_none() || (!self.writer && !self.settings.notice_read_only)
    }

    /// Post the notice to the dispatcher's inbox. A missing store or inbox
    /// drops it without error.
    pub(crate) fn post_notice(&self) -> Result<(), Error> {
        let Some(chair) = &self.chair else {
            return Ok(());
        };
        if !self.settings.notice || self.quiet() {
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
        let store = match Store::open(&self.state, &request) {
            Ok(store) => store,
            Err(provenant_fabric_store::Error::NotFound) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let message = Message {
            extra: Default::default(),
            header: provenant_records::RecordHeader::V1,
            project: self.project,
            // The attempt names the notice, and its settlement time fixes the
            // body, so a replay inserts the same record.
            id: MessageId::from_bytes(*self.attempt_id.as_bytes()),
            sender: self.actor,
            recipients: vec![chair.actor],
            conversation: None,
            kind: MessageKind::Note,
            body: self.notice(),
            artifacts: Vec::new(),
            reply_to: None,
            task: None,
            created_at: provenant_core::WallTime::from_micros(self.finished_micros)?,
        };
        match store.send(&message, &request) {
            Ok(()) | Err(provenant_fabric_store::Error::Conflict) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Append the settlement to the dispatching chair's session log.
    pub(crate) fn append_chair_log(&self) -> Result<(), Error> {
        let Some(chair) = &self.chair else {
            return Ok(());
        };
        if !self.settings.chair_log {
            return Ok(());
        }
        let Some(session) = chair
            .session
            .as_deref()
            .and_then(|session| HostSessionId::parse(session).ok())
        else {
            return Ok(());
        };
        let number = |text: &str| {
            let digits = text.trim_start_matches('#');
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .then(|| digits.to_owned())
        };
        let pr_ids: Vec<String> = self
            .pull_request
            .as_deref()
            .and_then(|url| url.rsplit('/').next())
            .and_then(number)
            .into_iter()
            .collect();
        let issue_ids: Vec<String> = self
            .work_item
            .as_deref()
            .and_then(number)
            .into_iter()
            .collect();
        let settings = &self.retention;
        let retention = Retention {
            max_age_seconds: settings.chair_log_days * 86400,
            replay_horizon_seconds: settings.chair_log_replay_seconds(),
            max_entries: settings.chair_log_entries,
            max_bytes: settings.chair_log_bytes,
        };
        let issued_at = self.finished_micros.div_euclid(1_000_000);
        let now = provenant_core::Clock::wall_now(&SystemClock::new())
            .map_or(issued_at, |at| at.as_micros().div_euclid(1_000_000));
        match LogStore::new(self.state.clone(), self.project, session).append(
            Append {
                operation_id: OperationId::parse(&format!("settle-{}", self.attempt))?,
                issued_at,
                host: chair.host.to_string(),
                kind: "lane_settled".into(),
                markdown: self.notice(),
                request_ids: Vec::new(),
                attempt_ids: vec![self.attempt.clone()],
                issue_ids,
                pr_ids,
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
}

fn truncate(line: &mut String, limit: usize) {
    if line.len() > limit {
        let mut end = limit;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn truncation_respects_character_boundaries() {
        let mut line = "\u{e9}".repeat(200);
        truncate(&mut line, NOTICE_BYTES);
        assert!(line.len() <= NOTICE_BYTES);
        assert!(line.chars().all(|c| c == '\u{e9}'));
    }
}
