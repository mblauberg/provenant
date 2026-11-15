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
        let id = if kind == SubjectKind::Worktree || kind.is_path() {
            f.temp
                .path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        } else {
            format!("{kind}-item")
        };
        let subject = if kind == SubjectKind::Worktree {
            Subject::worktree(id, "fixture-1")
        } else {
            Subject::new(kind, id)
        };
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
        assert_eq!(
            changed.record.state,
            CustodyState::Active {
                extra: Default::default(),
            }
        );
        ledger
            .change_custody_state(
                project(),
                &subject,
                CustodyState::Released {
                    extra: Default::default(),
                },
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
            CustodyState::Released {
                extra: Default::default(),
            }
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
    assert_eq!(
        seeded.state,
        CustodyState::Active {
            extra: Default::default(),
        }
    );

    let landed = CustodyState::Landed {
        evidence: LandedEvidence {
            predicate: LandingPredicate::Landed,
            source: LandingSource::RetainedRef {
                reference: "refs/heads/main".into(),
                commit: "0123456789abcdef0123456789abcdef01234567".parse().unwrap(),
                extra: Default::default(),
            },
            extra: Default::default(),
        },
        extra: Default::default(),
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

struct LandingRetirementFixture {
    fixture: Fixture,
    ledger: HostStore,
    project: ProjectId,
    reference_subject: Subject,
    worktrees: Vec<Subject>,
    record: RetirementRecord,
    subsumed_evidence: RemovalEvidence,
    landed_evidence: RemovalEvidence,
}

fn landing_retirement_fixture() -> LandingRetirementFixture {
    let fixture = Fixture::new();
    let ledger = HostStore::open(&fixture.root, &fixture.request()).unwrap();
    let project = project();
    let reference = RefOwnership {
        extra: Default::default(),
        repository: project,
        endpoint: RefEndpoint::Local {
            extra: Default::default(),
        },
        reference: "refs/heads/lane/retire".into(),
        generation: 1,
        origin: RefOrigin::Created,
    };
    let reference_subject = reference.custody_subject().unwrap();
    let root = fixture.temp.path().canonicalize().unwrap();
    let worktrees = ["retired-worktree", "second-retired-worktree"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            Subject::worktree(
                root.join(name).to_string_lossy().into_owned(),
                format!("fixture-{}", index + 1),
            )
        })
        .collect::<Vec<_>>();
    let owner = holder(1);
    for subject in std::iter::once(&reference_subject).chain(&worktrees) {
        ledger
            .acquire_custody(
                project,
                subject,
                &owner,
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
                extra: Default::default(),
            },
            extra: Default::default(),
        },
        extra: Default::default(),
    };
    let landed_evidence = RemovalEvidence::Landing {
        evidence: LandedEvidence {
            predicate: LandingPredicate::Landed,
            source: LandingSource::IntegrationTarget {
                reference: "refs/heads/main".into(),
                commit: "2123456789abcdef0123456789abcdef01234567".parse().unwrap(),
                extra: Default::default(),
            },
            extra: Default::default(),
        },
        extra: Default::default(),
    };
    ledger
        .change_custody_state(
            project,
            &reference_subject,
            CustodyState::Landed {
                evidence: LandedEvidence {
                    predicate: LandingPredicate::Landed,
                    source: LandingSource::ForgeMerged {
                        pr: "prior-proof".into(),
                        head: expected_tip,
                        merge_commit: None,
                        extra: Default::default(),
                    },
                    extra: Default::default(),
                },
                extra: Default::default(),
            },
            owner.actor(),
            &fixture.request(),
        )
        .unwrap();
    let record = RetirementRecord::plan(
        project,
        worktrees.clone(),
        reference,
        expected_tip,
        RetirementDecision::Discard {
            authority: actor(1),
            evidence: landed_evidence.clone(),
            extra: Default::default(),
        },
    )
    .unwrap();
    LandingRetirementFixture {
        fixture,
        ledger,
        project,
        reference_subject,
        worktrees,
        record,
        subsumed_evidence,
        landed_evidence,
    }
}

