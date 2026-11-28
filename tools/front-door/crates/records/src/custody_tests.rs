use super::*;
#[test]
fn endpoint_identity_keeps_ssh_principals_and_drops_secrets() {
    // Distinct SSH users at one host and path are distinct destinations.
    let alice = endpoint_identity("ssh://alice@example.invalid/team/repo.git");
    let bob = endpoint_identity("ssh://bob@example.invalid/team/repo.git");
    assert_ne!(alice, bob);
    assert_eq!(alice, "ssh://alice@example.invalid/team/repo.git");
    assert_eq!(bob, "ssh://bob@example.invalid/team/repo.git");
    // A password or token on the SSH user is a secret, never retained.
    assert_eq!(
        endpoint_identity("ssh://alice:token@example.invalid/team/repo.git"),
        alice
    );
    assert_eq!(
        endpoint_identity("git+ssh://alice@example.invalid/team/repo.git"),
        "git+ssh://alice@example.invalid/team/repo.git"
    );
    // scp-like URLs have no password form; keep the principal as written.
    assert_ne!(
        endpoint_identity("alice@example.invalid:team/repo.git"),
        endpoint_identity("bob@example.invalid:team/repo.git")
    );
    // For other transports the username can itself be a token: strip all
    // userinfo so two HTTP destinations at one host and path stay equal.
    assert_eq!(
        endpoint_identity("https://alice@example.invalid/team/repo.git"),
        endpoint_identity("https://bob:token@example.invalid/team/repo.git")
    );
    assert_eq!(
        endpoint_identity("https://alice@example.invalid/team/repo.git"),
        "https://example.invalid/team/repo.git"
    );
    assert_eq!(
        endpoint_identity("file://user:secret@localhost/tmp/repo.git"),
        "file://localhost/tmp/repo.git"
    );
    assert!(!alice.contains("token"));
}

#[test]

fn ref_custody_subject_binds_endpoint_reference_and_generation_without_exposing_url() {
    let project = ProjectId::from_bytes([9; 16]);
    let local = RefOwnership {
        repository: project,
        endpoint: RefEndpoint::Local,
        reference: "refs/heads/topic".into(),
        generation: 1,
        origin: RefOrigin::Created,
    };
    let remote = RefOwnership {
        endpoint: RefEndpoint::Remote {
            remote: "origin".into(),
            push_url: "ssh://example.invalid/team/repo.git".into(),
        },
        ..local.clone()
    };
    let another_generation = RefOwnership {
        generation: 2,
        ..remote.clone()
    };
    let another_remote = RefOwnership {
        endpoint: RefEndpoint::Remote {
            remote: "backup".into(),
            push_url: "ssh://backup.invalid/team/repo.git".into(),
        },
        ..remote.clone()
    };
    let local_subject = local.custody_subject().unwrap();
    let remote_subject = remote.custody_subject().unwrap();
    assert_eq!(local_subject.kind, SubjectKind::Ref);
    assert_eq!(
        local.logical_branch_subject(),
        remote.logical_branch_subject()
    );
    assert_ne!(local_subject, remote_subject);
    assert_ne!(
        remote_subject,
        another_generation.custody_subject().unwrap()
    );
    assert_ne!(remote_subject, another_remote.custody_subject().unwrap());
    let RefEndpoint::Remote { push_url, .. } = &remote.endpoint else {
        unreachable!()
    };
    assert!(!remote_subject.id.contains(push_url.as_str()));
    assert!(
        !serde_json::to_string(&remote.generation_identity().unwrap())
            .unwrap()
            .contains(push_url.as_str())
    );
    let detached = RefOwnership {
        endpoint: RefEndpoint::Worktree {
            path: "/fixture/detached".into(),
        },
        reference: "HEAD".into(),
        ..local.clone()
    };
    let detached_identity = detached.identity().unwrap();
    assert!(matches!(
        detached_identity.endpoint,
        RefEndpointIdentity::Worktree { .. }
    ));
    assert!(detached.logical_branch_subject().is_none());
    assert!(
        RefOwnership {
            endpoint: RefEndpoint::Worktree {
                path: "/fixture/../escape".into(),
            },
            ..detached
        }
        .identity()
        .is_err()
    );
}

