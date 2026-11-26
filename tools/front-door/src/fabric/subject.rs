//! The one host-owned subject an operation acts on, resolved once at admission.
//!
//! A review, a read-only graph and a writer each resolve their subject here;
//! nothing downstream re-reads a ref or re-resolves a source for this graph.
use super::interface::{read_source_choice, writer_base_choice};
use super::*;
use crate::pr::observer::Observer;
use provenant_cli::run::{
    ReadSourceChoice, ReadSourcePin, resolve_default_branch, resolve_read_source,
};
use provenant_fabric_runtime::graph::{AccessMode, GraphNode, NodeKind, SourceBinding};
use provenant_records::Layer;
use provenant_records::subject::{PinnedSubject, SubjectOrigin, SubjectPolicy, ref_origin};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

const IGNORED_PR_WARNING: &str = "--pr was ignored: the selected template does not accept a review range; using the default review subject (source: template review range)";

/// How a read-only lane sees its tree. `Pinned` binds an exact commit; `Live`
/// keeps the checkout's working tree, which is only valid on the local host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadPlan {
    Pinned,
    Live,
}

/// The one subject, base and read policy resolved for a graph at admission.
pub(super) struct SubjectAdmission {
    pub subject: Option<PinnedSubject>,
    pub writer_base: Option<provenant_core::GitOid>,
    pub read: ReadPlan,
    pub warnings: Vec<String>,
    /// The review advisory that must also be bound into review node prompts.
    pub review_warning: Option<String>,
    /// The sourced read-source and writer-base settings admission applied,
    /// and the read pin it resolved, so reports never resolve them again.
    pub read_source: ReadSourceChoice,
    pub writer_base_setting: WriterBaseChoice,
    pub read_pin: Option<ReadSourcePin>,
    /// Advisories about the writer base; the caller reports them only for a
    /// launch that writes.
    pub writer_warnings: Vec<String>,
}

