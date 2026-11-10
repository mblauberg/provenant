//! Whether a review still covers a head that moved after it.
//!
//! A review of head `H` read against base `B` covers a new head `H'` when
//! `H' == H`, or when, with `B'` the merge base of `H'` and the fetched target,
//! `B` is an ancestor of `B'`, `B'` is an ancestor of the target and
//! `tree(H')` equals `merge_tree(H, B')`. That is a merge-in from the target,
//! or a rebase with an unchanged delta. Anything else needs a review of the
//! changed delta.

/// The Git reads the proof needs, owned by the caller's repository boundary.
pub trait ReviewGit {
    type Error;
    /// The merge base of two commits, or `None` when they share no history.
    fn merge_base(&mut self, left: &str, right: &str) -> Result<Option<String>, Self::Error>;
    fn is_ancestor(&mut self, ancestor: &str, descendant: &str) -> Result<bool, Self::Error>;
    fn tree_of(&mut self, commit: &str) -> Result<String, Self::Error>;
    /// The tree of merging two commits, or `None` when they conflict.
    fn merge_tree(&mut self, left: &str, right: &str) -> Result<Option<String>, Self::Error>;
}

/// The review's own subject: the head it read and the base it read it against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reviewed<'a> {
    pub head: &'a str,
    pub base: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// The head is the reviewed head.
    Exact,
    /// The head carries the reviewed delta unchanged onto a later base.
    CarriedOver,
    /// The head moved and the review recorded no base to prove it against.
    MissingBase,
    /// The head shares no history with the target.
    Unrelated,
    /// The new base does not descend from the reviewed base, or is off the target.
    BaseNotForward,
    /// The head differs from the reviewed delta on the new base.
    DeltaChanged,
}

impl Coverage {
    pub fn covers(self) -> bool {
        matches!(self, Self::Exact | Self::CarriedOver)
    }
    pub fn reason(self) -> &'static str {
        match self {
            Self::Exact => "the head is the reviewed head",
            Self::CarriedOver => "the head carries the reviewed delta onto a later base",
            Self::MissingBase => "the head moved and the review recorded no base",
            Self::Unrelated => "the head shares no history with the target",
            Self::BaseNotForward => "the head's base does not descend from the reviewed base",
            Self::DeltaChanged => "the head changes the reviewed delta",
        }
    }
}

/// Prove whether `reviewed` covers `head` against the fetched `target`.
pub fn coverage<G: ReviewGit + ?Sized>(
    git: &mut G,
    reviewed: Reviewed<'_>,
    head: &str,
    target: &str,
) -> Result<Coverage, G::Error> {
    if head == reviewed.head {
        return Ok(Coverage::Exact);
    }
    let Some(base) = reviewed.base else {
        return Ok(Coverage::MissingBase);
    };
    let Some(new_base) = git.merge_base(head, target)? else {
        return Ok(Coverage::Unrelated);
    };
    if !git.is_ancestor(base, &new_base)? || !git.is_ancestor(&new_base, target)? {
        return Ok(Coverage::BaseNotForward);
    }
    let Some(replayed) = git.merge_tree(reviewed.head, &new_base)? else {
        return Ok(Coverage::DeltaChanged);
    };
    Ok(if git.tree_of(head)? == replayed {
        Coverage::CarriedOver
    } else {
        Coverage::DeltaChanged
    })
}