#[test]
fn retirement_identity_tracks_generation_tip_authority_and_proof() {
    let context = landing_retirement_fixture();
    let record = &context.record;
    assert_eq!(record.subject, context.reference_subject);
    assert_eq!(
        RetirementRecord::identity(
            context.project,
            &record
                .related_subjects
                .iter()
                .cloned()
                .rev()
                .collect::<Vec<_>>(),
            &record.reference,
            record.expected_tip,
            &record.decision,
        )
        .unwrap(),
        record.id
    );
    let identity = |reference: &RefGeneration, tip, decision: &RetirementDecision| {
        RetirementRecord::identity(
            context.project,
            &record.related_subjects,
            reference,
            tip,
            decision,
        )
        .unwrap()
    };
    let changed_generation = RefGeneration {
        generation: record.reference.generation + 1,
        ..record.reference.clone()
    };
    let changed_tip = "2123456789abcdef0123456789abcdef01234567".parse().unwrap();
    assert_ne!(
        identity(&changed_generation, record.expected_tip, &record.decision),
        record.id
    );
    assert_ne!(
        identity(&record.reference, changed_tip, &record.decision),
        record.id
    );
    let mut changed_authority = record.decision.clone();
    let mut changed_proof = record.decision.clone();
    if let RetirementDecision::Discard { authority, .. } = &mut changed_authority {
        *authority = actor(2);
    }
    if let RetirementDecision::Discard {
        evidence: RemovalEvidence::Landing { evidence, .. },
        ..
    } = &mut changed_proof
    {
        evidence.source = LandingSource::RetainedRef {
            reference: "refs/heads/lane/retire".into(),
            commit: record.expected_tip,
            extra: Default::default(),
        };
    }
    assert_ne!(
        identity(&record.reference, record.expected_tip, &changed_authority),
        record.id
    );
    assert_ne!(
        identity(&record.reference, record.expected_tip, &changed_proof),
        record.id
    );
}

