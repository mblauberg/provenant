//! What happens once a lane has settled, beyond its own outcome: graphs
//! advance, the chair that dispatched it is notified, and its session log
//! gains an entry.
//!
//! The run store owns the obligation. The row that says a settlement owes
//! these reactions is written in the transaction that commits the terminal
//! record, with its payload frozen there. A drain replays that payload at
//! least once, from the normal seam and from crash recovery alike; the notice
//! and the log are idempotent by attempt, so the effect is exactly once.
use provenant_core::{ActorId, CancelToken, MessageId, OperationId, ProjectId, Roots, RunId};
use provenant_fabric_store::{HostStore, Message, MessageKind, Request, Store};
use provenant_os::clock::SystemClock;
use provenant_run::Reaction;
use provenant_workflow::{
    log::{Append, LogStore, Retention},
    session::{HostSessionId, chair_session_for},
};
use std::path::PathBuf;

type Error = Box<dyn std::error::Error>;

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

// The read-only host store takes a real deadline.
#[allow(clippy::disallowed_methods)]
fn open_host(roots: &Roots) -> provenant_fabric_store::Result<Option<HostStore>> {
    HostStore::open_read_only(
        &roots.state,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
}

/// The dispatching chair and its chair-log session, resolved once and then
/// frozen in the row. `PROVENANT_SESSION_ID` (recorded with the chair at
/// dispatch) comes first; otherwise the session the chair saved, confirmed
/// against canonical discovery ownership.
fn resolve(reaction: &mut Reaction, roots: &Roots) -> Result<(), Error> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let request = Request {
        clock: &clock,
        cancel: &cancel,
    };
    // Lane-dispatched lanes carry no chair binding, which keeps their
    // settlements quiet.
    let chair = match open_host(roots) {
        Ok(Some(host)) => host
            .run_chair(reaction.project, reaction.run, &request)?
            .and_then(|found| found.chair),
        Ok(None) | Err(provenant_fabric_store::Error::NotFound) => None,
        Err(error) => return Err(error.into()),
    };
    reaction.session = chair.as_ref().and_then(|chair| {
        chair
            .session
            .as_deref()
            .and_then(|session| HostSessionId::parse(session).ok())
            .or_else(|| chair_session_for(&roots.state, reaction.project, chair.actor))
            .map(|session| session.as_str().to_owned())
    });
    reaction.chair = chair;
    reaction.resolved = true;
    Ok(())
}

/// React for every attempt of the subject's run that still owes reactions,
/// under the run's reaction lock so concurrent drains do not repeat each
/// other. A reaction that fails stays owed and is retried on the next drain;
/// the others still run, and the first error is returned. `graph` advances the
/// graphs for an attempt from launch inputs the host supplies, never from the
/// row.
pub(crate) fn drain_run(
    subject: &Subject,
    graph: &mut dyn FnMut(&Reaction) -> Result<(), Error>,
) -> Result<(), Error> {
    let roots = subject.roots()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: subject.project,
        actor_id: subject.actor,
        read_prompts: false,
    };
    let store = provenant_run::RunStore::open(&context, &call)?;
    let config = provenant_config::read_with_domains(&roots, &provenant_cli::domains::SECTIONS)?;
    let settings = config.domains.fabric.lanes.settlement;
    store.drain_reactions(subject.run, &clock, &cancel, &mut |reaction, save| {
        react(
            reaction,
            save,
            graph,
            &mut |reaction| resolve(reaction, &roots),
            &settings,
            &config.retention,
            &roots,
        )
    })
}

