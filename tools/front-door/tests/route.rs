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
fn resolve_reports_effort_controls_and_nearby_models() -> Result {
    let rig = rig()?;
    let known = call(&rig, &["route", "resolve", "codex/gpt-6-luna"], 0)?;
    assert_eq!(
        known["data"]["supported_efforts"],
        serde_json::json!(["low", "medium", "high", "xhigh"])
    );
    let none = call(
        &rig,
        &["route", "resolve", "opencode/opencode-go/glm-5.2"],
        0,
    )?;
    assert_eq!(none["data"]["supported_efforts"], serde_json::json!([]));
    let unknown = call(&rig, &["route", "resolve", "codex/gpt-6.1-luna"], 0)?;
    assert!(unknown["data"]["supported_efforts"].is_null());
    assert_eq!(unknown["data"]["nearest_models"][0], "codex/gpt-6-luna");
    let refused = call(
        &rig,
        &["route", "resolve", "codex/gpt-6.1-luna", "--effort", "high"],
        2,
    )?;
    assert!(
        refused["error"]["message"]
            .as_str()
            .ok_or("message")?
            .contains("codex/gpt-6-luna")
    );
    let output = rig.command(&["route", "resolve", "opencode/opencode-go/glm-5.2"])?;
    assert!(String::from_utf8_lossy(&output.stdout.bytes).contains("supported efforts: none"));
    Ok(())
}

