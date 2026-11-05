//! Shadow-only classification of a reviewed head moving before landing.
use super::ports::{MergeTree, Repo};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewedHead {
    pub head: String,
    /// The base recorded at approval time; old approvals may lack it.
    pub base: Option<String>,
    pub valid: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum HeadMove {
    A {
        reviewed_head: String,
        reviewed_base: String,
        base_parent: String,
        target: String,
        merge_tree: String,
    },
    B {
        reviewed_head: String,
        reviewed_base: Option<String>,
        path: String,
        added: u64,
        deleted: u64,
        delta_review_required: bool,
    },
    C {
        reviewed_head: String,
        reviewed_base: Option<String>,
        reason: String,
    },
}

/// The result is evidence only and must never be used as landing authority.
/// Missing or unreadable evidence cannot prove a class A or B move.
pub fn classify(
    repo: &mut dyn Repo,
    approval: &ReviewedHead,
    head: &str,
    target: &str,
) -> Option<HeadMove> {
    if approval.head == head {
        return None;
    }
    let class_c = |reason: &str| HeadMove::C {
        reviewed_head: approval.head.clone(),
        reviewed_base: approval.base.clone(),
        reason: reason.into(),
    };
    if !approval.valid {
        return Some(class_c("approval is revoked or unavailable"));
    }
    let parents = match repo.parents(head) {
        Ok(parents) => parents,
        Err(_) => return Some(class_c("commit parents unavailable")),
    };
    if parents.len() == 2 && parents[0] == approval.head {
        let Some(base) = &approval.base else {
            return Some(class_c("approval-time base unknown"));
        };
        let second = &parents[1];
        let valid_ancestry = repo.is_ancestor(base, second).unwrap_or(false)
            && repo.is_ancestor(second, target).unwrap_or(false);
        if valid_ancestry
            && let Ok(MergeTree::Tree(tree)) = repo.merge_tree(&approval.head, second)
            && repo.tree_of(head).is_ok_and(|actual| actual == tree)
        {
            return Some(HeadMove::A {
                reviewed_head: approval.head.clone(),
                reviewed_base: base.clone(),
                base_parent: second.clone(),
                target: target.into(),
                merge_tree: tree,
            });
        }
        return Some(class_c("merge-in proof incomplete or mismatched"));
    }
    if !repo.is_ancestor(&approval.head, head).unwrap_or(false) {
        return Some(class_c("head is not a descendant of the reviewed head"));
    }
    if let Ok(delta) = repo.review_delta(&approval.head, head)
        && delta.complete
        && delta.files.len() == 1
    {
        let file = &delta.files[0];
        if file.eligible
            && file
                .added
                .checked_add(file.deleted)
                .is_some_and(|lines| lines <= 20)
        {
            return Some(HeadMove::B {
                reviewed_head: approval.head.clone(),
                reviewed_base: approval.base.clone(),
                path: file.path.clone(),
                added: file.added,
                deleted: file.deleted,
                delta_review_required: true,
            });
        }
    }
    Some(class_c("delta is ineligible or incomplete"))
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without running them"
)]
mod tests {
    use super::*;
    use crate::pr::landing::{
        fakes::FakeRepo,
        ports::{DeltaFile, DeltaStats},
    };

    #[test]
    fn clean_merge_in_requires_complete_ancestry_and_tree_proof() {
        let mut repo = FakeRepo::default();
        repo.parents
            .insert("head".into(), vec!["reviewed".into(), "base-side".into()]);
        repo.parents
            .insert("base-side".into(), vec!["old-base".into()]);
        repo.parents
            .insert("target".into(), vec!["base-side".into()]);
        repo.trees
            .insert("head".into(), "tree-reviewed-base-side".into());
        let mut approval = ReviewedHead {
            head: "reviewed".into(),
            base: Some("old-base".into()),
            valid: true,
        };
        assert!(matches!(
            classify(&mut repo, &approval, "head", "target"),
            Some(HeadMove::A { .. })
        ));
        assert!(matches!(
            classify(&mut repo, &approval, "head", "other"),
            Some(HeadMove::C { .. })
        ));
        repo.trees.insert("head".into(), "smuggled".into());
        assert!(matches!(
            classify(&mut repo, &approval, "head", "target"),
            Some(HeadMove::C { .. })
        ));
        approval.base = None;
        assert!(
            matches!(classify(&mut repo, &approval, "head", "target"), Some(HeadMove::C { reason, .. }) if reason.contains("unknown"))
        );
        approval.valid = false;
        assert!(matches!(
            classify(&mut repo, &approval, "head", "target"),
            Some(HeadMove::C { .. })
        ));
        assert_eq!(classify(&mut repo, &approval, "reviewed", "target"), None);
    }

    #[test]
    fn small_delta_needs_complete_single_file_statistics() {
        let mut repo = FakeRepo::default();
        repo.parents.insert("head".into(), vec!["reviewed".into()]);
        let key = ("reviewed".into(), "head".into());
        let file = DeltaFile {
            path: "src/lib.rs".into(),
            added: 12,
            deleted: 8,
            eligible: true,
        };
        repo.review_deltas.insert(
            key.clone(),
            DeltaStats {
                complete: true,
                files: vec![file.clone()],
            },
        );
        let approval = ReviewedHead {
            head: "reviewed".into(),
            base: None,
            valid: true,
        };
        assert!(matches!(
            classify(&mut repo, &approval, "head", "target"),
            Some(HeadMove::B {
                delta_review_required: true,
                ..
            })
        ));
        repo.review_deltas.get_mut(&key).unwrap().complete = false;
        assert!(matches!(
            classify(&mut repo, &approval, "head", "target"),
            Some(HeadMove::C { .. })
        ));
        repo.review_deltas.insert(
            key.clone(),
            DeltaStats {
                complete: true,
                files: vec![DeltaFile {
                    eligible: false,
                    ..file.clone()
                }],
            },
        );
        assert!(matches!(
            classify(&mut repo, &approval, "head", "target"),
            Some(HeadMove::C { .. })
        ));
        repo.review_deltas.insert(
            key,
            DeltaStats {
                complete: true,
                files: vec![DeltaFile { added: 13, ..file }],
            },
        );
        assert!(matches!(
            classify(&mut repo, &approval, "head", "target"),
            Some(HeadMove::C { .. })
        ));
    }
}