#[test]
fn starting_retirement_owns_custody_and_rejects_unapproved_evidence() {
    let context = landing_retirement_fixture();
    let record = context
        .ledger
        .start_retirement(&context.record, &context.fixture.request())
        .unwrap();
    assert_eq!(record, context.record);
    assert_eq!(
        context
            .ledger
            .start_retirement(&record, &context.fixture.request())
            .unwrap(),
        record
    );
    assert!(
        context
            .ledger
            .pending_retirements(context.project, &context.fixture.request())
            .unwrap()
            .iter()
            .any(|pending| pending.id == record.id)
    );
    for subject in std::iter::once(&record.subject).chain(&record.related_subjects) {
        assert_eq!(
            context
                .ledger
                .custody_item(context.project, subject, &context.fixture.request())
                .unwrap()
                .unwrap()
                .state,
            CustodyState::Retired {
                retirement_id: record.id.clone(),
                extra: Default::default(),
            }
        );
    }
    let step = record.outcomes[0].step.clone();
    assert!(matches!(
        context.ledger.set_retirement_step(
            context.project,
            &record.id,
            record.revision,
            RetirementStepOutcome {
                extra: Default::default(),
                step,
                state: RetirementStepState::Completed {
                    evidence: RemovalEvidence::ExplicitDiscard {
                        archive: None,
                        extra: Default::default()
                    },
                    extra: Default::default(),
                },
            },
            &context.fixture.request(),
        ),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        context
            .ledger
            .retirement(context.project, &record.id, &context.fixture.request())
            .unwrap(),
        Some(record)
    );
}

#[test]
fn retirement_step_state_table_preserves_pending_work_and_removal_projection() {
    let cases = [
        (
            "pending",
            RetirementStepState::Pending {
                extra: Default::default(),
            },
            false,
            false,
        ),
        (
            "unknown",
            RetirementStepState::Unknown {
                reason: "interrupted".into(),
                extra: Default::default(),
            },
            true,
            false,
        ),
        (
            "refused",
            RetirementStepState::Refused {
                reason: "busy".into(),
                extra: Default::default(),
            },
            true,
            false,
        ),
        (
            "completed",
            RetirementStepState::Completed {
                evidence: RemovalEvidence::Landing {
                    evidence: LandedEvidence {
                        predicate: LandingPredicate::Subsumed,
                        source: LandingSource::RetainedRef {
                            reference: "refs/heads/main".into(),
                            commit: "2123456789abcdef0123456789abcdef01234567".parse().unwrap(),
                            extra: Default::default(),
                        },
                        extra: Default::default(),
                    },
                    extra: Default::default(),
                },
                extra: Default::default(),
            },
            true,
            true,
        ),
    ];
    for (case, state, changes_revision, removes_worktree) in cases {
        let context = landing_retirement_fixture();
        let started = context
            .ledger
            .start_retirement(&context.record, &context.fixture.request())
            .unwrap();
        let step = started
            .outcomes
            .iter()
            .find(|outcome| matches!(&outcome.step, RetirementStep::Worktree { .. }))
            .unwrap()
            .step
            .clone();
        let subject = match &step {
            RetirementStep::Worktree { subject, .. } => subject,
            RetirementStep::Reference { .. } => unreachable!(),
        };
        let updated = context
            .ledger
            .set_retirement_step(
                context.project,
                &started.id,
                started.revision,
                RetirementStepOutcome {
                    extra: Default::default(),
                    step: step.clone(),
                    state,
                },
                &context.fixture.request(),
            )
            .unwrap();
        let revision_delta = if changes_revision { 1 } else { 0 };
        assert_eq!(
            updated.revision,
            started.revision + revision_delta,
            "{case}"
        );
        assert!(
            context
                .ledger
                .pending_retirements(context.project, &context.fixture.request())
                .unwrap()
                .iter()
                .any(|pending| pending.id == updated.id),
            "{case}"
        );
        let projection = updated
            .removals()
            .into_iter()
            .find(|removal| &removal.subject == subject)
            .unwrap();
        if removes_worktree {
            assert!(
                matches!(
                    projection.outcome,
                    RemovalOutcome::Removed { evidence, retirement_id, .. }
                        if evidence == context.subsumed_evidence && retirement_id == updated.id
                ),
                "{case}"
            );
        } else {
            assert!(
                matches!(projection.outcome, RemovalOutcome::Kept { .. }),
                "{case}"
            );
        }
        assert_eq!(
            context
                .ledger
                .start_retirement(&updated, &context.fixture.request())
                .unwrap(),
            updated,
            "{case}"
        );
        if changes_revision {
            assert!(
                matches!(
                    context.ledger.set_retirement_step(
                        context.project,
                        &updated.id,
                        started.revision,
                        RetirementStepOutcome {
                            extra: Default::default(),
                            step,
                            state: RetirementStepState::Pending {
                                extra: Default::default(),
                            },
                        },
                        &context.fixture.request(),
                    ),
                    Err(Error::StaleFence)
                ),
                "{case}"
            );
        }
    }
}

#[test]
fn retirement_completes_worktrees_before_landed_reference_proof() {
    let context = landing_retirement_fixture();
    let mut current = context
        .ledger
        .start_retirement(&context.record, &context.fixture.request())
        .unwrap();
    let reference_step = current
        .outcomes
        .iter()
        .find(|outcome| matches!(&outcome.step, RetirementStep::Reference { .. }))
        .unwrap()
        .step
        .clone();
    assert!(matches!(
        context.ledger.set_retirement_step(
            context.project,
            &current.id,
            current.revision,
            RetirementStepOutcome {
                extra: Default::default(),
                step: reference_step.clone(),
                state: RetirementStepState::Completed {
                    evidence: context.landed_evidence.clone(),
                    extra: Default::default(),
                },
            },
            &context.fixture.request(),
        ),
        Err(Error::Conflict)
    ));
    let worktree_steps = current
        .outcomes
        .iter()
        .filter(|outcome| matches!(&outcome.step, RetirementStep::Worktree { .. }))
        .map(|outcome| outcome.step.clone())
        .collect::<Vec<_>>();
    for step in worktree_steps {
        current = context
            .ledger
            .set_retirement_step(
                context.project,
                &current.id,
                current.revision,
                RetirementStepOutcome {
                    extra: Default::default(),
                    step,
                    state: RetirementStepState::Completed {
                        evidence: context.subsumed_evidence.clone(),
                        extra: Default::default(),
                    },
                },
                &context.fixture.request(),
            )
            .unwrap();
    }
    assert!(matches!(
        context.ledger.set_retirement_step(
            context.project,
            &current.id,
            current.revision,
            RetirementStepOutcome {
                extra: Default::default(),
                step: reference_step.clone(),
                state: RetirementStepState::Completed {
                    evidence: context.subsumed_evidence.clone(),
                    extra: Default::default(),
                },
            },
            &context.fixture.request(),
        ),
        Err(Error::Invalid(_))
    ));
    current = context
        .ledger
        .set_retirement_step(
            context.project,
            &current.id,
            current.revision,
            RetirementStepOutcome {
                extra: Default::default(),
                step: reference_step.clone(),
                state: RetirementStepState::Completed {
                    evidence: context.landed_evidence.clone(),
                    extra: Default::default(),
                },
            },
            &context.fixture.request(),
        )
        .unwrap();
    assert!(
        !context
            .ledger
            .pending_retirements(context.project, &context.fixture.request())
            .unwrap()
            .iter()
            .any(|pending| pending.id == current.id)
    );
    let finished = context
        .ledger
        .retirement(context.project, &current.id, &context.fixture.request())
        .unwrap()
        .unwrap();
    assert_eq!(finished, current);
    let removals = finished.removals();
    assert!(removals.iter().all(|removal| matches!(
        &removal.outcome,
        RemovalOutcome::Removed { retirement_id, .. } if retirement_id == &finished.id
    )));
    for subject in std::iter::once(&finished.subject).chain(&finished.related_subjects) {
        assert!(removals.iter().any(|removal| &removal.subject == subject));
    }
    assert!(matches!(
        context.ledger.set_retirement_step(
            context.project,
            &current.id,
            current.revision,
            RetirementStepOutcome {
                extra: Default::default(),
                step: reference_step,
                state: RetirementStepState::Pending {
                    extra: Default::default(),
                },
            },
            &context.fixture.request(),
        ),
        Err(Error::Conflict)
    ));
}

#[test]
fn retirement_decisions_preserve_archive_proof_and_reference_custody() {
    for (with_worktree, remove_reference, detached) in [
        (false, true, false),
        (true, true, false),
        (true, false, false),
        (true, false, true),
    ] {
        let fixture = Fixture::new();
        let ledger = HostStore::open(&fixture.root, &fixture.request()).unwrap();
        let project = project();
        let worktree = Subject::worktree(
            fixture
                .temp
                .path()
                .canonicalize()
                .unwrap()
                .join("explicit-discard")
                .to_string_lossy()
                .into_owned(),
            "fixture-1",
        );
        let reference = RefOwnership {
            extra: Default::default(),
            repository: project,
            endpoint: if detached {
                RefEndpoint::Worktree {
                    path: worktree
                        .worktree_path()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    extra: Default::default(),
                }
            } else {
                RefEndpoint::Local {
                    extra: Default::default(),
                }
            },
            reference: if detached {
                "HEAD"
            } else {
                "refs/heads/lane/explicit-discard"
            }
            .into(),
            generation: 1,
            origin: if detached {
                RefOrigin::Adopted
            } else {
                RefOrigin::Created
            },
        };
        let ref_subject = reference.custody_subject().unwrap();
        let holder = holder(1);
        let custody_subjects = if detached {
            vec![&worktree]
        } else {
            vec![&ref_subject, &worktree]
        };
        for subject in custody_subjects {
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
        let original_ref = ledger
            .custody_item(project, &ref_subject, &fixture.request())
            .unwrap();
        let tip = "3123456789abcdef0123456789abcdef01234567".parse().unwrap();
        let evidence = if remove_reference {
            RemovalEvidence::ExplicitDiscard {
                archive: with_worktree.then(|| ArchiveProof {
                    extra: Default::default(),
                    id: "discard-fixture".into(),
                    tip,
                }),
                extra: Default::default(),
            }
        } else {
            RemovalEvidence::IdleRetirement {
                tip,
                extra: Default::default(),
            }
        };
        let decision = if remove_reference {
            RetirementDecision::Discard {
                authority: actor(1),
                evidence: evidence.clone(),
                extra: Default::default(),
            }
        } else {
            RetirementDecision::RetireWorktree {
                authority: actor(1),
                evidence: evidence.clone(),
                extra: Default::default(),
            }
        };
        let record = RetirementRecord::plan(
            project,
            if with_worktree {
                vec![worktree.clone()]
            } else {
                Vec::new()
            },
            reference,
            tip,
            decision,
        )
        .unwrap();
        let mut current = ledger
            .start_retirement(&record, &fixture.request())
            .unwrap_or_else(|error| {
                panic!("{remove_reference} {with_worktree} {detached}: {error:?} {record:?}")
            });
        assert_eq!(
            ledger
                .pending_retirements(project, &fixture.request())
                .unwrap()
                .len(),
            1
        );
        let completion_evidence = if matches!(
            &evidence,
            RemovalEvidence::ExplicitDiscard { archive: Some(_) }
        ) {
            let substitute = RemovalEvidence::ExplicitDiscard {
                archive: Some(ArchiveProof {
                    extra: Default::default(),
                    id: "wrong-tip-archive".into(),
                    tip: "4123456789abcdef0123456789abcdef01234567".parse().unwrap(),
                }),
                extra: Default::default(),
            };
            assert!(matches!(
                        ledger.set_retirement_step(
                            project,
                            &record.id,
                            current.revision,
                            RetirementStepOutcome {
                    extra: Default::default(),
                                step: current.outcomes[0].step.clone(),
                                state: RetirementStepState::Completed {
                                    evidence: substitute
            extra: Default::default(),
                                }
                            },
                            &fixture.request()
                        ),
                        Err(Error::Invalid(_))
                    ));
            RemovalEvidence::ExplicitDiscard {
                archive: Some(ArchiveProof {
                    extra: Default::default(),
                    id: "freshly-captured-archive".into(),
                    tip,
                }),
                extra: Default::default(),
            }
        } else {
            evidence.clone()
        };
        for outcome in &record.outcomes {
            current = ledger
                .set_retirement_step(
                    project,
                    &record.id,
                    current.revision,
                    RetirementStepOutcome {
                        extra: Default::default(),
                        step: outcome.step.clone(),
                        state: RetirementStepState::Completed {
                            evidence: completion_evidence.clone(),
                            extra: Default::default(),
                        },
                    },
                    &fixture.request(),
                )
                .unwrap();
        }
        assert!(
            ledger
                .pending_retirements(project, &fixture.request())
                .unwrap()
                .is_empty()
        );
        assert!(current.removals().iter().all(|removal| matches!(
            &removal.outcome, RemovalOutcome::Removed { evidence: proof, retirement_id, .. }
                if proof == &completion_evidence && retirement_id == &record.id
        )));
        assert_eq!(current.decision, record.decision);
        assert_eq!(current.id, record.id);
        let reference_after = ledger
            .custody_item(project, &ref_subject, &fixture.request())
            .unwrap();
        if remove_reference {
            assert_eq!(
                        reference_after.unwrap().state,
                        CustodyState::Retired {
                            retirement_id: record.id
            extra: Default::default(),
                        }
                    );
        } else {
            assert_eq!(reference_after, original_ref);
            assert!(
                current
                    .removals()
                    .iter()
                    .all(|removal| removal.subject == worktree)
            );
        }
    }
}

#[test]
fn a_reused_worktree_path_keeps_each_incarnation_custody() {
    let fixture = Fixture::new();
    let ledger = HostStore::open(&fixture.root, &fixture.request()).unwrap();
    let project = project();
    let path = fixture
        .temp
        .path()
        .canonicalize()
        .unwrap()
        .join("reused-worktree")
        .to_string_lossy()
        .into_owned();
    let first = Subject::worktree(&path, "generation-1");
    let second = Subject::worktree(&path, "generation-2");
    let reference = RefOwnership {
        extra: Default::default(),
        repository: project,
        endpoint: RefEndpoint::Local {
            extra: Default::default(),
        },
        reference: "refs/heads/lane/reused".into(),
        generation: 1,
        origin: RefOrigin::Created,
    };
    let reference_subject = reference.custody_subject().unwrap();
    let actor = holder(1);
    for subject in [&reference_subject, &first] {
        ledger
            .acquire_custody(
                project,
                subject,
                &actor,
                Origin::Provenant,
                &fixture.request(),
            )
            .unwrap();
    }
    let tip = "5123456789abcdef0123456789abcdef01234567".parse().unwrap();
    let evidence = RemovalEvidence::IdleRetirement {
        tip,
        extra: Default::default(),
    };
    let record = RetirementRecord::plan(
        project,
        vec![first.clone()],
        reference,
        tip,
        RetirementDecision::RetireWorktree {
            authority: actor.actor().expect("chair holder"),
            evidence: evidence.clone(),
            extra: Default::default(),
        },
    )
    .unwrap();
    let started = ledger
        .start_retirement(&record, &fixture.request())
        .unwrap();
    ledger
        .set_retirement_step(
            project,
            &record.id,
            started.revision,
            RetirementStepOutcome {
                extra: Default::default(),
                step: RetirementStep::Worktree {
                    subject: first.clone(),
                    extra: Default::default(),
                },
                state: RetirementStepState::Completed {
                    evidence,
                    extra: Default::default(),
                },
            },
            &fixture.request(),
        )
        .unwrap();

    ledger
        .acquire_custody(
            project,
            &second,
            &actor,
            Origin::Provenant,
            &fixture.request(),
        )
        .unwrap();
    assert_eq!(
        ledger
            .custody_item(project, &first, &fixture.request())
            .unwrap()
            .unwrap()
            .state,
        CustodyState::Retired {
            retirement_id: record.id.clone(),
            extra: Default::default(),
        }
    );
    assert_eq!(
        ledger
            .custody_item(project, &second, &fixture.request())
            .unwrap()
            .unwrap()
            .state,
        CustodyState::Active {
            extra: Default::default(),
        }
    );
    assert_eq!(
        ledger
            .retirement(project, &record.id, &fixture.request())
            .unwrap()
            .unwrap()
            .related_subjects,
        vec![first]
    );
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
            extra: Default::default(),
        },
        extra: Default::default(),
    };
    let landed = CustodyState::Landed {
        evidence,
        extra: Default::default(),
    };
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
                extra: Default::default(),
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
