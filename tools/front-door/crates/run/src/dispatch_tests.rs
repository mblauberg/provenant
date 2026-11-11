//! Dispatch, admission, holder and control behaviour at Run's owner boundary.
//! Fake providers and resources stand in for adapters and the host.
use super::tests;
use super::tests::{HostFacts, call, clock, context, exit, input, route};
use super::*;
use provenant_confinement::permissions::{PathGrant, PathScope};
use provenant_core::{AttemptId, CancelToken, Clock, MonoTime, RunId, TaskId, TestClock, WallTime};
use provenant_os::{exec::Termination, fs::Directory, sys::resources::MemorySample};
use provenant_records::CanonicalV1;
use provenant_supervisor::custody::ExitObservation;
use std::{
    cell::{Cell, RefCell},
    os::unix::fs::DirBuilderExt,
};
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

fn rewrite_child_request(
    store: &RunStore,
    run: RunId,
    request: &CanonicalRequest,
    call: &Request<'_>,
) -> TestResult {
    store.run_dir(run, false)?.atomic_write(
        std::ffi::OsStr::new("request.json"),
        ".swap-",
        &crate::db::bytes(request)?,
        provenant_os::fs::PublishMode::Replace,
    )?;
    let mut record = store.run(run, call)?;
    record.request_digest = request.digest()?;
    store.db.execute(
        "UPDATE runs SET record=?1 WHERE id=?2",
        rusqlite::params![crate::db::bytes(&record)?, run.to_string()],
    )?;
    assert_eq!(&store.request(run, call)?, request);
    Ok(())
}

/// Prepares a plan; no guardian ever runs it.
pub(super) struct Ready;

impl Providers for Ready {
    fn prepare(
        &self,
        _: &TaskRequest,
        _: &str,
        _: &LaunchPaths,
        _: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
        Ok(PreparedLaunch {
            limits: Default::default(),
            argv: vec!["fixture".into()],
            environment: Vec::new(),
            stdin: Vec::new(),
            secret_input: None,
            cwd: "/".into(),
            route: route(),
            skipped_routes: Vec::new(),
            observed_head: None,
            repository: None,
            confinement: None,
        })
    }
    fn decode(
        &self,
        _: &RouteRecord,
        _: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        unreachable!("no guardian runs a provider in these tests")
    }
}

struct FirstPriorityFailure(TaskId);
impl Providers for FirstPriorityFailure {
    fn priority(&self, task: &TaskRequest) -> std::result::Result<u8, String> {
        if task.task_id == self.0 {
            Err("priority configuration unavailable".into())
        } else {
            Ok(5)
        }
    }
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
        Ready.prepare(task, prompt, paths, resume)
    }
    fn decode(
        &self,
        route: &RouteRecord,
        output: &[u8],
        exit: &ExitObservation,
        path: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        Ready.decode(route, output, exit, path)
    }
}

/// Carries a real permission plan through launch, stopping before compilation.
struct Confined {
    writer: bool,
    tool_writes: Vec<PathGrant>,
}
impl Providers for Confined {
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
        use provenant_confinement::permissions::{
            AccessMode, NetworkAccess, PlanPermissions, ProviderProfile, ProviderProfileSpec,
            WorktreeWriteAuthority,
        };
        use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
        let grant = |path: &str, scope| PathGrant {
            path: path.into(),
            scope,
        };
        let mode = if self.writer {
            let authority: WorktreeWriteAuthority = serde_json::from_value(serde_json::json!({
                "git_grants_digest": vec![7; 32],
                "publishing": true,
                "cargo_cache": {"path": "/fixture/cache", "scope": "subtree"}
            }))
            .map_err(|e| crate::fixture_refusal(e.to_string()))?;
            AccessMode::WorktreeWrite(Box::new(authority))
        } else {
            AccessMode::ReadOnly
        };
        let plan = ConfinedPlan::new(PlanRequest {
            scope: provenant_core::WorkScope::Project(provenant_core::ProjectId::from_bytes(
                [1; 16],
            )),
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
            permissions: PlanPermissions {
                policy: None,
                read_only: None,
                mode,
                scratch: grant("/fixture/scratch", PathScope::Subtree),
                private_state: Some(grant("/fixture/private", PathScope::Subtree)),
                read_roots: vec![grant("/fixture/repo", PathScope::Subtree)],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: None,
                include_continuation: false,
                write_roots: vec![grant("/fixture/shared", PathScope::Subtree)],
                network: NetworkAccess::Provider,
                nested_confinement: false,
            },
        })
        .map_err(|e| crate::fixture_refusal(e.to_string()))?;
        let mut launch = Ready.prepare(task, prompt, paths, resume)?;
        launch.confinement = Some(LaunchConfinement {
            plan,
            profile: ProviderProfile {
                spec: ProviderProfileSpec {
                    state_writes: vec![grant("/fixture/state/session", PathScope::Exact)],
                    tool_writes: self.tool_writes.clone(),
                    ..ProviderProfileSpec::default()
                },
                observations: Default::default(),
            },
            writer: None,
            home: "/fixture/home".into(),
            cargo_registry_seed: false,
            state_root: std::path::PathBuf::from("/fixture/state"),
        });
        Ok(launch)
    }
    fn decode(
        &self,
        _: &RouteRecord,
        _: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        unreachable!("no guardian runs a provider in these tests")
    }
}

/// Gives the first task oversized tool grants and its sibling none.
struct FirstOversized {
    first: TaskId,
    oversized: Confined,
    ordinary: Confined,
}
impl Providers for FirstOversized {
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
        let provider = if task.task_id == self.first {
            &self.oversized
        } else {
            &self.ordinary
        };
        provider.prepare(task, prompt, paths, resume)
    }
    fn decode(
        &self,
        route: &RouteRecord,
        output: &[u8],
        exit: &ExitObservation,
        path: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        self.ordinary.decode(route, output, exit, path)
    }
}

