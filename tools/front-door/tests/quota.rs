mod support;
use support::{Result, TestRig, code};

#[test]
fn usage_reads_terminal_lanes_across_projects_and_keeps_unknown_accounts() -> Result {
    let mut rig = TestRig::new()?;
    let original = rig.project.clone();
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_provenant"));
    for (project, operation) in [
        (original.clone(), "first"),
        (rig.root.join("second"), "second"),
    ] {
        std::fs::create_dir_all(&project)?;
        rig.project = project;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        assert_eq!(code(&rig.execute(&git, &["init", "-q"], None)?), 0);
        let output = rig.execute(
            binary,
            &[
                "dispatch",
                "--prompt-stdin",
                "--model",
                "codex/gpt-6-luna",
                "--operation-id",
                operation,
                "--json",
            ],
            Some(b"private fixture prompt"),
        )?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stdout.bytes)
        );
    }
    rig.project = original;
    let output = rig.command(&["quota", "usage", "--json"])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    let rows = report["data"]["rows"].as_array().ok_or("rows")?;
    assert_eq!(rows.len(), 2, "{report}");
    assert_ne!(rows[0]["project"], rows[1]["project"]);
    for row in rows {
        assert_eq!(row["lane_attempts"], 1);
        assert_eq!(row["chair_sessions"], 0);
        assert_eq!(row["chair_sessions_unread"], 0);
        assert!(row["account"].is_null());
        assert_eq!(row["lane_usage"]["uncached_input"]["missing"], 1);
    }
    assert!(!String::from_utf8_lossy(&output.stdout.bytes).contains("private fixture prompt"));
    let human = rig.command(&["quota", "usage", "--full"])?;
    let human = String::from_utf8(human.stdout.bytes)?;
    assert!(human.contains("Chair tokens are read from each recorded chair session"));
    assert!(human.contains("unknown account"));
    assert!(human.contains("records missing this category"));
    assert!(human.contains("lanes cache-read share: not reported"));
    assert!(!rig.state.join("quota").exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn usage_with_missing_state_creates_nothing_and_rejects_invalid_windows() -> Result {
    let rig = TestRig::new()?.without_state()?;
    let output = rig.command(&["quota", "usage", "--json"])?;
    assert_eq!(code(&output), 0);
    assert!(!rig.state.exists());
    for days in ["0", "366"] {
        assert_eq!(
            code(&rig.command(&["quota", "usage", "--days", days, "--json"])?),
            2
        );
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn tick_does_not_create_missing_state_ancestors() -> Result {
    let mut rig = quota_rig()?;
    let missing = rig.root.join("missing");
    rig.state = missing.join("state");
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 3);
    assert!(!missing.exists());
    rig.assert_legacy_unchanged()
}

#[test]
fn fabric_and_quota_load_one_domain_config() -> Result {
    let rig = quota_rig()?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 0);
    std::fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[domains.fabric]\nclient_label = 'combined'\n[domains.quota]\nmax_parallel = 1\n",
    )?;
    for args in [
        &["quota", "show", "--json"][..],
        &["quota", "tick", "--json"],
        &["fabric", "identity", "--json"],
    ] {
        let out = rig.command(args)?;
        assert_eq!(
            code(&out),
            0,
            "{}",
            String::from_utf8_lossy(&out.stdout.bytes)
        );
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn show_is_read_only_and_tick_publishes_partial_coverage() -> Result {
    let rig = quota_rig()?;
    let shown = rig.command(&["quota", "show", "--json"])?;
    assert_eq!(code(&shown), 0);
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout.bytes)?;
    assert_eq!(shown["command"], "quota show");
    assert_eq!(shown["data"]["coverage"]["known"], 0);
    assert_eq!(shown["data"]["accounts"][0]["account"]["adapter"], "codex");
    assert!(!rig.state.join("quota").exists());
    let tick = rig.command(&["quota", "tick", "--json"])?;
    assert_eq!(
        code(&tick),
        0,
        "{} {}",
        String::from_utf8_lossy(&tick.stdout.bytes),
        String::from_utf8_lossy(&tick.stderr.bytes)
    );
    let tick: serde_json::Value = serde_json::from_slice(&tick.stdout.bytes)?;
    assert_eq!(tick["command"], "quota tick");
    // Only registered adapters count toward coverage (#1810).
    assert_eq!(tick["data"]["coverage"]["total"], 1);
    assert_eq!(tick["data"]["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(tick["data"]["accounts"][0]["status"], "unknown");
    assert_eq!(tick["data"]["accounts"][0]["error"], "missing_cli");
    let sources = tick["data"]["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 4);
    for source in sources {
        assert_ne!(source["status"], "unknown", "{source}");
        assert_eq!(source["error"], "disabled", "{source}");
    }
    let disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(rig.state.join("quota/report.json"))?)?;
    assert_eq!(disk, tick["data"]);
    rig.assert_legacy_unchanged()
}

#[test]
fn disabled_modes_never_read_auth_or_invoke_external_tools() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let mut rig = quota_rig()?;
    for key in [
        "XDG_DATA_HOME",
        "XDG_RUNTIME_DIR",
        "CURSOR_HOME",
        "AGY_HOME",
    ] {
        let path = rig.home.join(key);
        fs::create_dir_all(&path)?;
        rig.env.push((key.into(), path.into_os_string()));
    }
    let cursor = rig
        .home
        .join("Library/Application Support/Cursor/User/globalStorage");
    fs::create_dir_all(&cursor)?;
    let database = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/quota/expired.vscdb"
    ));
    fs::write(cursor.join("state.vscdb"), database)?;
    fs::write(
        rig.root.join("claude/.credentials.json"),
        br#"{"claudeAiOauth":{"accessToken":"fixture-secret"}}"#,
    )?;
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    let marker = rig.root.join("forbidden-invocation");
    // `quota tick` may launch the detached provider-update worker. Whether that
    // worker updates anything is a separate contract, so opt every provider out
    // and leave this test to what the quota commands themselves invoke.
    let config = rig.state.join("config.toml");
    let mut text = fs::read_to_string(&config)?;
    for provider in ["agy", "claude", "codex", "cursor", "opencode"] {
        text.push_str(&format!("\n[providers.{provider}]\nupdate = false\n"));
    }
    fs::write(&config, text)?;
    for tool in [
        "curl", "security", "claude", "codex", "cursor", "agy", "opencode",
    ] {
        let path = bin.join(tool);
        fs::write(
            &path,
            b"#!/bin/sh\nprintf forbidden > \"$QUOTA_MARKER\"\nexit 90\n",
        )?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    rig.env.retain(|(k, _)| k != "PATH");
    rig.env.push(("PATH".into(), bin.as_os_str().to_owned()));
    rig.env
        .push(("QUOTA_MARKER".into(), marker.as_os_str().to_owned()));
    for args in [
        &["quota", "show", "--json"][..],
        &["quota", "--json"],
        &["quota", "tick", "--power", "low", "--json"],
        &["quota", "--help"],
    ] {
        let out = rig.command(args)?;
        assert_eq!(code(&out), 0);
        assert!(!String::from_utf8_lossy(&out.stdout.bytes).contains("fixture-secret"));
        assert!(!String::from_utf8_lossy(&out.stderr.bytes).contains("fixture-secret"));
    }
    assert!(!marker.exists());
    assert_eq!(fs::read(cursor.join("state.vscdb"))?, database);
    for args in [
        &["quota", "forget", "opaque", "--json"][..],
        &["quota", "show", "--adapter", "bogus", "--json"],
    ] {
        assert_eq!(code(&rig.command(args)?), 2);
    }
    rig.assert_legacy_unchanged()
}

#[test]
fn malformed_records_and_unsafe_directories_fail_without_replacing_report() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let rig = quota_rig()?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 0);
    let report = rig.state.join("quota/report.json");
    let before = fs::read(&report)?;
    let account = rig.state.join("quota/opaque");
    fs::create_dir(&account)?;
    fs::set_permissions(&account, fs::Permissions::from_mode(0o700))?;
    fs::write(
        account.join("claude.json"),
        br#"{"schema_version":1,"schema_version":1}"#,
    )?;
    fs::set_permissions(
        account.join("claude.json"),
        fs::Permissions::from_mode(0o600),
    )?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 2);
    assert_eq!(fs::read(&report)?, before);
    fs::remove_file(account.join("claude.json"))?;
    fs::set_permissions(rig.state.join("quota"), fs::Permissions::from_mode(0o755))?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 3);
    assert_eq!(fs::read(report)?, before);
    rig.assert_legacy_unchanged()
}

