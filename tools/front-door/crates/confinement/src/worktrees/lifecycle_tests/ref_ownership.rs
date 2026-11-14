//! Ref ownership ledger and the shared landed check against a real repository.
use super::*;

fn ledger_record(
    context: &WorktreeContext<'_>,
    branch: &str,
) -> Result<Option<RefRecord>, Box<dyn Error>> {
    Ok(RefLedger::for_context(context)?.load(&RefEndpoint::Local, &GitRefName::branch(branch)?)?)
}

fn oid_of(fixture: &Fixture, revision: &str) -> Result<GitOid, Box<dyn Error>> {
    let output = fixture.run(&["rev-parse", "--verify", revision])?;
    Ok(GitOid::parse(
        std::str::from_utf8(&output.stdout.bytes)?.trim(),
    )?)
}

#[test]
fn creating_a_branch_records_creation_with_repository_and_full_ref() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let base = fixture.head()?;
    let created = create(
        &context,
        WorktreeCreate {
            branch: "lane/owned".into(),
            base: Some(base.to_string()),
        },
        &cancel(),
    )?;
    assert!(created.warnings.is_empty(), "{:?}", created.warnings);
    let record = ledger_record(&context, "lane/owned")?.ok_or("no ledger record")?;
    assert_eq!(record.repository, test.binding.project_id());
    assert_eq!(record.endpoint, RefEndpoint::Local);
    assert_eq!(record.reference, "refs/heads/lane/owned");
    assert_eq!(record.origin, RefOrigin::Created);
    assert_eq!(record.generation, 1);
    assert_eq!(record.tip, base);
    Ok(())
}

#[test]
fn reusing_an_existing_branch_records_adoption() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "human/topic"])?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    create(
        &context,
        WorktreeCreate {
            branch: "human/topic".into(),
            base: None,
        },
        &cancel(),
    )?;
    let record = ledger_record(&context, "human/topic")?.ok_or("no ledger record")?;
    assert_eq!(record.origin, RefOrigin::Adopted);
    assert_eq!(record.generation, 1);
    Ok(())
}

#[test]
fn recreating_a_ref_at_the_same_commit_is_a_new_generation() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let base = fixture.head()?;
    let request = || WorktreeCreate {
        branch: "lane/again".into(),
        base: Some(base.to_string()),
    };
    create(&context, request(), &cancel())?;
    let first = ledger_record(&context, "lane/again")?.ok_or("no first record")?;
    remove(
        &context,
        WorktreeRemove::Name {
            name: "lane/again".into(),
            discard: true,
            integration: None,
            expected_head: None,
        },
        &cancel(),
    )?;
    fixture.run(&["branch", "-D", "lane/again"]).ok();
    assert!(!branch_exists(&fixture, "lane/again"));
    create(&context, request(), &cancel())?;
    let second = ledger_record(&context, "lane/again")?.ok_or("no second record")?;
    assert_eq!(first.tip, second.tip);
    assert_eq!(second.origin, RefOrigin::Created);
    assert_eq!(second.generation, first.generation + 1);
    Ok(())
}

#[test]
fn adoption_keeps_a_created_generation_only_at_the_recorded_tip() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let ledger = RefLedger::for_context(&context)?;
    let reference = GitRefName::branch("lane/kept")?;
    let observe = |origin, tip| RefObservation {
        endpoint: RefEndpoint::Local,
        reference: reference.clone(),
        origin,
        tip,
        recorded_at: test.clock.wall_now().expect("wall clock"),
        attempt_id: None,
        operation_id: None,
    };
    let head = fixture.head()?;
    let created = ledger.record(observe(RefOrigin::Created, head), &test.clock, &cancel())?;
    let same = ledger.record(observe(RefOrigin::Adopted, head), &test.clock, &cancel())?;
    assert_eq!(same, created);
    fixture.run(&["commit", "--allow-empty", "-m", "moved"])?;
    let moved = fixture.head()?;
    let adopted = ledger.record(observe(RefOrigin::Adopted, moved), &test.clock, &cancel())?;
    assert_eq!(adopted.origin, RefOrigin::Adopted);
    assert_eq!(adopted.generation, created.generation + 1);
    Ok(())
}