/// An in-memory history for tests: each commit's tree is the set of changes it
/// holds, and a merge takes the union of both sides.
#[cfg(any(test, feature = "fixture"))]
pub mod history {
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Default)]
    pub struct History {
        commits: BTreeMap<String, (Vec<String>, BTreeSet<String>)>,
    }

    impl History {
        /// Add `commit` with `parents`, holding the parents' changes plus `changes`.
        pub fn commit(&mut self, commit: &str, parents: &[&str], changes: &[&str]) {
            let mut tree: BTreeSet<String> = parents
                .iter()
                .flat_map(|parent| self.commits[*parent].1.clone())
                .collect();
            tree.extend(changes.iter().map(|change| (*change).to_owned()));
            self.commits.insert(
                commit.to_owned(),
                (parents.iter().map(|p| (*p).to_owned()).collect(), tree),
            );
        }
        fn ancestors(&self, commit: &str) -> BTreeSet<String> {
            let mut seen = BTreeSet::new();
            let mut pending = vec![commit.to_owned()];
            while let Some(next) = pending.pop() {
                if seen.insert(next.clone())
                    && let Some((parents, _)) = self.commits.get(&next)
                {
                    pending.extend(parents.iter().cloned());
                }
            }
            seen
        }
    }

    impl super::ReviewGit for History {
        type Error = std::convert::Infallible;
        fn merge_base(&mut self, left: &str, right: &str) -> Result<Option<String>, Self::Error> {
            let right = self.ancestors(right);
            let common: Vec<String> = self
                .ancestors(left)
                .into_iter()
                .filter(|commit| right.contains(commit))
                .collect();
            // The best common ancestor is the one every other common ancestor precedes.
            Ok(common
                .iter()
                .find(|candidate| {
                    let below = self.ancestors(candidate);
                    common.iter().all(|other| below.contains(other))
                })
                .cloned())
        }
        fn is_ancestor(&mut self, ancestor: &str, descendant: &str) -> Result<bool, Self::Error> {
            Ok(self.ancestors(descendant).contains(ancestor))
        }
        fn tree_of(&mut self, commit: &str) -> Result<String, Self::Error> {
            Ok(self.commits[commit]
                .1
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(","))
        }
        fn merge_tree(&mut self, left: &str, right: &str) -> Result<Option<String>, Self::Error> {
            let mut tree = self.commits[left].1.clone();
            tree.extend(self.commits[right].1.iter().cloned());
            Ok(Some(tree.into_iter().collect::<Vec<_>>().join(",")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{history::History, *};

    /// `main` is m0 then m1. The pull request adds `x` on m0 and was
    /// reviewed at `h` against m0.
    fn history() -> History {
        let mut history = History::default();
        history.commit("m0", &[], &["base"]);
        history.commit("m1", &["m0"], &["main-change"]);
        history.commit("h", &["m0"], &["x"]);
        history
    }
    const REVIEWED: Reviewed<'static> = Reviewed {
        head: "h",
        base: Some("m0"),
    };

    #[test]
    fn merge_in_from_the_target_keeps_the_review() {
        let mut git = history();
        git.commit("merged", &["h", "m1"], &[]);
        let result = coverage(&mut git, REVIEWED, "merged", "m1");
        assert_eq!(result, Ok(Coverage::CarriedOver));
    }

    #[test]
    fn rebase_with_an_unchanged_delta_keeps_the_review() {
        let mut git = history();
        git.commit("rebased", &["m1"], &["x"]);
        let result = coverage(&mut git, REVIEWED, "rebased", "m1");
        assert_eq!(result, Ok(Coverage::CarriedOver));
    }

    #[test]
    fn an_extra_change_needs_review() {
        let mut git = history();
        git.commit("rebased", &["m1"], &["x", "one-more-line"]);
        git.commit("merged", &["h", "m1"], &["one-more-line"]);
        for head in ["rebased", "merged"] {
            let result = coverage(&mut git, REVIEWED, head, "m1");
            assert_eq!(result, Ok(Coverage::DeltaChanged), "{head}");
        }
    }

    #[test]
    fn a_missing_reviewed_base_fails_closed() {
        let mut git = history();
        git.commit("rebased", &["m1"], &["x"]);
        let reviewed = Reviewed {
            base: None,
            ..REVIEWED
        };
        let result = coverage(&mut git, reviewed, "rebased", "m1");
        assert_eq!(result, Ok(Coverage::MissingBase));
        assert_eq!(coverage(&mut git, reviewed, "h", "m1"), Ok(Coverage::Exact));
    }

    #[test]
    fn a_base_off_the_target_or_behind_the_reviewed_base_fails_closed() {
        let mut git = history();
        git.commit("side", &["m0"], &["side-change"]);
        // Rebased onto a branch that is not the target: its changes are new delta.
        git.commit("rebased", &["side"], &["x"]);
        let result = coverage(&mut git, REVIEWED, "rebased", "m1");
        assert_eq!(result, Ok(Coverage::DeltaChanged));
        // Reviewed against m1, then rebased back onto m0.
        let reviewed = Reviewed {
            head: "h",
            base: Some("m1"),
        };
        git.commit("back", &["m0"], &["x"]);
        let result = coverage(&mut git, reviewed, "back", "m1");
        assert_eq!(result, Ok(Coverage::BaseNotForward));
        assert!(!Coverage::BaseNotForward.covers());
    }
}