#[test]
fn valid_pairs_and_policy_preview_are_pure() -> Result {
    let rig = rig()?.without_state()?;
    let listed = call(&rig, &["route", "list", "--valid", "--adapter", "codex"], 0)?;
    let pairs = listed["data"]["valid_pairs"]
        .as_array()
        .ok_or("valid pairs")?;
    assert!(!pairs.is_empty());
    assert!(
        listed["data"]["models"]
            .as_array()
            .ok_or("models")?
            .is_empty()
    );
    for pair in pairs {
        let model = pair["model"].as_str().ok_or("model")?;
        let mut args = vec!["route", "resolve", model];
        if let Some(effort) = pair["effort"].as_str() {
            args.extend(["--effort", effort]);
        }
        assert_eq!(call(&rig, &args, 0)?["data"]["eligibility"], "eligible");
    }
    let claude = call(&rig, &["route", "resolve", "claude/claude-opus-5-5"], 0)?;
    assert_eq!(
        claude["data"]["model_policy"]["compaction"]["window"],
        275000
    );
    assert_eq!(
        claude["data"]["model_policy"]["compaction"]["mode"],
        "task_boundary"
    );
    assert!(
        claude["data"]["compaction_break_even_turns"]
            .as_f64()
            .is_some()
    );
    let restricted = call(
        &rig,
        &["route", "resolve", "agy/gemini-3.8-flash", "--writes"],
        2,
    )?;
    assert!(
        restricted["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("read-only"))
    );
    assert!(!rig.state.exists());
    Ok(())
}

#[test]
fn account_override_is_applied_before_preview_write_admission() -> Result {
    let rig = rig()?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[[domains.routing.model_policy.rules]]\naccount='agy/work'\n[domains.routing.model_policy.rules.policy.lane]\nread_only=false\n",
    )?;
    let preview = call(
        &rig,
        &[
            "route",
            "resolve",
            "agy/gemini-3.8-flash",
            "--account",
            "agy/work",
            "--writes",
        ],
        0,
    )?;
    assert_eq!(preview["data"]["model_policy"]["lane"]["read_only"], false);
    // A preview hint never supplies observed account identity or readiness.
    assert!(preview["data"]["account_key"].is_null());
    assert_eq!(preview["data"]["readiness"]["status"], "unavailable");
    call(
        &rig,
        &[
            "route",
            "resolve",
            "agy/gemini-3.8-flash",
            "--account",
            "agy/other",
            "--writes",
        ],
        2,
    )?;
    Ok(())
}
#[test]
fn override_set_echoes_replaces_lists_and_clears() -> Result {
    let rig = rig()?;
    fs::create_dir_all(&rig.state)?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[domains.accounts.entries.claude-pro]\nadapter='claude'\nlabel='pro'\nkey='pro'\ndirectory='/tmp/pro'\n[domains.routing.pacing.accounts.pro]\npolicy = 'pace'\nwindows = ['five_hour']\nreserve = { five_hour = 15 }\n",
    )?;
    let list = call(&rig, &["route", "list"], 0)?;
    assert_eq!(
        list["data"]["configured_accounts"]["claude/pro"]["policy"],
        "pace"
    );
    for args in [
        vec![
            "route", "override", "set", "--all", "--policy", "spend", "--until", "0h",
        ],
        vec![
            "route", "override", "set", "--all", "--policy", "spend", "--window", "9",
        ],
        vec![
            "route",
            "override",
            "set",
            "--all",
            "--policy",
            "spend",
            "--tz",
            "Mars/Base",
        ],
    ] {
        let refused = call(&rig, &args, 2)?;
        assert_eq!(refused["error"]["code"], "INVALID", "{refused}");
    }
    let empty = call(&rig, &["route", "override", "list"], 0)?;
    assert_eq!(empty["data"]["recorded"], Value::Null);
    assert_eq!(empty["data"]["overrides"], serde_json::json!([]));
    // Each change echoes the rule it recorded.
    let recorded = call(
        &rig,
        &[
            "route",
            "override",
            "set",
            "--account",
            "pro",
            "--policy",
            "spend",
            "--until",
            "2h",
        ],
        0,
    )?;
    assert_eq!(recorded["command"], "route override set");
    assert_eq!(recorded["data"]["recorded"]["scope"], "account claude/pro");
    assert_eq!(recorded["data"]["recorded"]["value"], "spend");
    assert_eq!(recorded["data"]["recorded"]["active"], true);
    let remaining = recorded["data"]["recorded"]["remaining_seconds"]
        .as_i64()
        .ok_or("remaining")?;
    assert!((7100..=7200).contains(&remaining), "{recorded}");
    let replaced = call(
        &rig,
        &[
            "route",
            "override",
            "set",
            "--account",
            "pro",
            "--policy",
            "pace",
            "--until",
            "1d",
        ],
        0,
    )?;
    assert_eq!(replaced["data"]["cleared"][0]["value"], "spend");
    let compact = rig.command(&[
        "route",
        "override",
        "set",
        "--provider",
        "codex",
        "--policy",
        "max",
        "--window",
        "mon-fri@23-7",
        "--tz",
        "Australia/Sydney",
    ])?;
    assert_eq!(code(&compact), 0);
    let compact = String::from_utf8(compact.stdout.bytes)?;
    assert!(
        compact.contains("recorded pacing provider codex max "),
        "{compact}"
    );
    assert!(
        compact.contains("no expiry; mon,tue,wed,thu,fri 23-07 Australia/Sydney"),
        "{compact}"
    );
    let listed = rig.command(&["route", "override", "list"])?;
    assert_eq!(code(&listed), 0);
    let listed = String::from_utf8(listed.stdout.bytes)?;
    assert!(
        listed.contains("pacing account claude/pro pace active until "),
        "{listed}"
    );
    assert!(listed.contains("s remaining)"), "{listed}");
    let config = call(&rig, &["config", "show"], 0)?;
    let overrides = config["data"]["domains"]["routing"]["pacing_overrides"]
        .as_array()
        .ok_or("overrides")?;
    assert_eq!(overrides.len(), 2);
    assert_eq!(overrides[0]["account"], "claude/pro");
    assert_eq!(overrides[0]["policy"], "pace");
    assert!(overrides[0]["schedule"]["expires"].as_i64().is_some());
    assert_eq!(overrides[1]["schedule"]["windows"][0]["start_hour"], 23);
    let one = call(
        &rig,
        &["route", "override", "clear", "--provider", "codex"],
        0,
    )?;
    assert_eq!(one["data"]["cleared"].as_array().map(Vec::len), Some(1));
    assert_eq!(one["data"]["overrides"].as_array().map(Vec::len), Some(1));
    let cleared = call(&rig, &["route", "override", "clear"], 0)?;
    assert_eq!(cleared["data"]["cleared"][0]["scope"], "account claude/pro");
    assert_eq!(cleared["data"]["overrides"], serde_json::json!([]));
    let config = call(&rig, &["config", "show"], 0)?;
    assert_eq!(
        config["data"]["domains"]["routing"]["pacing_overrides"],
        serde_json::json!([])
    );
    let pacing = call(&rig, &["route", "pacing", "codex", "spend"], 0)?;
    assert_eq!(pacing["data"]["recorded"], "provider codex pacing spend");
    Ok(())
}

