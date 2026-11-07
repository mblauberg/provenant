#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Process, filesystem and clock boundaries under test.

use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const HEAD: &str = "1111111111111111111111111111111111111111";
const NEXT: &str = "2222222222222222222222222222222222222222";
const BASE: &str = "3333333333333333333333333333333333333333";

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    root: tempfile::TempDir,
    config: PathBuf,
    directory: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let bin = root.path().join("bin");
        fs::create_dir(&bin)?;
        let gh = bin.join("gh");
        fs::write(
            &gh,
            r##"#!/bin/sh
base=${0%/*}
printf '%s\n' "$*" >> "$base/gh-calls"
if [ "$1" != api ]; then
  printf '%s\n' "$*" >> "$base/unsafe-calls"
  exit 90
fi
method=
for arg in "$@"; do
  if [ "$method" = next ] && [ "$arg" != GET ]; then
    printf '%s\n' "$*" >> "$base/unsafe-calls"
    exit 91
  fi
  method=
  case "$arg" in
    --method|-X) method=next ;;
    --method=*|-X?*)
      printf '%s\n' "$*" >> "$base/unsafe-calls"
      exit 92 ;;
  esac
  last=$arg
done
if [ -f "$base/outage" ]; then
  printf 'HTTP/1.1 503 Unavailable\nRetry-After: 120\nX-RateLimit-Remaining: 17\n\n{}'
  exit 0
fi
case "$last" in
  *'pulls?state=open'*) body='[{"number":12}]' ;;
  *'pulls?state=closed'*) body='[]' ;;
  */pulls/12) body=$(cat "$base/pr.json") ;;
  *'check-runs?'*) body=$(cat "$base/checks.json") ;;
  *'status?'*) body='{"statuses":[]}' ;;
  *'comments?'*|*'reviews?'*) body='[]' ;;
  *) printf '%s\n' "$*" >> "$base/unexpected-calls"; exit 93 ;;
esac
printf 'HTTP/1.1 200 OK\nETag: "fake"\nX-RateLimit-Remaining: 4999\n\n%s' "$body"
"##,
        )?;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700))?;
        for name in ["provenant", "launchctl"] {
            let trap = bin.join(name);
            fs::write(
                &trap,
                "#!/bin/sh\nbase=${0%/*}\nprintf '%s %s\\n' \"$0\" \"$*\" >> \"$base/unsafe-calls\"\nexit 94\n",
            )?;
            fs::set_permissions(trap, fs::Permissions::from_mode(0o700))?;
        }
        let config = root.path().join("settings.json");
        let directory = root.path().join("state");
        let settings = json!({
            "schema_version": 1,
            "repository": "o/r",
            "directory": directory,
            "checkout": root.path(),
            "executable": bin.join("provenant"),
            "worker": env!("CARGO_BIN_EXE_pr-reconciler"),
            "path": format!("{}:/usr/bin:/bin", bin.display()),
            "sweep_seconds": 300,
            "poll_seconds": 1,
            "mode": "observe",
            "policy_digest": "policy",
            "base_branch": "main",
            "bind": null,
        });
        fs::write(&config, serde_json::to_vec(&settings)?)?;
        let fixture = Self {
            root,
            config,
            directory,
        };
        fixture.head(HEAD)?;
        Ok(fixture)
    }

    fn head(&self, head: &str) -> Result {
        fs::write(
            self.root.path().join("bin/pr.json"),
            serde_json::to_vec(&json!({
                "number": 12,
                "state": "open",
                "draft": false,
                "head": {"sha": head, "repo": {"full_name": "o/r"}},
                "base": {"ref": "main", "sha": BASE, "repo": {"full_name": "o/r"}},
                "user": {"login": "author"},
                "labels": [{"name": "risk:substantial"}],
            }))?,
        )?;
        fs::write(
            self.root.path().join("bin/checks.json"),
            serde_json::to_vec(&json!({"check_runs": [{
                "id": 1,
                "name": "ci-status",
                "head_sha": head,
                "status": "completed",
                "conclusion": "success",
            }]}))?,
        )?;
        Ok(())
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pr-reconciler"));
        command.arg("--config").arg(&self.config);
        command.env_remove("PROVENANT_INTAKE_SECRET");
        command
    }

    fn once(&self) -> Result<Output> {
        let output = self.command().arg("--once").output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(output)
    }

    fn report(&self) -> Result<Value> {
        Ok(serde_json::from_slice(&fs::read(
            self.directory.join("report.json"),
        )?)?)
    }

    fn no_effects(&self) -> Result {
        for name in ["unsafe-calls", "unexpected-calls"] {
            let path = self.root.path().join("bin").join(name);
            assert!(
                !path.exists(),
                "{name}: {}",
                fs::read_to_string(path).unwrap_or_default()
            );
        }
        Ok(())
    }
}

