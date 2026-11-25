//! One host-owned subject for review selection, evidence and provider launch.
use super::*;
use crate::pr::observer::Observer;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

/// Read one pull request's subject through the one CI observation transport.
/// No caller shells out to `gh` for this: the observer selects the forge port,
/// shares the cache and honours the same rate-limit hold-off as every waiter.
fn observed_pull(
    state: &std::path::Path,
    project: &std::path::Path,
    number: u64,
    request: &provenant_config::ports::Request,
) -> Result<PullSubject, FacadeError> {
    let repository = match crate::pr::observer::repository_for_project(state, project, request) {
        Ok(repository) => repository,
        Err(error) => return subject_read_result(state, project, number, error),
    };
    let mut observer = match Observer::open(state, project, &repository, request) {
        Ok(observer) => observer,
        Err(error) => return subject_read_result(state, project, number, error),
    };
    let resource = match observer.resource(&format!("pulls/{number}"), Duration::ZERO, request) {
        Ok(resource) => resource,
        Err(error) => return subject_read_result(state, project, number, error),
    };
    pull_subject(&resource)
}

/// A rate-limited forge read prefers the batched GraphQL path: it has its own
/// quota and can still answer when the REST core allowance is spent. When no
/// GraphQL row can serve the range, the refusal is typed and carries the reset
/// instead of naming an input field the caller never supplied (#3986).
fn subject_read_result(
    state: &std::path::Path,
    project: &std::path::Path,
    number: u64,
    error: crate::pr::observer::ObserveError,
) -> Result<PullSubject, FacadeError> {
    match error {
        crate::pr::observer::ObserveError::RateLimited { reset } => {
            match graphql_pull_subject(state, project, number) {
                Some(subject) => Ok(subject),
                None => Err(forge_quota_error(reset)),
            }
        }
        error => Err(graph_subject_error(error.to_string())),
    }
}

/// The batched GraphQL read answers a subject only with a complete, current
/// base and head. Cached rows written before those fields existed simply
/// cannot serve this read; the typed refusal then names when to retry.
fn graphql_pull_subject(
    state: &std::path::Path,
    project: &std::path::Path,
    number: u64,
) -> Option<PullSubject> {
    subject_from_batch(crate::pr::status::batch::single(state, project, number))
}

/// Mapping a complete batched row to a subject is transport-independent, so a
/// row can be tested without standing up a forge port. A row missing either
/// base field is from an older cache and cannot answer a subject read.
fn subject_from_batch(batch: crate::pr::status::batch::Batch) -> Option<PullSubject> {
    let crate::pr::status::batch::Batch::Entries(entries) = batch else {
        return None;
    };
    let entry = entries.into_iter().next()?;
    if entry.state == "UNAVAILABLE"
        || entry.base_ref_oid.is_empty()
        || entry.base_ref_name.is_empty()
        || entry
            .url
            .rsplit_once("/pull/")
            .is_none_or(|(repository, _)| repository.is_empty())
    {
        return None;
    }
    Some(PullSubject {
        base_ref_oid: provenant_core::GitOid::parse(&entry.base_ref_oid).ok()?,
        base_ref_name: entry.base_ref_name,
        head_ref_oid: provenant_core::GitOid::parse(&entry.head).ok()?,
        head_ref_name: None,
        url: entry.url,
    })
}

/// `RunKind` carries the run's stable `FORGE_QUOTA` code through `code()` and
/// the facade's typed outcome; the reset lives in the safe `next` repair.
fn forge_quota_error(reset: jiff::Timestamp) -> FacadeError {
    FacadeError::RunKind {
        run_code: "FORGE_QUOTA".into(),
        next: format!("retry after {reset}"),
        detail: Some(format!("GitHub rate limit; reset {reset}")),
        resource: None,
    }
}

