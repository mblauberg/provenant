//! The one hermetic smoke lifecycle. A nonce fixture is created under the
//! target host's home, the writer and reader act on that fixture, and the
//! verdict counts only when the nonce is present in the reported results.
//!
//! The fixture is bound for the duration of a run and forgotten afterwards;
//! peer and tick dispatch locate graphs through that binding. A sweep at
//! lifecycle start removes fixtures left behind by crashed runs.

use provenant_cli::{
    Upgrade,
    fabric::Context,
    smoke::{
        BRANCH_PREFIX_KEY, FIXTURE_ROOT_KEY, Origin as SmokeOrigin, SWEEP_AFTER_HOURS_KEY,
        Settings, UNVERIFIED_KEY, Unverified,
    },
};
use provenant_core::{CancelToken, Clock, GitOid, HostId, WaitBudget, WallTime};
use provenant_fabric_runtime::{
    coordination::facade::FacadeError,
    hosts::{PeerAction, PeerResult, SmokeFixtureOp, SmokeFixtureReceipt},
};
use provenant_records::warning::{
    SMOKE_BINDING_RETAINED, SMOKE_FIXTURE_FOREIGN, SMOKE_PEER_FIXTURE_UNSUPPORTED, Warning, warn,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

/// Resolve the one `[domains.smoke]` view for an entry point. Unreadable
/// configuration warns and leaves compiled defaults; never refuse smoke for it.
pub(crate) fn settings(roots: &provenant_core::Roots) -> (Settings, SmokeOrigin) {
    Settings::resolve(roots).unwrap_or_else(|_| {
        (
            Settings::default(),
            SmokeOrigin::compiled(Default::default()),
        )
    })
}

/// The two commits a deterministic fixture creates: the base the writer starts
/// from and the head carrying the nonce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FixtureCommits {
    pub base: GitOid,
    pub head: GitOid,
}

/// The current deterministic fixture recipe. A peer echoes the recipe it built;
/// a mismatch fails the lifecycle rather than passing two different fixtures off
/// as the same one. Bump this when the fixture identity changes.
pub const FIXTURE_RECIPE: u32 = 1;

/// The ownership record written inside `.git`, outside the committed tree so
/// the nonce still determines both commit OIDs. A directory without a valid
/// marker is foreign and is never removed by the sweep.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FixtureMarker {
    pub schema_version: u32,
    pub nonce: String,
    pub created_at: WallTime,
    pub recipe: u32,
}

/// The typed verdict of a smoke lifecycle. A pass requires the nonce in both
/// read rows; an unverified result can never be presented as a pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum SmokeVerdict {
    Passed,
    Failed { row: String, reason: String },
    Unverified { reason: String },
}

/// One measured phase of a lifecycle. The five names are fixed: fixture,
/// launch, wait, status and cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmokePhase {
    pub name: String,
    pub elapsed_ms: u64,
}

/// One effective smoke template parameter and the layer that supplied it:
/// `config.toml`, `--set`, or the lifecycle's own fixture defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmokeParameter {
    pub value: String,
    pub source: String,
}

/// The structured result of one lifecycle: verdict, phase timings, the effective
/// parameters with sources and the typed envelopes that produced them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmokeOutcome {
    pub verdict: SmokeVerdict,
    pub phases: Vec<SmokePhase>,
    pub parameters: BTreeMap<String, SmokeParameter>,
    pub config: BTreeMap<String, SmokeParameter>,
    pub output: String,
}

impl SmokeOutcome {
    /// The typed result line: verdict fields, all five phase timings and the
    /// sourced parameters. It carries no command envelope, so verdict readers
    /// keep using the child envelopes in `output`.
    pub fn result_json(&self) -> Value {
        let mut value = serde_json::to_value(&self.verdict).unwrap_or(Value::Null);
        if let Some(map) = value.as_object_mut() {
            map.insert(
                "phases".into(),
                serde_json::to_value(&self.phases).unwrap_or(Value::Null),
            );
            map.insert(
                "parameters".into(),
                serde_json::to_value(&self.parameters).unwrap_or(Value::Null),
            );
            map.insert(
                "config".into(),
                serde_json::to_value(&self.config).unwrap_or(Value::Null),
            );
        }
        value
    }

