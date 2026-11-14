use super::*;
use provenant_core::{AttemptId, HostId};
use provenant_records::CanonicalV1;
use provenant_records::custody::*;
fn holder(who: u8) -> Holder {
    Holder::Chair {
        chair: Chair {
            extra: Default::default(),
            host: HostId::from_bytes([1; 16]),
            actor: actor(who),
            session: Some(format!("session-{who}")),
            process: None,
        },
    }
}
#[test]
fn custody_is_durable_advisory_and_queryable_by_declared_kinds() {
    let f = Fixture::new();
    let ledger = HostStore::open(&f.root, &f.request()).unwrap();
    let original = holder(1);
    let adopter = holder(2);
    for &kind in SubjectKind::ALL {
        let id = if kind.is_path() {
            f.temp
                .path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        } else {
            format!("{kind}-item")
        };
        let subject = Subject::new(kind, id);
        ledger
            .acquire_custody(
                project(),
                &subject,
                &original,
                Origin::Provenant,
                &f.request(),
            )
            .unwrap();
        let changed = ledger
            .acquire_custody(project(), &subject, &adopter, Origin::Adopted, &f.request())
            .unwrap();
        assert_eq!(changed.previous_holder, Some(original.clone()));
        assert_eq!(changed.record.holder, adopter);
        assert_eq!(changed.record.state, CustodyState::Active);
        ledger
            .change_custody_state(
                project(),
                &subject,
                CustodyState::Released,
                adopter.actor(),
                &f.request(),
            )
            .unwrap();
        assert_eq!(
            ledger
                .custody_item(project(), &subject, &f.request())
                .unwrap()
                .unwrap()
                .state,
            CustodyState::Released
        );
    }
    let active = ledger
        .custody(
            &CustodyQuery {
                state: Some(StateKind::Active),
                ..Default::default()
            },
            &f.request(),
        )
        .unwrap();
    assert!(active.items.is_empty());
    let released = ledger
        .custody(
            &CustodyQuery {
                project: Some(project()),
                state: Some(StateKind::Released),
                holder: Some(adopter),
                ..Default::default()
            },
            &f.request(),
        )
        .unwrap();
    for &kind in SubjectKind::ALL {
        assert!(
            released
                .items
                .iter()
                .any(|record| record.subject.kind == kind)
        );
    }
}

#[test]
fn custody_seed_copies_observed_owner_only_when_target_is_absent() {
    let fixture = Fixture::new();
    let ledger = HostStore::open(&fixture.root, &fixture.request()).unwrap();
    let owner = Subject::new(SubjectKind::Branch, "lane/seed-source");
    let target = Subject::new(SubjectKind::Ref, "endpoint-generation");
    let observed = holder(4);
    ledger
        .acquire_custody(
            project(),
            &owner,
            &observed,
            Origin::Adopted,
            &fixture.request(),
        )
        .unwrap();
    let provenance = CanonicalV1::record_digest("test-owner", 2, &owner).unwrap();
    ledger
        .seal_custody_provenance(project(), &owner, provenance, &fixture.request())
        .unwrap();

    let seeded = ledger
        .ensure_custody_from_existing(project(), &target, &owner, &fixture.request())
        .unwrap();
    assert_eq!(seeded.subject, target);
    assert_eq!(seeded.holder, observed);
    assert_eq!(seeded.origin, Origin::Adopted);
    assert_eq!(seeded.provenance, Some(provenance));
    assert_eq!(seeded.state, CustodyState::Active);

    let landed = CustodyState::Landed {
        evidence: LandedEvidence {
            predicate: LandingPredicate::Landed,
            source: LandingSource::RetainedRef {
                reference: "refs/heads/main".into(),
                commit: "0123456789abcdef0123456789abcdef01234567".parse().unwrap(),
            },
        },
    };
    ledger
        .change_custody_state(project(), &target, landed.clone(), None, &fixture.request())
        .unwrap();
    let unchanged = ledger
        .ensure_custody_from_existing(
            project(),
            &target,
            &Subject::new(SubjectKind::Branch, "missing-source"),
            &fixture.request(),
        )
        .unwrap();
    assert_eq!(unchanged.holder, observed);
    assert_eq!(unchanged.origin, Origin::Adopted);
    assert_eq!(unchanged.provenance, Some(provenance));
    assert_eq!(unchanged.state, landed);
}

