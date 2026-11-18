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

    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn child_launch_masks_typed_credentials_before_template_binding() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let secret = "opaque-child-credential-value";
        let brief = format!("Review token=abc; password: run and {secret}");
        let safe_brief = "Review token=abc; password: run and [REDACTED]";
        let request = factory.child_request(&json!({
            "op":"run",
            "document": {
                "use":"credential-task",
                "templates": {
                    "credential-task": {
                        "parameters": {"api_key":{"type":"string"}},
                        "node": {"acceptance_checks":["Use ${api_key}"]}
                    }
                }
            },
            "brief":brief,
            "set":{"api_key":secret}
        }))?;

        assert_eq!(request.prompt, safe_brief);
        let launch = request.launch.as_ref().ok_or("compiled child launch")?;
        assert_eq!(launch.node["prompt"], safe_brief);
        assert_eq!(launch.node["acceptance_checks"][0], "Use [REDACTED]");
        assert!(
            launch.input_warnings.iter().any(|warning| {
                warning.contains("field `graph.nodes.0.with.api_key` bytes 0..")
            })
        );
        assert!(!serde_json::to_string(&request)?.contains(secret));
        Ok(())
    }

    // Spec: docs/specs/fabric.md#checkpoint-envelope
    #[test]
    fn child_launch_masks_credentials_assembled_during_template_binding() -> Result {
        let rig = TestRig::new()?;
        let factory = factory(&rig)?;
        let suffix = "Z".repeat(16);
        let assembled = format!("AKIA{suffix}");
        let request = factory.child_request(&json!({
            "op":"run",
            "document": {
                "use":"joined-task",
                "templates": {
                    "joined-task": {
                        "parameters": {
                            "prefix":{"type":"string"},
                            "suffix":{"type":"string"}
                        },
                        "node": {
                            "acceptance_checks":["${prefix}${suffix}"]
                        }
                    }
                }
            },
            "brief":"Review the implementation",
            "set":{"prefix":"AKIA", "suffix":suffix}
        }))?;

        let launch = request.launch.as_ref().ok_or("compiled child launch")?;
        assert_eq!(launch.node["acceptance_checks"][0], "[REDACTED]");
        assert!(launch.input_warnings.iter().any(|warning| {
            warning.contains("field `graph.nodes.0.acceptance_checks.0` bytes 0..20")
        }));
        assert!(!serde_json::to_string(&request)?.contains(&assembled));
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
        assert!(
            preview["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning
                    .as_str()
                    .unwrap()
                    .contains("unbounded_brief_junior_writer")
                    && warning.as_str().unwrap().contains("--plan"))
        );
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
