//! The one "has this landed" check. Removal paths and delivery ask it rather
//! than each spelling its own ancestry test.
//!
//! A commit has landed when it is an ancestor of the freshly read target, or
//! when a chain of integration receipts carries it there and every link still
//! holds in the object graph. Anything else is unproven, which retains. Tree
//! and patch-id equality are never evidence: they give false positives and
//! misses.
use super::*;

/// The authorised root: an integration ref and the tip the caller read from
/// it for this decision, after fetching it when it is remote. A target known
/// only by its tip proves ancestry but authorises no receipt chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandingTarget {
    pub reference: Option<GitRefName>,
    pub tip: GitOid,
}

/// One delivery receipt: `source` at `head` was integrated into `target`,
/// which moved from `previous` (absent when the ref was created) to
/// `integrated`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationEdge {
    pub source: GitRefName,
    pub head: GitOid,
    pub target: GitRefName,
    pub previous: Option<GitOid>,
    pub integrated: GitOid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandedBy {
    Ancestry,
    ReceiptChain { edges: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unproven {
    /// The target does not contain the commit, for example after a force push.
    NotContained,
    /// A commit the proof needs is not in the object store.
    MissingCommit { oid: GitOid },
    /// A receipt's integration commit is not a fast-forward, merge or single
    /// commit on the recorded previous target tip.
    ReceiptMismatch { index: usize },
    /// A receipt does not follow from the previous link.
    ChainBroken { index: usize },
    /// The chain ends somewhere other than the authorised root.
    UnauthorisedRoot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    Landed(LandedBy),
    Unproven(Unproven),
}

impl Landing {
    pub fn is_landed(&self) -> bool {
        matches!(self, Self::Landed(_))
    }
}

/// The object-graph reads the check needs. Implementations report a missing
/// commit as `None`, never as an error, so a missing object is unproven.
pub trait CommitGraph {
    /// Parents of `commit`, or `None` when the object store lacks it.
    fn parents(&self, commit: GitOid) -> Result<Option<Vec<GitOid>>, WorktreeError>;
    /// Whether `ancestor` is reachable from `tip`. Both exist.
    fn contains(&self, ancestor: GitOid, tip: GitOid) -> Result<bool, WorktreeError>;
}

/// Decide whether `tip` has landed on `root`. `chain` lists receipts from the
/// one that delivered `tip` to the one that integrated into `root`.
pub fn landed(
    graph: &dyn CommitGraph,
    tip: GitOid,
    root: &LandingTarget,
    chain: &[IntegrationEdge],
) -> Result<Landing, WorktreeError> {
    let exists = |oid: GitOid| -> Result<Option<Vec<GitOid>>, WorktreeError> { graph.parents(oid) };
    let missing = |oid| Ok(Landing::Unproven(Unproven::MissingCommit { oid }));
    if exists(root.tip)?.is_none() {
        return missing(root.tip);
    }
    if exists(tip)?.is_none() {
        return missing(tip);
    }
    if graph.contains(tip, root.tip)? {
        return Ok(Landing::Landed(LandedBy::Ancestry));
    }
    if chain.is_empty() {
        return Ok(Landing::Unproven(Unproven::NotContained));
    }
    let mut current = tip;
    for (index, edge) in chain.iter().enumerate() {
        // Target edge: the delivered head carries what came before it, and a
        // later receipt delivers the ref the previous one integrated into.
        if index > 0 && edge.source != chain[index - 1].target {
            return Ok(Landing::Unproven(Unproven::ChainBroken { index }));
        }
        if exists(edge.head)?.is_none() {
            return missing(edge.head);
        }
        if !graph.contains(current, edge.head)? {
            return Ok(Landing::Unproven(Unproven::ChainBroken { index }));
        }
        // Receipt edge: the integration commit has the recorded shape.
        let Some(parents) = exists(edge.integrated)? else {
            return missing(edge.integrated);
        };
        if let Some(previous) = edge.previous
            && exists(previous)?.is_none()
        {
            return missing(previous);
        }
        let shaped = edge.integrated == edge.head
            || edge
                .previous
                .is_some_and(|previous| parents == [previous, edge.head] || parents == [previous]);
        let descends = match edge.previous {
            Some(previous) => graph.contains(previous, edge.integrated)?,
            None => true,
        };
        if !shaped || !descends {
            return Ok(Landing::Unproven(Unproven::ReceiptMismatch { index }));
        }
        current = edge.integrated;
    }
    if root.reference.is_none() || chain.last().map(|edge| &edge.target) != root.reference.as_ref()
    {
        return Ok(Landing::Unproven(Unproven::UnauthorisedRoot));
    }
    Ok(if graph.contains(current, root.tip)? {
        Landing::Landed(LandedBy::ReceiptChain { edges: chain.len() })
    } else {
        Landing::Unproven(Unproven::NotContained)
    })
}

/// The repository's object graph through the bounded Git utility.
pub struct RepoGraph<'a, 'b> {
    pub context: &'a WorktreeContext<'b>,
    pub cancel: &'a CancelToken,
}

impl CommitGraph for RepoGraph<'_, '_> {
    fn parents(&self, commit: GitOid) -> Result<Option<Vec<GitOid>>, WorktreeError> {
        let outcome = run_git(
            self.context,
            self.context.repo,
            GitRead::RawObject {
                kind: crate::utility::GitObjectKind::Commit,
                oid: commit,
            },
            self.cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Ok(None);
        }
        commit_parents(&outcome.stdout.bytes).map(Some)
    }

    fn contains(&self, ancestor: GitOid, tip: GitOid) -> Result<bool, WorktreeError> {
        is_ancestor(self.context, ancestor, tip, self.cancel)
    }
}

/// Parent OIDs from a raw commit object's header.
pub fn commit_parents(raw: &[u8]) -> Result<Vec<GitOid>, WorktreeError> {
    let mut parents = Vec::new();
    for line in raw.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            return Ok(parents);
        }
        if let Some(parent) = line.strip_prefix(b"parent ") {
            parents.push(parse_oid(parent)?);
        }
    }
    Err(WorktreeError::InvalidOutput)
}