    /// The text result published by both entry points: child envelopes, then
    /// the typed verdict and phase line. A failure names its row in that line.
    pub fn text(&self) -> String {
        let result = serde_json::to_string(&self.result_json()).unwrap_or_default();
        if self.output.is_empty() {
            result
        } else {
            format!("{}\n{result}", self.output)
        }
    }
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

/// The configured home-relative root for smoke fixtures. `root` has already
/// passed [`provenant_cli::smoke::validate_root`] on both hosts, so the same
/// relative path resolves under each host's home without carrying a caller path.
pub(crate) fn fixture_root(home: &Path, root: &Path) -> PathBuf {
    home.join(root)
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
    root: &Path,
) -> Result<FixtureCommits, String> {
    if !valid_nonce(nonce) {
        return Err("smoke fixture nonce must be twelve lowercase hex digits".into());
    }
    let fixture = fixture_root(home, root).join(nonce);
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

    // The marker lives in .git, so it cannot change either commit OID. It is
    // the recoverable ownership record the sweep uses to tell our fixtures
    // from anything else a user put under the fixture root.
    let created_at = provenant_os::clock::SystemClock::new()
        .wall_now()
        .map_err(|error| error.to_string())?;
    let marker = FixtureMarker {
        schema_version: 1,
        nonce: nonce.to_owned(),
        created_at,
        recipe: FIXTURE_RECIPE,
    };
    let marker_text = serde_json::to_string(&marker).map_err(|error| error.to_string())?;
    std::fs::write(
        fixture.join(".git").join("provenant-smoke.json"),
        marker_text,
    )
    .map_err(|error| error.to_string())?;

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
pub(crate) fn remove_fixture(
    state: &Path,
    home: &Path,
    nonce: &str,
    root: &Path,
) -> Result<(), String> {
    if !valid_nonce(nonce) {
        return Err("smoke fixture nonce must be twelve lowercase hex digits".into());
    }
    let fixture = fixture_root(home, root).join(nonce);
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

/// Remove fixtures older than the configured window at lifecycle start. Zero
/// hours skips the sweep entirely. Only a directory with a parsed marker whose
/// nonce matches its name and whose creation time is past the threshold is ours
/// to remove; a directory without such a marker is foreign and is left in place
/// with SMOKE_FIXTURE_FOREIGN. Each removal is returned as a warning; a bind
/// that cannot be forgotten is left for the next sweep rather than failing smoke.
pub(crate) fn sweep_fixtures(
    state: &Path,
    home: &Path,
    root: &Path,
    after_hours: u64,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if after_hours == 0 {
        return warnings;
    }
    let Ok(entries) = std::fs::read_dir(fixture_root(home, root)) else {
        return warnings;
    };
    let now = provenant_os::clock::SystemClock::new()
        .wall_now()
        .map(|time| time.as_micros())
        .unwrap_or(i64::MAX);
    let threshold = after_hours
        .saturating_mul(60 * 60 * 1_000_000)
        .min(i64::MAX as u64) as i64;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let marker = std::fs::read(entry.path().join(".git").join("provenant-smoke.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<FixtureMarker>(&bytes).ok())
            .filter(|marker| marker.schema_version == 1 && marker.nonce == name);
        let Some(marker) = marker else {
            let message = format!("smoke fixture {name} has no ownership marker; left in place");
            warn(Warning::new(SMOKE_FIXTURE_FOREIGN, message.clone()).once_per_change());
            warnings.push(message);
            continue;
        };
        let age = now.saturating_sub(marker.created_at.as_micros());
        if age < threshold {
            continue;
        }
        match remove_fixture(state, home, &name, root) {
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
    root: &Path,
) -> Result<SmokeFixtureReceipt, String> {
    let fixture = fixture_root(home, root).join(nonce);
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
            recipe: FIXTURE_RECIPE,
            root: root.to_string_lossy().into_owned(),
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

/// A peer must echo the recipe it built and the same two commits. A recipe
/// mismatch is named explicitly so drift is not mistaken for a different head.
fn peer_fixture_matches(
    receipt: &SmokeFixtureReceipt,
    commits: FixtureCommits,
) -> Result<(), String> {
    let peer_recipe = receipt.recipe.unwrap_or(0);
    if peer_recipe != FIXTURE_RECIPE {
        return Err(format!(
            "peer fixture recipe {peer_recipe} differs from {FIXTURE_RECIPE}"
        ));
    }
    if receipt.base != Some(commits.base) || receipt.head != Some(commits.head) {
        return Err("peer fixture differs".into());
    }
    Ok(())
}

/// A unique graph name for this lifecycle invocation, under the configured
/// branch prefix. The prefix has already passed Git ref validation.
fn smoke_graph_name(prefix: &str) -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!(
        "{prefix}{}-{}",
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

/// A failure always carries the row that caused it and the reason, so the
/// verdict text names exactly which node lost its fixture.
fn failed(row: &str, reason: String) -> SmokeVerdict {
    SmokeVerdict::Failed {
        row: row.to_owned(),
        reason,
    }
}

/// Verify that the reported rows are the ones this lifecycle produced. A
/// verdict without the nonce is a false pass: name the failing row so an
/// operator can see which node lost its fixture.
fn verify_status(status: &str, nonce: &str, write_enabled: bool) -> Result<(), SmokeVerdict> {
    let rows = status_rows(status);
    if rows.is_empty() {
        return Err(failed(
            "status",
            format!("smoke status named no rows for nonce {nonce}"),
        ));
    }
    let read = row_for(&rows, "read").ok_or_else(|| {
        failed(
            "read",
            format!("smoke status named no read row for nonce {nonce}"),
        )
    })?;
    if !summary_has(read, nonce) {
        return Err(failed(
            "read",
            format!("smoke read row does not report nonce {nonce}: {read}"),
        ));
    }
    let reviewer = if write_enabled {
        "review-write"
    } else {
        "review-fixture"
    };
    let review = row_for(&rows, reviewer).ok_or_else(|| {
        failed(
            reviewer,
            format!("smoke status named no {reviewer} row for nonce {nonce}"),
        )
    })?;
    if !summary_has(review, nonce) {
        return Err(failed(
            reviewer,
            format!("smoke {reviewer} row does not report nonce {nonce}: {review}"),
        ));
    }
    let write = row_for(&rows, "write").ok_or_else(|| {
        failed(
            "write",
            format!("smoke status named no write row for nonce {nonce}"),
        )
    })?;
    let accepted = if write_enabled {
        write["state"] == "succeeded"
    } else {
        write["state"] == "skipped"
    };
    if !accepted {
        return Err(failed(
            "write",
            format!(
                "smoke write row is not {} for nonce {nonce}: {write}",
                if write_enabled {
                    "succeeded"
                } else {
                    "skipped"
                }
            ),
        ));
    }
    Ok(())
}

/// Map a typed verdict and its report to an exit result. A failure always
/// fails; an unverified result fails only under the configured `fail` policy,
/// whose source is named in the report. The choice is host configuration, never
/// an implicit pass.
pub(crate) fn verdict_exit(
    verdict: &SmokeVerdict,
    text: &str,
    settings: &Settings,
    origin: &SmokeOrigin,
) -> Result<String, String> {
    match verdict {
        SmokeVerdict::Failed { .. } => Err(text.to_owned()),
        SmokeVerdict::Unverified { .. } if settings.unverified == Unverified::Fail => Err(format!(
            "{text}\nsource: {}",
            origin.describe(UNVERIFIED_KEY)
        )),
        SmokeVerdict::Passed | SmokeVerdict::Unverified { .. } => Ok(text.to_owned()),
    }
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
/// when placed remotely, launch, wait, status, and always clean up. The typed
/// outcome carries the verdict and five phase timings; a pass requires the
/// nonce in both read rows, and an old peer can only produce an unverified
/// result, never a pass.
pub(crate) fn run_lifecycle(
    clock: &dyn Clock,
    runner: &crate::install::ChildRunner,
    program: &Path,
    state: &Path,
    home: &Path,
    args: &Upgrade,
) -> Result<SmokeOutcome, String> {
    let roots =
        crate::roots::child(home, home, state.to_path_buf()).map_err(|error| error.to_string())?;
    let (settings, origin) = settings(&roots);
    let root = settings.root();
    let mut warnings = sweep_fixtures(state, home, &root, settings.sweep_after_hours);
    let config = BTreeMap::from([
        (
            FIXTURE_ROOT_KEY.to_owned(),
            SmokeParameter {
                value: root.to_string_lossy().into_owned(),
                source: origin.describe(FIXTURE_ROOT_KEY),
            },
        ),
        (
            SWEEP_AFTER_HOURS_KEY.to_owned(),
            SmokeParameter {
                value: settings.sweep_after_hours.to_string(),
                source: origin.describe(SWEEP_AFTER_HOURS_KEY),
            },
        ),
        (
            BRANCH_PREFIX_KEY.to_owned(),
            SmokeParameter {
                value: settings.branch_prefix.clone(),
                source: origin.describe(BRANCH_PREFIX_KEY),
            },
        ),
        (
            UNVERIFIED_KEY.to_owned(),
            SmokeParameter {
                value: settings.unverified.as_str().to_owned(),
                source: origin.describe(UNVERIFIED_KEY),
            },
        ),
    ]);
    let nonce = nonce()?;
    let initial = clock.mono_now();
    // Each phase keeps its begin/end reading; skipped phases stay at zero.
    let mut fixture_phase = (initial, initial);
    let mut launch_phase = (initial, initial);
    let mut wait_phase = (initial, initial);
    let mut status_phase = (initial, initial);
    let mut cleanup_phase = (initial, initial);

    fixture_phase.0 = clock.mono_now();
    let commits = create_fixture(state, home, &nonce, &root)?;
    let mut verdict = SmokeVerdict::Passed;
    let host = args
        .host
        .as_deref()
        .filter(|host| !host.is_empty() && !matches!(*host, "local" | "localhost"));
    if let Some(host) = host {
        match peer_fixture(state, home, host, &nonce, true, &root) {
            Ok(receipt) => {
                if let Err(error) = peer_fixture_matches(&receipt, commits) {
                    let _ = peer_fixture(state, home, host, &nonce, false, &root);
                    let _ = remove_fixture(state, home, &nonce, &root);
                    return Err(error);
                }
            }
            Err(error) if peer_too_old(&error) => {
                warnings.push(format!(
                    "smoke peer {host} cannot verify a fixture: {error}"
                ));
                verdict = SmokeVerdict::Unverified {
                    reason: "the peer fixture could not be checked".into(),
                };
            }
            Err(error) => {
                let _ = remove_fixture(state, home, &nonce, &root);
                return Err(error);
            }
        }
    }
    fixture_phase.1 = clock.mono_now();
    let graph = smoke_graph_name(&settings.branch_prefix);
    let fixture = fixture_root(home, &root).join(&nonce);
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
        format!("{}{graph}", settings.branch_prefix).into(),
        "--base".into(),
        commits.head.to_string().into(),
    ];
    if let Some(host) = host {
        run.extend(["--host".into(), host.into()]);
    }
    if let Some(route) = &args.route {
        run.extend(["--route".into(), route.clone().into()]);
    }
    // Configured parameters are merged under `--set`; a command-line item of
    // the same key wins. Each effective value carries the layer that chose it.
    let mut parameters = BTreeMap::new();
    let cli_keys: std::collections::BTreeSet<&str> = args
        .set
        .iter()
        .map(|item| item.split_once('=').map_or(item.as_str(), |(key, _)| key))
        .collect();
    for (key, value) in &settings.set {
        if !cli_keys.contains(key.as_str()) {
            parameters.insert(
                key.clone(),
                SmokeParameter {
                    value: value.clone(),
                    source: format!("config.toml (domains.smoke.set.{key})"),
                },
            );
        }
    }
    for item in &args.set {
        if let Some((key, value)) = item.split_once('=') {
            parameters.insert(
                key.to_owned(),
                SmokeParameter {
                    value: value.to_owned(),
                    source: "--set".into(),
                },
            );
        }
    }
    let mut set = settings.merged_set(&args.set);
    let has = |name: &str| {
        set.iter()
            .any(|item| item.split_once('=').is_some_and(|(key, _)| key == name))
    };
    if !has("diff_base") {
        set.push(format!("diff_base={}", commits.head));
        parameters.insert(
            "diff_base".to_owned(),
            SmokeParameter {
                value: commits.head.to_string(),
                source: "lifecycle (fixture head)".into(),
            },
        );
    }
    if !has("fixture_base") {
        set.push(format!("fixture_base={}", commits.base));
        parameters.insert(
            "fixture_base".to_owned(),
            SmokeParameter {
                value: commits.base.to_string(),
                source: "lifecycle (fixture base)".into(),
            },
        );
    }
    for setting in &set {
        run.extend(["--set".into(), setting.clone().into()]);
    }
    let write_enabled = !set.iter().any(|item| item == "write=false");
    let mut output = String::new();
    launch_phase.0 = clock.mono_now();
    let launched = runner.execute(&run, None, 1_048_576, 3600);
    launch_phase.1 = clock.mono_now();
    match launched {
        Ok(launched) => {
            output.push_str(&String::from_utf8_lossy(&launched));
            wait_phase.0 = clock.mono_now();
            let waited = runner.execute(
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
            wait_phase.1 = clock.mono_now();
            status_phase.0 = clock.mono_now();
            let reported = runner.execute(
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
            status_phase.1 = clock.mono_now();
            output.push_str(
                &waited
                    .as_ref()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .unwrap_or_else(|error| error.clone()),
            );
            let status_text = reported
                .as_ref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_else(|error| error.clone());
            output.push_str(&status_text);
            match (waited, reported) {
                (Ok(_), Ok(status_bytes)) => {
                    if matches!(&verdict, SmokeVerdict::Passed)
                        && let Err(failure) = verify_status(
                            &String::from_utf8_lossy(&status_bytes),
                            &nonce,
                            write_enabled,
                        )
                    {
                        verdict = failure;
                    }
                }
                (Err(error), status) => {
                    verdict = failed(
                        "wait",
                        format!(
                            "{}\nsmoke node report: {}",
                            error,
                            status
                                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                                .unwrap_or_else(|err| err)
                        ),
                    );
                }
                (Ok(_), Err(error)) => {
                    verdict = failed(
                        "status",
                        format!(
                            "{error}\nsmoke launch: {}",
                            String::from_utf8_lossy(&launched)
                        ),
                    );
                }
            }
        }
        Err(error) => {
            verdict = failed("launch", error.clone());
            output.push_str(&error);
        }
    }
    // The peer request still needs the local fixture path to build its
    // locator, so remove remotely first and locally afterwards.
    cleanup_phase.0 = clock.mono_now();
    let peer_cleanup = match host {
        Some(host) => match peer_fixture(state, home, host, &nonce, false, &root) {
            Ok(_) => Ok(()),
            Err(error) if peer_too_old(&error) => Ok(()),
            Err(error) => Err(error),
        },
        None => Ok(()),
    };
    let local_cleanup = remove_fixture(state, home, &nonce, &root);
    let cleanup_result = peer_cleanup.and(local_cleanup);
    cleanup_phase.1 = clock.mono_now();
    if matches!(verdict, SmokeVerdict::Unverified { .. }) {
        warn(
            Warning::new(
                SMOKE_PEER_FIXTURE_UNSUPPORTED,
                "the peer could not verify the smoke fixture",
            )
            .once_per_change(),
        );
    }
    if !warnings.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&warnings.join("\n"));
    }
    let phases = [
        ("fixture", fixture_phase),
        ("launch", launch_phase),
        ("wait", wait_phase),
        ("status", status_phase),
        ("cleanup", cleanup_phase),
    ]
    .into_iter()
    .map(|(name, (begin, end))| SmokePhase {
        name: name.to_owned(),
        elapsed_ms: end.as_micros().saturating_sub(begin.as_micros()) / 1000,
    })
    .collect();
    let outcome = SmokeOutcome {
        verdict,
        phases,
        parameters,
        config,
        output,
    };
    report_smoke_cleanup(Ok(outcome), cleanup_result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_root_is_home_relative() {
        let home = Path::new("/tmp/provenant-home");
        let default = provenant_cli::smoke::Settings::default().root();
        assert_eq!(
            fixture_root(home, &default),
            Path::new("/tmp/provenant-home/.local/state/provenant-smoke")
        );
        let custom = Path::new("scratch/fixtures");
        assert_eq!(fixture_root(home, custom), home.join(custom));
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
            r#"{{"schema_version":1,"command":"status","data":{{"rows":[{{"kind":"node","node_id":"read","state":"succeeded","result_summary":"nonce: {nonce}"}},{{"kind":"node","node_id":"review-write","state":"succeeded","result_summary":"nonce: {nonce}"}},{{"kind":"node","node_id":"write","state":"succeeded"}}]}}}}"#
        );
        verify_status(&good, nonce, true).expect("the nonce is present in every required row");
        let bad = good.replace(&format!("nonce: {nonce}"), "no nonce here");
        let SmokeVerdict::Failed { row, reason } =
            verify_status(&bad, nonce, true).expect_err("a false pass is refused")
        else {
            panic!("a false pass must be a failed verdict");
        };
        assert_eq!(row, "read");
        assert!(reason.contains(nonce), "{reason}");
        let no_review = good.replace(
            "\"node_id\":\"review-write\"",
            "\"node_id\":\"review-fixture\"",
        );
        assert!(
            matches!(
                verify_status(&no_review, nonce, true),
                Err(SmokeVerdict::Failed { ref row, .. }) if row == "review-write"
            ),
            "the wrong reviewer is refused"
        );
        let skipped = good.replace(
            "\"node_id\":\"write\",\"state\":\"succeeded\"",
            "\"node_id\":\"write\",\"state\":\"skipped\"",
        );
        assert!(
            matches!(
                verify_status(&skipped, nonce, true),
                Err(SmokeVerdict::Failed { ref row, .. }) if row == "write"
            ),
            "a skipped writer is not a pass when write is enabled"
        );
    }

    #[test]
    fn verdict_and_phases_are_typed_result_data() {
        let outcome = SmokeOutcome {
            verdict: SmokeVerdict::Failed {
                row: "read".into(),
                reason: "the nonce is missing".into(),
            },
            phases: ["fixture", "launch", "wait", "status", "cleanup"]
                .into_iter()
                .map(|name| SmokePhase {
                    name: name.to_owned(),
                    elapsed_ms: 1,
                })
                .collect(),
            parameters: BTreeMap::from([(
                "write".to_owned(),
                SmokeParameter {
                    value: "false".into(),
                    source: "config.toml (domains.smoke.set.write)".into(),
                },
            )]),
            config: BTreeMap::new(),
            output: "child envelopes".into(),
        };
        let json = outcome.result_json();
        assert_eq!(json["verdict"], "failed");
        assert_eq!(json["row"], "read");
        assert_eq!(json["reason"], "the nonce is missing");
        assert_eq!(json["phases"].as_array().map(Vec::len), Some(5));
        assert_eq!(json["parameters"]["write"]["value"], "false");
        assert_eq!(
            json["parameters"]["write"]["source"],
            "config.toml (domains.smoke.set.write)"
        );
        let text = outcome.text();
        assert!(text.starts_with("child envelopes\n"), "{text}");
        assert!(text.contains("\"verdict\":\"failed\""), "{text}");
        assert_eq!(
            serde_json::from_str::<SmokeVerdict>(r#"{"verdict":"passed"}"#).unwrap(),
            SmokeVerdict::Passed
        );
    }

    #[test]
    fn peer_fixture_mismatch_names_both_recipes() {
        let commits = FixtureCommits {
            base: GitOid::parse(&"11".repeat(20)).unwrap(),
            head: GitOid::parse(&"22".repeat(20)).unwrap(),
        };
        let receipt = SmokeFixtureReceipt {
            extra: Default::default(),
            base: Some(commits.base),
            head: Some(commits.head),
            recipe: Some(FIXTURE_RECIPE),
            reason: None,
        };
        assert!(peer_fixture_matches(&receipt, commits).is_ok());
        let drift = SmokeFixtureReceipt {
            recipe: Some(FIXTURE_RECIPE + 1),
            ..receipt
        };
        let error = peer_fixture_matches(&drift, commits).unwrap_err();
        assert!(error.contains(&FIXTURE_RECIPE.to_string()), "{error}");
        assert!(error.contains(&(FIXTURE_RECIPE + 1).to_string()), "{error}");
    }

    #[test]
    fn fixture_marker_is_written_inside_git() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        std::fs::create_dir(&home)?;
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        let nonce = "0123456789ab";
        let root = Settings::default().root();
        create_fixture(&state, &home, nonce, &root)?;
        let bytes = std::fs::read(
            fixture_root(&home, &root)
                .join(nonce)
                .join(".git")
                .join("provenant-smoke.json"),
        )?;
        let marker: FixtureMarker = serde_json::from_slice(&bytes)?;
        assert_eq!(marker.schema_version, 1);
        assert_eq!(marker.nonce, nonce);
        assert_eq!(marker.recipe, FIXTURE_RECIPE);
        Ok(())
    }

    #[test]
    fn sweep_removes_only_expired_marked_fixtures() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        std::fs::create_dir(&home)?;
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        let fresh = "000000000001";
        let stale = "000000000002";
        let root = Settings::default().root();
        create_fixture(&state, &home, fresh, &root)?;
        create_fixture(&state, &home, stale, &root)?;
        let marker_path = fixture_root(&home, &root)
            .join(stale)
            .join(".git")
            .join("provenant-smoke.json");
        let mut marker: FixtureMarker = serde_json::from_slice(&std::fs::read(&marker_path)?)?;
        marker.created_at = WallTime::from_micros(0)?;
        std::fs::write(&marker_path, serde_json::to_string(&marker)?)?;
        let warnings = sweep_fixtures(&state, &home, &root, 24);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("removed stale fixture") && warning.contains(stale)),
            "{warnings:?}"
        );
        assert!(!fixture_root(&home, &root).join(stale).exists());
        assert!(fixture_root(&home, &root).join(fresh).exists());
        Ok(())
    }

    #[test]
    fn sweep_leaves_foreign_directories_with_a_typed_warning()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        std::fs::create_dir(&home)?;
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        let root = Settings::default().root();
        std::fs::create_dir_all(fixture_root(&home, &root))?;
        let foreign = fixture_root(&home, &root).join("000000000003");
        std::fs::create_dir(&foreign)?;
        let (_, warnings) =
            provenant_records::warning::capture(|| sweep_fixtures(&state, &home, &root, 24));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.code.as_ref() == SMOKE_FIXTURE_FOREIGN.0),
            "{warnings:?}"
        );
        assert!(foreign.exists(), "a foreign directory is never removed");
        Ok(())
    }

    #[test]
    fn a_custom_root_fixture_is_forgotten_and_removed() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        std::fs::create_dir(&home)?;
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        let settings = Settings {
            fixture_root: "scratch/fixtures".into(),
            ..Settings::default()
        };
        let root = settings.root();
        let nonce = "0123456789ab";
        create_fixture(&state, &home, nonce, &root)?;
        let fixture = fixture_root(&home, &root).join(nonce);
        assert!(fixture.exists());
        std::fs::write(fixture.join("smoke.txt"), "written\n")?;
        let location = provenant_config::ProjectLocation::new(fixture.join(".git"))?;
        remove_fixture(&state, &home, nonce, &root)?;
        assert!(!fixture.exists(), "the fixture directory is removed");
        assert!(
            provenant_config::lookup_project(
                &crate::roots::child(&home, &home, state.clone())?,
                &location
            )?
            .is_none(),
            "the fixture binding is forgotten"
        );
        Ok(())
    }

    #[test]
    fn unverified_exit_policy_is_configurable_and_sourced() {
        let origin = SmokeOrigin::compiled(Default::default());
        let unverified = SmokeVerdict::Unverified {
            reason: "peer below protocol".into(),
        };
        let warn = Settings::default();
        assert!(verdict_exit(&unverified, "report", &warn, &origin).is_ok());
        let fail = Settings {
            unverified: Unverified::Fail,
            ..Settings::default()
        };
        let error = verdict_exit(&unverified, "report", &fail, &origin)
            .expect_err("fail makes an unverified result an error");
        assert!(error.contains("report"), "{error}");
        assert!(error.contains(UNVERIFIED_KEY), "{error}");
        assert!(verdict_exit(&SmokeVerdict::Passed, "report", &fail, &origin).is_ok());
        assert!(
            verdict_exit(
                &SmokeVerdict::Failed {
                    row: "read".into(),
                    reason: "no nonce".into()
                },
                "report",
                &warn,
                &origin
            )
            .is_err()
        );
    }

    #[test]
    fn a_zero_sweep_window_leaves_expired_fixtures_in_place()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        std::fs::create_dir(&home)?;
        let state = temp.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        let root = Settings::default().root();
        let nonce = "000000000004";
        create_fixture(&state, &home, nonce, &root)?;
        let marker_path = fixture_root(&home, &root)
            .join(nonce)
            .join(".git")
            .join("provenant-smoke.json");
        let mut marker: FixtureMarker = serde_json::from_slice(&std::fs::read(&marker_path)?)?;
        marker.created_at = WallTime::from_micros(0)?;
        std::fs::write(&marker_path, serde_json::to_string(&marker)?)?;
        assert!(sweep_fixtures(&state, &home, &root, 0).is_empty());
        assert!(fixture_root(&home, &root).join(nonce).exists());
        Ok(())
    }
}