#[test]
fn retirement_records_follow_one_declared_decision_and_archive_identity() {
    let project = ProjectId::from_bytes([12; 16]);
    let tip = provenant_core::GitOid::parse(&"c3".repeat(20)).unwrap();
    let reference = RefOwnership {
        repository: project,
        endpoint: RefEndpoint::Local,
        reference: "refs/heads/idle".into(),
        generation: 4,
        origin: RefOrigin::Created,
    };
    let worktree = Subject::worktree("/fixture/idle", "fixture-1");
    let idle = RetirementDecision::RetireWorktree {
        authority: ActorId::from_bytes([4; 16]),
        evidence: RemovalEvidence::IdleRetirement { tip },
    };
    let mut record = RetirementRecord::plan(
        project,
        vec![worktree.clone()],
        reference.clone(),
        tip,
        idle,
    )
    .unwrap();
    assert!(record.valid_evidence());
    assert_eq!(record.outcomes.len(), 1);
    assert_eq!(
        record.subjects().cloned().collect::<Vec<_>>(),
        std::slice::from_ref(&worktree)
    );
    assert_eq!(record.removals().len(), 1);
    assert_eq!(record.removal().subject, worktree);
    let completion = record.decision.evidence().clone();
    record.outcomes[0].state = RetirementStepState::Completed {
        evidence: completion.clone(),
    };
    assert!(matches!(
        record.removal().outcome,
        RemovalOutcome::Removed { .. }
    ));
    assert!(record.accepts_completion_evidence(&record.outcomes[0].step, &completion));

    let subsumed = RetirementRecord::plan(
        project,
        vec![Subject::worktree("/fixture/subsumed", "fixture-1")],
        reference.clone(),
        tip,
        RetirementDecision::RetireWorktree {
            authority: ActorId::from_bytes([4; 16]),
            evidence: RemovalEvidence::Landing {
                evidence: LandedEvidence {
                    predicate: LandingPredicate::Subsumed,
                    source: LandingSource::RetainedRef {
                        reference: "refs/heads/retained".into(),
                        commit: tip,
                    },
                },
            },
        },
    )
    .unwrap();
    assert!(subsumed.valid_evidence());
    assert_eq!(subsumed.outcomes.len(), 1);
    assert_eq!(subsumed.subjects().count(), 1);
    let refreshed_subsumed = RetirementDecision::RetireWorktree {
        authority: ActorId::from_bytes([5; 16]),
        evidence: RemovalEvidence::Landing {
            evidence: LandedEvidence {
                predicate: LandingPredicate::Subsumed,
                source: LandingSource::RetainedRef {
                    reference: "refs/heads/updated-base".into(),
                    commit: tip,
                },
            },
        },
    };
    assert!(subsumed.decision.permits_replay(&refreshed_subsumed));
    assert!(refreshed_subsumed.valid_for(&subsumed.related_subjects, tip));

    let archive = ArchiveProof {
        id: "archive-123".into(),
        tip,
    };
    let explicit = RetirementDecision::Discard {
        authority: ActorId::from_bytes([4; 16]),
        evidence: RemovalEvidence::ExplicitDiscard {
            archive: Some(archive.clone()),
        },
    };
    let explicit_record = RetirementRecord::plan(
        project,
        vec![Subject::worktree("/fixture/idle", "fixture-1")],
        reference.clone(),
        tip,
        explicit.clone(),
    )
    .unwrap();
    assert!(explicit_record.valid_evidence());
    assert_eq!(explicit_record.outcomes.len(), 2);
    let refreshed_archive = RetirementDecision::Discard {
        authority: ActorId::from_bytes([5; 16]),
        evidence: RemovalEvidence::ExplicitDiscard {
            archive: Some(ArchiveProof {
                id: "archive-retry".into(),
                tip,
            }),
        },
    };
    assert!(explicit.permits_replay(&refreshed_archive));
    assert!(refreshed_archive.valid_for(&explicit_record.related_subjects, tip));
    let worktree_step = RetirementStep::Worktree {
        subject: explicit_record.related_subjects[0].clone(),
    };
    assert!(
        explicit_record.accepts_completion_evidence(&worktree_step, refreshed_archive.evidence(),)
    );
    let wrong_tip_archive = RetirementDecision::Discard {
        authority: ActorId::from_bytes([5; 16]),
        evidence: RemovalEvidence::ExplicitDiscard {
            archive: Some(ArchiveProof {
                id: "archive-wrong-tip".into(),
                tip: provenant_core::GitOid::parse(&"d4".repeat(20)).unwrap(),
            }),
        },
    };
    assert!(explicit.permits_replay(&wrong_tip_archive));
    assert!(!wrong_tip_archive.valid_for(&explicit_record.related_subjects, tip));
    assert!(
        !explicit_record.accepts_completion_evidence(&worktree_step, wrong_tip_archive.evidence(),)
    );
    assert!(!explicit.permits_replay(&refreshed_subsumed));
    assert_ne!(
        explicit_record.id,
        RetirementRecord::identity(
            project,
            &explicit_record.related_subjects,
            &reference.generation_identity().unwrap(),
            tip,
            &RetirementDecision::Discard {
                authority: ActorId::from_bytes([4; 16]),
                evidence: RemovalEvidence::ExplicitDiscard {
                    archive: Some(ArchiveProof {
                        id: "archive-other".into(),
                        tip,
                    }),
                },
            },
        )
        .unwrap()
    );
    let invalid_archive = RetirementDecision::Discard {
        authority: ActorId::from_bytes([4; 16]),
        evidence: RemovalEvidence::ExplicitDiscard {
            archive: Some(ArchiveProof {
                id: "../escape".into(),
                tip,
            }),
        },
    };
    assert!(
        !RetirementRecord::plan(
            project,
            explicit_record.related_subjects,
            reference,
            tip,
            invalid_archive,
        )
        .unwrap()
        .valid_evidence()
    );
    let detached_path = "/fixture/detached";
    let detached_worktree = Subject::worktree(detached_path, "fixture-1");
    let detached_reference = RefOwnership {
        repository: project,
        endpoint: RefEndpoint::Worktree {
            path: detached_path.into(),
        },
        reference: "HEAD".into(),
        generation: 1,
        origin: RefOrigin::Adopted,
    };
    let detached = RetirementRecord::plan(
        project,
        vec![detached_worktree.clone()],
        detached_reference,
        tip,
        RetirementDecision::RetireWorktree {
            authority: ActorId::from_bytes([4; 16]),
            evidence: RemovalEvidence::ExplicitDiscard {
                archive: Some(ArchiveProof {
                    id: "archive-detached".into(),
                    tip,
                }),
            },
        },
    )
    .unwrap();
    assert!(detached.valid_evidence());
    assert_eq!(detached.outcomes.len(), 1);
    assert_eq!(
        detached.subjects().cloned().collect::<Vec<_>>(),
        [detached_worktree]
    );
    assert!(!archive.id.is_empty());
}