fn quota_rig() -> Result<TestRig> {
    let mut rig = TestRig::new()?;
    let auth = serde_json::to_vec(&serde_json::json!({
        "tokens": {"account_id": "quota-test"}
    }))?;
    let key = provenant_providers::identity::codex_account(&auth).ok_or("Codex fixture key")?;
    std::fs::write(rig.root.join("codex/auth.json"), auth)?;
    support::register_account(&rig, "codex", "default", &key, &rig.root.join("codex"))?;
    for key in [
        "XDG_DATA_HOME",
        "XDG_RUNTIME_DIR",
        "CURSOR_HOME",
        "AGY_HOME",
    ] {
        let path = rig.home.join(key);
        std::fs::create_dir_all(&path)?;
        rig.env.push((key.into(), path.into_os_string()));
    }
    Ok(rig)
}

#[test]
fn configuration_cannot_raise_probe_limits_and_publication_failure_is_io() -> Result {
    use std::fs;
    let rig = quota_rig()?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 0);
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[domains.quota]\nmax_parallel = 4\n",
    )?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 2);
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[domains.quota]\nmax_parallel = 1\nwall_budget_seconds = 1\n",
    )?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 0);
    let path = rig.state.join("quota/report.json");
    fs::remove_file(&path)?;
    fs::create_dir(&path)?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 3);
    assert!(path.is_dir());
    rig.assert_legacy_unchanged()
}