#[test]
fn retirement_is_custody_owned_and_resumes_by_revision() {
    let fixture = Fixture::new();
    let ledger = HostStore::open(&fixture.root, &fixture.request()).unwrap();
    let project = project();
    let reference = RefOwnership {
        repository: project,
        endpoint: RefEndpoint::Local,
        reference: "refs/heads/lane/retire".into(),
        generation: 1,
        origin: RefOrigin::Created,
    };
    let subject = reference.custody_subject().unwrap();
    let worktree_root = fixture.temp.path().canonicalize().unwrap();
    let worktree = Subject::new(
        SubjectKind::Worktree,
        worktree_root
            .join("retired-worktree")
            .to_string_lossy()
            .into_owned(),
    );
    let second_worktree = Subject::new(
        SubjectKind::Worktree,
        worktree_root
            .join("second-retired-worktree")
            .to_string_lossy()
            .into_owned(),
    );
    let holder = holder(1);
    for subject in [&subject, &worktree, &second_worktree] {
        ledger
            .acquire_custody(
                project,
                subject,
                &holder,
                Origin::Provenant,
                &fixture.request(),
            )
            .unwrap();
    }
    let expected_tip = "1123456789abcdef0123456789abcdef01234567".parse().unwrap();
    let subsumed_evidence = RemovalEvidence::Landing {
        evidence: LandedEvidence {
            predicate: LandingPredicate::Subsumed,
            source: LandingSource::RetainedRef {
                reference: "refs/heads/main".into(),
                commit: "2123456789abcdef0123456789abcdef01234567".parse().unwrap(),
            },
        },
    };
    let landed_evidence = RemovalEvidence::Landing {
        evidence: LandedEvidence {
            predicate: LandingPredicate::Landed,
            source: LandingSource::IntegrationTarget {
                reference: "refs/heads/main".into(),
                commit: "2123456789abcdef0123456789abcdef01234567".parse().unwrap(),
            },
        },
    };
    ledger
        .change_custody_state(
            project,
            &subject,
            CustodyState::Landed {
                evidence: LandedEvidence {
                    predicate: LandingPredicate::Landed,
                    source: LandingSource::ForgeMerged {
                        pr: "prior-proof".into(),
                        head: expected_tip,
                        merge_commit: None,
                    },
                },
            },
            holder.actor(),
            &fixture.request(),
        )
        .unwrap();
    assert_eq!(reference.custody_subject().unwrap(), subject);
    let record = RetirementRecord::plan(
        project,
        vec![second_worktree.clone(), worktree.clone()],
        reference,
        expected_tip,
        RetirementDecision::Discard {
            authority: actor(1),
            evidence: subsumed_evidence.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        RetirementRecord::identity(
            project,
            &[worktree.clone(), second_worktree.clone()],
            &record.reference,
            record.expected_tip,
            &record.decision,
        )
        .unwrap(),
        record.id
    );
    let retirement_identity = |reference: &RefGeneration, tip, decision: &RetirementDecision| {
        RetirementRecord::identity(project, &record.related_subjects, reference, tip, decision)
            .unwrap()
    };
    let changed_generation = RefGeneration {
        generation: record.reference.generation + 1,
        ..record.reference.clone()
    };
    assert_ne!(
        retirement_identity(&changed_generation, record.expected_tip, &record.decision),
        record.id
    );
    let changed_tip = "2123456789abcdef0123456789abcdef01234567".parse().unwrap();
    assert_ne!(
        retirement_identity(&record.reference, changed_tip, &record.decision),
        record.id
    );
    let mut changed_authority = record.decision.clone();
    let mut changed_proof = record.decision.clone();
    if let RetirementDecision::Discard { authority, .. } = &mut changed_authority {
        *authority = actor(2);
    }
    if let RetirementDecision::Discard {
        evidence: RemovalEvidence::Landing { evidence },
        ..
    } = &mut changed_proof
    {
        evidence.source = LandingSource::RetainedRef {
            reference: "refs/heads/lane/retire".into(),
            commit: record.expected_tip,
        };
    }
    assert_ne!(
        retirement_identity(&record.reference, record.expected_tip, &changed_authority),
        record.id
    );
    assert_ne!(
        retirement_identity(&record.reference, record.expected_tip, &changed_proof),
        record.id
    );
    assert_eq!(
        record
            .outcomes
            .iter()
            .map(|outcome| outcome.step.clone())
            .collect::<Vec<_>>(),
        RetirementStep::for_subjects(&record.related_subjects)
    );
    assert!(
        record
            .removals()
            .iter()
            .all(|removal| matches!(&removal.outcome, RemovalOutcome::Kept { .. }))
    );
    assert!(matches!(
        ledger.set_retirement_step(
            project,
            &record.id,
            record.revision,
            RetirementStepOutcome {
                step: record.outcomes[0].step.clone(),
                state: RetirementStepState::Completed {
                    evidence: RemovalEvidence::ExplicitDiscard,
                },
            },
            &fixture.request(),
        ),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        ledger
            .start_retirement(&record, &fixture.request())
            .unwrap(),
        record
    );
    for item in [&subject, &worktree, &second_worktree] {
        assert_eq!(
            ledger
                .custody_item(project, item, &fixture.request())
                .unwrap()
                .unwrap()
                .state,
            CustodyState::Retired {
                retirement_id: record.id.clone(),
            }
        );
    }

    let first_step = record.outcomes[0].step.clone();
    let first_subject = match &first_step {
        RetirementStep::Worktree { subject } => subject.clone(),
        RetirementStep::Reference => unreachable!("worktree outcomes precede the reference"),
    };
    let uncertain = ledger
        .set_retirement_step(
            project,
            &record.id,
            record.revision,
            RetirementStepOutcome {
                step: first_step.clone(),
                state: RetirementStepState::Unknown {
                    reason: "interrupted".into(),
                },
            },
            &fixture.request(),
        )
        .unwrap();
    let pending = ledger
        .set_retirement_step(
            project,
            &record.id,
            uncertain.revision,
            RetirementStepOutcome {
                step: first_step.clone(),
                state: RetirementStepState::Pending,
            },
            &fixture.request(),
        )
        .unwrap();
    let partial = ledger
        .set_retirement_step(
            project,
            &record.id,
            pending.revision,
            RetirementStepOutcome {
                step: first_step.clone(),
                state: RetirementStepState::Completed {
                    evidence: subsumed_evidence.clone(),
                },
            },
            &fixture.request(),
        )
        .unwrap();
    let partial_removals = partial.removals();
    let removed = partial_removals
        .iter()
        .find(|removal| matches!(&removal.outcome, RemovalOutcome::Removed { .. }))
        .unwrap();
    assert_eq!(removed.subject, first_subject);
    assert!(matches!(
        &removed.outcome,
        RemovalOutcome::Removed { evidence, retirement_id }
            if retirement_id == &record.id && evidence == &subsumed_evidence
    ));
    let removed_subjects = partial_removals
        .iter()
        .filter(|removal| matches!(&removal.outcome, RemovalOutcome::Removed { .. }))
        .map(|removal| &removal.subject)
        .collect::<Vec<_>>();
    let completed_subjects = partial
        .outcomes
        .iter()
        .filter(|outcome| matches!(&outcome.state, RetirementStepState::Completed { .. }))
        .map(|outcome| match &outcome.step {
            RetirementStep::Worktree { subject } => subject,
            RetirementStep::Reference => &partial.subject,
        })
        .collect::<Vec<_>>();
    assert!(completed_subjects.iter().all(|subject| {
        removed_subjects
            .iter()
            .any(|projected| projected == subject)
    }));
    assert!(removed_subjects.iter().all(|subject| {
        completed_subjects
            .iter()
            .any(|completed| completed == subject)
    }));
    assert!(matches!(
        ledger.set_retirement_step(
            project,
            &record.id,
            record.revision,
            RetirementStepOutcome {
                step: first_step.clone(),
                state: RetirementStepState::Completed {
                    evidence: subsumed_evidence.clone(),
                },
            },
            &fixture.request(),
        ),
        Err(Error::StaleFence)
    ));
    assert_eq!(
        ledger
            .start_retirement(&record, &fixture.request())
            .unwrap(),
        partial
    );
    assert!(matches!(
        ledger.set_retirement_step(
            project,
            &record.id,
            partial.revision,
            RetirementStepOutcome {
                step: RetirementStep::Reference,
                state: RetirementStepState::Completed {
                    evidence: subsumed_evidence.clone(),
                },
            },
            &fixture.request(),
        ),
        Err(Error::Invalid(_))
    ));

    let second_step = record.outcomes[1].step.clone();
    let refused = ledger
        .set_retirement_step(
            project,
            &record.id,
            partial.revision,
            RetirementStepOutcome {
                step: second_step.clone(),
                state: RetirementStepState::Refused {
                    reason: "busy".into(),
                },
            },
            &fixture.request(),
        )
        .unwrap();
    let pending = ledger
        .set_retirement_step(
            project,
            &record.id,
            refused.revision,
            RetirementStepOutcome {
                step: second_step,
                state: RetirementStepState::Pending,
            },
            &fixture.request(),
        )
        .unwrap();
    let complete_worktrees = ledger
        .set_retirement_step(
            project,
            &record.id,
            pending.revision,
            RetirementStepOutcome {
                step: record.outcomes[1].step.clone(),
                state: RetirementStepState::Completed {
                    evidence: subsumed_evidence.clone(),
                },
            },
            &fixture.request(),
        )
        .unwrap();
    let current = ledger
        .set_retirement_step(
            project,
            &record.id,
            complete_worktrees.revision,
            RetirementStepOutcome {
                step: RetirementStep::Reference,
                state: RetirementStepState::Completed {
                    evidence: landed_evidence.clone(),
                },
            },
            &fixture.request(),
        )
        .unwrap();
    let finished = ledger
        .retirement(project, &record.id, &fixture.request())
        .unwrap()
        .unwrap();
    let removals = finished.removals();
    assert!(removals.iter().all(|removal| matches!(
        &removal.outcome,
        RemovalOutcome::Removed { retirement_id, .. } if retirement_id == &record.id
    )));
    for subject in std::iter::once(&finished.subject).chain(&finished.related_subjects) {
        assert!(removals.iter().any(|removal| &removal.subject == subject));
    }
    let removed_reference = removals
        .into_iter()
        .find(|removal| removal.subject == finished.subject)
        .unwrap();
    assert!(matches!(
        removed_reference.outcome,
        RemovalOutcome::Removed { evidence, .. } if evidence == landed_evidence
    ));
    assert!(matches!(
        ledger.set_retirement_step(
            project,
            &record.id,
            current.revision,
            RetirementStepOutcome {
                step: RetirementStep::Reference,
                state: RetirementStepState::Pending,
            },
            &fixture.request(),
        ),
        Err(Error::Conflict)
    ));
}

#[test]
fn explicit_discard_retirement_does_not_claim_landing() {
    let fixture = Fixture::new();
    let ledger = HostStore::open(&fixture.root, &fixture.request()).unwrap();
    let project = project();
    let reference = RefOwnership {
        repository: project,
        endpoint: RefEndpoint::Local,
        reference: "refs/heads/lane/explicit-discard".into(),
        generation: 1,
        origin: RefOrigin::Created,
    };
    let ref_subject = reference.custody_subject().unwrap();
    let worktree = Subject::new(
        SubjectKind::Worktree,
        fixture
            .temp
            .path()
            .canonicalize()
            .unwrap()
            .join("explicit-discard")
            .to_string_lossy()
            .into_owned(),
    );
    let holder = holder(1);
    for subject in [&ref_subject, &worktree] {
        ledger
            .acquire_custody(
                project,
                subject,
                &holder,
                Origin::Provenant,
                &fixture.request(),
            )
            .unwrap();
    }
    let record = RetirementRecord::plan(
        project,
        vec![worktree],
        reference,
        "3123456789abcdef0123456789abcdef01234567".parse().unwrap(),
        RetirementDecision::Discard {
            authority: actor(1),
            evidence: RemovalEvidence::ExplicitDiscard,
        },
    )
    .unwrap();
    let started = ledger
        .start_retirement(&record, &fixture.request())
        .unwrap();
    let worktree_done = ledger
        .set_retirement_step(
            project,
            &record.id,
            started.revision,
            RetirementStepOutcome {
                step: record.outcomes[0].step.clone(),
                state: RetirementStepState::Completed {
                    evidence: RemovalEvidence::ExplicitDiscard,
                },
            },
            &fixture.request(),
        )
        .unwrap();
    let finished = ledger
        .set_retirement_step(
            project,
            &record.id,
            worktree_done.revision,
            RetirementStepOutcome {
                step: RetirementStep::Reference,
                state: RetirementStepState::Completed {
                    evidence: RemovalEvidence::ExplicitDiscard,
                },
            },
            &fixture.request(),
        )
        .unwrap();
    assert!(finished.removals().iter().all(|removal| matches!(
        &removal.outcome,
        RemovalOutcome::Removed {
            evidence: RemovalEvidence::ExplicitDiscard,
            retirement_id,
        } if retirement_id == &record.id
    )));
}

#[test]
fn custody_queries_page_and_include_nested_lanes_without_crossing_chairs() {
    let f = Fixture::new();
    let ledger = HostStore::open(&f.root, &f.request()).unwrap();
    let Holder::Chair { chair } = holder(1) else {
        unreachable!()
    };
    let nested = Holder::Attempt {
        attempt: AttemptId::from_bytes([3; 16]),
        chair: Some(chair.clone()),
    };
    let other = holder(2);
    for n in 1..=4 {
        ledger
            .acquire_custody(
                ProjectId::from_bytes([n; 16]),
                &Subject::new(SubjectKind::Branch, format!("branch-{n}")),
                if n % 2 == 0 { &nested } else { &other },
                Origin::Provenant,
                &f.request(),
            )
            .unwrap();
    }
    let mut query = CustodyQuery {
        chair: Some(chair),
        limit: 1,
        ..Default::default()
    };
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let page = ledger.custody(&query, &f.request()).unwrap();
        for record in page.items {
            assert_eq!(record.holder, nested);
            assert!(seen.insert(record.project.to_string()));
        }
        match page.next {
            Some(next) => {
                assert!(next > query.after);
                query.after = next;
            }
            None => break,
        }
    }
    assert!(!seen.is_empty());
}
#[test]
fn custody_event_failure_rolls_back_the_record() {
    let f = Fixture::new();
    let ledger = HostStore::open(&f.root, &f.request()).unwrap();
    ledger.0.db.execute_batch("CREATE TRIGGER reject_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'fault'); END;").unwrap();
    let subject = Subject::new(SubjectKind::Issue, "3078");
    assert!(
        ledger
            .acquire_custody(
                project(),
                &subject,
                &holder(1),
                Origin::Provenant,
                &f.request()
            )
            .is_err()
    );
    assert!(
        ledger
            .custody_item(project(), &subject, &f.request())
            .unwrap()
            .is_none()
    );
}
#[test]
fn custody_retains_evidence_links_and_extensions_after_settlement() {
    let f = Fixture::new();
    let ledger = HostStore::open(&f.root, &f.request()).unwrap();
    let subjects = [
        Subject::new(SubjectKind::Branch, "feature"),
        Subject::new(SubjectKind::Pr, "42"),
    ];
    for subject in &subjects {
        ledger
            .acquire_custody(
                project(),
                subject,
                &holder(1),
                Origin::Provenant,
                &f.request(),
            )
            .unwrap();
    }
    ledger
        .link_custody(project(), &subjects, &f.request())
        .unwrap();
    ledger
        .0
        .db
        .execute(
            "UPDATE custody SET record=CAST(json_set(CAST(record AS TEXT),'$.future',42) AS BLOB)",
            [],
        )
        .unwrap();
    let evidence = LandedEvidence {
        predicate: LandingPredicate::Landed,
        source: LandingSource::ForgeMerged {
            pr: "42".into(),
            head: "0123456789abcdef0123456789abcdef01234567".parse().unwrap(),
            merge_commit: None,
        },
    };
    let landed = CustodyState::Landed { evidence };
    let record = ledger
        .change_custody_state(
            project(),
            &subjects[0],
            landed.clone(),
            holder(1).actor(),
            &f.request(),
        )
        .unwrap();
    assert_eq!(record.state, landed);
    assert!(record.links.contains(&subjects[1]));
    assert_eq!(record.extra["future"], serde_json::json!(42));
    assert!(matches!(
        ledger.change_custody_state(
            project(),
            &subjects[0],
            CustodyState::Retired {
                retirement_id: "missing-retirement".into(),
            },
            holder(1).actor(),
            &f.request(),
        ),
        Err(Error::Conflict)
    ));
    assert_eq!(
        ledger
            .custody_item(project(), &subjects[0], &f.request())
            .unwrap()
            .unwrap()
            .state,
        landed
    );
}