#[test]
fn cooperating_writers_share_one_ledger_per_repository() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let endpoint = RefEndpoint::Remote {
        remote: "upstream".into(),
        push_url: "https://example.invalid/repo.git".into(),
    };
    let reference = GitRefName::branch("lane/pushed")?;
    // A second writer opens the same ledger from the state root alone.
    let other = RefLedger::open(fixture._temp.path(), test.binding.project_id())?;
    let observation = RefObservation {
        endpoint: endpoint.clone(),
        reference: reference.clone(),
        origin: RefOrigin::Created,
        tip: fixture.head()?,
        recorded_at: test.clock.wall_now()?,
        attempt_id: Some(AttemptId::from_bytes([9; 16])),
        operation_id: None,
    };
    let first = other.record(observation.clone(), &test.clock, &cancel())?;
    // The same attempt replaying its sealed receipt is not a recreation.
    let replay = other.record(observation, &test.clock, &cancel())?;
    assert_eq!(replay, first);
    let seen = RefLedger::for_context(&context)?
        .load(&endpoint, &reference)?
        .ok_or("record not shared")?;
    assert_eq!(seen.endpoint, endpoint);
    assert_eq!(seen.origin, RefOrigin::Created);
    // The local ref of the same name is a different, unobserved ref.
    assert!(ledger_record(&context, "lane/pushed")?.is_none());
    Ok(())
}

#[test]
fn lane_owners_link_the_created_generation() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let info = create(
        &context,
        WorktreeCreate {
            branch: "lane/linked".into(),
            base: None,
        },
        &cancel(),
    )?;
    let tree = verify_worktree(context.repo, &info.path)?;
    save_lane_owner_when_free(
        &context,
        &tree,
        &WorktreeOwner {
            attempt_id: Some(AttemptId::from_bytes([7; 16])),
            operation_id: Some("linked-operation".parse()?),
            branch: "lane/linked".into(),
            base: info.head.to_string(),
            host: provenant_core::HostId::from_bytes([1; 16]),
            created_at: test.clock.wall_now()?,
            origin: WorktreeOrigin::Lane,
            label: None,
            reference: branch_ownership(&context, "lane/linked", info.head)?,
        },
        info.head,
        None,
        false,
        &cancel(),
    )?;
    let owner = load_owner(&tree)?.ok_or("owner missing")?;
    let link = owner.reference.ok_or("owner has no ref link")?;
    assert_eq!(link.reference, "refs/heads/lane/linked");
    assert_eq!(link.origin, RefOrigin::Created);
    assert_eq!(link.repository, test.binding.project_id());
    Ok(())
}

#[test]
fn adopted_owner_without_a_ref_link_stays_unobserved() -> Result<(), Box<dyn Error>> {
    // An adopted checkout can lack observed branch ownership.
    let owner: WorktreeOwner = serde_json::from_value(serde_json::json!({
        "attempt_id": null,
        "operation_id": null,
        "branch": "human/adopted",
        "base": "1111111111111111111111111111111111111111",
        "host": provenant_core::HostId::from_bytes([1; 16]),
        "created_at": SystemClock.wall_now()?,
        "origin": "adopted",
    }))?;
    assert!(owner.reference.is_none());
    Ok(())
}

#[test]
fn landed_check_uses_ancestry_and_refuses_rewritten_or_missing_commits()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let graph = RepoGraph {
        context: &context,
        cancel: &cancel(),
    };
    let base = fixture.head()?;
    let primary = fixture.run(&["symbolic-ref", "--short", "HEAD"])?;
    let primary = std::str::from_utf8(&primary.stdout.bytes)?
        .trim()
        .to_owned();
    fixture.run(&["checkout", "-q", "-b", "lane/work"])?;
    fixture.run(&["commit", "--allow-empty", "-m", "lane work"])?;
    let lane = fixture.head()?;
    fixture.run(&["checkout", "-q", &primary])?;
    fixture.run(&["merge", "-q", "--no-ff", "lane/work", "-m", "land"])?;
    let main = LandingTarget {
        reference: Some(GitRefName::branch(&primary)?),
        tip: fixture.head()?,
    };
    assert_eq!(
        landed(&graph, lane, &main, &[])?,
        Landing::Landed(LandedBy::Ancestry)
    );
    // Same tree, no ancestry: a rewrite of the lane commit is not landed.
    let tree = oid_of(&fixture, "lane/work^{tree}")?;
    let rewrite = fixture.run(&[
        "commit-tree",
        &tree.to_string(),
        "-p",
        &base.to_string(),
        "-m",
        "same tree",
    ])?;
    let rewrite = GitOid::parse(std::str::from_utf8(&rewrite.stdout.bytes)?.trim())?;
    assert_eq!(
        landed(&graph, rewrite, &main, &[])?,
        Landing::Unproven(Unproven::NotContained)
    );
    // A force-pushed target that dropped the merge.
    let rewound = LandingTarget {
        reference: main.reference.clone(),
        tip: base,
    };
    assert_eq!(
        landed(&graph, lane, &rewound, &[])?,
        Landing::Unproven(Unproven::NotContained)
    );
    // A receipt naming an integration commit that is not in the store.
    let absent = GitOid::parse(&"ab".repeat(20))?;
    let chain = [IntegrationEdge {
        source: GitRefName::branch("lane/work")?,
        head: lane,
        target: GitRefName::branch(&primary)?,
        previous: Some(base),
        integrated: absent,
    }];
    assert_eq!(
        landed(&graph, rewrite, &main, &chain)?,
        Landing::Unproven(Unproven::ChainBroken { index: 0 })
    );
    assert_eq!(
        landed(&graph, lane, &rewound, &chain)?,
        Landing::Unproven(Unproven::MissingCommit { oid: absent })
    );
    Ok(())
}

