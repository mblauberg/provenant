//! Distinct deletion (subsumed) and completion (landed) predicates.
use super::*;
use provenant_records::custody::{
    LandedEvidence, LandingPredicate, LandingSource, RemovalUncertainty,
};

#[derive(Clone, Debug)]
pub struct EvidenceRef {
    pub reference: GitRefName,
    pub commit: GitOid,
    pub integration: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemovalEvidence {
    Proven(LandedEvidence),
    NotSubsumed,
    Unknown(RemovalUncertainty),
}

/// Last fetched refs are valid retention evidence; no network availability is
/// inferred from their presence. The candidate's own ref must be excluded.
pub fn removal_evidence(
    context: &WorktreeContext<'_>,
    head: GitOid,
    references: &[EvidenceRef],
    cancel: &CancelToken,
) -> Result<RemovalEvidence, WorktreeError> {
    let graph = RepoGraph { context, cancel };
    if graph.parents(head)?.is_none() {
        return Ok(RemovalEvidence::Unknown(RemovalUncertainty::MissingCommit));
    }
    let mut unknown = None;
    // Prefer completion evidence to mere preservation on an arbitrary ref.
    for integration in [true, false] {
        for target in references.iter().filter(|r| r.integration == integration) {
            if graph.parents(target.commit)?.is_none() {
                unknown = Some(RemovalUncertainty::MissingCommit);
                continue;
            }
            let predicate = if integration {
                LandingPredicate::Landed
            } else {
                LandingPredicate::Subsumed
            };
            if graph.contains(head, target.commit)? {
                return Ok(RemovalEvidence::Proven(LandedEvidence {
                    predicate,
                    source: if integration {
                        LandingSource::IntegrationTarget {
                            reference: target.reference.as_str().into(),
                            commit: target.commit,
                        }
                    } else {
                        LandingSource::RetainedRef {
                            reference: target.reference.as_str().into(),
                            commit: target.commit,
                        }
                    },
                }));
            }
            let base = run_git(
                context,
                context.repo,
                GitRead::MergeBase {
                    left: target.commit,
                    right: head,
                },
                cancel,
            )?;
            if exit_code(&base)? != 0 {
                let shallow = run_git(context, context.repo, GitRead::IsShallow, cancel)?;
                if exit_code(&shallow)? != 0 {
                    return Err(WorktreeError::InvalidOutput);
                }
                match trim_ascii(&shallow.stdout.bytes) {
                    b"true" => unknown = Some(RemovalUncertainty::ShallowClone),
                    b"false" if exit_code(&base)? == 1 => {}
                    _ => return Err(WorktreeError::InvalidOutput),
                }
                continue;
            }
            let tree = run_git(
                context,
                context.repo,
                GitRead::ResolveCommitTree { oid: target.commit },
                cancel,
            )?;
            if exit_code(&tree)? != 0 {
                unknown = Some(RemovalUncertainty::MissingCommit);
                continue;
            }
            let merged = run_git(
                context,
                context.repo,
                GitRead::MergeTree {
                    target: target.commit,
                    head,
                },
                cancel,
            )?;
            match exit_code(&merged)? {
                0 => {
                    let merged_tree = merged
                        .stdout
                        .bytes
                        .split(|b| *b == b'\n')
                        .next()
                        .ok_or(WorktreeError::InvalidOutput)?;
                    let merged_tree = parse_oid(merged_tree)?;
                    if merged_tree == parse_oid(trim_ascii(&tree.stdout.bytes))? {
                        return Ok(RemovalEvidence::Proven(LandedEvidence {
                            predicate,
                            source: LandingSource::TreeEquivalent {
                                head,
                                target: target.commit,
                                tree: merged_tree,
                            },
                        }));
                    }
                }
                1 => unknown = Some(RemovalUncertainty::ConflictedMergeTree),
                _ => return Err(WorktreeError::InvalidOutput),
            }
        }
    }
    Ok(unknown.map_or(RemovalEvidence::NotSubsumed, RemovalEvidence::Unknown))
}

pub fn retained_refs(
    context: &WorktreeContext<'_>,
    candidate: Option<&GitRefName>,
    integration: Option<&LandingTarget>,
    patterns: &[String],
    cancel: &CancelToken,
) -> Result<Vec<EvidenceRef>, WorktreeError> {
    let mut refs = read_branch_tips(context, cancel)?;
    let remote = run_git(context, context.repo, GitRead::RemoteBranchTips, cancel)?;
    if exit_code(&remote)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    refs.extend(parse_branch_tips(&remote.stdout.bytes)?);
    Ok(refs
        .into_iter()
        .filter(|(reference, _)| {
            Some(reference) != candidate && !reference.as_str().starts_with("refs/provenant/")
        })
        .map(|(reference, commit)| {
            let selected = integration.is_some_and(|target| {
                target.reference.as_ref() == Some(&reference) && target.tip == commit
            }) || patterns
                .iter()
                .any(|pattern| matches_pattern(reference.as_str(), pattern));
            EvidenceRef {
                reference,
                commit,
                integration: selected,
            }
        })
        .collect())
}

pub fn matches_pattern(path: &str, pattern: &str) -> bool {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => path.starts_with(prefix) && path.ends_with(suffix),
        None => path == pattern,
    }
}
