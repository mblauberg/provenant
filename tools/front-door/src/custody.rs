//! Command projection and host lifecycle effects for the single custody ledger.
use crate::{emit, fail, roots};
use provenant_cli::{Globals, Status};
use provenant_core::{CancelToken, Roots};
use provenant_fabric_store::{HostStore, Request};
use provenant_os::clock::SystemClock;
use provenant_records::{CommandResult, Outcome, custody::*};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};
type Error = Box<dyn std::error::Error>;

fn presence(
    record: &CustodyRecord,
    roots: &Roots,
    host: provenant_core::HostId,
    actor: provenant_core::ActorId,
) -> HolderPresence {
    if let Holder::Attempt { attempt, .. } = record.holder {
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let context = provenant_run::ExecutionContext {
            state_root: roots.state.clone(),
            project_id: record.project,
            actor_id: actor,
            read_prompts: false,
        };
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        return match provenant_run::RunStore::open_read_only(&context, &call) {
            Ok(Some(store)) => match store.record(attempt, &call) {
                Ok(attempt)
                    if matches!(
                        attempt.state,
                        provenant_records::run::AttemptState::Terminal { .. }
                    ) =>
                {
                    HolderPresence::Unattended
                }
                Ok(attempt) => {
                    if matches!(
                        attempt.state,
                        provenant_records::run::AttemptState::Queued { .. }
                            | provenant_records::run::AttemptState::Starting { .. }
                    ) {
                        return HolderPresence::Unknown;
                    }
                    let path = roots
                        .state
                        .as_path()
                        .join("attempts")
                        .join(attempt.attempt_id.to_string());
                    match provenant_os::fs::Directory::open_anchor(&path) {
                        Ok(directory)
                            if provenant_supervisor::custody::holder_gone(
                                &directory,
                                provenant_supervisor::custody::Scope {
                                    attempt: attempt.attempt_id,
                                    generation: attempt.generation,
                                },
                                &provenant_supervisor::census::SystemProcessObserver,
                            ) =>
                        {
                            HolderPresence::Unattended
                        }
                        _ => HolderPresence::Unknown,
                    }
                }
                Err(_) => HolderPresence::Unknown,
            },
            Ok(None) => HolderPresence::Unattended,
            Err(_) => HolderPresence::Unknown,
        };
    }
    let Some(chair) = record.holder.chair().filter(|chair| chair.host == host) else {
        return HolderPresence::Unknown;
    };
    let Some(process) = &chair.process else {
        return HolderPresence::Unknown;
    };
    use provenant_supervisor::census::{
        ProcessObservation, ProcessObserver, SystemProcessObserver,
    };
    match SystemProcessObserver.observe_topology(process.pid) {
        ProcessObservation::Present(fact)
            if !fact.zombie
                && fact.identity.boot == process.boot
                && fact.identity.native_start == process.native_start =>
        {
            HolderPresence::Live
        }
        ProcessObservation::Present(_) | ProcessObservation::Absent => HolderPresence::Unattended,
        ProcessObservation::Unknown(_) => HolderPresence::Unknown,
    }
}
fn context(
    resolved: &roots::Resolved,
    read_only: bool,
) -> Result<provenant_cli::fabric::Context, Error> {
    Ok(if read_only {
        provenant_cli::fabric::Context::open_read_only(
            resolved.roots.clone(),
            &resolved.home,
            &resolved.project,
        )?
    } else {
        provenant_cli::fabric::Context::open(
            resolved.roots.clone(),
            &resolved.home,
            &resolved.project,
        )?
    })
}
pub fn acquire(resolved: &roots::Resolved, subjects: &[Subject]) -> Result<(), Error> {
    let context = context(resolved, false)?;
    provenant_cli::fabric::acquire_custody(
        &context,
        &Holder::Chair {
            chair: provenant_cli::fabric::current_chair(&context),
        },
        subjects,
    );
    Ok(())
}
pub fn collect_cloud(
    resolved: &roots::Resolved,
    key: &str,
    branch: &str,
    worktree: &str,
) -> Result<(), Error> {
    let context = context(resolved, false)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let ledger = HostStore::open(&resolved.roots.state, &call)?;
    let key = Subject::new(SubjectKind::CloudKey, key);
    let holder = Holder::Chair {
        chair: provenant_cli::fabric::current_chair(&context),
    };
    let subjects = [
        key.clone(),
        Subject::new(SubjectKind::Branch, branch),
        Subject::new(SubjectKind::Worktree, worktree),
    ];
    let changes = ledger.acquire_custody_group(
        context.identity.project,
        &subjects,
        &holder,
        Origin::Provenant,
        &call,
    )?;
    provenant_fabric_runtime::custody::warn(&changes);
    ledger.change_custody_state(
        context.identity.project,
        &key,
        CustodyState::Released,
        holder.actor(),
        &call,
    )?;
    Ok(())
}