#[test]
fn oversized_show_returns_a_bounded_envelope_and_filter_remains_usable() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let rig = quota_rig()?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 0);
    let windows = (0..64).map(|i| serde_json::json!({"pool":format!("{i:02}{}", "p".repeat(250)),"used_basis_points":0,"resets_unix_seconds":null})).collect::<Vec<_>>();
    // Leave one slot for the configured Codex account.
    for i in 0..127 {
        let key = format!("account-{i}");
        let label = format!("work-{i}");
        support::register_account(&rig, "claude", &label, &key, &rig.root.join(&key))?;
        let account = serde_json::json!({"adapter":"claude","account_key":key,"label":label});
        let record = serde_json::json!({"schema_version":1,"account":account,"revision":1,
            "snapshot":{"schema_version":1,"account":account,"revision":1,"status":"available","observed_unix_seconds":1,"source_ref":"fixture","authority_ref":null,"windows":windows},
            "claim":null,"last_attempt_unix_seconds":1,"backoff_until_unix_seconds":null,"failures":0,"last_error":null,"refusals":[]});
        let dir = rig.state.join("quota").join(&key);
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let path = dir.join("claude.json");
        fs::write(&path, serde_json::to_vec(&record)?)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let out = rig.command(&["quota", "show", "--json"])?;
    assert_eq!(code(&out), 3);
    assert!(out.stdout.bytes.len() < 1024);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert_eq!(value["error"]["code"], "RESULT_TOO_LARGE");
    assert_eq!(
        code(&rig.command(&["quota", "show", "--adapter", "codex", "--json"])?),
        0
    );
    let account = rig.state.join("quota/account-0");
    fs::set_permissions(
        account.join("claude.json"),
        fs::Permissions::from_mode(0o644),
    )?;
    assert_eq!(code(&rig.command(&["quota", "show", "--json"])?), 3);
    fs::set_permissions(
        account.join("claude.json"),
        fs::Permissions::from_mode(0o600),
    )?;
    fs::set_permissions(account, fs::Permissions::from_mode(0o755))?;
    assert_eq!(code(&rig.command(&["quota", "show", "--json"])?), 3);
    rig.assert_legacy_unchanged()
}

#[test]
fn internal_lock_deadline_is_contention_and_keeps_the_report() -> Result {
    use provenant_core::{CancelToken, Clock, WaitBudget};
    use provenant_os::clock::SystemClock;
    use provenant_os::fs::Directory;
    use provenant_os::fs::LockMode;
    let rig = quota_rig()?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 0);
    let before = std::fs::read(rig.state.join("quota/report.json"))?;
    let dir = Directory::open_anchor(&rig.state.join("quota"))?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let _held = dir.lock(
        std::ffi::OsStr::new("quota.lock"),
        LockMode::Exclusive,
        WaitBudget::cli(clock.mono_now(), 1)?,
        &cancel,
        &clock,
    )?;
    let out = rig.command(&["quota", "tick", "--json"])?;
    assert_eq!(code(&out), 3);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert_eq!(value["status"], "error");
    assert_eq!(std::fs::read(rig.state.join("quota/report.json"))?, before);
    rig.assert_legacy_unchanged()
}

