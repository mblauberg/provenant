// Add `#[cfg(test)] mod landing_tests;` to git_transport.rs.
use super::*;
use provenant_core::{
    ActorId, CancelToken, ClaimId, Clock, DurationMicros, MonoTime, PathResolver, ProjectId,
    RootInputs, StateRoot, TestClock, WaitBudget, WallTime,
};
use provenant_fabric_store::{
    Error as StoreError, LandingRequest, PushPhase, Request as StoreRequest, ThreadWait,
};
use provenant_supervisor::exec::{SystemExecutor, Termination};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

struct TestResolver;
impl PathResolver for TestResolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
        if path.exists() {
            path.canonicalize()
        } else {
            Ok(self
                .resolve_existing_ancestor(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?
                .join(path.file_name().ok_or(io::ErrorKind::InvalidInput)?))
        }
    }
}

struct LandingFixture {
    temp: tempfile::TempDir,
    root: StateRoot,
    clock: TestClock,
    cancel: CancelToken,
    backoff: ThreadWait,
}
impl LandingFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        Self {
            temp,
            root: inputs.resolve(&TestResolver).unwrap().state,
            clock: TestClock::new(
                WallTime::from_micros(2_000_000_000_000_000).unwrap(),
                MonoTime::from_micros(0),
            ),
            cancel: CancelToken::new(),
            backoff: ThreadWait,
        }
    }

    fn request(&self) -> StoreRequest<'_> {
        StoreRequest {
            clock: &self.clock,
            budget: WaitBudget::cli(self.clock.mono_now(), 60).unwrap(),
            cancel: &self.cancel,
            backoff: &self.backoff,
        }
    }
}

struct Repositories {
    fixture: LandingFixture,
    store: Store,
    guard: LandingGuard,
    fence: ClaimFence,
    writer: PathBuf,
    origin: PathBuf,
    redirect: PathBuf,
    base: GitOid,
    candidate: GitOid,
    operation: OperationId,
}