#[test]
fn removal_records_round_trip_every_predicate_source_and_uncertainty() {
    let oid = provenant_core::GitOid::parse(&"a".repeat(40)).unwrap();
    let target = provenant_core::GitOid::parse(&"b".repeat(40)).unwrap();
    let project = ProjectId::from_bytes([7; 16]);
    let sources = [
        LandingSource::RetainedRef {
            reference: "refs/heads/feature".into(),
            commit: oid,
        },
        LandingSource::TreeEquivalent {
            reference: "refs/heads/retained".into(),
            head: oid,
            target,
            tree: oid,
        },
        LandingSource::IntegrationTarget {
            reference: "refs/remotes/origin/main".into(),
            commit: target,
        },
        LandingSource::ForgeMerged {
            pr: "42".into(),
            head: oid,
            merge_commit: Some(target),
        },
    ];
    for predicate in [LandingPredicate::Subsumed, LandingPredicate::Landed] {
        for source in &sources {
            let record = RemovalRecord {
                project,
                subject: Subject::worktree("/fixture/feature", "fixture-1"),
                tip: oid,
                outcome: RemovalOutcome::Removed {
                    evidence: RemovalEvidence::Landing {
                        evidence: LandedEvidence {
                            predicate,
                            source: source.clone(),
                        },
                    },
                    retirement_id: "retirement-test".into(),
                },
            };
            let encoded = serde_json::to_vec(&record).unwrap();
            let decoded: RemovalRecord = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(decoded, record);
            let mut without_owner = serde_json::to_value(&record).unwrap();
            without_owner["outcome"]
                .as_object_mut()
                .unwrap()
                .remove("retirement_id");
            assert!(serde_json::from_value::<RemovalRecord>(without_owner).is_err());
        }
    }
    let explicit = RemovalRecord {
        project,
        subject: Subject::worktree("/fixture/feature", "fixture-1"),
        tip: oid,
        outcome: RemovalOutcome::Removed {
            evidence: RemovalEvidence::ExplicitDiscard {
                archive: Some(ArchiveProof {
                    id: "archive-1".into(),
                    tip: oid,
                }),
            },
            retirement_id: "retirement-explicit".into(),
        },
    };
    let encoded = serde_json::to_vec(&explicit).unwrap();
    assert_eq!(
        serde_json::from_slice::<RemovalRecord>(&encoded).unwrap(),
        explicit
    );
    for &cause in RemovalUncertainty::ALL {
        let record = RemovalRecord {
            project,
            subject: Subject::worktree("/fixture/feature", "fixture-1"),
            tip: oid,
            outcome: RemovalOutcome::Kept { cause },
        };
        let encoded = serde_json::to_vec(&record).unwrap();
        let decoded: RemovalRecord = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, record);
    }
}