#[test]
fn failed_auth_refresh_shows_last_known_usage_age_and_auth_marker() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let rig = quota_rig()?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 0);
    // The report lists only registered accounts (#1810).
    support::register_account(
        &rig,
        "claude",
        "pro",
        "pro-account",
        &rig.root.join("claude-pro"),
    )?;
    let directory = rig.state.join("quota/pro-account");
    fs::create_dir(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let now = jiff_time()?;
    let account = serde_json::json!({"adapter":"claude","account_key":"pro-account","label":"pro"});
    let record = serde_json::json!({
        "schema_version":1,"account":account,"revision":2,
        "snapshot":{"schema_version":1,"account":account,"revision":1,"status":"available",
          "observed_unix_seconds":now-10,"source_ref":"fixture","authority_ref":null,
          "windows":[{"pool":"weekly\n\u{1b}[2J","used_basis_points":5300,"resets_unix_seconds":null}]},
        "claim":null,"last_attempt_unix_seconds":now-5,"backoff_until_unix_seconds":null,
        "failures":1,"last_error":"auth","refusals":[]
    });
    let path = directory.join("claude.json");
    fs::write(&path, serde_json::to_vec(&record)?)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let out = rig.command(&["quota", "show", "--json"])?;
    assert_eq!(code(&out), 0);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    let row = &value["data"]["accounts"][0];
    assert_eq!(row["status"], "auth_required");
    assert!(row["age_seconds"].as_u64().is_some_and(|age| age >= 10));
    assert_eq!(row["snapshot"]["windows"][0]["used_basis_points"], 5300);
    let compact = rig.command(&["quota", "show"])?;
    let compact = String::from_utf8(compact.stdout.bytes)?;
    assert!(compact.contains("pro [claude]: auth_required"), "{compact}");
    assert!(compact.contains("next: provenant quota tick"), "{compact}");
    let human = rig.command(&["quota", "show", "--full"])?;
    let text = String::from_utf8(human.stdout.bytes)?;
    for expected in [
        "pro",
        "AUTH_REQUIRED",
        "last known",
        "53.00%",
        "age",
        "auth",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("\\n"));
    rig.assert_legacy_unchanged()
}

fn jiff_time() -> Result<i64> {
    use provenant_core::Clock;
    Ok(provenant_os::clock::SystemClock::new()
        .wall_now()?
        .as_timestamp()
        .as_second())
}