/// The REST pull resource is the same shape the landing adapter observes.
fn pull_subject(value: &Value) -> Result<PullSubject, FacadeError> {
    let field = |pointer: &str| -> Result<&str, FacadeError> {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .ok_or_else(|| graph_subject_error(format!("pull resource has no {pointer}")))
    };
    let oid = |pointer: &str| -> Result<provenant_core::GitOid, FacadeError> {
        provenant_core::GitOid::parse(field(pointer)?)
            .map_err(|_| graph_subject_error(format!("invalid {pointer}")))
    };
    Ok(PullSubject {
        base_ref_oid: oid("/base/sha")?,
        base_ref_name: field("/base/ref")?.to_owned(),
        head_ref_oid: oid("/head/sha")?,
        head_ref_name: value["head"]["ref"].as_str().map(str::to_owned),
        url: field("/html_url")?.to_owned(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PullSubject {
    base_ref_oid: provenant_core::GitOid,
    base_ref_name: String,
    head_ref_oid: provenant_core::GitOid,
    // Older command-forged fixtures do not publish the branch name; without it
    // the local ahead-of-forge advisory is simply skipped.
    #[serde(default)]
    head_ref_name: Option<String>,
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
        cancel: &CancelToken,
    ) -> Result<Option<ReviewSubject>, FacadeError> {
        self.review_subject_with(input, review, cancel, |number| {
            let request =
                provenant_config::ports::Request::within(std::time::Duration::from_secs(60));
            observed_pull(
                self.context.roots.state.as_path(),
                &self.context.project_root,
                number,
                &request,
            )
        })
    }

    fn review_subject_with(
        &self,
        input: &Value,
        review: bool,
        cancel: &CancelToken,
        pull: impl Fn(u64) -> Result<PullSubject, FacadeError>,
    ) -> Result<Option<ReviewSubject>, FacadeError> {
        let retry = crate::pr::observer::retry(&self.context.roots);
        self.review_subject_with_wait(
            input,
            review,
            cancel,
            pull,
            retry.head_attempts.value,
            retry.head_delay.value,
        )
    }

    /// The injectable pull closure is the seam these invariants pin; the wait
    /// parameters are bounded so production re-reads the forge but tests need
    /// not sleep.
    fn review_subject_with_wait(
        &self,
        input: &Value,
        review: bool,
        cancel: &CancelToken,
        pull: impl Fn(u64) -> Result<PullSubject, FacadeError>,
        reread_attempts: usize,
        reread_delay: Duration,
    ) -> Result<Option<ReviewSubject>, FacadeError> {
        let pr = input.get("pr").filter(|value| !value.is_null());
        let mut expected_head = input
            .get("head")
            .filter(|value| !value.is_null())
            .map(|value| {
                let text = value
                    .as_str()
                    .ok_or_else(|| graph_subject_error("--head expects a commit SHA"))?;
                provenant_core::GitOid::parse(text)
                    .map_err(|_| graph_subject_error(format!("invalid --head SHA: {text}")))
            })
            .transpose()?;
        // Issue #3551: --pr against a template with no review range is advisory.
        // Warn with its source and proceed with the host's default review shape
        // instead of refusing the whole submission. The forge pin is ignored for
        // that shape because the template cannot consume a review range.
        let ignored_pr = !review && pr.is_some();
        if !review && !ignored_pr {
            return Ok(None);
        }
        let pr = if ignored_pr { None } else { pr };
        if ignored_pr {
            expected_head = None;
        }
        let ignored_warning = ignored_pr.then(|| {
            "--pr was ignored: the selected template does not accept a review range; using the default review subject (source: template review range)".to_owned()
        });
        if expected_head.is_some() && pr.is_none() {
            return Err(graph_subject_error("--head requires --pr"));
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
            // Issue #3878: a repair range may narrow a pinned PR review with
            // `--set review_base=SHA`. The head and PR binding still come from
            // the forge; only the base is taken from the explicit pin. Supplying
            // `subject` would replace the PR head and break that binding, so it
            // stays exclusive with `--pr`.
            if subject.is_some() {
                return Err(graph_subject_error(
                    "select --pr (optionally with review_base) or subject plus review_base",
                ));
            }
            let number = pr
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| graph_subject_error("invalid PR number"))?;
            // Validate the launch repository before admitting transport writes.
            resolve("HEAD")?;
            let mut pull_subject = pull(number)?;
            if let Some(expected) = expected_head {
                let mut rereads = 0;
                while pull_subject.head_ref_oid != expected && rereads < reread_attempts {
                    if !reread_delay.is_zero() {
                        provenant_core::Pause::wait(
                            &self.backoff,
                            provenant_core::DurationMicros::from_micros(
                                reread_delay.as_micros().min(u64::MAX as u128) as u64,
                            ),
                            cancel,
                        );
                    }
                    pull_subject = pull(number)?;
                    rereads += 1;
                }
                if pull_subject.head_ref_oid != expected {
                    // Issue #3976: name the field and the cause instead of
                    // hiding both ids inside a generic "invalid input field"
                    // detail with a "check the named input field" repair.
                    return Err(FacadeError::invalid(InvalidInputDetail::StaleHead {
                        supplied: expected.to_string(),
                        current: pull_subject.head_ref_oid.to_string(),
                    }));
                }
            }
            let (repository, _) = pull_subject
                .url
                .rsplit_once("/pull/")
                .ok_or_else(|| graph_subject_error("PR URL has no repository"))?;
            let base_ref =
                provenant_confinement::repo::GitRefName::branch(&pull_subject.base_ref_name)
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
            let base = match base {
                // The explicit pin resolves locally; the PR head still comes
                // from the forge, so the diff is review_base...PR head.
                Some(base) => resolve(base)?,
                None => materialise(base_ref.as_str(), pull_subject.base_ref_oid)?,
            };
            let head = materialise(&head_ref, pull_subject.head_ref_oid)?;
            // Without an expected head, a fresh push can leave the local
            // remote-tracking ref ahead of what the forge still reports. Warn
            // (never refuse) and name both commits so the reviewer can re-run
            // with --head to pin the intended one.
            let warning = expected_head
                .is_none()
                .then(|| {
                    stale_head_warning(
                        &git,
                        &self.cwd,
                        pull_subject.head_ref_name.as_deref(),
                        pull_subject.head_ref_oid,
                    )
                })
                .flatten();
            (base, head, warning)
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
        let warning = match (ignored_warning, warning) {
            (Some(ignored), Some(default)) => Some(format!("{ignored} {default}")),
            (one, other) => one.or(other),
        };
        Ok(Some(ReviewSubject {
            base,
            head,
            warning,
        }))
    }
}

/// Read a local ref with Git, returning trimmed stdout only on a clean exit.
/// All arguments are passed directly (never through a shell) and the branch
/// name is validated before it reaches a ref path.
fn local_git(git: &Path, cwd: &Path, args: &[&str]) -> Option<String> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let argv: Vec<_> = std::iter::once(git.as_os_str().to_owned())
        .chain(args.iter().map(|arg| std::ffi::OsString::from(*arg)))
        .collect();
    let executor = SystemExecutor::new(1).ok()?;
    let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));
    options.stdout = StreamPolicy::Capture { limit: 4096 };
    options.stderr = StreamPolicy::Discard;
    let outcome = executor.execute(options).ok()?;
    if outcome.timed_out || outcome.termination != Termination::Exit(0) {
        return None;
    }
    Some(
        String::from_utf8_lossy(&outcome.stdout.bytes)
            .trim()
            .to_owned(),
    )
}

