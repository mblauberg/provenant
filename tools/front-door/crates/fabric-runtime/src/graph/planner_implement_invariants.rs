mod final_fragment {
    use super::*;

    #[test]
    fn planner_emitted_implement_is_a_direct_bounded_writer() {
        let document = planner(1);
        let output = result(vec![json!({
            "id": "slice",
            "use": "implement",
            "brief": "Implement the sized parser slice",
            "worktree": ".worktrees/slice",
            "write_scope": ["src/parser"],
        })]);
        let mut owner = fixture_owner(&[&output, "implemented", "checked"]);
        let mut journal = MemoryJournal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(
            run.status,
            GraphStatus::Complete,
            "{:?}",
            run.checkpoint.pause_reason()
        );

        let expanded = plans::expand(
            &document,
            &run.checkpoint.extensions,
            &run.checkpoint.expansions,
            &plans::generous(),
        )
        .unwrap();
        let writer = expanded
            .nodes
            .iter()
            .find(|node| node.id == "plan--slice")
            .unwrap();
        assert_eq!(writer.access, AccessMode::WorktreeWrite);
        assert_eq!(writer.worktree.as_deref(), Some(".worktrees/slice"));
        assert_eq!(writer.write_scope, ["src/parser"]);
        assert_eq!(
            writer
                .route
                .as_ref()
                .and_then(|route| route.alias.as_deref()),
            Some("junior")
        );
        assert!(
            writer.composite.is_none(),
            "a planner-emitted slice is already bounded"
        );
        assert!(writer.plan.is_none());
    }
}
