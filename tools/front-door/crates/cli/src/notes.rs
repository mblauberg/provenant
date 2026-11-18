//! Notes grammar and host composition. Lane requests cross the bound relay.
use crate::metadata::CatalogueArg;
use clap::{Args, Subcommand};
use provenant_core::{CancelToken, Clock};
use provenant_fabric_runtime::notes::{Board, Host, Input, Scope};
use provenant_fabric_runtime::relay::{ServiceError, ToolCall};
use serde_json::Value;

#[derive(Debug, Subcommand)]
pub enum Notes {
    /// Append an observation; corrections reference an earlier source sequence
    Add {
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        resolves: Option<i64>,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Read a delta; successful output advances this reader's cursor
    Read {
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long, default_value = "last")]
        since: String,
        /// Replay from the beginning without changing the saved delta cursor
        #[arg(long, conflicts_with = "since")]
        replay: bool,
        /// Consumer alias within this host-bound lane (or chair)
        #[arg(long)]
        reader: Option<String>,
        /// Entries per page; reported with source=launch
        #[arg(long, catalog_u64_range = 1..)]
        limit: Option<u64>,
    },
}
#[derive(Debug, Args)]
pub struct ScopeArgs {
    /// Logical scope: project, GRAPH, or GRAPH/NODE (including descendants)
    #[arg(long, conflicts_with = "graph")]
    pub scope: Option<String>,
    #[arg(long)]
    pub graph: Option<String>,
    #[arg(long, requires = "graph", conflicts_with = "scope")]
    pub subtree: Option<String>,
}
impl ScopeArgs {
    pub fn resolve(&self) -> Scope {
        match (&self.graph, &self.subtree, self.scope.as_deref()) {
            (Some(graph), Some(node), _) => Scope::Subtree {
                graph: graph.clone(),
                node: node.clone(),
            },
            (Some(graph), None, _) => Scope::Graph {
                graph: graph.clone(),
            },
            (_, _, None | Some("project")) => Scope::Project,
            (_, _, Some(s)) => match s.split_once('/') {
                Some((graph, node)) => Scope::Subtree {
                    graph: graph.into(),
                    node: node.into(),
                },
                None => Scope::Graph { graph: s.into() },
            },
        }
    }
}
fn error(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::Cli {
        code: "notes_error".into(),
        message: e.to_string(),
        data: Value::Null,
    }
}
pub fn execute_host(
    ctx: &crate::fabric::Context,
    host: &Host,
    input: Input,
    clock: &dyn Clock,
    cancel: &CancelToken,
) -> Result<Value, ServiceError> {
    let board = Board::open(
        ctx.roots.state.as_path(),
        ctx.identity.project,
        provenant_fabric_runtime::notes::default_read_cap(),
        provenant_fabric_runtime::notes::default_byte_cap(),
    )
    .map_err(error)?;
    board.execute(host, input, clock, cancel).map_err(error)
}

/// Called on the guardian, never with a lane-selected attempt or provenance.
pub fn serve_lane(
    ctx: &crate::fabric::Context,
    holder: &provenant_run::HolderContext,
    input: Input,
    clock: &dyn Clock,
    cancel: &CancelToken,
) -> Result<Value, ServiceError> {
    if !matches!(input, Input::Add { .. }) {
        let mut host = Host::chair(holder.project_id, None);
        host.lineage.attempt = Some(holder.attempt_id);
        return execute_host(ctx, &host, input, clock, cancel);
    }
    let roots = ctx
        .roots
        .with_state_dir(
            holder.roots.state_root.clone(),
            &provenant_os::fs::SystemPathResolver,
        )
        .map_err(error)?;
    let context = provenant_run::ExecutionContext {
        state_root: roots.state,
        project_id: holder.project_id,
        actor_id: holder.actor_id,
        read_prompts: false,
    };
    let call = provenant_run::Request { clock, cancel };
    let store = provenant_run::RunStore::open_read_only(&context, &call)
        .map_err(error)?
        .ok_or_else(|| error("bound attempt store is missing"))?;
    let record = store.record(holder.attempt_id, &call).map_err(error)?;
    let operation = store
        .attempt_operation(holder.attempt_id, &call)
        .map_err(error)?;
    let mut host = Host::from_attempt(&record, operation.parent_attempt);
    if let Some(launch) = &holder.launch {
        host.commit = current_commit(ctx.roots.state.as_path(), &launch.cwd, cancel);
        host.commit_source = "host_checkout_head";
    }
    execute_host(ctx, &host, input, clock, cancel)
}

pub fn from_lane(
    attempt: provenant_core::AttemptId,
    input: Input,
    cancel: &CancelToken,
) -> Result<Value, ServiceError> {
    ToolCall::Notes(input).relay_within(
        &provenant_providers::LaneRuntime::fabric_socket(attempt),
        cancel,
        std::time::Duration::from_secs(10),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // Spec: docs/specs/lane-contracts.md#shared-notes
    #[test]
    fn notes_cli_accepts_free_tags_scopes_and_explicit_replay() {
        let parse = |args: &[&str]| crate::parse(args.iter().map(std::ffi::OsString::from));
        let crate::ParseOutcome::Invocation(inv) = parse(&[
            "notes",
            "add",
            "--tag",
            "interface/custom",
            "--scope",
            "g/root--child",
            "--resolves",
            "1",
            "--",
            "--verified",
            "is text",
        ]) else {
            panic!("add did not parse")
        };
        let crate::Command::Notes(Notes::Add { text, scope, .. }) = inv.command else {
            panic!("wrong command")
        };
        assert_eq!(text, ["--verified", "is text"]);
        assert_eq!(
            scope.resolve(),
            Scope::Subtree {
                graph: "g".into(),
                node: "root--child".into()
            }
        );
        assert!(matches!(
            parse(&[
                "notes",
                "read",
                "--replay",
                "--graph",
                "g",
                "--subtree",
                "root"
            ]),
            crate::ParseOutcome::Invocation(_)
        ));
        for field in ["--verified", "--model", "--lane", "--commit"] {
            assert!(matches!(
                parse(&["notes", "add", field, "fake", "--", "claim"]),
                crate::ParseOutcome::Invalid { .. }
            ));
        }
    }
}

/// Read HEAD through the host's bounded, read-only Git utility.
pub fn current_commit(
    state: &std::path::Path,
    cwd: &std::path::Path,
    cancel: &CancelToken,
) -> Option<String> {
    use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime, run_read_utility};
    let git = provenant_os::git::git_executable()?;
    let runtime = UtilityRuntime::read_only(git, state).ok()?;
    let executor = provenant_os::exec::SystemExecutor::new(1).ok()?;
    let budget = UtilityBudget::bounded(std::time::Duration::from_secs(5)).ok()?;
    let repo = runtime
        .open_read_repo(&executor, cwd, budget, cancel)
        .ok()?;
    let outcome = run_read_utility(
        &executor,
        GitRead::RevParseHead,
        &repo,
        &runtime,
        budget,
        cancel,
    )
    .ok()?;
    if outcome.termination != provenant_os::exec::Termination::Exit(0)
        || outcome.drain_truncated
        || outcome.stdout.truncated
        || outcome.timed_out
        || outcome.cancelled
    {
        return None;
    }
    let text = std::str::from_utf8(&outcome.stdout.bytes).ok()?.trim();
    provenant_core::GitOid::parse(text)
        .ok()
        .map(|oid| oid.to_string())
}
