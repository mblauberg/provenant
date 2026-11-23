//! Pre-push refusal policy over pinned, bounded, read-only Git evidence.
use crate::{
    GitRead, UtilityBudget, UtilityRuntime,
    repo::{GitRefName, ReadOnlyRepo},
    utility::{advertised_remote_branch, advertised_remote_head, run_release_read_utility},
};
use provenant_config::{DEFAULT_BRANCH_REWRITE, DefaultBranchRewrite, HistoryEffect, LaneHistory};
use provenant_core::{CancelToken, GitOid};
use provenant_os::exec::{Executor, Termination};
use std::io::Write as _;
use thiserror::Error;

pub struct PushUpdate {
    pub local: GitOid,
    pub remote_ref: String,
    pub remote: GitOid,
}
#[derive(Debug, Error)]
pub enum PushRefusal {
    #[error("Git evidence unavailable or incomplete")]
    Evidence,
    #[error(
        "deleting the default branch is refused; delete a topic branch instead, for example `git push origin --delete my-topic`"
    )]
    DefaultDeletion,
    #[error(
        "pushing the default branch from a linked worktree; push a topic branch and merge by pull request"
    )]
    LinkedWorktree,
    #[error(
        "non-fast-forward update to the default branch is refused; {setting}; set lanes.history.default_branch_rewrite to allow in the host configuration to rewrite it, or integrate the remote first with `git fetch origin && git merge origin/main` (or the default branch), then push again"
    )]
    NonFastForward { setting: String },
    #[error(
        "conflict markers in pushed commits; {setting}; set it to warn or off to push them, or remove the marker lines and `git commit --amend --no-edit`; a pushed topic branch can be updated with `git push --force-with-lease`"
    )]
    ConflictMarkers { setting: String },
}
pub fn is_zero(oid: GitOid) -> bool {
    match oid {
        GitOid::Sha1(b) => b.iter().all(|b| *b == 0),
        GitOid::Sha256(b) => b.iter().all(|b| *b == 0),
    }
}

