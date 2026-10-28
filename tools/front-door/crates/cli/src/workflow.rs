//! Workflow leaves: review coverage, receipts and session checkpoints. The edge
//! gathers owner facts (run attempts, trusted actor, clock) and hands workflow
//! plain values; request JSON never supplies verified facts or authority.
use crate::{Globals, RenderMode};
use clap::{Args, Subcommand};
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock, DeliveryId, DurationMicros, ProjectId, Roots,
    UtcMicros, WaitBudget,
};
use provenant_os::clock::SystemClock;
use provenant_records::{CommandError, CommandResult, Outcome, workflow::*};
use provenant_run::{AttemptReader, ExecutionContext, Request, RunStore};
use provenant_workflow::{
    bind::{self, ReviewClaim},
    coverage::{self, LegBlocker, ReviewContext, Status},
    receipt::{self, ReceiptInput},
    risk,
    session::{HostSessionId, SessionStore},
    store::{ReceiptStore, SaveContext},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io, io::Read, path::Path};

const REQUEST_LIMIT: u64 = 256 * 1024;
const FACT_LIFETIME_SECONDS: u64 = 300;
const WAIT_SECONDS: u64 = 30;

#[derive(Debug, Subcommand)]
pub enum Workflow {
    /// Check bound reviews against the receipt's risk tier.
    Review(Input),
    /// Show and save delivery receipts
    #[command(subcommand)]
    Receipt(Receipt),
    /// Save and show session checkpoints
    #[command(subcommand)]
    Session(Session),
}
#[derive(Debug, Args)]
pub struct Input {
    /// Request JSON file, or `-` for standard input.
    #[arg(long)]
    pub request: String,
}
#[derive(Debug, Subcommand)]
pub enum Receipt {
    /// Show a delivery receipt
    Show {
        /// Delivery ID
        delivery_id: DeliveryId,
    },
    /// Save a delivery receipt
    Save {
        /// Delivery ID
        delivery_id: DeliveryId,
        /// Zero creates the receipt; otherwise the stored revision.
        #[arg(long)]
        expected_revision: u64,
        #[command(flatten)]
        input: Input,
    },
}
#[derive(Debug, Subcommand)]
pub enum Session {
    /// Save a session checkpoint (up to 256 KiB)
    Save {
        /// Session ID
        session_id: String,
        /// Markdown checkpoint file, or `-` for standard input.
        #[arg(long)]
        file: String,
        /// Compatibility option; chair identity uses the configured Fabric label
        #[arg(long, value_parser = crate::install::harness_parser())]
        client: Option<provenant_records::Harness>,
        /// Replace the registered chair binding and record its previous identity
        #[arg(long)]
        rebind: bool,
    },
    /// Show a session checkpoint, one of its retained revisions or their list
    Show {
        /// Session ID
        session_id: String,
        /// Show this retained revision's text instead of the current one
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        rev: Option<u64>,
        /// List the retained revisions with their parents and hashes
        #[arg(long, conflicts_with = "rev")]
        revisions: bool,
    },
    /// Show what changed between a retained revision and the current checkpoint
    Diff {
        /// Session ID
        session_id: String,
        /// Revision to compare with the current checkpoint
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        rev: u64,
    },
    /// Hand this session to a fresh Claude session on a peer host
    #[command(long_about = HANDOFF_ABOUT)]
    Handoff(Box<Handoff>),
}
const HANDOFF_ABOUT: &str = "Hand this chair or task session to a fresh Claude session on a peer \
Fabric host that has opted in with `provenant config set fabric.accept_handoff true`; it is off \
by default and a peer that has not enabled it refuses with HANDOFF_DISABLED. The peer gets a generated prompt, not the transcript: the session checkpoint, each \
branch with its pushed commit, the lanes still running here and any notes. The peer checks the \
account against its own registrations, recreates each branch's worktree from origin, stores the \
prompt and opens Terminal running `claude` under that account.

