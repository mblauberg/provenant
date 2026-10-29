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