#[derive(Default)]
pub(super) struct Fake {
    network: Cell<Option<NetworkAccess>>,
    refuse: bool,
    policy_task: Option<TaskId>,
    policy_resource: Option<provenant_records::diagnostic::ResourceFact>,
    pub(super) observed: Option<RouteRecord>,
    pub(super) codex_usage: Option<UsageRecord>,
    pub(super) decoded: Cell<usize>,
    prepared: Cell<usize>,
    resumed: Cell<Option<AttemptId>>,
    prompt: RefCell<String>,
    /// The outcome a decode reports; success when unset.
    pub(super) outcome: Option<Outcome>,
    /// The terminal cause a decode reports.
    pub(super) terminal_cause: Option<TerminalCause>,
    /// The answer a decode reports, instead of the captured stdout.
    pub(super) answer: Option<String>,
    /// Partial-finish signals a decode reports.
    partial: Vec<PartialSignal>,
    /// Report `QuotaLimited` only when this exact sealed stderr arrives.
    quota_stderr: Option<&'static [u8]>,
    failure_note: Option<String>,
    /// Turn boundaries a decode reports in its answer.
    turns: Vec<provenant_records::handoff::TurnEnd>,
}

impl Fake {
    pub(super) fn with_observed(observed: Option<RouteRecord>, outcome: Option<Outcome>) -> Self {
        Self {
            observed,
            outcome,
            ..Default::default()
        }
    }
}

impl Providers for Fake {
    fn failure_diagnostic(&self, _: &RouteRecord, _: &[u8]) -> Option<String> {
        self.failure_note.clone()
    }

    fn check_policy(&self, task: &TaskRequest) -> std::result::Result<(), PrepareRefusal> {
        if self.policy_task == Some(task.task_id) {
            let mut refusal = crate::PrepareRefusal::new(
                TerminalCause::DiskFloor,
                "insufficient disk space; run `provenant gc`",
            );
            refusal.resource = self.policy_resource;
            return Err(refusal);
        }
        Ok(())
    }

    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
        self.network
            .set(Some(task.access.network(task.writer.is_some())));
        self.prepared.set(self.prepared.get() + 1);
        assert!(paths.temporary.ends_with("tmp"));
        assert!(paths.temporary.is_dir());
        self.prompt.replace(prompt.into());
        self.resumed.set(resume.map(|r| r.attempt_id));
        if self.refuse {
            return Err(crate::PrepareRefusal::new(
                TerminalCause::RouteResolutionRefused,
                "fixture adapter is disabled",
            ));
        }
        unreachable!("these tests never hand off to a guardian")
    }
    fn decode(
        &self,
        applied: &RouteRecord,
        stdout: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        assert_eq!(
            applied.provider.as_deref(),
            Some(if self.codex_usage.is_some() {
                "codex"
            } else {
                "fixture"
            })
        );
        assert_eq!(applied.model, route().model);
        assert_eq!(applied.effort, route().effort);
        self.decoded.set(self.decoded.get() + 1);
        Ok(Decoded {
            terminal_cause: self.terminal_cause,
            outcome: self.outcome.unwrap_or(Outcome::Succeeded),
            result: self
                .answer
                .clone()
                .unwrap_or_else(|| String::from_utf8_lossy(stdout).into_owned()),
            complete: self.outcome.is_none_or(|outcome| {
                matches!(outcome, Outcome::Succeeded | Outcome::ReviewRequired)
            }),
            observed_route: self.observed.clone(),
            usage: self.codex_usage.clone().unwrap_or_default(),
            session: None,
            reason: None,
            partial_signals: self.partial.clone(),
            turns: self.turns.clone(),
        })
    }
    fn decode_captures(
        &self,
        route: &RouteRecord,
        stdout: &[u8],
        stderr: &[u8],
        exit: &ExitObservation,
        provider_state: &std::path::Path,
        _: Option<&crate::HolderContext>,
    ) -> std::result::Result<Decoded, String> {
        let mut decoded = self.decode(route, stdout, exit, provider_state)?;
        if self.quota_stderr == Some(stderr) {
            decoded.outcome = Outcome::QuotaLimited;
            decoded.complete = false;
        }
        Ok(decoded)
    }
}

/// Settle one attempt whose provider reported `outcome` and `signals`, with
/// `answer` as its final answer, and return the committed record.
fn settle_with(
    answer: &str,
    outcome: Option<Outcome>,
    signals: Vec<PartialSignal>,
) -> std::result::Result<AttemptRecord, Box<dyn std::error::Error>> {
    settle_with_role(answer, outcome, signals, None)
}

fn settle_with_role(
    answer: &str,
    outcome: Option<Outcome>,
    signals: Vec<PartialSignal>,
    role: Option<&str>,
) -> std::result::Result<AttemptRecord, Box<dyn std::error::Error>> {
    settle_with_turns(answer, outcome, signals, role, Vec::new())
}

/// As `settle_with_role`, with the provider's turn boundaries in `answer`.
fn settle_with_turns(
    answer: &str,
    outcome: Option<Outcome>,
    signals: Vec<PartialSignal>,
    role: Option<&str>,
    turns: Vec<provenant_records::handoff::TurnEnd>,
) -> std::result::Result<AttemptRecord, Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut run = input();
    if let Some(role) = role {
        run.request.tasks[0].context = Some(provenant_records::run::WorkContext {
            role: Some(role.into()),
            ..Default::default()
        });
    }
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let dir = store.attempt_dir(run.run_id, id, false)?;
    artifacts::seal(&dir, "stdout.log", b"stream", CAPTURE_LIMIT)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    // A cancelled outcome requires proof that the stop preceded the exit.
    observed.cancel_delivered_before_exit = (outcome == Some(Outcome::Cancelled)).then_some(true);
    seal_exit(&store, &observed)?;
    let fake = Fake {
        observed: Some(route()),
        outcome,
        answer: Some(answer.into()),
        partial: signals,
        turns,
        ..Fake::default()
    };
    Ok(store.finish(id, &dir, &observed, &fake, &call)?)
}

/// A final answer that closes with a done handoff, as a finished lane's does.
const DONE_ANSWER: &str = "answer\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"note\":\"finished\"}\n```";
const STOPPED_EARLY: &str = "```handoff\n{\"schema_version\":2,\"status\":\"stopped_early\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"note\":\"Parser done; renderer and tests remain.\"}\n```";