#[test]
fn worktree_identity_preserves_incarnations_and_rejects_path_only_subjects() {
    let first = Subject::worktree("/fixture/reused", "generation-1");
    let second = Subject::worktree("/fixture/reused", "generation-2");
    assert_ne!(first, second);
    assert_eq!(first.worktree_path(), second.worktree_path());
    assert_eq!(first.to_string().parse::<Subject>().unwrap(), first);
    assert!(
        Subject::new(SubjectKind::Worktree, "/fixture/reused")
            .worktree_path()
            .is_none()
    );
    for (path, generation) in [
        ("relative", "generation-1"),
        ("/fixture/../escape", "generation-1"),
        ("/fixture/reused", ""),
    ] {
        assert!(
            Subject::worktree(path, generation)
                .worktree_path()
                .is_none()
        );
    }
}

#[test]
fn every_declared_subject_kind_round_trips() {
    for &kind in SubjectKind::ALL {
        assert_eq!(kind.as_str().parse::<SubjectKind>().unwrap(), kind);
        let subject = if kind == SubjectKind::Worktree {
            Subject::worktree("/fixture/checkout", "generation-1")
        } else {
            Subject::new(kind, "item:with:colons")
        };
        assert_eq!(subject.to_string().parse::<Subject>().unwrap(), subject);
        assert_eq!(
            serde_json::from_value::<Subject>(serde_json::to_value(&subject).unwrap()).unwrap(),
            subject
        );
    }
}

