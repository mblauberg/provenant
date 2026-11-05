use super::{
    PENDING_REVIEW_SIZE, ShapeQuery, append_plan_additions, review_selector_with_git, size_text,
};
use crate::preset::{Layers, StageBuilder};
use provenant_routing::ReviewConfig;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn selector(review: ReviewConfig) -> super::ShapeSelector {
    review_selector_with_git(
        review,
        PathBuf::from("/repo"),
        None,
        Box::new(|_, _| Ok(String::new())),
    )
}

/// Run `select_items` for a review stage with the given parameters.
fn select(parameters: Value, list: Vec<Value>, caller_supplied: bool) -> Vec<Value> {
    let mut layers = Layers::empty();
    layers.review = Some(selector(ReviewConfig::default()));
    let (mut stack, mut repo_files) = (Vec::new(), Vec::new());
    let mut origins = BTreeMap::new();
    if caller_supplied {
        origins.insert("parameters.lenses".to_owned(), "cli".to_owned());
    }
    StageBuilder {
        layers: &layers,
        name: "review-council",
        prompt: "",
        parameters,
        roles: None,
        stack: &mut stack,
        origins: &mut origins,
        repo_files: &mut repo_files,
        scope: "",
    }
    .select_items(list)
    .unwrap()
}

fn default_lenses() -> Vec<Value> {
    ReviewConfig::default()
        .lenses
        .order
        .iter()
        .map(|name| json!({"name": name, "tier": "", "when": ""}))
        .collect()
}

#[test]
fn shape_hint_carries_groups_and_their_family_preference() {
    let selector = selector(ReviewConfig::default());
    let hint = |shape| {
        selector(&ShapeQuery {
            shape: Some(shape),
            ..Default::default()
        })
        .unwrap()
    };
    let standard = hint("standard");
    assert_eq!(standard.groups.len(), 1);
    assert!(standard.groups[0].distinct_families);
    assert!(!hint("quick").groups[0].distinct_families);

    let deep = hint("deep");
    assert_eq!(deep.groups.len(), 2);
    let owners: Vec<_> = (0..9).map(|slot| deep.group_of_slot(slot)).collect();
    assert_eq!(
        owners,
        [0, 0, 0, 0, 1, 1, 1, 1]
            .map(Some)
            .into_iter()
            .chain([None])
            .collect::<Vec<_>>()
    );
}

#[test]
fn select_items_keeps_route_table_and_list_tiers() {
    let table = json!({"model": "claude-sonnet", "effort": "high"});
    let list = json!([{"alias": "junior"}, {"alias": "senior"}]);
    let selected = select(
        json!({}),
        vec![
            json!({"name": "correctness-spec", "tier": table, "when": ""}),
            json!({"name": "tests", "tier": list, "when": ""}),
            json!({"name": "simplicity", "tier": {}, "when": ""}),
            json!({"name": "design-fit-reuse", "tier": "anthropic/claude-sonnet@high", "when": ""}),
        ],
        true,
    );
    assert_eq!(selected[0]["tier"], table);
    assert_eq!(selected[1]["tier"], list);
    // An empty table names no route, so the shape's pool fills it.
    assert_eq!(selected[2]["tier"], "junior");
    assert_eq!(selected[3]["tier"], "anthropic/claude-sonnet@high");
    // Explicit tiers override the shape and stay out of its family groups.
    assert!(
        selected[..2]
            .iter()
            .all(|item| item.get("family_group").is_none())
    );
}

#[test]
fn select_items_names_the_family_group_of_each_slot() {
    let in_group = |parameters: Value| -> Vec<Option<String>> {
        select(parameters, default_lenses(), false)
            .iter()
            .map(|item| item["family_group"].as_str().map(str::to_owned))
            .collect()
    };
    let standard = in_group(json!({"review_shape": "standard"}));
    assert_eq!(standard, vec![Some("group-0".to_owned()); 5]);

    // quick prefers no spread, so its lens carries no group.
    assert_eq!(in_group(json!({"review_shape": "quick"})), [None]);

    let deep = in_group(json!({"review_shape": "deep"}));
    assert_eq!(deep[3].as_deref(), Some("group-0"));
    assert_eq!(deep[4].as_deref(), Some("group-1"));
}