/// One attempt's reactions. They are independent: one failing, or the host
/// store being busy, leaves it pending without stopping the others, and the
/// first failure in time order is reported. The destination is persisted
/// before any effect that uses it, so a replay after a crash cannot pick a
/// different session.
fn react(
    reaction: &mut Reaction,
    save: &mut provenant_run::reactions::Saver<'_>,
    graph: &mut dyn FnMut(&Reaction) -> Result<(), Error>,
    resolve: &mut dyn FnMut(&mut Reaction) -> Result<(), Error>,
    settings: &provenant_fabric_runtime::config::Settlement,
    retention: &provenant_config::RetentionSettings,
    roots: &Roots,
) -> Result<(), Error> {
    let mut first: Option<Error> = None;
    if !reaction.resolved {
        let outcome = resolve(reaction).and_then(|()| save(reaction));
        if let Err(error) = outcome {
            reaction.resolved = false;
            reaction.chair = None;
            reaction.session = None;
            first = Some(error);
        }
    }
    if reaction.graph && !reaction.graph_done {
        match graph(reaction) {
            Ok(()) => {
                reaction.graph_done = true;
                if let Err(error) = save(reaction) {
                    first.get_or_insert(error);
                }
            }
            Err(error) => {
                first.get_or_insert(error);
            }
        }
    }
    if reaction.resolved {
        if !reaction.notice_done {
            match post_notice(reaction, settings, roots) {
                Ok(()) => {
                    reaction.notice_done = true;
                    if let Err(error) = save(reaction) {
                        first.get_or_insert(error);
                    }
                }
                Err(error) => {
                    first.get_or_insert(error);
                }
            }
        }
        if !reaction.log_done {
            match append_chair_log(reaction, settings, retention, roots) {
                Ok(()) => reaction.log_done = true,
                Err(error) => {
                    first.get_or_insert(error);
                }
            }
        }
    }
    first.map_or(Ok(()), Err)
}