#[test]
fn writer_admission_obeys_live_lapsed_and_owner_sharing_rules() {
    let holder = AttemptId::from_bytes([1; 16]);
    let lease = WorktreeLease {
        holder,
        since: WallTime::from_micros(1).unwrap(),
        sharing: Sharing::Inherit,
    };
    let tree = Subject::worktree("/fixture/tree", "generation-1");
    let live = LeaseObservation {
        lease: lease.clone(),
        state: LeaseState::Live {
            attempt_state: "running".into(),
        },
    };

    let configured = Sourced {
        value: Sharing::Warn,
        source: Source::at(Layer::Host, "config.toml#lanes.worktree.sharing"),
    };
    assert!(matches!(
        admit_writer(&tree, None, &configured),
        Admit::Proceed { warning: None }
    ));
    let Admit::Proceed {
        warning: Some(warning),
    } = admit_writer(&tree, Some(&live), &configured)
    else {
        panic!("warn sharing should proceed with a warning");
    };
    assert_eq!(warning.code.as_ref(), WORKTREE_SHARED);
    assert!(warning.message.contains(&holder.to_string()));
    assert_eq!(warning.setting.as_ref().unwrap().layer, Layer::Host);
    assert!(matches!(
        admit_writer(
            &tree,
            Some(&live),
            &Sourced {
                value: Sharing::Allow,
                source: configured.source.clone(),
            }
        ),
        Admit::Proceed { warning: None }
    ));
    let refusal = admit_writer(
        &tree,
        Some(&live),
        &Sourced {
            value: Sharing::Refuse,
            source: configured.source.clone(),
        },
    );
    let Admit::Refuse { error } = refusal else {
        panic!("refuse sharing should return a typed refusal");
    };
    assert_eq!(error.code, crate::policy::WORKTREE_SHARING_REFUSED);
    assert_eq!(error.class, ErrorClass::Configured);
    assert_eq!(error.setting.as_ref().unwrap().layer, Layer::Host);

    let owner_refuse = LeaseObservation {
        lease: WorktreeLease {
            sharing: Sharing::Refuse,
            ..lease.clone()
        },
        state: live.state.clone(),
    };
    let owner_refusal = admit_writer(
        &tree,
        Some(&owner_refuse),
        &Sourced {
            value: Sharing::Allow,
            source: configured.source.clone(),
        },
    );
    let Admit::Refuse { error } = owner_refusal else {
        panic!("owning-lane refusal should win over configured allow");
    };
    let setting = error.setting.as_ref().unwrap();
    assert_eq!(setting.layer, Layer::Parent);
    assert_eq!(setting.origin, "lease.json#sharing");
    let unreachable = LeaseObservation {
        lease: lease.clone(),
        state: LeaseState::Unreachable {
            retry_at: WallTime::from_micros(10).unwrap(),
            setting: SettingRef {
                key: "domains.fabric.lanes.lease_unreachable_seconds".into(),
                value: "900".into(),
                layer: Layer::Host,
                origin: "config.toml#domains.fabric.lanes.lease_unreachable_seconds".into(),
            },
        },
    };
    let Admit::Proceed {
        warning: Some(warning),
    } = admit_writer(&tree, Some(&unreachable), &configured)
    else {
        panic!("unreachable holder should remain live during its grace period");
    };
    assert_eq!(warning.code.as_ref(), LEASE_HOLDER_UNREACHABLE);
    assert_eq!(
        warning.setting.as_ref().unwrap().key,
        "domains.fabric.lanes.lease_unreachable_seconds"
    );
    let lapsed_unreachable = LeaseObservation {
        lease: lease.clone(),
        state: LeaseState::LapsedUnreachable {
            retry_at: WallTime::from_micros(10).unwrap(),
            setting: SettingRef {
                key: "domains.fabric.lanes.lease_unreachable_seconds".into(),
                value: "900".into(),
                layer: Layer::Host,
                origin: "config.toml#domains.fabric.lanes.lease_unreachable_seconds".into(),
            },
        },
    };
    let Admit::Proceed {
        warning: Some(warning),
    } = admit_writer(&tree, Some(&lapsed_unreachable), &configured)
    else {
        panic!("lapsed unreachable holder must proceed with a handover warning");
    };
    assert_eq!(warning.code.as_ref(), LEASE_HOLDER_UNREACHABLE);
    assert!(warning.message.contains("lease has lapsed"));
    let lapsed = LeaseObservation {
        lease: lease.clone(),
        state: LeaseState::Lapsed {
            attempt_state: "stopped".into(),
        },
    };
    assert!(matches!(
        admit_writer(
            &tree,
            Some(&lapsed),
            &Sourced {
                value: Sharing::Refuse,
                source: configured.source.clone(),
            }
        ),
        Admit::Proceed { warning: None }
    ));

    assert!(guard_destructive("checkout", &tree, false, None).is_ok());
    let error = guard_destructive("checkout", &tree, true, Some(holder)).unwrap_err();
    assert_eq!(error.code, "DESTROYS_LIVE_WORK");
}

