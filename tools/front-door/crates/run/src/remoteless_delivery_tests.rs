//! Regression tests for #3869 outcome 3: a delivery request with no remote
//! must not pass an empty name to Git. `refresh_origin(.., None)` performs no
//! Git work at all; a push/PR destination with an empty remote is refused
//! before a binding can be sealed.
//! Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command

use std::{
    error::Error,
    path::{Path, PathBuf},
};

use provenant_core::CancelToken;
use provenant_records::run::{DeliveryMode, WriterDeliveryRequest, WriterRequest};

type FixtureResult<T> = std::result::Result<T, Box<dyn Error>>;

fn request(mode: DeliveryMode, remote: &str) -> WriterDeliveryRequest {
    WriterDeliveryRequest {
        extra: Default::default(),
        mode,
        remote: remote.to_owned(),
        branch: "lane".to_owned(),
        open_pr: false,
        fast_forward_only: false,
    }
}

fn writer() -> WriterRequest {
    WriterRequest {
        extra: Default::default(),
        custody_project: None,
        project: PathBuf::from("/nonexistent-remoteless/project"),
        worktree: PathBuf::from("/nonexistent-remoteless/lane"),
    }
}

/// The destination seam is the only way a resolved remote enters delivery. An
/// empty name is refused for push and PR, so a re-flatten of `None` to `""`
/// cannot silently produce a destination that pushes to a nameless remote.
#[test]
fn destination_for_remote_refuses_an_empty_name() {
    for mode in [DeliveryMode::Push, DeliveryMode::Pr] {
        let error = crate::HostDeliveryContext::destination_for_remote(mode, "lane", "")
            .expect_err("an empty remote must not make a push or PR destination");
        assert_eq!(error.code(), "INVALID");
        assert!(
            error.to_string().contains("configured remote name"),
            "{error}"
        );
    }
}

/// Local integration is the one explicit skip: an empty remote is allowed and
/// kept empty rather than replaced with an invented `origin`.
#[test]
fn destination_for_remote_keeps_local_integration_remote_empty() {
    let made = crate::HostDeliveryContext::destination_for_remote(
        DeliveryMode::Integrate("main".into()),
        "lane",
        "",
    )
    .expect("local integration needs no remote")
    .expect("an integration destination is produced");
    assert!(made.remote.is_empty());
    assert_eq!(made.branch, "main");
}

/// `prepare` validates the request before any Git work. With no remote a push
/// must be refused explicitly, not bound as if `""` were a remote name.
#[test]
fn prepare_refuses_a_push_without_a_remote() {
    let host = crate::HostDeliveryContext::new(Vec::new());
    let error = host
        .prepare(&writer(), &request(DeliveryMode::Push, ""))
        .expect_err("preparing a push with no remote must be refused");
    assert_eq!(error.code(), "INVALID");
    assert!(
        error.to_string().contains("configured remote name"),
        "{error}"
    );
}

const CHILD: &str = "PROVENANT_REMOTELESS_REFRESH_CHILD";
const LOG: &str = "PROVENANT_REMOTELESS_GIT_LOG";
const RAN: &str = "PROVENANT_REMOTELESS_REFRESH_RAN";

/// Assert, in a child with a controlled environment, that
/// `refresh_origin(.., None)` never invokes Git. The child runs with
/// `PROVENANT_GIT` pointed at a recording wrapper; the assertion is on the
/// recorded invocations, not on the return value (an unknown remote also
/// returns `Ok` from `refresh_origin_with`).
#[test]
fn refresh_origin_none_performs_no_fetch() -> FixtureResult<()> {
    if std::env::var_os(CHILD).is_some() {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let bin = temp.path().join("bin");
    let log = temp.path().join("git.log");
    let ran = temp.path().join("child.ran");
    std::fs::create_dir(&bin)?;
    let wrapper = bin.join("git");
    let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            git.display()
        ),
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;

    // Prove the wrapper records into the very log the parent reads, then clear
    // the prime so only the child's Git work can appear below.
    #[allow(
        clippy::disallowed_methods,
        reason = "the test primes its own recording Git wrapper"
    )]
    let primed = std::process::Command::new(&wrapper)
        .arg("--version")
        .output()?;
    assert!(
        primed.status.success(),
        "the recording wrapper must run Git"
    );
    assert!(
        std::fs::read_to_string(&log)?.contains("--version"),
        "the recording wrapper must append every invocation to the log"
    );
    std::fs::write(&log, "")?;

    let test_path =
        provenant_os::fixture::exact_test_path(module_path!(), "refresh_origin_none_child");
    #[allow(
        clippy::disallowed_methods,
        reason = "the test re-runs itself with a recording Git"
    )]
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", test_path.as_str(), "--nocapture"])
        .env_clear()
        .env("HOME", temp.path())
        .env("PATH", "/usr/bin:/bin")
        .env(provenant_core::consumer_env::GIT, &wrapper)
        .env(CHILD, "1")
        .env(LOG, &log)
        .env(RAN, &ran)
        .status()?;
    assert!(status.success(), "the recording child test failed");
    assert!(
        ran.exists(),
        "the exact child test must have run; a renamed child matches nothing and still exits 0"
    );

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.is_empty(),
        "refresh_origin(.., None) invoked Git: {calls}"
    );
    Ok(())
}

/// Child side of `refresh_origin_none_performs_no_fetch`. A no-op when the
/// whole suite runs; the parent sets `CHILD`, an exact test filter and `RAN`.
#[test]
fn refresh_origin_none_child() -> FixtureResult<()> {
    if std::env::var_os(CHILD).is_none() {
        return Ok(());
    }
    let ran = PathBuf::from(std::env::var_os(RAN).ok_or("missing child-ran marker path")?);
    std::fs::write(&ran, "1")?;
    let log = PathBuf::from(std::env::var_os(LOG).ok_or("missing log path")?);
    let cancel = CancelToken::new();
    crate::HostDeliveryContext::refresh_origin(
        &writer(),
        (Path::new("/nonexistent-remoteless/.git"), None),
        Path::new("/nonexistent-remoteless/state"),
        &cancel,
        &provenant_confinement::worktrees::RepoFacts::default(),
        (None, None),
        &provenant_records::policy::Policy::default(),
    )?;
    assert!(
        !log.exists() || std::fs::read_to_string(&log)?.is_empty(),
        "the None path must not invoke the recording Git"
    );
    Ok(())
}