/// Post the frozen notice to the dispatcher's inbox. Quiet cases and a
/// missing store or inbox finish without error.
pub(crate) fn post_notice(
    reaction: &Reaction,
    settings: &provenant_fabric_runtime::config::Settlement,
    roots: &Roots,
) -> Result<(), Error> {
    let Some(chair) = &reaction.chair else {
        return Ok(());
    };
    if !settings.notice || (!reaction.writer && !settings.notice_read_only) {
        return Ok(());
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let request = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = match Store::open(&roots.state, &request) {
        Ok(store) => store,
        Err(provenant_fabric_store::Error::NotFound) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let message = Message {
        extra: Default::default(),
        header: provenant_records::RecordHeader::V1,
        project: reaction.project,
        // The attempt names the notice, and the frozen body, sender and
        // settlement time fix its content, so a replay inserts the same record.
        id: MessageId::from_bytes(*reaction.attempt.as_bytes()),
        sender: reaction.sender,
        recipients: vec![chair.actor],
        conversation: None,
        kind: MessageKind::Note,
        body: reaction.notice.clone(),
        artifacts: Vec::new(),
        reply_to: None,
        task: None,
        created_at: provenant_core::WallTime::from_micros(reaction.finished_micros)?,
    };
    Ok(store.send(&message, &request)?)
}

/// Append the frozen entry to the chair's session log.
pub(crate) fn append_chair_log(
    reaction: &Reaction,
    settings: &provenant_fabric_runtime::config::Settlement,
    retention: &provenant_config::RetentionSettings,
    roots: &Roots,
) -> Result<(), Error> {
    let Some(chair) = &reaction.chair else {
        return Ok(());
    };
    if !settings.chair_log {
        return Ok(());
    }
    let Some(session) = reaction
        .session
        .as_deref()
        .and_then(|session| HostSessionId::parse(session).ok())
    else {
        return Ok(());
    };
    let retention = Retention {
        max_age_seconds: retention.chair_log_days * 86400,
        replay_horizon_seconds: retention.chair_log_replay_seconds(),
        max_entries: retention.chair_log_entries,
        max_bytes: retention.chair_log_bytes,
    };
    let issued_at = reaction.finished_micros.div_euclid(1_000_000);
    let now = provenant_core::Clock::wall_now(&SystemClock::new())
        .map_or(issued_at, |at| at.as_micros().div_euclid(1_000_000));
    match LogStore::new(roots.state.clone(), reaction.project, session).append(
        Append {
            operation_id: OperationId::parse(&format!("settle-{}", reaction.attempt))?,
            issued_at,
            host: chair.host.to_string(),
            kind: "lane_settled".into(),
            markdown: reaction.notice.clone(),
            request_ids: Vec::new(),
            attempt_ids: vec![reaction.attempt.to_string()],
            issue_ids: reaction.issue_ids.clone(),
            pr_ids: reaction.pr_ids.clone(),
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
    use provenant_records::run::ChairBinding;
    #[allow(unused_imports)]
    use std::cell::RefCell;

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

    fn roots(base: &std::path::Path) -> Roots {
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("assets"));
        inputs.state_dir = Some(base.join("state"));
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::fs::create_dir_all(base.join("assets")).unwrap();
        inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap()
    }

    fn reaction(notice: &str, session: Option<&str>) -> Reaction {
        Reaction {
            schema_version: 1,
            attempt: provenant_core::AttemptId::from_bytes([9; 16]),
            run: RunId::from_bytes([5; 16]),
            project: ProjectId::from_bytes([3; 16]),
            sender: ActorId::from_bytes([4; 16]),
            notice: notice.into(),
            writer: true,
            finished_micros: now() * 1_000_000,
            pr_ids: vec![],
            issue_ids: vec![],
            graph: false,
            resolved: true,
            chair: Some(ChairBinding {
                extra: Default::default(),
                host: provenant_core::HostId::from_bytes([1; 16]),
                actor: ActorId::from_bytes([2; 16]),
                session: session.map(Into::into),
            }),
            session: session.map(Into::into),
            graph_done: false,
            notice_done: false,
            log_done: false,
        }
    }

    fn inbox(roots: &Roots) -> Vec<String> {
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let request = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = Store::open(&roots.state, &request).unwrap();
        store
            .read_mail(
                ProjectId::from_bytes([3; 16]),
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
        let frozen = reaction("settled one", Some("session-1"));
        for _ in 0..3 {
            post_notice(&frozen, &settings, &roots).unwrap();
            append_chair_log(&frozen, &settings, &retention, &roots).unwrap();
        }
        assert_eq!(inbox(&roots), ["settled one"]);
        let page = LogStore::new(
            roots.state.clone(),
            frozen.project,
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
        let mut other = reaction("settled two", Some("session-1"));
        other.attempt = provenant_core::AttemptId::from_bytes([8; 16]);
        post_notice(&other, &off, &roots).unwrap();
        append_chair_log(&other, &off, &retention, &roots).unwrap();
        assert_eq!(inbox(&roots).len(), 1);

        // A read-only lane is quiet unless asked.
        other.writer = false;
        post_notice(&other, &settings, &roots).unwrap();
        assert_eq!(inbox(&roots).len(), 1);
        let loud = provenant_fabric_runtime::config::Settlement {
            notice_read_only: true,
            ..settings.clone()
        };
        post_notice(&other, &loud, &roots).unwrap();
        assert_eq!(inbox(&roots).len(), 2);

        // No chair, no reaction.
        other.chair = None;
        other.attempt = provenant_core::AttemptId::from_bytes([7; 16]);
        post_notice(&other, &loud, &roots).unwrap();
        assert_eq!(inbox(&roots).len(), 2);
    }

    fn unresolved(_session: &str) -> Reaction {
        let mut reaction = reaction("settled three", None);
        reaction.attempt = provenant_core::AttemptId::from_bytes([6; 16]);
        reaction.resolved = false;
        reaction.graph = true;
        reaction.session = None;
        reaction.chair = None;
        reaction
    }

    fn binding(session: &str) -> ChairBinding {
        ChairBinding {
            extra: Default::default(),
            host: provenant_core::HostId::from_bytes([1; 16]),
            actor: ActorId::from_bytes([2; 16]),
            session: Some(session.into()),
        }
    }

    fn run_react(
        reaction: &mut Reaction,
        roots: &Roots,
        events: &RefCell<Vec<&'static str>>,
        graph_fails: bool,
        resolve_fails: bool,
        save_fails_at: Option<usize>,
    ) -> Result<(), Error> {
        let saves = RefCell::new(0usize);
        react(
            reaction,
            &mut |_| {
                let n = *saves.borrow();
                *saves.borrow_mut() += 1;
                events.borrow_mut().push("save");
                if save_fails_at == Some(n) {
                    return Err("db".into());
                }
                Ok(())
            },
            &mut |_| {
                events.borrow_mut().push("graph");
                if graph_fails {
                    Err("graph".into())
                } else {
                    Ok(())
                }
            },
            &mut |reaction| {
                events.borrow_mut().push("resolve");
                if resolve_fails {
                    return Err("host store busy".into());
                }
                reaction.chair = Some(binding("session-1"));
                reaction.session = Some("session-1".into());
                reaction.resolved = true;
                Ok(())
            },
            &provenant_fabric_runtime::config::Settlement::default(),
            &provenant_config::RetentionSettings::default(),
            roots,
        )
    }

    #[test]
    fn a_busy_host_store_does_not_stop_the_graph_and_lookup_retries() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp.path().canonicalize().unwrap());
        let events = RefCell::new(Vec::new());
        let mut reaction = unresolved("session-1");
        let error = run_react(&mut reaction, &roots, &events, false, true, None).unwrap_err();
        assert_eq!(error.to_string(), "host store busy");
        assert!(reaction.graph_done && !reaction.resolved);
        assert!(!reaction.notice_done && !reaction.log_done && reaction.pending());
        assert!(inbox(&roots).is_empty());
        // The next drain resolves and finishes the other two.
        run_react(&mut reaction, &roots, &events, false, false, None).unwrap();
        assert!(!reaction.pending());
        assert_eq!(inbox(&roots), ["settled three"]);
    }

    #[test]
    fn a_failed_graph_does_not_stop_the_notice_and_stays_pending() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp.path().canonicalize().unwrap());
        let events = RefCell::new(Vec::new());
        let mut reaction = unresolved("session-1");
        let error = run_react(&mut reaction, &roots, &events, true, false, None).unwrap_err();
        assert_eq!(error.to_string(), "graph");
        assert!(reaction.notice_done && reaction.log_done && !reaction.graph_done);
        assert!(reaction.pending());
    }

    #[test]
    fn the_destination_is_saved_before_any_effect_uses_it() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp.path().canonicalize().unwrap());
        let events = RefCell::new(Vec::new());
        let mut reaction = unresolved("session-1");
        run_react(&mut reaction, &roots, &events, false, false, None).unwrap();
        assert_eq!(events.borrow()[..3], ["resolve", "save", "graph"]);
        // A destination that cannot be persisted is not used: no notice, no
        // log, and nothing frozen in memory for the driver to save later.
        let mut other = unresolved("session-2");
        other.attempt = provenant_core::AttemptId::from_bytes([5; 16]);
        let events = RefCell::new(Vec::new());
        let error = run_react(&mut other, &roots, &events, false, false, Some(0)).unwrap_err();
        assert_eq!(error.to_string(), "db");
        assert!(!other.resolved && other.chair.is_none() && other.session.is_none());
        assert!(!other.notice_done && !other.log_done);
        assert_eq!(inbox(&roots).len(), 1);
    }

    #[test]
    fn replay_launch_inputs_come_from_the_host_not_from_the_row() {
        let state = std::path::Path::new("/trusted/state");
        let launch =
            crate::fabric::recovery_launch(state, std::path::Path::new("/trusted/cwd")).unwrap();
        // A forged row has nowhere to name an executable or a config root.
        assert!(launch.executables.is_empty());
        assert_eq!(launch.configuration_root, state);
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