pub fn command(globals: &Globals, subject: Subject, release: bool) -> u8 {
    let leaf = if release { "release" } else { "adopt" };
    let action = || -> Result<CustodyRecord, Error> {
        if provenant_cli::fabric::in_lane() {
            return Err("custody changes require the host chair".into());
        }
        let resolved = roots::resolve(
            globals,
            roots::Workspace::Checkout,
            roots::Product::Required,
        )?;
        let context = context(&resolved, false)?;
        let chair = provenant_cli::fabric::current_chair(&context);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let ledger = HostStore::open(&resolved.roots.state, &call)?;
        if release {
            if let Some(old) = ledger.custody_item(context.identity.project, &subject, &call)?
                && !old.holder.belongs_to(&chair)
            {
                provenant_fabric_runtime::custody::warning(format_args!(
                    "acting on {} held by {:?}; action recorded",
                    subject, old.holder
                ));
            }
            Ok(ledger.change_custody_state(
                context.identity.project,
                &subject,
                CustodyState::Released,
                Some(chair.actor),
                &call,
            )?)
        } else {
            let change = ledger.acquire_custody(
                context.identity.project,
                &subject,
                &Holder::Chair { chair },
                Origin::Adopted,
                &call,
            )?;
            provenant_fabric_runtime::custody::warn(std::slice::from_ref(&change));
            Ok(change.record)
        }
    };
    match action() {
        Ok(record) => emit(
            &CommandResult::new(leaf, Outcome::Ok, json!({"record":record})),
            globals,
            &format!("{leaf} {subject}\n"),
            false,
        ),
        Err(error) => fail(leaf, Outcome::Error, "CUSTODY", &error.to_string(), globals),
    }
}
// The bounded read-only store boundary uses a real I/O deadline.
#[allow(clippy::disallowed_methods)]
pub fn status(args: &Status, globals: &Globals) -> u8 {
    let read = || -> Result<(serde_json::Value, String), Error> {
        let resolved = roots::resolve(
            globals,
            roots::Workspace::Checkout,
            roots::Product::Required,
        )?;
        let context = context(&resolved, true)?;
        let mut chair = provenant_cli::fabric::current_chair(&context);
        if let Some(session) = &args.custody_session {
            chair.session = Some(session.clone());
        }
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let ledger = HostStore::open_read_only(
            &resolved.roots.state,
            Instant::now() + Duration::from_secs(2),
        )?;
        let query = CustodyQuery {
            project: globals.project.as_ref().map(|_| context.identity.project),
            state: Some(StateKind::Active),
            after: args.cursor.unwrap_or(0),
            limit: args.limit.min(100),
            ..Default::default()
        };
        let page = match ledger {
            Some(ledger) => ledger.custody(&query, &call)?,
            None => CustodyPage {
                items: Vec::new(),
                next: None,
            },
        };
        let mut items = Vec::new();
        let mut lines = Vec::new();
        for record in page.items {
            let presence = presence(
                &record,
                &resolved.roots,
                context.identity.inbox.host,
                context.identity.inbox.actor,
            );
            let mine = record.holder.belongs_to(&chair);
            if !mine && presence != HolderPresence::Unattended {
                continue;
            }
            lines.push(if globals.full {
                format!(
                    "{} project={} {:?} {:?}",
                    record.subject, record.project, presence, record.holder
                )
            } else {
                format!(
                    "{} {}",
                    record.subject,
                    if presence == HolderPresence::Unattended {
                        "unattended"
                    } else {
                        "active"
                    }
                )
            });
            items.push(CustodyView {
                record,
                presence,
                mine,
            });
        }
        if let Some(cursor) = page.next {
            lines.push(format!("more: provenant status --mine --cursor {cursor}"));
        }
        if lines.is_empty() {
            lines.push("no active custody".into());
        }
        Ok((
            serde_json::to_value(CustodyStatus {
                items,
                next: page.next,
            })?,
            format!("{}\n", lines.join("\n")),
        ))
    };
    match read() {
        Ok((data, human)) => emit(
            &CommandResult::new("status", Outcome::Ok, data),
            globals,
            &human,
            true,
        ),
        Err(error) => fail(
            "status",
            Outcome::Error,
            "CUSTODY",
            &error.to_string(),
            globals,
        ),
    }
}

