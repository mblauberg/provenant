//! Scriptable in-memory boundaries for driver and lane tests.
use super::{LandingError, model::*, ports::*};
use provenant_cli::domains::MergeMethod;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type WaitReads = (
    std::rc::Rc<std::cell::Cell<bool>>,
    std::rc::Rc<std::cell::Cell<usize>>,
);

#[derive(Default)]
pub struct FakeForge {
    pub target_branch: String,
    pub shared_target: Option<std::rc::Rc<std::cell::RefCell<String>>>,
    pub move_after_merge: Option<(u64, String)>,
    pub pull_requests: BTreeMap<u64, PullRequestInfo>,
    pub stacks: BTreeMap<u64, Vec<StackMember>>,
    pub after_wait_reads: Option<WaitReads>,
    pub heads: BTreeMap<u64, String>,
    pub head_reads: BTreeMap<u64, VecDeque<String>>,
    pub approvals_at_head: BTreeMap<(u64, String), Vec<Approval>>,
    pub latest_reviews: BTreeMap<u64, super::head_move::ReviewedHead>,
    pub check_runs: BTreeMap<String, Vec<GateEvidence>>,
    pub check_timeouts: Vec<std::time::Duration>,
    pub labels: BTreeMap<u64, Vec<String>>,
    pub direct_push: bool,
    pub lose_merge_response: bool,
    pub lose_merge_response_at: Option<u64>,
    pub hide_merge_commit_at: Option<u64>,
    pub refuse_merge: Option<u64>,
    pub fail_settle_write_on_refusal: bool,
    pub fail_merge: Option<u64>,
    pub fail_fetch_after_merge: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    pub fail_fetch_on_refusal: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    pub fail_approvals: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    pub valid_merge_ids: bool,
    pub merges: Vec<(u64, String, MergeMethod)>,
    pub merge_attempts: Vec<u64>,
    pub stack_merge_attempts: Vec<u64>,
    pub reverts: Vec<(u64, String, String, String)>,
}
impl Forge for FakeForge {
    fn stack_members(&mut self, number: u64) -> Result<Vec<StackMember>, LandingError> {
        self.stacks
            .get(&number)
            .cloned()
            .ok_or_else(|| LandingError::Failed("missing stack".into()))
    }
    fn repository_identity(&mut self) -> Result<String, LandingError> {
        Ok("fake-repository".into())
    }
    fn pull_request(&mut self, number: u64) -> Result<PullRequestInfo, LandingError> {
        if let Some((waited, reads)) = &self.after_wait_reads
            && waited.get()
        {
            reads.set(reads.get() + 1);
        }
        self.pull_requests
            .get(&number)
            .cloned()
            .ok_or_else(|| LandingError::Failed("missing pull request".into()))
    }
    fn target_branch(&mut self) -> Result<String, LandingError> {
        Ok(self.target_branch.clone())
    }
    fn head(&mut self, number: u64) -> Result<String, LandingError> {
        if let Some(head) = self
            .head_reads
            .get_mut(&number)
            .and_then(VecDeque::pop_front)
        {
            return Ok(head);
        }
        self.heads
            .get(&number)
            .cloned()
            .ok_or_else(|| LandingError::Failed("missing head".into()))
    }
    fn approvals(&mut self, number: u64, head: &str) -> Result<Vec<Approval>, LandingError> {
        if self.fail_approvals.as_ref().is_some_and(|flag| flag.get()) {
            return Err(LandingError::Failed("approvals unavailable".into()));
        }
        Ok(self
            .approvals_at_head
            .get(&(number, head.into()))
            .cloned()
            .unwrap_or_default())
    }
    fn latest_review(
        &mut self,
        number: u64,
        _author: &str,
    ) -> Result<Option<super::head_move::ReviewedHead>, LandingError> {
        Ok(self.latest_reviews.get(&number).cloned())
    }
    #[allow(clippy::disallowed_methods, reason = "simulate a slow forge in tests")]
    fn checks(&mut self, commit: &str) -> Result<Vec<GateEvidence>, LandingError> {
        Ok(self.check_runs.get(commit).cloned().unwrap_or_default())
    }
    fn checks_with_timeout(
        &mut self,
        commit: &str,
        timeout: std::time::Duration,
    ) -> Result<Vec<GateEvidence>, LandingError> {
        self.check_timeouts.push(timeout);
        self.checks(commit)
    }
    fn merge(
        &mut self,
        number: u64,
        head: &str,
        method: MergeMethod,
    ) -> Result<String, LandingError> {
        self.merge_attempts.push(number);
        if self.head(number)? != head {
            return Err(LandingError::Failed("head moved".into()));
        }
        if self.fail_merge == Some(number) {
            return Err(LandingError::Failed("transport unavailable".into()));
        }
        if self.refuse_merge == Some(number) {
            if self.fail_settle_write_on_refusal {
                super::receipt::FAIL_SETTLE_WRITE.with(|flag| flag.set(true));
                self.fail_settle_write_on_refusal = false;
            }
            if let Some(flag) = &self.fail_fetch_on_refusal {
                flag.set(true);
            }
            return Err(LandingError::Refused("merge refused".into()));
        }
        self.merges.push((number, head.into(), method));
        if let Some(flag) = &self.fail_fetch_after_merge {
            flag.set(true);
        }
        let landed = if self.valid_merge_ids {
            format!("{:040x}", 2000 + number)
        } else {
            format!("landed-{number}")
        };
        if let Some(target) = &self.shared_target {
            *target.borrow_mut() = self
                .move_after_merge
                .as_ref()
                .filter(|(n, _)| *n == number)
                .map_or_else(|| landed.clone(), |(_, tip)| tip.clone());
        }
        if let Some(pr) = self.pull_requests.get_mut(&number) {
            pr.state = PullRequestState::Merged;
            pr.merged_commit =
                (self.hide_merge_commit_at != Some(number)).then_some(landed.clone());
        }
        if self.lose_merge_response || self.lose_merge_response_at == Some(number) {
            return Err(LandingError::Failed("lost response".into()));
        }
        Ok(landed)
    }
    fn merge_stack(
        &mut self,
        number: u64,
        head: &str,
        method: MergeMethod,
    ) -> Result<String, LandingError> {
        self.stack_merge_attempts.push(number);
        let result = self.merge(number, head, method);
        if let Some(stack) = self
            .pull_requests
            .get(&number)
            .and_then(|pr| pr.stack.clone())
            && self
                .pull_requests
                .get(&number)
                .is_some_and(|pr| pr.state == PullRequestState::Merged)
            && let Some(members) = self.stacks.get_mut(&stack.number)
            && let Some(position) = members.iter().position(|member| member.number == number)
        {
            members[position].state = PullRequestState::Merged;
            if let Some(upper) = members[position + 1..]
                .iter()
                .find(|member| member.state == PullRequestState::Open)
                && let Some(pr) = self.pull_requests.get_mut(&upper.number)
            {
                pr.base_branch = stack.base_branch;
            }
        }
        result
    }
    fn may_push_directly(&mut self, _branch: &str) -> Result<bool, LandingError> {
        Ok(self.direct_push)
    }
    fn labels(&mut self, number: u64) -> Result<Vec<String>, LandingError> {
        Ok(self.labels.get(&number).cloned().unwrap_or_default())
    }
    fn supports_revert_forward(&self) -> bool {
        true
    }
    fn open_revert(
        &mut self,
        number: u64,
        branch: &str,
        target: &str,
        checks: &str,
    ) -> Result<u64, LandingError> {
        self.reverts
            .push((number, branch.into(), target.into(), checks.into()));
        Ok(9000 + number)
    }
}
#[derive(Default)]
pub struct FakeRepo {
    pub target: String,
    pub shared_target: Option<std::rc::Rc<std::cell::RefCell<String>>>,
    pub fail_fetch: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    pub valid_commit_ids: bool,
    pub target_ref: String,
    pub fetches: VecDeque<String>,
    pub conflicts: BTreeSet<(String, String)>,
    pub refs: BTreeMap<String, String>,
    pub trees: BTreeMap<String, String>,
    pub parents: BTreeMap<String, Vec<String>>,
    pub review_deltas: BTreeMap<(String, String), DeltaStats>,
    pub pushes: Vec<(String, String, Option<String>)>,
    pub push_attempts: Vec<String>,
    pub pending_dir_to_observe: Option<std::path::PathBuf>,
    pub saw_pending_on_target_push: bool,
    pub refuse_push: bool,
    pub refuse_target_push: bool,
    pub fail_fetch_on_push_refusal: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    pub lose_push_response: bool,
    pub refuse_delete: bool,
    pub fail_local: bool,
    pub fail_local_at: Option<u64>,
    pub refuse_local: bool,
    pub local_followed: Vec<u64>,
}
impl Repo for FakeRepo {
    fn follow_local(&mut self, pr: &EligiblePr) -> Result<bool, LandingError> {
        self.local_followed.push(pr.number);
        if self.fail_local || self.fail_local_at == Some(pr.number) {
            Err(LandingError::LocalFollowUp(
                "local follow-up unavailable".into(),
            ))
        } else {
            Ok(!self.refuse_local)
        }
    }
    fn fetch_target(&mut self, branch: &str) -> Result<String, LandingError> {
        if self.fail_fetch.as_ref().is_some_and(|flag| flag.get()) {
            return Err(LandingError::Failed("fetch unavailable".into()));
        }
        if let Some(target) = &self.shared_target {
            self.target = target.borrow().clone();
        }
        if let Some(next) = self.fetches.pop_front() {
            self.target = next;
        }
        self.target_ref = branch.into();
        self.refs.insert(branch.into(), self.target.clone());
        Ok(self.target.clone())
    }
    fn fetch_target_untracked(&mut self, branch: &str) -> Result<String, LandingError> {
        if let Some(next) = self.fetches.pop_front() {
            self.target = next;
        }
        self.target_ref = branch.into();
        Ok(self.target.clone())
    }
    fn revert_commit(&mut self, _target: &str, landed: &str) -> Result<String, LandingError> {
        Ok(landed.into())
    }
    fn merge_tree(&mut self, base: &str, head: &str) -> Result<MergeTree, LandingError> {
        if self.conflicts.contains(&(base.into(), head.into())) {
            Ok(MergeTree::Conflict)
        } else {
            Ok(MergeTree::Tree(format!("tree-{base}-{head}")))
        }
    }
    fn commit_tree(
        &mut self,
        tree: &str,
        parents: &[String],
        _message: &str,
    ) -> Result<String, LandingError> {
        let commit = if self.valid_commit_ids {
            format!("{:040x}", self.parents.len() + 1000)
        } else {
            format!("commit-{}", self.parents.len())
        };
        self.parents.insert(commit.clone(), parents.into());
        self.trees.insert(commit.clone(), tree.into());
        Ok(commit)
    }
    fn push_ref(
        &mut self,
        name: &str,
        commit: &str,
        expected_old: Option<&str>,
    ) -> Result<(), LandingError> {
        self.push_attempts.push(name.into());
        if name == self.target_ref {
            self.saw_pending_on_target_push =
                self.pending_dir_to_observe.as_ref().is_some_and(|dir| {
                    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
                });
        }
        if self.refuse_push || (self.refuse_target_push && name == self.target_ref) {
            if let Some(flag) = &self.fail_fetch_on_push_refusal {
                flag.set(true);
            }
            return Err(LandingError::Refused("push refused".into()));
        }
        if self.refs.get(name).map(String::as_str) != expected_old {
            return Err(LandingError::Refused("ref moved".into()));
        }
        self.refs.insert(name.into(), commit.into());
        if name == self.target_ref {
            self.target = commit.into();
        }
        self.pushes
            .push((name.into(), commit.into(), expected_old.map(str::to_owned)));
        if self.lose_push_response && name == self.target_ref {
            return Err(LandingError::Failed("lost response".into()));
        }
        Ok(())
    }
    fn delete_ref(&mut self, name: &str, expected_old: &str) -> Result<(), LandingError> {
        if self.refuse_delete {
            return Err(LandingError::Failed("delete refused".into()));
        }
        if !self.refs.contains_key(name) {
            return Ok(());
        }
        if self.refs.get(name).map(String::as_str) != Some(expected_old) {
            return Err(LandingError::Failed("ref moved".into()));
        }
        self.refs.remove(name);
        Ok(())
    }
    fn is_ancestor(&mut self, ancestor: &str, descendant: &str) -> Result<bool, LandingError> {
        let mut pending = vec![descendant.to_owned()];
        let mut seen = BTreeSet::new();
        while let Some(commit) = pending.pop() {
            if commit == ancestor {
                return Ok(true);
            }
            if seen.insert(commit.clone()) {
                pending.extend(self.parents.get(&commit).cloned().unwrap_or_default());
            }
        }
        Ok(false)
    }
    fn tree_of(&mut self, commit: &str) -> Result<String, LandingError> {
        self.trees
            .get(commit)
            .cloned()
            .ok_or_else(|| LandingError::Failed("missing tree".into()))
    }
    fn parents(&mut self, commit: &str) -> Result<Vec<String>, LandingError> {
        self.parents
            .get(commit)
            .cloned()
            .ok_or_else(|| LandingError::Failed("missing parents".into()))
    }
    fn review_delta(&mut self, old: &str, new: &str) -> Result<DeltaStats, LandingError> {
        self.review_deltas
            .get(&(old.into(), new.into()))
            .cloned()
            .ok_or_else(|| LandingError::Failed("missing delta".into()))
    }
}
#[derive(Default)]
pub struct FakeGate {
    pub results: VecDeque<GateEvidence>,
    pub observed: Vec<String>,
    pub auto: bool,
    pub red_head: Option<String>,
    pub check_result: Option<GateResult>,
    pub omit_check: bool,
    pub wrong_tree: bool,
    pub wrong_issuer: bool,
    pub pool: Option<String>,
    pub duration_seconds: Option<u64>,
}
impl GateRunner for FakeGate {
    fn gate(
        &mut self,
        candidate: &Candidate,
        definition: &GateDefinition,
        _poll_seconds: u64,
    ) -> Result<GateEvidence, LandingError> {
        self.observed.push(candidate.commit.clone());
        if self.auto {
            let red = self
                .red_head
                .as_ref()
                .is_some_and(|head| candidate.steps.iter().any(|step| &step.pr.head == head));
            let result = self.check_result.clone().unwrap_or(if red {
                GateResult::Failed
            } else {
                GateResult::Passed
            });
            return Ok(GateEvidence {
                commit: candidate.commit.clone(),
                tree: if self.wrong_tree {
                    "other-tree".into()
                } else {
                    candidate.tree.clone()
                },
                definition_digest: definition.digest.clone(),
                checks: definition
                    .checks
                    .iter()
                    .filter(|_| !self.omit_check)
                    .map(|name| CheckEvidence {
                        name: name.clone(),
                        issuer: if self.wrong_issuer {
                            Some("wrong".into())
                        } else {
                            definition.issuer.clone()
                        },
                        result: result.clone(),
                    })
                    .collect(),
                issuer: None,
                coverage: vec![],
                result,
                pool: self.pool.clone(),
                duration_seconds: self.duration_seconds,
            });
        }
        self.results
            .pop_front()
            .ok_or_else(|| LandingError::Failed("missing gate result".into()))
    }
}
#[derive(Default)]
pub struct FakeAlarms(pub Vec<Alarm>, pub bool);
impl AlarmSink for FakeAlarms {
    fn post(&mut self, alarm: &Alarm) -> Result<(), LandingError> {
        if self.1 {
            return Err(LandingError::Failed("alarm unavailable".into()));
        }
        self.0.push(alarm.clone());
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    use super::*;

    #[test]
    fn stale_expected_old_push_is_refused() -> Result<(), LandingError> {
        let mut repo = FakeRepo {
            target: "new".into(),
            ..FakeRepo::default()
        };
        repo.fetch_target("main")?;
        assert!(matches!(
            repo.push_ref("main", "candidate", Some("old")),
            Err(LandingError::Refused(_))
        ));
        Ok(())
    }

    #[test]
    fn lost_merge_response_leaves_observable_forge_state() -> Result<(), LandingError> {
        let mut forge = FakeForge {
            lose_merge_response: true,
            ..FakeForge::default()
        };
        forge.heads.insert(7, "head".into());
        forge.pull_requests.insert(
            7,
            PullRequestInfo {
                number: 7,
                head: "head".into(),
                base_branch: "main".into(),
                author: "author".into(),
                state: PullRequestState::Open,
                merged_commit: None,
                stack: None,
            },
        );
        assert!(forge.merge(7, "head", MergeMethod::Merge).is_err());
        let observed = forge.pull_request(7)?;
        assert_eq!(observed.state, PullRequestState::Merged);
        assert_eq!(observed.merged_commit.as_deref(), Some("landed-7"));
        Ok(())
    }

    #[test]
    fn lost_push_response_keeps_the_advanced_target_for_reconciliation() -> Result<(), LandingError>
    {
        let mut repo = FakeRepo {
            target: "base".into(),
            ..FakeRepo::default()
        };
        let base = repo.fetch_target("main")?;
        let commit = repo.commit_tree("tree", std::slice::from_ref(&base), "merge")?;
        repo.lose_push_response = true;
        assert!(repo.push_ref("main", &commit, Some(&base)).is_err());
        let observed = repo.fetch_target("main")?;
        assert_eq!(observed, commit);
        assert!(repo.is_ancestor(&base, &observed)?);
        assert_eq!(repo.tree_of(&observed)?, "tree");
        Ok(())
    }
}
