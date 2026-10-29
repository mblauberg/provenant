use super::*;
use crate::AccountHome;

fn default_claude(f: &Fixture) -> PathBuf {
    f.home.join(".claude")
}
fn account(f: &Fixture, label: &str) -> AccountHome {
    let home = f.home.join(format!(".claude-{label}"));
    fs::create_dir_all(&home).unwrap();
    AccountHome {
        harness: Harness::Claude,
        label: label.into(),
        home,
    }
}
/// One batch of the process edge: a single target, hooks on, off or inherited.
fn apply(f: &mut Fixture, harness: Harness, base: &Path, hooks: Option<bool>) -> InstallOutcome {
    f.targets.harnesses = vec![HarnessTarget::at(harness, &f.home, base)];
    f.targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
    f.targets.hooks = hooks == Some(true);
    f.targets.remove_hooks = hooks == Some(false);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap()
}
fn rules(f: &Fixture) -> Vec<(String, PathBuf)> {
    let inspection = inspect_install(
        &f.targets,
        None,
        "fixture-target",
        &Smoke,
        &CancelToken::new(),
    )
    .unwrap();
    assert!(inspection.error.is_none());
    inspection
        .findings
        .into_iter()
        .map(|finding| (finding.rule, finding.path.to_path_buf().unwrap()))
        .collect()
}
fn hook_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|line| line.contains("'hook' 'claude'"))
        .map(|line| line.trim().to_owned())
        .collect()
}

