mod relay_provisioning {
    use super::*;

    #[test]
    fn relayed_implement_keeps_source_and_fence_on_the_writer() {
        let (mut document, mut checkpoint) = live(None);
        document.integration_branch = Some("integration".into());
        let library = template::TemplateLibrary::bundled();
        let resolvers = Default::default();
        let limits = plans::generous();
        let context = patch::PatchContext {
            journal: None,
            library: &library,
            resolvers: &resolvers,
            scan: &|_| Ok(()),
            limits: &limits,
        };
        let request: relay::ChildRequest = serde_json::from_value(json!({
            "id": "planned",
            "brief": "Implement a bounded slice",
            "writer": true,
            "write_scope": ["src/parser"],
            "idempotency_key": "planned-implement",
            "launch": {"node": {
                "id": "planned",
                "use": "implement",
                "brief": "Implement a bounded slice",
                "write_scope": ["src/parser"]
            }}
        }))
        .unwrap();
        assert!(request.needs_worktree_provisioning());
        let source = "a".repeat(40);
        let receipt = relay::admit_child_bound(
            &mut patch::LiveGraph {
                document: &document,
                checkpoint: &mut checkpoint,
            },
            "a",
            &request,
            &context,
            Some(("integration", &source)),
        )
        .unwrap();
        let path = receipt.worktree.as_deref().unwrap();
        assert_eq!(receipt.node, "a--planned");

        let expanded = plans::expand(
            &document,
            &checkpoint.extensions,
            &checkpoint.expansions,
            &limits,
        )
        .unwrap();
        let boundary = expanded
            .nodes
            .iter()
            .find(|node| node.id == receipt.node)
            .unwrap();
        let commit_export = boundary
            .composite
            .as_ref()
            .unwrap()
            .exports
            .iter()
            .find(|export| export.outputs.contains(&OutputKind::Commit))
            .unwrap();
        let writer = expanded
            .nodes
            .iter()
            .find(|node| node.id == commit_export.node)
            .unwrap();
        assert_eq!(writer.worktree.as_deref(), Some(path));
        assert_eq!(
            writer.source,
            Some(SourceBinding::Commit {
                commit: source.clone()
            })
        );
        assert_eq!(
            relay::child_source(&document, &checkpoint, &receipt.node, &limits).unwrap(),
            Some(source.clone())
        );

        relay::fence_provisioning(&mut checkpoint, &receipt.node, path, &source);
        let writer_spec = checkpoint
            .expansions
            .iter()
            .flat_map(|expansion| &expansion.nodes)
            .find(|node| {
                node.id.starts_with(&format!("{}--", receipt.node))
                    && node.access == AccessMode::WorktreeWrite
                    && node.worktree.as_deref() == Some(path)
            })
            .expect("the flattened writer spec owns the provisioned path");
        assert_eq!(
            writer_spec.extra.get("provision_worktree"),
            Some(&json!(true))
        );
        assert_eq!(
            relay::pending_provisioning(&checkpoint),
            vec![(receipt.node, path.to_owned(), source),]
        );
    }
}
