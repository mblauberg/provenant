use super::*;
use crate::census::{Census, ProcessFact};
use std::collections::BTreeSet;

#[test]
fn census_fallback_is_recorded_once_even_if_the_source_keeps_failing() {
    let mut recorded = false;
    assert_eq!(super::census_fallback_notice(false, &mut recorded), None);
    assert!(super::census_fallback_notice(true, &mut recorded).is_some());
    assert_eq!(super::census_fallback_notice(true, &mut recorded), None);
}

#[test]
fn event_bursts_coalesce_census_walks_and_failures_request_a_full_scan() {
    let mut last = None;
    let mut walks = 0;
    for now in 0..1_000 {
        if super::census_walk_due(now, last) {
            last = Some(now);
            walks += 1;
        }
    }
    assert!(walks > 0, "an event burst must eventually be observed");
    assert!(walks < 1_000, "a census walk followed every event");
    assert!(super::census_full_due(0, Some(u64::MAX), true));
    assert!(!super::census_full_due(0, Some(u64::MAX), false));
    assert!(super::census_full_due(u64::MAX, Some(0), false));
}

/// An idle lane gets a census eventually, with no repeated walk before its
/// full-scan backstop is due. Event-driven work can still request a walk.
#[test]
fn idle_lane_census_walks_respect_the_full_backstop() {
    let mut last_walk = None;
    let mut last_full = None;
    let mut walks = Vec::new();
    for now in (0..60_000).step_by(100) {
        if super::census_turn_due(now, last_walk, last_full, false) {
            last_walk = Some(now);
            if super::census_full_due(now, last_full, false) {
                last_full = Some(now);
            }
            walks.push(now);
        }
    }
    assert!(!walks.is_empty(), "an idle lane still needs a full census");
    assert!(
        walks
            .windows(2)
            .all(|pair| pair[1] - pair[0] >= super::FULL_CENSUS_INTERVAL_MS)
    );
    // Events can request a walk as soon as the walk throttle permits it.
    assert!(super::census_turn_due(
        super::CENSUS_INTERVAL_MS,
        Some(0),
        Some(u64::MAX),
        true
    ));
    assert!(!super::census_turn_due(
        super::CENSUS_INTERVAL_MS.saturating_sub(1),
        Some(0),
        Some(u64::MAX),
        true
    ));
    assert!(!super::census_turn_due(0, Some(0), Some(0), false));
    assert!(super::census_turn_due(0, None, None, false));
}

#[test]
fn a_failed_journal_retry_can_proceed_without_a_census() {
    assert!(super::tracking_publication_due(u64::MAX, None, true));
    assert!(!super::tracking_publication_due(0, Some(u64::MAX), true));
    assert!(!super::census_turn_due(
        u64::MAX,
        Some(u64::MAX),
        Some(u64::MAX),
        false
    ));
}

#[test]
fn census_watches_only_possible_live_identities_without_discarding_custody_history() {
    let live = identity(10, 100, 10, 10);
    let dead = identity(20, 200, 10, 10);
    let unknown = identity(30, 300, 30, 30);
    let reused = identity(40, 400, 40, 40);
    let absent = identity(50, 500, 50, 50);
    let mut zombie = fact(dead.clone(), 10);
    zombie.zombie = true;
    let snapshot = Census::from_observations(
        [
            fact(live.clone(), 1),
            zombie,
            fact(identity(40, 401, 40, 40), 1),
        ],
        [(30, crate::census::ObservationIssue::Io)],
        [50],
        true,
        false,
        100,
    )
    .unwrap();
    let history = BTreeSet::from([live, dead, unknown, reused, absent]);
    assert_eq!(
        super::census_watch_pids(&snapshot, history.iter()),
        BTreeSet::from([10, 30])
    );
    // Watch selection borrows the evidence. Zombie/reused/unknown custody is
    // still available to the existing conservative cleanup policy.
    assert_eq!(history.len(), 5);
    assert!(
        matches!(snapshot.process(20), crate::census::ProcessLookup::Present(row) if row.zombie)
    );
}

