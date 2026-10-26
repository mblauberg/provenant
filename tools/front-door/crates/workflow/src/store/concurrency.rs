#![allow(clippy::unwrap_used)]
// Process integration tests use real time to bound coordination.
#![allow(clippy::disallowed_methods)]

use super::*;
use crate::session::{HostSessionId, SessionStore};
use provenant_core::{Clock, RootInputs, StateRoot};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const CHILD_ID_ENV: &str = "PROVENANT_WORKFLOW_STORE_CHILD";
const BASE_ENV: &str = "PROVENANT_WORKFLOW_STORE_BASE";
const SOCKET_ENV: &str = "PROVENANT_WORKFLOW_STORE_SOCKET";
const CHILD_FILTER: &str = "store::tests::concurrency::concurrent_writer_fixture_process";
const REVISION_DEADLINE: Duration = Duration::from_secs(10);
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(15);

/// Real subprocesses exercise the stable lock inode and revision check.
#[test]
fn concurrent_process_writers_on_one_revision_have_one_winner() {
    if std::env::var_os(CHILD_ID_ENV).is_some() {
        return;
    }

    let (_temp, store, candidate) = setup();
    let base = store.root.as_path().parent().unwrap().to_path_buf();
    let candidate_path = base.join("candidate.json");
    fs::write(
        &candidate_path,
        receipt::canonical(&candidate).unwrap().as_bytes(),
    )
    .unwrap();

    let sockets = crate::test_support::short_tempdir("writer-ready.sock").unwrap();
    let socket_path = sockets.path().join("writer-ready.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut children = Vec::new();
    for id in ["a", "b"] {
        children.push(spawn_fixture(&base, &socket_path, id));
    }

    let mut streams = accept_ready(&listener, 2, HANDSHAKE_DEADLINE);
    for stream in &mut streams {
        stream.write_all(b"G").unwrap();
    }

    let statuses = wait_for_children(&mut children, REVISION_DEADLINE);
    assert!(statuses.iter().all(std::process::ExitStatus::success));

    let outcomes =
        ["a", "b"].map(|id| fs::read_to_string(base.join(format!("outcome-{id}"))).unwrap());
    assert_eq!(
        outcomes
            .iter()
            .filter(|value| value.as_str() == "WIN:1")
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|value| value.as_str() == "CONFLICT:0:1")
            .count(),
        1
    );
    let saved = store.read().unwrap().unwrap();
    assert_eq!(saved.revision(), 1);
    assert_eq!(saved.subject(), candidate.subject());
}

/// The test runner re-enters this exact test in each isolated fixture process.
#[test]
fn concurrent_writer_fixture_process() {
    let Some(id) = std::env::var_os(CHILD_ID_ENV) else {
        return;
    };
    let id = id.to_str().unwrap();
    let base = PathBuf::from(std::env::var_os(BASE_ENV).unwrap());
    let socket = PathBuf::from(std::env::var_os(SOCKET_ENV).unwrap());
    let result = run_writer_fixture(&base, &socket);
    fs::write(base.join(format!("outcome-{id}")), result).unwrap();
}