/// Settlement releases the attempt, retaining unlanded writer artefacts for adoption.
pub fn settle_attempt(
    roots: &Roots,
    record: &provenant_records::run::AttemptRecord,
    request: &provenant_records::run::CanonicalRequest,
    actor: provenant_core::ActorId,
) -> Result<(), Error> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let ledger = HostStore::open(&roots.state, &call)?;
    let project = record.project_id;
    let subject = Subject::new(SubjectKind::Attempt, record.attempt_id.to_string());
    let previous = ledger.custody_item(project, &subject, &call)?;
    let holder = previous
        .as_ref()
        .map(|item| item.holder.clone())
        .unwrap_or(Holder::Attempt {
            attempt: record.attempt_id,
            chair: request.chair.clone(),
        });
    if let Some(delivery) = &record.delivery {
        if let provenant_records::run::WriterDeliveryState::Delivered {
            pull_request: Some(pr),
            ..
        } = &delivery.state
        {
            let id = pr
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .ok_or("empty PR reference")?;
            let subjects = [
                Subject::new(SubjectKind::Pr, id),
                Subject::new(SubjectKind::Branch, &delivery.branch),
            ];
            // Replaying settlement must not reactivate landed or released artefacts.
            for subject in &subjects {
                if ledger.custody_item(project, subject, &call)?.is_none() {
                    ledger.acquire_custody(project, subject, &holder, Origin::Provenant, &call)?;
                }
            }
            ledger.link_custody(project, &subjects, &call)?;
        }
        if request.task.deliver.as_ref().is_some_and(|intent| {
            matches!(
                intent.mode,
                provenant_records::run::DeliveryMode::Integrate(_)
            )
        }) && let Some(integration) = &delivery.integration
        {
            let evidence = LandedEvidence {
                predicate: LandingPredicate::Landed,
                source: LandingSource::IntegrationTarget {
                    reference: integration.target.clone(),
                    commit: integration.integrated,
                },
            };
            if let Some(item) = &previous {
                for subject in &item.links {
                    if matches!(subject.kind, SubjectKind::Branch | SubjectKind::Worktree)
                        && ledger.custody_item(project, subject, &call)?.is_some()
                    {
                        ledger.change_custody_state(
                            project,
                            subject,
                            CustodyState::Landed {
                                evidence: evidence.clone(),
                            },
                            Some(actor),
                            &call,
                        )?;
                    }
                }
            }
        }
    }
    if previous.is_some() {
        ledger.change_custody_state(
            project,
            &subject,
            CustodyState::Released,
            Some(actor),
            &call,
        )?;
    }
    Ok(())
}

pub fn settle_graph(
    roots: &Roots,
    project: provenant_core::ProjectId,
    graph: &str,
    actor: provenant_core::ActorId,
) -> Result<(), Error> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let ledger = HostStore::open(&roots.state, &call)?;
    let subject = Subject::new(SubjectKind::Graph, graph);
    if ledger.custody_item(project, &subject, &call)?.is_some() {
        ledger.change_custody_state(
            project,
            &subject,
            CustodyState::Released,
            Some(actor),
            &call,
        )?;
    }
    Ok(())
}

/// Called once at the landing owner's settlement point; no deletion or retention policy.
pub fn settle_landing(
    project: Option<&Path>,
    state: &Path,
    record: &provenant_records::landing::LandingRecord,
) -> Result<(), Error> {
    let Some(project) = project else {
        return Ok(());
    };
    let globals = Globals {
        project: Some(project.to_owned()),
        state_dir: Some(state.to_owned()),
        ..Default::default()
    };
    let resolved = roots::resolve(
        &globals,
        roots::Workspace::Checkout,
        roots::Product::Required,
    )?;
    let context = context(&resolved, false)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let ledger = HostStore::open(&resolved.roots.state, &call)?;
    let holder = Holder::Chair {
        chair: provenant_cli::fabric::current_chair(&context),
    };
    for member in &record.request.members {
        let subject = Subject::new(SubjectKind::Pr, member.pr.to_string());
        if ledger
            .custody_item(context.identity.project, &subject, &call)?
            .is_none()
        {
            ledger.acquire_custody(
                context.identity.project,
                &subject,
                &holder,
                Origin::Provenant,
                &call,
            )?;
        }
        if let Some(provenant_records::landing::MemberOutcome::Landed { commit }) =
            record.outcomes.get(&member.pr)
        {
            let evidence = LandedEvidence {
                predicate: LandingPredicate::Landed,
                source: LandingSource::ForgeMerged {
                    pr: member.pr.to_string(),
                    head: member
                        .head
                        .as_deref()
                        .ok_or("landed member has no pinned head")?
                        .parse()?,
                    merge_commit: commit.as_deref().map(str::parse).transpose()?,
                },
            };
            let pr = ledger
                .custody_item(context.identity.project, &subject, &call)?
                .ok_or("PR custody missing")?;
            let mut artefacts = pr.links.clone();
            artefacts.push(subject.clone());
            let mut index = 0;
            while index < artefacts.len() {
                let item = artefacts[index].clone();
                index += 1;
                if !matches!(
                    item.kind,
                    SubjectKind::Pr | SubjectKind::Branch | SubjectKind::Worktree
                ) {
                    continue;
                }
                if let Some(linked) = ledger.custody_item(context.identity.project, &item, &call)? {
                    for link in linked.links {
                        if matches!(link.kind, SubjectKind::Branch | SubjectKind::Worktree)
                            && !artefacts.contains(&link)
                        {
                            artefacts.push(link);
                        }
                    }
                    ledger.change_custody_state(
                        context.identity.project,
                        &item,
                        CustodyState::Landed {
                            evidence: evidence.clone(),
                        },
                        holder.actor(),
                        &call,
                    )?;
                }
            }
        }
    }
    Ok(())
}