Worktrees must be clean and pushed. --wip commits their tracked changes and pushes them first; \
--allow-dirty proceeds and lists them in the prompt. --dry-run prints the prompt and plan and \
sends nothing. The checkpoint is saved after the peer accepts. The owner lease moves, on both \
hosts, only when Terminal opened or --no-launch was given. If the peer stored the hand-off but \
Terminal did not open, the command fails with LAUNCH_FAILED, keeps the lease here and names the \
launch script to run on the peer.";
/// `workflow session handoff`. The binary composes it: it needs worktree and
/// peer owners that this crate does not hold.
#[derive(Debug, Args)]
pub struct Handoff {
    /// Peer host, by label or host ID
    #[arg(long)]
    pub to: String,
    /// Registered Claude account on the peer
    #[arg(long)]
    pub account: String,
    /// Add a line to the prompt (repeatable)
    #[arg(long = "note", value_name = "TEXT")]
    pub notes: Vec<String>,
    /// Send this file with the prompt (repeatable; 512 KiB in total)
    #[arg(long = "attach", value_name = "PATH")]
    pub attachments: Vec<std::path::PathBuf>,
    /// Use this Markdown file as the prompt body instead of the generated one
    #[arg(long, value_name = "PATH")]
    pub prompt_file: Option<std::path::PathBuf>,
    /// Neither read nor save the session checkpoint
    #[arg(long)]
    pub no_checkpoint: bool,
    /// Provider for the new session; only `claude` is supported
    #[arg(long, default_value = "claude")]
    pub provider: String,
    /// Model for the new session
    #[arg(long)]
    pub model: Option<String>,
    /// Commit and push dirty or unpushed worktrees as WIP first
    #[arg(long, conflicts_with = "allow_dirty")]
    pub wip: bool,
    /// Proceed with dirty or unpushed worktrees and list them in the prompt
    #[arg(long)]
    pub allow_dirty: bool,
    /// Print the prompt and plan without sending anything
    #[arg(long)]
    pub dry_run: bool,
    /// Deliver everything but do not open Terminal on the peer
    #[arg(long)]
    pub no_launch: bool,
    /// Source session ID; defaults to CLAUDE_CODE_SESSION_ID, then CLAUDE_SESSION_ID
    #[arg(long)]
    pub session_id: Option<String>,
    /// Compatibility option; uses the same configured chair actor as session save
    #[arg(long, value_parser = crate::install::harness_parser())]
    pub client: Option<provenant_records::Harness>,
}
impl Workflow {
    pub fn path(&self) -> &'static str {
        match self {
            Self::Review(_) => "workflow review",
            Self::Receipt(Receipt::Show { .. }) => "workflow receipt show",
            Self::Receipt(Receipt::Save { .. }) => "workflow receipt save",
            Self::Session(Session::Save { .. }) => "workflow session save",
            Self::Session(Session::Show { .. }) => "workflow session show",
            Self::Session(Session::Diff { .. }) => "workflow session diff",
            Self::Session(Session::Handoff(_)) => "workflow session handoff",
        }
    }
    fn read_only(&self) -> bool {
        matches!(
            self,
            Self::Review(_)
                | Self::Receipt(Receipt::Show { .. })
                | Self::Session(Session::Show { .. })
                | Self::Session(Session::Diff { .. })
        )
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("{message}")]
    Refused {
        code: &'static str,
        message: String,
        data: Value,
    },
    #[error("not found")]
    Missing,
    #[error(transparent)]
    Context(#[from] crate::fabric::EdgeError),
    #[error(transparent)]
    Store(#[from] provenant_workflow::store::StoreError),
    #[error(transparent)]
    Session(#[from] provenant_workflow::session::SessionError),
    #[error(transparent)]
    Run(#[from] provenant_run::RunError),
    #[error(transparent)]
    Time(#[from] provenant_core::TimeError),
    #[error(transparent)]
    Io(#[from] io::Error),
}
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewInput {
    attempt_id: AttemptId,
    #[serde(flatten)]
    claim: ReviewClaim,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockerInput {
    reference: EvidenceRef,
    expires_at: UtcMicros,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalInput {
    actor_id: ActorId,
    evidence: EvidenceRef,
}
impl From<ApprovalInput> for UserApproval {
    fn from(a: ApprovalInput) -> Self {
        UserApproval {
            actor_id: a.actor_id,
            evidence: a.evidence,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewRequest {
    /// Delivery ID
    delivery_id: DeliveryId,
    #[serde(default)]
    reviews: Vec<ReviewInput>,
    #[serde(default)]
    blockers: Vec<BlockerInput>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RiskInput {
    factors: RiskFactors,
    tier: RiskTier,
    downgrade_approval: Option<ApprovalInput>,
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum DecisionInput {
    Pending {},
    Accepted {
        reason: String,
        user_approval: Option<ApprovalInput>,
    },
    Abandoned {
        reason: String,
        user_approval: Option<ApprovalInput>,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveRequest {
    subject: Subject,
    authors: Vec<Author>,
    risk: RiskInput,
    #[serde(default)]
    reviews: Vec<ReviewInput>,
    #[serde(default)]
    blockers: Vec<BlockerInput>,
    decision: DecisionInput,
}

// Stdin reads have a real transport deadline.
#[allow(clippy::disallowed_methods)]
fn read_input(source: &str) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    if source == "-" {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(WAIT_SECONDS);
        bytes = provenant_os::sys::hook_io::read_stdin(REQUEST_LIMIT as usize, deadline)?;
    } else {
        std::fs::File::open(Path::new(source))?
            .take(REQUEST_LIMIT + 1)
            .read_to_end(&mut bytes)?;
    }
    if bytes.len() as u64 > REQUEST_LIMIT {
        return Err(invalid("input exceeds 256 KiB"));
    }
    Ok(bytes)
}
fn request<T: serde::de::DeserializeOwned>(source: &str) -> Result<T, Error> {
    serde_json::from_slice(&read_input(source)?)
        .map_err(|e| invalid(format!("invalid request: {e}")))
}

struct Edge {
    roots: Roots,
    project: ProjectId,
    actor: ActorId,
    clock: SystemClock,
    cancel: CancelToken,
}
impl Edge {
    fn budget(&self) -> Result<WaitBudget, Error> {
        Ok(WaitBudget::cli(self.clock.mono_now(), WAIT_SECONDS)?)
    }
    /// Bind each claim to run's verified attempt fact for this project.
    fn bind(
        &self,
        reviews: &[ReviewInput],
        subject: &Subject,
        delivery: DeliveryId,
    ) -> Result<Vec<ReviewFact>, Error> {
        if reviews.is_empty() {
            return Ok(vec![]);
        }
        let budget = self.budget()?;
        let call = Request {
            clock: &self.clock,
            budget,
            cancel: &self.cancel,
        };
        let store = RunStore::open(
            &ExecutionContext {
                state_root: self.roots.state.clone(),
                project_id: self.project,
                actor_id: self.actor,
                read_prompts: false,
            },
            &call,
        )?;
        let policy = ReviewPolicy::default();
        let catalogue =
            provenant_routing::Catalogue::bundled().map_err(|e| invalid(e.to_string()))?;
        reviews
            .iter()
            .map(|r| {
                let mut attempt = store.attempt(r.attempt_id, &call)?;
                if let Some(reviewer) = attempt.reviewer.as_mut() {
                    reviewer.family = catalogue_family(&catalogue, reviewer.model.as_deref());
                }
                bind::review_fact(&attempt, &r.claim, subject, delivery, &policy).map_err(|e| {
                    Error::Refused {
                        code: "REVIEW_UNBOUND",
                        message: format!("attempt {}: {e}", r.attempt_id),
                        data: Value::Null,
                    }
                })
            })
            .collect()
    }
    fn report(
        &self,
        receipt: &DeliveryReceipt,
        facts: &[ReviewFact],
        blockers: Vec<BlockerInput>,
    ) -> Result<coverage::ValidationReport, Error> {
        let blockers: Vec<_> = blockers
            .into_iter()
            .map(|b| LegBlocker {
                reference: b.reference,
                expires_at: b.expires_at,
            })
            .collect();
        Ok(coverage::check(
            &ReviewContext {
                subject: receipt.subject(),
                authors: receipt.authors(),
                decision_makers: &[self.actor],
                risk: receipt.risk().tier(),
                now: self.clock.wall_now()?,
                policy: &ReviewPolicy::default(),
                blockers: &blockers,
            },
            facts,
        ))
    }
}

/// Model family from the routing catalogue for the observed `adapter/model`.
/// An adapter name is not a family: a model missing from the catalogue, or
/// listed without a family, stays unknown.
fn catalogue_family(
    catalogue: &provenant_routing::Catalogue,
    model: Option<&str>,
) -> Option<String> {
    let id = provenant_routing::ModelId::parse(model?).ok()?;
    catalogue.model(&id)?.family.clone()
}

fn execute(
    cmd: Workflow,
    globals: &Globals,
    roots: Roots,
    home: &Path,
    cwd: &Path,
) -> Result<Value, Error> {
    // Session reads resolve only an existing project binding. Fabric's writable
    // context would allocate host/project identity and a utility home here.
    if matches!(
        cmd,
        Workflow::Session(Session::Show { .. } | Session::Diff { .. })
    ) {
        let selected = globals.project.as_deref().unwrap_or(cwd);
        let selected =
            provenant_os::fs::canonicalize(selected).map_err(crate::fabric::EdgeError::from)?;
        return read_session(cmd, &roots, &selected);
    }
    let context = crate::fabric::Context::open(roots, home, cwd)?;
    if context.in_lane && matches!(cmd, Workflow::Session(Session::Save { .. })) {
        return Err(Error::Refused {
            code: "IN_LANE",
            message: "a lane cannot register or save a chair session".into(),
            data: Value::Null,
        });
    }
    let actor = match &cmd {
        Workflow::Session(Session::Save { client, .. }) => {
            session_actor(&context, *client).map_err(|e| invalid(e.to_string()))?
        }
        _ => context.identity.inbox.actor,
    };
    let project = context.identity.project;
    let roots = context.roots;
    let edge = Edge {
        roots,
        project,
        actor,
        clock: SystemClock::new(),
        cancel: CancelToken::new(),
    };
    match cmd {
        Workflow::Review(input) => {
            let req: ReviewRequest = request(&input.request)?;
            let stored = ReceiptStore::new(edge.roots.state.clone(), project, req.delivery_id)
                .read()?
                .ok_or(Error::Missing)?;
            let facts = edge.bind(&req.reviews, stored.subject(), req.delivery_id)?;
            let report = edge.report(&stored, &facts, req.blockers)?;
            Ok(serde_json::to_value(report).map_err(|e| invalid(e.to_string()))?)
        }
        Workflow::Receipt(Receipt::Show { delivery_id }) => {
            let stored = ReceiptStore::new(edge.roots.state.clone(), project, delivery_id)
                .read()?
                .ok_or(Error::Missing)?;
            Ok(serde_json::to_value(stored).map_err(|e| invalid(e.to_string()))?)
        }
        Workflow::Receipt(Receipt::Save {
            delivery_id,
            expected_revision,
            input,
        }) => save(
            &edge,
            delivery_id,
            expected_revision,
            request(&input.request)?,
        ),
        Workflow::Session(Session::Save {
            session_id,
            file,
            rebind,
            ..
        }) => {
            let id = HostSessionId::parse(&session_id).map_err(|e| invalid(e.to_string()))?;
            let text = read_input(&file)?;
            let keep = session_revisions(&edge.roots);
            let host = provenant_config::bind_host(&edge.roots)
                .map_err(|error| invalid(error.to_string()))?;
            let saved = SessionStore::new(edge.roots.state.clone(), project, id)
                .with_chair(host, actor)
                .with_rebind(rebind)
                .with_revisions(keep)
                .save_revision(&text, edge.budget()?, &edge.cancel, &edge.clock)?;
            Ok(serde_json::json!({
                "saved_bytes": text.len(),
                "limit_bytes": provenant_workflow::session::CHECKPOINT_LIMIT,
                "revision": saved.revision,
                "parent": saved.parent,
                "unchanged": saved.unchanged,
                "retained": saved.retained,
                "keep": keep,
                "warnings": saved.warnings,
            }))
        }
        Workflow::Session(Session::Show { .. } | Session::Diff { .. }) => unreachable_read(),
        Workflow::Session(Session::Handoff(_)) => Err(invalid(
            "workflow session handoff is composed by the provenant binary",
        )),
    }
}

fn unreachable_read() -> Result<Value, Error> {
    Err(invalid("session reads require the read-only context"))
}

/// Chair identity is independent of transport/client defaults. Read the stored
/// configuration so an MCP edge's client override cannot change the binding.
pub fn session_actor(
    context: &crate::fabric::Context,
    _client: Option<provenant_records::Harness>,
) -> Result<ActorId, String> {
    let config = provenant_config::read_with_domains(&context.roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?
        .domains
        .fabric;
    let explicit = std::env::var_os("PROVENANT_AGENT_LABEL").filter(|v| !v.is_empty());
    let label = match explicit.as_ref() {
        Some(label) => label
            .to_str()
            .ok_or_else(|| "agent label must be UTF-8".to_owned())?,
        None if !config.agent_label.is_empty() => &config.agent_label,
        None => &config.client_label,
    };
    provenant_fabric_runtime::identity::Address::new(context.identity.host, label)
        .map(|address| address.actor)
        .map_err(|e| e.to_string())
}

fn read_session(cmd: Workflow, roots: &Roots, cwd: &Path) -> Result<Value, Error> {
    use provenant_confinement::{UtilityBudget, UtilityRuntime};
    let git = provenant_os::git::git_executable().ok_or_else(|| invalid("Git unavailable"))?;
    let runtime = UtilityRuntime::read_only(git).map_err(|e| invalid(e.to_string()))?;
    let executor =
        provenant_os::exec::SystemExecutor::new(1).map_err(|error| invalid(error.to_string()))?;
    let budget = UtilityBudget::bounded(std::time::Duration::from_secs(5))
        .map_err(|e| invalid(e.to_string()))?;
    let location = runtime
        .project_location(&executor, cwd, budget, &CancelToken::new())
        .map_err(|e| invalid(e.to_string()))?
        .location;
    let project = provenant_config::lookup_project(roots, &location)
        .map_err(|e| invalid(e.to_string()))?
        .ok_or(Error::Missing)?
        .project_id();
    match cmd {
        Workflow::Session(Session::Show {
            session_id,
            rev,
            revisions,
        }) => {
            let id = HostSessionId::parse(&session_id).map_err(|e| invalid(e.to_string()))?;
            let store = SessionStore::new(roots.state.clone(), project, id);
            if revisions {
                let chain = store.revisions()?;
                if chain.is_empty() {
                    return Err(Error::Missing);
                }
                return Ok(serde_json::json!({ "revisions": chain }));
            }
            if let Some(rev) = rev {
                let found = store.read_revision(rev)?.ok_or(Error::Missing)?;
                return serde_json::to_value(found).map_err(|e| invalid(e.to_string()));
            }
            let checkpoint = store.read()?.ok_or(Error::Missing)?;
            let mut value = serde_json::to_value(checkpoint).map_err(|e| invalid(e.to_string()))?;
            // The current revision is context; unreadable history must not hide the text.
            value["revision"] = serde_json::json!(
                store
                    .revisions()
                    .ok()
                    .and_then(|chain| chain.last().map(|r| r.revision))
            );
            Ok(value)
        }
        Workflow::Session(Session::Diff { session_id, rev }) => {
            let id = HostSessionId::parse(&session_id).map_err(|e| invalid(e.to_string()))?;
            let diff = SessionStore::new(roots.state.clone(), project, id)
                .diff(rev)?
                .ok_or(Error::Missing)?;
            serde_json::to_value(diff).map_err(|e| invalid(e.to_string()))
        }
        _ => unreachable_read(),
    }
}
fn save(
    edge: &Edge,
    delivery: DeliveryId,
    expected: u64,
    req: SaveRequest,
) -> Result<Value, Error> {
    let now = edge.clock.wall_now()?;
    let policy = provenant_records::workflow::RiskPolicy::default();
    let downgrade: Option<UserApproval> = req.risk.downgrade_approval.map(Into::into);
    let risk = risk::assess(req.risk.factors, req.risk.tier, &policy, downgrade.as_ref())
        .map_err(|e| invalid(e.to_string()))?;
    let mut candidate = receipt::new(
        ReceiptInput {
            delivery_id: delivery,
            project_id: edge.project,
            subject: req.subject.clone(),
            authors: req.authors,
            risk,
        },
        now,
        &policy,
        &ReviewPolicy::default(),
        downgrade.as_ref(),
    )
    .map_err(|e| invalid(e.to_string()))?;
    let facts = edge.bind(&req.reviews, &req.subject, delivery)?;
    candidate = candidate
        .with_review_attempts(facts.iter().map(|f| f.attempt_id).collect())
        .map_err(|e| invalid(e.to_string()))?;
    let earliest_blocker = req.blockers.iter().map(|b| b.expires_at).min();
    let decision_at = edge.clock.wall_now()?;
    let record = |e: RecordError| invalid(e.to_string());
    candidate = match req.decision {
        DecisionInput::Pending {} => candidate,
        DecisionInput::Accepted {
            reason,
            user_approval,
        } => {
            let report = edge.report(&candidate, &facts, req.blockers)?;
            if report.status != Status::Passed {
                return Err(Error::Refused {
                    code: "REVIEW_INCOMPLETE",
                    message: "review coverage does not support acceptance".into(),
                    data: serde_json::to_value(report).unwrap_or(Value::Null),
                });
            }
            let approval = user_approval.map(UserApproval::from);
            candidate
                .with_decision(decision_at, |ctx| {
                    Decision::accept(ctx, edge.actor, decision_at, reason, approval.as_ref())
                })
                .map_err(record)?
        }
        DecisionInput::Abandoned {
            reason,
            user_approval,
        } => {
            let approval = user_approval.map(UserApproval::from);
            candidate
                .with_decision(decision_at, |ctx| {
                    Decision::abandon(ctx, edge.actor, decision_at, reason, approval.as_ref())
                })
                .map_err(record)?
        }
    };
    let mut valid_until = now.checked_add(DurationMicros::from_seconds(FACT_LIFETIME_SECONDS)?)?;
    if let Some(blocker) = earliest_blocker {
        valid_until = valid_until.min(blocker);
    }
    let saved = ReceiptStore::new(edge.roots.state.clone(), edge.project, delivery).save(
        &candidate,
        expected,
        SaveContext {
            subject: &req.subject,
            valid_until,
            risk_policy: &policy,
            downgrade_approval: downgrade.as_ref(),
        },
        edge.budget()?,
        &edge.cancel,
        &edge.clock,
    )?;
    #[derive(Serialize)]
    struct Saved {
        receipt: DeliveryReceipt,
        clock_regression: Option<bool>,
    }
    serde_json::to_value(Saved {
        receipt: saved.receipt,
        clock_regression: saved.clock_regression.map(|_| true),
    })
    .map_err(|e| invalid(e.to_string()))
}

fn classify(error: &Error) -> (Outcome, &'static str) {
    use provenant_workflow::store::StoreError as S;
    match error {
        Error::Invalid(_) => (Outcome::Invalid, "INVALID"),
        Error::Refused { code, .. } => (Outcome::Refused, code),
        Error::Missing => (Outcome::Failed, "MISSING"),
        Error::Store(S::Conflict { .. } | S::StaleSubject | S::Expired) => {
            (Outcome::Refused, "CONFLICT")
        }
        Error::Store(
            S::Malformed(_) | S::InvalidCandidate(_) | S::Version | S::Limit | S::Risk(_),
        ) => (Outcome::Invalid, "INVALID"),
        Error::Store(S::Cancelled) => (Outcome::Cancelled, "CANCELLED"),
        Error::Store(S::Deadline | S::Contention) => (Outcome::Timeout, "TIMEOUT"),
        Error::Session(provenant_workflow::session::SessionError::TooLarge) => {
            (Outcome::Invalid, "INVALID")
        }
        Error::Session(provenant_workflow::session::SessionError::History) => {
            (Outcome::Failed, "HISTORY")
        }
        Error::Session(provenant_workflow::session::SessionError::DiscoveryIdentity) => {
            (Outcome::Refused, "IDENTITY")
        }
        _ => (Outcome::Error, "IO"),
    }
}

/// Plain text for the revision views: a retained checkpoint as written, the
/// revision list one per line and a diff as a unified diff. Everything else
/// uses the generic rendering.
fn human_text(command: &str, data: &Value) -> String {
    let with_newline = |text: &str| {
        if text.ends_with('\n') {
            text.to_owned()
        } else {
            format!("{text}\n")
        }
    };
    match command {
        "workflow session diff" => match data["text"].as_str() {
            Some("") => "no differences\n".to_owned(),
            Some(text) => with_newline(text),
            None => crate::terse(data),
        },
        "workflow session show" if data["revision"].is_object() => data["markdown"]
            .as_str()
            .map_or_else(|| crate::terse(data), with_newline),
        "workflow session show" if data["revisions"].is_array() => data["revisions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                let parent = r["parent"]
                    .as_u64()
                    .map_or_else(|| "none".to_owned(), |p| p.to_string());
                let hash = r["sha256"].as_str().unwrap_or_default();
                format!(
                    "{} parent={parent} {hash} {} bytes {}\n",
                    r["revision"],
                    r["bytes"],
                    r["saved_at"].as_str().unwrap_or_default()
                )
            })
            .collect(),
        _ => crate::terse(data),
    }
}

/// How many revisions a session checkpoint keeps, from `[retention]`. Every
/// path that saves a checkpoint applies it. An unreadable config must not stop
/// a checkpoint from being saved, so it falls back to the default.
pub fn session_revisions(roots: &Roots) -> u64 {
    provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_or(provenant_workflow::session::REVISIONS_DEFAULT, |config| {
            config.retention.session_revisions
        })
}

pub fn run(cmd: Workflow, globals: &Globals, roots: &Roots, home: &Path, cwd: &Path) -> u8 {
    let command = cmd.path();
    let read_only = cmd.read_only();
    let result = execute(cmd, globals, roots.clone(), home, cwd);
    let mut out = match result {
        Ok(data) => CommandResult::new(command, Outcome::Ok, data),
        Err(error) => {
            let (status, code) = classify(&error);
            let data = match &error {
                Error::Refused { data, .. } => data.clone(),
                _ => Value::Null,
            };
            let mut out = CommandResult::new(command, status, data);
            out.error = Some(CommandError {
                code: code.into(),
                message: error.to_string(),
                cause: None,
                repair: None,
            });
            out
        }
    };
    // Incomplete or failed coverage is a result, not an error.
    if command == "workflow review"
        && out.error.is_none()
        && out.data.get("status").and_then(Value::as_str) != Some("passed")
    {
        out.status = Outcome::Failed;
    }
    let human = if out.error.is_none() {
        human_text(command, &out.data)
    } else {
        String::new()
    };
    let mode = if globals.json {
        RenderMode::Json
    } else {
        RenderMode::Human {
            text: &human,
            read_only,
        }
    };
    match crate::render_or_publish(
        &out,
        mode,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        |_| Err(io::Error::other("result too large")),
    ) {
        Ok(status) => crate::exit_status(&status),
        Err(_) => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::catalogue_family;
    use provenant_routing::Catalogue;

    #[test]
    fn family_comes_from_the_catalogue_and_is_unknown_otherwise() {
        let catalogue = Catalogue::bundled().unwrap();
        let known = catalogue
            .models()
            .iter()
            .find(|entry| entry.family.is_some())
            .expect("a catalogue model with a family");
        assert_eq!(
            catalogue_family(&catalogue, Some(known.model.as_str())),
            known.family
        );
        // A bare adapter name, an unlisted model and a missing model are unknown.
        assert_eq!(catalogue_family(&catalogue, Some("codex")), None);
        assert_eq!(
            catalogue_family(&catalogue, Some("codex/not-a-model")),
            None
        );
        assert_eq!(catalogue_family(&catalogue, None), None);
    }
}