/// Read one pull request's subject through the one CI observation transport.
/// No caller shells out to `gh` for this: the observer selects the forge port,
/// shares the cache and honours the same rate-limit hold-off as every waiter.
fn observed_pull(
    state: &std::path::Path,
    project: &std::path::Path,
    number: u64,
    request: &provenant_config::ports::Request,
) -> Result<PullSnapshot, FacadeError> {
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
) -> Result<PullSnapshot, FacadeError> {
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
/// base and head. A cached row that cannot serve this read is refreshed before
/// the typed refusal names when to retry.
fn graphql_pull_subject(
    state: &std::path::Path,
    project: &std::path::Path,
    number: u64,
) -> Option<PullSnapshot> {
    subject_from_batch(crate::pr::status::batch::single_with_cache_filter(
        state,
        project,
        number,
        |entry| subject_from_entry(entry).is_some(),
    ))
}

/// Map a complete batched row to a subject, independently of check rollup completeness.
fn subject_from_batch(batch: crate::pr::status::batch::Batch) -> Option<PullSnapshot> {
    let crate::pr::status::batch::Batch::Entries(entries) = batch else {
        return None;
    };
    subject_from_entry(entries.first()?)
}

fn subject_from_entry(entry: &crate::pr::status::batch::Entry) -> Option<PullSnapshot> {
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
    Some(PullSnapshot {
        base_ref_oid: provenant_core::GitOid::parse(&entry.base_ref_oid).ok()?,
        base_ref_name: entry.base_ref_name.clone(),
        head_ref_oid: provenant_core::GitOid::parse(&entry.head).ok()?,
        head_ref_name: None,
        url: entry.url.clone(),
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
fn pull_subject(value: &Value) -> Result<PullSnapshot, FacadeError> {
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
    Ok(PullSnapshot {
        base_ref_oid: oid("/base/sha")?,
        base_ref_name: field("/base/ref")?.to_owned(),
        head_ref_oid: oid("/head/sha")?,
        head_ref_name: value["head"]["ref"].as_str().map(str::to_owned),
        url: field("/html_url")?.to_owned(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PullSnapshot {
    base_ref_oid: provenant_core::GitOid,
    base_ref_name: String,
    head_ref_oid: provenant_core::GitOid,
    // Older command-forged fixtures do not publish the branch name; without it
    // the local ahead-of-forge advisory is simply skipped.
    #[serde(default)]
    head_ref_name: Option<String>,
    url: String,
}

impl Factory {
    pub(super) fn review_subject(
        &self,
        input: &Value,
        review: bool,
        cancel: &CancelToken,
    ) -> Result<Option<(PinnedSubject, Option<String>)>, FacadeError> {
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
        pull: impl Fn(u64) -> Result<PullSnapshot, FacadeError>,
    ) -> Result<Option<(PinnedSubject, Option<String>)>, FacadeError> {
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
        pull: impl Fn(u64) -> Result<PullSnapshot, FacadeError>,
        reread_attempts: usize,
        reread_delay: Duration,
    ) -> Result<Option<(PinnedSubject, Option<String>)>, FacadeError> {
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
        let ignored_warning = ignored_pr.then(|| IGNORED_PR_WARNING.to_owned());
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
        // Capability provenance for the review: the credential-free forge id
        // and the ref the comparison base came from, when the route had them.
        let mut forge_repository: Option<String> = None;
        let mut subject_base_ref: Option<String> = None;
        let git = provenant_os::git::git_executable()
            .ok_or_else(|| graph_subject_error("Git unavailable"))?;
        let resolve = |reference: &str| {
            provenant_run::resolve_git_base(&git, &self.cwd, reference).map_err(run_error)
        };
        let (base, head, warning, origin) = if let Some(pr) = pr {
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
            let repository_id = forge_repository_id(repository)
                .ok_or_else(|| graph_subject_error("PR URL has no owner/name repository"))?;
            forge_repository = Some(repository_id.clone());
            subject_base_ref = Some(pull_subject.base_ref_name.clone());
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
                        self.context.roots.state.as_path(),
                        &self.cwd,
                        pull_subject.head_ref_name.as_deref(),
                        pull_subject.head_ref_oid,
                    )
                })
                .flatten();
            let origin = if expected_head.is_some() {
                // An explicit --head is the authoritative pin, even with --pr.
                SubjectOrigin::Explicit
            } else {
                SubjectOrigin::ForgePr {
                    repository: repository_id,
                    number,
                }
            };
            (base, head, warning, origin)
        } else if let (Some(subject), Some(base)) = (subject, base) {
            subject_base_ref = Some(base.to_owned());
            (resolve(base)?, resolve(subject)?, None, ref_origin(subject))
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
                        "checkout HEAD's parent; default-branch comparison is empty or unavailable",
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
            let origin = local_git(&git, &self.cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
                .filter(|name| name != "HEAD")
                .unwrap_or_else(|| "HEAD".to_owned());
            (
                base,
                head,
                Some(warning),
                SubjectOrigin::LocalRef { name: origin },
            )
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
        let base = provenant_core::GitOid::parse(&base)
            .map_err(|_| graph_subject_error("invalid review base"))?;
        let head = provenant_core::GitOid::parse(&head)
            .map_err(|_| graph_subject_error("invalid review head"))?;
        let pinned_at = self
            .clock
            .wall_now()
            .map_err(|error| FacadeError::StateCorruptDetail {
                detail: format!("subject admission clock: {error}"),
            })?;
        let base = if base == head && warning.is_some() {
            None
        } else {
            Some(base)
        };
        Ok(Some((
            PinnedSubject {
                extra: Default::default(),
                base,
                head,
                origin,
                pinned_at,
                policy: SubjectPolicy::Pinned,
                repository: forge_repository,
                base_ref: subject_base_ref,
                policy_source: Some(review_subject_source()),
                followed_from: Vec::new(),
            },
            warning,
        )))
    }
}

/// The policy source every review subject records. A review subject without a
/// base is the empty-range fallback; a read subject never has a base, so this
/// source is what tells the two apart when a task's base is chosen.
pub(crate) const REVIEW_SUBJECT_SOURCE: &str = "graph admission (always pinned)";

/// The typed source of a review subject: admission always pins it.
pub(crate) fn review_subject_source() -> provenant_records::Source {
    provenant_records::Source::at(provenant_records::Layer::Default, REVIEW_SUBJECT_SOURCE)
}

/// Whether an admitted subject is the review fallback with an empty range.
pub(crate) fn is_empty_review_range(subject: &PinnedSubject) -> bool {
    subject.base.is_none() && subject.policy_source == Some(review_subject_source())
}

impl Factory {
    /// Resolve the operation's one subject. Admission calls this once; nothing
    /// inside the admitted graph re-reads a ref or re-resolves a source.
    pub(super) fn admit_subject(
        &self,
        input: &Value,
        writer_base_input: Option<&Value>,
        review: bool,
        remote: bool,
        cancel: &CancelToken,
    ) -> Result<SubjectAdmission, FacadeError> {
        let pinned_at = self
            .clock
            .wall_now()
            .map_err(|error| FacadeError::StateCorruptDetail {
                detail: format!("subject observation clock: {error}"),
            })?;
        let read_config = provenant_cli::config_reads::read_for(&self.context.roots, "run").ok();
        let read_source = read_config
            .as_ref()
            .map(|config| {
                read_source_choice(
                    input,
                    config,
                    self.context.roots.state.as_path(),
                    &self.context.project_root,
                )
            })
            .unwrap_or_default();
        let writer_base_setting = read_config
            .as_ref()
            .map(|config| {
                writer_base_choice(
                    input,
                    config,
                    self.context.roots.state.as_path(),
                    &self.context.project_root,
                )
            })
            .unwrap_or_default();
        let git = provenant_os::git::git_executable();
        let mut warnings = Vec::new();
        if read_config.is_none() {
            warnings.push(
                "could not read the run configuration; lanes.read_source keeps its compiled default"
                    .to_owned(),
            );
        }
        // A writer's base is resolved by role, for every graph shape including
        // a review that also writes: explicit base, branch default, named
        // worktree HEAD, then no base at all. A missing base is advisory.
        let writer_base = resolve_writer_base(
            writer_base_input.unwrap_or(input),
            &self.context.project_root,
            &self.cwd,
            self.context.roots.state.as_path(),
            &writer_base_setting,
            &read_source,
            git.as_deref(),
        );
        let writer_warnings: Vec<String> = writer_base
            .warning
            .clone()
            .into_iter()
            .chain(writer_base.fallback_warning(&self.cwd))
            .collect();
        let writer_base = writer_base
            .commit
            .as_deref()
            .and_then(|commit| provenant_core::GitOid::parse(commit).ok());
        if review {
            let (subject, review_warning) = self
                .review_subject(input, true, cancel)?
                .ok_or_else(|| graph_subject_error("no review subject"))?;
            if let Some(warning) = &review_warning {
                warnings.push(warning.clone());
            }
            return Ok(SubjectAdmission {
                subject: Some(subject),
                writer_base,
                read: ReadPlan::Pinned,
                warnings,
                review_warning,
                read_source,
                writer_base_setting,
                read_pin: None,
                writer_warnings,
            });
        }
        if input.get("pr").is_some_and(|value| !value.is_null()) {
            warnings.push(IGNORED_PR_WARNING.to_owned());
        }
        // Every other graph also resolves its one read source here: whether
        // the launch writes is known only once its template resolves, so the
        // caller keeps the writer base for a writer and this subject otherwise. A pinned source
        // is a commit; the live choice is HEAD with a follow-latest policy,
        // live locally and pinned to that same HEAD on a peer.
        let selected =
            resolve_read_source(self.context.roots.state.as_path(), &self.cwd, &read_source);
        if let Some(selected) = selected {
            return Ok(SubjectAdmission {
                subject: Some(PinnedSubject {
                    extra: Default::default(),
                    base: None,
                    head: selected.commit,
                    origin: fixture_origin(&self.context.project_root)
                        .unwrap_or_else(|| ref_origin(&selected.reference)),
                    pinned_at,
                    policy: SubjectPolicy::Pinned,
                    repository: None,
                    base_ref: None,
                    policy_source: Some(read_source.provenance()),
                    followed_from: Vec::new(),
                }),
                writer_base,
                read: ReadPlan::Pinned,
                warnings,
                review_warning: None,
                read_source,
                writer_base_setting,
                read_pin: Some(selected),
                writer_warnings,
            });
        }
        // A checkout with no commit yet (or no Git at all) has nothing to pin:
        // the graph admits with no subject and its readers see the live tree,
        // exactly as before admission pinned anything. This is advisory.
        let head = git.as_deref().and_then(|git| {
            provenant_run::resolve_git_base(git, &self.cwd, "HEAD")
                .ok()
                .and_then(|head| head.parse::<provenant_core::GitOid>().ok())
                .map(|head| (git, head))
        });
        let Some((git, head)) = head else {
            warnings.push(format!(
                "{} has no commit to pin; read-only lanes read the live checkout and the graph records no subject",
                self.cwd.display()
            ));
            return Ok(SubjectAdmission {
                subject: None,
                writer_base,
                read: ReadPlan::Live,
                warnings,
                review_warning: None,
                read_source,
                writer_base_setting,
                read_pin: None,
                writer_warnings,
            });
        };
        let name = local_git(git, &self.cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
            .filter(|name| name != "HEAD")
            .unwrap_or_else(|| "HEAD".to_owned());
        if remote {
            let message = format!("live read source pinned to {head} for a peer placement");
            provenant_records::warning::warn(provenant_records::warning::Warning::new(
                provenant_records::warning::READ_SOURCE_PINNED_FOR_PEER,
                message.clone(),
            ));
            warnings.push(message);
        }
        Ok(SubjectAdmission {
            subject: Some(PinnedSubject {
                extra: Default::default(),
                base: None,
                head,
                origin: fixture_origin(&self.context.project_root)
                    .unwrap_or(SubjectOrigin::LocalRef { name }),
                pinned_at,
                policy: SubjectPolicy::FollowLatest,
                repository: None,
                base_ref: None,
                policy_source: Some(read_source.provenance()),
                followed_from: Vec::new(),
            }),
            writer_base,
            read: if remote {
                ReadPlan::Pinned
            } else {
                ReadPlan::Live
            },
            warnings,
            review_warning: None,
            read_source,
            writer_base_setting,
            read_pin: None,
            writer_warnings,
        })
    }
}

/// A smoke fixture project's origin, from its ownership marker; any other
/// checkout has none and keeps its ref origin.
fn fixture_origin(project: &Path) -> Option<SubjectOrigin> {
    crate::smoke::fixture_nonce(project).map(|nonce| SubjectOrigin::Fixture { nonce })
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
    state: &Path,
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
    // The branch's own upstream remote, else the configured forge remote;
    // never an assumed `origin`.
    let remote = configured.or_else(|| {
        provenant_cli::run::resolved_forge_remote(state, cwd)
            .map(|remote| remote.as_str().to_owned())
    })?;
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

/// The credential-free `owner/name` forge id of a pull request's repository
/// URL prefix (`https://host/owner/name`), as subject provenance records it.
/// The full URL stays a transport detail of the commit fetch.
fn forge_repository_id(url: &str) -> Option<String> {
    let path = url.split_once("://").map_or(url, |(_, rest)| rest);
    let (_, path) = path.split_once('/')?;
    let path = path.trim_matches('/').trim_end_matches(".git");
    let (owner, name) = path.rsplit_once('/')?;
    let owner = owner.rsplit('/').next().unwrap_or(owner);
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
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
    fn writer_base_setting_parses_the_config_vocabulary() {
        use super::*;

        assert_eq!(
            WriterBaseSetting::parse("default_branch"),
            Ok(WriterBaseSetting::DefaultBranch)
        );
        assert_eq!(
            WriterBaseSetting::parse(""),
            Ok(WriterBaseSetting::DefaultBranch)
        );
        assert_eq!(
            WriterBaseSetting::parse("checkout"),
            Ok(WriterBaseSetting::Checkout)
        );
        assert_eq!(
            WriterBaseSetting::parse("refs/heads/topic"),
            Ok(WriterBaseSetting::Ref("refs/heads/topic".into()))
        );
        for invalid in ["not a ref", "-bad"] {
            assert!(
                WriterBaseSetting::parse(invalid).is_err(),
                "{invalid} must not be a writer base"
            );
        }
    }

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

        fn subject(fx: &Fixture, head: &str, name: Option<&str>) -> PullSnapshot {
            PullSnapshot {
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
            pull: impl Fn(u64) -> std::result::Result<PullSnapshot, FacadeError>,
        ) -> std::result::Result<Option<(PinnedSubject, Option<String>)>, FacadeError> {
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
            assert_eq!(result.0.head.to_string(), fx.forge_new);
            assert_eq!(result.0.base.unwrap().to_string(), fx.base);
            assert_eq!(result.0.repository.as_deref(), Some("owner/repo"));
            assert_eq!(result.0.base_ref.as_deref(), Some("main"));
            assert_eq!(result.0.policy_source, Some(review_subject_source()));
            assert!(result.1.is_none());
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
            assert_eq!(
                result.0.head.to_string(),
                fx.forge_new,
                "PR head must stay bound"
            );
            assert_eq!(
                result.0.base.unwrap().to_string(),
                fx.forge_old,
                "range must start at review_base"
            );
            assert!(result.1.is_none());
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
            assert_eq!(result.0.head.to_string(), fx.forge_new);
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
            assert_eq!(result.0.head.to_string(), fx.forge_old);
            let warning = result.1.as_deref().ok_or("expected warning")?;
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
            assert!(result.1.is_none(), "unexpected warning: {:?}", result.1);
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
            let warning = result.1.as_deref().ok_or("expected warning")?;
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
    /// A fresh cached row missing subject fields must not hide a usable GraphQL response.
    #[cfg(unix)]
    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "the fixture needs a fresh cache timestamp"
    )]
    fn rate_limited_subject_refreshes_incomplete_fresh_cache()
    -> crate::worktree_tests::support::Result {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        let state = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let command = project.path().join("forge");
        std::fs::write(
            &command,
            "#!/bin/sh\nif [ \"$PROVENANT_PORT_REQUEST_PATH\" = graphql ]; then cat graphql.json; else printf '%s' '{\"nameWithOwner\":\"owner/repo\"}'; fi\n",
        )?;
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700))?;
        let mut ports = provenant_config::PortTable::builtin();
        ports.forge.backend = provenant_config::PortBackend::Command {
            command: provenant_records::PortCommand {
                argv: vec![command.to_string_lossy().into_owned()],
                exit_codes: provenant_records::CommandExitCodes {
                    success: vec![0],
                    unknown: Vec::new(),
                    other: provenant_records::ExitDisposition::Refused,
                },
            },
        };
        std::fs::write(state.path().join("ports.json"), serde_json::to_vec(&ports)?)?;
        std::fs::write(
            state.path().join("config.toml"),
            "schema_version = 1\n[domains.wait]\npr_interval_ms = 60000\n",
        )?;
        let base = "1111111111111111111111111111111111111111";
        let head = "2222222222222222222222222222222222222222";
        let row = serde_json::json!({
            "number": 7, "state": "OPEN", "head": head, "draft": false,
            "merge": "CLEAN", "review": null,
            "url": "https://github.com/owner/repo/pull/7", "checks": [], "truncated": false
        });
        let cache = serde_json::json!({"entries": [{
            "fetched_at": jiff::Timestamp::now().as_millisecond(), "entry": row
        }]});
        let dir = crate::pr::observer::shared_dir(state.path(), "owner/repo")?;
        dir.atomic_write(
            std::ffi::OsStr::new("pr-status.json"),
            ".fixture-",
            &serde_json::to_vec(&cache)?,
            provenant_os::fs::PublishMode::Replace,
        )?;
        let response = serde_json::json!({"data": {"repository": {"a00": {
            "state": "OPEN", "isDraft": false, "mergeStateStatus": "CLEAN",
            "url": "https://github.com/owner/repo/pull/7", "headRefOid": head,
            "baseRefOid": base, "baseRefName": "main"
        }}}});
        std::fs::write(
            project.path().join("graphql.json"),
            serde_json::to_vec(&serde_json::json!({"status": 200, "body": response.to_string()}))?,
        )?;
        let crate::pr::status::batch::Batch::Entries(cached) =
            crate::pr::status::batch::single(state.path(), project.path(), 7)
        else {
            return Err("fixture cache must be readable".into());
        };
        assert!(
            cached[0].base_ref_oid.is_empty(),
            "fixture must hit the legacy cache"
        );
        let reset = jiff::Timestamp::from_second(2_000_000_000)?;
        let subject = subject_read_result(
            state.path(),
            project.path(),
            7,
            crate::pr::observer::ObserveError::RateLimited { reset },
        )?;
        assert_eq!(subject.base_ref_oid.to_string(), base);
        assert_eq!(subject.head_ref_oid.to_string(), head);
        assert_eq!(subject.base_ref_name, "main");
        Ok(())
    }
}

/// The key whose value selects a writer's base when it names no `--base`.
pub(super) const WRITER_BASE_KNOB: &str = "lanes.writer_base";

/// A resolved `lanes.writer_base` value, before Git is consulted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WriterBaseSetting {
    /// The resolved forge remote's default branch (remote HEAD, then the forge).
    DefaultBranch,
    /// The caller's checkout, with no pinned base expression.
    Checkout,
    /// A caller-supplied ref, resolved by the one read-source resolver.
    Ref(String),
}

impl WriterBaseSetting {
    /// Parse one knob spelling. An empty value keeps the compiled
    /// `default_branch`; `checkout` keeps the caller checkout; every other
    /// bounded printable value names a Git ref.
    pub(super) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "" | "default_branch" => Ok(Self::DefaultBranch),
            "checkout" => Ok(Self::Checkout),
            value if provenant_confinement::repo::GitRevision::new(value).is_ok() => {
                Ok(Self::Ref(value.to_owned()))
            }
            _ => Err(format!(
                "expected default_branch, checkout, or a Git ref (got {value:?})"
            )),
        }
    }
}

impl std::fmt::Display for WriterBaseSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DefaultBranch => f.write_str("default_branch"),
            Self::Checkout => f.write_str("checkout"),
            Self::Ref(value) => f.write_str(value),
        }
    }
}

/// The sourced writer-base policy and the configuration layer that chose it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WriterBaseChoice {
    pub(super) value: WriterBaseSetting,
    pub(super) layer: Layer,
    pub(super) origin: Option<String>,
}

