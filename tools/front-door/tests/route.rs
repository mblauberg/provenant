//! Real command tree, isolated state and trap providers: previews perform no effects.
mod support;
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt};
use support::{Result, TestRig, code};

fn rig() -> Result<TestRig> {
    let mut rig = TestRig::new()?;
    for (name, tail) in [
        ("XDG_DATA_HOME", "xdg-data"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("AGY_CONFIG_DIR", "agy"),
        ("AGY_HOME", "agy"),
        ("CURSOR_USER_DATA_DIR", "cursor"),
    ] {
        rig.env
            .push((name.into(), rig.root.join(tail).into_os_string()));
    }
    fs::create_dir_all(rig.home.join("Library/Application Support/Cursor/User"))?;
    fs::create_dir_all(rig.home.join(".gemini/antigravity-cli"))?;
    let bin = rig.root.join("bin");
    fs::create_dir_all(&bin)?;
    for tool in [
        "claude",
        "codex",
        "cursor-agent",
        "agy",
        "opencode",
        "curl",
        "security",
    ] {
        let path = bin.join(tool);
        fs::write(
            &path,
            "#!/bin/sh\n: > \"$HOME/provider-started\"\nexit 97\n",
        )?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    rig.env.retain(|(name, _)| name != "PATH");
    rig.env.push(("PATH".into(), bin.into_os_string()));
    Ok(rig)
}
fn call(rig: &TestRig, args: &[&str], expected: i64) -> Result<Value> {
    let mut args = args.to_vec();
    args.push("--json");
    let out = rig.command(&args)?;
    assert_eq!(
        code(&out),
        expected,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout.bytes),
        String::from_utf8_lossy(&out.stderr.bytes)
    );
    assert!(out.stdout.bytes.len() <= 1_048_576);
    let value: Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value.as_object().ok_or("envelope")?.len(), 5);
    assert!(!rig.home.join("provider-started").exists());
    rig.assert_legacy_unchanged()?;
    Ok(value)
}
#[test]
fn help_list_and_resolve_are_pure_and_resolve_reports_disabled_exact_candidates() -> Result {
    let rig = rig()?;
    call(&rig, &["route", "--help"], 0)?;
    let listed = call(&rig, &["route", "list", "--adapter", "opencode"], 0)?;
    assert_eq!(listed["command"], "route list");
    assert_eq!(listed["data"]["availability"][0]["reason"], "disabled");
    assert!(
        listed["data"]["models"]
            .as_array()
            .ok_or("models")?
            .iter()
            .any(|m| m["model"] == "opencode/openrouter/~anthropic/claude-haiku-latest")
    );
    let resolved = call(
        &rig,
        &[
            "route",
            "resolve",
            "--model",
            "codex/gpt-6.1-sol",
            "--fallback",
            "opencode/openrouter/~anthropic/claude-haiku-latest,agy/explicit",
        ],
        0,
    )?;
    assert_eq!(resolved["data"]["model"], "gpt-6.1-sol");
    assert_eq!(resolved["data"]["effort"], Value::Null);
    assert_eq!(resolved["data"]["readiness"]["reason"], "disabled");
    assert_eq!(
        resolved["data"]["alternatives"][0]["model"],
        "openrouter/~anthropic/claude-haiku-latest"
    );
    assert_eq!(
        resolved["data"]["exclusions"]
            .as_array()
            .ok_or("exclusions")?
            .len(),
        3
    );
    let selected = rig.root.join("selected-project");
    fs::create_dir_all(&selected)?;
    let project = selected.to_str().ok_or("fixture project")?;
    let initialized = rig.execute(
        std::path::Path::new("/usr/bin/git"),
        &["init", "-q", project],
        None,
    )?;
    assert_eq!(code(&initialized), 0);
    assert_ne!(
        fs::canonicalize(&selected)?,
        fs::canonicalize(&rig.project)?
    );
    call(
        &rig,
        &[
            "route",
            "resolve",
            "--model",
            "codex/exact",
            "--project",
            project,
        ],
        0,
    )?;
    assert!(!rig.state.exists());
    Ok(())
}
#[test]
fn named_routes_and_cached_availability_are_read_only_and_fail_closed() -> Result {
    let rig = rig()?;
    fs::create_dir_all(&rig.state)?;
    let config = "schema_version=1\n[domains.routing.routes.review]\nmodel='opencode/openrouter/vendor/exact'\n";
    fs::write(rig.state.join("config.toml"), config)?;
    let resolved = call(&rig, &["route", "resolve", "--route", "review"], 0)?;
    assert_eq!(resolved["data"]["model"], "openrouter/vendor/exact");
    assert_eq!(fs::read_to_string(rig.state.join("config.toml"))?, config);
    assert_eq!(fs::read_dir(&rig.state)?.count(), 1);
    fs::write(rig.state.join("config.toml"), "schema_version=2\n")?;
    call(&rig, &["route", "list"], 2)?;
    fs::remove_file(rig.state.join("config.toml"))?;
    std::os::unix::fs::symlink(rig.root.join("foreign"), rig.state.join("config.toml"))?;
    call(&rig, &["route", "list"], 3)?;
    fs::remove_file(rig.state.join("config.toml"))?;
    fs::write(rig.state.join("config.toml"), config)?;
    let cache = rig.state.join("routing/availability");
    fs::create_dir_all(&cache)?;
    let record = serde_json::json!({"schema_version":1,"adapter":"codex","executable":"/fixture/codex","version":"fixture","fixture_ref":"recording","catalogue_revision":"recordings-20261024-v1","account_key":"opaque-account","observed_models":["exact"],"last_preflight_error":null,"observed_unix_seconds":1,"expires_unix_seconds":2});
    let bytes = serde_json::to_vec(&record)?;
    fs::write(cache.join("codex.json"), &bytes)?;
    let listed = call(&rig, &["route", "list", "--adapter", "codex"], 0)?;
    assert!(listed["data"]["availability"][0]["age_seconds"].is_u64());
    assert_eq!(
        listed["data"]["availability"][0]["record"]["account_key"],
        "opaque-account"
    );
    assert_eq!(fs::read(cache.join("codex.json"))?, bytes);
    let mut diagnostic = record.clone();
    diagnostic["last_preflight_error"] = "secret-bearing-provider-diagnostic".into();
    fs::write(cache.join("codex.json"), serde_json::to_vec(&diagnostic)?)?;
    let rejected = call(&rig, &["route", "list"], 2)?;
    assert!(!serde_json::to_string(&rejected)?.contains("secret-bearing-provider-diagnostic"));
    fs::write(
        cache.join("codex.json"),
        "{\"schema_version\":1,\"schema_version\":1}",
    )?;
    call(&rig, &["route", "list"], 2)?;
    fs::remove_file(cache.join("codex.json"))?;
    std::os::unix::fs::symlink(rig.root.join("foreign"), cache.join("codex.json"))?;
    call(&rig, &["route", "list"], 3)?;
    Ok(())
}
#[test]
fn invalid_choices_effort_and_old_policy_flags_are_usage_errors() -> Result {
    let rig = rig()?;
    for args in [
        vec!["route", "resolve"],
        vec![
            "route", "resolve", "--model", "codex/a", "--route", "review",
        ],
        vec![
            "route",
            "resolve",
            "--model",
            "codex/a",
            "--fallback",
            "claude/a,agy/b,opencode/c",
        ],
        vec![
            "route",
            "resolve",
            "--model",
            "codex/a",
            "--fallback",
            "codex/a",
        ],
        vec!["route", "resolve", "--route", "missing"],
        vec!["route", "resolve", "--model", "codex/a", "--effort", "high"],
        vec!["route", "resolve", "--model", "codex/a#high"],
        vec!["route", "list", "--adapter", "invented"],
        vec!["route", "resolve", "--pool", "principal"],
        vec![
            "route",
            "resolve",
            "--model",
            "codex/a",
            "--commit-selection",
        ],
        vec!["extract"],
        vec!["digest"],
        vec!["classify"],
    ] {
        call(&rig, &args, 2)?;
    }
    assert!(!rig.state.exists());
    Ok(())
}

#[test]
fn large_cached_observation_sets_have_a_compact_bounded_projection() -> Result {
    let rig = rig()?;
    let cache = rig.state.join("routing/availability");
    fs::create_dir_all(&cache)?;
    for adapter in ["codex", "claude", "cursor", "agy", "opencode"] {
        let record = serde_json::json!({"schema_version":1,"adapter":adapter,"executable":"/fixture/provider","version":"fixture","fixture_ref":"recording","catalogue_revision":"recordings-20261024-v1","account_key":"opaque-account","observed_models":vec!["\"".repeat(512); 256],"last_preflight_error":null,"observed_unix_seconds":1,"expires_unix_seconds":2});
        fs::write(
            cache.join(format!("{adapter}.json")),
            serde_json::to_vec(&record)?,
        )?;
    }
    let listed = call(&rig, &["route", "list"], 0)?;
    for row in listed["data"]["availability"]
        .as_array()
        .ok_or("availability")?
    {
        assert_eq!(row["record"]["observed_model_count"], 256);
    }
    Ok(())
}