#[test]
fn agy_registration_and_quota_follow_the_google_identity() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let mut rig = quota_rig()?;
    support::clear_accounts(&rig)?;
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    let agy = bin.join("agy");
    fs::write(
        &agy,
        r#"#!/bin/sh
pwd -P > "$HOME/agy-cwd"
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--log-file" ]; then shift; log=$1; fi
    shift
done
printf 'OAuth: authenticated successfully as %s\n' "$(cat "$HOME/agy-email")" > "$log"
cat "$HOME/agy-usage"
"#,
    )?;
    fs::set_permissions(&agy, fs::Permissions::from_mode(0o700))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../crates/providers/src/quota/fixtures/agy.json"
    ))?;
    fs::write(
        rig.home.join("agy-usage"),
        serde_json::to_vec(&fixture["payload"])?,
    )?;
    let register = || -> Result<serde_json::Value> {
        let out = rig.command(&[
            "accounts",
            "register",
            "work",
            "--adapter",
            "agy",
            "--directory",
            rig.home.to_str().ok_or("home path")?,
            "--json",
        ])?;
        assert_eq!(
            code(&out),
            0,
            "{}",
            String::from_utf8_lossy(&out.stdout.bytes)
        );
        let value: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert!(!String::from_utf8_lossy(&out.stdout.bytes).contains("@example.test"));
        Ok(value["data"]["entries"]["agy-work"]["key"].clone())
    };
    fs::write(rig.home.join("agy-email"), "one@example.test")?;
    let one = register()?.as_str().ok_or("account key")?.to_owned();
    let fingerprint = provenant_providers::identity::fingerprint("one@example.test")
        .ok_or("fixture fingerprint")?;
    assert_eq!(one, format!("agy-google-{}", &fingerprint[..12]));
    let stale = rig.state.join("probe-0123456789abcdef01234567");
    let stale_lock = rig.state.join("probe-0123456789abcdef01234567.lock");
    fs::create_dir(&stale)?;
    fs::write(&stale_lock, b"")?;
    let out = rig.command(&["quota", "tick", "--adapter", "agy", "--json"])?;
    assert_eq!(code(&out), 0);
    let cwd = fs::read_to_string(rig.home.join("agy-cwd"))?;
    let cwd = std::path::Path::new(cwd.trim());
    assert_eq!(cwd.parent(), Some(fs::canonicalize(&rig.state)?.as_path()));
    assert!(
        cwd.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("probe-"))
    );
    assert!(!stale.exists());
    assert!(!stale_lock.exists());
    for entry in fs::read_dir(&rig.state)? {
        assert!(!entry?.file_name().to_string_lossy().starts_with("probe-"));
    }
    let record_path = rig.state.join("quota").join(&one).join("agy.json");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&record_path)?)?;
    assert_eq!(
        record["snapshot"]["details"]["fingerprint"],
        format!("google-{}", &fingerprint[..12])
    );
    let at = jiff_time()?;
    record["refusals"] = serde_json::json!([{
        "account": record["account"], "model": "fixture-model", "pool": null,
        "observed_unix_seconds": at, "resets_unix_seconds": at + 300
    }]);
    fs::write(&record_path, serde_json::to_vec(&record)?)?;

    fs::write(rig.home.join("agy-email"), "two@example.test")?;
    let two = register()?.as_str().ok_or("second account key")?.to_owned();
    assert_ne!(one, two);
    assert_eq!(
        code(&rig.command(&["quota", "tick", "--adapter", "agy", "--json"])?),
        0
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&record_path)?)?,
        record
    );
    let second: serde_json::Value = serde_json::from_slice(&fs::read(
        rig.state.join("quota").join(&two).join("agy.json"),
    )?)?;
    assert_eq!(second["account"]["account_key"], two);
    assert_eq!(second["refusals"], serde_json::json!([]));
    fs::write(rig.home.join("agy-email"), "one@example.test")?;
    assert_eq!(register()?, one);
    assert_eq!(
        code(&rig.command(&["quota", "tick", "--adapter", "agy", "--json"])?),
        0
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&record_path)?)?,
        record
    );

    // A changed sign-in must never publish usage into the registered scope.
    record["last_attempt_unix_seconds"] = serde_json::json!(at - 60);
    fs::write(&record_path, serde_json::to_vec(&record)?)?;
    fs::write(rig.home.join("agy-email"), "two@example.test")?;
    assert_eq!(
        code(&rig.command(&["quota", "tick", "--adapter", "agy", "--json"])?),
        0
    );
    let refused: serde_json::Value = serde_json::from_slice(&fs::read(&record_path)?)?;
    assert_eq!(refused["last_error"], "auth");
    assert_eq!(refused["snapshot"], record["snapshot"]);

    // Missing identity cannot silently register a label-based account.
    fs::write(rig.home.join("agy-email"), "")?;
    let out = rig.command(&[
        "accounts",
        "register",
        "missing",
        "--adapter",
        "agy",
        "--directory",
        rig.home.to_str().ok_or("home path")?,
        "--json",
    ])?;
    assert_ne!(code(&out), 0);
    rig.assert_legacy_unchanged()
}