fn two_tasks(concurrency: u64) -> PreparedRun {
    let mut run = input();
    let mut task = run.request.tasks[0].clone();
    task.task_id = TaskId::from_bytes([7; 16]);
    run.request.tasks.push(task.clone());
    run.request.concurrency = concurrency;
    run.attempts.push(NewAttempt {
        task_id: task.task_id,
        attempt_id: AttemptId::from_bytes([8; 16]),
        prompt: "exact prompt".into(),
    });
    run
}
pub(super) fn seal_exit(store: &RunStore, exit: &ExitObservation) -> TestResult {
    let mut value = serde_json::to_value(exit)?;
    let object = value.as_object_mut().ok_or("exit fixture")?;
    object.remove("publication");
    object.insert("schema_version".into(), 1.into());
    let dir = store.attempt_dir(exit.scope.run, exit.scope.attempt, false)?;
    artifacts::seal(
        &dir,
        "process-exit.json",
        &serde_json::to_vec(&value)?,
        RECORD_LIMIT,
    )?;
    Ok(())
}

/// One-task run with its own IDs: a writer when `writer`, else read-only.
fn lane(seed: u8, writer: bool) -> PreparedRun {
    let mut run = batch(seed, 1);
    if writer {
        run.request.tasks[0].writer = Some(WriterRequest {
            extra: Default::default(),
            project: "/project".into(),
            worktree: format!("/project/.worktrees/lane-{seed}").into(),
        });
    }
    run
}
/// Create and start `run`, returning its only attempt ready to claim.
fn starting(store: &RunStore, run: &PreparedRun, call: &Request<'_>) -> Result<AttemptId> {
    store.create(run, call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        call,
    )?;
    Ok(id)
}
/// A clock stopped at this time on both its wall and its host monotonic
/// coordinate, to start a claim at an exact moment.
fn at_micros(micros: i64) -> TestClock {
    TestClock::new(
        WallTime::from_micros(micros).unwrap(),
        MonoTime::from_micros(u64::try_from(micros).unwrap()),
    )
}
fn at(ms: i64) -> TestClock {
    at_micros(ms * 1000)
}

/// A clock a test steps by hand. The wall clock, the host's monotonic clock and
/// the boot identity each move on their own, as they do on a real host; waits
/// move the wall and monotonic clocks together like `TestClock`.
struct Stepped {
    inner: TestClock,
    wall_offset_micros: std::sync::Mutex<i64>,
    host_base_micros: std::sync::Mutex<u64>,
    boot: std::sync::RwLock<&'static str>,
    host_readable: std::sync::Mutex<bool>,
    boot_readable: std::sync::Mutex<bool>,
}
impl Stepped {
    /// A host that has been up `host_ms`, in boot `"boot-a"`.
    fn up(host_ms: u64) -> Self {
        Self {
            inner: clock(),
            wall_offset_micros: std::sync::Mutex::new(0),
            host_base_micros: std::sync::Mutex::new(host_ms * 1000),
            boot: std::sync::RwLock::new("boot-a"),
            host_readable: std::sync::Mutex::new(true),
            boot_readable: std::sync::Mutex::new(true),
        }
    }
    /// The wall clock alone steps, as an operator or NTP can make it.
    fn step_wall_ms(&self, ms: i64) {
        *self.wall_offset_micros.lock().unwrap() += ms * 1000;
    }
    /// Real time passes: the wall and host clocks both move.
    fn pass_micros(&self, micros: u64) {
        *self.host_base_micros.lock().unwrap() += micros;
        *self.wall_offset_micros.lock().unwrap() += i64::try_from(micros).unwrap_or(0);
    }
    fn pass_ms(&self, ms: u64) {
        self.pass_micros(ms * 1000);
    }
    /// The host restarts: a new boot whose clock has run `host_ms`.
    fn reboot(&self, boot: &'static str, host_ms: u64) {
        *self.boot.write().unwrap() = boot;
        *self.host_base_micros.lock().unwrap() = host_ms * 1000 - self.inner.mono_now().as_micros();
    }
}
impl Clock for Stepped {
    fn wall_now(&self) -> std::result::Result<WallTime, provenant_core::TimeError> {
        WallTime::from_micros(
            self.inner.wall_now()?.as_micros() + *self.wall_offset_micros.lock().unwrap(),
        )
    }
    fn mono_now(&self) -> MonoTime {
        self.inner.mono_now()
    }
    fn wait_until(&self, deadline: MonoTime) -> std::result::Result<(), provenant_core::TimeError> {
        self.inner.wait_until(deadline)
    }
    fn host_monotonic_micros(&self) -> std::result::Result<u64, provenant_core::TimeError> {
        if !*self.host_readable.lock().unwrap() {
            return Err(provenant_core::TimeError::HostClock);
        }
        Ok(self.inner.mono_now().as_micros() + *self.host_base_micros.lock().unwrap())
    }
    fn boot_identity(&self) -> std::result::Result<String, provenant_core::TimeError> {
        if !*self.boot_readable.lock().unwrap() {
            return Err(provenant_core::TimeError::HostClock);
        }
        Ok((*self.boot.read().unwrap()).to_owned())
    }
}
fn stepped_call<'a>(clock: &'a Stepped, cancel: &'a CancelToken) -> Request<'a> {
    Request { clock, cancel }
}
/// Writer lanes ready to claim, each a lane of its own run.
fn writers<const N: usize>(
    store: &RunStore,
    seeds: [u8; N],
    call: &Request<'_>,
) -> Result<[AttemptId; N]> {
    let mut ids = [AttemptId::from_bytes([0; 16]); N];
    for (id, seed) in ids.iter_mut().zip(seeds) {
        *id = starting(store, &lane(seed, true), call)?;
    }
    Ok(ids)
}

/// Probes memory, and while it does another process starts a writer.
struct RivalStarts<'a> {
    store: &'a RunStore,
    rival: AttemptId,
    clock: &'a Stepped,
    call: &'a Request<'a>,
    started: Cell<bool>,
}
impl Resources for RivalStarts<'_> {
    fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
        crate::tests::HostFacts::new(100).guardian_identity()
    }
    fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
        crate::tests::HostFacts::new(100).observe_process(pid)
    }
    fn memory(&self) -> std::io::Result<MemorySample> {
        if !self.started.replace(true) {
            self.clock.pass_ms(1_000);
            let rival = self.store.claim(self.rival, self.clock, self.call);
            assert!(rival.is_ok_and(|record| record.is_some()), "rival starts");
            self.clock.pass_ms(500);
        }
        Ok(MemorySample {
            available_bytes: 100,
            total_bytes: 100,
        })
    }
    fn disk(&self, _: &Directory) -> std::io::Result<u64> {
        Ok(u64::MAX)
    }
}

