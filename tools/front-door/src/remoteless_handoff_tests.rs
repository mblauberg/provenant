//! Regression tests for #3869 outcome 3: a hand-off checkout with no Git
//! remote resolves the absence as `None` and both the `--wip` source path and
//! the recreate target path refuse explicitly instead of building a remote
//! name from an empty string.
//! Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
#![allow(
    dead_code,
    unused_imports,
    reason = "the custom worktree driver omits libtest functions"
)]

use std::{collections::BTreeMap, error::Error, path::PathBuf};

use provenant_cli::fabric::Context;
use provenant_confinement::worktrees::{Occupancy, WorkerObserver};
use provenant_core::{Clock, HostId, RootInputs, WaitBudget};
use provenant_fabric_runtime::handoff::session::Tree;
use provenant_os::clock::SystemClock;

use crate::session_handoff::{Source, Target, recreate, resolve_remote, wip};

type FixtureResult<T> = std::result::Result<T, Box<dyn Error>>;

struct Free;
impl WorkerObserver for Free {
    fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
        roots
            .iter()
            .cloned()
            .map(|path| (path, Occupancy::Available))
            .collect()
    }
}

/// A checkout opened into the real hand-off `Source`. `new` has no remotes at
/// all; the absence (or presence, in the control) is resolved by production
/// code via `resolve_remote` and passed to the consumers as a typed
/// `Option<RemoteName>` — never injected by the test as a literal `None`. With
/// that type, `Some("")` is unrepresentable.
struct HandoffSource {
    _temp: tempfile::TempDir,
    source: Source,
    state: PathBuf,
    project: PathBuf,
    home: PathBuf,
}

impl HandoffSource {
    fn new() -> FixtureResult<Self> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        let state = temp.path().join("state");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&state)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
        std::fs::create_dir_all(&project)?;
        provenant_os::fixture::run_git(&project, &["init", "-q", "-b", "trunk"])?;
        std::fs::write(project.join("tracked.txt"), "base\n")?;
        provenant_os::fixture::run_git(&project, &["add", "tracked.txt"])?;
        provenant_os::fixture::run_git(&project, &["commit", "-qm", "base"])?;
        let remotes = provenant_os::fixture::run_git(&project, &["remote"])?;
        assert!(
            String::from_utf8_lossy(&remotes).trim().is_empty(),
            "fixture must have no remotes, saw {remotes:?}"
        );

        let mut inputs = RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(state.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = Context::open(roots.clone(), &home, &project)?;
        let source = Source {
            roots,
            home: home.clone(),
            assets: temp.path().join("assets"),
            cwd: project.clone(),
            context,
        };
        Ok(Self {
            _temp: temp,
            source,
            state,
            project,
            home,
        })
    }

    /// Positive control: the same checkout with a single configured remote.
    /// The URL is deliberately unreachable so a consumer that receives the
    /// resolved `Some("origin")` fails in Git rather than silently succeeding.
    fn with_origin() -> FixtureResult<Self> {
        let fixture = Self::new()?;
        provenant_os::fixture::run_git(
            &fixture.project,
            &[
                "remote",
                "add",
                "origin",
                "/nonexistent-remoteless-handoff.git",
            ],
        )?;
        Ok(fixture)
    }

    fn tree(&self) -> Tree {
        Tree {
            path: self.project.clone(),
            branch: Some("lane".into()),
            head: String::new(),
            primary: false,
            dirty: true,
            unknown: false,
            live_writer: false,
            writer_unknown: false,
            writer_evidence: None,
            writer_advisory: None,
            upstream: None,
            pushed: false,
        }
    }
}

/// The one resolver hands back `None`, never an empty name: `RemoteName` makes
/// `Some("")` unrepresentable. If this re-flattens, both consumers below would
/// receive a bogus name instead of an absence.
#[test]
fn resolve_remote_reports_an_absent_remote_as_none() -> FixtureResult<()> {
    let fixture = HandoffSource::new()?;
    let ports = provenant_config::PortTable::builtin();
    let resolved = resolve_remote(&fixture.state, &fixture.project, &ports)
        .map_err(|refusal| refusal.message)?;
    assert_eq!(resolved, None);
    Ok(())
}

