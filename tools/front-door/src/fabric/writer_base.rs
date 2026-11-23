//! A writer launch's one base: the commit its identity and its read-only
//! members start from, and the launch expression that supplied it.
//!
//! Writer-ness is the resolved graph's answer, so the base is resolved once by
//! role before the launch identity is sealed. Read-only members of a writer
//! graph (the plan node of `implement --plan`, a composite's review agents) read
//! that base rather than the caller checkout's stale upstream; gates, decisions
//! and landings take no execution binding. Only a writer that names no base at
//! all falls back to the caller checkout.

use provenant_cli::run::{ReadSourceChoice, read_only_source_ref};
use provenant_fabric_runtime::graph::{AccessMode, GraphNode, NodeKind, SourceBinding};
use provenant_records::Layer;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The base a `--branch` writer starts from when it names no `--base`.
pub(super) const DEFAULT_WRITER_BASE: &str = "refs/remotes/origin/HEAD";

/// The launch expression that supplied a writer's base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WriterBaseSource {
    ExplicitBase,
    BranchDefault,
    WorktreeHead,
    CallerCheckout,
}

impl WriterBaseSource {
    fn label(self) -> &'static str {
        match self {
            Self::ExplicitBase => "explicit --base",
            Self::BranchDefault => "the --branch default",
            Self::WorktreeHead => "the named worktree HEAD",
            Self::CallerCheckout => "no base expression",
        }
    }
}

/// The one base a writer launch starts from, resolved before its identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WriterBase {
    pub(super) commit: Option<String>,
    pub(super) source: WriterBaseSource,
}

impl WriterBase {
    /// Name the expression and the fallback when no base could be resolved.
    /// Missing Git evidence is advisory, never a refusal.
    pub(super) fn fallback_warning(&self, cwd: &Path) -> Option<String> {
        self.commit.is_none().then(|| {
            format!(
                "writer launch has no resolved base (source: {}); its identity falls back to the caller checkout at {}",
                self.source.label(),
                cwd.display()
            )
        })
    }
}

/// Resolve one writer base by role: explicit `--base`, else the `--branch`
/// default, else the named worktree's HEAD, else none.
pub(super) fn resolve_writer_base(
    input: &Value,
    project_root: &Path,
    cwd: &Path,
    state: &Path,
    read_source: &ReadSourceChoice,
    git: Option<&Path>,
) -> WriterBase {
    if let Some(base) = input
        .get("base")
        .and_then(Value::as_str)
        .filter(|base| !base.is_empty())
    {
        return WriterBase {
            commit: resolve_ref(state, cwd, base, read_source, "--base"),
            source: WriterBaseSource::ExplicitBase,
        };
    }
    if input.get("branch").is_some_and(|branch| !branch.is_null()) {
        return WriterBase {
            commit: resolve_ref(state, cwd, DEFAULT_WRITER_BASE, read_source, "--branch"),
            source: WriterBaseSource::BranchDefault,
        };
    }
    if let Some(worktree) = named_worktree(input) {
        let path = absolute(project_root, worktree);
        return WriterBase {
            commit: git.and_then(|git| provenant_run::resolve_git_base(git, &path, "HEAD").ok()),
            source: WriterBaseSource::WorktreeHead,
        };
    }
    WriterBase {
        commit: None,
        source: WriterBaseSource::CallerCheckout,
    }
}

/// A `--worktree` control, or a composite's `--set worktree=PATH` binding.
fn named_worktree(input: &Value) -> Option<&str> {
    input
        .get("worktree")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .or_else(|| {
            input
                .pointer("/set/worktree")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
        })
}

fn absolute(project_root: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_owned()
    } else {
        project_root.join(path)
    }
}

/// Resolve a named base through the same read-source resolver as any explicit
/// ref, keeping the launch layer and the expression that chose it.
fn resolve_ref(
    state: &Path,
    cwd: &Path,
    base: &str,
    read_source: &ReadSourceChoice,
    origin: &str,
) -> Option<String> {
    read_only_source_ref(
        state,
        cwd,
        &ReadSourceChoice {
            value: base.to_owned(),
            fetch_first: read_source.fetch_first,
            layer: Layer::Launch,
            origin: Some(origin.to_owned()),
        },
    )
    .map(|selected| selected.commit.to_string())
}

/// Pin a read-only node and its read-only members to the commit their launch
/// reads. Writer nodes, continuations and explicit source bindings keep their
/// own subject; nested templates are visited so every reader in a composite
/// sees the same tree.
pub(super) fn bind_default_read_only_source(node: &mut GraphNode, commit: &str) {
    if node.access == AccessMode::ReadOnly && node.source.is_none() && node.continues.is_none() {
        node.source = Some(SourceBinding::Commit {
            commit: commit.into(),
        });
    }
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            bind_default_read_only_source(child, commit);
        }
    }
    if let Some(loop_spec) = &mut node.loop_body {
        for child in &mut loop_spec.body {
            bind_default_read_only_source(child, commit);
        }
    }
}

/// Pin a writer graph's read-only leaves to the writer base. A composite that
/// names its own source keeps it: its members inherit that subject when the
/// graph expands, and a fixed base must not override (say) the writer head a
/// review panel reads. Container nodes stay unbindable so a writer member never
/// inherits a read source from its read-only boundary.
pub(super) fn bind_writer_base_source(node: &mut GraphNode, commit: &str) {
    if node.source.is_some() {
        return;
    }
    // Only an agent node executes, so only it takes an execution binding;
    // graph validation refuses a source on a gate, decision or landing.
    let agent = matches!(
        node.kind,
        None | Some(NodeKind::Task | NodeKind::Synthesise)
    );
    if agent
        && node.access == AccessMode::ReadOnly
        && node.continues.is_none()
        && node.composite.is_none()
        && node.loop_body.is_none()
    {
        node.source = Some(SourceBinding::Commit {
            commit: commit.into(),
        });
    }
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            bind_writer_base_source(child, commit);
        }
    }
    if let Some(loop_spec) = &mut node.loop_body {
        for child in &mut loop_spec.body {
            bind_writer_base_source(child, commit);
        }
    }
}