/// Read one Git config value through the core's typed command builder, so the
/// operation, file and static key are named and the argv cannot be assembled
/// here. An absent key keeps the existing advisory default; no raw config argv
/// escapes into this helper.
fn local_config_get(git: &Path, cwd: &Path, key: &str) -> Option<String> {
    let context = provenant_os::git_config::ConfigContext::new(
        "read branch remote for stale-head warning",
        cwd.join(".git/config"),
        Some("branch remote"),
    );
    provenant_os::git_config::read(
        context,
        provenant_os::git_config::Read::Get(key),
        |command| {
            let args: Vec<&str> = command
                .iter()
                .map(|arg| arg.to_str().unwrap_or_default())
                .collect();
            Ok::<_, std::convert::Infallible>(local_git(git, cwd, &args))
        },
    )
    .ok()
    .flatten()
}

/// When the PR head branch's local remote-tracking tip has commits the forge
/// head lacks, name both SHAs: a push may not have propagated to the forge yet.
fn stale_head_warning(
    git: &Path,
    cwd: &Path,
    head_ref_name: Option<&str>,
    forge_head: provenant_core::GitOid,
) -> Option<String> {
    let branch = head_ref_name?;
    provenant_confinement::repo::GitRefName::branch(branch).ok()?;
    let configured =
        local_config_get(git, cwd, &format!("branch.{branch}.remote")).filter(|name| {
            !name.is_empty() && provenant_confinement::repo::GitRefName::branch(name).is_ok()
        });
    let remote = configured.unwrap_or_else(|| "origin".to_owned());
    let reference = format!("refs/remotes/{remote}/{branch}");
    let tip = local_git(
        git,
        cwd,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )?;
    let tip_oid = provenant_core::GitOid::parse(&tip).ok()?;
    if tip_oid == forge_head {
        return None;
    }
    let ahead = local_git(
        git,
        cwd,
        &["rev-list", "--count", &format!("{forge_head}..{tip_oid}")],
    )?;
    if ahead.parse::<u64>().ok()? == 0 {
        return None;
    }
    Some(format!(
        "PR head branch {branch} has local remote-tracking tip {tip_oid} ahead of forge head {forge_head}; the forge may still be stale after a push. Pin the expected head with --head SHA to refuse a mismatch."
    ))
}