#[test]
fn observation_restarts_refresh_heads_and_status_stays_offline() -> Result {
    let fixture = Fixture::new()?;
    fixture.once()?;
    let first = fixture.report()?;
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["repository"], "o/r");
    assert_eq!(first["mode"], "observe");
    assert_eq!(first["boots"], 1);
    assert_eq!(first["effect_calls"], 0);
    assert_eq!(first["decisions"][0]["pr"], 12);
    assert_eq!(first["decisions"][0]["head"], HEAD);
    assert_eq!(first["decisions"][0]["action"], "review");
    assert_eq!(first["digest"]["transitions"], 1);

    fixture.once()?;
    let second = fixture.report()?;
    assert_eq!(second["boots"], 2);
    assert_eq!(second["decisions"], first["decisions"]);
    assert!(second["observations"].as_u64() > first["observations"].as_u64());
    assert_eq!(second["digest"]["transitions"], 1);

    fixture.head(NEXT)?;
    fixture.once()?;
    let moved = fixture.report()?;
    assert_eq!(moved["boots"], 3);
    assert_eq!(moved["decisions"][0]["head"], NEXT);
    assert_eq!(moved["decisions"][0]["action"], "review");
    assert_eq!(moved["effect_calls"], 0);
    assert_eq!(moved["digest"]["transitions"], 2);
    let day = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() / 86400;
    assert!(
        fixture
            .directory
            .join("digests")
            .join(format!("{day}.json"))
            .is_file()
    );

    let calls = fixture.root.path().join("bin/gh-calls");
    let before = fs::read(&calls)?;
    let status = fixture.command().arg("--status").output()?;
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(status["boots"], 3);
    assert_eq!(status["decisions"], moved["decisions"]);
    assert_eq!(fs::read(calls)?, before);
    fixture.no_effects()
}