#[test]
fn cancellation_directory_watch_allows_idle_backoff_with_polling_fallback() {
    assert_eq!(super::cancel_poll_interval(true), 1_000);
    assert_eq!(super::cancel_poll_interval(false), 100);
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[test]
#[allow(clippy::disallowed_methods)]
fn guardian_missing_attempt_finishes_unresolved_group_cleanup() {
    use crate::census::{CensusError, ProcessObservation, ProcessObserver};
    use provenant_core::{AttemptId, WaitBudget};
    use provenant_os::clock::SystemClock;
    use std::os::fd::{AsFd, AsRawFd};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    // Keep the group alive until the first publication attempt. This forces
    // the unresolved path, rather than testing only an already empty group.
    struct GatedObserver(Arc<AtomicBool>);
    impl ProcessObserver for GatedObserver {
        fn boot_identity(&self) -> std::result::Result<String, CensusError> {
            crate::fixture::RecordedBootObserver.boot_identity()
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            crate::fixture::RecordedBootObserver.observe(pid)
        }
        fn census(&self, limit: usize) -> std::result::Result<Census, CensusError> {
            if !self.0.load(Ordering::SeqCst) {
                return Err(CensusError::InvalidRecord);
            }
            crate::fixture::RecordedBootObserver.census(limit)
        }
    }
    struct Missing(Arc<AtomicBool>, PathBuf);
    impl ExitPublisher for Missing {
        fn publish(&self, directory: &Directory, bytes: &[u8]) -> Result<Publication> {
            let exit: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(
                exit["unresolved_cleanup"], true,
                "initial cleanup must be unresolved"
            );
            let pid: i64 = std::fs::read_to_string(&self.1)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(
                matches!(crate::fixture::RecordedBootObserver.observe(pid), ProcessObservation::Present(fact) if !fact.zombie),
                "group member must still be live at publication"
            );
            self.0.store(true, Ordering::SeqCst);
            // The fixture models removal, not an error against a live copy.
            let path = directory.current_path()?;
            std::fs::remove_dir_all(&path).map_err(|source| provenant_os::fs::FsError::Io {
                operation: "remove fixture attempt directory",
                path: path.clone(),
                source,
            })?;
            Err(provenant_os::fs::FsError::Io {
                operation: "publish fixture exit",
                path: "process-exit.json".into(),
                source: io::Error::from_raw_os_error(libc::ENOENT),
            }
            .into())
        }
    }
    const CHILD_ARGS: &str = "PROVENANT_TEST_SUPERVISE_FDS";
    const CHILD_PID: &str = "PROVENANT_TEST_PROVIDER_PID_PATH";
    // The guarded subprocess runs only this test in a fresh session. Its
    // inherited descriptor marker selects supervise instead of spawning again.
    if let Some(args) = std::env::var_os(CHILD_ARGS) {
        let args: Vec<OsString> = args
            .to_str()
            .unwrap()
            .split(',')
            .map(OsString::from)
            .collect();
        let gate = Arc::new(AtomicBool::new(false));
        let observer = GatedObserver(gate.clone());
        let publisher = Missing(gate, std::env::var_os(CHILD_PID).unwrap().into());
        supervise(
            &args,
            GuardianHooks {
                observer: Some(&observer),
                exit_publisher: Some(&publisher),
                ..Default::default()
            },
        )
        .unwrap();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let writer_path = root.path().join("writer");
    let attempt_path = root.path().join("attempt");
    std::fs::create_dir(&writer_path).unwrap();
    std::fs::create_dir(&attempt_path).unwrap();
    let child_path = root.path().join("child.pid");
    let clock = SystemClock::new();
    let scope = Scope {
        attempt: AttemptId::from_bytes([32; 16]),
        generation: 1,
    };
    let budget = WaitBudget::nonblocking(clock.mono_now());
    let cancel = CancelToken::new();
    let writer = crate::fixture::acquire_writer(
        Directory::open_anchor(&writer_path).unwrap(),
        scope,
        budget,
        &cancel,
        &clock,
    )
    .unwrap();
    let publication = PublicationReservation::prepare(
        Directory::open_anchor(&attempt_path).unwrap(),
        budget,
        &cancel,
        &clock,
    )
    .unwrap();
    let capture = CapturePlan {
        root_flush_ms: 50,
        descendant_grace_ms: 100,
        wall_ms: 5_000,
        ..CapturePlan::default()
    };
    let message = LaunchMessage {
        profile: None,
        schema_version: 1,
        priority: 0,
        scope,
        resolved_route: "fixture/provider".into(),
        permission_plan_digest: provenant_core::Digest::from_bytes([0; 32]),
        key: crate::custody::fixture_key(33),
        requester: provenant_core::ActorId::from_bytes([34; 16]),
        binding: writer.reservation.binding.clone(),
        executable_snapshot: snapshot_executable(OsStr::new("/bin/sh")).unwrap(),
        protected_commands: Vec::new(),
        argv: vec![
            b"/bin/sh".to_vec(),
            b"-c".to_vec(),
            b"mkfifo \"$2\" || exit 1; /bin/cat \"$2\" >/dev/null 2>&1 & echo $! > \"$1\"".to_vec(),
            b"fixture".to_vec(),
            child_path.as_os_str().as_bytes().to_vec(),
            root.path()
                .join("child.fifo")
                .as_os_str()
                .as_bytes()
                .to_vec(),
        ],
        environment: Vec::new(),
        lane_ports: None,
        cwd: root.path().as_os_str().as_bytes().to_vec(),
        stdin: Vec::new(),
        secret_input: None,
        holder_context: Vec::new(),
        capture,
    };
    let (mut parent, child) = std::os::unix::net::UnixStream::pair().unwrap();
    let descriptors = [
        child.as_fd(),
        writer.lock_directory.handoff_file().as_fd(),
        publication.directory.handoff_file().as_fd(),
        writer.lease.as_ref().unwrap().handoff_file().as_fd(),
        publication.custody.handoff_file().as_fd(),
    ];
    let args = descriptors
        .iter()
        .map(|fd| fd.as_raw_fd().to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut environment: Vec<_> = std::env::vars_os().collect();
    environment.push((CHILD_ARGS.into(), args.into()));
    environment.push((CHILD_PID.into(), child_path.as_os_str().to_owned()));
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "guardian::tests::guardian_missing_attempt_finishes_unresolved_group_cleanup",
        "--nocapture",
    ]);
    command.envs(environment.iter().cloned());
    provenant_os::sys::child::configure_command(
        &mut command,
        &descriptors,
        Some(&environment),
        true,
    )
    .unwrap();
    let mut guardian = crate::fixture::ChildGuard::new(command.spawn().unwrap());
    drop(child);
    crate::process_os::handoff::send(
        &mut parent,
        &serde_json::to_vec(&message).unwrap(),
        Duration::from_secs(5),
        &cancel,
    )
    .unwrap();
    assert_eq!(
        crate::process_os::handoff::receive(&mut parent, Duration::from_secs(5)).unwrap(),
        b"custody-accepted-v1"
    );
    drop(writer);
    drop(publication);
    let status = provenant_test_support::wait_for(
        "guardian exit after ENOENT",
        Duration::from_secs(10),
        || guardian.try_wait(),
        |result| !matches!(result, Ok(None)),
    )
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(status.success(), "supervise fixture failed: {status}");
    assert!(!attempt_path.join("process-exit.json").exists());
    let child_pid: i64 = std::fs::read_to_string(&child_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        matches!(
            crate::fixture::RecordedBootObserver.observe(child_pid),
            ProcessObservation::Absent
        ),
        "group child survived cleanup"
    );
    let evidence =
        crate::custody::attempt_evidence(&Directory::open_anchor(&writer_path).unwrap(), scope)
            .unwrap();
    let reservation: Reservation =
        read_record(&evidence, "reservation.json", RECORD_LIMIT).unwrap();
    let leader = reservation.provider.unwrap();
    assert!(
        matches!(
            crate::fixture::RecordedBootObserver.observe(leader.pid),
            ProcessObservation::Absent
        ),
        "provider leader was not reaped"
    );
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
struct RemoveAttemptOnSync;

#[cfg(all(feature = "fixture", target_os = "macos"))]
impl provenant_os::fs::AtomicWriteOperations for RemoveAttemptOnSync {
    fn write(
        &mut self,
        file: &mut provenant_os::fs::FileHandle,
        bytes: &[u8],
        path: &Path,
    ) -> provenant_os::fs::Result<()> {
        file.write_all(bytes)
            .map_err(|source| provenant_os::fs::FsError::Io {
                operation: "write fixture staging file",
                path: path.into(),
                source,
            })
    }
    fn sync_file(&mut self, file: &provenant_os::fs::FileHandle) -> provenant_os::fs::Result<()> {
        file.sync()
    }
    fn rename(
        &mut self,
        directory: &Directory,
        temporary: &OsStr,
        name: &OsStr,
        mode: PublishMode,
    ) -> provenant_os::fs::Result<()> {
        match mode {
            PublishMode::NoReplace => directory.rename_noreplace(temporary, directory, name),
            PublishMode::Replace => directory.replace(temporary, directory, name),
        }
    }
    fn sync_directory(&mut self, directory: &Directory) -> provenant_os::fs::Result<()> {
        // Remove the attempt between the rename and its directory sync, then
        // report a durability failure that is not ENOENT: only re-checking the
        // directory's existence can stop publication here.
        let path = directory.current_path()?;
        std::fs::remove_dir_all(&path).map_err(|source| provenant_os::fs::FsError::Io {
            operation: "remove fixture attempt directory",
            path: path.clone(),
            source,
        })?;
        Err(provenant_os::fs::FsError::Io {
            operation: "fixture post-link directory sync",
            path: PathBuf::from("process-exit.json"),
            source: io::Error::from_raw_os_error(libc::EIO),
        })
    }
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
struct UnconfirmedAfterAttemptRemoval;

#[cfg(all(feature = "fixture", target_os = "macos"))]
impl ExitPublisher for UnconfirmedAfterAttemptRemoval {
    fn publish(&self, directory: &Directory, bytes: &[u8]) -> Result<Publication> {
        let exit: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            exit["unresolved_cleanup"], false,
            "publication requires settled cleanup"
        );
        Ok(directory.atomic_write_with_faults(
            OsStr::new("process-exit.json"),
            ".exit-",
            bytes,
            PublishMode::NoReplace,
            &mut RemoveAttemptOnSync,
        )?)
    }
    fn sync_visible(&self, _: &Directory) -> Result<()> {
        // The directory is gone, so a real sync cannot confirm durability.
        // Keeping this failing proves publication stops on existence rather
        // than eventually succeeding against a removed directory.
        Err(provenant_os::fs::FsError::Io {
            operation: "fixture visible exit sync",
            path: PathBuf::from("process-exit.json"),
            source: io::Error::from_raw_os_error(libc::EIO),
        }
        .into())
    }
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
struct ReplaceAttemptPath {
    as_symlink: bool,
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
impl ExitPublisher for ReplaceAttemptPath {
    fn publish(&self, directory: &Directory, _: &[u8]) -> Result<Publication> {
        let path = directory.current_path()?;
        std::fs::remove_dir_all(&path).map_err(|source| provenant_os::fs::FsError::Io {
            operation: "remove fixture attempt directory",
            path: path.clone(),
            source,
        })?;
        if self.as_symlink {
            std::os::unix::fs::symlink("replacement-target", &path).map_err(|source| {
                provenant_os::fs::FsError::Io {
                    operation: "replace fixture attempt directory",
                    path: path.clone(),
                    source,
                }
            })?;
        } else {
            std::fs::create_dir(&path).map_err(|source| provenant_os::fs::FsError::Io {
                operation: "replace fixture attempt directory",
                path: path.clone(),
                source,
            })?;
        }
        Ok(Publication::VisibleDurabilityUnconfirmed {
            sync_error: provenant_os::fs::FsError::Io {
                operation: "fixture post-link directory sync",
                path: PathBuf::from("process-exit.json"),
                source: io::Error::from_raw_os_error(libc::EIO),
            },
        })
    }
    fn sync_visible(&self, _: &Directory) -> Result<()> {
        Err(provenant_os::fs::FsError::Io {
            operation: "fixture visible exit sync",
            path: PathBuf::from("process-exit.json"),
            source: io::Error::from_raw_os_error(libc::EIO),
        }
        .into())
    }
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[derive(Clone, Copy)]
enum RenameTarget {
    Attempt,
    Ancestor,
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
struct RenameBeforePublish(RenameTarget);

#[cfg(all(feature = "fixture", target_os = "macos"))]
impl ExitPublisher for RenameBeforePublish {
    fn publish(&self, directory: &Directory, bytes: &[u8]) -> Result<Publication> {
        let path = directory.current_path()?;
        let (source, destination) = match self.0 {
            RenameTarget::Attempt => (path.clone(), path.with_file_name("attempt-renamed")),
            RenameTarget::Ancestor => {
                let ancestor = path
                    .parent()
                    .ok_or(CustodyError::InvalidRecord)?
                    .to_path_buf();
                let destination = ancestor.with_file_name("outer-renamed");
                (ancestor, destination)
            }
        };
        std::fs::rename(&source, &destination).map_err(|error| provenant_os::fs::FsError::Io {
            operation: "rename fixture attempt directory",
            path: source.clone(),
            source: error,
        })?;
        Ok(directory.atomic_write(
            OsStr::new("process-exit.json"),
            ".exit-",
            bytes,
            PublishMode::NoReplace,
        )?)
    }
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
struct UnconfirmedThenDurable(std::cell::Cell<bool>);

#[cfg(all(feature = "fixture", target_os = "macos"))]
impl ExitPublisher for UnconfirmedThenDurable {
    fn publish(&self, directory: &Directory, bytes: &[u8]) -> Result<Publication> {
        assert!(!self.0.replace(true), "publish must run once");
        directory.atomic_write(
            OsStr::new("process-exit.json"),
            ".exit-",
            bytes,
            PublishMode::NoReplace,
        )?;
        // The directory is present; only this first sync reports ENOENT.
        Ok(Publication::VisibleDurabilityUnconfirmed {
            sync_error: provenant_os::fs::FsError::Io {
                operation: "fixture post-link directory sync",
                path: PathBuf::from("process-exit.json"),
                source: io::Error::from_raw_os_error(libc::ENOENT),
            },
        })
    }
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
struct MissingAfterAttemptRemoval;

#[cfg(all(feature = "fixture", target_os = "macos"))]
impl ExitPublisher for MissingAfterAttemptRemoval {
    fn publish(&self, directory: &Directory, _: &[u8]) -> Result<Publication> {
        let path = directory.current_path()?;
        std::fs::remove_dir_all(&path).map_err(|source| provenant_os::fs::FsError::Io {
            operation: "remove fixture attempt directory",
            path: path.clone(),
            source,
        })?;
        Err(provenant_os::fs::FsError::Io {
            operation: "fixture exit publication",
            path: PathBuf::from("process-exit.json"),
            source: io::Error::from_raw_os_error(libc::ENOENT),
        }
        .into())
    }
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
struct TerminalMustNotRun {
    refuse_admission: bool,
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
impl Holder for TerminalMustNotRun {
    fn admit(&self, _: &[u8], _: &Directory) -> Result<()> {
        if self.refuse_admission {
            Err(CustodyError::Admission(AdmissionError::BuildSettings))
        } else {
            Ok(())
        }
    }
    fn terminal(&self, _: &[u8], _: &Directory, _: &ExitObservation, _: &Recovery) -> Result<()> {
        panic!("terminal settlement ran after the attempt directory was removed")
    }
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
struct SettlingHolder;

#[cfg(all(feature = "fixture", target_os = "macos"))]
impl Holder for SettlingHolder {
    fn admit(&self, _: &[u8], _: &Directory) -> Result<()> {
        Ok(())
    }
    fn terminal(
        &self,
        _: &[u8],
        attempt: &Directory,
        exit: &ExitObservation,
        _: &Recovery,
    ) -> Result<()> {
        match attempt.mkdir(OsStr::new("quarantine"), 0o700) {
            Ok(()) => attempt.sync()?,
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        let quarantine = attempt.open_verified_directory(OsStr::new("quarantine"))?;
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "scope": exit.scope,
            "kind": "quarantine_manifest",
        }))
        .map_err(|_| CustodyError::InvalidRecord)?;
        quarantine
            .atomic_write(
                OsStr::new("manifest.json"),
                ".test-",
                &manifest,
                PublishMode::NoReplace,
            )?
            .require_durable()?;
        publish_release(attempt, exit.scope)
    }
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
fn child_descriptors(name: &str) -> Option<Vec<OsString>> {
    let args = std::env::var_os(name)?;
    Some(
        args.to_str()
            .unwrap()
            .split(',')
            .map(OsString::from)
            .collect(),
    )
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[allow(clippy::disallowed_methods)]
fn spawn_missing_attempt_guardian(
    test_name: &str,
    descriptor_env: &str,
    argv: Vec<Vec<u8>>,
) -> (
    tempfile::TempDir,
    crate::fixture::ChildGuard,
    PathBuf,
    PathBuf,
    Scope,
) {
    use provenant_core::{AttemptId, WaitBudget};
    use provenant_os::clock::SystemClock;
    use std::os::fd::{AsFd, AsRawFd};

    let root = tempfile::tempdir().unwrap();
    let writer_path = root.path().join("writer");
    let outer = root.path().join("outer");
    let attempt_path = outer.join("attempt");
    std::fs::create_dir(&writer_path).unwrap();
    std::fs::create_dir(&outer).unwrap();
    std::fs::create_dir(&attempt_path).unwrap();
    let clock = SystemClock::new();
    let scope = Scope {
        attempt: AttemptId::from_bytes([42; 16]),
        generation: 1,
    };
    let budget = WaitBudget::nonblocking(clock.mono_now());
    let cancel = CancelToken::new();
    let writer = crate::fixture::acquire_writer(
        Directory::open_anchor(&writer_path).unwrap(),
        scope,
        budget,
        &cancel,
        &clock,
    )
    .unwrap();
    let publication = PublicationReservation::prepare(
        Directory::open_anchor(&attempt_path).unwrap(),
        budget,
        &cancel,
        &clock,
    )
    .unwrap();
    let executable_snapshot = snapshot_executable(OsStr::from_bytes(&argv[0])).unwrap();
    let message = LaunchMessage {
        profile: None,
        schema_version: 1,
        priority: 0,
        scope,
        resolved_route: "fixture/provider".into(),
        permission_plan_digest: provenant_core::Digest::from_bytes([0; 32]),
        key: crate::custody::fixture_key(43),
        requester: provenant_core::ActorId::from_bytes([44; 16]),
        binding: writer.reservation.binding.clone(),
        executable_snapshot,
        protected_commands: Vec::new(),
        argv,
        environment: Vec::new(),
        lane_ports: None,
        cwd: root.path().as_os_str().as_bytes().to_vec(),
        stdin: Vec::new(),
        secret_input: None,
        holder_context: Vec::new(),
        capture: CapturePlan {
            root_flush_ms: 50,
            descendant_grace_ms: 100,
            wall_ms: 5_000,
            ..CapturePlan::default()
        },
    };
    let (mut parent, child) = std::os::unix::net::UnixStream::pair().unwrap();
    let descriptors = [
        child.as_fd(),
        writer.lock_directory.handoff_file().as_fd(),
        publication.directory.handoff_file().as_fd(),
        writer.lease.as_ref().unwrap().handoff_file().as_fd(),
        publication.custody.handoff_file().as_fd(),
    ];
    let args = descriptors
        .iter()
        .map(|fd| fd.as_raw_fd().to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut environment: Vec<_> = std::env::vars_os().collect();
    environment.push((descriptor_env.into(), args.into()));
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", test_name, "--nocapture"]);
    command.envs(environment.iter().cloned());
    provenant_os::sys::child::configure_command(
        &mut command,
        &descriptors,
        Some(&environment),
        true,
    )
    .unwrap();
    let guardian = crate::fixture::ChildGuard::new(command.spawn().unwrap());
    drop(child);
    crate::process_os::handoff::send(
        &mut parent,
        &serde_json::to_vec(&message).unwrap(),
        Duration::from_secs(5),
        &cancel,
    )
    .unwrap();
    assert_eq!(
        crate::process_os::handoff::receive(&mut parent, Duration::from_secs(5)).unwrap(),
        b"custody-accepted-v1"
    );
    drop(writer);
    drop(publication);
    (root, guardian, writer_path, attempt_path, scope)
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[allow(clippy::disallowed_methods)]
fn wait_for_guardian(guardian: &mut crate::fixture::ChildGuard) -> std::process::ExitStatus {
    provenant_test_support::wait_for(
        "guardian exit after removal",
        Duration::from_secs(10),
        || guardian.try_wait(),
        |result| !matches!(result, Ok(None)),
    )
    .unwrap()
    .unwrap()
    .unwrap()
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
struct RemoveWriterEvidence;

#[cfg(all(feature = "fixture", target_os = "macos"))]
impl ExitPublisher for RemoveWriterEvidence {
    fn commit_reservation(&self, directory: &Directory, _: &[u8]) -> Result<Publication> {
        let path = directory.current_path()?;
        std::fs::remove_dir_all(&path).map_err(|source| provenant_os::fs::FsError::Io {
            operation: "remove fixture writer evidence",
            path: path.clone(),
            source,
        })?;
        Err(CustodyError::Io(io::Error::from(io::ErrorKind::NotFound)))
    }

    fn publish(&self, _: &Directory, _: &[u8]) -> Result<Publication> {
        Err(CustodyError::InvalidRecord)
    }
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[test]
fn guardian_retires_after_unlinked_writer_evidence_and_quiescence() {
    const CHILD_ARGS: &str = "PROVENANT_TEST_GUARDIAN_UNLINKED_WRITER_FDS";
    if let Some(args) = child_descriptors(CHILD_ARGS) {
        let observer = crate::fixture::RecordedBootObserver;
        let error = supervise(
            &args,
            GuardianHooks {
                observer: Some(&observer),
                exit_publisher: Some(&RemoveWriterEvidence),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(error, CustodyError::Io(error) if error.kind() == io::ErrorKind::NotFound)
        );
        return;
    }
    let (_root, mut guardian, writer_path, attempt_path, scope) = spawn_missing_attempt_guardian(
        "guardian::tests::guardian_retires_after_unlinked_writer_evidence_and_quiescence",
        CHILD_ARGS,
        vec![b"/usr/bin/true".to_vec()],
    );
    assert!(wait_for_guardian(&mut guardian).success());
    assert!(!writer_path.join(scope.attempt.to_string()).exists());
    assert!(!attempt_path.join("process-exit.json").exists());
    let writer_dir = Directory::open_anchor(&writer_path).unwrap();
    let _lease = writer_dir
        .try_lock(
            OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )
        .unwrap();
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[test]
#[allow(clippy::disallowed_methods)]
fn guardian_missing_attempt_after_rename_stops_publication_and_settlement() {
    const CHILD_ARGS: &str = "PROVENANT_TEST_GUARDIAN_UNCONFIRMED_FDS";
    if let Some(args) = child_descriptors(CHILD_ARGS) {
        let observer = crate::fixture::RecordedBootObserver;
        let publisher = UnconfirmedAfterAttemptRemoval;
        let holder = TerminalMustNotRun {
            refuse_admission: false,
        };
        supervise(
            &args,
            GuardianHooks {
                observer: Some(&observer),
                exit_publisher: Some(&publisher),
                holder: Some(&holder),
                ..Default::default()
            },
        )
        .unwrap();
        return;
    }
    let (_root, mut guardian, _, attempt_path, _) = spawn_missing_attempt_guardian(
        "guardian::tests::guardian_missing_attempt_after_rename_stops_publication_and_settlement",
        CHILD_ARGS,
        vec![b"/usr/bin/true".to_vec()],
    );
    let status = wait_for_guardian(&mut guardian);
    assert!(status.success(), "supervise fixture failed: {status}");
    assert!(
        !attempt_path.exists(),
        "the fixture publisher must remove the attempt directory"
    );
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[test]
#[allow(clippy::disallowed_methods)]
fn guardian_never_spawned_missing_attempt_skips_release() {
    const CHILD_ARGS: &str = "PROVENANT_TEST_GUARDIAN_NEVER_SPAWNED_FDS";
    if let Some(args) = child_descriptors(CHILD_ARGS) {
        let observer = crate::fixture::RecordedBootObserver;
        let publisher = MissingAfterAttemptRemoval;
        let holder = TerminalMustNotRun {
            refuse_admission: true,
        };
        supervise(
            &args,
            GuardianHooks {
                observer: Some(&observer),
                exit_publisher: Some(&publisher),
                holder: Some(&holder),
                ..Default::default()
            },
        )
        .unwrap();
        return;
    }
    let (_root, mut guardian, writer_path, attempt_path, scope) = spawn_missing_attempt_guardian(
        "guardian::tests::guardian_never_spawned_missing_attempt_skips_release",
        CHILD_ARGS,
        vec![b"/usr/bin/true".to_vec()],
    );
    let status = wait_for_guardian(&mut guardian);
    assert!(status.success(), "supervise fixture failed: {status}");
    assert!(
        !attempt_path.exists(),
        "the fixture publisher must remove the attempt directory"
    );
    let evidence =
        crate::custody::attempt_evidence(&Directory::open_anchor(&writer_path).unwrap(), scope)
            .unwrap();
    let reservation: Reservation =
        read_record(&evidence, "reservation.json", RECORD_LIMIT).unwrap();
    assert_eq!(reservation.launch_state, LaunchState::NeverSpawned);
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[test]
#[allow(clippy::disallowed_methods)]
fn guardian_replaced_attempt_directory_is_treated_as_removed() {
    const CHILD_ARGS: &str = "PROVENANT_TEST_GUARDIAN_REPLACED_DIR_FDS";
    if let Some(args) = child_descriptors(CHILD_ARGS) {
        let observer = crate::fixture::RecordedBootObserver;
        let publisher = ReplaceAttemptPath { as_symlink: false };
        let holder = TerminalMustNotRun {
            refuse_admission: false,
        };
        supervise(
            &args,
            GuardianHooks {
                observer: Some(&observer),
                exit_publisher: Some(&publisher),
                holder: Some(&holder),
                ..Default::default()
            },
        )
        .unwrap();
        return;
    }
    let (_root, mut guardian, _, attempt_path, _) = spawn_missing_attempt_guardian(
        "guardian::tests::guardian_replaced_attempt_directory_is_treated_as_removed",
        CHILD_ARGS,
        vec![b"/usr/bin/true".to_vec()],
    );
    let status = wait_for_guardian(&mut guardian);
    assert!(status.success(), "supervise fixture failed: {status}");
    assert!(
        attempt_path.is_dir(),
        "the fixture must recreate the pathname as a directory"
    );
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[test]
#[allow(clippy::disallowed_methods)]
fn guardian_replaced_attempt_symlink_is_treated_as_removed() {
    const CHILD_ARGS: &str = "PROVENANT_TEST_GUARDIAN_REPLACED_LINK_FDS";
    if let Some(args) = child_descriptors(CHILD_ARGS) {
        let observer = crate::fixture::RecordedBootObserver;
        let publisher = ReplaceAttemptPath { as_symlink: true };
        let holder = TerminalMustNotRun {
            refuse_admission: false,
        };
        supervise(
            &args,
            GuardianHooks {
                observer: Some(&observer),
                exit_publisher: Some(&publisher),
                holder: Some(&holder),
                ..Default::default()
            },
        )
        .unwrap();
        return;
    }
    let (_root, mut guardian, _, attempt_path, _) = spawn_missing_attempt_guardian(
        "guardian::tests::guardian_replaced_attempt_symlink_is_treated_as_removed",
        CHILD_ARGS,
        vec![b"/usr/bin/true".to_vec()],
    );
    let status = wait_for_guardian(&mut guardian);
    assert!(status.success(), "supervise fixture failed: {status}");
    assert!(
        std::fs::symlink_metadata(&attempt_path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the fixture must recreate the pathname as a symlink"
    );
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[test]
#[allow(clippy::disallowed_methods)]
fn guardian_renamed_attempt_still_settles() {
    const CHILD_ARGS: &str = "PROVENANT_TEST_GUARDIAN_RENAMED_ATTEMPT_FDS";
    if let Some(args) = child_descriptors(CHILD_ARGS) {
        let observer = crate::fixture::RecordedBootObserver;
        let publisher = RenameBeforePublish(RenameTarget::Attempt);
        let holder = SettlingHolder;
        supervise(
            &args,
            GuardianHooks {
                observer: Some(&observer),
                exit_publisher: Some(&publisher),
                holder: Some(&holder),
                ..Default::default()
            },
        )
        .unwrap();
        return;
    }
    let (_root, mut guardian, _, attempt_path, _) = spawn_missing_attempt_guardian(
        "guardian::tests::guardian_renamed_attempt_still_settles",
        CHILD_ARGS,
        vec![b"/usr/bin/true".to_vec()],
    );
    let status = wait_for_guardian(&mut guardian);
    assert!(status.success(), "supervise fixture failed: {status}");
    let renamed = attempt_path.with_file_name("attempt-renamed");
    assert!(!attempt_path.exists(), "the attempt pathname must be gone");
    assert!(
        renamed.join("release.json").exists(),
        "the renamed attempt must still receive terminal settlement"
    );
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[test]
#[allow(clippy::disallowed_methods)]
fn guardian_renamed_attempt_ancestor_still_settles() {
    const CHILD_ARGS: &str = "PROVENANT_TEST_GUARDIAN_RENAMED_ANCESTOR_FDS";
    if let Some(args) = child_descriptors(CHILD_ARGS) {
        let observer = crate::fixture::RecordedBootObserver;
        let publisher = RenameBeforePublish(RenameTarget::Ancestor);
        let holder = SettlingHolder;
        supervise(
            &args,
            GuardianHooks {
                observer: Some(&observer),
                exit_publisher: Some(&publisher),
                holder: Some(&holder),
                ..Default::default()
            },
        )
        .unwrap();
        return;
    }
    let (_root, mut guardian, _, attempt_path, _) = spawn_missing_attempt_guardian(
        "guardian::tests::guardian_renamed_attempt_ancestor_still_settles",
        CHILD_ARGS,
        vec![b"/usr/bin/true".to_vec()],
    );
    let status = wait_for_guardian(&mut guardian);
    assert!(status.success(), "supervise fixture failed: {status}");
    let renamed = attempt_path
        .parent()
        .unwrap()
        .with_file_name("outer-renamed")
        .join("attempt");
    assert!(
        !attempt_path.exists(),
        "the old attempt pathname must be gone"
    );
    assert!(
        renamed.join("release.json").exists(),
        "the attempt under the renamed ancestor must still receive settlement"
    );
}

#[cfg(all(feature = "fixture", target_os = "macos"))]
#[test]
#[allow(clippy::disallowed_methods)]
fn guardian_present_attempt_with_enoent_sync_still_settles() {
    const CHILD_ARGS: &str = "PROVENANT_TEST_GUARDIAN_PRESENT_ENOENT_FDS";
    if let Some(args) = child_descriptors(CHILD_ARGS) {
        let observer = crate::fixture::RecordedBootObserver;
        let publisher = UnconfirmedThenDurable(std::cell::Cell::new(false));
        let holder = SettlingHolder;
        supervise(
            &args,
            GuardianHooks {
                observer: Some(&observer),
                exit_publisher: Some(&publisher),
                holder: Some(&holder),
                ..Default::default()
            },
        )
        .unwrap();
        return;
    }
    let (_root, mut guardian, _, attempt_path, _) = spawn_missing_attempt_guardian(
        "guardian::tests::guardian_present_attempt_with_enoent_sync_still_settles",
        CHILD_ARGS,
        vec![b"/usr/bin/true".to_vec()],
    );
    let status = wait_for_guardian(&mut guardian);
    assert!(status.success(), "supervise fixture failed: {status}");
    assert!(
        attempt_path.join("process-exit.json").exists(),
        "the present attempt keeps its published exit"
    );
    assert!(
        attempt_path.join("release.json").exists(),
        "a sync ENOENT alone must not skip terminal settlement"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn descriptor_identity_distinguishes_removal_replacement_and_rename() {
    let root = tempfile::tempdir().unwrap();
    let outer = root.path().join("outer");
    let attempt = outer.join("attempt");
    std::fs::create_dir_all(&attempt).unwrap();
    let directory = Directory::open_anchor(&attempt).unwrap();
    let identity = DirectoryIdentity::of(&directory).unwrap();
    let path = directory.current_path().unwrap();
    assert_eq!(
        descriptor_link(&directory, identity),
        DescriptorLink::Linked
    );
    assert!(!attempt_removed(&directory, Some(&path), identity));

    let renamed = outer.join("renamed");
    std::fs::rename(&attempt, &renamed).unwrap();
    assert_eq!(
        descriptor_link(&directory, identity),
        DescriptorLink::Linked,
        "a rename keeps the held directory linked at its new path"
    );
    assert!(!attempt_removed(&directory, Some(&path), identity));

    std::fs::remove_dir(&renamed).unwrap();
    assert_eq!(
        descriptor_link(&directory, identity),
        DescriptorLink::Unlinked,
        "APFS keeps the link count, so the descriptor path proves removal"
    );
    assert!(attempt_removed(&directory, Some(&path), identity));

    // Recreating the old name does not resurrect the removed directory.
    std::fs::create_dir(&attempt).unwrap();
    assert_eq!(
        descriptor_link(&directory, identity),
        DescriptorLink::Unlinked
    );
    assert!(attempt_removed(&directory, Some(&path), identity));
}

#[test]
fn settlement_lock_clone_failure_retains_exclusion_and_returns_reconcile_hint() {
    use provenant_core::{AttemptId, Clock, WaitBudget};
    use provenant_os::{
        clock::SystemClock,
        fs::{FsError, LockMode},
    };
    let root = tempfile::tempdir().unwrap();
    let directory = Directory::open_anchor(root.path()).unwrap();
    let clock = SystemClock;
    let scope = Scope {
        attempt: AttemptId::from_bytes([2; 16]),
        generation: 1,
    };
    let mut writer = crate::fixture::acquire_writer(
        directory.try_clone().unwrap(),
        scope,
        WaitBudget::nonblocking(clock.mono_now()),
        &CancelToken::new(),
        &clock,
    )
    .unwrap();
    let before = writer
        .directory
        .read_bounded(OsStr::new("reservation.json"), RECORD_LIMIT)
        .unwrap();
    let error = settlement_recovery(
        &mut writer.lease,
        writer.directory.try_clone().unwrap(),
        Err(FsError::Io {
            operation: "clone directory",
            path: root.path().into(),
            source: io::Error::from_raw_os_error(libc::EMFILE),
        }),
        &writer.reservation,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("provenant reconcile")
    );
    assert!(writer.lease.is_some());
    assert!(matches!(
        directory.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive),
        Err(FsError::LockTimeout { .. })
    ));
    assert_eq!(
        writer
            .directory
            .read_bounded(OsStr::new("reservation.json"), RECORD_LIMIT)
            .unwrap(),
        before
    );
    writer.withdraw_unlaunched().unwrap();
}

#[test]
fn terminal_retries_wait_until_the_holder_can_publish() {
    use std::cell::{Cell, RefCell};
    let elapsed = Cell::new(Duration::ZERO);
    let attempts = RefCell::new(Vec::new());
    let delays = RefCell::new(Vec::new());
    assert!(retry_terminal_with(
        PRODUCTION_TERMINAL_RETRY_MS,
        || {
            attempts.borrow_mut().push(elapsed.get());
            elapsed.get() >= Duration::from_secs(35)
        },
        |delay| {
            delays.borrow_mut().push(delay);
            elapsed.set(elapsed.get() + delay);
        },
    ));
    assert_eq!(
        *attempts.borrow(),
        [
            Duration::ZERO,
            Duration::from_secs(5),
            Duration::from_secs(35),
        ]
    );
    assert_eq!(
        *delays.borrow(),
        [Duration::from_secs(5), Duration::from_secs(30),]
    );
}

#[test]
fn terminal_retry_exhaustion_keeps_the_failure_bounded() {
    use std::cell::{Cell, RefCell};
    let attempts = Cell::new(0);
    let delays = RefCell::new(Vec::new());
    assert!(!retry_terminal_with(
        PRODUCTION_TERMINAL_RETRY_MS,
        || {
            attempts.set(attempts.get() + 1);
            false
        },
        |delay| delays.borrow_mut().push(delay),
    ));
    assert_eq!(attempts.get(), 4);
    assert_eq!(
        *delays.borrow(),
        [
            Duration::from_secs(5),
            Duration::from_secs(30),
            Duration::from_secs(120),
        ]
    );
}

#[test]
fn successful_terminal_publication_does_not_wait() {
    assert!(retry_terminal(
        || true,
        |_| panic!("a successful terminal publication must not wait"),
    ));
}

#[test]
fn handoff_round_trips_one_mib_stdin_with_claude_argv_and_environment() {
    let message = LaunchMessage {
        profile: None,
        priority: 0,
        schema_version: 1,
        scope: Scope {
            attempt: provenant_core::AttemptId::from_bytes([2; 16]),
            generation: 1,
        },
        resolved_route: "claude/claude-sonnet-4-6@high".into(),
        permission_plan_digest: provenant_core::Digest::from_bytes([3; 32]),
        key: crate::custody::fixture_key(4),
        requester: provenant_core::ActorId::from_bytes([5; 16]),
        binding: StoredWriterBinding::ReadOnly,
        executable_snapshot: ExecutableSnapshot {
            device: 1,
            inode: 2,
            length: 1024,
            modified_seconds: 1,
            modified_nanoseconds: 0,
        },
        protected_commands: vec!["provenant".into(), "git".into()],
        argv: [
            "/fixture/claude/bin/claude",
            "-p",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--permission-prompts",
            "none",
            "--output-format",
            "stream-json",
            "--verbose",
            "--session-id",
            "01234567-89ab-4cde-8f01-23456789abcd",
            "--permission-mode",
            "dontAsk",
            "--tools",
            "Read,Grep,Glob",
            "--model",
            "claude-sonnet-4-6",
            "--effort",
            "high",
            "--mcp-config",
            "/fixture/attempt/provider-state/fabric-mcp.json",
        ]
        .map(|arg| arg.as_bytes().to_vec())
        .to_vec(),
        environment: [
            ("HOME", "/fixture/home"),
            ("USER", "fixture"),
            ("PATH", "/usr/bin:/bin"),
            ("TMPDIR", "/fixture/attempt/tmp/candidate-0"),
            ("LANG", "en_US.UTF-8"),
            ("CLAUDE_CODE_TMPDIR", "/fixture/attempt/tmp/candidate-0"),
            ("DISABLE_AUTOUPDATER", "1"),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ("CLAUDE_CONFIG_DIR", "/fixture/home/.claude"),
        ]
        .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
        .to_vec(),
        lane_ports: None,
        cwd: b"/fixture/repo".to_vec(),
        stdin: vec![0xff; 1024 * 1024],
        secret_input: Some(provenant_os::exec::SecretInput {
            descriptor_env: "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR".into(),
            bytes: b"fixture-token".to_vec(),
        }),
        holder_context: Vec::new(),
        capture: CapturePlan::default(),
    };
    let payload = serde_json::to_vec(&message).unwrap();
    let (mut sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    let timeout = Duration::from_secs(10);
    let sending = std::thread::spawn(move || {
        crate::process_os::handoff::send(&mut sender, &payload, timeout, &CancelToken::new())
    });
    let received = crate::process_os::handoff::receive(&mut receiver, timeout);
    sending.join().unwrap().unwrap();
    let received = received.unwrap();
    let decoded: LaunchMessage = decode_v1(received.as_slice(), LAUNCH_LIMITS).unwrap();
    assert_eq!(decoded.stdin, message.stdin);
    assert_eq!(decoded.argv, message.argv);
    assert_eq!(decoded.environment, message.environment);
}

fn identity(pid: i64, start: u64, group: i64, session: i64) -> ProcessIdentity {
    ProcessIdentity {
        boot: "darwin:test".into(),
        pid,
        native_start: start,
        group_id: group,
        session_id: session,
    }
}
fn fact(identity: ProcessIdentity, parent_pid: i64) -> ProcessFact {
    ProcessFact {
        identity,
        parent_pid,
        zombie: false,
        cpu_micros: None,
        command: None,
    }
}
fn census(facts: Vec<ProcessFact>, complete: bool) -> Census {
    let absent: Vec<_> = [20, 30]
        .into_iter()
        .filter(|pid| !facts.iter().any(|fact| fact.identity.pid == *pid))
        .collect();
    Census::from_observations(facts, [], absent, complete, false, 100).unwrap()
}
#[test]
fn unlinked_writer_retirement_requires_the_original_lock_to_be_free() {
    let root = tempfile::tempdir().unwrap();
    let directory = Directory::open_anchor(root.path()).unwrap();
    let lease = directory
        .try_lock(
            OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )
        .unwrap();
    let expected = lease.metadata().unwrap();
    let inherited = lease.handoff_file().try_clone().unwrap();
    let probe = directory.open_read(OsStr::new("writer.lock")).unwrap();
    let other = directory.open_read(OsStr::new("writer.lock")).unwrap();
    let mut writer = Some(lease);
    assert!(!try_unlinked_writer_lease(&probe, expected, &mut writer));
    assert!(writer.is_some(), "a linked entry must retain custody");
    std::fs::rename(root.path().join("writer.lock"), root.path().join("renamed")).unwrap();
    assert!(!try_unlinked_writer_lease(&probe, expected, &mut writer));
    assert!(writer.is_some(), "a renamed inode remains linked");
    std::fs::remove_file(root.path().join("renamed")).unwrap();
    let mut different = expected;
    different.inode = different.inode.wrapping_add(1);
    assert!(!try_unlinked_writer_lease(&probe, different, &mut writer));
    assert!(writer.is_some(), "a different inode cannot prove custody");
    assert!(!try_unlinked_writer_lease(&probe, expected, &mut writer));
    assert!(writer.is_none());
    drop(inherited);
    assert!(try_unlinked_writer_lease(&probe, expected, &mut writer));
    assert!(
        provenant_os::sys::fs::try_flock(
            other.handoff_file(),
            provenant_os::fs::LockMode::Exclusive
        )
        .is_err()
    );
    drop(probe);
    assert!(
        provenant_os::sys::fs::try_flock(
            other.handoff_file(),
            provenant_os::fs::LockMode::Exclusive
        )
        .is_ok()
    );
}

#[test]
fn fresh_writer_lease_refuses_a_replaced_or_unlinked_original() {
    for unlink in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(root.path()).unwrap();
        let lease = directory
            .try_lock(
                OsStr::new("writer.lock"),
                provenant_os::fs::LockMode::Exclusive,
            )
            .unwrap();
        let inherited = lease.handoff_file().try_clone().unwrap();
        let expected = provenant_os::fs::Metadata::from_std(inherited.metadata().unwrap());
        if unlink {
            std::fs::remove_file(root.path().join("writer.lock")).unwrap();
        } else {
            std::fs::rename(root.path().join("writer.lock"), root.path().join("held")).unwrap();
        }
        drop(lease);
        assert!(try_writer_lease(&directory, expected).is_err());
        drop(inherited);
    }
    let root = tempfile::tempdir().unwrap();
    let directory = Directory::open_anchor(root.path()).unwrap();
    let lease = directory
        .try_lock(
            OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )
        .unwrap();
    let expected = provenant_os::fs::Metadata::from_std(lease.handoff_file().metadata().unwrap());
    assert!(try_writer_lease(&directory, expected).is_err());
    drop(lease);
    assert!(try_writer_lease(&directory, expected).is_ok());
}

#[test]
fn darwin_settlement_requires_complete_absence_of_members_and_recorded_escapes() {
    let guardian = identity(10, 1, 10, 10);
    let provider = identity(20, 2, 20, 10);
    let escaped = identity(30, 3, 30, 30);
    let history = BTreeSet::from([escaped.clone()]);
    let clear = vec![fact(guardian.clone(), 1)];
    assert!(darwin_census_clear(
        &census(clear.clone(), true),
        &provider,
        &guardian,
        &history
    ));
    assert!(!darwin_census_clear(
        &census(clear.clone(), false),
        &provider,
        &guardian,
        &history
    ));
    for survivor in [
        provider.clone(),
        identity(21, 4, 20, 99),
        identity(22, 4, 99, 10),
        escaped.clone(),
    ] {
        let mut facts = clear.clone();
        facts.push(fact(survivor, 1));
        assert!(!darwin_census_clear(
            &census(facts, true),
            &provider,
            &guardian,
            &history
        ));
    }
    // Complete enumeration alone does not prove a known PID's absence.
    let unknown_history = BTreeSet::from([identity(40, 3, 40, 40)]);
    assert!(!darwin_census_clear(
        &census(clear.clone(), true),
        &provider,
        &guardian,
        &unknown_history
    ));
    // Reused PID with a different start stamp is not the recorded daemon.
    let mut facts = clear;
    facts.push(fact(identity(30, 40, 30, 30), 1));
    assert!(darwin_census_clear(
        &census(facts, true),
        &provider,
        &guardian,
        &history
    ));
}

#[test]
fn published_disk_admission_refusal_names_figures_and_repair() {
    let reason = super::admission_refusal(&super::CustodyError::Admission(
        super::AdmissionError::Disk {
            free: 99,
            floor: 100,
            origin: provenant_records::diagnostic::SettingSource::Configuration,
        },
    ));
    assert_eq!(
        reason,
        "insufficient disk space: 99 bytes available, gc.free_floor 100 bytes (source: host configuration); run `provenant gc --apply`"
    );
}

#[test]
fn admission_configuration_and_guardian_failures_are_named() {
    let configuration = super::admission_refusal(&super::CustodyError::Admission(
        super::AdmissionError::BuildSettings,
    ));
    assert!(configuration.contains("domains.fabric.lanes.max_concurrent_builds"));
    let guardian = super::admission_refusal(&super::CustodyError::Admission(
        super::AdmissionError::GuardianIdentity,
    ));
    assert!(guardian.contains("guardian process identity unavailable"));
}

#[test]
fn process_tree_path_replaces_foreign_environment_and_clears_it_when_unavailable() {
    let input = [
        ("PATH".into(), "/bin".into()),
        (
            crate::process_tree::ENV.into(),
            "/foreign/process-tree.json".into(),
        ),
        (
            crate::process_tree::ENV.into(),
            "/another/process-tree.json".into(),
        ),
    ];
    let path = Path::new("/own/attempt/process-tree.json");
    let env = super::with_process_tree_path(&input, Some(path));
    assert_eq!(
        env,
        [
            (OsString::from("PATH"), OsString::from("/bin")),
            (crate::process_tree::ENV.into(), path.as_os_str().to_owned())
        ]
    );
    assert_eq!(
        super::with_process_tree_path(&input, None),
        [(OsString::from("PATH"), OsString::from("/bin"))]
    );
}

#[cfg(feature = "fixture")]
#[test]
fn a_leased_port_block_replaces_any_block_the_launch_environment_named() {
    let temp = tempfile::tempdir().unwrap();
    let plan = provenant_os::lane_ports::Plan {
        state: temp.path().to_owned(),
        base: 30000,
        size: 16,
        blocks: 2,
        blocks_origin: None,
    };
    let held = plan.try_acquire().unwrap().unwrap();
    let lease = plan.try_acquire().unwrap().unwrap();
    assert_ne!(held.base, lease.base);
    let env: Vec<(OsString, OsString)> = vec![
        ("PATH".into(), "/bin".into()),
        ("PROVENANT_LANE_PORT_BASE".into(), "1".into()),
    ];
    let out = with_lane_ports(&env, &lease);
    assert_eq!(out.len(), 3);
    assert!(out.contains(&("PATH".into(), "/bin".into())));
    assert!(out.contains(&(
        "PROVENANT_LANE_PORT_BASE".into(),
        lease.base.to_string().into()
    )));
    assert!(out.contains(&("PROVENANT_LANE_PORT_COUNT".into(), "16".into())));
}

#[test]
fn live_usage_counts_a_call_once_and_a_revised_figure_replaces_it() {
    use provenant_os::capture::{DecodedEventSink, FrameEvent};
    let events = Events::default();
    let report = |key, tokens, context| {
        events
            .try_send(FrameEvent::Usage {
                key,
                tokens,
                context,
            })
            .unwrap()
    };
    report(1, 600, None);
    report(1, 600, None);
    assert_eq!(events.tokens(), 600);
    assert_eq!(events.context(), None);
    report(2, 300, Some(200_000));
    assert_eq!(events.tokens(), 900);
    assert_eq!(events.context(), Some(200_000));
    report(1, 700, None);
    assert_eq!(events.tokens(), 1000);
    // A call without a context figure keeps the latest reported one; a later
    // call after compaction replaces it with its smaller context.
    assert_eq!(events.context(), Some(200_000));
    report(3, 10, Some(40_000));
    assert_eq!(events.context(), Some(40_000));
}

#[test]
fn context_handoff_is_due_only_at_a_reported_context_past_a_set_threshold() {
    // Off at 0 whatever the context; a provider with no figure never stops.
    assert!(!context_handoff_due(0, Some(900_000)));
    assert!(!context_handoff_due(200_000, None));
    assert!(!context_handoff_due(200_000, Some(199_999)));
    assert!(context_handoff_due(200_000, Some(200_000)));
}

#[test]
fn unavailable_port_allocation_warns_and_does_not_claim_a_block()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("not-a-directory");
    std::fs::write(&file, b"fixture")?;
    let unavailable = provenant_os::lane_ports::Plan {
        state: file,
        base: 30000,
        size: 16,
        blocks: 1,
        blocks_origin: Some(provenant_os::lane_ports::BlocksOrigin::HostConfig),
    };
    let (lease, warning) =
        lane_port_allocation(Some(&unavailable)).map_err(std::io::Error::other)?;
    assert!(lease.is_none());
    assert!(warning.is_some_and(|text| text.contains("domains.fabric.lanes.lane_port_blocks")));
    let env = vec![
        ("PATH".into(), "/bin".into()),
        (provenant_os::lane_ports::BASE_ENV.into(), "1".into()),
        (provenant_os::lane_ports::COUNT_ENV.into(), "2".into()),
    ];
    assert_eq!(
        without_lane_ports(&env),
        [(OsString::from("PATH"), OsString::from("/bin"))]
    );
    let available = provenant_os::lane_ports::Plan {
        state: temp.path().into(),
        ..unavailable
    };
    let (first, warning) = lane_port_allocation(Some(&available)).map_err(std::io::Error::other)?;
    assert!(first.is_some());
    assert!(warning.is_none());
    let error = lane_port_allocation(Some(&available))
        .err()
        .ok_or("expected exhausted pool")?;
    assert!(
        error
            .to_string()
            .contains("domains.fabric.lanes.lane_port_blocks=1 (host configuration)")
    );
    assert_eq!(
        error.fact(),
        provenant_records::diagnostic::ResourceFact::LanePorts {
            blocks: 1,
            origin: provenant_records::diagnostic::PortSettingSource::Configuration
        }
    );
    // The automatic port range is a convenience, never an implicit writer cap.
    let automatic = provenant_os::lane_ports::Plan {
        blocks_origin: Some(provenant_os::lane_ports::BlocksOrigin::CompiledDefault),
        ..available
    };
    let (lease, warning) = lane_port_allocation(Some(&automatic)).map_err(std::io::Error::other)?;
    assert!(lease.is_none());
    assert!(warning.is_some());
    Ok(())
}

// Medium: descriptor pairing is checked before ACK or provider spawn.
#[test]
fn profile_handoff_requires_the_verified_file_and_matching_command()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let directory = Directory::open_anchor(root.path())?;
    let profile = crate::ProfileFile::create(b"(version 1)\n(deny default)\n", &directory)?;
    let argv = vec![
        "/usr/bin/sandbox-exec".into(),
        "-f".into(),
        profile.path(),
        "/usr/bin/true".into(),
    ];
    assert!(adopt_profile(None, Some(profile.identity().clone()), &argv).is_err());
    assert!(adopt_profile(Some(profile.file().try_clone()?), None, &argv).is_err());
    assert!(adopt_profile(None, None, &argv).is_err());
    let mut substituted = argv.clone();
    substituted[2] = "/tmp/replaceable.sb".into();
    assert!(
        adopt_profile(
            Some(profile.file().try_clone()?),
            Some(profile.identity().clone()),
            &substituted
        )
        .is_err()
    );
    let inherited = profile.file().try_clone()?;
    use std::os::fd::AsRawFd;
    let mut inherited_argv = argv.clone();
    inherited_argv[2] = format!("/dev/fd/{}", inherited.as_raw_fd()).into();
    let mut changed = profile.identity().clone();
    changed.sha256 = [0; 32];
    assert!(adopt_profile(Some(inherited), Some(changed), &inherited_argv).is_err());
    let inherited = profile.file().try_clone()?;
    inherited_argv[2] = format!("/dev/fd/{}", inherited.as_raw_fd()).into();
    assert!(
        adopt_profile(
            Some(inherited),
            Some(profile.identity().clone()),
            &inherited_argv
        )?
        .is_some()
    );
    Ok(())
}

#[test]
fn tracking_batches_fork_churn_but_publishes_initial_state_immediately() {
    let mut last = None;
    let mut publications = Vec::new();
    for now in (0..3_000).step_by(100) {
        if super::tracking_publication_due(now, last, false) {
            publications.push(now);
            last = Some(now);
        }
    }
    assert_eq!(publications, [0, 1_000, 2_000]);
    assert!(super::tracking_publication_due(2_100, last, true));
}

/// All budget actions obey the same completion and clean-point precedence.
#[test]
fn budget_actions_preserve_clean_points_and_natural_completion() {
    use provenant_records::BudgetAction;
    for action in [BudgetAction::WrapUp, BudgetAction::Warn, BudgetAction::Stop] {
        for budget in [0, 1, 1000, u64::MAX] {
            for used in [0, budget, budget.saturating_add(1), u64::MAX] {
                for clean in [false, true] {
                    for completed in [false, true] {
                        let boundary =
                            super::budget_boundary(budget, action, used, clean, completed);
                        if completed
                            || action == BudgetAction::Warn
                            || budget == 0
                            || used <= budget
                        {
                            assert_eq!(boundary, None);
                        } else {
                            assert_eq!(
                                boundary,
                                Some(if clean {
                                    super::BudgetBoundary::Clean
                                } else {
                                    super::BudgetBoundary::Dirty
                                })
                            );
                        }
                    }
                }
            }
        }
    }
}