#[test]
fn a_registered_account_is_unsynced_until_install_reaches_it() {
    let mut f = Fixture::new();
    let claude = default_claude(&f);
    let work = account(&f, "work");
    apply(&mut f, Harness::Claude, &claude, None);
    f.targets.accounts = vec![work.clone()];
    assert_eq!(
        rules(&f),
        vec![("unsynced_account".to_owned(), work.home.clone())]
    );

    apply(&mut f, work.harness, &work.home, None);
    assert!(rules(&f).is_empty());
    assert!(fs::symlink_metadata(work.home.join("skills/example")).is_ok());
    let mcp: serde_json::Value =
        serde_json::from_slice(&fs::read(work.home.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["fabric"]["type"], "stdio");
}

#[test]
fn a_registered_account_without_a_directory_is_reported_not_created() {
    let mut f = Fixture::new();
    let claude = default_claude(&f);
    apply(&mut f, Harness::Claude, &claude, None);
    let gone = f.home.join(".claude-gone");
    f.targets.accounts = vec![AccountHome {
        harness: Harness::Claude,
        label: "gone".into(),
        home: gone.clone(),
    }];
    assert_eq!(
        rules(&f),
        vec![("missing_account_directory".to_owned(), gone.clone())]
    );
    assert!(!gone.exists());
}

#[test]
fn a_new_account_takes_the_hooks_and_leaves_user_settings_alone() {
    let mut f = Fixture::new();
    let claude = default_claude(&f);
    let work = account(&f, "work");
    let settings = work.home.join("settings.json");
    fs::write(
        &settings,
        "{\n  \"model\": \"opus\",\n  \"hooks\": {\"Stop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"mine\"}]}]}\n}\n",
    )
    .unwrap();
    apply(&mut f, Harness::Claude, &claude, Some(true));
    // No `--hooks` flag: the account follows the targets that own hooks.
    apply(&mut f, work.harness, &work.home, None);
    f.targets.accounts = vec![work.clone()];
    assert!(rules(&f).is_empty());

    let installed = fs::read_to_string(&settings).unwrap();
    assert!(installed.contains("\"model\": \"opus\""), "{installed}");
    assert!(installed.contains("\"command\": \"mine\""), "{installed}");
    assert_eq!(hook_lines(&settings).len(), 3);
    assert_eq!(
        hook_lines(&settings),
        hook_lines(&f.home.join(".claude/settings.json"))
    );
}

#[test]
fn a_sibling_hook_missing_from_an_account_is_drift_that_install_repairs() {
    let mut f = Fixture::new();
    let claude = default_claude(&f);
    let work = account(&f, "work");
    apply(&mut f, work.harness, &work.home, None);
    apply(&mut f, Harness::Claude, &claude, Some(true));
    f.targets.accounts = vec![work.clone()];
    let drift = rules(&f);
    assert_eq!(drift.len(), 3, "{drift:?}");
    assert!(
        drift
            .iter()
            .all(|(rule, path)| rule == "account_missing_entry"
                && path == &work.home.join("settings.json"))
    );

    apply(&mut f, work.harness, &work.home, None);
    assert!(rules(&f).is_empty());
    assert_eq!(
        hook_lines(&work.home.join("settings.json")),
        hook_lines(&f.home.join(".claude/settings.json"))
    );
}

#[test]
fn explicit_hooks_off_reaches_every_target_and_leaves_no_drift() {
    let mut f = Fixture::new();
    let claude = default_claude(&f);
    let work = account(&f, "work");
    apply(&mut f, Harness::Claude, &claude, Some(true));
    apply(&mut f, work.harness, &work.home, None);
    for base in [claude.clone(), work.home.clone()] {
        apply(&mut f, Harness::Claude, &base, Some(false));
    }
    f.targets.accounts = vec![work.clone()];
    assert!(rules(&f).is_empty());
    assert!(hook_lines(&work.home.join("settings.json")).is_empty());
}

#[test]
fn sync_never_overwrites_an_edited_hook_and_check_keeps_reporting_it() {
    let mut f = Fixture::new();
    let claude = default_claude(&f);
    let work = account(&f, "work");
    apply(&mut f, Harness::Claude, &claude, Some(true));
    apply(&mut f, work.harness, &work.home, None);
    let settings = work.home.join("settings.json");
    let edited = fs::read_to_string(&settings)
        .unwrap()
        .replace("\"timeout\": 5", "\"timeout\": 123");
    assert!(edited.contains("123"));
    fs::write(&settings, &edited).unwrap();

    let outcome = apply(&mut f, work.harness, &work.home, None);
    assert!(
        outcome
            .findings
            .iter()
            .any(|finding| finding.rule == "relinquished_modified_entry")
    );
    assert_eq!(fs::read_to_string(&settings).unwrap(), edited);
    f.targets.accounts = vec![work.clone()];
    let drift = rules(&f);
    assert!(!drift.is_empty());
    assert!(
        drift
            .iter()
            .all(|(rule, path)| rule == "account_missing_entry" && path == &settings)
    );
}

#[test]
fn accounts_of_other_adapters_get_their_own_managed_entries() {
    let mut f = Fixture::new();
    let codex = f.home.join(".codex-work");
    fs::create_dir_all(&codex).unwrap();
    let account = AccountHome {
        harness: Harness::Codex,
        label: "work".into(),
        home: codex.clone(),
    };
    let default = f.home.join(".codex");
    apply(&mut f, Harness::Codex, &default, None);
    apply(&mut f, account.harness, &account.home, None);
    f.targets.accounts = vec![account];
    assert!(rules(&f).is_empty());
    assert!(fs::symlink_metadata(codex.join("skills/example")).is_ok());
    let config = fs::read_to_string(codex.join("config.toml")).unwrap();
    assert!(config.contains("mcp_servers"), "{config}");
}

fn shared(f: &Fixture, source: serde_json::Value) -> crate::shared_settings::SharedSettings {
    let serde_json::Value::Object(object) = source else {
        panic!("source must be an object")
    };
    crate::shared_settings::SharedSettings {
        source_file: f.home.join(".claude/settings.json"),
        source: object.into_iter().collect(),
        keys: crate::shared_settings::DEFAULT_KEYS
            .map(str::to_owned)
            .to_vec(),
        overrides: BTreeMap::new(),
    }
}
fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn apply_result(f: &mut Fixture, base: &Path, force: bool) -> Result<InstallOutcome, InstallError> {
    f.targets.harnesses = vec![HarnessTarget::at(Harness::Claude, &f.home, base)];
    f.targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
    f.targets.force_settings = force;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    install(
        &f.plan(),
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
}

#[test]
fn shared_keys_reach_every_account_and_a_per_account_value_wins() {
    let mut f = Fixture::new();
    let (work, play) = (account(&f, "work"), account(&f, "play"));
    fs::write(
        play.home.join("settings.json"),
        "{\n  \"model\": \"opus\",\n  \"apiKeyHelper\": \"/bin/helper\"\n}\n",
    )
    .unwrap();
    let mut settings = shared(
        &f,
        serde_json::json!({
            "permissions": {"allow": ["Bash(ls)"]},
            "outputStyle": "terse",
            "env": {"EDITOR": "vim"},
            "model": "sonnet",
            "apiKeyHelper": "/bin/leak",
        }),
    );
    settings.overrides.insert(
        work.home.clone(),
        BTreeMap::from([("outputStyle".to_owned(), serde_json::json!("verbose"))]),
    );
    f.targets.shared_settings = Some(settings);
    f.targets.accounts = vec![work.clone(), play.clone()];
    for account in [&work, &play] {
        apply_result(&mut f, &account.home, false).unwrap();
    }
    let work_settings = read_json(&work.home.join("settings.json"));
    let play_settings = read_json(&play.home.join("settings.json"));
    assert_eq!(work_settings["outputStyle"], "verbose");
    assert_eq!(play_settings["outputStyle"], "terse");
    for settings in [&work_settings, &play_settings] {
        assert_eq!(settings["permissions"]["allow"][0], "Bash(ls)");
        assert_eq!(settings["env"]["EDITOR"], "vim");
        // Not on the allow-list, so neither copied nor touched.
        assert!(settings.get("model").is_none_or(|m| m == "opus"));
    }
    assert_eq!(play_settings["model"], "opus");
    assert_eq!(play_settings["apiKeyHelper"], "/bin/helper");
    assert!(work_settings.get("apiKeyHelper").is_none());
    assert!(rules(&f).is_empty(), "{:?}", rules(&f));
    // Re-running changes nothing.
    assert!(
        apply_result(&mut f, &play.home, false)
            .unwrap()
            .actions
            .is_empty()
    );
}

#[test]
fn a_locally_edited_shared_key_is_a_conflict_until_forced() {
    let mut f = Fixture::new();
    let work = account(&f, "work");
    f.targets.shared_settings = Some(shared(
        &f,
        serde_json::json!({"outputStyle": "terse", "cleanupPeriodDays": 7}),
    ));
    f.targets.accounts = vec![work.clone()];
    apply_result(&mut f, &work.home, false).unwrap();
    let path = work.home.join("settings.json");
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("\"terse\"", "\"mine\"");
    fs::write(&path, &edited).unwrap();
    // The source moves on; the account's own edit must survive.
    f.targets.shared_settings = Some(shared(
        &f,
        serde_json::json!({"outputStyle": "formal", "cleanupPeriodDays": 9}),
    ));
    let outcome = apply_result(&mut f, &work.home, false).unwrap();
    let conflicts: Vec<_> = outcome
        .findings
        .iter()
        .filter(|finding| finding.rule == "settings_conflict")
        .map(|finding| finding.detail.clone().unwrap())
        .collect();
    assert_eq!(conflicts, ["account work key outputStyle"]);
    let now = read_json(&path);
    assert_eq!(now["outputStyle"], "mine");
    assert_eq!(now["cleanupPeriodDays"], 9, "unedited keys still follow");
    let check = rules(&f);
    assert!(check.iter().any(|(rule, _)| rule == "settings_conflict"));

    apply_result(&mut f, &work.home, true).unwrap();
    assert_eq!(read_json(&path)["outputStyle"], "formal");
    assert!(rules(&f).is_empty(), "{:?}", rules(&f));
}

#[test]
fn a_value_the_account_held_before_sync_is_a_conflict_too() {
    let mut f = Fixture::new();
    let work = account(&f, "work");
    let path = work.home.join("settings.json");
    fs::write(&path, "{\"outputStyle\": \"mine\"}\n").unwrap();
    f.targets.shared_settings = Some(shared(&f, serde_json::json!({"outputStyle": "terse"})));
    f.targets.accounts = vec![work.clone()];
    let outcome = apply_result(&mut f, &work.home, false).unwrap();
    assert!(
        outcome
            .findings
            .iter()
            .any(|finding| finding.rule == "settings_conflict")
    );
    assert_eq!(read_json(&path)["outputStyle"], "mine");
}

#[test]
fn adopting_moves_only_locally_edited_allow_listed_keys_into_the_source() {
    let source = b"{\n  // mine\n  \"outputStyle\": \"terse\",\n  \"model\": \"opus\"\n}\n";
    let account = br#"{"outputStyle":"mine","cleanupPeriodDays":3,"model":"haiku","apiKeyHelper":"x","permissions":{"allow":[]}}"#;
    let keys: Vec<String> = [
        "outputStyle",
        "cleanupPeriodDays",
        "permissions",
        "apiKeyHelper",
    ]
    .map(str::to_owned)
    .to_vec();
    let synced = BTreeMap::from([("permissions".to_owned(), serde_json::json!({"allow": []}))]);
    let (bytes, adopted) = crate::shared_settings::adopt(
        source,
        account,
        &keys,
        &synced,
        &BTreeSet::from(["cleanupPeriodDays".to_owned()]),
    )
    .unwrap();
    // Unchanged-since-sync, overridden, deny-listed and unlisted keys stay out.
    assert_eq!(adopted, ["outputStyle"]);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("// mine") && text.contains("\"model\": \"opus\""));
    assert!(text.contains("\"outputStyle\": \"mine\""));
}

#[test]
fn identity_and_credential_keys_are_never_syncable() {
    use crate::shared_settings::{check_key, check_value, denied_key};
    for key in [
        "apiKeyHelper",
        "awsAuthRefresh",
        "awsCredentialExport",
        "forceLoginMethod",
        "forceLoginOrgUUID",
        "otelHeadersHelper",
        "oauthAccount",
        "accessToken",
        "user_credentials",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "authtoken",
        "AUTHTOKEN",
        "refreshtoken",
        "ANTHROPICAUTHTOKEN",
        "GITHUBTOKEN",
        "token2",
        "oauth2",
        "sessionId",
        "COOKIEJAR",
        "bearerValue",
        "dbpasswd",
        "clientsecret",
        "PRIVATEKEY",
        "MYAPIKEY",
    ] {
        assert!(denied_key(key), "{key}");
        assert!(check_key(key).is_err(), "{key}");
    }
    for key in crate::shared_settings::DEFAULT_KEYS {
        assert!(check_key(key).is_ok(), "{key}");
    }
    assert!(check_key("hooks").is_err());
    assert!(check_value("env", &serde_json::json!({"ANTHROPIC_AUTH_TOKEN": "x"})).is_err());
    assert!(check_value("env", &serde_json::json!({"EDITOR": "vim"})).is_ok());
    for name in [
        "AUTHTOKEN",
        "REFRESHTOKEN",
        "ANTHROPICAUTHTOKEN",
        "GITHUBTOKEN",
        "TOKEN2",
        "OAUTH2",
        "MY_SESSION_ID",
    ] {
        let error = check_value("env", &serde_json::json!({ name: "x" })).unwrap_err();
        assert!(error.contains(&format!("`env.{name}`")), "{error}");
        assert!(error.contains("contains"), "{error}");
    }
    // The reason names the substring that matched.
    assert!(check_key("refreshtoken").unwrap_err().contains("`token`"));
}

#[test]
fn a_symlinked_source_keeps_its_link_and_a_concurrent_edit_aborts() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().canonicalize().unwrap().join("real.json");
    let link = real.with_file_name("settings.json");
    fs::write(&real, b"{\"a\": 1}\n").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    crate::shared_settings::publish_source(&link, b"{\"a\": 1}\n", b"{\"a\": 2}\n", || {}).unwrap();
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&real).unwrap(), b"{\"a\": 2}\n");
    // An edit between planning and the swap is detected and nothing is written.
    let error =
        crate::shared_settings::publish_source(&link, b"{\"a\": 2}\n", b"{\"a\": 3}\n", || {
            fs::write(&real, b"{\"a\": 9}\n").unwrap()
        })
        .unwrap_err();
    assert!(matches!(error, InstallError::Drift(_)), "{error:?}");
    assert_eq!(fs::read(&real).unwrap(), b"{\"a\": 9}\n");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_live_edit_between_plan_and_publish_is_not_lost() {
    let mut f = Fixture::new();
    let work = account(&f, "work");
    f.targets.shared_settings = Some(shared(&f, serde_json::json!({"outputStyle": "terse"})));
    f.targets.accounts = vec![work.clone()];
    f.targets.harnesses = vec![HarnessTarget::at(Harness::Claude, &f.home, &work.home)];
    f.targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
    let plan = f.plan();
    // A running Claude session saves its own settings after the snapshot.
    let live = work.home.join("settings.json");
    fs::write(&live, "{\"model\": \"opus\"}\n").unwrap();
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let error = install(
        &plan,
        &context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    assert!(matches!(error, InstallError::Drift(_)), "{error:?}");
    assert_eq!(
        fs::read_to_string(&live).unwrap(),
        "{\"model\": \"opus\"}\n"
    );
}

#[test]
fn unlisted_settings_and_identity_fields_are_byte_preserved() {
    let mut f = Fixture::new();
    let work = account(&f, "work");
    let settings = work.home.join("settings.json");
    let original = "{\n  \"theme\": \"dark\",   \"model\": \"opus\"\n}\n";
    fs::write(&settings, original).unwrap();
    let identity = work.home.join(".claude.json");
    let identity_text =
        "{\n \"oauthAccount\": {\"emailAddress\": \"x@example.com\"},\n \"userID\": \"abc\"\n}\n";
    fs::write(&identity, identity_text).unwrap();
    f.targets.shared_settings = Some(shared(&f, serde_json::json!({"outputStyle": "terse"})));
    f.targets.accounts = vec![work.clone()];
    apply_result(&mut f, &work.home, false).unwrap();
    let text = fs::read_to_string(&settings).unwrap();
    assert!(
        text.contains("\"theme\": \"dark\",   \"model\": \"opus\""),
        "{text}"
    );
    // Only the MCP entry was added to `.claude.json`; everything else is intact.
    let written = fs::read_to_string(&identity).unwrap();
    assert!(written.contains(" \"oauthAccount\": {\"emailAddress\": \"x@example.com\"},"));
    assert!(written.contains(" \"userID\": \"abc\""));
    let value: serde_json::Value = serde_json::from_str(&written).unwrap();
    assert_eq!(value["oauthAccount"]["emailAddress"], "x@example.com");
    assert!(value["mcpServers"]["fabric"].is_object());
}

#[test]
fn the_source_account_is_never_rewritten() {
    let mut f = Fixture::new();
    let claude = default_claude(&f);
    fs::create_dir_all(&claude).unwrap();
    let source = claude.join("settings.json");
    fs::write(&source, "{\"outputStyle\":\"terse\"}\n").unwrap();
    let mut settings = shared(&f, serde_json::json!({"outputStyle": "terse"}));
    settings.overrides.insert(
        claude.clone(),
        BTreeMap::from([("outputStyle".to_owned(), serde_json::json!("other"))]),
    );
    f.targets.shared_settings = Some(settings);
    apply_result(&mut f, &claude, false).unwrap();
    assert_eq!(
        fs::read_to_string(&source).unwrap(),
        "{\"outputStyle\":\"terse\"}\n"
    );
}

#[test]
fn removing_every_hook_from_an_account_persists_across_syncs() {
    let mut f = Fixture::new();
    let claude = default_claude(&f);
    let work = account(&f, "work");
    apply(&mut f, Harness::Claude, &claude, Some(true));
    apply(&mut f, work.harness, &work.home, None);
    let settings = work.home.join("settings.json");
    assert!(!hook_lines(&settings).is_empty());
    fs::write(&settings, "{\n  \"theme\": \"dark\"\n}\n").unwrap();

    for _ in 0..3 {
        apply(&mut f, work.harness, &work.home, None);
        apply(&mut f, Harness::Claude, &claude, None);
        assert!(hook_lines(&settings).is_empty());
    }
    f.targets.accounts = vec![work.clone()];
    assert!(rules(&f).is_empty(), "{:?}", rules(&f));

    // An explicit `--hooks on` ends the relinquishment.
    apply(&mut f, work.harness, &work.home, Some(true));
    assert!(!hook_lines(&settings).is_empty());
    assert!(rules(&f).is_empty());
}

#[test]
fn a_failed_write_mid_sync_reports_exactly_what_completed() {
    let mut f = Fixture::new();
    let claude = default_claude(&f);
    let work = account(&f, "work");
    apply(&mut f, Harness::Claude, &claude, Some(true));
    f.targets.accounts = vec![work.clone()];
    f.targets.harnesses = vec![HarnessTarget::at(Harness::Claude, &f.home, &work.home)];
    f.targets.hook_specs = crate::hooks::CLAUDE_HOOKS.to_vec();
    let plan = f.plan();
    let clock = SystemClock::new();
    let cancel = TestCancelToken::new();
    let _fault = FaultInjection::new(FaultMode::ReceiptPublish, None);
    let error = install(
        &plan,
        &fault_context(&clock, &cancel),
        &mut Reporter::default(),
        &Smoke,
        false,
    )
    .unwrap_err();
    drop(_fault);
    let InstallError::Partial {
        completed,
        remaining,
        ..
    } = error
    else {
        panic!("expected a partial report")
    };
    assert_eq!(completed, plan.actions()[..plan.actions().len() - 1]);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].selector, "receipt");
    // What the report calls completed is on disk; the receipt is not.
    assert!(!hook_lines(&work.home.join("settings.json")).is_empty());
    assert!(work.home.join("skills/example").exists());
    // Re-running the same sync finishes only what remained.
    let recovery = f.plan();
    assert_eq!(recovery.actions().len(), 1);
    assert_eq!(recovery.actions()[0].selector, "receipt");
}

#[test]
fn a_finding_belongs_to_the_account_with_the_longest_matching_directory() {
    let mut f = Fixture::new();
    let outer = account(&f, "outer");
    let inner = AccountHome {
        harness: Harness::Claude,
        label: "inner".into(),
        home: outer.home.join("nested"),
    };
    fs::create_dir_all(&inner.home).unwrap();
    f.targets.accounts = vec![outer.clone(), inner.clone()];
    // Neither is installed: each is unsynced, and only its own.
    let statuses = crate::inspect_accounts(&f.targets).unwrap();
    assert!(statuses.iter().all(|status| status.findings.is_empty()));
    let claude = default_claude(&f);
    apply(&mut f, Harness::Claude, &claude, Some(true));
    apply(&mut f, Harness::Claude, &inner.home, None);
    fs::remove_file(inner.home.join("skills/example")).unwrap();
    let statuses = crate::inspect_accounts(&f.targets).unwrap();
    let by_label = |label: &str| statuses.iter().find(|s| s.label == label).unwrap();
    assert!(!by_label("inner").findings.is_empty());
    assert!(
        by_label("outer")
            .findings
            .iter()
            .all(|finding| { !finding.path.to_path_buf().unwrap().starts_with(&inner.home) })
    );
}
