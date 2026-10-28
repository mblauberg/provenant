//! Local delivery observations without Context::open, Git or writable state.
use provenant_core::StateRoot;
use provenant_fabric_runtime::{config::FabricConfig, identity::inbox_address};
use provenant_fabric_store::{Page, Store};
use provenant_os::fs::Directory;
use provenant_records::{Harness, inbox::*};
use provenant_workflow::session::{HostSessionId, discover_session_at};
use std::{ffi::OsStr, path::Path, time::Instant};

type Error = Box<dyn std::error::Error>;

pub struct LocalReader<'a> {
    pub root: &'a StateRoot,
    pub workspace: &'a Path,
    pub config: &'a FabricConfig,
    pub agent_label: Option<&'a OsStr>,
    pub harness: Harness,
    pub in_lane: bool,
}

impl LocalReader<'_> {
    /// Validate an explicitly registered workspace/session before opening an
    /// existing read-only store. Missing or stale registration returns no offers.
    /// Deadlines are cooperative, checked between steps and during SQLite work.
    /// Filesystem calls cannot be interrupted; callers needing a hard bound must
    /// run the reader off-thread with a timeout and discard late results.
    pub fn read(
        &self,
        session: &str,
        max_bytes: usize,
        deadline: Instant,
    ) -> Result<Page<AnswerOffer>, Error> {
        let empty = Page {
            items: Vec::new(),
            truncated: false,
        };
        let Some(target) = self.target(session, deadline)? else {
            return Ok(empty);
        };
        let Some(store) = Store::open_read_only(self.root, deadline)? else {
            return Ok(empty);
        };
        Ok(store.read_human_answers(&target, MAX_LOCAL_OFFER_ITEMS, max_bytes, deadline)?)
    }

    /// Resolve the registered target with the same cooperative deadline as read.
    pub fn target(&self, session: &str, deadline: Instant) -> Result<Option<AnswerTarget>, Error> {
        check(deadline)?;
        if self.in_lane {
            return Ok(None);
        }
        let session = HostSessionId::parse(session)?;
        let Some(found) = discover_session_at(self.root, self.workspace, &session, deadline)?
        else {
            return Ok(None);
        };
        let host =
            Directory::open_anchor(self.root.as_path())?.read_bounded(OsStr::new("host-id"), 32)?;
        let host = std::str::from_utf8(&host)?.parse()?;
        let address = inbox_address(
            host,
            self.config,
            self.agent_label,
            Some(self.harness.spec().name),
        )?;
        if found.record.host != host || found.record.chair_actor != address.actor {
            return Ok(None);
        }
        // Without the regular federation context, defer configured remote homes
        // to Fabric mail rather than guessing whether a local mirror is current.
        if !self.config.home_hosts.is_empty() {
            return Ok(None);
        }
        Ok(Some(AnswerTarget {
            host,
            project: found.record.project_id,
            session: session.as_str().into(),
            actor: address.actor,
        }))
    }
}