pub struct PushContext<'a> {
    pub repo: &'a ReadOnlyRepo,
    pub runtime: &'a UtilityRuntime,
    pub executor: &'a dyn Executor,
    pub budget: UtilityBudget,
    pub cancel: &'a CancelToken,
    /// Lane history decisions for conflict markers and the default branch.
    pub history: LaneHistory,
}
impl PushContext<'_> {
    fn read(&self, request: GitRead) -> Result<provenant_os::exec::ExecOutcome, PushRefusal> {
        run_release_read_utility(
            self.executor,
            request,
            self.repo,
            self.runtime,
            self.budget,
            self.cancel,
        )
        .map_err(|_| PushRefusal::Evidence)
    }
    pub fn check(&self, remote: &str, updates: &[PushUpdate]) -> Result<(), PushRefusal> {
        let head = advertised_remote_head(
            self.executor,
            self.repo,
            self.runtime,
            remote,
            self.budget,
            self.cancel,
        )
        .map_err(|_| PushRefusal::Evidence)?;
        if head.termination != Termination::Exit(0) {
            return Err(PushRefusal::Evidence);
        }
        let text = std::str::from_utf8(&head.stdout.bytes).map_err(|_| PushRefusal::Evidence)?;
        let mut targets = text.lines().filter_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        });
        let branch = targets.next().ok_or(PushRefusal::Evidence)?;
        if targets.next().is_some() {
            return Err(PushRefusal::Evidence);
        }
        let default = GitRefName::branch(branch).map_err(|_| PushRefusal::Evidence)?;
        for update in updates {
            let mut is_default = update.remote_ref == default.as_str();
            if !is_default && update.remote_ref.starts_with("refs/heads/") {
                let branch =
                    GitRefName::new(&update.remote_ref).map_err(|_| PushRefusal::Evidence)?;
                let advertised = advertised_remote_branch(
                    self.executor,
                    self.repo,
                    self.runtime,
                    remote,
                    &branch,
                    self.budget,
                    self.cancel,
                )
                .map_err(|_| PushRefusal::Evidence)?;
                if advertised.termination != Termination::Exit(0) {
                    return Err(PushRefusal::Evidence);
                }
                let text = std::str::from_utf8(&advertised.stdout.bytes)
                    .map_err(|_| PushRefusal::Evidence)?;
                is_default = text.lines().any(|line| {
                    line.strip_prefix("ref: ")
                        .and_then(|line| line.split_once('\t'))
                        .is_some_and(|(target, name)| {
                            name == branch.as_str() && target == default.as_str()
                        })
                });
            }
            if is_default {
                if is_zero(update.local) {
                    return Err(PushRefusal::DefaultDeletion);
                }
                if self.repo.0.private_git_dir() != self.repo.0.common_git_dir() {
                    return Err(PushRefusal::LinkedWorktree);
                }
            }
            if is_zero(update.local) {
                continue;
            }
            if is_default && !is_zero(update.remote) {
                let ancestor = self.read(GitRead::MergeBaseIsAncestor {
                    ancestor: update.remote,
                    tip: update.local,
                })?;
                match ancestor.termination {
                    Termination::Exit(0) => {}
                    Termination::Exit(1) => {
                        let setting = self
                            .history
                            .default_branch_rewrite
                            .setting(DEFAULT_BRANCH_REWRITE);
                        match self.history.default_branch_rewrite.value {
                            DefaultBranchRewrite::Allow => {
                                let _ = writeln!(
                                    std::io::stderr().lock(),
                                    "pre-push: warning: rewriting the default branch {branch}; {setting}"
                                );
                            }
                            DefaultBranchRewrite::Refuse => {
                                return Err(PushRefusal::NonFastForward { setting });
                            }
                        }
                    }
                    _ => return Err(PushRefusal::Evidence),
                }
            }
            if self.history.conflict_markers.value != HistoryEffect::Off {
                let patch = self.read(GitRead::PushPatch {
                    base: (!is_zero(update.remote)).then_some(update.remote),
                    head: update.local,
                })?;
                if patch.termination != Termination::Exit(0) {
                    return Err(PushRefusal::Evidence);
                }
                if patch.stdout.bytes.split(|b| *b == b'\n').any(|line| {
                    [b"+<<<<<<<".as_slice(), b"+>>>>>>>".as_slice()]
                        .iter()
                        .any(|marker| {
                            line.strip_prefix(*marker).is_some_and(|tail| {
                                tail.is_empty() || tail.first().is_some_and(u8::is_ascii_whitespace)
                            })
                        })
                }) {
                    let setting = self
                        .history
                        .conflict_markers
                        .setting(provenant_config::CONFLICT_MARKERS);
                    match self.history.conflict_markers.value {
                        HistoryEffect::Warn => {
                            let _ = writeln!(
                                std::io::stderr().lock(),
                                "pre-push: warning: conflict markers in pushed commits; {setting}"
                            );
                        }
                        HistoryEffect::Refuse => {
                            return Err(PushRefusal::ConflictMarkers { setting });
                        }
                        HistoryEffect::Off => {}
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Fixture;
    use std::error::Error;

    #[test]
    fn non_fast_forward_to_topic_branch_is_allowed() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-m", "main"])?;
        let first = fixture.head()?;
        fixture.run(&["commit", "--allow-empty", "-m", "second"])?;
        let second = fixture.head()?;
        let root = fixture.root.to_str().ok_or("non-UTF8 root")?;
        fixture.run(&["remote", "add", "origin", root])?;
        let repo = fixture.runtime.open_read_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        let cancel = CancelToken::new();
        let context = PushContext {
            repo: &repo,
            runtime: &fixture.runtime,
            executor: &fixture.executor,
            budget: fixture.budget()?,
            cancel: &cancel,
            history: LaneHistory::default(),
        };
        // Spec: docs/specs/cli.md#pre-push-refuses-data-loss
        let update = PushUpdate {
            local: first,
            remote_ref: "refs/heads/topic".to_owned(),
            remote: second,
        };
        assert!(context.check("origin", &[update]).is_ok());
        Ok(())
    }

    #[test]
    fn conflict_markers_follow_the_lane_history_setting() -> Result<(), Box<dyn Error>> {
        use provenant_config::{CONFLICT_MARKERS, HistoryEffect, Setting};
        use provenant_records::Layer;
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-m", "main"])?;
        let base = fixture.head()?;
        fixture.run(&["commit", "--allow-empty", "-m", "old topic"])?;
        let old_tip = fixture.head()?;
        fixture.run(&["reset", "--hard", &base.to_string()])?;
        std::fs::write(fixture.root.join("rebased.txt"), "<<<<<<< ours\n")?;
        fixture.run(&["add", "rebased.txt"])?;
        fixture.run(&["commit", "-m", "rebased topic"])?;
        let new_tip = fixture.head()?;
        let root = fixture.root.to_str().ok_or("non-UTF8 root")?;
        fixture.run(&["remote", "add", "origin", root])?;
        let repo = fixture.runtime.open_read_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        let cancel = CancelToken::new();
        for value in [
            HistoryEffect::Warn,
            HistoryEffect::Off,
            HistoryEffect::Refuse,
        ] {
            let history = LaneHistory {
                conflict_markers: Setting {
                    value,
                    source: Layer::Host,
                },
                ..LaneHistory::default()
            };
            let context = PushContext {
                repo: &repo,
                runtime: &fixture.runtime,
                executor: &fixture.executor,
                budget: fixture.budget()?,
                cancel: &cancel,
                history,
            };
            let update = PushUpdate {
                local: new_tip,
                remote_ref: "refs/heads/topic".to_owned(),
                remote: old_tip,
            };
            let result = context.check("origin", &[update]);
            if value == HistoryEffect::Refuse {
                let refusal = result.expect_err("refuse stops the push");
                let message = format!("{refusal}");
                assert!(message.contains(CONFLICT_MARKERS), "{message}");
                assert!(message.contains("source: host"), "{message}");
            } else {
                assert!(result.is_ok(), "{value:?}");
            }
        }
        Ok(())
    }

    #[test]
    fn default_branch_rewrite_is_an_owner_gate() -> Result<(), Box<dyn Error>> {
        use provenant_config::{DEFAULT_BRANCH_REWRITE, DefaultBranchRewrite, Setting};
        use provenant_records::Layer;
        let fixture = Fixture::new()?;
        fixture.run(&["branch", "-m", "main"])?;
        let first = fixture.head()?;
        fixture.run(&["commit", "--allow-empty", "-m", "second"])?;
        let second = fixture.head()?;
        let root = fixture.root.to_str().ok_or("non-UTF8 root")?;
        fixture.run(&["remote", "add", "origin", root])?;
        let repo = fixture.runtime.open_read_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        let cancel = CancelToken::new();
        for (value, allowed) in [
            (DefaultBranchRewrite::Refuse, false),
            (DefaultBranchRewrite::Allow, true),
        ] {
            let history = LaneHistory {
                default_branch_rewrite: Setting {
                    value,
                    source: Layer::Host,
                },
                ..LaneHistory::default()
            };
            let context = PushContext {
                repo: &repo,
                runtime: &fixture.runtime,
                executor: &fixture.executor,
                budget: fixture.budget()?,
                cancel: &cancel,
                history,
            };
            let update = PushUpdate {
                local: first,
                remote_ref: "refs/heads/main".to_owned(),
                remote: second,
            };
            let result = context.check("origin", &[update]);
            assert_eq!(result.is_ok(), allowed, "{value:?}");
            if !allowed {
                let message = format!("{}", result.expect_err("owner gate"));
                assert!(message.contains(DEFAULT_BRANCH_REWRITE), "{message}");
                assert!(message.contains("source: host"), "{message}");
            }
        }
        Ok(())
    }

    #[test]
    fn advertised_default_overrides_stale_tracking_head() -> Result<(), Box<dyn Error>> {
        let fixture = Fixture::new()?;
        let head = fixture.head()?;
        let root = fixture.root.to_str().ok_or("non-UTF8 root")?;
        fixture.run(&["remote", "add", "origin", root])?;
        let symbolic = fixture.run(&["symbolic-ref", "HEAD"])?;
        let default = std::str::from_utf8(&symbolic.stdout.bytes)?.trim();
        fixture.run(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/stale",
        ])?;
        let repo = fixture.runtime.open_read_repo(
            &fixture.executor,
            &fixture.root,
            fixture.budget()?,
            &CancelToken::new(),
        )?;
        let cancel = CancelToken::new();
        let context = PushContext {
            repo: &repo,
            runtime: &fixture.runtime,
            executor: &fixture.executor,
            budget: fixture.budget()?,
            cancel: &cancel,
            history: LaneHistory::default(),
        };
        let update = PushUpdate {
            local: GitOid::parse(&"0".repeat(40))?,
            remote_ref: default.to_owned(),
            remote: head,
        };
        assert!(matches!(
            context.check("origin", &[update]),
            Err(PushRefusal::DefaultDeletion)
        ));
        Ok(())
    }
}