fn child_wait_lease_cycle(block_reclaim: bool) -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut store = RunStore::open(&context(temp.path())?, &call)?;
    store.set_admission_policy(AdmissionPolicy {
        startup_spacing_ms: 0,
        ..AdmissionPolicy::default()
    });
    std::fs::write(
        store.writer_registry.join("config.toml"),
        "schema_version=1\n[domains.fabric.lanes]\nmax_writer_lanes=1\nmax_project_writer_lanes=1\nmax_concurrent_builds=1\n",
    )?;
    let facts = HostFacts::new(100);
    let mut parent = lane(121, true);
    parent.request.tasks[0].build_heavy = true;
    let mut child = lane(122, true);
    child.request.tasks[0].build_heavy = true;
    let parent_id = starting(&store, &parent, &call)?;
    let parent_record = store
        .claim_observed(parent_id, &clock, &call, &facts)?
        .ok_or("parent claim")?;
    let parent_dir = store.attempt_dir(parent.run_id, parent_id, false)?;
    artifacts::seal(&parent_dir, "stdout.log", b"parent work", CAPTURE_LIMIT)?;
    let child_id = starting(&store, &child, &call)?;
    assert!(
        store
            .claim_observed(child_id, &clock, &call, &facts)?
            .is_none()
    );
    if block_reclaim {
        // The ledger yield commits before the owning store. Simulate the
        // latter failing: cleanup must still reclaim despite an active ticket.
        store.db.execute_batch("CREATE TEMP TRIGGER fail_child_wait BEFORE UPDATE ON admission BEGIN SELECT RAISE(ABORT,'injected store failure'); END;")?;
        assert!(
            store
                .set_child_wait_observed(parent_id, true, &call, &facts)
                .is_err()
        );
        store.db.execute_batch("DROP TRIGGER fail_child_wait;")?;
    } else {
        assert!(store.set_child_wait_observed(parent_id, true, &call, &facts)?);
        assert!(
            store.lane_waits_with_resources(&[store.record(parent_id, &call)?], &call, &facts)[0]
                .waiting_for
                .contains("children")
        );
    }
    let child_record = store
        .claim_observed(child_id, &clock, &call, &facts)?
        .ok_or("child claim")?;
    if block_reclaim {
        assert!(!store.set_child_wait_observed(parent_id, false, &call, &facts)?);
        let reopened = RunStore::open(&context(temp.path())?, &call)?;
        let retained = reopened.record(parent_id, &call)?;
        assert_eq!(retained.started_at, parent_record.started_at);
        assert_eq!(retained.generation, parent_record.generation);
        assert!(matches!(retained.state, AttemptState::Running { .. }));
        assert!(
            reopened.lane_waits_with_resources(&[retained], &call, &facts)[0]
                .waiting_for
                .contains("admission")
        );
        assert_eq!(
            parent_dir.read_stable("stdout.log".as_ref(), CAPTURE_LIMIT)?,
            b"parent work"
        );
    }
    let child_dir = store.attempt_dir(child.run_id, child_id, false)?;
    artifacts::seal(&child_dir, "stdout.log", b"child work", CAPTURE_LIMIT)?;
    let child_exit = ExitObservation {
        scope: RunStore::scope(&child_record),
        ..exit()
    };
    seal_exit(&store, &child_exit)?;
    store.publish_terminal(
        child_id,
        child_record.revision,
        &TerminalPublication {
            started_at: child_record.started_at,
            result: "child work",
            ..super::tests::publication()
        },
        &child_exit,
        &call,
    )?;
    // More build-heavy writer children than the host's single writer/build
    // slot must drain while their parent remains alive and waiting.
    let mut finished = vec![parent_id, child_id];
    for seed in 123..126 {
        let mut child = lane(seed, true);
        child.request.tasks[0].build_heavy = true;
        let id = starting(&store, &child, &call)?;
        let record = store
            .claim_observed(id, &clock, &call, &facts)?
            .ok_or("later child starved")?;
        assert!(matches!(
            store.record(parent_id, &call)?.state,
            AttemptState::Running { .. }
        ));
        let dir = store.attempt_dir(child.run_id, id, false)?;
        artifacts::seal(&dir, "stdout.log", b"child work", CAPTURE_LIMIT)?;
        let observed = ExitObservation {
            scope: RunStore::scope(&record),
            ..exit()
        };
        seal_exit(&store, &observed)?;
        store.publish_terminal(
            id,
            record.revision,
            &TerminalPublication {
                started_at: record.started_at,
                result: "child work",
                ..super::tests::publication()
            },
            &observed,
            &call,
        )?;
        finished.push(id);
    }
    assert!(store.set_child_wait_observed(parent_id, false, &call, &facts)?);
    let resumed = store.record(parent_id, &call)?;
    assert_eq!(resumed.started_at, parent_record.started_at);
    assert_eq!(resumed.runtime_deadline, parent_record.runtime_deadline);
    assert!(
        store
            .lane_waits_with_resources(&[resumed], &call, &facts)
            .is_empty()
    );
    let parent_exit = ExitObservation {
        scope: RunStore::scope(&parent_record),
        ..exit()
    };
    seal_exit(&store, &parent_exit)?;
    store.publish_terminal(
        parent_id,
        store.record(parent_id, &call)?.revision,
        &TerminalPublication {
            started_at: parent_record.started_at,
            result: "parent work",
            ..super::tests::publication()
        },
        &parent_exit,
        &call,
    )?;
    for id in finished {
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Succeeded,
                ..
            }
        ));
    }
    Ok(())
}