#[test]
fn ledger_lists_current_records_and_forgets_only_the_generation_acted_on()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let base = fixture.head()?;
    for branch in ["lane/one", "lane/two"] {
        create(
            &context,
            WorktreeCreate {
                branch: branch.into(),
                base: Some(base.to_string()),
            },
            &cancel(),
        )?;
    }
    let ledger = RefLedger::for_context(&context)?;
    let mut listed = ledger
        .list()?
        .into_iter()
        .map(|record| record.reference)
        .collect::<Vec<_>>();
    listed.sort();
    assert_eq!(listed, ["refs/heads/lane/one", "refs/heads/lane/two"]);
    let one = ledger_record(&context, "lane/one")?.ok_or("no record")?;
    // A newer generation is not forgotten on the strength of an older one.
    let mut stale = one.clone();
    stale.generation += 1;
    assert!(!ledger.forget(&stale, &test.clock, &cancel())?);
    assert!(ledger.forget(&one, &test.clock, &cancel())?);
    assert!(ledger_record(&context, "lane/one")?.is_none());
    assert_eq!(ledger.list()?.len(), 1);
    Ok(())
}

#[test]
fn only_a_created_branch_at_its_linked_generation_is_a_removal_candidate()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    fixture.run(&["branch", "human/topic"])?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let base = fixture.head()?;
    create(
        &context,
        WorktreeCreate {
            branch: "lane/made".into(),
            base: Some(base.to_string()),
        },
        &cancel(),
    )?;
    create(
        &context,
        WorktreeCreate {
            branch: "human/topic".into(),
            base: None,
        },
        &cancel(),
    )?;
    let made = GitRefName::branch("lane/made")?;
    let record = created_branch(&context, &made, None).ok_or("created branch refused")?;
    assert_eq!(
        created_branch(&context, &made, Some(&record.ownership())),
        Some(record.clone())
    );
    let mut older = record.ownership();
    older.generation += 1;
    assert!(created_branch(&context, &made, Some(&older)).is_none());
    assert!(created_branch(&context, &GitRefName::branch("human/topic")?, None).is_none());
    assert!(created_branch(&context, &GitRefName::branch("never/seen")?, None).is_none());
    Ok(())
}

#[test]
fn leased_ref_deletion_keeps_a_moved_or_checked_out_branch() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let base = fixture.head()?;
    let lane = create(
        &context,
        WorktreeCreate {
            branch: "lane/leased".into(),
            base: Some(base.to_string()),
        },
        &cancel(),
    )?;
    let reference = GitRefName::branch("lane/leased")?;
    assert_eq!(
        delete_ref_leased(&context, &reference, base, &cancel())?,
        RefDeletion::CheckedOut
    );
    fixture.run(&[
        "worktree",
        "remove",
        "--force",
        lane.path.to_str().ok_or("path")?,
    ])?;
    fixture.run(&["commit", "--allow-empty", "-m", "elsewhere"])?;
    let moved = fixture.head()?;
    fixture.run(&["branch", "-f", "lane/leased", &moved.to_string()])?;
    assert_eq!(
        delete_ref_leased(&context, &reference, base, &cancel())?,
        RefDeletion::Changed
    );
    assert!(branch_exists(&fixture, "lane/leased"));
    assert_eq!(
        delete_ref_leased(&context, &reference, moved, &cancel())?,
        RefDeletion::Removed
    );
    assert!(!branch_exists(&fixture, "lane/leased"));
    assert_eq!(
        delete_ref_leased(&context, &reference, moved, &cancel())?,
        RefDeletion::Absent
    );
    assert_eq!(ref_tip(&context, &reference, &cancel())?, None);
    Ok(())
}
