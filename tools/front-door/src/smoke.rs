//! The one hermetic smoke lifecycle. A nonce fixture is created under the
//! target host's home, the writer and reader act on that fixture, and the
//! verdict counts only when the nonce is present in the reported results.
//!
//! The fixture is bound for the duration of a run and forgotten afterwards;
//! peer and tick dispatch locate graphs through that binding. A sweep at
//! lifecycle start removes fixtures left behind by crashed runs.

use provenant_cli::fabric::Context;
use provenant_core::{CancelToken, Clock, GitOid, HostId, WaitBudget};
use provenant_fabric_runtime::{
    coordination::facade::FacadeError,
    hosts::{PeerAction, PeerResult, SmokeFixtureOp, SmokeFixtureReceipt},
};
use provenant_records::{
    warning::{SMOKE_BINDING_RETAINED, SMOKE_FIXTURE_UNVERIFIED},
    warning::{Warning, warn},
};
use serde_json::Value;
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

/// The two commits a deterministic fixture creates: the base the writer starts
/// from and the head carrying the nonce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FixtureCommits {
    pub base: GitOid,
    pub head: GitOid,
}

/// A nonce is the only path input. It is always twelve lowercase hex digits,
/// so a caller can never name a path outside the fixture root.
fn valid_nonce(nonce: &str) -> bool {
    nonce.len() == 12
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Six bytes of `/dev/urandom`, hex encoded, exactly as the cloud nonce.
pub(crate) fn nonce() -> Result<String, String> {
    let mut bytes = [0_u8; 6];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("smoke nonce is unavailable: {error}"))?;
    Ok(provenant_core::hex(&bytes))
}

/// The home-relative root for every smoke fixture. A fixture path derived
/// here resolves identically on the initiator and every peer.
pub(crate) fn fixture_root(home: &Path) -> PathBuf {
    home.join(".local").join("state").join("provenant-smoke")
}