/// Ancestry of the target the caller just read, with no receipts: the check
/// lifecycle removal and GC share.
pub(super) fn landed_on(
    context: &WorktreeContext<'_>,
    tip: GitOid,
    root: &LandingTarget,
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    Ok(landed(&RepoGraph { context, cancel }, tip, root, &[])?.is_landed())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory commit graph: each commit maps to its parents.
    struct Graph(std::collections::HashMap<GitOid, Vec<GitOid>>);

    impl CommitGraph for Graph {
        fn parents(&self, commit: GitOid) -> Result<Option<Vec<GitOid>>, WorktreeError> {
            Ok(self.0.get(&commit).cloned())
        }

        fn contains(&self, ancestor: GitOid, tip: GitOid) -> Result<bool, WorktreeError> {
            let mut pending = vec![tip];
            let mut seen = std::collections::HashSet::new();
            while let Some(commit) = pending.pop() {
                if commit == ancestor {
                    return Ok(true);
                }
                if seen.insert(commit) {
                    pending.extend(self.0.get(&commit).into_iter().flatten().copied());
                }
            }
            Ok(false)
        }
    }

    fn oid(byte: u8) -> GitOid {
        GitOid::parse(&format!("{byte:02x}").repeat(20)).expect("fixture OID")
    }

    fn branch(name: &str) -> GitRefName {
        GitRefName::branch(name).expect("fixture branch")
    }

    // base(1) <- main(2); lane head(3) on base; batch merge(4) = [2, 3];
    // main lands batch as a single commit(5) on 2; unrelated(6) on 2 shares
    // nothing with the lane.
    fn graph() -> Graph {
        Graph(std::collections::HashMap::from([
            (oid(1), vec![]),
            (oid(2), vec![oid(1)]),
            (oid(3), vec![oid(1)]),
            (oid(4), vec![oid(2), oid(3)]),
            (oid(5), vec![oid(2)]),
            (oid(6), vec![oid(2)]),
        ]))
    }

    fn main_at(tip: GitOid) -> LandingTarget {
        LandingTarget {
            reference: Some(branch("main")),
            tip,
        }
    }

    fn lane_into_batch() -> IntegrationEdge {
        IntegrationEdge {
            source: branch("lane/x"),
            head: oid(3),
            target: branch("batch/y"),
            previous: Some(oid(2)),
            integrated: oid(4),
        }
    }

    fn batch_into_main() -> IntegrationEdge {
        IntegrationEdge {
            source: branch("batch/y"),
            head: oid(4),
            target: branch("main"),
            previous: Some(oid(2)),
            integrated: oid(5),
        }
    }

    #[test]
    fn ancestry_of_the_fetched_target_is_landed() {
        assert_eq!(
            landed(&graph(), oid(3), &main_at(oid(4)), &[]).unwrap(),
            Landing::Landed(LandedBy::Ancestry)
        );
    }

    #[test]
    fn a_target_without_the_commit_is_unproven() {
        assert_eq!(
            landed(&graph(), oid(3), &main_at(oid(6)), &[]).unwrap(),
            Landing::Unproven(Unproven::NotContained)
        );
    }

    #[test]
    fn a_revalidated_receipt_chain_to_the_root_is_landed() {
        let chain = [lane_into_batch(), batch_into_main()];
        assert_eq!(
            landed(&graph(), oid(3), &main_at(oid(5)), &chain).unwrap(),
            Landing::Landed(LandedBy::ReceiptChain { edges: 2 })
        );
    }

    #[test]
    fn a_force_pushed_root_is_unproven_despite_receipts() {
        // Main was rewound past the integration commit to an unrelated tip.
        let chain = [lane_into_batch(), batch_into_main()];
        assert_eq!(
            landed(&graph(), oid(3), &main_at(oid(6)), &chain).unwrap(),
            Landing::Unproven(Unproven::NotContained)
        );
    }

    #[test]
    fn a_missing_integration_commit_is_unproven() {
        let mut graph = graph();
        graph.0.remove(&oid(5));
        graph.0.insert(oid(7), vec![oid(2)]);
        let chain = [lane_into_batch(), batch_into_main()];
        assert_eq!(
            landed(&graph, oid(3), &main_at(oid(7)), &chain).unwrap(),
            Landing::Unproven(Unproven::MissingCommit { oid: oid(5) })
        );
    }

    #[test]
    fn a_receipt_whose_commit_has_another_shape_is_unproven() {
        // Commit 6 has the same parent as a single-commit landing, but the
        // receipt names it on a different previous tip.
        let mut edge = batch_into_main();
        edge.previous = Some(oid(1));
        edge.integrated = oid(6);
        let chain = [lane_into_batch(), edge];
        assert_eq!(
            landed(&graph(), oid(3), &main_at(oid(6)), &chain).unwrap(),
            Landing::Unproven(Unproven::ReceiptMismatch { index: 1 })
        );
    }

    #[test]
    fn a_chain_that_skips_a_ref_or_ends_elsewhere_is_unproven() {
        let mut skipped = batch_into_main();
        skipped.source = branch("batch/other");
        assert_eq!(
            landed(
                &graph(),
                oid(3),
                &main_at(oid(5)),
                &[lane_into_batch(), skipped]
            )
            .unwrap(),
            Landing::Unproven(Unproven::ChainBroken { index: 1 })
        );
        let release = LandingTarget {
            reference: Some(branch("release")),
            tip: oid(5),
        };
        assert_eq!(
            landed(
                &graph(),
                oid(3),
                &release,
                &[lane_into_batch(), batch_into_main()]
            )
            .unwrap(),
            Landing::Unproven(Unproven::UnauthorisedRoot)
        );
        let anonymous = LandingTarget {
            reference: None,
            tip: oid(5),
        };
        assert_eq!(
            landed(
                &graph(),
                oid(3),
                &anonymous,
                &[lane_into_batch(), batch_into_main()]
            )
            .unwrap(),
            Landing::Unproven(Unproven::UnauthorisedRoot)
        );
    }

    #[test]
    fn a_lane_tip_past_its_delivered_head_is_unproven() {
        let mut graph = graph();
        graph.0.insert(oid(8), vec![oid(3)]);
        assert_eq!(
            landed(
                &graph,
                oid(8),
                &main_at(oid(5)),
                &[lane_into_batch(), batch_into_main()]
            )
            .unwrap(),
            Landing::Unproven(Unproven::ChainBroken { index: 0 })
        );
    }

    #[test]
    fn parents_come_from_the_commit_header_only() {
        let raw = format!(
            "tree {}\nparent {}\nparent {}\nauthor a\n\nparent {}\n",
            oid(9),
            oid(1),
            oid(2),
            oid(3)
        );
        assert_eq!(
            commit_parents(raw.as_bytes()).unwrap(),
            vec![oid(1), oid(2)]
        );
        assert!(commit_parents(b"tree x").is_err());
    }
}