/// A run of `tasks` attempts at full concurrency, with IDs unique to `seed`.
fn batch(seed: u8, tasks: u64) -> PreparedRun {
    let mut run = input();
    let template = run.request.tasks[0].clone();
    run.run_id = provenant_core::RunId::from_bytes([seed; 16]);
    run.operation_id = provenant_core::OperationId::parse(&format!("batch-{seed}")).unwrap();
    run.request.concurrency = tasks.min(8);
    run.request.tasks.clear();
    run.attempts.clear();
    for index in 0..tasks as u8 {
        let mut task = template.clone();
        task.task_id = TaskId::from_bytes([seed, index, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
        run.attempts.push(NewAttempt {
            task_id: task.task_id,
            attempt_id: AttemptId::from_bytes([
                seed, index, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            ]),
            prompt: "exact prompt".into(),
        });
        run.request.tasks.push(task);
    }
    run
}

/// Starts a one-task run after `damage` has altered its stored prompt, and
/// asserts the attempt ends Rejected before preparation with its reason recorded.
fn assert_unreadable_prompt_rejected(
    damage: impl FnOnce(&std::path::Path) -> TestResult,
) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    let prompt = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}/prompt.txt", run.run_id));
    damage(&prompt)?;
    let fake = Fake::default();
    let started = store.start(run.run_id, &fake, &|_, _| unreachable!(), &call)?;
    // Restore access so the tempdir can be removed whatever the outcome.
    if prompt.exists() {
        std::fs::set_permissions(&prompt, std::fs::Permissions::from_mode(0o600))?;
    }
    assert!(matches!(
        &started[..],
        [Started::Refused { attempt, kind: RefusalKind::StoredPromptUnreadable, .. }]
            if *attempt == id
    ));
    assert_eq!(fake.prepared.get(), 0);
    assert_eq!(terminal(&store, id, &call)?, Outcome::Rejected);
    let fact = store.attempt(id, &call)?;
    assert_eq!(
        fact.terminal_state,
        provenant_records::AttemptOutcome::Rejected
    );
    assert!(fact.started_at.is_none());
    let [Started::Refused { reason, .. }] = &started[..] else {
        unreachable!()
    };
    let recorded = fact.reason.as_deref().ok_or("missing refusal reason")?;
    assert!(reason.starts_with(recorded));
    assert_eq!(recorded.len(), reason.len().min(256));
    assert!(
        store
            .start(run.run_id, &fake, &|_, _| unreachable!(), &call)?
            .is_empty()
    );
    Ok(())
}