/// Run one Git command in `directory` with a hermetic environment. Author,
/// committer and dates come from the caller's `env` so the same nonce yields
/// the same OIDs everywhere; global and system config are ignored.
fn git(directory: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String, String> {
    let executable = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let mut command = std::process::Command::new(executable);
    command
        .current_dir(directory)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("smoke Git launch failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "smoke Git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The identity the writer uses for both fixture commits.
fn author_env() -> [(&'static str, &'static str); 6] {
    [
        ("GIT_AUTHOR_NAME", "Provenant Smoke"),
        ("GIT_AUTHOR_EMAIL", "smoke@localhost"),
        ("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z"),
        ("GIT_COMMITTER_NAME", "Provenant Smoke"),
        ("GIT_COMMITTER_EMAIL", "smoke@localhost"),
        ("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z"),
    ]
}

/// Build the deterministic fixture for `nonce` and register it as a project
/// just as a first launch does. The same nonce always yields the same two OIDs.
pub(crate) fn create_fixture(
    state: &Path,
    home: &Path,
    nonce: &str,
) -> Result<FixtureCommits, String> {
    if !valid_nonce(nonce) {
        return Err("smoke fixture nonce must be twelve lowercase hex digits".into());
    }
    let fixture = fixture_root(home).join(nonce);
    if fixture.exists() {
        std::fs::remove_dir_all(&fixture).map_err(|error| error.to_string())?;
    }
    std::fs::create_dir_all(&fixture).map_err(|error| error.to_string())?;
    let env = author_env();
    git(&fixture, &["init", "-q", "-b", "main"], &[])?;
    std::fs::write(fixture.join("fixture.txt"), "smoke base\n")
        .map_err(|error| error.to_string())?;
    git(&fixture, &["add", "fixture.txt"], &[])?;
    git(
        &fixture,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "smoke base"],
        &env,
    )?;
    let base_text = git(&fixture, &["rev-parse", "HEAD"], &[])?;
    let base = GitOid::parse(&base_text).map_err(|error| error.to_string())?;
    std::fs::write(fixture.join("fixture.txt"), format!("nonce: {nonce}\n"))
        .map_err(|error| error.to_string())?;
    git(&fixture, &["add", "fixture.txt"], &[])?;
    git(
        &fixture,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            &format!("smoke fixture {nonce}"),
        ],
        &env,
    )?;
    let head_text = git(&fixture, &["rev-parse", "HEAD"], &[])?;
    let head = GitOid::parse(&head_text).map_err(|error| error.to_string())?;

    let roots =
        crate::roots::child(home, home, state.to_path_buf()).map_err(|error| error.to_string())?;
    let project = std::fs::canonicalize(&fixture).map_err(|error| error.to_string())?;
    let common = std::fs::canonicalize(fixture.join(".git")).map_err(|error| error.to_string())?;
    let location =
        provenant_config::ProjectLocation::from_verified_primary_checkout(common, project)
            .map_err(|error| error.to_string())?;
    provenant_config::bind_project(&roots, location).map_err(|error| error.to_string())?;
    Ok(FixtureCommits { base, head })
}

/// Forget the fixture's binding and remove its directory. The nonce is the
/// only path guard; every caller has already proved it is twelve hex digits.
pub(crate) fn remove_fixture(state: &Path, home: &Path, nonce: &str) -> Result<(), String> {
    if !valid_nonce(nonce) {
        return Err("smoke fixture nonce must be twelve lowercase hex digits".into());
    }
    let fixture = fixture_root(home).join(nonce);
    let directory = provenant_config::host_project_dir(state, &fixture)
        .ok_or_else(|| "smoke fixture project has no host binding path".to_owned())?;
    let id = directory
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| provenant_core::ProjectId::parse(name).ok())
        .ok_or_else(|| "smoke fixture project binding name is invalid".to_owned())?;
    provenant_config::forget_project_at_state(state, id).map_err(|error| error.to_string())?;
    if fixture.exists() {
        std::fs::remove_dir_all(&fixture).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Remove fixtures older than twenty-four hours at lifecycle start. Each
/// removal is returned as a warning; a bind that cannot be forgotten is left
/// for the next sweep rather than failing the current smoke.
pub(crate) fn sweep_fixtures(state: &Path, home: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    let Ok(entries) = std::fs::read_dir(fixture_root(home)) else {
        return warnings;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_nonce(&name) {
            continue;
        }
        let age = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .unwrap_or_default();
        if age < Duration::from_secs(24 * 60 * 60) {
            continue;
        }
        match remove_fixture(state, home, &name) {
            Ok(()) => warnings.push(format!("smoke removed stale fixture {name}")),
            Err(error) => warnings.push(format!(
                "smoke could not remove stale fixture {name}: {error}"
            )),
        }
    }
    warnings
}

/// Ask one peer to create or remove the same fixture under its own home. The
/// ProjectLocator is built from the fixture's own binding, so the home-relative
/// fixture path resolves on both sides without carrying a caller path.
fn peer_fixture(
    state: &Path,
    home: &Path,
    host: &str,
    nonce: &str,
    create: bool,
) -> Result<SmokeFixtureReceipt, String> {
    let fixture = fixture_root(home).join(nonce);
    let common = std::fs::canonicalize(fixture.join(".git")).map_err(|error| error.to_string())?;
    let roots =
        crate::roots::child(home, home, state.to_path_buf()).map_err(|error| error.to_string())?;
    let location =
        provenant_config::ProjectLocation::new(common).map_err(|error| error.to_string())?;
    let binding = provenant_config::lookup_project(&roots, &location)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "smoke fixture has no binding for the peer request".to_owned())?;
    let context =
        Context::open_for_project(roots, home, &binding).map_err(|error| error.to_string())?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 120).map_err(|error| error.to_string())?;
    let host = HostId::parse(host).map_err(|_| "smoke peer host is invalid".to_owned())?;
    let op = if create {
        SmokeFixtureOp::Create {
            nonce: nonce.to_owned(),
        }
    } else {
        SmokeFixtureOp::Remove {
            nonce: nonce.to_owned(),
        }
    };
    match context.peer(
        &clock,
        host,
        PeerAction::SmokeFixture {
            extra: Default::default(),
            op,
        },
        budget,
        &cancel,
    ) {
        Ok(PeerResult::SmokeFixture { data, .. }) => Ok(data),
        Ok(_) => Err("smoke fixture peer reply has the wrong shape".into()),
        Err(failure) => Err(match failure.error.as_ref() {
            FacadeError::PeerProtocolMismatch { .. } | FacadeError::PeerVersion => {
                "SMOKE_PEER_TOO_OLD".into()
            }
            _ => failure.detail.unwrap_or_else(|| failure.error.to_string()),
        }),
    }
}

