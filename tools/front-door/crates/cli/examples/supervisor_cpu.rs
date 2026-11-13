//! Manual macOS capture of the guardian with the production Fabric child reader.
//! cargo run --release -p provenant-cli --example supervisor_cpu -- idle|churn
#![allow(clippy::disallowed_methods)]

use provenant_core::{ActorId, AttemptId, CancelToken, Clock, Digest, RootInputs, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, LockMode},
};
use provenant_supervisor::{GuardianHooks, Holder, custody::*, fixture, machine::StopReason};
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    time::{Duration, Instant},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct InertHolder;
impl Holder for InertHolder {
    fn admit(&self, _: &[u8], _: &Directory) -> provenant_supervisor::custody::Result<()> {
        Ok(())
    }
    fn terminal(
        &self,
        _: &[u8],
        attempt: &Directory,
        exit: &ExitObservation,
        _: &Recovery,
    ) -> provenant_supervisor::custody::Result<()> {
        attempt.mkdir(OsStr::new("quarantine"), 0o700)?;
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "scope": exit.scope, "kind": "quarantine_manifest",
        }))
        .map_err(|_| CustodyError::InvalidRecord)?;
        attempt
            .open_verified_directory(OsStr::new("quarantine"))?
            .atomic_write(
                OsStr::new("manifest.json"),
                ".fixture-",
                &manifest,
                provenant_os::fs::PublishMode::NoReplace,
            )?
            .require_durable()?;
        publish_release(attempt, exit.scope)
    }
}

/// Admission is synthetic; census uses the real composition edge. Run's store
/// and provider credentials are not needed to measure this callback.
struct CensusHolder<'a>(provenant_cli::run::HostHolder<'a>);
impl Holder for CensusHolder<'_> {
    fn has_live_children(&self, raw: &[u8]) -> provenant_supervisor::custody::Result<bool> {
        self.0.has_live_children(raw)
    }
    fn admit(&self, _: &[u8], _: &Directory) -> provenant_supervisor::custody::Result<()> {
        Ok(())
    }
    fn terminal(
        &self,
        raw: &[u8],
        attempt: &Directory,
        exit: &ExitObservation,
        recovery: &Recovery,
    ) -> provenant_supervisor::custody::Result<()> {
        InertHolder.terminal(raw, attempt, exit, recovery)
    }
}

fn budget(clock: &SystemClock) -> Result<WaitBudget> {
    Ok(WaitBudget::cli(clock.mono_now(), 5)?)
}