fn holder_release_temporary_fixture(valid_binding: bool) -> TestResult {
    let temp = tempfile::tempdir()?;
    let custody_dir = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store.claim(id, &clock, &call)?.ok_or("not admitted")?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let mut observed = exit();
    observed.scope = RunStore::scope(&store.record(id, &call)?);
    seal_exit(&store, &observed)?;
    store.finish(id, &attempt, &observed, &Fake::default(), &call)?;
    let temporary = store.temporary_path(id, &call)?;
    std::fs::create_dir_all(&temporary)?;
    std::fs::write(temporary.join("payload.lock"), "discard")?;
    drop(provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(custody_dir.path())?,
        observed.scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let reservation = custody_dir
        .path()
        .join(observed.scope.attempt.to_string())
        .join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let meta = attempt.metadata()?;
    value["launch_state"] = "never_spawned".into();
    value["unresolved_cleanup"] = false.into();
    value["census_incomplete"] = false.into();
    value["attempt_device"] = meta.device.into();
    value["attempt_inode"] = if valid_binding {
        meta.inode
    } else {
        meta.inode + 1
    }
    .into();
    std::fs::write(reservation, serde_json::to_vec(&value)?)?;
    let recovery = provenant_supervisor::custody::acquire_recovery(
        Directory::open_anchor(custody_dir.path())?,
        observed.scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?;
    crate::holder::release_and_reclaim(&attempt, observed.scope, &recovery)?;
    // Settlement publishes release without walking temporary payloads. GC
    // reclaims them later, even when the reservation binding is valid.
    assert!(temporary.exists());
    assert_eq!(std::fs::read(temporary.join("payload.lock"))?, b"discard");
    assert!(
        attempt
            .metadata_at(std::ffi::OsStr::new("release.json"), false)
            .is_ok()
    );
    Ok(())
}

/// Recovery drains the reactions a terminal commit owes; tests that check the
/// rest of recovery finish them first.
fn finish_reactions(store: &RunStore) -> TestResult {
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    for run in store.open_runs(100, &call)? {
        store.drain_reactions(run, &clock, &cancel, &mut |reaction, _| {
            reaction.graph_done = true;
            reaction.notice_done = true;
            reaction.log_done = true;
            reaction.ledger_done = true;
            Ok(())
        })?;
    }
    Ok(())
}

fn terminal_reason(store: &RunStore, id: AttemptId, call: &Request<'_>) -> Result<String> {
    match store.record(id, call)?.state {
        AttemptState::Terminal { reason, .. } => Ok(reason.unwrap_or_default()),
        _ => Err(RunError::Nonterminal),
    }
}

fn terminal(store: &RunStore, id: AttemptId, call: &Request<'_>) -> Result<Outcome> {
    match store.record(id, call)?.state {
        AttemptState::Terminal { outcome, .. } => Ok(outcome),
        _ => Err(RunError::Nonterminal),
    }
}

fn placed_writer_without_reservation(cancel_run: bool, evidence: u8) -> TestResult {
    placed_writer_with_missing_or_stale_reservation(RecoveryCase {
        cancel_run,
        evidence,
        reservation: ReservationCase::Missing,
        removed: false,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReservationCase {
    Missing,
    Stale,
    Newer,
    DifferentAttempt,
}

struct RecoveryCase {
    cancel_run: bool,
    evidence: u8,
    reservation: ReservationCase,
    removed: bool,
}

fn placed_writer_with_missing_or_stale_reservation(case: RecoveryCase) -> TestResult {
    let RecoveryCase {
        cancel_run,
        evidence,
        reservation: reservation_case,
        removed,
    } = case;
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = input();
    let worktree = temp.path().join("lane");
    std::fs::create_dir(&worktree)?;
    run.request.tasks[0].writer = Some(WriterRequest {
        extra: Default::default(),
        project: temp.path().to_path_buf(),
        worktree: worktree.clone(),
    });
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store
        .claim(id, &at_micros(100), &call)?
        .ok_or("not admitted")?;
    if reservation_case == ReservationCase::Stale {
        let old = store.record(id, &call)?;
        let mut next = old.clone();
        next.generation += 1;
        next.revision += 1;
        store.write(&call, |tx| {
            crate::admission::update_attempt(tx, &old, &next)
        })?;
    }
    let scope = RunStore::scope(&store.record(id, &call)?);
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let custody = store
        .writer_registry
        .join("worktrees")
        .join(context.project_id.to_string())
        .join(provenant_confinement::worktrees::state_dir_name(&worktree));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&custody)?;
    drop(provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&custody)?,
        scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let summary =
        serde_json::json!({"schema_version":1, "scope":scope, "requester":context.actor_id});
    artifacts::seal(
        &attempt,
        "launch.json",
        &serde_json::to_vec(&summary)?,
        RECORD_LIMIT,
    )?;
    if evidence != 0 {
        let (guardian, provider) = if evidence == 1 || evidence == 3 {
            let provenant_supervisor::census::ProcessObservation::Present(fact) =
                provenant_supervisor::census::current_process()
            else {
                return Err("current process identity unavailable".into());
            };
            let mut guardian = fact.identity.clone();
            if evidence == 3 {
                guardian.native_start = guardian.native_start.saturating_add(1);
            }
            (guardian, fact.identity)
        } else {
            use provenant_supervisor::census::ProcessIdentity;
            (
                ProcessIdentity {
                    boot: OLD_BOOT.into(),
                    pid: 100,
                    native_start: 1,
                    group_id: 100,
                    session_id: 100,
                },
                ProcessIdentity {
                    boot: OLD_BOOT.into(),
                    pid: 101,
                    native_start: 2,
                    group_id: 101,
                    session_id: 100,
                },
            )
        };
        attempt.atomic_write(
            std::ffi::OsStr::new("process.json"),
            ".checkpoint-",
            &serde_json::to_vec(
                &serde_json::json!({"schema_version": 1, "scope": scope, "guardian": guardian, "provider": provider}),
            )?,
            provenant_os::fs::PublishMode::Replace,
        )?;
    }
    // A launch summary alone does not prove the provider was never spawned.
    let reservation = custody
        .join(scope.attempt.to_string())
        .join("reservation.json");
    if reservation_case != ReservationCase::Missing {
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
        match reservation_case {
            ReservationCase::Stale => value["scope"]["generation"] = (scope.generation - 1).into(),
            ReservationCase::Newer => value["scope"]["generation"] = (scope.generation + 1).into(),
            ReservationCase::DifferentAttempt => {
                value["scope"]["attempt"] = serde_json::to_value(AttemptId::from_bytes([9; 16]))?
            }
            ReservationCase::Missing => unreachable!(),
        }
        std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    } else {
        std::fs::remove_file(&reservation)?;
    }
    if removed {
        std::fs::remove_dir(&worktree)?;
    }
    let system = provenant_supervisor::census::SystemProcessObserver;
    let reboot = RebootObserver(NEW_BOOT);
    let observer: &dyn provenant_supervisor::census::ProcessObserver =
        if evidence == 2 { &reboot } else { &system };
    let should_settle = evidence == 2
        && matches!(
            reservation_case,
            ReservationCase::Missing | ReservationCase::Stale
        );
    if cancel_run {
        let before = std::fs::read(&reservation).ok();
        assert_eq!(
            store.cancel_with_observer(id, "stop", &call, observer)?,
            CancelReceipt::Requested
        );
        let request: serde_json::Value = serde_json::from_slice(
            &artifacts::read_sealed(&attempt, "cancel.json", RECORD_LIMIT)?
                .ok_or("cancel request missing")?,
        )?;
        assert_eq!(request["scope"], serde_json::to_value(scope)?);
        assert!(!matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
        assert_eq!(std::fs::read(&reservation).ok(), before);
    }
    let reconciled =
        store.reconcile_with_observer(run.run_id, &Fake::default(), &call, observer)?;
    if reservation_case == ReservationCase::DifferentAttempt {
        assert!(
            matches!(reconciled.as_slice(),
            [(attempt, Reconciled::Uncertain { reason: UncertainReason::RebootProofUnavailable { .. } })] if *attempt == id),
            "{reconciled:?}"
        );
    } else {
        assert!(reconciled.contains(&(
            id,
            match evidence {
                1 => Reconciled::Held,
                2 if should_settle => Reconciled::Finished,
                _ => Reconciled::Uncertain {
                    reason: UncertainReason::GuardianNotObservable
                },
            }
        )));
    }
    if should_settle {
        assert_eq!(terminal(&store, id, &call)?, Outcome::Lost);
        assert!(!reservation.exists());
        if removed {
            assert!(
                matches!(store.record(id, &call)?.state, AttemptState::Terminal { reason: Some(reason), .. } if reason.contains("worktree"))
            );
        }
    } else {
        assert!(!matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
        if reservation_case != ReservationCase::Missing {
            assert!(reservation.exists());
        }
    }
    Ok(())
}

const OLD_BOOT: &str = "darwin:00000000-0000-0000-0000-000000000000";
const NEW_BOOT: &str = "darwin:11111111-1111-1111-1111-111111111111";

struct RebootObserver(&'static str);
impl provenant_supervisor::census::ProcessObserver for RebootObserver {
    fn boot_identity(
        &self,
    ) -> std::result::Result<String, provenant_supervisor::census::CensusError> {
        Ok(self.0.into())
    }
    fn observe(&self, _: i64) -> provenant_supervisor::census::ProcessObservation {
        // Every recorded process is gone, as after a reboot or a crash.
        provenant_supervisor::census::ProcessObservation::Absent
    }
    fn census(
        &self,
        _: usize,
    ) -> std::result::Result<
        provenant_supervisor::census::Census,
        provenant_supervisor::census::CensusError,
    > {
        unreachable!("reboot proof needs only the boot identity")
    }
}

fn recover_rebooted_writer(
    cancel_run: bool,
    interrupted: bool,
    writer: bool,
    boot: &'static str,
) -> TestResult {
    recover_rebooted_writer_inner(cancel_run, interrupted, writer, boot, false, false, false)
}

fn recover_rebooted_writer_removed_worktree() -> TestResult {
    recover_rebooted_writer_inner(false, false, true, NEW_BOOT, true, false, false)
}

fn recover_rebooted_writer_inner(
    cancel_run: bool,
    interrupted: bool,
    writer: bool,
    boot: &'static str,
    removed: bool,
    changed_device: bool,
    unresolved: bool,
) -> TestResult {
    let observer = RebootObserver(boot);
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut run = input();
    let worktree = temp.path().join("lane");
    std::fs::create_dir(&worktree)?;
    std::fs::write(worktree.join("unfinished.rs"), "keep my work")?;
    run.request.tasks[0].writer = writer.then(|| WriterRequest {
        extra: Default::default(),
        project: temp.path().to_path_buf(),
        worktree: worktree.clone(),
    });
    store.create(&run, &call)?;
    let id = run.attempts[0].attempt_id;
    store.mark_starting(
        &store.record(id, &call)?,
        &route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    store
        .claim(id, &at_micros(100), &call)?
        .ok_or("not admitted")?;
    let before = store.record(id, &call)?;
    let scope = RunStore::scope(&before);
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let custody = if writer {
        store
            .writer_registry
            .join("worktrees")
            .join(context.project_id.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(&worktree))
    } else {
        context
            .state_root
            .as_path()
            .join(format!("runs/{}/attempts/{id}", run.run_id))
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&custody)?;
    drop(provenant_supervisor::fixture::acquire_writer(
        Directory::open_anchor(&custody)?,
        scope,
        call.lock_budget(),
        &cancel,
        &clock,
    )?);
    let reservation = custody
        .join(scope.attempt.to_string())
        .join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let meta = attempt.metadata()?;
    let guardian = serde_json::json!({
        "boot": "darwin:00000000-0000-0000-0000-000000000000",
        "pid": 123, "native_start": 1, "group_id": 123, "session_id": 123,
    });
    value["boot"] = guardian["boot"].clone();
    value["guardian"] = guardian.clone();
    if boot == OLD_BOOT && !unresolved {
        value["provider"] = serde_json::json!({
            "boot": OLD_BOOT, "pid": 124, "native_start": 1,
            "group_id": 124, "session_id": 123,
        });
        value["unresolved_cleanup"] = false.into();
        value["census_incomplete"] = false.into();
    }
    value["launch_state"] = "spawned".into();
    value["attempt_device"] = meta.device.into();
    value["attempt_inode"] = meta.inode.into();
    if changed_device {
        value["reservation_device"] =
            (value["reservation_device"].as_u64().ok_or("device")? + 1).into();
        value["attempt_device"] = (meta.device + 1).into();
    }
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    let summary =
        serde_json::json!({"schema_version":1, "scope":scope, "requester":context.actor_id});
    artifacts::seal(
        &attempt,
        "launch.json",
        &serde_json::to_vec(&summary)?,
        RECORD_LIMIT,
    )?;
    let checkpoint = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1, "scope": scope, "guardian": guardian,
        "provider": value["provider"],
    }))?;
    artifacts::seal(&attempt, "process.json", &checkpoint, RECORD_LIMIT)?;
    // Recovery preserves existing captures, results, scratch and provider history.
    let telemetry = [
        ("stdout.log", b"partial output".as_slice()),
        ("stderr.log", b"partial diagnostics".as_slice()),
        ("result.md", b"partial result".as_slice()),
        ("process.json", checkpoint.as_slice()),
    ];
    for (name, bytes) in telemetry {
        if name != "process.json" {
            artifacts::seal(&attempt, name, bytes, CAPTURE_LIMIT)?;
        }
    }
    let temporary = store.temporary_path(id, &call)?;
    std::fs::write(temporary.join("keep"), "scratch")?;
    artifacts::subdir(&attempt, std::ffi::OsStr::new("provider-state"), true)?;
    let history = context
        .state_root
        .as_path()
        .join(format!("runs/{}/attempts/{id}/provider-state", run.run_id));
    std::fs::write(history.join("session"), "history")?;

    if removed {
        std::fs::remove_dir_all(&worktree)?;
    }

    if boot == OLD_BOOT {
        // Held writer exclusion means a guardian may remain: stay uncertain.
        let held = Directory::open_anchor(&custody)?.try_lock(
            std::ffi::OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        assert!(
            matches!(store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?.as_slice(),
            [(attempt, Reconciled::Uncertain { .. })] if *attempt == id)
        );
        assert_eq!(store.record(id, &call)?, before);
        assert!(reservation.exists());
        assert_eq!(std::fs::read_to_string(temporary.join("keep"))?, "scratch");
        drop(held);
        if changed_device {
            return Ok(());
        }
        if unresolved {
            assert!(matches!(
                store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?.as_slice(),
                [(attempt, Reconciled::Uncertain { .. })] if *attempt == id
            ));
            assert_eq!(store.record(id, &call)?, before);
            assert!(reservation.exists());
            return Ok(());
        }
        // Free exclusion and an absent guardian and provider leave no holder.
        assert_eq!(store.reap_with(&call, |_, _| Ok(()))?, 1);
        if cancel_run {
            assert_eq!(
                store.cancel_with_observer(id, "stop", &call, &observer)?,
                CancelReceipt::Requested
            );
        }
        assert_eq!(
            store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?,
            vec![(id, Reconciled::Finished)]
        );
        let AttemptState::Terminal {
            outcome, reason, ..
        } = store.record(id, &call)?.state
        else {
            return Err("holderless attempt stayed open".into());
        };
        assert_eq!(outcome, Outcome::Lost);
        assert_eq!(
            reason.as_deref(),
            Some(crate::controls::LostReason::Holderless.message())
        );
        assert!(!reservation.exists());
        assert_eq!(std::fs::read_to_string(temporary.join("keep"))?, "scratch");
        return Ok(());
    }
    if interrupted {
        let recovery = provenant_supervisor::custody::acquire_recovery(
            Directory::open_anchor(&custody)?,
            scope,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        let proof =
            provenant_supervisor::fixture::after_reboot_with(recovery, &attempt, scope, &observer)?
                .ok_or("reboot not proven")?;
        store.commit_reboot_lost(&proof, &call)?;
        drop(proof);
        assert!(reservation.exists());
        assert_eq!(terminal(&store, id, &call)?, Outcome::Lost);
    }
    if cancel_run && !interrupted {
        let custody_lock = attempt.try_lock(
            std::ffi::OsStr::new("custody.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        assert_eq!(
            store.cancel_with_observer(id, "stop", &call, &observer)?,
            CancelReceipt::Requested
        );
        assert_eq!(store.record(id, &call)?, before);
        assert!(reservation.exists());
        drop(custody_lock);
    }
    if cancel_run {
        let receipt = store.cancel_with_observer(id, "stop", &call, &observer)?;
        assert!(matches!(
            receipt,
            CancelReceipt::AlreadyTerminal
                | CancelReceipt::AlreadyRequested
                | CancelReceipt::Requested
        ));
        store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?;
    } else {
        assert_eq!(
            store.reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?,
            vec![(id, Reconciled::Finished)]
        );
    }
    assert_eq!(terminal(&store, id, &call)?, Outcome::Lost);
    if removed {
        assert!(
            matches!(store.record(id, &call)?.state, AttemptState::Terminal { reason: Some(reason), .. } if reason.contains("worktree"))
        );
    }
    let fact = store.attempt(id, &call)?;
    assert_eq!(fact.terminal_state, Outcome::Lost.into());
    assert!(fact.output_refs.is_empty());
    let after = store.record(id, &call)?;
    assert_eq!(after.started_at, before.started_at);
    assert_eq!(after.applied_route, before.applied_route);
    assert_eq!(after.usage, before.usage);
    assert_eq!(after.session, before.session);
    assert_eq!(store.run(run.run_id, &call)?.closure, Closure::Closed);
    assert!(store.writer_fences()?.is_empty());
    assert!(!reservation.exists());
    assert!(
        store
            .reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?
            .is_empty()
    );
    // Also retry a crash after fence release but before clearing the marker.
    store.set_temporary_retained(id, true, &call)?;
    assert!(
        store
            .reconcile_with_observer(run.run_id, &Fake::default(), &call, &observer)?
            .is_empty()
    );
    for (name, bytes) in telemetry {
        assert_eq!(
            attempt.read_bounded(std::ffi::OsStr::new(name), CAPTURE_LIMIT)?,
            bytes
        );
    }
    assert_eq!(std::fs::read_to_string(temporary.join("keep"))?, "scratch");
    assert_eq!(std::fs::read_to_string(history.join("session"))?, "history");
    if !removed {
        assert_eq!(
            std::fs::read_to_string(worktree.join("unfinished.rs"))?,
            "keep my work"
        );
    }
    assert!(artifacts::read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_none());
    finish_reactions(&store)?;
    assert!(store.open_runs(10, &call)?.is_empty());
    Ok(())
}

/// Refuses preparation, after recording whether the launch had a short alias.
struct RefusesAfterAlias(std::cell::Cell<Option<bool>>);

impl Providers for RefusesAfterAlias {
    fn prepare(
        &self,
        _: &TaskRequest,
        _: &str,
        paths: &LaunchPaths,
        _: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
        self.0.set(Some(paths.temporary_alias.is_some()));
        Err(crate::fixture_refusal("provider unavailable"))
    }
    fn decode(
        &self,
        _: &RouteRecord,
        _: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        unreachable!("no provider runs")
    }
}

#[test]
fn read_only_disk_floor_warning_is_emitted_once_by_admit() -> TestResult {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor};
    use std::{ffi::OsString, time::Duration};
    const CHILD: &str = "PROVENANT_TEST_ADMISSION_WARNING";
    if std::env::var_os(CHILD).is_none() {
        let temp = tempfile::tempdir()?;
        let argv: Vec<OsString> = vec![
            std::env::current_exe()?.into(),
            "--exact".into(),
            "dispatch_tests::read_only_disk_floor_warning_is_emitted_once_by_admit".into(),
            "--nocapture".into(),
        ];
        let env = [(CHILD.into(), "1".into())];
        let mut options = ExecOptions::new(&argv, temp.path(), Duration::from_secs(10));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 8192 };
        options.stderr = StreamPolicy::Capture { limit: 8192 };
        let output = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(output.termination, Termination::Exit(0), "{output:?}");
        let stderr = String::from_utf8(output.stderr.bytes)?;
        assert_eq!(
            stderr.matches("admission warning [DISK_FLOOR]").count(),
            1,
            "{stderr}"
        );
        assert!(
            stderr.contains(
                "99 bytes available; gc.free_floor is 100 bytes (source: host configuration)"
            ),
            "{stderr}"
        );
        assert!(stderr.contains("read-only lane is proceeding"), "{stderr}");
        assert!(stderr.contains("provenant gc --apply"), "{stderr}");
        return Ok(());
    }
    struct LowDisk {
        facts: HostFacts,
        probes: Cell<u32>,
    }
    impl Resources for LowDisk {
        fn memory(&self) -> std::io::Result<MemorySample> {
            self.facts.memory()
        }
        fn disk(&self, _: &Directory) -> std::io::Result<u64> {
            Ok(99)
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            self.facts.observe_process(pid)
        }
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            self.facts.guardian_identity()
        }
        fn process_budget(&self) -> std::io::Result<provenant_os::sys::resources::ProcessBudget> {
            let probes = self.probes.get() + 1;
            self.probes.set(probes);
            // Hold admission across several probes to prove the warning is once.
            Ok(provenant_os::sys::resources::ProcessBudget {
                used: if probes < 3 { 100 } else { 0 },
                limit: 100,
            })
        }
    }
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let registry = temp.path().canonicalize()?.join("registry");
    std::fs::DirBuilder::new().mode(0o700).create(&registry)?;
    std::fs::write(
        registry.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='100B'\n",
    )?;
    let store = RunStore::open(&context, &call)?.with_writer_registry(&registry);
    let run = lane(99, false);
    let id = starting(&store, &run, &call)?;
    let attempt = store.attempt_dir(run.run_id, id, false)?;
    let probe = LowDisk {
        facts: HostFacts::new(100),
        probes: Cell::new(0),
    };
    let admitted = store.admit(id, &attempt, &probe, &clock, &cancel)?;
    assert!(matches!(admitted.state, AttemptState::Running { .. }));
    assert!(probe.probes.get() >= 3);
    Ok(())
}

#[path = "dispatch_admission_tests.rs"]
mod admission_tests;
#[path = "dispatch_custody_tests.rs"]
mod custody_tests;
#[path = "dispatch_disk_admission_tests.rs"]
mod disk_admission_tests;
#[path = "dispatch_core_tests.rs"]
mod dispatch_core_tests;
#[path = "dispatch_ledger_tests.rs"]
mod ledger_tests;
#[path = "dispatch_lifecycle_tests.rs"]
mod lifecycle_tests;
#[path = "dispatch_reconcile_tests.rs"]
mod reconcile_tests;
#[path = "dispatch_recovery_tests.rs"]
mod recovery_tests;
#[path = "dispatch_settlement_tests.rs"]
mod settlement_tests;
#[path = "dispatch_temporary_tests.rs"]
mod temporary_tests;
#[path = "dispatch_writer_tests.rs"]
mod writer_tests;
