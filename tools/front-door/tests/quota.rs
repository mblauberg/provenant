mod support;
use support::{Result, TestRig, code};

#[test]
fn failed_tick_creates_only_usage_state() -> Result {
    let mut rig = quota_rig()?;
    let missing = rig.root.join("missing");
    rig.state = missing.join("state");
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 3);
    rig.assert_only_usage_state()?;
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
    rig.assert_only_usage_state()?;
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
    assert_eq!(tick["data"]["coverage"]["total"], 5);
    assert_eq!(tick["data"]["accounts"], serde_json::json!([]));
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
        &["quota", "--json"][..],
        &["quota", "forget", "opaque", "--json"],
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
    for i in 0..128 {
        let key = format!("account-{i}");
        let account = serde_json::json!({"adapter":"claude","account_key":key,"label":"work"});
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
    use provenant_supervisor::{
        clock::SystemClock,
        fs::{Directory, LockMode},
    };
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
fn failed_auth_refresh_shows_last_known_usage_age_and_stale_marker() -> Result {
    use std::{fs, os::unix::fs::PermissionsExt};
    let rig = quota_rig()?;
    assert_eq!(code(&rig.command(&["quota", "tick", "--json"])?), 0);
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
    assert_eq!(row["status"], "stale");
    assert!(row["age_seconds"].as_u64().is_some_and(|age| age >= 10));
    assert_eq!(row["snapshot"]["windows"][0]["used_basis_points"], 5300);
    let human = rig.command(&["quota", "show"])?;
    let text = String::from_utf8(human.stdout.bytes)?;
    for expected in ["pro", "STALE", "last known", "53.00%", "age", "auth"] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("\\n"));
    rig.assert_legacy_unchanged()
}

fn jiff_time() -> Result<i64> {
    use provenant_core::Clock;
    Ok(provenant_supervisor::clock::SystemClock::new()
        .wall_now()?
        .as_timestamp()
        .as_second())
}