impl Default for WriterBaseChoice {
    fn default() -> Self {
        Self {
            value: WriterBaseSetting::DefaultBranch,
            layer: Layer::Default,
            origin: None,
        }
    }
}

impl WriterBaseChoice {
    /// Where this choice was set, for warnings and receipts.
    pub(super) fn description(&self) -> String {
        match &self.origin {
            Some(origin) => format!("{} ({origin})", self.layer.as_str()),
            None => self.layer.as_str().to_owned(),
        }
    }
}

/// The launch expression that supplied a writer's base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WriterBaseSource {
    ExplicitBase,
    BranchDefault,
    ConfiguredRef,
    WorktreeHead,
    NodeBase,
    WorktreeUpstream,
    DefaultBranchMergeBase,
    CallerCheckout,
}

impl WriterBaseSource {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::ExplicitBase => "explicit --base",
            Self::BranchDefault => "the --branch default",
            Self::ConfiguredRef => "lanes.writer_base",
            Self::WorktreeHead => "the named worktree HEAD",
            Self::NodeBase => "the writer node's base",
            Self::WorktreeUpstream => "the reused worktree branch's upstream",
            Self::DefaultBranchMergeBase => "the worktree's merge base with the default branch",
            Self::CallerCheckout => "no base expression",
        }
    }
}