#[test]
fn override_set_resolves_labels_and_rejects_ambiguous_keys() -> Result {
    let rig = rig()?;
    fs::create_dir_all(&rig.state)?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[domains.accounts.entries.one]\nadapter='claude'\nlabel='work'\nkey='shared'\ndirectory='/tmp/a'\n[domains.accounts.entries.two]\nadapter='codex'\nlabel='personal'\nkey='shared'\ndirectory='/tmp/b'\n[[domains.routing.pacing_overrides]]\naccount='work'\npolicy='pace'\nschedule={expires=2000000000}\n",
    )?;
    let rejected = call(
        &rig,
        &[
            "route",
            "override",
            "set",
            "--account",
            "shared",
            "--policy",
            "spend",
            "--until",
            "2h",
        ],
        2,
    )?;
    assert!(
        rejected["error"]["message"]
            .as_str()
            .ok_or("error")?
            .contains("ambiguous")
    );
    let replaced = call(
        &rig,
        &[
            "route",
            "override",
            "set",
            "--account",
            "claude/shared",
            "--policy",
            "spend",
            "--until",
            "2h",
        ],
        0,
    )?;
    assert_eq!(replaced["data"]["cleared"][0]["scope"], "account work");
    let config = call(&rig, &["config", "show"], 0)?;
    let overrides = config["data"]["domains"]["routing"]["pacing_overrides"]
        .as_array()
        .ok_or("overrides")?;
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[0]["account"], "claude/shared");
    Ok(())
}

#[test]
fn override_list_shows_active_future_and_expired_schedules() -> Result {
    let rig = rig()?;
    fs::create_dir_all(&rig.state)?;
    let now = jiff::Timestamp::now().as_second();
    let config = format!(
        "schema_version=1\n[[domains.routing.pacing_overrides]]\nprovider='codex'\npolicy='pace'\n[domains.routing.pacing_overrides.schedule]\nstarts={}\nexpires={}\n[[domains.routing.pacing_overrides]]\nprovider='claude'\npolicy='spend'\n[domains.routing.pacing_overrides.schedule]\nstarts={}\nexpires={}\n[[domains.routing.pacing_overrides]]\nprovider='agy'\npolicy='off'\n[domains.routing.pacing_overrides.schedule]\nstarts={}\nexpires={}\n[[domains.routing.model_policy.rules]]\nmodel='codex/gpt-6.1-terra'\nschedule={{windows=[{{weekdays=[5,6],start_hour=0,end_hour=24}}]}}\n[domains.routing.model_policy.rules.policy.cache]\nttl_seconds=60\n",
        now - 30,
        now + 300,
        now + 300,
        now + 600,
        now - 600,
        now - 300,
    );
    fs::write(rig.state.join("config.toml"), config)?;
    let listed = call(&rig, &["route", "override", "list"], 0)?;
    let rows = listed["data"]["overrides"].as_array().ok_or("overrides")?;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["active"], true);
    assert!(
        rows[0]["remaining_seconds"]
            .as_i64()
            .is_some_and(|s| s > 0 && s <= 300)
    );
    assert_eq!(rows[1]["active"], false);
    assert_eq!(rows[2]["active"], false);
    assert_eq!(rows[2]["remaining_seconds"], 0);
    assert_eq!(rows[3]["kind"], "model_policy");
    assert_eq!(
        rows[3]["schedule"]["windows"][0]["weekdays"],
        serde_json::json!([5, 6])
    );
    assert!(rows[3]["remaining_seconds"].is_null());
    Ok(())
}

