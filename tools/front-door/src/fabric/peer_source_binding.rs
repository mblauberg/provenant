//! Bind the read-only source checkout a peer uses for its own graph nodes.
//!
//! A peer never inherits the sender's checkout (#3744): it resolves its own
//! source from the host state and boundary. Each refusing step names the
//! confinement rule and the path it refused, so a remote smoke failure is
//! attributable instead of collapsing into a bare `confinement_refused`.
use provenant_fabric_runtime::coordination::facade::FacadeError;
use provenant_records::{
    diagnostic::{ConfinementRule, ResourceFact},
    run::{LaneAccessRequest, NetworkAccess, SourceIdentity},
};
use std::path::Path;

pub(crate) fn refusal(rule: &'static str, path: &Path) -> FacadeError {
    FacadeError::ConfinementRefused {
        resource: Some(ResourceFact::ConfinementRule(Box::new(ConfinementRule {
            rule: rule.to_owned(),
            path: path.display().to_string(),
        }))),
    }
}

/// Resolve and fingerprint the receiver's own source checkout.
pub(crate) fn bind_reader_source(
    state: &Path,
    cwd: &Path,
    network: Option<NetworkAccess>,
) -> Result<LaneAccessRequest, FacadeError> {
    let source = provenant_run::resolve_reader_source(state, cwd, cwd)
        .map_err(|_| refusal("peer_source_resolve", cwd))?;
    let directory = provenant_os::fs::Directory::open_anchor(&source)
        .map_err(|_| refusal("peer_source_anchor", &source))?;
    let metadata = directory
        .metadata()
        .map_err(|_| refusal("peer_source_identity", &source))?;
    let access = LaneAccessRequest {
        network,
        source_root: Some(source),
        source_identity: Some(SourceIdentity {
            device: metadata.device,
            inode: metadata.inode,
        }),
        ..Default::default()
    };
    Ok(access)
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "the custom worktree driver omits libtest functions"
)]
mod tests {
    use super::*;

    #[test]
    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    fn reader_source_binding_names_the_rule_and_path_it_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let cwd = dir.path();
        // A directory with no Git project cannot yield a reader source, so the
        // real resolution path must refuse with the named rule and refused path.
        let error = bind_reader_source(cwd, cwd, None)
            .expect_err("a non-Git directory must not resolve a reader source");
        match error {
            FacadeError::ConfinementRefused {
                resource: Some(ResourceFact::ConfinementRule(rule)),
            } => {
                assert_eq!(rule.rule, "peer_source_resolve");
                assert_eq!(rule.path, cwd.display().to_string());
            }
            other => panic!("expected a named confinement refusal, got {other:?}"),
        }
        for rule in ["peer_source_anchor", "peer_source_identity"] {
            let named = refusal(rule, cwd);
            match named {
                FacadeError::ConfinementRefused {
                    resource: Some(ResourceFact::ConfinementRule(fact)),
                } => {
                    assert_eq!(fact.rule, rule);
                    assert_eq!(fact.path, cwd.display().to_string());
                }
                other => panic!("expected a named confinement refusal, got {other:?}"),
            }
        }
        Ok(())
    }
}