#[test]
fn only_one_worker_owns_state_and_a_stopped_worker_can_restart() -> Result {
    let fixture = Fixture::new()?;
    let worker = Worker(
        fixture
            .command()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    provenant_test_support::wait_for(
        "observe-only worker startup",
        Duration::from_secs(20),
        || fixture.report().ok(),
        |report| report.as_ref().is_some_and(|value| value["boots"] == 1),
    )?;
    let competing = fixture.command().arg("--once").output()?;
    assert!(!competing.status.success());
    assert_eq!(fixture.report()?["boots"], 1);
    drop(worker);
    fixture.once()?;
    assert_eq!(fixture.report()?["boots"], 2);
    fixture.no_effects()
}

#[test]
fn launchd_generation_is_offline_and_act_mode_refuses_before_forge_access() -> Result {
    let fixture = Fixture::new()?;
    let worker_version = Command::new(env!("CARGO_BIN_EXE_pr-reconciler"))
        .args(["--version", "--json"])
        .output()?;
    let main_version = Command::new(env!("CARGO_BIN_EXE_provenant"))
        .args(["--version", "--json"])
        .output()?;
    assert!(worker_version.status.success());
    assert!(main_version.status.success());
    let worker_version: Value = serde_json::from_slice(&worker_version.stdout)?;
    let main_version: Value = serde_json::from_slice(&main_version.stdout)?;
    assert_eq!(worker_version["data"]["binary"], "pr-reconciler");
    for field in [
        "package_version",
        "source_digest",
        "asset_digest",
        "target",
        "toolchain",
        "record_schema_version",
        "config_schema_version",
    ] {
        assert!(!worker_version["data"][field].is_null(), "{field}");
        assert_eq!(worker_version["data"][field], main_version["data"][field]);
    }
    let generated = fixture.command().arg("--launchd").output()?;
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let plist = String::from_utf8(generated.stdout)?;
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("<true/>"));
    assert!(plist.contains("<key>ThrottleInterval</key>"));
    assert!(plist.contains("<key>ProgramArguments</key>"));
    assert!(plist.contains("--config"));
    assert!(!fixture.directory.exists());
    assert!(!fixture.root.path().join("bin/gh-calls").exists());

    let mut settings: Value = serde_json::from_slice(&fs::read(&fixture.config)?)?;
    settings["mode"] = "act".into();
    fs::write(&fixture.config, serde_json::to_vec(&settings)?)?;
    let refused = fixture.command().arg("--once").output()?;
    assert!(!refused.status.success());
    assert!(!fixture.root.path().join("bin/gh-calls").exists());
    assert!(!fixture.directory.exists());
    fixture.no_effects()
}

#[test]
fn failed_sweeps_report_live_intake_metrics_without_promoting_freshness() -> Result {
    use provenant::event_intake::{Admission, Intake};
    use std::os::unix::fs::DirBuilderExt;

    // Cover a first-boot outage as well as an outage following a good snapshot.
    for preceding_success in [false, true] {
        let fixture = Fixture::new()?;
        if preceding_success {
            fixture.once()?;
        } else {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&fixture.directory)?;
        }
        let before = if preceding_success {
            fixture.report()?
        } else {
            json!({"observations":0,"intake":{"requests":0},"decisions":[]})
        };
        let started = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let received = started - 10;
        let mut intake = Intake::open(
            fixture.directory.join("deliveries.json"),
            "o/r".into(),
            b"secret".to_vec(),
        )?;
        assert_eq!(
            intake.admit(
                "pending-during-outage",
                "pull_request",
                Some("sha256=db4061419529a8addfd7db7f148ff8677579ec4723d46ba890f9c7c80f8629ab"),
                br#"{"repository":{"full_name":"o/r"},"pull_request":{"number":12}}"#,
                received,
            )?,
            Admission::Accepted
        );
        fs::write(fixture.root.path().join("bin/outage"), "")?;
        let failed = fixture.command().arg("--once").output()?;
        assert!(!failed.status.success());
        let after = fixture.report()?;
        assert_eq!(after["errors"], 1);
        assert_eq!(after["digest"]["errors"], 1);
        assert_eq!(after["effect_calls"], 0);
        assert_eq!(after["observations"], before["observations"]);
        assert_eq!(after["decisions"], before["decisions"]);
        assert_eq!(after["first_observed_at"], before["first_observed_at"]);
        assert_eq!(after["observed_at"], before["observed_at"]);
        assert_eq!(
            after["intake"]["last_successful_sweep"],
            before["intake"]["last_successful_sweep"]
        );
        assert_eq!(
            after["intake"]["requests"].as_u64(),
            before["intake"]["requests"].as_u64().map(|n| n + 1)
        );
        assert!(
            after["intake"]["retry_at"]
                .as_u64()
                .is_some_and(|at| at >= started + 120)
        );
        assert_eq!(after["intake"]["rate_limit"]["x-ratelimit-remaining"], "17");
        assert_eq!(after["intake"]["oldest_pending_marker"], received);
        let deliveries: Value =
            serde_json::from_slice(&fs::read(fixture.directory.join("deliveries.json"))?)?;
        assert_eq!(
            deliveries["deliveries"]["pending-during-outage"]["pending"],
            true
        );
        fixture.no_effects()?;
    }
    Ok(())
}