/// Whether a peer failure means the peer cannot speak the fixture protocol at all.
fn peer_too_old(error: &str) -> bool {
    error == "SMOKE_PEER_TOO_OLD"
}

/// A unique graph name for this lifecycle invocation.
fn verify_graph_name() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!(
        "smoke-verify-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Collect status envelopes' rows without trusting any single line as whole
/// output. Only typed JSON envelopes contribute rows.
fn status_rows(output: &str) -> Vec<Value> {
    let mut rows = Vec::new();
    for line in output.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("schema_version").is_none() || value.get("command").is_none() {
            continue;
        }
        if let Some(found) = value["data"]["rows"].as_array() {
            rows.extend(found.iter().cloned());
        }
    }
    rows
}

/// A node row's id is exactly the template node id; attempt rows name the same
/// node. Only node rows carry the settled state we verify.
fn row_for<'a>(rows: &'a [Value], node: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|row| row["kind"] == "node" && row["node_id"].as_str() == Some(node))
}

fn summary_has(row: &Value, nonce: &str) -> bool {
    row["result_summary"]
        .as_str()
        .is_some_and(|summary| summary.contains(nonce))
}

/// Verify that the reported rows are the ones this lifecycle produced. A
/// verdict without the nonce is a false pass: name the failing row so an
/// operator can see which node lost its fixture.
fn verify_status(status: &str, nonce: &str, write_enabled: bool) -> Result<(), String> {
    let rows = status_rows(status);
    if rows.is_empty() {
        return Err(format!("smoke status named no rows for nonce {nonce}"));
    }
    let read = row_for(&rows, "read")
        .ok_or_else(|| format!("smoke status named no read row for nonce {nonce}"))?;
    if !summary_has(read, nonce) {
        return Err(format!(
            "smoke read row does not report nonce {nonce}: {read}"
        ));
    }
    let reviewer = if write_enabled {
        "review"
    } else {
        "review-fixture"
    };
    let review = row_for(&rows, reviewer)
        .ok_or_else(|| format!("smoke status named no {reviewer} row for nonce {nonce}"))?;
    if !summary_has(review, nonce) {
        return Err(format!(
            "smoke {reviewer} row does not report nonce {nonce}: {review}"
        ));
    }
    let write = row_for(&rows, "write")
        .ok_or_else(|| format!("smoke status named no write row for nonce {nonce}"))?;
    let accepted = if write_enabled {
        write["state"] == "succeeded"
    } else {
        write["state"] == "skipped"
    };
    if !accepted {
        return Err(format!(
            "smoke write row is not {} for nonce {nonce}: {write}",
            if write_enabled {
                "succeeded"
            } else {
                "skipped"
            }
        ));
    }
    Ok(())
}

/// Cleanup is secondary to the smoke verdict, but a retained binding is never
/// silent: its warning accompanies a passed smoke and is appended to a failed
/// one.
pub(crate) fn report_smoke_cleanup<T>(
    outcome: Result<T, String>,
    cleanup: Result<(), String>,
) -> Result<T, String> {
    let Err(error) = cleanup else {
        return outcome;
    };
    let warning = format!("smoke fixture binding was not removed: {error}");
    warn(Warning::new(SMOKE_BINDING_RETAINED, warning.clone()).once_per_change());
    match outcome {
        Ok(output) => Ok(output),
        Err(error) => Err(format!("{error}\n{warning}")),
    }
}

