use super::{
    LandingError,
    model::*,
    ports::{Forge, MergeTree, PullRequestState, Repo},
};
use provenant_core::{Clock, MonoTime};

/// Select eligible pull requests in requested order, up to the configured batch limit.
pub fn select_batch(
    forge: &mut dyn Forge,
    numbers: &[u64],
    target: &str,
    limit: usize,
    batch_wait_seconds: u64,
) -> Result<Vec<EligiblePr>, LandingError> {
    select_batch_with_dependencies(forge, numbers, numbers, target, limit, batch_wait_seconds)
}

pub fn select_batch_with_dependencies(
    forge: &mut dyn Forge,
    numbers: &[u64],
    requested: &[u64],
    target: &str,
    limit: usize,
    batch_wait_seconds: u64,
) -> Result<Vec<EligiblePr>, LandingError> {
    if limit == 0 || numbers.is_empty() {
        return Ok(Vec::new());
    }
    let clock = provenant_os::clock::SystemClock::new();
    let start = clock.mono_now().as_micros();
    let budget = batch_wait_seconds.saturating_mul(1_000_000);
    loop {
        let mut selected = Vec::new();
        for &number in numbers {
            if selected.iter().any(|pr: &EligiblePr| pr.number == number) {
                continue;
            }
            let info = forge.pull_request(number)?;
            if let Some(stack) = &info.stack {
                if stack.base_branch != target {
                    continue;
                }
                let members = forge.stack_members(stack.number)?;
                let position = members
                    .iter()
                    .position(|member| member.number == number)
                    .ok_or_else(|| {
                        LandingError::Failed(format!(
                            "stack {} does not contain pull request #{number}",
                            stack.number
                        ))
                    })?;
                let lower = members[..position]
                    .iter()
                    .filter(|member| member.state != PullRequestState::Merged)
                    .collect::<Vec<_>>();
                if let Some(missing) = lower
                    .iter()
                    .find(|member| !requested.contains(&member.number))
                {
                    return Err(LandingError::Refused(format!(
                        "pull request #{number} has unmerged lower pull request #{}",
                        missing.number
                    )));
                }
                if !lower.is_empty() {
                    continue;
                }
            }
            if info.number != number
                || info.state != PullRequestState::Open
                || info.base_branch != target
                || provenant_core::GitOid::parse(&info.head).is_err()
            {
                continue;
            }
            let approval = forge
                .approvals(number, &info.head)?
                .into_iter()
                .find(|approval| {
                    !approval.reference.is_empty()
                        && !approval.reviewer.is_empty()
                        && !approval.reviewer.eq_ignore_ascii_case(&info.author)
                });
            if info.author.is_empty() {
                continue;
            }
            if let Some(approval) = approval {
                selected.push(EligiblePr {
                    number,
                    head: info.head,
                    approval: approval.reference,
                    base_branch: info.base_branch,
                });
                if selected.len() == limit {
                    break;
                }
            }
        }
        if selected.len() == limit
            || selected.len() == numbers.len()
            || clock.mono_now().as_micros().saturating_sub(start) >= budget
        {
            return Ok(selected);
        }
        let now = clock.mono_now().as_micros();
        let deadline = start
            .saturating_add(budget)
            .min(now.saturating_add(1_000_000));
        clock
            .wait_until(MonoTime::from_micros(deadline))
            .map_err(|error| LandingError::Failed(format!("batch wait: {error}")))?;
    }
}
/// Build and push an exact candidate chain from the fetched target and approved heads.
pub fn build_candidate(
    repo: &mut dyn Repo,
    base: &str,
    prs: &[EligiblePr],
    ref_prefix: &str,
) -> Result<Candidate, LandingError> {
    if provenant_core::GitOid::parse(base).is_err()
        || prs.is_empty()
        || prs
            .iter()
            .any(|pr| provenant_core::GitOid::parse(&pr.head).is_err())
    {
        return Err(LandingError::Failed(
            "candidate requires a base and pinned heads".into(),
        ));
    }
    let mut previous = base.to_owned();
    let mut steps = Vec::with_capacity(prs.len());
    for pr in prs {
        let tree = match repo.merge_tree(&previous, &pr.head)? {
            MergeTree::Tree(tree) => tree,
            MergeTree::Conflict => return Err(LandingError::Conflict(pr.number)),
        };
        let message = format!("Merge pull request #{} at {}", pr.number, pr.head);
        let commit = repo.commit_tree(&tree, &[previous, pr.head.clone()], &message)?;
        previous = commit.clone();
        steps.push(CandidateStep {
            pr: pr.clone(),
            commit,
            tree,
        });
    }
    let tree = steps
        .last()
        .ok_or_else(|| LandingError::Failed("empty candidate".into()))?
        .tree
        .clone();
    // A retry may reproduce the same commit while an earlier candidate ref remains.
    static NEXT_REF: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    #[allow(clippy::disallowed_methods, reason = "unique candidate ref boundary")]
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| LandingError::Failed(format!("system clock: {error}")))?
        .as_nanos();
    let sequence = NEXT_REF.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ref_name = format!(
        "{ref_prefix}{}-{previous}-{nanos:x}-{:x}-{sequence:x}",
        prs[0].number,
        std::process::id()
    );
    repo.push_ref(&ref_name, &previous, None)?;
    Ok(Candidate {
        base: base.into(),
        steps,
        commit: previous,
        tree,
        ref_name,
    })
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    use super::*;
    use crate::pr::landing::{
        fakes::{FakeForge, FakeRepo},
        ports::{Approval, PullRequestInfo},
    };
    fn info(number: u64, author: &str, branch: &str) -> PullRequestInfo {
        PullRequestInfo {
            number,
            head: format!("{number:040x}"),
            base_branch: branch.into(),
            author: author.into(),
            state: PullRequestState::Open,
            merged_commit: None,
            stack: None,
        }
    }
    #[test]
    fn upper_stack_selection_names_its_unmerged_lower_member() {
        use crate::pr::landing::ports::{StackMember, StackMembership};
        let mut forge = FakeForge::default();
        let mut upper = info(2, "author", "lower");
        upper.stack = Some(StackMembership {
            number: 42,
            size: 2,
            position: 2,
            base_branch: "main".into(),
        });
        forge.pull_requests.insert(2, upper);
        forge.stacks.insert(
            42,
            vec![
                StackMember {
                    number: 1,
                    state: PullRequestState::Open,
                },
                StackMember {
                    number: 2,
                    state: PullRequestState::Open,
                },
            ],
        );
        let error = select_batch(&mut forge, &[2], "main", 1, 0).unwrap_err();
        assert!(error.to_string().contains("#1"));
        assert!(forge.merge_attempts.is_empty());
    }
    #[test]
    fn stack_selection_names_missing_lower_even_when_another_lower_was_requested() {
        use crate::pr::landing::ports::{StackMember, StackMembership};
        let mut forge = FakeForge::default();
        let mut upper = info(3, "author", "middle");
        upper.stack = Some(StackMembership {
            number: 42,
            size: 3,
            position: 3,
            base_branch: "main".into(),
        });
        forge.pull_requests.insert(3, upper);
        forge.stacks.insert(
            42,
            (1..=3)
                .map(|number| StackMember {
                    number,
                    state: PullRequestState::Open,
                })
                .collect(),
        );
        let error =
            select_batch_with_dependencies(&mut forge, &[3], &[1, 3], "main", 1, 0).unwrap_err();
        assert!(error.to_string().contains("#2"));
    }
    #[test]
    fn selection_requires_independent_approval_at_head_and_target() -> Result<(), LandingError> {
        let mut forge = FakeForge::default();
        for (number, author, branch) in [
            (1, "alice", "main"),
            (2, "bob", "main"),
            (3, "eve", "other"),
            (4, "dave", "main"),
        ] {
            forge
                .pull_requests
                .insert(number, info(number, author, branch));
        }
        for (number, head, reviewer) in [
            (1, "0000000000000000000000000000000000000001", "ALICE"),
            (2, "0000000000000000000000000000000000000002", "carol"),
            (3, "0000000000000000000000000000000000000003", "carol"),
            (4, "old-head", "carol"),
        ] {
            forge.approvals_at_head.insert(
                (number, head.into()),
                vec![Approval {
                    reference: format!("approval-{number}"),
                    reviewer: reviewer.into(),
                }],
            );
        }
        let selected = select_batch(&mut forge, &[4, 2, 1, 3], "main", 4, 0)?;
        assert_eq!(
            selected.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(selected[0].approval, "approval-2");
        Ok(())
    }
    #[test]
    fn selection_keeps_requested_order_and_limit_and_rejects_unpinned_heads()
    -> Result<(), LandingError> {
        let mut forge = FakeForge::default();
        for number in 1..=3 {
            let mut pr = info(number, "author", "main");
            if number == 3 {
                pr.head = "main".into();
            }
            forge.approvals_at_head.insert(
                (number, pr.head.clone()),
                vec![Approval {
                    reference: "review".into(),
                    reviewer: "reviewer".into(),
                }],
            );
            forge.pull_requests.insert(number, pr);
        }
        let selected = select_batch(&mut forge, &[3, 2, 1], "main", 1, 0)?;
        assert_eq!(
            selected.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            vec![2]
        );
        let selected = select_batch(&mut forge, &[3, 2, 1], "main", 3, 0)?;
        assert_eq!(
            selected.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            vec![2, 1]
        );
        let unpinned = vec![EligiblePr {
            head: "main".into(),
            ..selected[0].clone()
        }];
        assert!(
            build_candidate(
                &mut FakeRepo::default(),
                &"a".repeat(40),
                &unpinned,
                "land/"
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn candidate_records_ordered_merge_chain_and_conflict() -> Result<(), LandingError> {
        let prs: Vec<_> = (1..=2)
            .map(|number| EligiblePr {
                number,
                head: format!("{number:040x}"),
                approval: "approved".into(),
                base_branch: "main".into(),
            })
            .collect();
        let mut repo = FakeRepo::default();
        let candidate = build_candidate(&mut repo, &"a".repeat(40), &prs, "land/")?;
        assert_eq!(
            repo.parents[&candidate.steps[0].commit],
            vec!["a".repeat(40), format!("{:040x}", 1)]
        );
        assert_eq!(
            repo.parents[&candidate.steps[1].commit],
            vec![candidate.steps[0].commit.clone(), format!("{:040x}", 2)]
        );
        assert_eq!(candidate.tree, candidate.steps[1].tree);
        assert_eq!(repo.refs.get(&candidate.ref_name), Some(&candidate.commit));
        let mut conflict = FakeRepo::default();
        conflict
            .conflicts
            .insert(("commit-0".into(), format!("{:040x}", 2)));
        assert!(matches!(
            build_candidate(&mut conflict, &"a".repeat(40), &prs, "land/"),
            Err(LandingError::Conflict(2))
        ));
        assert!(conflict.pushes.is_empty());
        Ok(())
    }
}