fn run_writer_fixture(base: &Path, socket_path: &Path) -> String {
    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream.set_read_timeout(Some(HANDSHAKE_DEADLINE)).unwrap();
    stream.write_all(b"R").unwrap();
    let mut release = [0];
    if stream.read_exact(&mut release).is_err() || release != *b"G" {
        return "HANDSHAKE_FAILED".into();
    }
    drop(stream);

    let mut inputs = RootInputs::new(
        PathBuf::from(std::env::var_os("HOME").unwrap()),
        base.join("checkout"),
    );
    inputs.state_dir = Some(PathBuf::from(
        std::env::var_os(provenant_core::STATE_DIRECTORY_ENV).unwrap(),
    ));
    let root = StateRoot::resolve(&inputs, &Resolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let delivery = DeliveryId::from_bytes([2; 16]);
    let store = ReceiptStore::new(root, project, delivery);
    let mut candidate_file = fs::File::open(base.join("candidate.json")).unwrap();
    let candidate = receipt::read(&mut candidate_file).unwrap();
    let clock = provenant_os::clock::SystemClock::new();
    let valid_until = time(4_102_444_800_000_000);
    let budget = WaitBudget::cli(clock.mono_now(), 3).unwrap();
    match store.save(
        &candidate,
        0,
        SaveContext {
            risk_policy: &RiskPolicy::default(),
            downgrade_approval: None,
            subject: candidate.subject(),
            valid_until,
        },
        budget,
        &CancelToken::new(),
        &clock,
    ) {
        Ok(saved) => format!("WIN:{}", saved.receipt.revision()),
        Err(StoreError::Conflict { expected, observed }) => {
            format!("CONFLICT:{expected}:{observed}")
        }
        Err(error) => format!("ERROR:{error}"),
    }
}

#[allow(clippy::disallowed_methods)]
fn spawn_fixture(base: &Path, socket: &Path, id: &str) -> Child {
    // ADR 0028: test-only fixture launches this binary with a fixed --exact target.
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_FILTER, "--nocapture"])
        .env_clear()
        .env("HOME", base.join("home"))
        .env(provenant_core::STATE_DIRECTORY_ENV, base.join("state"))
        .env(BASE_ENV, base)
        .env(SOCKET_ENV, socket)
        .env(CHILD_ID_ENV, id)
        .current_dir(base)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn accept_ready(listener: &UnixListener, count: usize, timeout: Duration) -> Vec<UnixStream> {
    let deadline = Instant::now().checked_add(timeout).unwrap();
    let mut streams = Vec::with_capacity(count);
    while streams.len() < count {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "fixture readiness deadline expired");
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(remaining)).unwrap();
                let mut ready = [0];
                stream.read_exact(&mut ready).unwrap();
                assert_eq!(ready, *b"R");
                streams.push(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "fixture processes did not reach the save barrier"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("fixture handshake accept failed: {error}"),
        }
    }
    streams
}

fn wait_for_children(children: &mut [Child], timeout: Duration) -> Vec<std::process::ExitStatus> {
    let deadline = Instant::now().checked_add(timeout).unwrap();
    let mut statuses = [None, None];
    while statuses.iter().any(Option::is_none) {
        for (index, child) in children.iter_mut().enumerate() {
            if statuses[index].is_none() {
                statuses[index] = child.try_wait().unwrap();
            }
        }
        assert!(
            Instant::now() < deadline || statuses.iter().all(Option::is_some),
            "fixture process exceeded its bounded wait"
        );
        if statuses.iter().any(Option::is_none) {
            std::thread::yield_now();
        }
    }
    statuses.into_iter().map(Option::unwrap).collect()
}

#[test]
fn delivery_and_session_locks_are_independent() {
    let (_temp, store, _candidate) = setup();
    let clock = clock(20);
    let budget = WaitBudget::cli(clock.mono_now(), 1).unwrap();
    let cancel = CancelToken::new();
    let first_dir = store.directory(true).unwrap();
    let _first_delivery = first_dir
        .lock(
            std::ffi::OsStr::new("receipt.lock"),
            provenant_os::fs::LockMode::Exclusive,
            budget,
            &cancel,
            &clock,
        )
        .unwrap();

    let other = ReceiptStore::new(
        store.root.clone(),
        store.project,
        DeliveryId::from_bytes([6; 16]),
    );
    let second_dir = other.directory(true).unwrap();
    let _second_delivery = second_dir
        .lock(
            std::ffi::OsStr::new("receipt.lock"),
            provenant_os::fs::LockMode::Exclusive,
            budget,
            &cancel,
            &clock,
        )
        .unwrap();

    let session = SessionStore::new(
        store.root.clone(),
        store.project,
        HostSessionId::parse("host-1").unwrap(),
    );
    session
        .save(b"# checkpoint", budget, &cancel, &clock)
        .unwrap();
    assert_eq!(session.read().unwrap().unwrap().markdown, "# checkpoint");
}