fn words(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

fn run_git(transport: &GitTransport<'_>, cwd: &Path, argv: &[OsString]) -> Vec<u8> {
    transport
        .require_success(
            "landing test setup",
            cwd,
            argv,
            None,
            MAX_TEXT_BYTES,
            &CancelToken::new(),
        )
        .unwrap()
        .stdout
        .bytes
}

fn git(transport: &GitTransport<'_>, cwd: &Path, parts: &[&str]) -> String {
    String::from_utf8(run_git(transport, cwd, &words(parts)))
        .unwrap()
        .trim()
        .to_owned()
}

fn git_with_paths(transport: &GitTransport<'_>, cwd: &Path, parts: &[&str], path: &Path) {
    let mut argv = words(parts);
    argv.push(path.as_os_str().to_owned());
    run_git(transport, cwd, &argv);
}

fn repositories() -> Repositories {
    let fixture = LandingFixture::new();
    let seed_executor = SystemExecutor::new(2).unwrap();
    let seed_transport = GitTransport::discover(&seed_executor, None).unwrap();
    let origin = fixture.temp.path().join("origin.git");
    let redirect = fixture.temp.path().join("redirect.git");
    let writer = fixture.temp.path().join("writer");
    fs::create_dir(&writer).unwrap();
    git_with_paths(
        &seed_transport,
        fixture.temp.path(),
        &["init", "--bare", "--initial-branch=main"],
        &origin,
    );
    git_with_paths(
        &seed_transport,
        fixture.temp.path(),
        &["init", "--bare", "--initial-branch=main"],
        &redirect,
    );
    git_with_paths(
        &seed_transport,
        fixture.temp.path(),
        &["init", "-b", "main"],
        &writer,
    );
    let mut add_remote = words(&["remote", "add", "origin"]);
    add_remote.push(origin.as_os_str().to_owned());
    run_git(&seed_transport, &writer, &add_remote);

    let identity = [
        "-c",
        "user.name=Fabric landing test",
        "-c",
        "user.email=fabric-landing@example.test",
        "commit",
        "--allow-empty",
        "-m",
        "base",
    ];
    git(&seed_transport, &writer, &identity);
    let base = seed_transport.head(&writer, &CancelToken::new()).unwrap();
    git(
        &seed_transport,
        &writer,
        &["push", "origin", "refs/heads/main"],
    );
    git(
        &seed_transport,
        &writer,
        &[
            "-c",
            "user.name=Fabric landing test",
            "-c",
            "user.email=fabric-landing@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "candidate",
        ],
    );
    let candidate = seed_transport.head(&writer, &CancelToken::new()).unwrap();

    let store = Store::open(&fixture.root, &fixture.request()).unwrap();
    let project = ProjectId::from_bytes([31; 16]);
    let branch = "refs/heads/main";
    let guard = store
        .lock_landing(project, branch, &fixture.request())
        .unwrap();
    let claim = LandingRequest {
        id: ClaimId::from_bytes([32; 16]),
        project,
        actor: ActorId::from_bytes([33; 16]),
        token: [34; 16],
        reference: branch.to_owned(),
        expected: base,
        candidate,
        lease: DurationMicros::from_seconds(1).unwrap(),
    };
    let fence = store
        .acquire_landing(&guard, &claim, &fixture.request())
        .unwrap();
    Repositories {
        fixture,
        store,
        guard,
        fence,
        writer,
        origin,
        redirect,
        base,
        candidate,
        operation: OperationId::parse("landing-lost-reply-1").unwrap(),
    }
}

struct LostReplyExecutor {
    inner: SystemExecutor,
    lose_first_push: AtomicBool,
}
impl LostReplyExecutor {
    fn new() -> Self {
        Self {
            inner: SystemExecutor::new(2).unwrap(),
            lose_first_push: AtomicBool::new(false),
        }
    }
}
impl Executor for LostReplyExecutor {
    fn execute(&self, options: ExecOptions<'_>) -> std::result::Result<ExecOutcome, ExecError> {
        let is_push = options.argv.iter().any(|arg| arg == OsStr::new("push"));
        let mut outcome = self.inner.execute(options)?;
        // The real child completed and changed the bare remote. Only its reply
        // is replaced, reproducing the owner's lost-response boundary.
        if is_push && !self.lose_first_push.swap(true, Ordering::SeqCst) {
            outcome.termination = Termination::Exit(255);
        }
        Ok(outcome)
    }
}

#[test]
fn lost_push_reply_keeps_successor_fenced_until_remote_reconciliation() {
    let mut setup = repositories();
    let executor = LostReplyExecutor::new();
    let transport = GitTransport::discover(&executor, None).unwrap();
    let project = ProjectId::from_bytes([31; 16]);
    let req = setup.fixture.request();
    let result = transport.push_landing(
        &setup.store,
        &setup.guard,
        &setup.fence,
        &setup.writer,
        &setup.operation,
        &req,
        &setup.fixture.cancel,
    );
    assert!(matches!(result, Err(GitTransportError::PushUnknown { .. })));
    assert!(executor.lose_first_push.load(Ordering::SeqCst));
    let unknown = setup
        .store
        .push_operation(project, &setup.operation, &req)
        .unwrap();
    assert_eq!(unknown.phase, PushPhase::Unknown);
    assert_eq!(unknown.request.expected, setup.base);
    assert_eq!(unknown.request.candidate, setup.candidate);
    assert_eq!(
        unknown.request.remote,
        fs::canonicalize(&setup.origin).unwrap().to_str().unwrap()
    );

    setup
        .fixture
        .clock
        .advance(DurationMicros::from_seconds(2).unwrap())
        .unwrap();
    let successor = LandingRequest {
        id: ClaimId::from_bytes([35; 16]),
        project,
        actor: ActorId::from_bytes([36; 16]),
        token: [37; 16],
        reference: "refs/heads/main".into(),
        expected: setup.base,
        candidate: setup.candidate,
        lease: DurationMicros::from_seconds(1).unwrap(),
    };
    assert!(matches!(
        setup
            .store
            .acquire_landing(&setup.guard, &successor, &setup.fixture.request()),
        Err(StoreError::PushUnresolved { operation }) if operation == setup.operation
    ));

    let reconciled = transport
        .reconcile_landing(
            &setup.store,
            &setup.guard,
            &setup.fence,
            project,
            &setup.operation,
            &setup.fixture.request(),
            &setup.writer,
            &setup.fixture.cancel,
        )
        .unwrap();
    assert_eq!(reconciled.phase, PushPhase::Committed);
    assert_eq!(reconciled.observed_remote, Some(setup.candidate));
    // A committed receipt must survive later local edits and lease expiry.
    fs::write(setup.writer.join("later-edit"), b"dirty after commit").unwrap();
    let replay = transport
        .push_landing(
            &setup.store,
            &setup.guard,
            &setup.fence,
            &setup.writer,
            &setup.operation,
            &setup.fixture.request(),
            &setup.fixture.cancel,
        )
        .unwrap();
    assert_eq!(replay, reconciled);
}

#[test]
fn local_url_rewrite_is_refused_before_it_can_push_to_another_repository() {
    let setup = repositories();
    let seed_executor = SystemExecutor::new(2).unwrap();
    let setup_transport = GitTransport::discover(&seed_executor, None).unwrap();
    let mut rewrite = words(&["config", "--local", "--add"]);
    rewrite.push(format!("url.{}.insteadOf", setup.redirect.display()).into());
    rewrite.push(setup.origin.as_os_str().to_owned());
    run_git(&setup_transport, &setup.writer, &rewrite);

    let executor = SystemExecutor::new(2).unwrap();
    let transport = GitTransport::discover(&executor, None).unwrap();
    let project = ProjectId::from_bytes([31; 16]);
    assert!(matches!(
        transport.push_landing(
            &setup.store,
            &setup.guard,
            &setup.fence,
            &setup.writer,
            &setup.operation,
            &setup.fixture.request(),
            &setup.fixture.cancel,
        ),
        Err(GitTransportError::InvalidRemote)
    ));
    assert_eq!(
        git(
            &setup_transport,
            &setup.origin,
            &["show-ref", "--hash", "--verify", "refs/heads/main"],
        ),
        setup.base.to_string()
    );
    assert!(!setup.redirect.join("refs/heads/main").exists());
    // A refusal before the durable push boundary leaves no operation that
    // could be mistaken for an attempted remote mutation.
    assert!(matches!(
        setup
            .store
            .push_operation(project, &setup.operation, &setup.fixture.request()),
        Err(StoreError::NotFound)
    ));
}

#[test]
fn ssh_url_host_cannot_be_interpreted_as_an_ssh_option() {
    assert!(validate_remote_url("ssh://user@-oProxyCommand=sh/repo.git").is_err());
}

#[test]
fn worktree_url_rewrite_is_also_refused_before_resolving_origin() {
    let setup = repositories();
    let executor = SystemExecutor::new(2).unwrap();
    let transport = GitTransport::discover(&executor, None).unwrap();
    run_git(
        &transport,
        &setup.writer,
        &words(&["config", "extensions.worktreeConfig", "true"]),
    );
    let mut rewrite = words(&["config", "--worktree", "--add"]);
    rewrite.push(format!("url.{}.insteadOf", setup.redirect.display()).into());
    rewrite.push(setup.origin.as_os_str().to_owned());
    run_git(&transport, &setup.writer, &rewrite);
    assert!(matches!(
        transport.resolve_origin_url(&setup.writer, &setup.fixture.cancel),
        Err(GitTransportError::InvalidRemote)
    ));
}
