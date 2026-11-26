//! Regression tests for #3869 and #3895: a delivery destination is typed.
//! `Destination::Local` is the only way to spell "no remote"; a malformed
//! remote name is refused by the one `RemoteName` grammar at decode, so it can
//! never reach Git as a push destination or a `refs/remotes/` namespace.
//! Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command

use std::{
    error::Error,
    path::{Path, PathBuf},
};

use provenant_core::CancelToken;
use provenant_records::run::{DeliveryMode, Destination, WriterDeliveryRequest, WriterRequest};

type FixtureResult<T> = std::result::Result<T, Box<dyn Error>>;

fn request(mode: DeliveryMode, remote: Destination) -> WriterDeliveryRequest {
    WriterDeliveryRequest {
        extra: Default::default(),
        mode,
        remote,
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

/// The destination is parsed once on decode with the single `RemoteName`
/// grammar. An empty string spells neither a remote nor a local destination;
/// a name the resolver accepts, such as `team/upstream`, is usable here too.
#[test]
fn destination_is_parsed_once_with_the_single_remote_grammar() -> FixtureResult<()> {
    let good = serde_json::to_value(request(
        DeliveryMode::Push,
        Destination::from_name("origin").expect("fixture remote"),
    ))?;
    for bad in [
        serde_json::json!(""),
        serde_json::json!("-origin"),
        serde_json::json!(" origin"),
        serde_json::json!("or igin"),
        serde_json::json!("or..igin"),
    ] {
        let mut value = good.clone();
        value["remote"] = bad.clone();
        assert!(
            serde_json::from_value::<WriterDeliveryRequest>(value).is_err(),
            "the single grammar must refuse {bad}"
        );
    }
    let mut value = good.clone();
    value["remote"] = serde_json::json!("team/upstream");
    let decoded: WriterDeliveryRequest = serde_json::from_value(value)?;
    assert_eq!(
        decoded.remote,
        Destination::from_name("team/upstream").expect("fixture remote")
    );
    let mut value = good;
    value["remote"] = serde_json::Value::Null;
    let decoded: WriterDeliveryRequest = serde_json::from_value(value)?;
    assert_eq!(decoded.remote, Destination::Local);
    Ok(())
}

/// The destination seam is the only way a resolved remote enters delivery. A
/// missing remote is refused for push and PR, so re-flattening absence to an
/// empty name cannot silently produce a nameless destination.
#[test]
fn destination_for_remote_refuses_a_push_without_a_remote() {
    for mode in [DeliveryMode::Push, DeliveryMode::Pr] {
        let error = crate::HostDeliveryContext::destination_for_remote(mode, "lane", None)
            .expect_err("an absent remote must not make a push or PR destination");
        assert_eq!(error.code(), "INVALID");
        assert!(
            error.to_string().contains("configured remote name"),
            "{error}"
        );
    }
}

/// Local integration is the one explicit skip: its destination is typed
/// `Local`, never an empty remote name.
#[test]
fn destination_for_remote_keeps_local_integration_local() -> FixtureResult<()> {
    let made = crate::HostDeliveryContext::destination_for_remote(
        DeliveryMode::Integrate("main".into()),
        "lane",
        None,
    )?
    .expect("an integration destination is produced");
    assert_eq!(made.remote, Destination::Local);
    assert_eq!(made.branch, "main");
    Ok(())
}

/// A slash name that the resolver accepts is also accepted here: the write and
/// read paths share one grammar.
#[test]
fn destination_for_remote_accepts_a_slash_name() -> FixtureResult<()> {
    let upstream = provenant_config::RemoteName::new("team/upstream")?;
    let made = crate::HostDeliveryContext::destination_for_remote(
        DeliveryMode::Push,
        "lane",
        Some(upstream.clone()),
    )?
    .expect("push destination");
    assert_eq!(made.remote, Destination::remote(upstream));
    Ok(())
}

/// A sealed local-integration destination is typed `Local`; a
/// stop-publication binding on a real remote still yields that remote.
#[test]
fn sealed_local_integration_is_local_and_stop_publish_is_named() -> FixtureResult<()> {
    let host = crate::HostDeliveryContext::new(Vec::new()).with_bound_remotes_for_test(
        Destination::Local,
        Some(Destination::from_name("origin").expect("fixture remote")),
    );
    assert_eq!(host.delivery_remote(), None);
    let origin = provenant_config::RemoteName::new("origin")?;
    assert_eq!(host.stop_publish_remote(), Some(origin.clone()));
    assert_eq!(crate::writer::bound_stop_remote(Some(&host))?, Some(origin));
    Ok(())
}

/// The launch destination is read only when the stop binding has no remote. A
/// named launch destination stays named; a local-only binding is absence.
#[test]
fn bound_stop_remote_reads_the_launch_binding_only_when_needed() -> FixtureResult<()> {
    let host = crate::HostDeliveryContext::new(Vec::new()).with_bound_remotes_for_test(
        Destination::from_name("upstream").expect("fixture remote"),
        Some(Destination::from_name("origin").expect("fixture remote")),
    );
    assert_eq!(
        crate::writer::bound_stop_remote(Some(&host))?,
        Some(provenant_config::RemoteName::new("origin")?)
    );
    // The named launch destination is readable, but stop publication still
    // wins; a local-only launch has no remote at all.
    assert_eq!(
        host.delivery_remote(),
        Some(provenant_config::RemoteName::new("upstream")?)
    );
    let local_launch = crate::HostDeliveryContext::new(Vec::new())
        .with_bound_remotes_for_test(Destination::Local, None);
    assert_eq!(local_launch.delivery_remote(), None);
    assert_eq!(crate::writer::bound_stop_remote(Some(&local_launch))?, None);
    assert_eq!(crate::writer::bound_stop_remote(None)?, None);
    Ok(())
}

/// `prepare` validates the request before any Git work. A local destination
/// must be refused for push, not bound as if it named a remote.
#[test]
fn prepare_refuses_a_push_without_a_remote() {
    let host = crate::HostDeliveryContext::new(Vec::new());
    let error = host
        .prepare(&writer(), &request(DeliveryMode::Push, Destination::Local))
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