/// The one base a writer launch starts from, resolved before its identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WriterBase {
    pub(super) commit: Option<String>,
    pub(super) source: WriterBaseSource,
    /// Set when a configured value could not be resolved; advisory only.
    pub(super) warning: Option<String>,
}

impl WriterBase {
    /// Name the expression and the fallback when no base could be resolved.
    /// Missing Git evidence is advisory, never a refusal.
    pub(super) fn fallback_warning(&self, cwd: &Path) -> Option<String> {
        self.commit.is_none().then(|| {
            format!(
                "writer launch has no resolved base (source: {}); its identity falls back to the caller checkout at {}",
                self.source.label(),
                cwd.display()
            )
        })
    }
}

/// Resolve one writer base by role: explicit `--base`, else the configured
/// `lanes.writer_base` for a `--branch` launch, else the named worktree's HEAD,
/// else none. The explicit flag always wins; no Git evidence is fatal.
pub(super) fn resolve_writer_base(
    input: &Value,
    project_root: &Path,
    cwd: &Path,
    state: &Path,
    choice: &WriterBaseChoice,
    read_source: &ReadSourceChoice,
    git: Option<&Path>,
) -> WriterBase {
    if let Some(base) = input
        .get("base")
        .and_then(Value::as_str)
        .filter(|base| !base.is_empty())
    {
        return WriterBase {
            commit: resolve_ref(state, cwd, base, read_source, "--base"),
            source: WriterBaseSource::ExplicitBase,
            warning: None,
        };
    }
    if input.get("branch").is_some_and(|branch| !branch.is_null()) {
        let unresolved = || {
            format!(
                "{WRITER_BASE_KNOB}={} (source: {}) could not be resolved; writer base falls back to the caller checkout",
                choice.value,
                choice.description()
            )
        };
        return match &choice.value {
            // `checkout` names the caller checkout's HEAD; resolving it here
            // lets provisioning and identity start from the same commit.
            WriterBaseSetting::Checkout => WriterBase {
                commit: git.and_then(|git| provenant_run::resolve_git_base(git, cwd, "HEAD").ok()),
                source: WriterBaseSource::CallerCheckout,
                warning: None,
            },
            WriterBaseSetting::DefaultBranch => match resolve_default_branch(state, cwd) {
                Some(pin) => WriterBase {
                    commit: Some(pin.commit.to_string()),
                    source: WriterBaseSource::BranchDefault,
                    warning: None,
                },
                None => WriterBase {
                    commit: None,
                    source: WriterBaseSource::CallerCheckout,
                    warning: Some(unresolved()),
                },
            },
            WriterBaseSetting::Ref(reference) => {
                match resolve_ref(state, cwd, reference, read_source, WRITER_BASE_KNOB) {
                    Some(commit) => WriterBase {
                        commit: Some(commit),
                        source: WriterBaseSource::ConfiguredRef,
                        warning: None,
                    },
                    None => WriterBase {
                        commit: None,
                        source: WriterBaseSource::CallerCheckout,
                        warning: Some(unresolved()),
                    },
                }
            }
        };
    }
    if let Some(worktree) = named_worktree(input) {
        let path = absolute(project_root, worktree);
        return WriterBase {
            commit: git.and_then(|git| provenant_run::resolve_git_base(git, &path, "HEAD").ok()),
            source: WriterBaseSource::WorktreeHead,
            warning: None,
        };
    }
    WriterBase {
        commit: None,
        source: WriterBaseSource::CallerCheckout,
        warning: None,
    }
}