fn capture(root: &Path, churn: bool) -> Result {
    let root = std::fs::canonicalize(root)?;
    let home = root.join("home");
    let project = home.join("project");
    let state = root.join("state");
    let attempt = root.join("attempt");
    let writer = root.join("writer");
    for path in [&project, &attempt, &writer] {
        std::fs::create_dir_all(path)?;
    }
    let mut inputs = RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let ctx = provenant_cli::fabric::Context::open(roots, &home, &project)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let scope = Scope {
        attempt: AttemptId::from_bytes([2; 16]),
        generation: 1,
    };
    let holder = provenant_run::HolderContext {
        schema_version: 1,
        roots: provenant_run::RootSeed {
            home,
            checkout: project.clone(),
            state_root: state.clone(),
        },
        project_id: ctx.identity.project,
        actor_id: ActorId::from_bytes([3; 16]),
        attempt_id: scope.attempt,
        host_delivery: None,
        sealed: None,
        launch: Some(provenant_run::HostLaunchContext::from_environment(
            &state,
            &project,
            provenant_os::git::git_executable().ok_or("Git unavailable")?,
        )),
    };
    // Retain a real pending graph so the census also exercises checkpoint reads.
    use provenant_fabric_runtime::graph::{
        FileJournal, GraphCheckpoint, GraphDocument, GraphExecution, GraphJournal,
    };
    let mut journal = FileJournal::open(
        &ctx.roots.state,
        ctx.identity.project,
        "cpu",
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let document: GraphDocument = serde_json::from_value(
        serde_json::json!({"schema_version":2,"graph_id":"cpu","nodes":[{"id":"parent","access":"read_only","prompt":"fixture","route":{"alias":"junior"}}]}),
    )?;
    journal.bind(GraphExecution {
        review_base: None,
        extra: Default::default(),
        integration_base: None,
        directory: project,
        document,
    })?;
    let checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"cpu","plan_digest":"fixture","nodes":[{
            "id":"parent","state":"running","attempts":[{"key":{"graph_id":"cpu","node_id":"parent","route_index":0},"launched":true,"route":{"alias":"junior"},"state":"running","attempt_ref":format!("{}/{}",ctx.identity.host,scope.attempt),"launch_actor":"fixture"}]
        }]
    }))?;
    journal.save(&checkpoint)?;
    drop(journal);
    let socket = root.join("ready.sock");
    let listener = UnixListener::bind(&socket)?;
    let mut lease = fixture::acquire_writer(
        Directory::open_anchor(&writer)?,
        scope,
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let publication = PublicationReservation::prepare(
        Directory::open_anchor(&attempt)?,
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let command = ProviderCommand {
        profile_file: None,
        priority: 0,
        lane_ports: None,
        requester: holder.actor_id,
        resolved_route: "fixture/cpu".into(),
        permission_plan_digest: Digest::from_bytes([0; 32]),
        key: provenant_supervisor::custody::fixture_key(4),
        protected_commands: vec![],
        config_environment: fixture::config_environment(),
        argv: fixture::argv_with_args(
            if churn { "cpu-churn" } else { "wait-on-socket" },
            &[socket.into_os_string()],
        )?,
        environment: fixture::environment(&root),
        cwd: root.clone(),
        stdin: vec![],
        secret_input: None,
        holder_context: serde_json::to_vec(&holder)?,
    };
    let handle = launch(
        OwnedLaunch {
            command,
            lease: &mut lease,
            handshake: budget(&clock)?,
            cancel: &cancel,
            clock: &clock,
        },
        CapturePlan::default(),
        publication,
    )?;
    let mut held = fixture::fixture_accept(&listener, Duration::from_secs(5))?;
    assert_eq!(
        fixture::fixture_read_byte(&mut held, Duration::from_secs(5))?,
        b'R'
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !attempt.join("process.json").is_file() {
        if Instant::now() >= deadline {
            return Err("guardian observation unavailable".into());
        }
        std::thread::park_timeout(Duration::from_millis(10));
    }
    let mut value = fixture::capture_guardian_cpu(&attempt)?;
    value["workload"] = if churn {
        "loop spawning /usr/bin/true"
    } else {
        "idle socket wait"
    }
    .into();
    value["fabric_child_reader"] = true.into();
    writeln!(std::io::stdout().lock(), "{value}")?;
    stop(&handle, StopReason::ExplicitCancellation)?;
    let exit = observe(&handle, budget(&clock)?, &cancel, &clock)?;
    let Observation::Exited(exit) = exit else {
        return Err("guardian did not publish exit".into());
    };
    assert_eq!(exit.stop_reason, Some(StopReason::ExplicitCancellation));
    assert!(!exit.unresolved_cleanup, "{exit:?}");
    drop(held);
    drop(lease);
    let _settled = Directory::open_anchor(&writer)?.lock(
        OsStr::new("writer.lock"),
        LockMode::Exclusive,
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    Ok(())
}

fn main() -> Result {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "internal")
        && args.get(1).is_some_and(|arg| arg == "supervise")
    {
        let holder = CensusHolder(provenant_cli::run::HostHolder::new(&InertHolder));
        provenant_supervisor::supervise(
            &args[2..],
            GuardianHooks {
                holder: Some(&holder),
                observer: Some(&fixture::RecordedBootObserver),
                ..Default::default()
            },
        )?;
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "--fixture")
        && args.get(1).is_some_and(|arg| arg == "cpu-churn")
    {
        let mut socket = UnixStream::connect(args.get(2).ok_or("socket missing")?)?;
        socket.write_all(b"R")?;
        loop {
            if !std::process::Command::new("/usr/bin/true")
                .status()?
                .success()
            {
                return Err("churning child failed".into());
            }
        }
    }
    if fixture::dispatch()? {
        return Ok(());
    }
    let churn = match args.as_slice() {
        [arg] if arg == "idle" => false,
        [arg] if arg == "churn" => true,
        _ => return Err("usage: supervisor_cpu idle|churn (macOS)".into()),
    };
    // The capture represents a host supervisor. Re-enter without lane identity,
    // while retaining the lane's filesystem and process restrictions.
    if provenant_cli::fabric::in_lane() {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(&args)
            .env_remove(provenant_core::RUN_STATE_DIRECTORY_ENV)
            .env_remove(provenant_run::LANE_DEPTH_ENV)
            .env_remove(provenant_run::PARENT_ATTEMPT_ENV)
            .status()?;
        return if status.success() {
            Ok(())
        } else {
            Err("host capture failed".into())
        };
    }
    let root = provenant_os::fixture::short_tempdir("ready.sock", |root| {
        tempfile::Builder::new().prefix("cpu").tempdir_in(root)
    })?;
    let cleanup = fixture::ProcessCleanupGuard::new(root.path());
    capture(root.path(), churn)?;
    cleanup.disarm();
    Ok(())
}