#[allow(clippy::disallowed_methods)]
fn check(deadline: Instant) -> Result<(), Error> {
    if Instant::now() >= deadline {
        return Err("local answer deadline expired".into());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Real deadlines are the contract under test.
mod tests {
    // The custom worktree test driver includes lib.rs without the test harness.
    #[allow(unused_imports)]
    use super::*;
    #[allow(unused_imports)]
    use provenant_core::{ActorId, CancelToken, Clock, ProjectId, RootInputs, WaitBudget};
    #[allow(unused_imports)]
    use provenant_fabric_runtime::{
        identity::Identity,
        inbox::{InboxInput, InboxResult, InboxService},
    };
    #[allow(unused_imports)]
    use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
    #[allow(unused_imports)]
    use std::{fs, time::Duration};

    #[test]
    fn local_answers_require_current_exact_registration_and_repeat_until_ack() -> Result<(), Error>
    {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let workspace = base.join("workspace");
        fs::create_dir(&workspace)?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("assets"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&SystemPathResolver)?;
        let config =
            provenant_config::read_with_domains(&roots, &provenant_cli::domains::SECTIONS)?
                .domains
                .fabric;
        let reader = LocalReader {
            root: &roots.state,
            workspace: &workspace,
            config: &config,
            agent_label: None,
            harness: Harness::Claude,
            in_lane: false,
        };
        let deadline = || Instant::now() + Duration::from_millis(100);
        assert!(
            reader
                .read("chair-a", MAX_OFFER_BYTES, deadline())?
                .items
                .is_empty()
        );
        assert!(!roots.state.as_path().exists());
        fs::create_dir(roots.state.as_path())?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(roots.state.as_path(), fs::Permissions::from_mode(0o700))?;
        let host = provenant_config::bind_host(&roots)?;
        let actor = inbox_address(host, &config, None, Some("claude"))?.actor;
        let project = ProjectId::from_bytes([2; 16]);
        let session = HostSessionId::parse("chair-a")?;
        let store = provenant_workflow::session::SessionStore::new(
            roots.state.clone(),
            project,
            session.clone(),
        )
        .with_chair(host, actor)
        .with_workspace(workspace.clone());
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = || WaitBudget::cli(clock.mono_now(), 1).unwrap();
        store.save(
            b"---\nprovenant_summary:\n  schema_version: 1\n  revision: 1\n  updated_at: \"2026-10-28T03:00:00Z\"\n  now: \"Working\"\n  next: \"Continue\"\n  needs_you: []\n  risk: \"None\"\n---\ncheckpoint",
            budget(),
            &cancel,
            &clock,
        )?;
        assert!(store.discovery()?.unwrap().record.summary.is_some());
        let identity = Identity {
            project,
            host,
            inbox: inbox_address(host, &config, None, Some("claude"))?,
            client: "claude".into(),
            cwd: provenant_records::AbsolutePath::new(&workspace)?,
            common_dir: provenant_records::AbsolutePath::new(&workspace)?,
            federation: None,
        };
        let service = InboxService::open(&roots.state, &identity, &clock, budget(), &cancel)?;
        let created = service.execute(
            InboxInput::Ask {
                session: "chair-a".into(),
                operation_id: None,
                request: Box::new(RequestDraft {
                    deadline: None,
                    supersedes: None,
                    work: None,
                    kind: RequestKind::Question,
                    title: "Next?".into(),
                    body: "Choose".into(),
                    blocking: false,
                    options: vec![],
                    recommended_option_id: None,
                    proceeding_with: None,
                }),
            },
            budget(),
            &cancel,
        )?;
        let InboxResult::Saved { receipt } = created else {
            return Err("ask receipt".into());
        };
        let answered = service.execute(
            InboxInput::Answer {
                id: receipt.request_id,
                action: AnswerAction::Reply,
                option_id: None,
                text: Some("Yes \"🦀\"\n".into()),
                expect_revision: receipt.revision,
                channel: InteractionChannel::Console,
                operation_id: None,
            },
            budget(),
            &cancel,
        )?;
        let InboxResult::Saved { receipt } = answered else {
            return Err("answer receipt".into());
        };
        let first = reader.read("chair-a", MAX_OFFER_BYTES, deadline())?;
        assert_eq!(first.items.len(), 1);
        assert_eq!(reader.read("chair-a", MAX_OFFER_BYTES, deadline())?, first);
        assert_eq!(first.items[0].delivery.state, AnswerDeliveryState::Queued);
        assert_eq!(first.items[0].delivery.attempts, 0);
        assert!(
            LocalReader {
                in_lane: true,
                ..reader
            }
            .read("chair-a", MAX_OFFER_BYTES, deadline())?
            .items
            .is_empty()
        );
        assert!(
            LocalReader {
                agent_label: Some(OsStr::new("other")),
                ..reader
            }
            .read("chair-a", MAX_OFFER_BYTES, deadline())?
            .items
            .is_empty()
        );
        assert!(
            LocalReader {
                workspace: &base,
                ..reader
            }
            .read("chair-a", MAX_OFFER_BYTES, deadline())?
            .items
            .is_empty()
        );
        assert!(
            reader
                .read(
                    "chair-a",
                    MAX_OFFER_BYTES,
                    Instant::now() - Duration::from_millis(1)
                )
                .is_err()
        );
        let checkpoint = roots.state.as_path().join(format!(
            "workflow/projects/{project}/sessions/chair-a/checkpoint.md"
        ));
        fs::write(&checkpoint, b"interrupted")?;
        assert!(
            reader
                .read("chair-a", MAX_OFFER_BYTES, deadline())?
                .items
                .is_empty()
        );
        store.save(b"repaired", budget(), &cancel, &clock)?;
        assert_eq!(
            reader
                .read("chair-a", MAX_OFFER_BYTES, deadline())?
                .items
                .len(),
            1
        );
        let rebound =
            provenant_workflow::session::SessionStore::new(roots.state.clone(), project, session)
                .with_chair(host, ActorId::from_bytes([99; 16]))
                .with_rebind(true);
        rebound.save(b"rebound", budget(), &cancel, &clock)?;
        assert!(
            reader
                .read("chair-a", MAX_OFFER_BYTES, deadline())?
                .items
                .is_empty()
        );
        store
            .with_rebind(true)
            .save(b"returned", budget(), &cancel, &clock)?;
        service.execute(
            InboxInput::Acknowledge {
                id: receipt.request_id,
                answer_id: receipt.answer_id.unwrap(),
                session: "chair-a".into(),
                operation_id: None,
            },
            budget(),
            &cancel,
        )?;
        assert!(
            reader
                .read("chair-a", MAX_OFFER_BYTES, deadline())?
                .items
                .is_empty()
        );
        Ok(())
    }
}