/// Resolve the base of a writer node that reuses an existing worktree (#4064).
/// The node's declared `base` wins; otherwise the worktree branch's upstream,
/// else the point where it diverged from the default branch of the configured
/// forge remote (never an assumed `origin`). A worktree with neither has no
/// Git evidence to pin, so its base stays unresolved and a graph that needs
/// one is refused before launch.
/// Spec: docs/specs/run-launch.md#writer-binding
pub(super) fn resolve_reused_worktree_base(
    node: &GraphNode,
    project_root: &Path,
    cwd: &Path,
    state: &Path,
    read_source: &ReadSourceChoice,
    git: Option<&Path>,
) -> Option<WriterBase> {
    if let Some(base) = node.base.as_deref().filter(|base| !base.is_empty()) {
        return Some(WriterBase {
            commit: resolve_ref(state, cwd, base, read_source, "node base"),
            source: WriterBaseSource::NodeBase,
            warning: None,
        });
    }
    let worktree = node.worktree.as_deref().filter(|path| !path.is_empty())?;
    let git = git?;
    let path = absolute(project_root, worktree);
    if let Ok(commit) = provenant_run::resolve_git_base(git, &path, "HEAD@{upstream}") {
        return Some(WriterBase {
            commit: Some(commit),
            source: WriterBaseSource::WorktreeUpstream,
            warning: None,
        });
    }
    Some(WriterBase {
        commit: resolve_default_branch(state, &path).and_then(|pin| {
            provenant_run::review_merge_base_between(git, &path, "HEAD", &pin.commit.to_string())
                .ok()
        }),
        source: WriterBaseSource::DefaultBranchMergeBase,
        warning: None,
    })
}

