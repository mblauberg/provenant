use provenant_core::{AttemptId, CancelToken, RunId, WaitBudget};
use provenant_supervisor::{
    ExitPublisher, GuardianHooks, clock::SystemClock, custody::*, fixture, fs::Directory,
};
use std::error::Error;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
fn scope() -> Scope {
    Scope {
        run: RunId::from_bytes([11; 16]),
        attempt: AttemptId::from_bytes([12; 16]),
        generation: 1,
    }
}
fn budget(clock: &SystemClock) -> Result<WaitBudget> {
    use provenant_core::Clock;
    Ok(WaitBudget::cli(clock.mono_now(), 5)?)
}
fn start(root: &Path, name: &str, scenario: &str) -> Result<(WriterLease, CustodyHandle)> {
    let writer_path = root.join(format!("{name}-writer"));
    let attempt_path = root.join(format!("{name}-attempt"));
    std::fs::create_dir(&writer_path)?;
    std::fs::create_dir(&attempt_path)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut writer = fixture::acquire_writer(
        Directory::open_anchor(&writer_path)?,
        scope(),
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let publication = PublicationReservation::prepare(
        Directory::open_anchor(&attempt_path)?,
        budget(&clock)?,
        &cancel,
        &clock,
    )?;
    let command = ProviderCommand {
        requester: provenant_core::ActorId::from_bytes([4; 16]),
        resolved_route: "fixture/provider".into(),
        permission_plan_digest: provenant_core::Digest::from_bytes([0; 32]),
        task: provenant_core::TaskId::from_bytes([13; 16]),
        argv: fixture::argv(scenario)?,
        environment: fixture::environment(root),
        cwd: root.into(),
        stdin: vec![],
    };
    let handle = launch(
        OwnedLaunch {
            command,
            writer: &mut writer,
            handshake: budget(&clock)?,
            cancel: &cancel,
            clock: &clock,
        },
        CapturePlan::default(),
        publication,
    )?;
    Ok((writer, handle))
}
fn wait_for_pending_exit(root: &Path, name: &str) -> Result<Vec<u8>> {
    let attempt = root.join(format!("{name}-attempt"));
    let observation_path = attempt.join("process.json");
    let exit_path = attempt.join("process-exit.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pending = std::fs::read(&observation_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| {
                value
                    .get("publication_pending")
                    .and_then(|item| item.as_bool())
            })
            == Some(true);
        if pending {
            return Ok(std::fs::read(exit_path)?);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("guardian did not expose pending publication").into());
        }
        std::thread::yield_now();
    }
}
fn wait_for_file(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if path.is_file() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("publisher did not confirm directory sync").into());
        }
        std::thread::yield_now();
    }
}
fn lock_is_held(path: &Path, name: &OsStr) -> Result<bool> {
    let directory = Directory::open_anchor(path)?;
    assert!(directory.metadata_at(name, false).is_ok());
    let clock = SystemClock::new();
    let lock = directory.lock(
        name,
        provenant_supervisor::fs::LockMode::Exclusive,
        {
            use provenant_core::Clock;
            WaitBudget::cli(clock.mono_now(), 1)?
        },
        &CancelToken::new(),
        &clock,
    );
    Ok(matches!(
        lock,
        Err(provenant_supervisor::fs::FsError::LockTimeout { .. })
    ))
}
fn exit_publication_enospc_and_visible_sync_retry_keeps_one_snapshot(root: &Path) -> Result<()> {
    let state_dir = root.join("PROVENANT_STATE_DIR");
    std::fs::create_dir(&state_dir)?;
    std::fs::write(
        state_dir.join(fixture::S2_EXIT_PUBLICATION_CONFIG),
        b"enospc-post-link-sync-v1\n",
    )?;
    std::fs::write(
        state_dir.join(fixture::S2_EXIT_PUBLICATION_ENOSPC_ONCE),
        b"inject first publication ENOSPC",
    )?;
    std::fs::write(
        state_dir.join(fixture::S2_EXIT_PUBLICATION_SYNC_BLOCKED),
        b"keep directory sync pending until the test removes this marker",
    )?;

    let name = "publication-fault";
    let (writer, handle) = start(root, name, "exit")?;
    let before = wait_for_pending_exit(root, name)?;
    drop(writer);

    let attempt = root.join(format!("{name}-attempt"));
    let writer_path = root.join(format!("{name}-writer"));
    assert!(!attempt.join("reserve").exists());
    assert!(
        !state_dir
            .join(fixture::S2_EXIT_PUBLICATION_ENOSPC_ONCE)
            .exists()
    );
    let visible: serde_json::Value = serde_json::from_slice(&before)?;
    assert_eq!(visible["delivered_signals"], serde_json::json!([]));
    assert_eq!(visible["stop_reason"], serde_json::Value::Null);
    assert!(lock_is_held(&writer_path, OsStr::new("writer.lock"))?);
    assert!(lock_is_held(&attempt, OsStr::new("custody.lock"))?);

    std::fs::remove_file(state_dir.join(fixture::S2_EXIT_PUBLICATION_SYNC_BLOCKED))?;
    wait_for_file(&state_dir.join(fixture::S2_EXIT_PUBLICATION_DURABLE))?;
    let after = std::fs::read(attempt.join("process-exit.json"))?;
    assert_eq!(
        after, before,
        "retry must retain the exact immutable exit bytes"
    );

    let clock = SystemClock::new();
    let exit = match observe(&handle, budget(&clock)?, &CancelToken::new(), &clock)? {
        Observation::Exited(exit) => exit,
        other => return Err(io::Error::other(format!("expected exit, got {other:?}")).into()),
    };
    assert_eq!(
        exit.termination,
        Some(provenant_supervisor::exec::Termination::Exit(0))
    );
    assert_eq!(
        exit.stop_reason, None,
        "publication faults must not signal the provider"
    );
    assert_eq!(
        exit.publication,
        PublicationState::Durable,
        "the retried directory sync must make the visible record durable"
    );
    let clock = SystemClock::new();
    let _settled = Directory::open_anchor(&writer_path)?.lock(
        OsStr::new("writer.lock"),
        provenant_supervisor::fs::LockMode::Exclusive,
        budget(&clock)?,
        &CancelToken::new(),
        &clock,
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "internal")
        && args.get(1).is_some_and(|arg| arg == "supervise")
    {
        let publisher = match std::env::var_os("PROVENANT_STATE_DIR") {
            Some(path) => fixture::ExitPublicationFaults::from_state_dir(&PathBuf::from(path))?,
            None => None,
        };
        provenant_supervisor::supervise(
            &args[2..],
            GuardianHooks {
                observer: Some(&fixture::RecordedBootObserver),
                exit_publisher: publisher.as_ref().map(|item| item as &dyn ExitPublisher),
                ..Default::default()
            },
        )?;
        return Ok(());
    }
    if fixture::dispatch()? {
        return Ok(());
    }
    let root = tempfile::Builder::new()
        .prefix("s2-publication-")
        .tempdir_in("/tmp")?;
    exit_publication_enospc_and_visible_sync_retry_keeps_one_snapshot(root.path())?;
    Ok(())
}