/// Run one hermetic smoke graph: create the fixture, verify the peer's fixture
/// when placed remotely, launch, wait, status, and always clean up. Success
/// requires the nonce in both read and review results; an old peer can only
/// produce an unverified result, never a pass.
pub(crate) fn run_lifecycle(
    runner: &crate::install::UpgradeRunner,
    program: &Path,
    state: &Path,
    home: &Path,
    args: &cli::Upgrade,
) -> Result<String, String> {
    let mut warnings = sweep_fixtures(state, home);
    let nonce = nonce()?;
    let commits = create_fixture(state, home, &nonce)?;
    let mut unverified = false;
    let host = args
        .host
        .as_deref()
        .filter(|host| !host.is_empty() && !matches!(*host, "local" | "localhost"));
    if let Some(host) = host {
        match peer_fixture(state, home, host, &nonce, true) {
            Ok(receipt) => {
                if receipt.base != Some(commits.base) || receipt.head != Some(commits.head) {
                    let _ = peer_fixture(state, home, host, &nonce, false);
                    let _ = remove_fixture(state, home, &nonce);
                    return Err("peer fixture differs".into());
                }
            }
            Err(error) if peer_too_old(&error) => {
                warnings.push(format!(
                    "smoke peer {host} cannot verify a fixture: {error}"
                ));
                unverified = true;
            }
            Err(error) => {
                let _ = remove_fixture(state, home, &nonce);
                return Err(error);
            }
        }
    }
    let graph = verify_graph_name();
    let fixture = fixture_root(home).join(&nonce);
    let program = program.as_os_str().to_owned();
    let mut run = vec![
        program.clone(),
        "--project".into(),
        fixture.as_os_str().to_owned(),
        "--state-dir".into(),
        state.as_os_str().to_owned(),
        "--json".into(),
        "run".into(),
        "smoke".into(),
        "--name".into(),
        graph.clone().into(),
        "--branch".into(),
        format!("smoke/{graph}").into(),
        "--base".into(),
        commits.head.to_string().into(),
    ];
    if let Some(host) = host {
        run.extend(["--host".into(), host.into()]);
    }
    if let Some(route) = &args.route {
        run.extend(["--route".into(), route.clone().into()]);
    }
    let mut diff_base_set = false;
    let mut fixture_base_set = false;
    for setting in &args.set {
        if setting.starts_with("diff_base=") {
            diff_base_set = true;
        }
        if setting.starts_with("fixture_base=") {
            fixture_base_set = true;
        }
        run.extend(["--set".into(), setting.clone().into()]);
    }
    if !diff_base_set {
        run.extend(["--set".into(), format!("diff_base={}", commits.head).into()]);
    }
    if !fixture_base_set {
        run.extend([
            "--set".into(),
            format!("fixture_base={}", commits.base).into(),
        ]);
    }
    let write_enabled = !args
        .set
        .iter()
        .any(|setting| setting == "smoke-write=false");
    let outcome = match runner.execute(&run, None, 1_048_576, 3600) {
        Ok(launched) => {
            let wait = runner.execute(
                &[
                    program.clone(),
                    "--project".into(),
                    fixture.as_os_str().to_owned(),
                    "--state-dir".into(),
                    state.as_os_str().to_owned(),
                    "--json".into(),
                    "wait".into(),
                    graph.clone().into(),
                    "--timeout".into(),
                    "3600".into(),
                ],
                None,
                1_048_576,
                3700,
            );
            let status = runner.execute(
                &[
                    program,
                    "--project".into(),
                    fixture.as_os_str().to_owned(),
                    "--state-dir".into(),
                    state.as_os_str().to_owned(),
                    "--json".into(),
                    "status".into(),
                    graph.into(),
                ],
                None,
                1_048_576,
                60,
            );
            let combined = format!(
                "{}{}{}",
                String::from_utf8_lossy(&launched),
                wait.as_ref()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .unwrap_or_else(|error| error.clone()),
                status
                    .as_ref()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .unwrap_or_else(|error| error.clone())
            );
            match (wait, status) {
                (Ok(_), Ok(status)) => {
                    if unverified {
                        Ok(combined)
                    } else {
                        verify_status(&String::from_utf8_lossy(&status), &nonce, write_enabled)
                            .map(|()| combined)
                    }
                }
                (Err(error), status) => Err(format!(
                    "{}\nsmoke node report: {}",
                    error,
                    status
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                        .unwrap_or_else(|err| err)
                )),
                (Ok(_), Err(error)) => Err(format!(
                    "{error}\nsmoke launch: {}",
                    String::from_utf8_lossy(&launched)
                )),
            }
        }
        Err(error) => Err(error),
    };
    // An unverified peer can never produce a pass, whatever the status rows
    // say. Keep the report but change the outcome so the caller cannot present
    // it as a verified smoke.
    let outcome = if unverified {
        match outcome {
            Ok(output) => Err(format!(
                "{output}\nsmoke result is unverified: the peer fixture could not be checked"
            )),
            Err(error) => Err(error),
        }
    } else {
        outcome
    };
    // The peer request still needs the local fixture path to build its
    // locator, so remove remotely first and locally afterwards.
    let peer_cleanup = match host {
        Some(host) => match peer_fixture(state, home, host, &nonce, false) {
            Ok(_) => Ok(()),
            Err(error) if peer_too_old(&error) => Ok(()),
            Err(error) => Err(error),
        },
        None => Ok(()),
    };
    let local_cleanup = remove_fixture(state, home, &nonce);
    let cleanup = peer_cleanup.and(local_cleanup);
    if unverified {
        warn(
            Warning::new(
                SMOKE_FIXTURE_UNVERIFIED,
                "the peer could not verify the smoke fixture",
            )
            .once_per_change(),
        );
    }
    let suffix = if warnings.is_empty() {
        String::new()
    } else {
        format!("\n{}", warnings.join("\n"))
    };
    let outcome = outcome
        .map(|output| format!("{output}{suffix}"))
        .map_err(|error| format!("{error}{suffix}"));
    report_smoke_cleanup(outcome, cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_root_is_home_relative() {
        let home = Path::new("/tmp/provenant-home");
        assert_eq!(
            fixture_root(home),
            Path::new("/tmp/provenant-home/.local/state/provenant-smoke")
        );
    }

    #[test]
    fn nonce_guard_refuses_everything_outside_twelve_hex_digits() {
        assert!(valid_nonce("0123456789ab"));
        for bad in [
            "0123456789a",
            "0123456789abc",
            "0123456789aB",
            "../etc/passwd",
            "",
        ] {
            assert!(!valid_nonce(bad), "{bad} must be refused");
        }
    }

    #[test]
    fn status_verification_requires_the_nonce_in_read_and_review() {
        let nonce = "0123456789ab";
        let good = format!(
            r#"{{"schema_version":1,"command":"status","data":{{"rows":[{{"kind":"node","node_id":"read","state":"succeeded","result_summary":"nonce: {nonce}"}},{{"kind":"node","node_id":"review","state":"succeeded","result_summary":"nonce: {nonce}"}},{{"kind":"node","node_id":"write","state":"succeeded"}}]}}}}"#
        );
        verify_status(&good, nonce, true).expect("the nonce is present in every required row");
        let bad = good.replace(&format!("nonce: {nonce}"), "no nonce here");
        let error = verify_status(&bad, nonce, true).expect_err("a false pass is refused");
        assert!(error.contains("read"), "{error}");
        let no_review = good.replace("\"node_id\":\"review\"", "\"node_id\":\"review-fixture\"");
        assert!(
            verify_status(&no_review, nonce, true)
                .expect_err("the wrong reviewer is refused")
                .contains("review"),
        );
        let skipped = good.replace(
            "\"node_id\":\"write\",\"state\":\"succeeded\"",
            "\"node_id\":\"write\",\"state\":\"skipped\"",
        );
        assert!(
            verify_status(&skipped, nonce, true)
                .expect_err("a skipped writer is not a pass when write is enabled")
                .contains("write"),
        );
    }
}
