use crate::{
    ConfigError, EditOptions, apply_patch, domains::NO_DOMAINS, edit_config, history, revision_of,
    schema, show,
};
use provenant_core::Roots;
use serde_json::json;
use std::path::Path;

fn roots(dir: &Path) -> Roots {
    crate::tests::state_roots(dir)
}
fn patch(
    roots: &Roots,
    value: serde_json::Value,
    options: &EditOptions,
) -> Result<crate::Edited<crate::NoDomains>, ConfigError> {
    edit_config(roots, &NO_DOMAINS, options, |doc, current| {
        apply_patch(&value, doc, current, &NO_DOMAINS)
    })
}
fn text(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("config.toml")).ok()
}

#[test]
fn schema_lists_bounds_defaults_values_and_layers() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(temp.path());
    patch(
        &roots,
        json!({"chair.switch_threshold": 80}),
        &EditOptions::new("test"),
    )
    .unwrap();
    let (config, revision) = show(&roots, &NO_DOMAINS).unwrap();
    let schema = schema(&config, &NO_DOMAINS, revision.clone()).unwrap();
    assert_eq!(schema.revision, revision);
    let field = schema
        .fields
        .iter()
        .find(|f| f.path == "chair.switch_threshold")
        .unwrap();
    let field = serde_json::to_value(field).unwrap();
    assert_eq!(field["type"], "int");
    assert_eq!(field["min"], 1);
    assert_eq!(field["max"], 100);
    assert_eq!(field["default"], 92);
    assert_eq!(field["value"], 80);
    assert_eq!(field["source"], "user");
    assert_eq!(field["restart_required"], false);
}

#[test]
fn census_ignore_schema_apply_and_unset_keep_structured_host_entries() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(temp.path());
    let name = "confinement.census_ignore_unreadable";
    let value = json!([{ "path": "/Library/Daemon", "team_id": "TEAM123" }]);
    let edited = patch(&roots, json!({name: value}), &EditOptions::new("test")).unwrap();
    assert_eq!(edited.config.census_ignore_unreadable().unwrap().len(), 1);
    let (config, revision) = show(&roots, &NO_DOMAINS).unwrap();
    let schema = schema(&config, &NO_DOMAINS, revision).unwrap();
    let field = schema
        .fields
        .iter()
        .find(|field| field.path == name)
        .unwrap();
    assert_eq!(field.kind, "census_ignore_list");
    assert_eq!(field.value, value);
    let cleared = patch(&roots, json!({name: null}), &EditOptions::new("test")).unwrap();
    assert!(
        cleared
            .config
            .census_ignore_unreadable()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn stale_revision_is_refused_naming_the_current_one() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(temp.path());
    let (_, first) = show(&roots, &NO_DOMAINS).unwrap();
    patch(
        &roots,
        json!({"chair.switch_threshold": 70}),
        &EditOptions::new("test"),
    )
    .unwrap();
    let current = revision_of(text(temp.path()).as_deref());
    let mut options = EditOptions::new("test");
    options.expect_revision = Some(&first);
    let error = patch(&roots, json!({"chair.switch_threshold": 60}), &options)
        .err()
        .unwrap();
    assert!(matches!(&error, ConfigError::StaleRevision { current: c } if *c == current));
    assert!(text(temp.path()).unwrap().contains("70"));
}

#[test]
fn concurrent_applies_on_one_revision_commit_exactly_one() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(temp.path());
    let (_, revision) = show(&roots, &NO_DOMAINS).unwrap();
    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = [50, 60, 70, 80]
            .into_iter()
            .map(|n| {
                let (roots, revision) = (&roots, &revision);
                scope.spawn(move || {
                    let mut options = EditOptions::new("test");
                    options.expect_revision = Some(revision);
                    patch(roots, json!({"chair.switch_threshold": n}), &options).map(|_| n)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let won: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(
        won.len(),
        1,
        "{:?}",
        results.iter().map(|r| r.is_ok()).collect::<Vec<_>>()
    );
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| matches!(e, ConfigError::StaleRevision { .. }))
    );
    assert!(text(temp.path()).unwrap().contains(&won[0].to_string()));
}

#[test]
fn partial_invalid_patch_writes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(temp.path());
    patch(
        &roots,
        json!({"chair.switch_threshold": 70}),
        &EditOptions::new("test"),
    )
    .unwrap();
    let before = text(temp.path());
    let entries = history(&roots, None, 100).unwrap().len();
    for bad in [
        json!({"chair.switch_threshold": 60, "chair.auto_switch": "maybe"}),
        json!({"chair.switch_threshold": 60, "not.a.field": 1}),
        json!({"chair.switch_threshold": 600}),
    ] {
        assert!(patch(&roots, bad, &EditOptions::new("test")).is_err());
        assert_eq!(text(temp.path()), before);
    }
    assert_eq!(history(&roots, None, 100).unwrap().len(), entries);
}

#[test]
fn dry_run_reports_the_change_and_writes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(temp.path());
    let mut options = EditOptions::new("test");
    options.dry_run = true;
    let edited = patch(&roots, json!({"chair.switch_threshold": 70}), &options).unwrap();
    assert!(edited.dry_run);
    assert_eq!(edited.changes.len(), 1);
    assert_ne!(edited.before_revision, edited.after_revision);
    assert_eq!(text(temp.path()), None);
    assert!(!temp.path().join("config-history.jsonl").exists());
}

#[test]
fn history_records_changes_and_redacts_nothing_secret_looking() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(temp.path());
    let edited = patch(
        &roots,
        json!({"chair.switch_threshold": 70}),
        &EditOptions::new("mcp"),
    )
    .unwrap();
    let entries = history(&roots, None, 10).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].channel, "mcp");
    assert_eq!(entries[0].after_revision, edited.after_revision);
    assert_eq!(entries[0].changes[0].path, "knobs.chair.switch_threshold");
    assert_eq!(entries[0].changes[0].after, Some(json!(70)));
    let future = "2999-01-01T00:00:00Z".parse().unwrap();
    assert!(history(&roots, Some(future), 10).unwrap().is_empty());
}