fn graph_subject_error(reason: impl Into<String>) -> FacadeError {
    FacadeError::invalid(InvalidInputDetail::Field(reason.into()))
}

#[cfg(test)]
mod tests {
    // One entry point keeps every helper and import inside a `#[test]` body:
    // the harness-less worktree driver omits libtest functions, so module-level
    // items would be reported as unused there.
    #[test]
    fn review_subject_stale_head_invariants() -> crate::worktree_tests::support::Result {
        use super::*;
        use crate::worktree_tests::support::{Result, TestRig, git};

        struct Workers;
        impl WorkerObserver for Workers {
            fn observe_roots(
                &self,
                _: &[PathBuf],
            ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
            {
                std::collections::BTreeMap::new()
            }
        }

        fn bind_fixture_project(roots: &provenant_core::Roots, path: &Path) -> Result {
            let project = std::fs::canonicalize(path)?;
            let common_dir = std::fs::canonicalize(project.join(".git"))?;
            let location = provenant_config::ProjectLocation::from_verified_primary_checkout(
                common_dir, &project,
            )?;
            provenant_config::bind_project(roots, location)?;
            Ok(())
        }

        fn factory(rig: &TestRig) -> Result<Factory> {
            let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
            bind_fixture_project(&roots, &rig.project)?;
            Ok(Factory {
                context: Context::open(roots, &rig.home, &rig.project)?,
                project_attribution: false,
                clock: SystemClock,
                backoff: ThreadPause,
                executor: SystemExecutor::new(1)?,
                home: rig.home.clone(),
                cwd: rig.project.clone(),
                assets: rig.project.clone(),
                host_launch: None,
                workers: &Workers,
            })
        }

        struct Fixture {
            rig: TestRig,
            base: String,
            forge_old: String,
            forge_new: String,
        }

        /// Real commits so every synthetic PR OID resolves locally without a fetch.
        fn fixture() -> Result<Fixture> {
            let rig = crate::worktree_tests::repository()?;
            let base = git(&rig, &["rev-parse", "HEAD"])?;
            git(&rig, &["checkout", "-q", "-b", "feature"])?;
            std::fs::write(rig.project.join("old.txt"), "old\n")?;
            git(&rig, &["add", "."])?;
            git(&rig, &["commit", "-qm", "old head"])?;
            let forge_old = git(&rig, &["rev-parse", "HEAD"])?;
            std::fs::write(rig.project.join("new.txt"), "new\n")?;
            git(&rig, &["add", "."])?;
            git(&rig, &["commit", "-qm", "new head"])?;
            let forge_new = git(&rig, &["rev-parse", "HEAD"])?;
            Ok(Fixture {
                rig,
                base,
                forge_old,
                forge_new,
            })
        }

        fn subject(fx: &Fixture, head: &str, name: Option<&str>) -> PullSubject {
            PullSubject {
                base_ref_oid: provenant_core::GitOid::parse(&fx.base).unwrap(),
                base_ref_name: "main".into(),
                head_ref_oid: provenant_core::GitOid::parse(head).unwrap(),
                head_ref_name: name.map(str::to_owned),
                url: "https://github.com/owner/repo/pull/7".into(),
            }
        }

        fn run_with(
            factory: &Factory,
            input: &serde_json::Value,
            pull: impl Fn(u64) -> std::result::Result<PullSubject, FacadeError>,
        ) -> std::result::Result<Option<ReviewSubject>, FacadeError> {
            factory.review_subject_with_wait(
                input,
                true,
                &CancelToken::new(),
                pull,
                3,
                Duration::ZERO,
            )
        }

        // A matching expected head pins the forge head and base without a warning.
        {
            let fx = fixture()?;
            let factory = factory(&fx.rig)?;
            let input = serde_json::json!({"pr":7,"head":fx.forge_new});
            let result = run_with(&factory, &input, |_| Ok(subject(&fx, &fx.forge_new, None)))?
                .ok_or("no subject")?;
            assert_eq!(result.head, fx.forge_new);
            assert_eq!(result.base, fx.base);
            assert!(result.warning.is_none());
        }

        // Issue #3878: --pr with review_base keeps the PR binding and narrows
        // the diff to review_base...PR head, re-checking just the repair commits.
        {
            let fx = fixture()?;
            let factory = factory(&fx.rig)?;
            let input = serde_json::json!({
                "pr":7, "head":fx.forge_new, "set":{"review_base":fx.forge_old}
            });
            let result = run_with(&factory, &input, |_| Ok(subject(&fx, &fx.forge_new, None)))?
                .ok_or("no subject")?;
            assert_eq!(result.head, fx.forge_new, "PR head must stay bound");
            assert_eq!(result.base, fx.forge_old, "range must start at review_base");
            assert!(result.warning.is_none());
        }

        // A forge that catches up is re-read and accepted within the bounded wait.
        {
            let fx = fixture()?;
            let factory = factory(&fx.rig)?;
            let calls = std::cell::Cell::new(0);
            let input = serde_json::json!({"pr":7,"head":fx.forge_new});
            let result = run_with(&factory, &input, |_| {
                calls.set(calls.get() + 1);
                let head = if calls.get() < 3 {
                    &fx.forge_old
                } else {
                    &fx.forge_new
                };
                Ok(subject(&fx, head, None))
            })?
            .ok_or("no subject")?;
            assert_eq!(calls.get(), 3);
            assert_eq!(result.head, fx.forge_new);
        }

        // A persistent mismatch refuses and names both SHAs.
        {
            let fx = fixture()?;
            let factory = factory(&fx.rig)?;
            let calls = std::cell::Cell::new(0);
            let input = serde_json::json!({"pr":7,"head":fx.forge_new});
            let error = run_with(&factory, &input, |_| {
                calls.set(calls.get() + 1);
                Ok(subject(&fx, &fx.forge_old, None))
            })
            .err()
            .ok_or("stale head must refuse")?;
            assert_eq!(calls.get(), 4);
            let diagnostic = error.diagnostic();
            let detail = diagnostic.detail.as_deref().unwrap_or_default();
            assert!(detail.contains("--head"), "{detail}");
            assert!(detail.contains("not the pull request's head"), "{detail}");
            assert!(detail.contains(&fx.forge_new), "{detail}");
            assert!(detail.contains(&fx.forge_old), "{detail}");
            let next = diagnostic.next.as_deref().unwrap_or_default();
            assert!(!next.contains("check the named input field"), "{next}");
            assert!(next.contains(&fx.forge_new), "{next}");
        }

        // --head without --pr is a usage refusal.
        {
            let fx = fixture()?;
            let factory = factory(&fx.rig)?;
            let input = serde_json::json!({"head":fx.forge_new});
            let error = run_with(&factory, &input, |_| Ok(subject(&fx, &fx.forge_new, None)))
                .err()
                .ok_or("--head without --pr must refuse")?;
            let text = format!("{error:?}");
            assert!(text.contains("--head requires --pr"), "{text}");
        }

        // Without --head, a local remote-tracking tip ahead of the forge warns.
        {
            let fx = fixture()?;
            git(
                &fx.rig,
                &["update-ref", "refs/remotes/origin/feature", &fx.forge_new],
            )?;
            let factory = factory(&fx.rig)?;
            let input = serde_json::json!({"pr":7});
            let result = run_with(&factory, &input, |_| {
                Ok(subject(&fx, &fx.forge_old, Some("feature")))
            })?
            .ok_or("no subject")?;
            assert_eq!(result.head, fx.forge_old);
            let warning = result.warning.as_deref().ok_or("expected warning")?;
            assert!(warning.contains(&fx.forge_old), "{warning}");
            assert!(warning.contains(&fx.forge_new), "{warning}");
        }

        // A matching local tip, or no remote-tracking ref at all, is silent.
        for (tip, name) in [(None, "feature"), (Some("forge_old"), "feature")] {
            let fx = fixture()?;
            if let Some(which) = tip {
                let oid = if which == "forge_old" {
                    &fx.forge_old
                } else {
                    &fx.forge_new
                };
                git(&fx.rig, &["update-ref", "refs/remotes/origin/feature", oid])?;
            }
            let factory = factory(&fx.rig)?;
            let input = serde_json::json!({"pr":7});
            let result = run_with(&factory, &input, |_| {
                Ok(subject(&fx, &fx.forge_old, Some(name)))
            })?
            .ok_or("no subject")?;
            assert!(
                result.warning.is_none(),
                "unexpected warning: {:?}",
                result.warning
            );
        }

        // --pr without a review template warns and proceeds with the host's
        // default review shape rather than refusing the submission (issue #3551).
        {
            let fx = fixture()?;
            let factory = factory(&fx.rig)?;
            let input = serde_json::json!({"pr":7});
            let result = factory
                .review_subject_with_wait(
                    &input,
                    false,
                    &CancelToken::new(),
                    |_| Ok(subject(&fx, &fx.forge_old, None)),
                    3,
                    Duration::ZERO,
                )?
                .ok_or("no default review subject")?;
            let warning = result.warning.as_deref().ok_or("expected warning")?;
            assert!(warning.contains("--pr was ignored"), "{warning}");
            assert!(warning.contains("review range"), "{warning}");
        }

        Ok(())
    }

    /// Issue #3986: a spent REST core quota is a typed, retryable refusal, not
    /// an invalid input field the caller never supplied. With no GraphQL row
    /// available the reset is carried in the safe repair. The forge port is
    /// disabled here so the fallback cannot answer and no real forge is read.
    #[test]
    fn rate_limited_subject_is_a_typed_retryable_refusal() -> crate::worktree_tests::support::Result
    {
        use super::*;

        let state = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let mut ports = provenant_config::PortTable::builtin();
        ports.forge.backend = provenant_config::PortBackend::None;
        std::fs::write(state.path().join("ports.json"), serde_json::to_vec(&ports)?)?;

        let reset = jiff::Timestamp::from_second(2_000_000_000).expect("reset");
        let error = subject_read_result(
            state.path(),
            project.path(),
            7,
            crate::pr::observer::ObserveError::RateLimited { reset },
        )
        .err()
        .ok_or("a rate-limited subject must refuse")?;
        assert_eq!(error.code(), "FORGE_QUOTA", "{error:?}");
        let diagnostic = error.diagnostic();
        let next = diagnostic.next.as_deref().unwrap_or_default();
        assert!(next.contains("retry after"), "{next}");
        assert!(next.contains(&reset.to_string()), "{next}");
        let detail = diagnostic.detail.as_deref().unwrap_or_default();
        assert!(detail.contains("rate limit"), "{detail}");
        assert_eq!(
            provenant_cli::fabric::facade_outcome(&error),
            (
                provenant_records::Outcome::Refused,
                provenant_run::ErrorKind::ForgeQuota
            )
        );
        Ok(())
    }

    /// Issue #3986: where the batched GraphQL read can serve the range, it
    /// answers the subject instead of refusing at all. A complete row carries
    /// base and head; an older row without a base cannot serve the read.
    #[test]
    fn graphql_row_serves_a_rate_limited_subject() -> crate::worktree_tests::support::Result {
        use super::*;
        use crate::pr::status::batch::{Batch, Entry};

        let oid = provenant_core::GitOid::parse("1111111111111111111111111111111111111111")
            .expect("fixture oid");
        let entry = Entry {
            number: 7,
            state: "OPEN".into(),
            head: oid.to_string(),
            draft: false,
            merge: "CLEAN".into(),
            review: None,
            url: "https://github.com/owner/repo/pull/7".into(),
            base_ref_oid: oid.to_string(),
            base_ref_name: "main".into(),
            checks: Vec::new(),
            truncated: true,
        };
        // The rollup may be incomplete while the subject fields are complete.
        // That must not force a rate-limited review launch back to REST.
        let subject = subject_from_batch(Batch::Entries(vec![entry.clone()]))
            .ok_or("complete subject fields must serve despite a truncated check rollup")?;
        assert_eq!(subject.base_ref_oid, oid);
        assert_eq!(subject.head_ref_oid, oid);
        assert_eq!(subject.base_ref_name, "main");
        let mut older = entry;
        older.base_ref_oid.clear();
        assert!(subject_from_batch(Batch::Entries(vec![older])).is_none());
        Ok(())
    }
}