#[test]
fn retired_schedule_keys_drop_only_their_record_with_a_named_warning() -> Result {
    let rig = rig()?;
    fs::create_dir_all(&rig.state)?;
    for (config, path, replacement) in [
        (
            "schema_version=1\n[[domains.routing.pacing_overrides]]\nprovider='codex'\npolicy='pace'\nexpires_unix_seconds=2000000000\n[[domains.routing.pacing_overrides]]\nprovider='claude'\npolicy='spend'\n",
            "domains.routing.pacing_overrides.0",
            "moved_to_schedule.expires",
        ),
        (
            "schema_version=1\n[[domains.routing.model_policy.rules]]\nmodel='codex/gpt-6.1-terra'\nwindows=[{start_hour=1,end_hour=4}]\n[domains.routing.model_policy.rules.policy.cache]\nttl_seconds=60\n[[domains.routing.pacing_overrides]]\nprovider='claude'\npolicy='spend'\n",
            "domains.routing.model_policy.rules.0",
            "moved_to_schedule.windows",
        ),
    ] {
        fs::write(rig.state.join("config.toml"), config)?;
        let out = rig.command(&["route", "override", "list", "--json"])?;
        assert_eq!(code(&out), 0);
        let stderr = String::from_utf8(out.stderr.bytes)?;
        assert!(
            stderr.contains(&format!(
                "{path}: ignored invalid configuration (domain: {replacement})"
            )),
            "{stderr}"
        );
        let listed: Value = serde_json::from_slice(&out.stdout.bytes)?;
        let rows = listed["data"]["overrides"].as_array().ok_or("overrides")?;
        assert_eq!(rows.len(), 1, "{listed}");
        assert_eq!(rows[0]["scope"], "provider claude");
    }
    // A rule that enforces a lane restriction stays strict.
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[[domains.routing.model_policy.rules]]\nmodel='codex/gpt-6.1-terra'\nexpires_unix_seconds=1\n[domains.routing.model_policy.rules.policy.lane]\nmax_turns=5\n",
    )?;
    let refused = call(&rig, &["route", "override", "list"], 2)?;
    assert_eq!(refused["error"]["code"], "CONFIG", "{refused}");
    let message = refused["error"]["message"].as_str().ok_or("message")?;
    assert!(
        message.contains("domains.routing.model_policy.rules.0.expires_unix_seconds"),
        "{message}"
    );
    assert!(message.contains("moved_to_schedule.expires"), "{message}");
    Ok(())
}

#[test]
fn cached_account_pacing_is_visible_without_probing() -> Result {
    use provenant_quota::{AccountRecord, AccountScope, QuotaSnapshot, QuotaStatus, QuotaWindow};
    let rig = rig()?;
    let at = jiff::Timestamp::now().as_second();
    let directory = rig.state.join("quota/opaque-one");
    fs::create_dir_all(&directory)?;
    for path in [&rig.state, &rig.state.join("quota"), &directory] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let account = AccountScope {
        adapter: "claude".into(),
        account_key: "opaque-one".into(),
        label: "michael".into(),
    };
    let mut record = AccountRecord::empty(account.clone());
    record.revision = 1;
    record.snapshot = Some(QuotaSnapshot {
        schema_version: 1,
        account,
        revision: 1,
        status: QuotaStatus::Available,
        observed_unix_seconds: at,
        source_ref: "fixture".into(),
        authority_ref: None,
        details: None,
        windows: vec![QuotaWindow {
            pool: "seven_day".into(),
            window_seconds: Some(604_800),
            used_basis_points: 9000,
            resets_unix_seconds: Some(at + 300_000),
        }],
    });
    let path = directory.join("claude.json");
    fs::write(&path, serde_json::to_vec(&record)?)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[domains.accounts.entries.claude-michael]\nadapter='claude'\nlabel='michael'\nkey='opaque-one'\ndirectory='/tmp/michael'\n[domains.accounts.entries.claude-anese]\nadapter='claude'\nlabel='anese'\nkey='opaque-two'\ndirectory='/tmp/anese'\n[domains.routing.pacing.accounts.michael]\npolicy='pace'\nwindows=['seven_day']\nreserve={seven_day=15}\ndeadline={reset_of='anese'}\n",
    )?;
    let list = call(&rig, &["route", "list", "--adapter", "claude"], 0)?;
    assert_eq!(list["data"]["account_pacing"][0]["account"], "opaque-one");
    assert_eq!(list["data"]["account_pacing"][0]["source"], "account");
    let resolved = call(&rig, &["route", "resolve", "claude/claude-haiku-5-5"], 0)?;
    let detail = resolved["data"]["pacing"][0]["detail"]
        .as_str()
        .ok_or("pacing detail")?;
    assert!(detail.contains("source=account") && detail.contains("seven_day"));
    assert!(detail.contains("unknown or stale") && detail.contains("falling back"));
    assert!(detail.contains("reserve exclusion window=seven_day"));
    call(
        &rig,
        &[
            "route",
            "override",
            "set",
            "--account",
            "michael",
            "--policy",
            "spend",
            "--until",
            "2h",
        ],
        0,
    )?;
    let resolved = call(&rig, &["route", "resolve", "claude/claude-haiku-5-5"], 0)?;
    assert_eq!(resolved["data"]["pacing"][0]["policy"], "spend");
    assert!(
        resolved["data"]["pacing"][0]["detail"]
            .as_str()
            .ok_or("override detail")?
            .contains("source=override")
    );
    Ok(())
}

