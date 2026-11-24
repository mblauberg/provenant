use super::NewWorktree;
use serde_json::json;

#[test]
fn declared_worktree_metadata_round_trips_and_rejects_wrong_types() {
    for default_base in [false, true] {
        let worktree = NewWorktree {
            extra: [(
                "future_worktree_field".into(),
                json!({"source": "newer minor"}),
            )]
            .into(),
            default_base,
            base: "main".into(),
            branch: "writer".into(),
        };
        let mut value = serde_json::to_value(&worktree).unwrap();
        let decoded: NewWorktree = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded, worktree);
        for malformed in [json!("true"), json!(1), json!({})] {
            value["default_base"] = malformed;
            assert!(serde_json::from_value::<NewWorktree>(value.clone()).is_err());
        }
    }
}
