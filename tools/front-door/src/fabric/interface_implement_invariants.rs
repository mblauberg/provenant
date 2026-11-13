mod implementation_graph_contract {
    use super::*;

    #[test]
    fn an_unplanned_child_keeps_its_writer_controls_below_the_plan_boundary() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let request = factory.child_request(&json!({
            "op":"run","template":"implement","brief":"Implement the feature",
            "worktree":".worktrees/feature","write_scope":["src"]
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
        assert_eq!(writer["worktree"], ".worktrees/feature");
        assert_eq!(writer["write_scope"], json!(["src"]));
        assert!(
            children
                .iter()
                .any(|node| node["template"] == "plan" && node["access"] == "read_only")
        );
        Ok(())
    }
}