#[test]
fn pacing_profile_account_labels_resolve_and_unknown_names_fail() -> Result {
    let rig = rig()?;
    fs::create_dir_all(&rig.state)?;
    let registrations = "schema_version=1\n[domains.accounts.entries.claude-michael]\nadapter='claude'\nlabel='michael'\nkey='opaque-one'\ndirectory='/tmp/michael'\n[domains.accounts.entries.claude-anese]\nadapter='claude'\nlabel='anese'\nkey='opaque-two'\ndirectory='/tmp/anese'\n";
    let config = rig.state.join("config.toml");
    fs::write(
        &config,
        format!(
            "{registrations}[domains.routing.pacing.accounts.michael]\npolicy='pace'\ndeadline={{reset_of='anese'}}\n[domains.routing.pacing.free.opencode]\ndeadline={{reset_of='michael'}}\n"
        ),
    )?;
    let list = call(&rig, &["route", "list"], 0)?;
    assert_eq!(
        list["data"]["configured_accounts"]["claude/opaque-one"]["deadline"]["reset_of"],
        "claude/opaque-two"
    );
    assert_eq!(
        list["data"]["configured_free"]["opencode"]["deadline"]["reset_of"],
        "claude/opaque-one"
    );
    for profile in [
        "[domains.routing.pacing.accounts.missing]\npolicy='pace'\n",
        "[domains.routing.pacing.accounts.opaque-one]\ndeadline={reset_of='missing'}\n",
        "[domains.routing.pacing.free.opencode]\ndeadline={reset_of='missing'}\n",
    ] {
        fs::write(&config, format!("{registrations}{profile}"))?;
        let result = call(&rig, &["route", "list"], 2)?;
        let message = result["error"]["message"].as_str().ok_or("error")?;
        assert!(
            message.contains("missing")
                && message.contains("michael")
                && message.contains("opaque-one"),
            "{message}"
        );
    }
    fs::write(
        &config,
        format!(
            "{registrations}[domains.routing.pacing.accounts.michael]\npolicy='pace'\n[domains.routing.pacing.accounts.opaque-one]\npolicy='spend'\n"
        ),
    )?;
    let duplicate = call(&rig, &["route", "list"], 2)?;
    assert!(
        duplicate["error"]["message"]
            .as_str()
            .ok_or("error")?
            .contains("multiple pacing profiles")
    );
    fs::write(
        &config,
        format!(
            "{registrations}[domains.accounts.entries.codex-michael]\nadapter='codex'\nlabel='michael'\nkey='opaque-three'\ndirectory='/tmp/codex'\n[domains.routing.pacing.accounts.michael]\npolicy='pace'\n"
        ),
    )?;
    let ambiguous = call(&rig, &["route", "list"], 2)?;
    assert!(
        ambiguous["error"]["message"]
            .as_str()
            .ok_or("error")?
            .contains("ambiguous")
    );
    Ok(())
}