#[test]
fn registered_accounts_have_distinct_quota_keys() -> Result {
    use std::fs;
    let rig = quota_rig()?;
    support::clear_accounts(&rig)?;
    let mut registrations = Vec::new();
    for provider in ["codex", "cursor"] {
        for (name, identity) in [
            ("one", "fixture-account-one"),
            ("two", "fixture-account-two"),
            ("missing", ""),
        ] {
            let dir = rig.home.join(format!("{provider}-{name}"));
            fs::create_dir(&dir)?;
            if !identity.is_empty() {
                if provider == "codex" {
                    fs::write(
                        dir.join("auth.json"),
                        serde_json::to_vec(&serde_json::json!({"tokens":{"account_id":identity}}))?,
                    )?;
                } else {
                    let db = rusqlite::Connection::open(dir.join("state.vscdb"))?;
                    db.execute(
                        "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
                        [],
                    )?;
                    let token = match name {
                        "one" => {
                            "fixture.eyJzdWIiOiJhdXRoaDB8Zml4dHVyZS1hY2NvdW50LW9uZSIsImV4cCI6NDAwMDAwMDAwMH0.signature"
                        }
                        _ => {
                            "fixture.eyJzdWIiOiJhdXRoaDB8Zml4dHVyZS1hY2NvdW50LXR3byIsImV4cCI6NDAwMDAwMDAwMH0.signature"
                        }
                    };
                    db.execute(
                        "INSERT INTO ItemTable VALUES ('cursorAuth/accessToken', ?1)",
                        [token],
                    )?;
                }
            }
            let key = if identity.is_empty() {
                format!("{provider}-missing")
            } else if provider == "codex" {
                provenant_providers::identity::codex_account(&serde_json::to_vec(
                    &serde_json::json!({"tokens":{"account_id":identity}}),
                )?)
                .ok_or("Codex account key")?
            } else {
                let token = match name {
                    "one" => {
                        "fixture.eyJzdWIiOiJhdXRoaDB8Zml4dHVyZS1hY2NvdW50LW9uZSIsImV4cCI6NDAwMDAwMDAwMH0.signature"
                    }
                    _ => {
                        "fixture.eyJzdWIiOiJhdXRoaDB8Zml4dHVyZS1hY2NvdW50LXR3byIsImV4cCI6NDAwMDAwMDAwMH0.signature"
                    }
                };
                provenant_providers::identity::cursor_account(token).ok_or("cursor account key")?
            };
            registrations.push((provider, name, key, dir));
        }
    }
    for (provider, label, key, dir) in registrations {
        support::register_account(&rig, provider, label, &key, &dir)?;
    }
    let out = rig.command(&["quota", "show", "--json"])?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout.bytes)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    let rows = report["data"]["accounts"].as_array().unwrap();
    for provider in ["codex", "cursor"] {
        let rows: Vec<_> = rows
            .iter()
            .filter(|r| r["account"]["adapter"] == provider)
            .collect();
        assert_eq!(rows.len(), 3);
        let keys: std::collections::BTreeSet<_> = rows
            .iter()
            .map(|r| r["account"]["account_key"].as_str().unwrap())
            .collect();
        assert_eq!(keys.len(), 3);
        for key in keys {
            assert!(key.starts_with(&format!("{provider}-")));
        }
        use provenant_quota::{
            AccountRecord, AccountScope, Admission, ProbeCapture, QuotaProbeInput, Refusal,
        };
        use provenant_records::{
            ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
        };
        let known: Vec<AccountScope> = rows
            .iter()
            .map(|r| serde_json::from_value(r["account"].clone()).unwrap())
            .collect();
        let at = jiff_time()?;
        let input = QuotaProbeInput {
            now_unix_seconds: at,
            deadline_unix_seconds: at + 30,
            authority_ref: Some("fixture".into()),
        };
        let fixture = if provider == "codex" {
            include_str!("../crates/providers/src/quota/fixtures/codex-app-server.json")
        } else {
            include_str!("../crates/providers/src/quota/fixtures/cursor.json")
        };
        for account in &known {
            let plan = provenant_providers::quota::plan(provider, account, &input)?;
            let snapshot = provenant_providers::quota::decode(
                &plan,
                &ProbeCapture {
                    stdout: fixture.as_bytes().to_vec(),
                    stderr: vec![],
                    exit_code: Some(0),
                    overflowed: false,
                },
            )?;
            assert_eq!(&snapshot.account, account);
        }
        let mut record = AccountRecord::empty(known[0].clone());
        record.refusals.push(Refusal {
            extra: Default::default(),
            account: known[0].clone(),
            model: "fixture-model".into(),
            pool: None,
            observed_unix_seconds: at,
            resets_unix_seconds: Some(at + 300),
        });
        let records = [record];
        let admission = Admission {
            policies: &provenant_providers::quota_policies(provenant_providers::REGISTRY),
            now_unix_seconds: at,
            records: &records,
            pools: &[],
        };
        let mut route = ResolvedRoute {
            extra: Default::default(),
            adapter: provider.into(),
            model: "fixture-model".into(),
            effort: None,
            catalogue_revision: "fixture".into(),
            account_key: None,
            host: "fixture".into(),
            alternatives: Default::default(),
            exclusions: vec![],
            eligibility: RouteEligibility::Eligible,
            readiness: RouteReadiness::Ready {
                extra: Default::default(),
                availability_ref: "fixture".into(),
                expires_unix_seconds: at + 60,
            },
            confidentiality: RouteConfidentiality {
                extra: Default::default(),
                evidence_ref: "fixture".into(),
            },
            pool_selection: None,
        };
        for (account, allowed) in [(&known[0], false), (&known[1], true), (&known[0], false)] {
            route.account_key = Some(account.account_key.clone());
            assert_eq!(admission.check(&route).is_ok(), allowed);
        }
    }
    assert!(!String::from_utf8_lossy(&out.stdout.bytes).contains("fixture-account"));
    rig.assert_legacy_unchanged()
}

