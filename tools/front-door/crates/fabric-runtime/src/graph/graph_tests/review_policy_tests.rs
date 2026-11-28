use super::*;
use provenant_records::Layer;

#[test]
fn admission_resolves_template_launch_and_route_review_policy_sources() {
    let mut document = graph(vec![node("review", route("junior"))]);
    document.schema_version = 2;
    document.review_policy = Some(ReviewPolicyOverride {
        accept_below: Some(AcceptBelow::Blocking),
        ..Default::default()
    });
    let mut authored = template::TemplateLibrary::bundled().snapshot();
    authored
        .get_mut("task")
        .expect("bundled task template")
        .review_policy = Some(ReviewPolicyOverride {
        later_rounds: Some(ReviewRounds::Full),
        accept_below: Some(AcceptBelow::Low),
        adjudicator_route: Some("senior".into()),
        evidence_retries: Some(4),
        ..Default::default()
    });
    document.template_library = authored;
    document.template_layers = template::TemplateLibrary::bundled().snapshot_layers();
    document.nodes[0].template = Some("task".to_owned());
    document.nodes[0].effective_review_policy = Some(ReviewPolicy::default());
    let route = document.nodes[0].route.as_mut().expect("route");
    route.review_policy = Some(ReviewPolicyOverride {
        adjudicator_route: Some("principal".into()),
        lens_retry: Some(3),
        ..Default::default()
    });
    route.effective_review_policy = Some(ReviewPolicy::default());

    let mut project_policy = ReviewPolicy::default();
    project_policy.accept_below.source = provenant_records::Source {
        layer: Layer::Project,
        origin: Some(".provenant/config.toml".into()),
    };
    let admitted = candidate::admit_candidate_with_review_policy(
        &document,
        None,
        &[],
        &plans::generous(),
        &Default::default(),
        None,
        &project_policy,
    )
    .expect("policy resolves during production admission");
    let resolved_node = &admitted.expanded().nodes[0];
    let node_policy = resolved_node
        .effective_review_policy
        .as_ref()
        .expect("host-sealed node policy");
    assert_eq!(node_policy.accept_below.value, AcceptBelow::Blocking);
    assert_eq!(node_policy.accept_below.source.layer, Layer::Launch);
    assert_eq!(node_policy.later_rounds.value, ReviewRounds::Full);
    assert_eq!(node_policy.later_rounds.source.layer, Layer::Template);
    assert_eq!(node_policy.evidence_retries.value, 4);
    assert_eq!(node_policy.evidence_retries.source.layer, Layer::Template);
    let resolved_route = resolved_node.route.as_ref().expect("resolved route");
    let route_policy = resolved_route
        .effective_review_policy
        .as_ref()
        .expect("host-sealed route policy");
    assert_eq!(route_policy.adjudicator_route.value, "principal");
    assert_eq!(route_policy.adjudicator_route.source.layer, Layer::Route);
    assert_eq!(route_policy.lens_retry.value, 3);
    assert_eq!(route_policy.lens_retry.source.layer, Layer::Route);
}

#[test]
fn launch_cannot_override_route_only_lens_retry() {
    let mut document = graph(vec![node("review", route("junior"))]);
    document.schema_version = 2;
    document.review_policy = Some(ReviewPolicyOverride {
        lens_retry: Some(2),
        ..Default::default()
    });
    let refused = candidate::admit_candidate_with_review_policy(
        &document,
        None,
        &[],
        &plans::generous(),
        &Default::default(),
        None,
        &ReviewPolicy::default(),
    );
    assert!(refused.is_err(), "scope enforcement is part of admission");
}

#[test]
fn unavailable_review_lens_retries_on_next_route_and_replays_without_an_extra_attempt() {
    let head = "1".repeat(40);
    let mut first = route("junior");
    first.review_policy = Some(ReviewPolicyOverride {
        lens_retry: Some(1),
        ..Default::default()
    });
    let second = route("senior");
    let mut lens = node("lens", first.clone());
    lens.role = Some("lens.correctness".into());
    lens.source = Some(SourceBinding::Commit {
        commit: head.clone(),
    });
    lens.route = None;
    lens.routes = Some(vec![first, second]);
    let mut document = graph(vec![lens]);
    document.schema_version = 2;
    document.subject = Some(provenant_records::subject::PinnedSubject {
        extra: Default::default(),
        base: None,
        head: provenant_core::GitOid::parse(&head).unwrap(),
        origin: provenant_records::subject::SubjectOrigin::LocalRef {
            name: "main".into(),
        },
        pinned_at: provenant_core::WallTime::from_micros(1).unwrap(),
        policy: provenant_records::subject::SubjectPolicy::Pinned,
        repository: None,
        base_ref: None,
        policy_source: None,
        followed_from: Vec::new(),
    });
    let mut global_policy = ReviewPolicy::default();
    global_policy.lens_retry.value = 0;
    let mut owner = FakeOwner {
        observations: [
            terminal(GraphAttemptOutcome::ModelUnavailable),
            terminal(GraphAttemptOutcome::Succeeded),
        ]
        .into(),
        resolved_review_policy: Some(global_policy),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let first_run = advance(&document, &mut owner, &mut journal).unwrap();
    let progress = &first_run.checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Succeeded);
    assert_eq!(progress.previous_attempts.len(), 1);
    assert_eq!(progress.attempts.len(), 1);
    assert_ne!(
        progress.previous_attempts[0].route,
        progress.attempts[0].route
    );
    let retry = progress.review_retry.as_ref().unwrap();
    assert_eq!(retry.lens_retries, 1);
    assert!(retry.pending_retry.is_none());
    assert_eq!(owner.calls.len(), 2);
    assert_eq!(owner.calls[0].1.adapter.as_deref(), Some("junior"));
    assert_eq!(owner.calls[1].1.adapter.as_deref(), Some("senior"));

    let replay = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(replay.checkpoint, first_run.checkpoint);
    assert_eq!(owner.calls.len(), 2);
}