#[test]
fn pacing_profiles_warn_on_unknown_keys_and_list_accounts() -> Result {
    let rig = rig()?;
    fs::create_dir_all(&rig.state)?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[domains.routing.pacing.accounts.pro]\nwindow=['five_hour']\n",
    )?;
    // The misspelt key is tuning, so it warns and is ignored; the profile
    // still names an unregistered account, which refuses and lists choices.
    let out = rig.command(&["route", "list", "--json"])?;
    assert_eq!(code(&out), 2);
    let stderr = String::from_utf8_lossy(&out.stderr.bytes);
    assert!(
        stderr.contains("domains.routing.pacing.accounts.pro.window")
            && stderr.contains("unknown_key"),
        "{stderr}"
    );
    let output: Value = serde_json::from_slice(&out.stdout.bytes)?;
    let message = output["error"]["message"].as_str().ok_or("config error")?;
    assert!(
        message.contains("unknown account `pro`") && message.contains("valid accounts"),
        "{message}"
    );
    Ok(())
}

#[test]
fn help_list_and_resolve_are_pure_and_resolve_reports_exact_candidates() -> Result {
    let rig = rig()?.without_state()?;
    call(&rig, &["route", "--help"], 0)?;
    let listed = call(&rig, &["route", "list", "--adapter", "opencode"], 0)?;
    assert_eq!(listed["command"], "route list");
    assert!(
        listed["data"]["models"]
            .as_array()
            .ok_or("models")?
            .iter()
            .any(|m| m["model"] == "opencode/openrouter/~anthropic/claude-haiku-latest")
    );
    let pool = call(
        &rig,
        &["route", "resolve", "--route", "utility", "--confidential"],
        0,
    )?;
    assert_eq!(pool["data"]["model"], "claude-haiku-5-5");
    let resolved = call(
        &rig,
        &[
            "route",
            "resolve",
            "--model",
            "codex/gpt-6.1-terra",
            "--fallback",
            "opencode/openrouter/~anthropic/claude-haiku-latest,agy/explicit",
        ],
        0,
    )?;
    assert_eq!(resolved["data"]["model"], "gpt-6.1-terra");
    assert_eq!(resolved["data"]["effort"], Value::Null);
    assert_eq!(
        resolved["data"]["readiness"]["reason"],
        "unknown_availability"
    );
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
    // A positional name is a route, an exact model, or a unique model by its ID (#1838).
    for (name, model) in [
        ("utility", "claude-haiku-5-5"),
        ("codex/gpt-6.1-terra", "gpt-6.1-terra"),
        ("gpt-6.1-terra", "gpt-6.1-terra"),
        ("terra", "gpt-6.1-terra"),
    ] {
        let named = call(&rig, &["route", "resolve", name], 0)?;
        assert_eq!(named["data"]["model"], model, "{name}");
    }
    let selected = rig.root.join("selected-project");
    fs::create_dir_all(&selected)?;
    let project = selected.to_str().ok_or("fixture project")?;
    let initialized = rig.execute(
        &provenant_os::git::git_executable().ok_or("Git unavailable")?,
        &["init", "-q", project],
        None,
    )?;
    assert_eq!(
        code(&initialized),
        0,
        "git init stderr={}",
        String::from_utf8_lossy(&initialized.stderr.bytes)
    );
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
fn route_list_human_output_omits_dispatch_only_fields() -> Result {
    let rig = rig()?;
    let out = rig.command(&["route", "list", "--pool", "junior", "--full"])?;
    assert_eq!(code(&out), 0);
    let stdout = String::from_utf8(out.stdout.bytes)?;
    assert!(stdout.contains("weight="));
    assert!(stdout.contains("pacing="));
    assert!(!stdout.contains("factor="));
    assert!(!stdout.contains("readiness="));
    assert!(!rig.home.join("provider-started").exists());
    rig.assert_legacy_unchanged()?;
    Ok(())
}

#[test]
fn route_config_verbs_round_trip_atomic_config_and_recover_invalid_weights() -> Result {
    let rig = rig()?;
    let set = call(
        &rig,
        &[
            "route",
            "set",
            "junior",
            "codex/gpt-6-luna",
            "--weight",
            "7",
            "--effort",
            "high",
            "--efforts",
            "medium,high,xhigh",
        ],
        0,
    )?;
    assert_eq!(set["command"], "route set");
    let persisted = fs::read_to_string(rig.state.join("config.toml"))?;
    assert!(persisted.contains("[domains.routing]"));

    let listed = call(&rig, &["route", "list", "--pool", "junior"], 0)?;
    let member = listed["data"]["pools"]["junior"]
        .as_array()
        .and_then(|members| members.iter().find(|m| m["model"] == "codex/gpt-6-luna"))
        .ok_or("configured junior member")?;
    assert_eq!(member["weight"], 7);
    assert_eq!(member["effort"], "high");
    assert_eq!(
        member["efforts"],
        serde_json::json!(["medium", "high", "xhigh"])
    );
    // Listing never probes, so it reports no readiness or pacing factor.
    assert!(member.get("factor_basis_points").is_none());
    assert!(member.get("readiness").is_none());

    let pacing = call(&rig, &["route", "pacing", "codex", "spend"], 0)?;
    assert_eq!(pacing["command"], "route pacing");
    assert_eq!(pacing["data"]["recorded"], "provider codex pacing spend");
    let pacing_list = call(&rig, &["route", "list", "--pool", "junior"], 0)?;
    assert_eq!(pacing_list["data"]["pacing"]["codex"], "spend");

    call(&rig, &["route", "remove", "junior", "codex/gpt-6-luna"], 0)?;
    let after_remove = call(&rig, &["route", "list", "--pool", "junior"], 0)?;
    assert!(
        !after_remove["data"]["pools"]["junior"]
            .as_array()
            .ok_or("junior members")?
            .iter()
            .any(|m| m["model"] == "codex/gpt-6-luna")
    );
    call(&rig, &["route", "reset", "junior"], 0)?;
    let reset = call(&rig, &["route", "list", "--pool", "junior"], 0)?;
    assert!(
        reset["data"]["pools"]["junior"]
            .as_array()
            .ok_or("reset junior members")?
            .iter()
            .any(|m| m["model"] == "codex/gpt-6-luna")
    );

    // Pool weights are tuning: an invalid member recovers with a warning
    // naming the weight, and the bundled pool applies.
    fs::write(
        rig.state.join("config.toml"),
        r#"schema_version = 1
[domains.routing]
pools = { junior = [{ model = "codex/gpt-6-luna", weight = 101, effort = "high", efforts = ["high"] }] }
"#,
    )?;
    let out = rig.command(&["route", "list", "--pool", "junior", "--json"])?;
    assert_eq!(code(&out), 0);
    let stderr = String::from_utf8_lossy(&out.stderr.bytes);
    assert!(
        stderr.contains("domains.routing.pools.junior") && stderr.contains("weight"),
        "{stderr}"
    );
    let recovered: Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert!(
        !recovered["data"]["pools"]["junior"]
            .as_array()
            .ok_or("junior members")?
            .iter()
            .any(|m| m["weight"] == 101)
    );

    // Routing lives only under its domain; the root alias is an unknown key.
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[routing.pacing]\ncodex = \"spend\"\n",
    )?;
    let out = rig.command(&["route", "list", "--pool", "junior", "--json"])?;
    assert_eq!(code(&out), 0);
    let stderr = String::from_utf8_lossy(&out.stderr.bytes);
    assert!(
        stderr.contains("routing: ignored invalid configuration (unknown_key)"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn route_config_verbs_edit_inline_domain_tables() -> Result {
    let rig = rig()?;
    let defaults = call(&rig, &["route", "list", "--pool", "junior"], 0)?;
    fs::create_dir_all(&rig.state)?;
    for tables in [
        "domains = { routing = {} }",
        "[domains]\nrouting = {}",
        "[domains.routing]\npools = {}\npacing = {}",
    ] {
        fs::write(
            rig.state.join("config.toml"),
            format!("schema_version = 1 # keep\n{tables}\n"),
        )?;
        call(
            &rig,
            &[
                "route",
                "set",
                "junior",
                "codex/gpt-6-luna",
                "--weight",
                "7",
                "--effort",
                "high",
            ],
            0,
        )?;
        let listed = call(&rig, &["route", "list", "--pool", "junior"], 0)?;
        assert!(
            listed["data"]["pools"]["junior"]
                .as_array()
                .ok_or("members")?
                .iter()
                .any(|m| m["model"] == "codex/gpt-6-luna" && m["weight"] == 7)
        );
        call(&rig, &["route", "pacing", "codex", "spend"], 0)?;
        call(&rig, &["route", "remove", "junior", "codex/gpt-6-luna"], 0)?;
        call(&rig, &["route", "reset", "junior"], 0)?;
        let reset = call(&rig, &["route", "list", "--pool", "junior"], 0)?;
        assert!(
            reset["data"]["pools"]["junior"]
                .as_array()
                .ok_or("members")?
                .iter()
                .any(|m| m["model"] == "codex/gpt-6-luna")
        );
        let persisted = fs::read_to_string(rig.state.join("config.toml"))?;
        assert!(persisted.contains("# keep"));
        let config = call(&rig, &["config", "show"], 0)?;
        assert_eq!(
            config["data"]["domains"]["routing"]["pacing"]["codex"],
            "spend"
        );
    }
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\ndomains = { routing = { pools = { junior = [{ model = 'codex/gpt-6-luna', weight = 7, effort = 'high', efforts = ['high'] }] } } }\n",
    )?;
    call(&rig, &["route", "reset", "junior"], 0)?;
    let reset = call(&rig, &["route", "list", "--pool", "junior"], 0)?;
    assert_eq!(reset["data"]["pools"], defaults["data"]["pools"]);
    Ok(())
}

#[test]
fn named_routes_are_read_only_and_fail_closed() -> Result {
    let rig = rig()?;
    fs::create_dir_all(&rig.state)?;
    let config = "schema_version=1\n[domains.routing.routes.review]\nmodel='opencode/openrouter/~anthropic/claude-haiku-latest'\n";
    fs::write(rig.state.join("config.toml"), config)?;
    let resolved = call(&rig, &["route", "resolve", "--route", "review"], 0)?;
    assert_eq!(
        resolved["data"]["model"],
        "openrouter/~anthropic/claude-haiku-latest"
    );
    assert_eq!(fs::read_to_string(rig.state.join("config.toml"))?, config);
    // Only the usage counter may appear beside the config.
    let names = fs::read_dir(&rig.state)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    assert!(
        names
            .iter()
            .all(|n| n == "config.toml" || n == "usage.jsonl")
    );
    fs::write(rig.state.join("config.toml"), "schema_version=2\n")?;
    call(&rig, &["route", "list"], 2)?;
    fs::remove_file(rig.state.join("config.toml"))?;
    std::os::unix::fs::symlink(rig.root.join("foreign"), rig.state.join("config.toml"))?;
    call(&rig, &["route", "list"], 3)?;
    fs::remove_file(rig.state.join("config.toml"))?;
    fs::write(rig.state.join("config.toml"), config)?;
    Ok(())
}
#[test]
fn unresolved_names_list_close_matches_and_examples() -> Result {
    let rig = rig()?.without_state()?;
    let message = |args: &[&str]| -> Result<String> {
        let value = call(&rig, args, 2)?;
        Ok(value["error"]["message"]
            .as_str()
            .ok_or("message")?
            .to_owned())
    };
    // A near miss names the real route and model, and shows how to ask. The
    // misspelling is assembled so that no literal one sits in the source.
    let misspelt = ["util", "ty"].concat();
    let near = message(&["route", "resolve", &misspelt])?;
    assert!(near.contains("close matches:"), "{near}");
    assert!(near.contains("utility"), "{near}");
    assert!(near.contains("provenant route resolve "), "{near}");
    assert!(near.contains("provenant route list"), "{near}");
    let flag = message(&["route", "resolve", "--route", &misspelt])?;
    assert!(flag.contains("utility"), "{flag}");
    // Nothing close still teaches the shapes.
    let none = message(&["route", "resolve", "zzzzzz-nothing"])?;
    assert!(!none.contains("close matches"), "{none}");
    assert!(none.contains("provenant route resolve "), "{none}");
    // No argument at all is a usage error that shows examples, before any state.
    let missing = message(&["route", "resolve"])?;
    assert!(
        missing.contains("provenant route resolve senior"),
        "{missing}"
    );
    assert!(!rig.state.exists());
    Ok(())
}

#[test]
fn invalid_choices_effort_and_old_policy_flags_are_usage_errors() -> Result {
    let rig = rig()?.without_state()?;
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
        vec!["route", "resolve", "missing-route"],
        vec!["route", "resolve", "utility", "--route", "utility"],
        vec!["route", "resolve", "utility", "--model", "codex/a"],
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