fn json(out: &provenant_os::exec::ExecOutcome) -> Result<serde_json::Value> {
    Ok(serde_json::from_slice(&out.stdout.bytes)?)
}

#[test]
fn show_carries_display_blocks_and_display_command_persists() -> Result {
    let rig = quota_rig()?;
    let shown = json(&rig.command(&["quota", "show", "--json"])?)?;
    assert_eq!(
        shown["data"]["display"],
        serde_json::json!({"headline": "auto", "icon": "most_constrained"})
    );
    assert_eq!(
        shown["data"]["accounts"][0]["display"],
        serde_json::json!({"id": "codex-default", "hidden": false})
    );
    let set = rig.command(&[
        "accounts",
        "set",
        "default",
        "--display-name",
        "Work",
        "--order",
        "2",
        "--hidden",
        "true",
        "--headline",
        "weekly",
        "--json",
    ])?;
    assert_eq!(
        code(&set),
        0,
        "{}",
        String::from_utf8_lossy(&set.stdout.bytes)
    );
    let out = rig.command(&[
        "quota",
        "display",
        "--headline",
        "weekly",
        "--icon",
        "default",
        "--json",
    ])?;
    assert_eq!(
        code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout.bytes)
    );
    let out = json(&out)?;
    assert_eq!(out["command"], "quota display");
    assert_eq!(
        out["data"]["display"],
        serde_json::json!({"headline": "weekly", "icon": "codex-default"})
    );
    assert_eq!(
        out["data"]["accounts"][0]["display"],
        serde_json::json!({"id": "codex-default", "name": "Work", "hidden": true, "order": 2, "headline": "weekly"})
    );
    let config = std::fs::read_to_string(rig.state.join("config.toml"))?;
    assert!(config.contains("[domains.quota.display]"), "{config}");
    assert!(config.contains("codex-default"));
    // No flags changes nothing; unknown icon is refused and leaves config alone.
    let before = config;
    assert_eq!(code(&rig.command(&["quota", "display", "--json"])?), 0);
    assert_ne!(
        code(&rig.command(&["quota", "display", "--icon", "nope", "--json"])?),
        0
    );
    assert_eq!(
        std::fs::read_to_string(rig.state.join("config.toml"))?,
        before
    );
    let reset =
        json(&rig.command(&["quota", "display", "--icon", "most_constrained", "--json"])?)?;
    assert_eq!(reset["data"]["display"]["icon"], "most_constrained");
    // Tick output and report.json decorate identically.
    let tick = json(&rig.command(&["quota", "tick", "--json"])?)?;
    let disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(rig.state.join("quota/report.json"))?)?;
    assert_eq!(disk, tick["data"]);
    assert_eq!(disk["accounts"][0]["display"]["name"], "Work");
    rig.assert_legacy_unchanged()
}

#[test]
fn show_warns_when_display_icon_account_is_not_registered() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let rig = TestRig::in_directory(&std::env::temp_dir())?;
    std::fs::create_dir_all(&rig.state)?;
    std::fs::set_permissions(&rig.state, std::fs::Permissions::from_mode(0o700))?;
    let path = rig.state.join("config.toml");
    let before = "schema_version = 1\n[domains.quota.display]\nicon = 'codex-removed'\n";
    std::fs::write(&path, before)?;
    let shown = rig.command(&["quota", "show", "--json"])?;
    assert_eq!(code(&shown), 0);
    let shown = json(&shown)?;
    let warning = "display.icon names unregistered account 'codex-removed'; using most_constrained";
    assert_eq!(shown["data"]["display_icon_warning"], warning);
    assert_eq!(shown["data"]["display"]["icon"], "most_constrained");
    let human = rig.command(&["quota", "show"])?;
    assert_eq!(code(&human), 0);
    let warning_line = format!("Warning: {warning}");
    assert_eq!(
        String::from_utf8_lossy(&human.stdout.bytes)
            .lines()
            .filter(|line| *line == warning_line)
            .count(),
        1
    );
    assert_eq!(std::fs::read_to_string(path)?, before);
    rig.assert_legacy_unchanged()
}

fn history_line(at: i64, plan: &str, used: u64, reset: i64, marker: &str) -> String {
    format!(
        "{{\"schema_version\":1,\"observed_unix_seconds\":{at},\"account_key\":\"k1\",\"label\":\"work\",\"provider\":\"claude\",\"status\":\"available\",\"plan\":\"{plan}\",\"tier\":null,\"windows\":[{{\"pool\":\"five_hour\",\"window_seconds\":18000,\"used_basis_points\":{used},\"resets_unix_seconds\":{reset}}}],\"credit_balance\":null,\"reset_credits\":null{marker}}}\n"
    )
}