/// `--wip` publishes a commit to the checkout's remote. With no remote it must
/// refuse with `WIP_REFUSED` before any `git push` could see an empty name.
#[test]
fn wip_refuses_without_a_remote() -> FixtureResult<()> {
    let fixture = HandoffSource::new()?;
    let ports = provenant_config::PortTable::builtin();
    let remote = resolve_remote(&fixture.state, &fixture.project, &ports)
        .map_err(|refusal| refusal.message)?;
    assert_eq!(remote, None, "the fixture must resolve to a real absence");
    let refusal = wip(
        &fixture.source,
        &Free,
        &provenant_confinement::worktrees::RepoFacts::default(),
        &fixture.tree(),
        remote,
    )
    .expect_err("a WIP hand-off without a remote must be refused");
    assert_eq!(refusal.code(), "WIP_REFUSED");
    assert!(
        refusal.message.contains("needs a Git remote to push to"),
        "{}",
        refusal.message
    );
    Ok(())
}

/// Recreating a handed-off branch starts from `<remote>/<branch>`. With no
/// remote every branch is refused explicitly with the typed per-branch error,
/// never with a `"/<branch>"` name built from an empty string.
#[test]
fn recreate_refuses_each_branch_without_a_remote() -> FixtureResult<()> {
    let fixture = HandoffSource::new()?;
    let ports = provenant_config::PortTable::builtin();
    let remote = resolve_remote(&fixture.state, &fixture.project, &ports)
        .map_err(|refusal| refusal.message)?;
    assert_eq!(remote, None, "the fixture must resolve to a real absence");
    let clock = SystemClock::new();
    let target = Target {
        context: &fixture.source.context,
        home: &fixture.home,
        cwd: &fixture.project,
        workers: &Free,
        source: HostId::from_bytes([1; 16]),
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
    };
    let branches = vec!["lane".to_owned(), "other".to_owned()];
    let made = recreate(&target, &branches, remote);
    assert_eq!(made.len(), branches.len());
    for worktree in &made {
        assert!(!worktree.created);
        assert!(worktree.path.is_none());
        assert!(worktree.head.is_none());
        assert_eq!(
            worktree.error.as_deref(),
            Some("a hand-off needs a Git remote to recreate branches")
        );
    }
    Ok(())
}

/// Positive control for both refusal tests: a configured remote resolves to a
/// typed `Some(RemoteName("origin"))` and is forwarded into the Git path. A
/// rewrite that injects a literal `None` fails here; an empty name cannot even
/// be constructed.
#[test]
fn a_configured_remote_reaches_git_through_recreate() -> FixtureResult<()> {
    let fixture = HandoffSource::with_origin()?;
    let ports = provenant_config::PortTable::builtin();
    let remote = resolve_remote(&fixture.state, &fixture.project, &ports)
        .map_err(|refusal| refusal.message)?;
    assert_eq!(
        remote,
        Some(provenant_config::RemoteName::new("origin").expect("fixture remote"))
    );

    let clock = SystemClock::new();
    let target = Target {
        context: &fixture.source.context,
        home: &fixture.home,
        cwd: &fixture.project,
        workers: &Free,
        source: HostId::from_bytes([1; 16]),
        clock: &clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
    };
    let made = recreate(&target, &["lane".to_owned()], remote);
    assert_eq!(made.len(), 1);
    let error = made[0]
        .error
        .as_deref()
        .expect("recreating from a configured remote must attempt the Git fetch");
    assert!(
        error.contains("origin/lane"),
        "the resolved remote must reach the Git fetch, got {error}"
    );
    assert!(
        !error.contains("needs a Git remote"),
        "the Some path must not take the absent-remote refusal: {error}"
    );
    Ok(())
}