#[test]
fn writer_admission_preserves_warning_code_and_setting_layer_for_each_state() {
    let holder = AttemptId::from_bytes([2; 16]);
    let tree = Subject::worktree("/fixture/tree", "generation-1");
    let source = Source::at(
        Layer::Project,
        ".provenant/config.toml#lanes.worktree.sharing",
    );
    let states = [
        LeaseState::Live {
            attempt_state: "running".into(),
        },
        LeaseState::Unreachable {
            retry_at: WallTime::from_micros(10).unwrap(),
            setting: SettingRef {
                key: "domains.fabric.lanes.lease_unreachable_seconds".into(),
                value: "900".into(),
                layer: Layer::Host,
                origin: "config.toml#domains.fabric.lanes.lease_unreachable_seconds".into(),
            },
        },
        LeaseState::LapsedUnreachable {
            retry_at: WallTime::from_micros(10).unwrap(),
            setting: SettingRef {
                key: "domains.fabric.lanes.lease_unreachable_seconds".into(),
                value: "900".into(),
                layer: Layer::Host,
                origin: "config.toml#domains.fabric.lanes.lease_unreachable_seconds".into(),
            },
        },
        LeaseState::Lapsed {
            attempt_state: "stopped".into(),
        },
    ];
    let settings = [Sharing::Allow, Sharing::Warn, Sharing::Refuse];
    for state in states {
        for value in settings {
            let observation = LeaseObservation {
                lease: WorktreeLease {
                    holder,
                    since: WallTime::from_micros(1).unwrap(),
                    sharing: Sharing::Inherit,
                },
                state: state.clone(),
            };
            let setting = Sourced {
                value,
                source: source.clone(),
            };
            match (
                state.clone(),
                value,
                admit_writer(&tree, Some(&observation), &setting),
            ) {
                (LeaseState::Lapsed { .. }, _, Admit::Proceed { warning: None })
                | (_, Sharing::Allow, Admit::Proceed { warning: None }) => {}
                (
                    LeaseState::LapsedUnreachable { .. },
                    _,
                    Admit::Proceed {
                        warning: Some(warning),
                    },
                ) => {
                    assert_eq!(warning.code.as_ref(), LEASE_HOLDER_UNREACHABLE);
                    assert_eq!(
                        warning.setting.as_ref().unwrap().key,
                        "domains.fabric.lanes.lease_unreachable_seconds"
                    );
                }
                (
                    LeaseState::Live { .. },
                    Sharing::Warn,
                    Admit::Proceed {
                        warning: Some(warning),
                    },
                ) => {
                    assert_eq!(warning.code.as_ref(), WORKTREE_SHARED);
                    assert_eq!(warning.setting.as_ref().unwrap().layer, Layer::Project);
                }
                (
                    LeaseState::Unreachable { .. },
                    Sharing::Warn,
                    Admit::Proceed {
                        warning: Some(warning),
                    },
                ) => {
                    assert_eq!(warning.code.as_ref(), LEASE_HOLDER_UNREACHABLE);
                    assert_eq!(warning.setting.as_ref().unwrap().layer, Layer::Host);
                    assert_eq!(
                        warning.setting.as_ref().unwrap().key,
                        "domains.fabric.lanes.lease_unreachable_seconds"
                    );
                }
                (_, Sharing::Refuse, Admit::Refuse { error }) => {
                    assert_eq!(error.code, crate::policy::WORKTREE_SHARING_REFUSED);
                    assert_eq!(error.setting.as_ref().unwrap().layer, Layer::Project);
                }
                (state, sharing, admission) => panic!(
                    "unexpected admission for {state:?} and {sharing:?}: {}",
                    match admission {
                        Admit::Proceed {
                            warning: Some(warning),
                        } => warning.code.to_string(),
                        Admit::Proceed { warning: None } => "proceed".to_owned(),
                        Admit::Refuse { error } => error.into_payload().code,
                    }
                ),
            }
        }
    }
}

#[test]
fn handover_fact_round_trips_with_preserved_paths() {
    let handover = Handover {
        from: AttemptId::from_bytes([1; 16]),
        to: AttemptId::from_bytes([2; 16]),
        at: WallTime::from_micros(123).unwrap(),
        head: Some(GitOid::parse(&"ab".repeat(20)).unwrap()),
        uncommitted: vec!["src/kept.rs".into(), "notes.txt".into()],
    };
    let value = serde_json::to_value(&handover).unwrap();
    assert_eq!(
        value["uncommitted"],
        serde_json::json!(["src/kept.rs", "notes.txt"])
    );
    assert_eq!(serde_json::from_value::<Handover>(value).unwrap(), handover);
}
