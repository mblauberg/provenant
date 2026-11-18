//! One host-owned subject for review selection, evidence and provider launch.
use super::*;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PullSubject {
    base_ref_oid: provenant_core::GitOid,
    base_ref_name: String,
    head_ref_oid: provenant_core::GitOid,
    url: String,
}

pub(super) struct ReviewSubject {
    pub base: String,
    pub head: String,
    pub warning: Option<String>,
}

impl Factory {
    pub(super) fn review_subject(
        &self,
        input: &Value,
        review: bool,
    ) -> Result<Option<ReviewSubject>, FacadeError> {
        self.review_subject_with(input, review, |number| {
            let bytes = crate::pr::forge_execute(
                self.context.roots.state.as_path(),
                &self.cwd,
                &[
                    "gh",
                    "pr",
                    "view",
                    &number.to_string(),
                    "--json",
                    "baseRefOid,baseRefName,headRefOid,url",
                ],
                &provenant_config::ports::Request::within(std::time::Duration::from_secs(60)),
                false,
                None,
            )
            .and_then(crate::pr::successful)
            .map_err(|error| graph_subject_error(error.to_string()))?;
            serde_json::from_slice(&bytes)
                .map_err(|_| graph_subject_error("invalid PR subject metadata"))
        })
    }

    fn review_subject_with(
        &self,
        input: &Value,
        review: bool,
        pull: impl FnOnce(u64) -> Result<PullSubject, FacadeError>,
    ) -> Result<Option<ReviewSubject>, FacadeError> {
        let pr = input.get("pr").filter(|value| !value.is_null());
        if !review {
            return if pr.is_some() {
                Err(graph_subject_error("--pr requires a review template"))
            } else {
                Ok(None)
            };
        }
        let setting = |name: &str| -> Result<Option<&str>, FacadeError> {
            input
                .pointer(&format!("/set/{name}"))
                .map(|value| {
                    value
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| graph_subject_error(format!("invalid {name}")))
                })
                .transpose()
        };
        let subject = setting("subject")?;
        let base = setting("review_base")?;
        let git = provenant_os::git::git_executable()
            .ok_or_else(|| graph_subject_error("Git unavailable"))?;
        let resolve = |reference: &str| {
            provenant_run::resolve_git_base(&git, &self.cwd, reference).map_err(run_error)
        };
        let (base, head, warning) = if let Some(pr) = pr {
            if subject.is_some() || base.is_some() {
                return Err(graph_subject_error(
                    "select --pr or subject plus review_base",
                ));
            }
            let number = pr
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| graph_subject_error("invalid PR number"))?;
            // Validate the launch repository before admitting transport writes.
            resolve("HEAD")?;
            let pull = pull(number)?;
            let (repository, _) = pull
                .url
                .rsplit_once("/pull/")
                .ok_or_else(|| graph_subject_error("PR URL has no repository"))?;
            let base_ref = provenant_confinement::repo::GitRefName::branch(&pull.base_ref_name)
                .map_err(|_| graph_subject_error("invalid PR base branch"))?;
            let head_ref = format!("refs/pull/{number}/head");
            let materialise = |reference: &str, oid: provenant_core::GitOid| {
                provenant_run::resolve_review_commit(
                    &git,
                    &self.cwd,
                    self.context.roots.state.as_path(),
                    repository,
                    reference,
                    &oid.to_string(),
                )
                .map_err(run_error)
            };
            (
                materialise(base_ref.as_str(), pull.base_ref_oid)?,
                materialise(&head_ref, pull.head_ref_oid)?,
                None,
            )
        } else if let (Some(subject), Some(base)) = (subject, base) {
            (resolve(base)?, resolve(subject)?, None)
        } else if subject.is_some() || base.is_some() {
            return Err(graph_subject_error(
                "review requires both subject and review_base",
            ));
        } else {
            let head = resolve("HEAD")?;
            let merge = resolve("HEAD^2").is_ok();
            let (base, reason) = match provenant_run::review_merge_base(&git, &self.cwd, &head) {
                Ok(base) if base != head => (
                    base,
                    if merge {
                        "checkout merge commit's first parent"
                    } else {
                        "merge base of checkout HEAD and origin/HEAD"
                    },
                ),
                _ => match resolve("HEAD^") {
                    Ok(base) => (
                        base,
                        "checkout first parent; default-branch comparison is empty or unavailable",
                    ),
                    Err(_) => (
                        head.clone(),
                        "checkout has no parent or resolvable origin/HEAD; empty range",
                    ),
                },
            };
            let warning = format!(
                "No review subject was supplied; host chose {base}...{head} from {reason}. Pin --pr N or --set subject=HEAD --set review_base=BASE."
            );
            (base, head, Some(warning))
        };
        if base == head && warning.is_none() {
            return Err(graph_subject_error(
                "review_empty_range: base and subject resolve to the same commit; select a subject with changes",
            ));
        }
        Ok(Some(ReviewSubject {
            base,
            head,
            warning,
        }))
    }
}

fn graph_subject_error(reason: impl Into<String>) -> FacadeError {
    FacadeError::invalid(InvalidInputDetail::Field(reason.into()))
}
