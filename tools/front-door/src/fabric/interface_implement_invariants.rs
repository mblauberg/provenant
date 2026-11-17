mod implementation_graph_contract {
    use super::*;

    #[test]
    fn a_planned_child_keeps_its_writer_controls_below_the_plan_boundary() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let request = factory.child_request(&json!({
            "op":"run","template":"implement","brief":"Implement the feature",
            "worktree":".worktrees/feature","write_scope":["src"],"set":{"plan":true}
        }))?;
        assert!(request.has_writers());
        let node = &request.launch.as_ref().ok_or("compiled launch")?.node;
        let children = node["composite"]["nodes"]
            .as_array()
            .ok_or("plan and writer")?;
        let writer = children
            .iter()
            .find(|node| node["access"] == "worktree_write")
            .ok_or("writer")?;
        let expected = rig.project.join(".worktrees/feature");
        assert_eq!(
            writer["worktree"].as_str().map(std::path::Path::new),
            Some(expected.as_path())
        );
        assert_eq!(writer["write_scope"], json!(["src"]));
        assert!(
            children
                .iter()
                .any(|node| node["template"] == "plan" && node["access"] == "read_only")
        );
        Ok(())
    }

    // Spec: docs/specs/fabric.md#implementation-planning
    #[test]
    fn unbounded_junior_warning_is_advisory_and_configurable() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(factory.clock.mono_now(), 10)?;
        let input = json!({"op":"run", "template":"implement", "brief":"Fix it",
            "worktree":rig.project, "explain":true});
        let preview = factory.graph_document(input.clone(), budget, &cancel)?;
        assert!(preview["warnings"].as_array().unwrap().iter().any(|warning|
            warning.as_str().unwrap().contains("unbounded_brief_junior_writer") && warning.as_str().unwrap().contains("--plan")));
        assert_eq!(preview["implement_plan"]["value"], false);
        let document: GraphDocument = serde_json::from_value(preview["document"].clone())?;
        let expanded = document.expanded(&factory.context.config.graph.limits)?;
        assert!(implementation_warnings(&expanded.nodes, &input, false).is_empty());
        let mut explicit = input;
        explicit["set"] = json!({"plan":false});
        let preview = factory.graph_document(explicit, budget, &cancel)?;
        assert!(preview["warnings"].as_array().unwrap().is_empty());
        Ok(())
    }
}
