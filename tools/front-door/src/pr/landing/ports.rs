//! Narrow side-effect boundaries for landing orchestration.
use super::{LandingError, model::*};
use provenant_cli::domains::MergeMethod;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestInfo {
    pub number: u64,
    pub head: String,
    pub base_branch: String,
    pub author: String,
    pub state: PullRequestState,
    pub merged_commit: Option<String>,
    pub stack: Option<StackMembership>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackMembership {
    pub number: u64,
    pub size: u64,
    pub position: u64,
    pub base_branch: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackMember {
    pub number: u64,
    pub state: PullRequestState,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub reference: String,
    pub reviewer: String,
}
pub trait Forge {
    fn set_project_path(&mut self, _path: &std::path::Path) {}
    fn pull_request(&mut self, number: u64) -> Result<PullRequestInfo, LandingError>;
    /// The stack's pull requests in bottom-to-top order.
    fn stack_members(&mut self, number: u64) -> Result<Vec<StackMember>, LandingError>;
    fn repository_identity(&mut self) -> Result<String, LandingError>;
    fn target_branch(&mut self) -> Result<String, LandingError>;
    fn head(&mut self, number: u64) -> Result<String, LandingError>;
    fn approvals(&mut self, number: u64, head: &str) -> Result<Vec<Approval>, LandingError>;
    fn checks(&mut self, commit: &str) -> Result<Vec<GateEvidence>, LandingError>;
    fn checks_with_timeout(
        &mut self,
        commit: &str,
        _timeout: std::time::Duration,
    ) -> Result<Vec<GateEvidence>, LandingError> {
        self.checks(commit)
    }
    fn merge(
        &mut self,
        number: u64,
        head: &str,
        method: MergeMethod,
    ) -> Result<String, LandingError>;
    /// Merge a stack member through the forge's asynchronous operation.
    fn merge_stack(
        &mut self,
        number: u64,
        head: &str,
        method: MergeMethod,
    ) -> Result<String, LandingError>;
    fn may_push_directly(&mut self, branch: &str) -> Result<bool, LandingError>;
    fn labels(&mut self, number: u64) -> Result<Vec<String>, LandingError>;
    /// Whether this adapter can open revert-forward pull requests.
    fn supports_revert_forward(&self) -> bool {
        false
    }
    fn find_revert(
        &mut self,
        _number: u64,
        _branch: &str,
        _target: &str,
    ) -> Result<Option<u64>, LandingError> {
        Ok(None)
    }
    /// Open a pull request reverting a landed pull request's commits; returns its number.
    fn open_revert(
        &mut self,
        number: u64,
        branch: &str,
        target: &str,
        checks: &str,
    ) -> Result<u64, LandingError> {
        let _ = (number, branch, target, checks);
        Err(LandingError::Unimplemented("forge revert pull request"))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeTree {
    Tree(String),
    Conflict,
}
pub trait Repo {
    fn project_path(&self) -> Option<&std::path::Path> {
        None
    }
    /// Complete local follow-up only after a landing is proven. Return false for
    /// a reported refusal that should be retried without blocking other work.
    fn follow_local(&mut self, _pr: &EligiblePr) -> Result<bool, LandingError> {
        Ok(true)
    }
    fn fetch_target(&mut self, branch: &str) -> Result<String, LandingError>;
    fn take_local_reports(&mut self) -> std::collections::BTreeMap<u64, serde_json::Value> {
        Default::default()
    }
    /// Fetch without changing any persistent reference, as required in shadow mode.
    fn fetch_target_untracked(&mut self, _branch: &str) -> Result<String, LandingError> {
        Err(LandingError::Unimplemented("untracked target fetch"))
    }
    fn revert_commit(&mut self, _target: &str, _landed: &str) -> Result<String, LandingError> {
        Err(LandingError::Unimplemented("revert commit"))
    }
    fn merge_tree(&mut self, base: &str, head: &str) -> Result<MergeTree, LandingError>;
    fn commit_tree(
        &mut self,
        tree: &str,
        parents: &[String],
        message: &str,
    ) -> Result<String, LandingError>;
    /// Push without force, conditional on the expected previous value.
    fn push_ref(
        &mut self,
        name: &str,
        commit: &str,
        expected_old: Option<&str>,
    ) -> Result<(), LandingError>;
    fn delete_ref(&mut self, name: &str, expected_old: &str) -> Result<(), LandingError>;
    fn is_ancestor(&mut self, ancestor: &str, descendant: &str) -> Result<bool, LandingError>;
    fn tree_of(&mut self, commit: &str) -> Result<String, LandingError>;
}
pub trait GateRunner {
    fn set_project_path(&mut self, _path: &std::path::Path) {}
    fn gate(
        &mut self,
        candidate: &Candidate,
        definition: &GateDefinition,
        poll_seconds: u64,
    ) -> Result<GateEvidence, LandingError>;
}
pub trait AlarmSink {
    fn set_project_path(&mut self, _path: &std::path::Path) {}
    fn post(&mut self, alarm: &Alarm) -> Result<(), LandingError>;
}

/// Follow a relocated caller when cleanup removes its checkout.
pub fn sync_project(
    repo: &dyn Repo,
    forge: &mut dyn Forge,
    runner: &mut dyn GateRunner,
    alarms: &mut dyn AlarmSink,
) {
    if let Some(path) = repo.project_path() {
        forge.set_project_path(path);
        runner.set_project_path(path);
        alarms.set_project_path(path);
    }
}