#[test]
fn review_spec_states_only_what_the_stage_sets() {
    let mut layers = Layers::empty();
    let mut review = ReviewConfig::default();
    review.lenses.by_paths.insert(
        "**/special/**".into(),
        provenant_routing::PathLenses {
            add: vec!["special-review".into()],
        },
    );
    layers.review = Some(selector(review));
    let spec = |parameters: Value| {
        let (mut stack, mut origins, mut repo_files) = (Vec::new(), BTreeMap::new(), Vec::new());
        StageBuilder {
            layers: &layers,
            name: "review-council",
            prompt: "",
            parameters,
            roles: None,
            stack: &mut stack,
            origins: &mut origins,
            repo_files: &mut repo_files,
            scope: "",
        }
        .review_spec(
            json!({"min_successful":"${min_reviews}"})
                .as_object()
                .unwrap(),
        )
        .map(|spec| serde_json::to_value(spec).unwrap())
        .unwrap()
    };
    let table = json!({"model": "claude-sonnet"});
    let spec_json = spec(json!({
        "review_shape": "deep",
        "risk_tier": "crucial",
        "risk_tags": ["security"],
        "min_reviews": 0,
        "lenses": [
            {"name": "correctness-spec", "tier": table, "when": ""},
            {"name": "security-privacy", "tier": "", "when": "security"},
        ],
    }));
    assert_eq!(spec_json["shape"], "deep");
    assert_eq!(spec_json["risk_tier"], "crucial");
    // The host measures an unset size at launch; no placeholder is recorded as an override.
    assert!(spec_json["size"].is_null());
    assert_eq!(spec_json["min_successful"], 2);
    assert_eq!(spec_json["lenses"][0]["tier"], table);
    assert!(spec_json["lenses"][1]["tier"].is_null());
    assert_eq!(spec_json["lenses"][1]["when"], "security");
    // The root, test-marker lens and a possible changed-path lens are budgeted.
    assert_eq!(spec_json["max_nodes"], 5);

    let sized = spec(json!({"size": "large", "min_reviews": 2}));
    assert_eq!(sized["size"], "large");
    assert_eq!(sized["min_successful"], 2);
    // An empty list selects the configured default lenses at launch.
    assert!(sized["max_nodes"].as_u64().unwrap() > 2);
}

#[test]
fn unknown_size_reads_pending_only_when_launch_measures_it() {
    assert_eq!(size_text(Some("small"), true), "small");
    assert_eq!(size_text(None, false), "unknown");
    assert_eq!(size_text(None, true), PENDING_REVIEW_SIZE);
    assert_eq!(PENDING_REVIEW_SIZE, "pending (measured at review launch)");
}

#[test]
fn default_preset_scans_test_paths_without_changing_shape_selection() {
    let mut review = ReviewConfig::default();
    review
        .select
        .by_paths
        .insert("**/tests/**".into(), "deep".into());
    let selector = review_selector_with_git(
        review,
        PathBuf::from("/repo"),
        None,
        Box::new(|_, args| {
            Ok(match args[0] {
                "symbolic-ref" => "main\n".into(),
                "rev-parse" => "main\n".into(),
                "diff" if args.contains(&"--name-only") => "crates/x/tests/a.rs\0".into(),
                "diff" if args.contains(&"--unified=0") => "@@ -1,0 +2 @@\n+#[test]\n".into(),
                _ => String::new(),
            })
        }),
    );
    let hint = selector(&ShapeQuery {
        risk: Some("routine"),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(hint.name, "quick");
    assert!(
        hint.source.contains("by_paths:**/tests/**"),
        "{}",
        hint.source
    );
    assert!(hint.source.contains("test-marker"), "{}", hint.source);
    assert_eq!(hint.plan.add, ["tests"]);
}

#[test]
fn plan_additions_are_included_once_even_when_lenses_are_caller_supplied() {
    let mut lenses = vec![serde_json::json!({"name": "review"})];
    append_plan_additions(&mut lenses, &["tests".into(), "review".into()]);
    assert_eq!(
        lenses,
        [
            serde_json::json!({"name": "review"}),
            serde_json::json!({"name": "tests"})
        ]
    );
}