#[test]
fn history_lists_readings_with_price_in_force_and_filters() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let rig = TestRig::new()?;
    let history = rig.state.join("quota/history");
    std::fs::create_dir_all(&history)?;
    for path in [&rig.state, &rig.state.join("quota"), &history] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    // 2026-01-15, 2026-06-15 and 2026-06-15 (second, after a plan change and reset).
    let (a, b, c) = (1_768_478_400, 1_781_524_800, 1_781_528_400);
    let mut text = history_line(a, "max_5x", 500, a + 100, "");
    // Torn line from a crashed writer must not break the listing.
    text.push_str("{\"schema_version\":1,\"obs\n");
    text.push_str(&history_line(b, "max_5x", 900, b + 100, ""));
    text.push_str(&history_line(
        c,
        "max_20x",
        10,
        c + 100,
        ",\"markers\":[{\"kind\":\"reset\",\"pool\":\"five_hour\"},{\"kind\":\"plan.change\",\"from\":\"max_5x\",\"to\":\"max_20x\"}]",
    ));
    std::fs::write(history.join("k1.claude.jsonl"), text)?;
    std::fs::set_permissions(
        history.join("k1.claude.jsonl"),
        std::fs::Permissions::from_mode(0o600),
    )?;
    std::fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version = 1\n[domains.accounts.entries.claude-work]\nadapter = \"claude\"\nlabel = \"work\"\nkey = \"k1\"\ndirectory = \"{}\"\n\n[[domains.accounts.entries.claude-work.plans]]\nname = \"max_5x\"\nprice = 100.0\ncurrency = \"USD\"\neffective_from = \"2026-01-01\"\n\n[[domains.accounts.entries.claude-work.plans]]\nname = \"max_20x\"\nprice = 200.0\ncurrency = \"USD\"\neffective_from = \"2026-06-01\"\n",
            rig.root.join("claude").display()
        ),
    )?;
    let output = rig.command(&["quota", "history", "--json"])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    let data = &report["data"];
    assert_eq!(data["schema_version"], 1);
    assert_eq!(data["count"], 3);
    assert_eq!(data["truncated"], false);
    let readings = data["readings"].as_array().ok_or("readings")?;
    let times: Vec<i64> = readings
        .iter()
        .map(|r| r["observed_unix_seconds"].as_i64().unwrap_or(0))
        .collect();
    assert_eq!(times, [a, b, c]);
    assert_eq!(readings[0]["plan_price"]["name"], "max_5x");
    assert_eq!(readings[0]["plan_price"]["price"], 100.0);
    assert_eq!(readings[1]["plan_price"]["name"], "max_20x");
    assert_eq!(readings[2]["markers"][1]["kind"], "plan.change");
    assert_eq!(readings[2]["windows"][0]["used_basis_points"], 10);
    for key in ["tier", "credit_balance", "account_key", "label", "provider"] {
        assert!(readings[0].get(key).is_some(), "{key}");
    }
    let since = rig.command(&["quota", "history", "--json", "--since", "2026-06-01"])?;
    let since: serde_json::Value = serde_json::from_slice(&since.stdout.bytes)?;
    assert_eq!(since["data"]["count"], 2);
    let other = rig.command(&["quota", "history", "--json", "--account", "other"])?;
    let other: serde_json::Value = serde_json::from_slice(&other.stdout.bytes)?;
    assert_eq!(other["data"]["count"], 0);
    let limited = rig.command(&["quota", "history", "--json", "--limit", "1"])?;
    let limited: serde_json::Value = serde_json::from_slice(&limited.stdout.bytes)?;
    assert_eq!(limited["data"]["truncated"], true);
    assert_eq!(limited["data"]["readings"][0]["observed_unix_seconds"], c);
    let human = String::from_utf8(rig.command(&["quota", "history"])?.stdout.bytes)?;
    assert!(human.contains("RESET five_hour"), "{human}");
    assert!(human.contains("PLAN CHANGE max_5x -> max_20x"), "{human}");
    Ok(())
}

#[test]
fn history_without_state_is_empty_and_creates_nothing() -> Result {
    let rig = TestRig::new()?.without_state()?;
    let output = rig.command(&["quota", "history", "--json"])?;
    assert_eq!(code(&output), 0);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"]["count"], 0);
    assert!(!rig.state.exists());
    Ok(())
}
