//! Fabric's clap subtree and edge composition. Runtime owns coordination policy.
use crate::metadata::CatalogueArg;
mod context;
pub mod graph_relay;

/// Shared identity/config setup for CLI leaves and the binary's fabric edges.
pub use context::{
    Context, PeerFailure, acquire_custody, chair_process, checkout_branch, current_chair,
    current_session, facade_outcome, in_lane, publish,
};

/// Trusted project and actor identity for sibling leaves such as workflow.
pub(crate) fn identity(
    roots: provenant_core::Roots,
    home: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<(provenant_core::Roots, provenant_core::ProjectId, ActorId), EdgeError> {
    let ctx = if in_lane() {
        Context::open_read_only(roots, home, cwd)?
    } else {
        Context::open(roots, home, cwd)?
    };
    Ok((ctx.roots, ctx.identity.project, ctx.identity.inbox.actor))
}

use crate::Globals;
use clap::{Args, Subcommand, ValueEnum};
use provenant_core::{ActorId, CancelToken, Clock, OperationId, WaitBudget};
use provenant_fabric_runtime::coordination::*;
use provenant_records::{CommandError, CommandResult, Outcome};
use serde::Serialize;
use serde_json::Value;
use std::{io, path::PathBuf};

#[derive(Debug, Subcommand)]
pub enum Fabric {
    /// Show this agent's project and inbox identity
    Identity,
    #[command(hide = true)]
    Peer {
        /// Handle the request in this process; never forward to the peer agent
        #[arg(long)]
        in_process: bool,
    },
    /// Serve peer requests from the login session (macOS LaunchAgent)
    #[command(hide = true)]
    PeerAgent,
    /// Send and read inbox messages
    #[command(subcommand)]
    Mail(Mail),
    /// Read and add to the activity log
    #[command(subcommand)]
    Events(Events),
    /// Claim issues, paths and worktrees
    #[command(subcommand)]
    Claim(Claim),
}
#[derive(Debug, Args)]
pub struct Operation {
    /// Caller-chosen ID that makes retries idempotent
    #[arg(long)]
    pub operation_id: OperationId,
}
#[derive(Debug, Args)]
pub struct SequencePage {
    /// Return entries after this sequence number
    #[arg(long, catalog_i64_range = 0..)]
    pub after: Option<i64>,
    /// Maximum entries (1 to 100)
    #[arg(long, default_value = "100", catalog_u64_range = 1..=100)]
    pub limit: u64,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Kind {
    Request,
    Response,
    Note,
}
#[derive(Debug, Subcommand)]
pub enum Mail {
    /// Send a message to one or more inboxes
    Send {
        /// Inbox label on this host, or HOST_ID/LABEL.
        #[arg(long, required = true)]
        to: Vec<String>,
        /// Message kind
        #[arg(long, value_enum, default_value = "note")]
        kind: Kind,
        /// Message body
        #[arg(long)]
        body: String,
        /// Message this replies to
        #[arg(long)]
        reply_to: Option<provenant_core::MessageId>,
        #[command(flatten)]
        operation: Operation,
    },
    /// Read inbox messages; --session reads that chair session's control mail
    Read {
        #[command(flatten)]
        page: SequencePage,
        /// Exact chair session whose mailbox to read, instead of this agent's
        #[arg(long)]
        session: Option<String>,
    },
    /// Mark messages read up to a sequence number
    MarkRead {
        /// Last sequence number to mark read
        #[arg(catalog_i64_range = 0..)]
        through: i64,
        /// Exact chair session whose mailbox to mark, instead of this agent's
        #[arg(long)]
        session: Option<String>,
        #[command(flatten)]
        operation: Operation,
    },
}
#[derive(Debug, Subcommand)]
pub enum Events {
    /// Read activity log entries
    Read {
        #[command(flatten)]
        page: SequencePage,
        /// Wait up to this many seconds for new entries
        #[arg(long, catalog_observation_wait())]
        wait: Option<u64>,
    },
    /// Add a note to the activity log
    Note {
        /// Note text
        #[arg(long)]
        note: String,
        #[command(flatten)]
        operation: Operation,
    },
}
#[derive(Debug, Subcommand)]
pub enum Claim {
    /// Acquire durable advisory custody
    Acquire {
        subject: Subject,
        #[command(flatten)]
        operation: Operation,
    },
    /// Record activity without expiring or locking custody
    Renew {
        subject: Subject,
        #[command(flatten)]
        operation: Operation,
    },
    /// Inspect custody
    Verify { subject: Subject },
    /// Release custody
    Release {
        subject: Subject,
        #[command(flatten)]
        operation: Operation,
    },
    /// List this project's custody ledger
    List {
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value = "100", catalog_u64_range = 1..=100)]
        limit: u64,
    },
}
#[derive(Debug, thiserror::Error)]
pub enum EdgeError {
    #[error(transparent)]
    Relay(#[from] provenant_fabric_runtime::relay::ServiceError),
    #[error(transparent)]
    Exec(#[from] provenant_os::exec::ExecError),
    #[error(transparent)]
    Coordination(#[from] CoordinationError),
    #[error(transparent)]
    Identity(#[from] provenant_fabric_runtime::identity::IdentityError),
    #[error(transparent)]
    Config(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Fs(#[from] provenant_os::fs::FsError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Root(#[from] provenant_core::RootError),
    #[error(transparent)]
    Utility(#[from] provenant_confinement::utility::UtilityError),
    #[error(transparent)]
    Time(#[from] provenant_core::TimeError),
    #[error(transparent)]
    Encode(#[from] serde_json::Error),
    #[error("Fabric state directory {} has mode {mode:04o}; other users can read or access it", path.display())]
    StateDirectoryPermissions { path: PathBuf, mode: u32 },
    #[error("invalid Fabric input")]
    Invalid,
    #[error("invalid Fabric input: {0}")]
    InvalidField(&'static str),
    #[error("Git executable is unavailable")]
    GitUnavailable,
}
pub fn edge_diagnostic(error: &EdgeError) -> Option<provenant_records::diagnostic::Diagnostic> {
    use provenant_fabric_runtime::coordination::facade::FacadeError;
    match error {
        EdgeError::StateDirectoryPermissions { path, mode } => {
            Some(FacadeError::state_directory_permissions(
                path,
                *mode,
                provenant_records::diagnostic::Destination::Local,
            ))
        }
        EdgeError::Coordination(error) => Some(FacadeError::from(error).diagnostic()),
        EdgeError::Relay(provenant_fabric_runtime::relay::ServiceError::Facade(error)) => {
            Some(error.diagnostic())
        }
        EdgeError::Relay(provenant_fabric_runtime::relay::ServiceError::Coordination(error)) => {
            Some(FacadeError::from(error).diagnostic())
        }
        _ => None,
    }
}

fn json(value: impl Serialize) -> Result<Value, EdgeError> {
    Ok(serde_json::to_value(value)?)
}
fn recipient(value: &str, host: provenant_core::HostId) -> Result<ActorId, EdgeError> {
    let (host, label) = match value.split_once('/') {
        Some((host, label)) => (
            host.parse().map_err(|_| EdgeError::InvalidField("--to"))?,
            label,
        ),
        None => (host, value),
    };
    Ok(provenant_fabric_runtime::identity::Address::new(host, label)?.actor)
}
impl Fabric {
    fn path(&self) -> String {
        let (domain, verb) = match self {
            Self::Identity => return "fabric identity".into(),
            Self::Peer { .. } => return "fabric peer".into(),
            Self::PeerAgent => return "fabric peer-agent".into(),
            Self::Mail(v) => (
                "mail",
                match v {
                    Mail::Send { .. } => "send",
                    Mail::Read { .. } => "read",
                    Mail::MarkRead { .. } => "mark-read",
                },
            ),
            Self::Events(v) => (
                "events",
                match v {
                    Events::Read { .. } => "read",
                    Events::Note { .. } => "note",
                },
            ),
            Self::Claim(v) => (
                "claim",
                match v {
                    Claim::Acquire { .. } => "acquire",
                    Claim::Renew { .. } => "renew",
                    Claim::Verify { .. } => "verify",
                    Claim::Release { .. } => "release",
                    Claim::List { .. } => "list",
                },
            ),
        };
        format!("fabric {domain} {verb}")
    }
    fn wait(&self) -> u64 {
        match self {
            Self::Events(Events::Read { wait: Some(s), .. }) => *s,
            _ => 55,
        }
    }
    fn read_only(&self) -> bool {
        matches!(
            self,
            Self::Identity
                | Self::Mail(Mail::Read { .. })
                | Self::Events(Events::Read { .. })
                | Self::Claim(Claim::Verify { .. } | Claim::List { .. })
        )
    }
}
#[allow(clippy::disallowed_methods)] // Native read-only store open deadline.
fn execute(
    cmd: Fabric,
    roots: provenant_core::Roots,
    home: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<(Value, bool), EdgeError> {
    let seconds = cmd.wait();
    let ctx = if in_lane() {
        Context::open_read_only(roots, home, cwd)?
    } else {
        Context::open(roots, home, cwd)?
    };
    let lane_mail_send = matches!(&cmd, Fabric::Mail(Mail::Send { .. }));
    if ctx.in_lane && !cmd.read_only() && !lane_mail_send {
        return Err(CoordinationError::LaneMutation.into());
    }
    if matches!(cmd, Fabric::Identity) {
        return Ok((json(ctx.identity)?, false));
    }
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), seconds)?;
    let backoff = ThreadPause;
    let service = if ctx.in_lane {
        let store = provenant_fabric_store::Store::open_read_only(
            &ctx.roots.state,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .map_err(CoordinationError::from)?
        .ok_or(CoordinationError::from(
            provenant_fabric_store::Error::NotFound,
        ))?
        .live_read_only()
        .map_err(CoordinationError::from)?;
        CoordinationService::new(
            store,
            ctx.identity.project,
            ctx.identity.inbox.actor,
            ctx.local_observation.clone(),
            &clock,
            &backoff,
        )
    } else {
        CoordinationService::for_identity(
            &ctx.roots.state,
            &ctx.identity,
            &ctx.config,
            ctx.local_observation.clone(),
            &ctx.home,
            &ctx.cwd,
            &ctx.executor,
            &clock,
            &backoff,
            &cancel,
        )?
    };
    let mutate = |input, operation: Operation| {
        if ctx.in_lane {
            let Mutation::Mail(input @ MailInput::Send { .. }) = input else {
                return Err(CoordinationError::LaneMutation.into());
            };
            let call = provenant_fabric_runtime::relay::ToolCall::Mail(
                input,
                Some(operation.operation_id),
            );
            return Ok(call.relay_within(
                &ctx.lane_relay_socket()?,
                &cancel,
                std::time::Duration::from_micros(budget.remaining(clock.mono_now()).as_micros()),
            )?);
        }
        json(service.mutate(input, &operation.operation_id, budget, &cancel)?)
    };
    let observe = |input: Mutation| -> Result<Value, EdgeError> {
        if ctx.in_lane && ctx.remote_home() {
            use provenant_fabric_runtime::relay::ToolCall;
            let waiting = matches!(
                &input,
                Mutation::Events(EventsInput::Read {
                    wait_seconds: Some(_),
                    ..
                })
            );
            loop {
                let mut observation = input.clone();
                let seconds = if matches!(
                    &input,
                    Mutation::Events(EventsInput::Read {
                        wait_seconds: Some(0),
                        ..
                    })
                ) {
                    0
                } else {
                    (budget.remaining(clock.mono_now()).as_micros() / 1_000_000).clamp(1, 55)
                };
                if let Mutation::Events(EventsInput::Read {
                    wait_seconds: Some(wait_seconds),
                    ..
                }) = &mut observation
                {
                    *wait_seconds = seconds;
                }
                let call = match observation {
                    Mutation::Mail(input) => ToolCall::Mail(input, None),
                    Mutation::Events(input) => ToolCall::Events(input, None),
                    Mutation::Claim(input) => ToolCall::Claim(input, None),
                };
                let value = call.relay_within(
                    &ctx.lane_relay_socket()?,
                    &cancel,
                    std::time::Duration::from_micros(
                        budget.remaining(clock.mono_now()).as_micros(),
                    ),
                )?;
                if !waiting
                    || value["timed_out"] != true
                    || budget.remaining(clock.mono_now()).as_micros() < 1_000_000
                {
                    return Ok(value);
                }
            }
        }
        match input {
            Mutation::Mail(input) => json(service.mail(input, budget, &cancel)?),
            Mutation::Events(input) => json(service.events(input, budget, &cancel)?),
            Mutation::Claim(input) => json(service.claim(input, budget, &cancel)?),
        }
    };
    let value = match cmd {
        Fabric::Identity | Fabric::Peer { .. } | Fabric::PeerAgent => {
            return Err(EdgeError::InvalidField("command"));
        }
        Fabric::Mail(Mail::Send {
            to,
            kind,
            body,
            reply_to,
            operation,
        }) => {
            ctx.text(&body)?;
            let recipients = to
                .iter()
                .map(|v| recipient(v, ctx.identity.host))
                .collect::<Result<_, _>>()?;
            let kind = match kind {
                Kind::Request => MessageKind::Request,
                Kind::Response => MessageKind::Response,
                Kind::Note => MessageKind::Note,
            };
            mutate(
                Mutation::Mail(MailInput::Send {
                    extra: Default::default(),
                    recipients,
                    kind,
                    body,
                    artifacts: vec![],
                    reply_to: reply_to.map(|id| MessageRef::Local {
                        extra: Default::default(),
                        id,
                    }),
                }),
                operation,
            )?
        }
        Fabric::Mail(Mail::Read { page, session }) => {
            ctx.page(page.limit)?;
            observe(Mutation::Mail(MailInput::Read {
                extra: Default::default(),
                after: page.after,
                limit: page.limit,
                session,
            }))?
        }
        Fabric::Mail(Mail::MarkRead {
            through,
            session,
            operation,
        }) => mutate(
            Mutation::Mail(MailInput::MarkRead {
                extra: Default::default(),
                through,
                session,
            }),
            operation,
        )?,
        Fabric::Events(Events::Read { page, wait }) => {
            ctx.page(page.limit)?;
            let result = observe(Mutation::Events(EventsInput::Read {
                extra: Default::default(),
                after: page.after,
                limit: page.limit,
                wait_seconds: wait,
            }))?;
            let timed_out = result["timed_out"].as_bool().unwrap_or(false);
            return Ok((result, timed_out));
        }
        Fabric::Events(Events::Note { note, operation }) => {
            ctx.text(&note)?;
            mutate(
                Mutation::Events(EventsInput::Note {
                    extra: Default::default(),
                    note,
                    dedup_key: None,
                }),
                operation,
            )?
        }
        Fabric::Claim(Claim::Acquire { subject, operation }) => mutate(
            Mutation::Claim(ClaimInput::Acquire {
                extra: Default::default(),
                subject,
                holder: Holder::Chair {
                    chair: current_chair(&ctx),
                },
            }),
            operation,
        )?,
        Fabric::Claim(Claim::Renew { subject, operation }) => mutate(
            Mutation::Claim(ClaimInput::Renew {
                extra: Default::default(),
                subject,
            }),
            operation,
        )?,
        Fabric::Claim(Claim::Verify { subject }) => observe(Mutation::Claim(ClaimInput::Verify {
            extra: Default::default(),
            subject,
        }))?,
        Fabric::Claim(Claim::Release { subject, operation }) => mutate(
            Mutation::Claim(ClaimInput::Release {
                extra: Default::default(),
                subject,
            }),
            operation,
        )?,
        Fabric::Claim(Claim::List { after, limit }) => {
            ctx.page(limit)?;
            observe(Mutation::Claim(ClaimInput::List {
                extra: Default::default(),
                after,
                limit,
            }))?
        }
    };
    Ok((value, false))
}

pub fn run(
    cmd: Fabric,
    globals: &Globals,
    roots: &provenant_core::Roots,
    home: &std::path::Path,
    cwd: &std::path::Path,
) -> u8 {
    let command = cmd.path();
    let read_only = cmd.read_only();
    let mut result = match execute(cmd, roots.clone(), home, cwd) {
        Ok((data, timeout)) => CommandResult::new(
            &command,
            if timeout {
                Outcome::Timeout
            } else {
                Outcome::Ok
            },
            data,
        ),
        Err(error) => {
            let (status, code) = match &error {
                EdgeError::Coordination(error) => (
                    coordination_outcome(
                        &provenant_fabric_runtime::coordination::facade::FacadeError::from(error),
                    ),
                    error.code(),
                ),
                EdgeError::Identity(_)
                | EdgeError::Invalid
                | EdgeError::InvalidField(_)
                | EdgeError::StateDirectoryPermissions { .. }
                | EdgeError::Root(_) => (Outcome::Invalid, "invalid_input"),
                EdgeError::Relay(error) => (Outcome::Error, error.code()),
                EdgeError::Config(_) => (Outcome::Invalid, "config"),
                _ => (Outcome::Error, "io"),
            };
            let projection = edge_diagnostic(&error);
            let mut result = CommandResult::new(&command, status, Value::Null);
            result.error = Some(CommandError {
                diagnostic: projection.as_ref().map(Into::into),
                code: code.into(),
                message: projection.as_ref().map_or_else(
                    || error.to_string(),
                    |projection| {
                        projection
                            .detail
                            .clone()
                            .unwrap_or_else(|| projection.code.clone())
                    },
                ),
                cause: None,
                repair: projection.and_then(|projection| projection.next),
            });
            result
        }
    };
    let human = if result.error.is_none() {
        crate::terse(&result.data)
    } else {
        String::new()
    };
    let mode = globals.render_mode(&human, read_only, None);
    // Runtime pages/mutation receipts are bounded well below the ordinary cap.
    match crate::render_or_publish(
        &result,
        mode,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        |value| context::publish(value, roots),
    ) {
        Ok(status) => crate::exit_status(&status),
        Err(_) => {
            result.status = Outcome::Error;
            3
        }
    }
}

/// The command outcome for a typed coordination error. Store upgrades and peer
/// versions are refusals here, as on the facade edge (`context::facade_outcome`).
pub fn coordination_outcome(
    error: &provenant_fabric_runtime::coordination::facade::FacadeError,
) -> Outcome {
    use provenant_fabric_runtime::coordination::facade::FacadeError;
    match error {
        FacadeError::InvalidInput { .. } | FacadeError::NotFound => Outcome::Invalid,
        FacadeError::GraphRelayRefused { .. }
        | FacadeError::StaleFence
        | FacadeError::GenerationConflict { .. }
        | FacadeError::Conflict
        | FacadeError::LaneCoordinationRefused
        | FacadeError::UpgradeRefused
        | FacadeError::UpgradeRequired { .. }
        | FacadeError::PeerVersion
        | FacadeError::PeerProtocolMismatch { .. } => Outcome::Refused,
        FacadeError::Timeout => Outcome::Timeout,
        FacadeError::Cancelled => Outcome::Cancelled,
        _ => Outcome::Error,
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;

    #[test]
    fn coordination_edge_uses_safe_owner_projection() {
        let error = EdgeError::Coordination(CoordinationError::Peer(
            provenant_fabric_runtime::hosts::PeerError::ProtocolMismatch {
                peer: "/private/TOKEN_SENTINEL".into(),
                remote: 1,
                expected: 2,
            },
        ));
        let projection = edge_diagnostic(&error).unwrap();
        let encoded = serde_json::to_string(&projection).unwrap();
        assert!(!encoded.contains("TOKEN_SENTINEL"));
        assert!(!encoded.contains("/private"));
        assert_eq!(projection.code, "peer_version");
        assert!(projection.next.is_some());
    }

    #[test]
    fn upgrade_and_peer_version_refusals_match_on_both_edges() {
        use provenant_fabric_runtime::coordination::facade::FacadeError;
        for error in [FacadeError::UpgradeRefused, FacadeError::PeerVersion] {
            assert_eq!(coordination_outcome(&error), Outcome::Refused);
            assert_eq!(facade_outcome(&error).0, Outcome::Refused);
        }
    }
}

#[cfg(test)]
mod input_error_tests {
    use super::*;

    #[test]
    fn unparsable_recipient_host_names_the_to_flag() {
        let host = provenant_core::HostId::from_bytes([1; 16]);
        assert!(matches!(
            recipient("not-a-host/label", host),
            Err(EdgeError::InvalidField("--to"))
        ));
        assert_eq!(
            EdgeError::InvalidField("--to").to_string(),
            "invalid Fabric input: --to"
        );
    }
}