/// A `--worktree` control, or a composite's `--set worktree=PATH` binding.
fn named_worktree(input: &Value) -> Option<&str> {
    input
        .get("worktree")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .or_else(|| {
            input
                .pointer("/set/worktree")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
        })
}

fn absolute(project_root: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_owned()
    } else {
        project_root.join(path)
    }
}

/// Resolve a named base through the same read-source resolver as any explicit
/// ref, keeping the launch layer and the expression that chose it.
fn resolve_ref(
    state: &Path,
    cwd: &Path,
    base: &str,
    read_source: &ReadSourceChoice,
    origin: &str,
) -> Option<String> {
    resolve_read_source(
        state,
        cwd,
        &ReadSourceChoice {
            source: provenant_cli::run::ReadSource::Ref(base.to_owned()),
            fetch_first: read_source.fetch_first,
            layer: Layer::Launch,
            origin: Some(origin.to_owned()),
        },
    )
    .map(|selected| selected.commit.to_string())
}

/// Pin every read-only agent leaf to the one commit admission chose for it: the
/// writer base in a writer graph, else the admitted subject's head. This is the
/// single read-binding rule. A node or composite that names its own source
/// keeps it: its members inherit that subject when the graph expands, and a
/// fixed commit must not override (say) the writer head a review panel reads.
/// Container nodes stay unbindable so a writer member never inherits a read
/// source from its read-only boundary, and only an agent node takes an
/// execution binding at all.
pub(super) fn bind_read_source(node: &mut GraphNode, commit: &str) {
    if node.source.is_some() {
        return;
    }
    // Only an agent node executes, so only it takes an execution binding;
    // graph validation refuses a source on a gate, decision or landing.
    let agent = matches!(
        node.kind,
        None | Some(NodeKind::Task | NodeKind::Synthesise)
    );
    if agent
        && node.access == AccessMode::ReadOnly
        && node.continues.is_none()
        && node.composite.is_none()
        && node.loop_body.is_none()
    {
        node.source = Some(SourceBinding::Commit {
            commit: commit.into(),
        });
    }
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            bind_read_source(child, commit);
        }
    }
    if let Some(loop_spec) = &mut node.loop_body {
        for child in &mut loop_spec.body {
            bind_read_source(child, commit);
        }
    }
}

/// Carry the admitted writer base into every defaulted provisioning intent,
/// so the worktree a `--branch` launch creates starts from the
/// `lanes.writer_base` commit its identity names instead of re-deriving the
/// forge default. An explicit `--base` is not defaulted and stays as given;
/// an existing remote branch tip still wins during provisioning.
pub(super) fn bind_provisioning_base(node: &mut GraphNode, commit: &str) {
    if let Some(spec) = node
        .attempt
        .as_mut()
        .and_then(|attempt| attempt.new_worktree.as_mut())
        && spec.default_base
    {
        spec.base = commit.into();
    }
    if let Some(composite) = &mut node.composite {
        for child in &mut composite.nodes {
            bind_provisioning_base(child, commit);
        }
    }
    if let Some(loop_spec) = &mut node.loop_body {
        for child in &mut loop_spec.body {
            bind_provisioning_base(child, commit);
        }
    }
}
