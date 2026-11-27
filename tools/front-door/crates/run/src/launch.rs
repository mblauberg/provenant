//! Supervisor handoff for accepted attempts. The provider seam is minimal and
//! shaped for the providers crate's launch specification: the composition edge
//! resolves routes, applies fallback and builds the exact invocation.
use crate::{
    artifacts::{read_sealed, read_sealed_observed},
    db::decode,
    *,
};
use provenant_core::{ActorId, AttemptId, DurationMicros, OperationId, Pause};
use provenant_os::exec::Executor;
use provenant_records::CommandError;
use provenant_records::command_result::ErrorClass;
use provenant_records::custody::Scope;
use provenant_supervisor::custody::{
    self, CapturePlan, ExitObservation, OwnedLaunch, ProviderCommand, PublicationReservation,
};
use rusqlite::params;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

/// A `--commit own` commit: only the claimed paths, staged in a private index,
/// with a compare-and-swap update of the lane branch.
///
/// Spec: docs/specs/agent-native.md#4-a-lane-as-a-native-sub-agent (#3453)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnCommit {
    pub head: provenant_core::GitOid,
    pub ref_name: String,
    pub paths: Vec<String>,
}

/// Exact source for a private-index commit. Path mode snapshots selected
/// worktree files; patch mode applies the supplied diff to the expected tree.
#[derive(Clone, Copy, Debug)]
pub enum OwnCommitInput<'a> {
    Paths(&'a [PathBuf]),
    Patch {
        bytes: &'a [u8],
        paths: &'a [String],
    },
}

/// Inputs and runtime dependencies for an own-path commit.
pub struct OwnCommitRequest<'a> {
    pub worktree: &'a Path,
    pub git_dir: &'a Path,
    pub ref_name: &'a str,
    pub expected: provenant_core::GitOid,
    pub input: OwnCommitInput<'a>,
    pub message: &'a str,
    pub retries: u32,
    pub env: &'a [(OsString, OsString)],
    pub executor: &'a dyn Executor,
    pub cancel: &'a provenant_core::CancelToken,
    pub pause: &'a dyn Pause,
    pub timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
enum CommitOwnError {
    #[error("git executable unavailable")]
    GitUnavailable,
    #[error("git {operation} failed: {detail}")]
    Git {
        operation: &'static str,
        detail: String,
    },
    #[error("git {operation} timed out")]
    TimedOut { operation: &'static str },
    #[error("invalid git object id: {0}")]
    InvalidOid(String),
    #[error("command failed: {0}")]
    Exec(#[from] provenant_os::exec::ExecError),
}

/// Remove a private index on every exit path, including an early error.
struct PrivateIndex(PathBuf);
impl Drop for PrivateIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Clone, Copy)]
struct GitOwnContext<'a> {
    cwd: &'a Path,
    env: &'a [(OsString, OsString)],
    executor: &'a dyn Executor,
    cancel: &'a provenant_core::CancelToken,
    timeout: Duration,
}

impl GitOwnContext<'_> {
    fn run(
        &self,
        args: &[&str],
        operation: &'static str,
    ) -> std::result::Result<crate::worktree_carry::GitOutput, CommitOwnError> {
        self.with_stdin(args, None, operation)
    }

    fn with_stdin(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        operation: &'static str,
    ) -> std::result::Result<crate::worktree_carry::GitOutput, CommitOwnError> {
        let out = crate::worktree_carry::git_with_stdin(
            self.cwd,
            args,
            stdin,
            self.env,
            self.executor,
            self.cancel,
            self.timeout,
        )
        .map_err(|error| match error {
            crate::worktree_carry::CarryError::GitUnavailable => CommitOwnError::GitUnavailable,
            crate::worktree_carry::CarryError::Exec(source) => CommitOwnError::Exec(source),
            other => CommitOwnError::Git {
                operation,
                detail: other.to_string(),
            },
        })?;
        if out.timed_out {
            return Err(CommitOwnError::TimedOut { operation });
        }
        Ok(out)
    }
}

fn index_contention(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("index.lock") || lower.contains("unable to create")
}

fn cas_mismatch(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("cannot lock ref") || lower.contains("is at") || lower.contains("expected")
}

fn retry_error(retries: u32, detail: &str) -> CommandError {
    CommandError::new(
        "INDEX_LOCK_RETRY_EXHAUSTED",
        format!("commit could not complete after {retries} index-lock/compare-and-swap retries"),
        Some(detail),
        "provenant wait the holder, then retry the launch",
    )
    .with_class(ErrorClass::Transient { retry_at: None })
}

/// Commit selected worktree paths or an exact patch through a private index,
/// leaving the shared index untouched. The branch moves only through a
/// compare-and-swap `update-ref`; a lost race reapplies the same input to the
/// new head, while index-lock contention retries with a cancellable backoff.
pub fn commit_own_paths(
    request: OwnCommitRequest<'_>,
) -> std::result::Result<OwnCommit, CommandError> {
    let OwnCommitRequest {
        worktree,
        git_dir,
        ref_name,
        expected,
        input,
        message,
        retries,
        env,
        executor,
        cancel,
        pause,
        timeout,
    } = request;
    let git = GitOwnContext {
        cwd: worktree,
        env,
        executor,
        cancel,
        timeout,
    };
    let to_error = |error: CommitOwnError| -> CommandError {
        match error {
            CommitOwnError::GitUnavailable => CommandError::new(
                "GIT_UNAVAILABLE",
                "git executable unavailable",
                None,
                "install git, then retry the launch",
            ),
            CommitOwnError::TimedOut { operation } => CommandError::new(
                "COMMIT_TIMEOUT",
                format!("git {operation} timed out"),
                None,
                "retry with a longer --timeout",
            )
            .with_class(ErrorClass::Transient { retry_at: None }),
            other => CommandError::new(
                "COMMIT_OWN_FAILED",
                "could not commit the lane's own paths",
                Some(other.to_string().as_str()),
                "inspect the worktree, then retry the launch",
            ),
        }
    };
    let (path_strings, pathspecs, patch) = match input {
        OwnCommitInput::Paths(paths) => {
            let path_strings = paths
                .iter()
                .map(|path| {
                    use std::path::Component;

                    let relative = if path.is_absolute() {
                        path.strip_prefix(worktree).map_err(|_| {
                            "commit path is outside the selected worktree".to_owned()
                        })?
                    } else {
                        path.as_path()
                    };
                    if relative.as_os_str().is_empty()
                        || path.to_str().is_some_and(|path| {
                            path.split('/').any(|part| part == ".." || part == ".")
                        })
                        || relative.components().any(|component| {
                            matches!(
                                component,
                                Component::CurDir
                                    | Component::ParentDir
                                    | Component::RootDir
                                    | Component::Prefix(_)
                            )
                        })
                    {
                        return Err("commit path is not worktree-relative".to_owned());
                    }
                    relative
                        .to_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "commit path is not valid UTF-8".to_owned())
                })
                .collect::<std::result::Result<Vec<_>, _>>();
            let path_strings = path_strings.map_err(|detail| {
                CommandError::new(
                    "COMMIT_OWN_PATH_INVALID",
                    "could not commit the lane's own paths",
                    Some(detail.as_str()),
                    "inspect the requested paths, then retry the launch",
                )
            })?;
            let pathspecs = path_strings
                .iter()
                .map(|path| format!(":(literal){path}"))
                .collect::<Vec<_>>();
            (path_strings, pathspecs, None)
        }
        OwnCommitInput::Patch { bytes, paths } => {
            let path_strings = paths
                .iter()
                .map(|path| {
                    let relative = Path::new(path);
                    if relative.as_os_str().is_empty()
                        || relative.is_absolute()
                        || path
                            .split('/')
                            .any(|part| part.is_empty() || part == "." || part == "..")
                        || relative.components().any(|component| {
                            matches!(
                                component,
                                std::path::Component::CurDir
                                    | std::path::Component::ParentDir
                                    | std::path::Component::RootDir
                                    | std::path::Component::Prefix(_)
                            )
                        })
                    {
                        return Err("patch path is not worktree-relative".to_owned());
                    }
                    Ok(path.clone())
                })
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|detail| {
                    CommandError::new(
                        "COMMIT_OWN_PATH_INVALID",
                        "could not commit the lane's own paths",
                        Some(detail.as_str()),
                        "inspect the requested paths, then retry the launch",
                    )
                })?;
            (path_strings, Vec::new(), Some(bytes))
        }
    };
    if path_strings.is_empty() {
        return Ok(OwnCommit {
            head: expected,
            ref_name: ref_name.to_owned(),
            paths: Vec::new(),
        });
    }
    let mut expected = expected;
    let mut attempt = 0_u32;
    loop {
        if cancel.is_cancelled() {
            return Err(CommandError::new(
                "LAUNCH_CANCELLED",
                "commit cancelled before completion",
                None,
                "relaunch the lane",
            )
            .with_class(ErrorClass::Transient { retry_at: None }));
        }
        let tag = format!("{}-{attempt}", std::process::id());
        let private = PrivateIndex(crate::worktree_carry::private_index_path(git_dir, &tag));
        let mut index_env = env.to_vec();
        index_env.push((
            OsString::from("GIT_INDEX_FILE"),
            private.0.as_os_str().to_owned(),
        ));
        let index_git = GitOwnContext {
            env: &index_env,
            ..git
        };
        let expected_text = expected.to_string();

        let read = index_git
            .run(&["read-tree", expected_text.as_str()], "read-tree")
            .map_err(to_error)?;
        if !read.ok {
            if index_contention(&read.stderr) {
                attempt += 1;
                if attempt > retries {
                    return Err(retry_error(retries, &read.stderr));
                }
                pause.wait(
                    DurationMicros::from_micros(5_000_u64 * u64::from(attempt)),
                    cancel,
                );
                continue;
            }
            return Err(to_error(CommitOwnError::Git {
                operation: "read-tree",
                detail: read.stderr,
            }));
        }

        let stage = match patch {
            Some(bytes) => {
                index_git.with_stdin(&["apply", "--cached"], Some(bytes), "apply cached patch")
            }
            None => {
                let path_refs: Vec<&str> = pathspecs.iter().map(String::as_str).collect();
                let mut add_args = vec!["add", "--"];
                add_args.extend(path_refs.iter().copied());
                index_git.run(&add_args, "add")
            }
        }
        .map_err(to_error)?;
        if !stage.ok {
            if index_contention(&stage.stderr) {
                attempt += 1;
                if attempt > retries {
                    return Err(retry_error(retries, &stage.stderr));
                }
                pause.wait(
                    DurationMicros::from_micros(5_000_u64 * u64::from(attempt)),
                    cancel,
                );
                continue;
            }
            return Err(to_error(CommitOwnError::Git {
                operation: if patch.is_some() {
                    "apply cached patch"
                } else {
                    "add"
                },
                detail: stage.stderr,
            }));
        }

        let tree = index_git
            .run(&["write-tree"], "write-tree")
            .map_err(to_error)?;
        if !tree.ok {
            return Err(to_error(CommitOwnError::Git {
                operation: "write-tree",
                detail: tree.stderr,
            }));
        }
        let tree_oid = tree.stdout.trim().to_owned();

        let commit_args = vec![
            "commit-tree",
            &tree_oid,
            "-p",
            expected_text.as_str(),
            "-m",
            message,
        ];
        let made = git.run(&commit_args, "commit-tree").map_err(to_error)?;
        if !made.ok {
            return Err(to_error(CommitOwnError::Git {
                operation: "commit-tree",
                detail: made.stderr,
            }));
        }
        let new = provenant_core::GitOid::parse(made.stdout.trim())
            .map_err(|_| to_error(CommitOwnError::InvalidOid(made.stdout.trim().to_owned())))?;
        let new_text = new.to_string();

        let update = git
            .run(
                &[
                    "update-ref",
                    ref_name,
                    new_text.as_str(),
                    expected_text.as_str(),
                ],
                "update-ref",
            )
            .map_err(to_error)?;
        if update.ok {
            return Ok(OwnCommit {
                head: new,
                ref_name: ref_name.to_owned(),
                paths: path_strings,
            });
        }
        if cas_mismatch(&update.stderr) || index_contention(&update.stderr) {
            attempt += 1;
            if attempt > retries {
                return Err(retry_error(retries, &update.stderr));
            }
            pause.wait(
                DurationMicros::from_micros(5_000_u64 * u64::from(attempt)),
                cancel,
            );
            let head = git
                .run(&["rev-parse", ref_name], "rev-parse")
                .map_err(to_error)?;
            if !head.ok {
                return Err(to_error(CommitOwnError::Git {
                    operation: "rev-parse",
                    detail: head.stderr,
                }));
            }
            expected = provenant_core::GitOid::parse(head.stdout.trim())
                .map_err(|_| to_error(CommitOwnError::InvalidOid(head.stdout.trim().to_owned())))?;
            continue;
        }
        return Err(to_error(CommitOwnError::Git {
            operation: "update-ref",
            detail: update.stderr,
        }));
    }
}

#[cfg(test)]
#[path = "launch/commit_own_tests.rs"]
mod commit_own_tests;

fn sandbox_mode_receipt(
    mode: &provenant_confinement::run_config::SandboxMode,
) -> provenant_records::run::SandboxModeReceipt {
    provenant_records::run::SandboxModeReceipt {
        requested: mode.requested,
        effective: mode.effective,
        // The preparation edge records the knob's actual layer on the sibling
        // admission receipt; the supervisor rebind keeps the host-layer default.
        source: mode.source,
        constraint: mode.constraint,
        warning: mode.warning.clone(),
        extra: Default::default(),
    }
}

/// An explicit dispatch argument wins over the effective profile default. A
/// batch with no writer defaults from `lanes.max_concurrency_read_only`
/// (16); any batch with a writer defaults from `lanes.max_concurrency` (8).
/// `bound` is a `lanes.max_concurrency` the user set while leaving the
/// read-only knob alone: it caps the read-only default, never an explicit value.
pub fn concurrency(
    explicit: Option<u64>,
    profile: &HarnessProfile,
    read_only: bool,
    bound: Option<u64>,
) -> u64 {
    explicit.unwrap_or_else(|| {
        if read_only {
            let default = profile
                .knob_u64("lanes.max_concurrency_read_only")
                .unwrap_or(16);
            bound.map_or(default, |bound| default.min(bound))
        } else {
            profile.knob_u64("lanes.max_concurrency").unwrap_or(8)
        }
    })
}

pub fn capture_plan(task: &TaskRequest, limits: &provenant_records::LaneLimits) -> CapturePlan {
    let enforced = |limit: &Option<provenant_records::Limit>| {
        limit
            .as_ref()
            .filter(|limit| limit.enforced())
            .map_or(0, |limit| limit.value)
    };
    CapturePlan {
        stdout: if matches!(&task.route, crate::RequestedRoute::Adapter { provider, .. }
            if provenant_records::AdapterSpec::get(provider)
                .is_some_and(|s| s.launch.framing == provenant_records::StreamFraming::Raw))
        {
            provenant_supervisor::custody::StreamMode::Tail {
                limit: provenant_records::COMMAND_OUTPUT_TAIL_BYTES as u64,
            }
        } else {
            CapturePlan::default().stdout
        },
        wall_ms: enforced(&limits.deadline_seconds).saturating_mul(1000),
        token_budget: enforced(&limits.token_budget),
        budget_action: limits.budget_policy.clone().unwrap_or_default().action,
        // Writers only: a fresh attempt continues a writer's retained
        // worktree from its handoff. 0 or unset is off; a provider that
        // reports no per-call context figure is never stopped for it.
        context_handoff: enforced(&limits.context_handoff),
        ..CapturePlan::default()
    }
}

/// One prepared provider invocation. Secret inputs stay in memory and travel
/// on the handoff pipe, then a dedicated read pipe into the provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationExpectation {
    /// The predecessor session bound by the provider's prepared stream.
    pub session: String,
}

/// Host-owned decode context for a resumed provider invocation.
#[derive(Clone, Copy)]
pub struct ContinuationDecodeContext<'a> {
    /// The sealed roots used by terminal settlement, when available.
    pub context: Option<&'a HolderContext>,
    /// The retained session that the child must prove it continued.
    pub expectation: Option<&'a ContinuationExpectation>,
    /// The attempt request whose dispatch overrides refine settlement policy.
    pub task: Option<&'a TaskRequest>,
}

pub struct PreparedLaunch {
    pub argv: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    pub stdin: Vec<u8>,
    pub secret_input: Option<provenant_os::exec::SecretInput>,
    pub cwd: PathBuf,
    /// Exact provider/model/effort the invocation applies.
    pub route: RouteRecord,
    /// Fallback candidates that failed before this one launched.
    pub skipped_routes: Vec<SkippedRoute>,
    /// HEAD of the checkout a read-only lane runs in, read by the host at
    /// preparation. Writers record theirs from delivery instead.
    pub observed_head: Option<provenant_core::GitOid>,
    /// Canonical Git common directory verified by the host preparation edge.
    pub repository: Option<provenant_records::AbsolutePath>,
    pub confinement: Option<crate::LaunchConfinement>,
    /// The lane's limits, resolved once at preparation. Admission records
    /// them; launch, enforcement and the preamble read nothing else.
    pub limits: provenant_records::LaneLimits,
    /// The retained session this prepared provider invocation must continue.
    /// Admission seals it with the selected route for both live and terminal
    /// identity validation.
    pub continuation_expectation: Option<ContinuationExpectation>,
}

impl PreparedLaunch {
    /// Resolve backend availability once, before recording the applied route.
    /// The provider's already-prepared environment stays at least as narrow.
    fn bind_sandbox_backend(
        &mut self,
        cancel: &provenant_core::CancelToken,
        policy: &provenant_records::policy::Policy,
    ) -> Result<()> {
        use provenant_core::consumer_env::SANDBOX;
        let Some(confinement) = self.confinement.as_mut() else {
            return Ok(());
        };
        if confinement.sandbox.requires_backend() {
            let availability = confinement.bind_backend(cancel, policy)?;
            let access = self.route.lane_access.as_ref();
            let isolated = access.is_some_and(|receipt| receipt.access.is_isolated());
            confinement
                .sandbox
                .resolve_backend(availability, isolated)?;
            if !confinement.sandbox.effective {
                let source = access.map_or("default", |receipt| receipt.source.as_str());
                let detail = format!(
                    "{} (lanes.access=host, source: {source})",
                    confinement
                        .sandbox
                        .warning
                        .as_deref()
                        .unwrap_or("sandbox unavailable")
                );
                confinement.sandbox.warning = Some(detail.clone());
                if let Some(message) = policy
                    .check(
                        "LAUNCH_SANDBOX_UNAVAILABLE_NONE",
                        provenant_records::policy::Facts::violation(detail),
                    )
                    .message()
                {
                    use provenant_records::warning::{SANDBOX_UNAVAILABLE, Warning, warn};
                    // Spec: docs/specs/cli.md#warnings-are-typed-values
                    warn(
                        Warning::new(
                            SANDBOX_UNAVAILABLE,
                            message,
                            provenant_records::warning::detected_source(SANDBOX_UNAVAILABLE),
                        )
                        .subject("LAUNCH_SANDBOX_UNAVAILABLE_NONE"),
                    );
                }
                self.environment.retain(|(name, _)| name != SANDBOX);
                self.environment
                    .push((SANDBOX.into(), confinement.sandbox.env_value().into()));
                self.route.sandbox = Some(sandbox_mode_receipt(&confinement.sandbox));
            }
        }
        if !confinement.sandbox.effective {
            // No OS mark survives a double-fork and setsid on this lane. The
            // guardian still records the spawned group and session as an
            // attempt tag, so reconcile can name a detached process after
            // guardian loss, but cannot prove it enough to signal it. Warn
            // once per change, naming where the access/sandbox mode came from
            // and letting the chair turn the notice off.
            let source = self
                .route
                .lane_access
                .as_ref()
                .map_or("default", |receipt| receipt.source.as_str());
            let detail = format!(
                "no OS process containment for this lane (lanes.access=host, source: {source}); a detached process is attributed by its recorded group and session and can only be reported"
            );
            if let provenant_records::policy::Effect::Warn(warning) = policy.check(
                "LAUNCH_PROCESS_ATTRIBUTION_NONE",
                provenant_records::policy::Facts::violation(detail),
            ) {
                use provenant_records::warning::warn;
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(warning.once_per_change());
            }
            return Ok(());
        }
        let mode = &confinement.sandbox;
        if let Some(warning) = &mode.warning {
            use provenant_records::warning::{SANDBOX_UNAVAILABLE, Warning, warn};
            // Spec: docs/specs/cli.md#warnings-are-typed-values
            warn(Warning::new(
                SANDBOX_UNAVAILABLE,
                warning.clone(),
                provenant_records::warning::detected_source(SANDBOX_UNAVAILABLE),
            ));
        }
        self.environment.retain(|(name, _)| name != SANDBOX);
        self.environment
            .push((SANDBOX.into(), mode.env_value().into()));
        self.route.sandbox = Some(sandbox_mode_receipt(mode));
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum WriteGrantError {
    #[error("invalid write grant path")]
    InvalidPath,
    #[error("write-grant threshold selected strict policy")]
    Policy(provenant_records::diagnostic::ResourceFact),
}

#[allow(clippy::print_stderr)]
fn applied_write_grants(
    plan: &PreparedLaunch,
    limit: WriteGrantLimit,
    policy: &provenant_records::policy::Policy,
) -> std::result::Result<admission::AppliedWriteGrants, WriteGrantError> {
    let Some(confinement) = &plan.confinement else {
        return Ok(admission::AppliedWriteGrants::default());
    };
    let permissions = confinement.plan.permissions();
    let grants = provenant_confinement::write_grants(permissions, &confinement.profile)
        .into_iter()
        .map(|grant| {
            Ok(WriteGrantRecord {
                extra: Default::default(),
                source: grant.source,
                path: provenant_records::AbsolutePath::new(&grant.path).map_err(|_| {
                    policy.boundary_error(
                        "LAUNCH_APPLIED_WRITE_GRANTS_AUTHORITY",
                        provenant_records::policy::Facts::violation(
                            "applied write grant path is not canonical and absolute",
                        ),
                        WriteGrantError::InvalidPath,
                    )
                })?,
                scope: grant.scope,
            })
        })
        .collect::<std::result::Result<Vec<_>, WriteGrantError>>()?;
    let encoded = provenant_records::CanonicalV1::bytes(&grants).map_err(|_| {
        policy.boundary_error(
            "LAUNCH_APPLIED_WRITE_GRANTS_AUTHORITY",
            provenant_records::policy::Facts::violation(
                "applied write grants cannot be encoded canonically",
            ),
            WriteGrantError::InvalidPath,
        )
    })?;
    let effect = policy.check(
        "WRITE_GRANT_BYTES",
        provenant_records::policy::Facts {
            violated: encoded.as_bytes().len() as u64 > limit.bytes,
            detail: format!(
                "encoded write grants {} bytes, threshold={}, threshold source={}",
                encoded.as_bytes().len(),
                limit.bytes,
                limit.origin.description()
            ),
        },
    );
    if effect.blocks() {
        return Err(WriteGrantError::Policy(
            provenant_records::diagnostic::ResourceFact::WriteGrantLimit {
                limit: limit.bytes,
                origin: limit.origin,
            },
        ));
    }
    if let provenant_records::policy::Effect::Warn(warning) = &effect {
        // Spec: docs/specs/cli.md#warnings-are-typed-values
        provenant_records::warning::warn((**warning).clone());
    }
    let git_grants_digest = match &permissions.mode {
        provenant_confinement::permissions::AccessMode::ReadOnly => None,
        provenant_confinement::permissions::AccessMode::WorktreeWrite(authority) => Some(
            provenant_core::Digest::from_bytes(authority.git_grants_digest),
        ),
    };
    Ok(admission::AppliedWriteGrants {
        grants,
        git_grants_digest,
    })
}
/// Provider-decoded facts from a completed capture.
pub struct Decoded {
    pub cause: Option<CauseKind>,
    pub outcome: Outcome,
    pub result: String,
    pub complete: bool,
    pub observed_route: Option<RouteRecord>,
    pub usage: UsageRecord,
    /// Provider session handle decoded from the stream, if any.
    pub session: Option<String>,
    pub reason: Option<String>,
    /// Partial-finish signals the provider decode found; run adds its own.
    pub signals: Vec<SettlementSignal>,
}
/// A task-local preparation refusal, retaining candidate classifications.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct PrepareRefusal {
    /// Boundary established by the original typed preparation failure, if any.
    pub boundary_class: provenant_records::policy::Boundary,
    /// Stable table row for the predicate that refused preparation.
    pub policy_code: Option<String>,
    /// A configurable resource threshold that refused this launch, if known.
    pub resource: Option<provenant_records::diagnostic::ResourceFact>,
    /// The specific cause. There is no generic refusal.
    pub cause: CauseKind,
    /// Fixed-form text naming a configured value, never captured output.
    pub terminal_note: Option<String>,
    pub message: String,
    /// Host-authored routing explanation, never provider or OS diagnostics.
    pub host_message: Option<String>,
    pub skipped_routes: Vec<SkippedRoute>,
    pub fallback_outcome: Option<provenant_records::run::PreparationFailure>,
}
/// A refusal for test fixtures that do not care about its cause.
#[cfg(test)]
pub(crate) fn fixture_refusal(message: impl Into<String>) -> Box<PrepareRefusal> {
    PrepareRefusal::boxed(CauseKind::LaunchHandoffRefused, message)
}

impl PrepareRefusal {
    /// Public continuation errors use typed candidate projections, never the
    /// raw preparation message, which may include paths or provider output.
    pub(crate) fn safe_reason(&self) -> String {
        let facts: Vec<_> = self
            .skipped_routes
            .iter()
            .filter_map(|route| route.continuation_guard)
            .map(|guard| guard.to_string())
            .collect();
        if facts.is_empty() {
            self.cause.diagnostic(true)
        } else {
            facts.join("; ")
        }
    }

    pub fn boxed(cause: CauseKind, message: impl Into<String>) -> Box<Self> {
        Box::new(Self {
            boundary_class: provenant_records::policy::Boundary::None,
            policy_code: None,
            resource: None,
            cause,
            terminal_note: None,
            message: message.into(),
            host_message: None,
            skipped_routes: Vec::new(),
            fallback_outcome: None,
        })
    }

    fn kind(&self) -> RefusalKind {
        if let Some(route) = self.skipped_routes.iter().find(|route| {
            route.cause == Some(provenant_records::RouteExclusionReason::PacingReserve)
        }) {
            match route.guard_resource.clone() {
                Some(fact @ provenant_records::diagnostic::ResourceFact::PacingReserve { .. }) => {
                    RefusalKind::PacingReserveResource(fact)
                }
                _ => RefusalKind::PacingReserve,
            }
        } else if let Some(fact) = self.resource.clone() {
            RefusalKind::PreparationResource(fact)
        } else if self.boundary_class != provenant_records::policy::Boundary::None {
            RefusalKind::PreparationBoundary(self.boundary_class)
        } else {
            RefusalKind::Preparation
        }
    }

    pub fn auth_required(&self) -> bool {
        !self.skipped_routes.is_empty()
            && self
                .skipped_routes
                .iter()
                .all(|route| route.kind == SkipKind::AuthRequired)
    }
}
#[cfg(test)]
#[path = "launch/pacing_refusal_tests.rs"]
mod pacing_refusal_tests;

/// Run-owned per-attempt paths. Allocated once, shared by explicit fallback candidates.
pub struct LaunchPaths {
    pub attempt_id: AttemptId,
    pub operation_id: OperationId,
    pub task_index: u64,
    pub lane_depth: u64,
    pub deadline_unix_ms: u64,
    pub scratch: PathBuf,
    pub provider_state: PathBuf,
    /// Private temporary directory for a lane, under the per-attempt scratch
    /// root outside the attempt record directory (#3947), never a shared
    /// system one. Terminal commit and withdrawal remove it best-effort.
    pub temporary: PathBuf,
    /// Short symlink to `temporary` on macOS, for a `TMPDIR` that fits
    /// `sun_path`. Absent when unavailable; grants always use `temporary`.
    pub temporary_alias: Option<PathBuf>,
}

/// A fresh continuation prepared before its operation and attempt are recorded.
/// Only this invocation's exclusively created, unrecorded staging is disposable.
pub struct PreparedStart {
    pub task: TaskRequest,
    pub attempt: AttemptId,
    pub plan: std::cell::RefCell<Option<PreparedLaunch>>,
    attempts: provenant_os::fs::Directory,
    scratch: provenant_os::fs::Directory,
    temporary: std::path::PathBuf,
    retained: bool,
}
impl Drop for PreparedStart {
    fn drop(&mut self) {
        if !self.retained {
            // No attempt record survives for retention to find the alias by,
            // so unlink it before its scratch and attempt directories go.
            crate::short_temporary::remove(&self.temporary);
            let name = self.attempt.to_string();
            let name = std::ffi::OsStr::new(&name);
            let _ = crate::artifacts::remove_entry_tree(&self.scratch, name);
            let _ = crate::artifacts::remove_entry_tree(&self.attempts, name);
        }
    }
}

fn output_prompt(directory: &std::path::Path, prompt: &str) -> String {
    format!(
        "Durable output for this attempt: {result} (your final answer); {exit}, {stdout}, {stderr} (harness evidence). These are the only durable outputs. Put your report and evidence in your final answer; the harness writes these files. Keep scratch files under the provided $TMPDIR, not in the worktree. Files in $TMPDIR are discarded when the attempt ends. Provider history is kept. The harness writes the attempt's own records, including the sealed handoff.json in the attempt directory, and the lane never writes them. Declare your result with `provenant declare --stdin` (schema v2 JSON; see STATUS_GUIDANCE). Field errors come back at once; fix and call again. The last accepted declaration settles the lane; prose and fenced blocks are not read. To stop early, declare `stopped_early` or `blocked`. Files left in $TMPDIR are not deliverables.\n\n{handoff}\n\n{prompt}",
        handoff = crate::completion::guidance(),
        result = directory.join("result.md").display(),
        exit = directory.join("process-exit.json").display(),
        stdout = directory.join("stdout.log").display(),
        stderr = directory.join("stderr.log").display(),
    )
}

#[cfg(test)]
#[path = "launch/output_prompt_tests.rs"]
mod output_prompt_tests;

/// Resolve policy once for this launch from host, project and dispatch settings.
pub fn launch_policy(
    state: &Path,
    project: &str,
    task: &TaskRequest,
) -> provenant_records::policy::Policy {
    provenant_config::policy::for_task(state, project, task)
}

fn check_launch_policy(
    providers: &dyn Providers,
    task: &TaskRequest,
    policy: &provenant_records::policy::Policy,
) -> std::result::Result<(), Box<PrepareRefusal>> {
    use provenant_records::policy::{Effect, Facts};
    let Err(refusal) = providers.check_policy(task) else {
        return Ok(());
    };
    // DiskFloor is emitted only by the shared critical-space decision.
    // Advisory pressure has already warned and continued at that core.
    if refusal.cause == CauseKind::DiskFloor {
        return Err(refusal);
    }
    let owned = refusal.policy_code.clone();
    let code = if let Some(code) = owned.as_deref() {
        code
    } else {
        match refusal.boundary_class {
            provenant_records::policy::Boundary::Credentials => {
                "PREPARATION_PREPARE_WITH_CREDENTIALS"
            }
            provenant_records::policy::Boundary::OtherWork => {
                "VALIDATION_VERIFY_WORKTREE_AUTHORITY_GIT_ISOLATION"
            }
            provenant_records::policy::Boundary::Operation => {
                "PREPARATION_PREPARE_CANDIDATES_DATA_INTEGRITY"
            }
            provenant_records::policy::Boundary::Authority => {
                "LAUNCH_RUNSTORE_START_INNER_AUTHORITY"
            }
            provenant_records::policy::Boundary::None
                if matches!(refusal.cause, CauseKind::DiskObservationUnavailable) =>
            {
                "SUBMIT_CHECK_WRITER_DISK_WITH_COLLECTION_NONE"
            }
            // A generic refusal with no boundary is not evidence of disk pressure.
            // Keep its typed outcome instead of letting a disk knob authorise it.
            provenant_records::policy::Boundary::None => return Err(refusal),
        }
    };
    if refusal.boundary_class != provenant_records::policy::Boundary::None {
        return Err(policy.boundary_error(code, Facts::violation(&refusal.message), refusal));
    }
    match policy.check(code, Facts::violation(&refusal.message)) {
        Effect::Proceed => Ok(()),
        Effect::Warn(message) => {
            use provenant_records::warning::warn;
            // Spec: docs/specs/cli.md#warnings-are-typed-values
            warn(*message);
            Ok(())
        }
        Effect::Refuse(_) | Effect::Stop(_) => Err(refusal),
    }
}

fn check_preparation_refusal(
    policy: &provenant_records::policy::Policy,
    refusal: Box<PrepareRefusal>,
) -> Box<PrepareRefusal> {
    use provenant_records::policy::Boundary;
    let class = refusal.boundary_class;
    if class == Boundary::None {
        return refusal;
    }
    let fallback = match class {
        Boundary::Credentials => Some("PREPARATION_PREPARE_WITH_CREDENTIALS"),
        Boundary::OtherWork => Some("VALIDATION_VERIFY_WORKTREE_AUTHORITY_GIT_ISOLATION"),
        Boundary::Operation => Some("PREPARATION_PREPARE_CANDIDATES_DATA_INTEGRITY"),
        Boundary::Authority => Some("PREPARATION_PREPARE_CANDIDATES_AUTHORITY"),
        Boundary::None => None,
    };
    let owned = refusal.policy_code.clone();
    let Some(code) = owned.as_deref().or(fallback) else {
        return refusal;
    };
    policy.boundary_error(
        code,
        provenant_records::policy::Facts::violation(&refusal.message),
        refusal,
    )
}

impl RunStore {
    /// Persist final plan facts for the launcher that already claimed
    /// Queued -> Starting before any external admission or provider work.
    #[allow(clippy::too_many_arguments)]
    fn claim_launch(
        &self,
        expected: &AttemptRecord,
        plan: &PreparedLaunch,
        grants: &admission::AppliedWriteGrants,
        priority: u8,
        limits: &provenant_records::LaneLimits,
        call: &Request<'_>,
        output: &mut impl std::io::Write,
    ) -> Result<AttemptRecord> {
        let writable = plan
            .confinement
            .as_ref()
            .and_then(|confinement| confinement.plan.permissions().read_only.as_ref())
            .map(|grants| grants.writable_paths())
            .unwrap_or_default();
        let record = self.record_starting_launch(
            expected,
            &plan.route,
            plan.continuation_expectation.as_ref(),
            writable,
            grants,
            plan.observed_head,
            priority,
            limits,
            call,
        )?;
        if let Some(policy) = plan
            .confinement
            .as_ref()
            .and_then(|confinement| confinement.plan.permissions().policy.as_ref())
        {
            let _ = policy.report_launch(output);
        }
        Ok(record)
    }

    pub fn effective_launch_task(
        &self,
        request: &CanonicalRequest,
        task: &TaskRequest,
    ) -> TaskRequest {
        let mut task = task.clone();
        if (request.parent.is_some() && request.parent_attempt.is_some())
            || self.context.state_root.as_path() != self.writer_registry()
        {
            // A sealed contributor cannot add a capability through a rewritten
            // request. Database access needs a host-validated declaration.
            task.access.database_ipc = false;
            let bound = self.sealed_parent_network.unwrap_or(NetworkAccess::Open);
            let reach = |network| match network {
                NetworkAccess::Provider => 0,
                NetworkAccess::Local => 1,
                NetworkAccess::Open => 2,
            };
            if reach(task.access.network(task.writer.is_some())) > reach(bound) {
                task.access.network = Some(bound);
            }
        }
        task
    }
    /// Seal the parent's user-enabled sandbox evidence for the shared
    /// admission-floor policy. It is a fact, not an authority boundary; warn
    /// or strict is decided from the request's sealed knob at preparation.
    pub(crate) fn seal_parent_sandbox(
        &self,
        request: &CanonicalRequest,
        task: &mut TaskRequest,
        call: &Request<'_>,
    ) {
        if let Some(parent) = request.parent_attempt
            && self.attempt_floors_children(parent, call)
        {
            task.profile.parent_sandboxed = true;
        }
    }
    pub(crate) fn prepare_fresh_start(
        &self,
        input: &PreparedRun,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<PreparedStart> {
        let mut effective = self.effective_launch_task(&input.request, &input.request.task);
        self.seal_parent_sandbox(&input.request, &mut effective, call);
        let task = &effective;
        let attempts =
            crate::artifacts::subdir(&self.root, std::ffi::OsStr::new("attempts"), true)?;
        // Exclusive creation makes cleanup safe even if admission races a replay.
        attempts.mkdir(std::ffi::OsStr::new(&input.attempt_id.to_string()), 0o700)?;
        let scratch_root = crate::artifacts::subdir(
            &self.root,
            std::ffi::OsStr::new(crate::plan::ATTEMPT_SCRATCH),
            true,
        )?;
        let scratch = crate::artifacts::subdir(
            &scratch_root,
            std::ffi::OsStr::new(&input.attempt_id.to_string()),
            true,
        )?;
        let temporary =
            crate::plan::scratch_root(self.context.state_root.as_path(), input.attempt_id)
                .join("tmp");
        let mut ready = PreparedStart {
            task: task.clone(),
            attempt: input.attempt_id,
            plan: std::cell::RefCell::new(None),
            attempts,
            scratch: scratch_root,
            temporary: temporary.clone(),
            retained: false,
        };
        ready.attempts.sync()?;
        let directory = self.attempt_dir(input.attempt_id, true)?;
        crate::artifacts::subdir(&directory, std::ffi::OsStr::new("provider-state"), true)?;
        let tmp = crate::artifacts::subdir(&scratch, std::ffi::OsStr::new("tmp"), true)?;
        crate::artifacts::subdir(&tmp, std::ffi::OsStr::new("scratch"), true)?;
        let base = self
            .context
            .state_root
            .as_path()
            .join(format!("attempts/{}", input.attempt_id));
        let temporary_alias = crate::short_temporary::ensure(&temporary);
        let paths = LaunchPaths {
            attempt_id: input.attempt_id,
            operation_id: input.operation_id.clone(),
            task_index: 0,
            lane_depth: input.request.lane_depth.unwrap_or(1),
            deadline_unix_ms: u64::try_from(
                call.clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))?
                    .as_micros()
                    / 1000,
            )
            .unwrap_or(0)
            .saturating_add(task.provider_timeout_seconds.saturating_mul(1000)),
            scratch: temporary.join("scratch"),
            provider_state: base.join("provider-state"),
            temporary,
            temporary_alias,
        };
        let prompt = output_prompt(&base, &input.prompt);
        let policy = launch_policy(
            &self.writer_registry,
            &self.context.project_id.to_string(),
            task,
        );
        let plan = check_launch_policy(providers, task, &policy)
            .map_err(|refusal| RunError::ContinuationRefused { refusal })
            .and_then(|()| {
                providers
                    .prepare(task, &prompt, &paths, None)
                    .map_err(|refusal| RunError::ContinuationRefused { refusal })
            })
            .and_then(|mut plan| {
                let limit = providers.write_grant_limit(&policy).checked(&policy);
                applied_write_grants(&plan, limit, &policy).map_err(|error| RunError::Input {
                    kind: ErrorKind::ContinuationUnsupported,
                    message: error.to_string(),
                })?;
                plan.route.write_grant_limit = Some(limit.receipt());
                Ok(plan)
            })?;
        *ready.plan.get_mut() = Some(plan);
        Ok(ready)
    }

    pub(crate) fn retain_prepared_start(&self, ready: &mut PreparedStart, attempt: AttemptId) {
        ready.retained = ready.attempt == attempt;
    }
}
/// The host-selected cap and its source, carried into each applied receipt.
#[derive(Clone, Copy, Debug)]
pub struct WriteGrantLimit {
    pub bytes: u64,
    pub origin: provenant_records::diagnostic::Layer,
}
impl Default for WriteGrantLimit {
    fn default() -> Self {
        Self {
            bytes: provenant_records::run::MAX_WRITE_GRANT_BYTES,
            origin: provenant_records::diagnostic::Layer::Default,
        }
    }
}
impl WriteGrantLimit {
    fn checked(self, policy: &provenant_records::policy::Policy) -> Self {
        if (1..=provenant_records::run::MAX_WRITE_GRANT_BYTES_CEILING).contains(&self.bytes) {
            self
        } else {
            if let provenant_records::policy::Effect::Warn(message) = policy.check(
                "WRITE_GRANT_LIMIT_INVALID_NONE",
                provenant_records::policy::Facts::violation(format!(
                    "domains.run.write_grant_limit_bytes={} has source {}; using compiled default",
                    self.bytes,
                    self.origin.description()
                )),
            ) {
                use provenant_records::warning::warn;
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(*message);
            }
            Self::default()
        }
    }
    fn receipt(self) -> provenant_records::run::WriteGrantLimitReceipt {
        use provenant_records::run::WriteGrantLimitOrigin;
        provenant_records::run::WriteGrantLimitReceipt {
            key: "domains.run.write_grant_limit_bytes".into(),
            value: self.bytes,
            origin: match self.origin {
                provenant_records::diagnostic::Layer::Host => WriteGrantLimitOrigin::Configuration,
                provenant_records::diagnostic::Layer::Default => {
                    WriteGrantLimitOrigin::CompiledDefault
                }
                // Only shipped-default and configured values reach this receipt.
                _ => WriteGrantLimitOrigin::Configuration,
            },
            extra: Default::default(),
        }
    }
}

pub trait Providers {
    /// Host cap for the encoded applied write-grant ledger.
    fn write_grant_limit(&self, _policy: &provenant_records::policy::Policy) -> WriteGrantLimit {
        WriteGrantLimit::default()
    }
    /// Host-selected minimum nice value for this lane.
    fn priority(&self, task: &TaskRequest) -> std::result::Result<u8, String> {
        Ok(provenant_records::run::default_lane_priority(
            task.writer.is_some(),
            task.build_heavy,
        ))
    }
    /// Host policy checks before provider preparation. A refusal is task-local;
    /// no provider is prepared and siblings remain eligible to launch.
    fn check_policy(&self, _task: &TaskRequest) -> std::result::Result<(), Box<PrepareRefusal>> {
        Ok(())
    }
    /// Prepare one task. An error is a
    /// task-local refusal reason; siblings still launch. `resume` is the prior
    /// terminal attempt of a continuation, whose exact session must be used:
    /// without one, refuse rather than start a fresh session.
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, Box<PrepareRefusal>>;
    /// A recognised diagnostic from the sealed stderr of a failed provider.
    fn failure_diagnostic(&self, _route: &RouteRecord, _stderr: &[u8]) -> Option<String> {
        None
    }
    /// Decode the sealed stdout capture of a provider that exited.
    fn decode(
        &self,
        route: &RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
        provider_state: &std::path::Path,
    ) -> std::result::Result<Decoded, String>;
    /// Decode the sealed captures: the single production entry point. `stderr`
    /// is extra evidence for providers that report limits there, and `context`
    /// carries the host-sealed roots used by the guardian.
    fn decode_captures(
        &self,
        route: &RouteRecord,
        stdout: &[u8],
        _stderr: &[u8],
        exit: &ExitObservation,
        provider_state: &std::path::Path,
        _context: Option<&crate::HolderContext>,
    ) -> std::result::Result<Decoded, String> {
        self.decode(route, stdout, exit, provider_state)
    }
    /// Decode captures with the retained session identity bound to a resumed
    /// provider invocation. Existing decoders remain valid for fresh attempts.
    fn decode_captures_with_continuation(
        &self,
        route: &RouteRecord,
        stdout: &[u8],
        stderr: &[u8],
        exit: &ExitObservation,
        provider_state: &std::path::Path,
        continuation: ContinuationDecodeContext<'_>,
    ) -> std::result::Result<Decoded, String> {
        if continuation.expectation.is_some() {
            return Ok(Decoded {
                cause: None,
                outcome: Outcome::Unsettled,
                result: String::new(),
                complete: false,
                observed_route: None,
                usage: UsageRecord::default(),
                session: None,
                reason: Some(
                    "provider decoder cannot verify the retained continuation session".into(),
                ),
                signals: Vec::new(),
            });
        }
        self.decode_captures(
            route,
            stdout,
            stderr,
            exit,
            provider_state,
            continuation.context,
        )
    }
}
pub(crate) enum HandOff {
    /// Failed before anything was sent to a guardian.
    Local(String),
    /// A typed boundary refused the prepared handoff.
    Boundary {
        message: String,
        class: provenant_records::policy::Boundary,
        cause: Option<CauseKind>,
    },
    Resource {
        message: String,
        fact: provenant_records::diagnostic::ResourceFact,
    },
    TypedLocal {
        message: String,
        cause: CauseKind,
    },
    Busy,
    /// No guardian was contacted, but custody remains protected.
    Protected(String),
    RetainedImport {
        fence: Box<provenant_supervisor::custody::PendingImport>,
        worktree: std::path::PathBuf,
    },
    /// The guardian may have received the launch.
    Uncertain(String),
}

/// The typed pre-provider evidence a handoff carries into withdrawal. A
/// Git-setup preparation refusal is local setup, not an adapter failure: its
/// typed failure names the step to repair and its note names the remedy implied
/// by the host's bounded reason. Other local refusals keep the adapter-failure
/// failure fact and own their note through the refusal kind.
pub(crate) fn handoff_preparation(
    error: &HandOff,
    reason: &str,
) -> (
    Option<provenant_records::run::PreparationFailure>,
    Option<String>,
) {
    match error {
        HandOff::TypedLocal {
            cause: CauseKind::PreparationRefused,
            ..
        } => (
            Some(provenant_records::run::PreparationFailure::GitSetup),
            Some(CauseKind::preparation_note(reason)),
        ),
        HandOff::Local(_) | HandOff::TypedLocal { .. } => (
            Some(provenant_records::run::PreparationFailure::AdapterUnavailable),
            None,
        ),
        _ => (None, None),
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Started {
    Launched(AttemptId),
    Refused {
        attempt: AttemptId,
        reason: String,
        kind: RefusalKind,
    },
    /// Repository admission was unavailable; release proof controls retry eligibility.
    Skipped {
        attempt: AttemptId,
        reason: String,
    },
    /// Handoff did not confirm; the attempt stays fenced for `reconcile`.
    Unknown {
        attempt: AttemptId,
        error: String,
    },
    /// Preparation failed and custody must be recovered before retrying.
    Blocked {
        attempt: AttemptId,
        error: String,
    },
}

pub(crate) fn mark_admission_release_pending(record: &mut AttemptRecord) -> Result<()> {
    let Some(binding) = record.repository_admission_binding.as_mut() else {
        return Ok(());
    };
    if !binding.valid() {
        return Err(RunError::Validation(
            "stored repository admission binding digest",
        ));
    }
    if binding.phase != RepositoryAdmissionPhase::Released {
        binding.phase = RepositoryAdmissionPhase::ReleasePending;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RefusalKind {
    Policy,
    PolicyBoundary(provenant_records::policy::Boundary),
    PolicyResource(provenant_records::diagnostic::ResourceFact),
    Preparation,
    PreparationBoundary(provenant_records::policy::Boundary),
    PreparationResource(provenant_records::diagnostic::ResourceFact),
    PacingReserve,
    PacingReserveResource(provenant_records::diagnostic::ResourceFact),
    WorktreeBusy,
    PriorityUnavailable,
    StoredPromptUnreadable,
    CloudPlacementRemoved,
    InvalidWriteGrantPath,
    WriteGrantsLimit(provenant_records::diagnostic::ResourceFact),
    Handoff,
    HandoffBoundary(provenant_records::policy::Boundary),
    HandoffResource(provenant_records::diagnostic::ResourceFact),
    RetainedImport {
        fence: provenant_supervisor::custody::PendingImport,
        worktree: std::path::PathBuf,
    },
}

impl RefusalKind {
    /// Project an inventoried resource refusal without trusting source text.
    pub fn diagnostic(&self) -> Option<provenant_records::diagnostic::Diagnostic> {
        use provenant_records::diagnostic::{Diagnostic, ResourceFact};
        let fact = match self {
            Self::HandoffResource(fact)
            | Self::PolicyResource(fact)
            | Self::PreparationResource(fact)
            | Self::WriteGrantsLimit(fact)
            | Self::PacingReserveResource(fact) => fact,
            _ => return None,
        };
        let kind = match fact {
            ResourceFact::Disk { .. } => ErrorKind::DiskFloor,
            ResourceFact::ArgvPromptLimit { .. } => ErrorKind::Invalid,
            ResourceFact::WriteGrantLimit { .. } => ErrorKind::Limit,
            ResourceFact::ContinuationCopy { .. }
            | ResourceFact::DependencyPreparation { .. }
            | ResourceFact::LanePorts { .. }
            | ResourceFact::PacingReserve { .. } => ErrorKind::Refused,
            ResourceFact::GrantLimit { .. } | ResourceFact::ProfileLimit { .. } => {
                ErrorKind::Confinement
            }
            ResourceFact::MemoryPressure { .. } => ErrorKind::MemoryPressure,
            ResourceFact::Processes { .. } => ErrorKind::Processes,
            ResourceFact::LaneCap { .. } => ErrorKind::LaneCap,
            ResourceFact::ConfinementRule(..) => ErrorKind::Confinement,
        };
        Some(
            fact.project(Diagnostic::new(kind.code()).category(kind.category()))
                .fact(provenant_records::diagnostic::DiagnosticFact::Resource(
                    fact.clone(),
                )),
        )
    }

    /// Retain only the typed owner's safe detail and corrective action.
    fn terminal_note(&self) -> Option<String> {
        let diagnostic = self.diagnostic()?;
        let fields = [diagnostic.detail, diagnostic.next];
        Some(provenant_records::diagnostic::safe_text(
            &fields.into_iter().flatten().collect::<Vec<_>>().join("; "),
        ))
    }

    /// The boundary established by this kind alone. Generic preparation and
    /// handoff errors need their original cause to establish a boundary.
    pub fn boundary_class(&self) -> provenant_records::policy::Boundary {
        use provenant_records::policy::Boundary as B;
        match self {
            Self::HandoffBoundary(class)
            | Self::PreparationBoundary(class)
            | Self::PolicyBoundary(class) => *class,
            Self::WorktreeBusy | Self::InvalidWriteGrantPath => B::Authority,
            Self::RetainedImport { .. } => B::OtherWork,
            Self::StoredPromptUnreadable | Self::CloudPlacementRemoved => B::Authority,
            Self::Policy
            | Self::Preparation
            | Self::PacingReserve
            | Self::PacingReserveResource(_)
            | Self::PriorityUnavailable
            | Self::WriteGrantsLimit(_)
            | Self::Handoff
            | Self::HandoffResource(_)
            | Self::PreparationResource(_)
            | Self::PolicyResource(_) => B::None,
        }
    }
}

/// Check a canonical root against state, allowing installed releases for reads.
pub fn access_root_allowed(root: &std::path::Path, state: &std::path::Path, read: bool) -> bool {
    let releases = state.join("releases");
    let release = root.strip_prefix(&releases).ok().is_some_and(|relative| {
        matches!(relative.components().next(), Some(std::path::Component::Normal(digest))
            if digest.to_str().is_some_and(|text| provenant_records::SourceDigest::parse(text).is_ok()))
    });
    (read && release) || (!root.starts_with(state) && !state.starts_with(root))
}

impl RunStore {
    /// Refuse queued proposals without touching any launched provider.
    pub fn refuse_queued(
        &self,
        attempt: AttemptId,
        reason: &str,
        call: &Request<'_>,
    ) -> Result<()> {
        let ids: Vec<String> = {
            let mut query = self
                .db
                .prepare("SELECT id FROM attempts WHERE id=?1 AND project=?2 AND terminal=0")?;
            let rows = query.query_map(
                rusqlite::params![attempt.to_string(), self.context.project_id.to_string()],
                |row| row.get(0),
            )?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            if matches!(self.record(id, call)?.state, AttemptState::Queued { .. }) {
                self.withdraw_with_preparation_failure(
                    id,
                    crate::store::Withdrawal {
                        expected: AttemptState::Queued {
                            extra: Default::default(),
                        },
                        outcome: Outcome::Rejected,
                        reason,
                        finished_at: call
                            .clock
                            .wall_now()
                            .map_err(|_| RunError::Validation("wall clock"))?,
                        failure: None,
                        cause: None,
                        terminal_note: None,
                    },
                    call,
                )?;
            }
        }
        Ok(())
    }
    pub fn request(&self, attempt: AttemptId, call: &Request<'_>) -> Result<CanonicalRequest> {
        Ok(self.request_with_digest(attempt, call)?.0)
    }
    /// An observed request read that does not invoke `F_FULLFSYNC` disk flushes.
    pub fn request_observed(
        &self,
        attempt: AttemptId,
        call: &Request<'_>,
    ) -> Result<CanonicalRequest> {
        Ok(self.request_observed_with_digest(attempt, call)?.0)
    }
    /// The request and the digest of its stored bytes. Callers compare this
    /// digest, never a reserialised request, because its schema may have
    /// evolved since the run was written.
    pub(crate) fn request_with_digest(
        &self,
        attempt: AttemptId,
        call: &Request<'_>,
    ) -> Result<(CanonicalRequest, provenant_core::Digest)> {
        let raw = read_sealed(
            &self.attempt_dir(attempt, false)?,
            "request.json",
            RECORD_LIMIT,
        )?
        .ok_or(RunError::NotFound)?;
        let digest = provenant_records::stored_record_digest("run-request", 1, &raw)?;
        let stored = self.attempt_operation(attempt, call)?.request_digest;
        if digest != stored {
            return Err(RunError::artifact_mismatch(
                ArtifactCheck::RequestDigest,
                "request.json",
                Some(ArtifactValue::digest(stored)),
                Some(ArtifactValue::digest(digest)),
            ));
        }
        Ok((decode(&raw)?, digest))
    }
    pub(crate) fn request_observed_with_digest(
        &self,
        attempt: AttemptId,
        call: &Request<'_>,
    ) -> Result<(CanonicalRequest, provenant_core::Digest)> {
        let raw = read_sealed_observed(
            &self.attempt_dir(attempt, false)?,
            "request.json",
            RECORD_LIMIT,
        )?
        .ok_or(RunError::NotFound)?;
        let digest = provenant_records::stored_record_digest("run-request", 1, &raw)?;
        let stored = self.attempt_operation(attempt, call)?.request_digest;
        if digest != stored {
            return Err(RunError::artifact_mismatch(
                ArtifactCheck::RequestDigest,
                "request.json",
                Some(ArtifactValue::digest(stored)),
                Some(ArtifactValue::digest(digest)),
            ));
        }
        Ok((decode(&raw)?, digest))
    }
    /// Hand each queued attempt of `run` to its own guardian, which admits it
    /// and later commits its terminal record. `context` builds the opaque
    /// holder reference for one attempt and the requester it launches for.
    /// Only the host writes a top-level store, so its run record names the
    /// requester.
    pub fn start(
        &self,
        attempt: AttemptId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(
            attempt,
            providers,
            context,
            &|directory, scope, binding| {
                custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    call.lock_budget(),
                    call.cancel,
                    call.clock,
                )
            },
            None,
            call,
        )
    }
    pub fn start_checked(
        &self,
        attempt: AttemptId,
        expected: &CanonicalRequest,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(
            attempt,
            providers,
            context,
            &|directory, scope, binding| {
                custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    call.lock_budget(),
                    call.cancel,
                    call.clock,
                )
            },
            Some(expected.digest()?),
            call,
        )
    }
    /// Native observation seam for integration fixtures. The ordinary start
    /// path always uses supervisor's system observer and the store lock budget;
    /// that budget is spent only when the slot proves a release in progress.
    #[cfg(feature = "fixture")]
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_writer_lease(
        &self,
        attempt: AttemptId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_os::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        expected: Option<&CanonicalRequest>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(
            attempt,
            providers,
            context,
            acquire_writer,
            expected.map(CanonicalRequest::digest).transpose()?,
            call,
        )
    }
    fn admission_binding(
        &self,
        request: &CanonicalRequest,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> std::result::Result<(AttemptRecord, Option<RepositoryAdmissionBinding>), &'static str>
    {
        if let Some(binding) = &record.repository_admission_binding {
            if !binding.valid() {
                return Err("stored repository admission binding digest is invalid");
            }
            return Ok((record.clone(), Some(binding.clone())));
        }
        let Some(writer) = request.task.writer.as_ref() else {
            return Ok((record.clone(), None));
        };
        let ports =
            provenant_config::PortTable::read_for(Some(&writer.project), &self.writer_registry)
                .map_err(|_| "repository admission declaration is unavailable")?;
        let command = provenant_config::dispatch_port_backend(
            &ports.admission.backend,
            || None,
            Some,
            || None,
        );
        let Some(command) = command else {
            return Ok((record.clone(), None));
        };
        let binding = RepositoryAdmissionBinding::new(ports.admission.source, command);
        let updated = self
            .record_admission_binding(record, binding, call)
            .map_err(|_| "repository admission binding could not be stored")?;
        let binding = updated
            .repository_admission_binding
            .clone()
            .ok_or("repository admission binding was not stored")?;
        if !binding.valid() {
            return Err("stored repository admission binding digest is invalid");
        }
        Ok((updated, Some(binding)))
    }

    fn run_admission_phase(
        &self,
        request: &CanonicalRequest,
        attempt: AttemptId,
        binding: &RepositoryAdmissionBinding,
        phase: RepositoryAdmissionPhase,
        cancel: &provenant_core::CancelToken,
    ) -> bool {
        let Ok(executor) = provenant_os::exec::SystemExecutor::new(1) else {
            return false;
        };
        self.run_admission_phase_with_executor(request, attempt, binding, phase, cancel, &executor)
    }

    fn run_admission_phase_with_executor(
        &self,
        request: &CanonicalRequest,
        attempt: AttemptId,
        binding: &RepositoryAdmissionBinding,
        phase: RepositoryAdmissionPhase,
        cancel: &provenant_core::CancelToken,
        executor: &dyn provenant_os::exec::Executor,
    ) -> bool {
        let Some(writer) = request.task.writer.as_ref() else {
            return false;
        };
        let Some(command_phase) = phase.command() else {
            return false;
        };
        let execution = if binding.valid() {
            let environment = [
                (
                    provenant_core::consumer_env::PORT_PHASE.into(),
                    command_phase.into(),
                ),
                (
                    provenant_core::consumer_env::ADMISSION_ATTEMPT.into(),
                    attempt.to_string().into(),
                ),
                (
                    provenant_core::consumer_env::ADMISSION_WORKTREE.into(),
                    writer.worktree.as_os_str().to_owned(),
                ),
                (
                    provenant_core::consumer_env::ADMISSION_PROJECT.into(),
                    writer.project.as_os_str().to_owned(),
                ),
                (
                    provenant_core::consumer_env::ADMISSION_OWNER_PID.into(),
                    std::process::id().to_string().into(),
                ),
                (
                    provenant_core::consumer_env::ADMISSION_EPOCH.into(),
                    binding.lease_epoch.to_string().into(),
                ),
            ];
            let timeout =
                std::time::Duration::from_secs(request.task.provider_timeout_seconds.max(1));
            let port = provenant_config::Port {
                source: binding.source,
                backend: provenant_config::PortBackend::Command {
                    command: binding.command.clone(),
                },
            };
            provenant_config::dispatch_port_backend(
                &port.backend,
                || provenant_config::PortCommandExecution::failed(&binding.command),
                |command| {
                    provenant_config::execute_port_command(
                        executor,
                        command,
                        &writer.project,
                        &[],
                        &environment,
                        timeout,
                        cancel,
                    )
                },
                || provenant_config::PortCommandExecution::failed(&binding.command),
            )
        } else {
            provenant_config::PortCommandExecution::failed(&binding.command)
        };
        let request_key = format!("{attempt}:{}", binding.lease_epoch);
        let evidence = provenant_config::record_port_command(
            &self.writer_registry,
            "admission",
            binding.source,
            command_phase,
            &request_key,
            &execution,
            provenant_config::PortReceiptLifetime::AttemptTerminalAndRelease {
                attempt_id: attempt,
                project_id: self.context.project_id,
                actor_id: self.context.actor_id,
                lease_epoch: binding.lease_epoch,
            },
        );
        let evidence_visible = match evidence {
            Ok(provenant_config::PortCommandPublication::Durable) => true,
            Ok(provenant_config::PortCommandPublication::VisibleDurabilityUnconfirmed {
                cause,
            }) => {
                use provenant_records::warning::{ADMISSION_RECEIPT_UNCONFIRMED, Warning, warn};
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(Warning::new(
                    ADMISSION_RECEIPT_UNCONFIRMED,
                    format!("admission receipt is visible but durability is unconfirmed: {cause}"),
                    provenant_records::warning::detected_source(ADMISSION_RECEIPT_UNCONFIRMED),
                ));
                true
            }
            Err(_) => false,
        };
        execution.disposition == provenant_config::PortCommandDisposition::Succeeded
            && evidence_visible
    }

    #[cfg(test)]
    fn admission_skip_with_phase(
        &self,
        request: &CanonicalRequest,
        record: &AttemptRecord,
        call: &Request<'_>,
        run_phase: &mut dyn FnMut(
            &CanonicalRequest,
            AttemptId,
            &RepositoryAdmissionBinding,
            RepositoryAdmissionPhase,
            &provenant_core::CancelToken,
        ) -> bool,
    ) -> (AttemptRecord, Option<String>) {
        let (record, binding) = match self.admission_binding(request, record, call) {
            Ok(value) => value,
            Err(_) => {
                return (
                    record.clone(),
                    Some("repository admission configuration or binding is unavailable; attempt remains queued".into()),
                );
            }
        };
        self.admission_skip_with_phase_bound(request, &record, binding, call, run_phase)
    }

    fn admission_skip_with_phase_bound(
        &self,
        request: &CanonicalRequest,
        record: &AttemptRecord,
        binding: Option<RepositoryAdmissionBinding>,
        call: &Request<'_>,
        run_phase: &mut dyn FnMut(
            &CanonicalRequest,
            AttemptId,
            &RepositoryAdmissionBinding,
            RepositoryAdmissionPhase,
            &provenant_core::CancelToken,
        ) -> bool,
    ) -> (AttemptRecord, Option<String>) {
        let mut record = record.clone();
        let Some(mut binding) = binding else {
            return (record, None);
        };
        if !matches!(
            record.state,
            AttemptState::Queued { .. } | AttemptState::Starting { .. }
        ) {
            return (
                record,
                Some("attempt state changed during repository admission".into()),
            );
        }
        if binding.phase == RepositoryAdmissionPhase::Released {
            match self.record_admission_phase(
                &record,
                RepositoryAdmissionPhase::AcquirePending,
                call,
            ) {
                Ok(updated) => {
                    record = updated;
                    binding = match self.admission_binding(request, &record, call) {
                        Ok((_, Some(binding))) => binding,
                        _ => {
                            return (
                                record,
                                Some("repository admission binding could not be reloaded".into()),
                            );
                        }
                    };
                }
                Err(_) => {
                    record = self.record(record.attempt_id, call).unwrap_or(record);
                    return (
                        record,
                        Some("repository admission state changed; attempt remains queued".into()),
                    );
                }
            }
        }
        if binding.phase == RepositoryAdmissionPhase::Acquired {
            return (record, None);
        }
        if binding.phase == RepositoryAdmissionPhase::ReleasePending {
            let released = self.release_admission_with_phase(&record, call, run_phase);
            if !released {
                use provenant_records::warning::{ADMISSION_RELEASE_PENDING, Warning, warn};
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(Warning::new(
                    ADMISSION_RELEASE_PENDING,
                    "repository admission release remains pending; attempt remains queued",
                    provenant_records::warning::detected_source(ADMISSION_RELEASE_PENDING),
                ));
            }
            return (
                self.record(record.attempt_id, call).unwrap_or(record),
                Some("repository admission cleanup is pending; attempt remains queued".into()),
            );
        }

        let cancelled_before_acquire = call.cancel.is_cancelled()
            || self
                .attempt_dir(record.attempt_id, false)
                .is_ok_and(|dir| crate::store::cancel_requested(&dir, Self::scope(&record)));
        if cancelled_before_acquire {
            return (
                record,
                Some("cancellation requested before repository admission".into()),
            );
        }
        let acquired = run_phase(
            request,
            record.attempt_id,
            &binding,
            RepositoryAdmissionPhase::AcquirePending,
            call.cancel,
        );
        let current = if acquired {
            match self.record_admission_phase(&record, RepositoryAdmissionPhase::Acquired, call) {
                Ok(updated) => return (updated, None),
                Err(_) => self.record(record.attempt_id, call).unwrap_or(record),
            }
        } else {
            self.record(record.attempt_id, call).unwrap_or(record)
        };
        let current_phase = current
            .repository_admission_binding
            .as_ref()
            .map(|binding| binding.phase);
        if current_phase == Some(RepositoryAdmissionPhase::Acquired)
            && matches!(
                current.state,
                AttemptState::Queued { .. } | AttemptState::Starting { .. }
            )
        {
            return (current, None);
        }
        if acquired {
            let released = self.release_admission_with_phase(&current, call, run_phase);
            if !released {
                use provenant_records::warning::{ADMISSION_RELEASE_PENDING, Warning, warn};
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(Warning::new(
                    ADMISSION_RELEASE_PENDING,
                    "repository admission release failed after a concurrent terminal change",
                    provenant_records::warning::detected_source(ADMISSION_RELEASE_PENDING),
                ));
            }
            return (
                self.record(current.attempt_id, call).unwrap_or(current),
                Some("attempt state changed during repository admission".into()),
            );
        }
        let current = match self.record_admission_phase(
            &current,
            RepositoryAdmissionPhase::ReleasePending,
            call,
        ) {
            Ok(updated) => updated,
            Err(_) => self.record(current.attempt_id, call).unwrap_or(current),
        };
        let released = self.release_admission_with_phase(&current, call, run_phase);
        if !released {
            use provenant_records::warning::{ADMISSION_RELEASE_PENDING, Warning, warn};
            // Spec: docs/specs/cli.md#warnings-are-typed-values
            warn(Warning::new(
                ADMISSION_RELEASE_PENDING,
                "repository admission release failed after an unsuccessful acquire",
                provenant_records::warning::detected_source(ADMISSION_RELEASE_PENDING),
            ));
        }
        (
            self.record(current.attempt_id, call).unwrap_or(current),
            Some(
                "repository admission skipped this attempt; cleanup must complete before retry"
                    .into(),
            ),
        )
    }

    /// Release a frozen attempt binding after terminal publication. Failed
    /// release leaves the existing terminal retention marker set for reconcile.
    pub(crate) fn release_admission(&self, record: &AttemptRecord, call: &Request<'_>) -> bool {
        let released = self.release_admission_with_phase(
            record,
            call,
            &mut |request, attempt, binding, phase, cancel| {
                self.run_admission_phase(request, attempt, binding, phase, cancel)
            },
        );
        if !released && matches!(record.state, AttemptState::Terminal { .. }) {
            let _ = self.set_temporary_retained(record.attempt_id, true, call);
        }
        released
    }

    fn admission_release_receipt_confirmed(
        &self,
        attempt: AttemptId,
        binding: &RepositoryAdmissionBinding,
    ) -> bool {
        let key = format!("{attempt}:{}", binding.lease_epoch);
        provenant_config::port_command_receipt_proof(
            &self.writer_registry,
            "admission",
            binding.source,
            "release",
            &key,
            binding.command_digest,
            &binding.command.exit_codes.success,
        )
        .ok()
        .flatten()
        .is_some_and(|proof| proof.successful_exit)
    }

    fn release_admission_with_phase(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        run_phase: &mut dyn FnMut(
            &CanonicalRequest,
            AttemptId,
            &RepositoryAdmissionBinding,
            RepositoryAdmissionPhase,
            &provenant_core::CancelToken,
        ) -> bool,
    ) -> bool {
        let Some(mut binding) = record.repository_admission_binding.clone() else {
            return true;
        };
        if !binding.valid() {
            use provenant_records::warning::{ADMISSION_RELEASE_PENDING, Warning, warn};
            // Spec: docs/specs/cli.md#warnings-are-typed-values
            warn(Warning::new(
                ADMISSION_RELEASE_PENDING,
                "stored repository admission binding digest is invalid; release was not run",
                provenant_records::warning::detected_source(ADMISSION_RELEASE_PENDING),
            ));
            return false;
        }
        if binding.phase == RepositoryAdmissionPhase::Released {
            return true;
        }
        let mut current = match self.record(record.attempt_id, call) {
            Ok(current) => current,
            Err(_) => return false,
        };
        let Some(current_binding) = current.repository_admission_binding.clone() else {
            return false;
        };
        if !current_binding.valid() {
            return false;
        }
        binding = current_binding;
        if binding.phase == RepositoryAdmissionPhase::Released {
            return true;
        }
        if matches!(current.state, AttemptState::Terminal { .. })
            && binding.phase == RepositoryAdmissionPhase::ReleasePending
            && self.admission_release_receipt_confirmed(record.attempt_id, &binding)
        {
            return true;
        }
        if binding.phase != RepositoryAdmissionPhase::ReleasePending {
            match self.record_admission_phase(
                &current,
                RepositoryAdmissionPhase::ReleasePending,
                call,
            ) {
                Ok(updated) => current = updated,
                Err(_) => {
                    current = match self.record(record.attempt_id, call) {
                        Ok(current) => current,
                        Err(_) => return false,
                    };
                    let Some(current_binding) = current.repository_admission_binding.clone() else {
                        return false;
                    };
                    if !current_binding.valid() {
                        return false;
                    }
                    binding = current_binding;
                    if binding.phase == RepositoryAdmissionPhase::Released {
                        return true;
                    }
                    if binding.phase != RepositoryAdmissionPhase::ReleasePending {
                        return false;
                    }
                }
            }
            binding.phase = RepositoryAdmissionPhase::ReleasePending;
        }
        let request = match self.request(record.attempt_id, call) {
            Ok(request) => request,
            Err(_) => {
                use provenant_records::warning::{ADMISSION_RELEASE_PENDING, Warning, warn};
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(Warning::new(
                    ADMISSION_RELEASE_PENDING,
                    "repository admission release could not read the stored request",
                    provenant_records::warning::detected_source(ADMISSION_RELEASE_PENDING),
                ));
                return false;
            }
        };
        if !run_phase(
            &request,
            record.attempt_id,
            &binding,
            RepositoryAdmissionPhase::ReleasePending,
            &provenant_core::CancelToken::new(),
        ) {
            use provenant_records::warning::{ADMISSION_RELEASE_PENDING, Warning, warn};
            // Spec: docs/specs/cli.md#warnings-are-typed-values
            warn(Warning::new(
                ADMISSION_RELEASE_PENDING,
                "repository admission release failed; reconcile will retry the idempotent command",
                provenant_records::warning::detected_source(ADMISSION_RELEASE_PENDING),
            ));
            return false;
        }
        if matches!(current.state, AttemptState::Terminal { .. }) {
            return self.admission_release_receipt_confirmed(record.attempt_id, &binding);
        }
        match self.record_admission_phase(&current, RepositoryAdmissionPhase::Released, call) {
            Ok(_) => true,
            Err(_) => {
                let latest = match self.record(record.attempt_id, call) {
                    Ok(latest) => latest,
                    Err(_) => return false,
                };
                if matches!(latest.state, AttemptState::Terminal { .. }) {
                    return self.admission_release_receipt_confirmed(record.attempt_id, &binding);
                }
                let Some(latest_binding) = latest.repository_admission_binding.clone() else {
                    return false;
                };
                if !latest_binding.valid() {
                    return false;
                }
                latest_binding.phase == RepositoryAdmissionPhase::Released
                    || (latest_binding.phase == RepositoryAdmissionPhase::ReleasePending
                        && self
                            .record_admission_phase(
                                &latest,
                                RepositoryAdmissionPhase::Released,
                                call,
                            )
                            .is_ok())
            }
        }
    }

    fn settle_starting_refusal(
        &self,
        id: AttemptId,
        reason: &str,
        finished_at: provenant_core::WallTime,
        call: &Request<'_>,
    ) -> Result<()> {
        self.withdraw_with_preparation_failure(
            id,
            crate::store::Withdrawal {
                expected: AttemptState::Starting {
                    extra: Default::default(),
                },
                outcome: Outcome::Rejected,
                reason,
                finished_at,
                failure: None,
                cause: Some(CauseKind::LaunchHandoffRefused),
                terminal_note: None,
            },
            call,
        )?;
        Ok(())
    }

    fn cancel_preparation_if_requested(
        &self,
        id: AttemptId,
        record: &AttemptRecord,
        attempt_dir: &provenant_os::fs::Directory,
        finished_at: provenant_core::WallTime,
        call: &Request<'_>,
    ) -> Result<bool> {
        if !call.cancel.is_cancelled()
            && !crate::store::cancel_requested(attempt_dir, Self::scope(record))
        {
            return Ok(false);
        }
        // Cancellation stops preparation, but its owner must still settle and
        // release custody under a fresh bounded store request.
        let cleanup_cancel = provenant_core::CancelToken::new();
        let cleanup_call = Request {
            clock: call.clock,
            cancel: &cleanup_cancel,
        };
        self.withdraw_with_preparation_failure(
            id,
            crate::store::Withdrawal {
                expected: AttemptState::Starting {
                    extra: Default::default(),
                },
                outcome: Outcome::Cancelled,
                reason: "cancelled before launch",
                finished_at,
                failure: None,
                cause: None,
                terminal_note: None,
            },
            &cleanup_call,
        )?;
        Ok(true)
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "preparation diagnostics use the launch command's stderr output seam"
    )]
    fn start_inner(
        &self,
        attempt: AttemptId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_os::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        expected: Option<provenant_core::Digest>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        // Every later use reads this one value, never the store again.
        let request = self.request(attempt, call)?;
        let policy = launch_policy(
            &self.writer_registry,
            &self.context.project_id.to_string(),
            &request.task,
        );
        if let Some(digest) = expected
            && request.digest()? != digest
        {
            return Err(policy.boundary_error(
                "LAUNCH_RUNSTORE_START_INNER_AUTHORITY_2",
                provenant_records::policy::Facts::violation(
                    "stored request digest changed after launch validation",
                ),
                RunError::Validation("request changed after launch validation"),
            ));
        }
        let operation = self.attempt_operation(attempt, call)?;
        let requester = operation.actor_id;
        let resume = request
            .parent
            .filter(|_| request.fresh_continuation.is_none())
            .map(|prior| self.record(prior, call))
            .transpose()?;
        let ids: Vec<String> = {
            let mut query = self.db.prepare(
                "SELECT id FROM attempts WHERE id=?1 AND project=?2 AND terminal=0 ORDER BY sequence",
            )?;
            let rows = query.query_map(
                params![attempt.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut started = Vec::new();
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            let mut record = self.record(id, call)?;
            if !matches!(record.state, AttemptState::Queued { .. }) {
                continue;
            }
            let now = || {
                call.clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))
            };

            let task = &request.task;
            // A request stored before cloud placement was removed still names
            // `cloud`. Never hand it to the local supervisor; settle it so
            // status, reconcile and gc can clear the run.
            if task.extra.contains_key("cloud") {
                let cause = CauseKind::CloudPlacementRemoved;
                match self.withdraw_with_preparation_failure(
                    id,
                    crate::store::Withdrawal {
                        expected: AttemptState::Queued {
                            extra: Default::default(),
                        },
                        outcome: Outcome::Rejected,
                        reason: cause.detail(),
                        finished_at: now()?,
                        failure: None,
                        cause: Some(cause),
                        terminal_note: None,
                    },
                    call,
                ) {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(RunError::Conflict) => continue,
                    Err(error) => return Err(error),
                }
                started.push(Started::Refused {
                    attempt: id,
                    reason: cause.detail().to_owned(),
                    kind: RefusalKind::CloudPlacementRemoved,
                });
                continue;
            }
            if let Some(prior) = &resume
                && prior.attempt_id == record.attempt_id
                && !self.custody_released(prior, call)?
            {
                continue;
            }
            // Freeze the repository command while the attempt is still queued,
            // then publish Starting under the launcher's liveness lock before
            // running any repository command or provider preparation.
            let (bound_record, admission_binding) = match self
                .admission_binding(&request, &record, call)
            {
                Ok(value) => value,
                Err(reason) => {
                    use provenant_records::warning::{REPOSITORY_ADMISSION_SKIPPED, Warning, warn};
                    // Spec: docs/specs/cli.md#warnings-are-typed-values
                    warn(
                        Warning::new(
                            REPOSITORY_ADMISSION_SKIPPED,
                            format!("repository admission skipped {id}: {reason}"),
                            provenant_records::warning::detected_source(
                                REPOSITORY_ADMISSION_SKIPPED,
                            ),
                        )
                        .subject(format!("attempt {id}")),
                    );
                    started.push(Started::Skipped {
                        attempt: id,
                        reason: reason.to_owned(),
                    });
                    continue;
                }
            };
            record = bound_record;
            let attempt_dir = self.attempt_dir(id, true)?;
            let _launcher = match attempt_dir.try_lock(
                std::ffi::OsStr::new("launcher.lock"),
                provenant_os::fs::LockMode::Exclusive,
            ) {
                Ok(guard) => guard,
                Err(provenant_os::fs::FsError::LockTimeout { .. }) => continue,
                Err(error) => return Err(error.into()),
            };
            record = match self.claim_starting_for_preparation(&record, call) {
                Ok(record) => record,
                Err(RunError::Conflict) => continue,
                Err(error) => return Err(error),
            };
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let mut effective_task = self.effective_launch_task(&request, task);
            self.seal_parent_sandbox(&request, &mut effective_task, call);
            let task = &effective_task;
            let (admitted_record, skipped) = self.admission_skip_with_phase_bound(
                &request,
                &record,
                admission_binding,
                call,
                &mut |request, attempt, binding, phase, cancel| {
                    self.run_admission_phase(request, attempt, binding, phase, cancel)
                },
            );
            record = admitted_record;
            if let Some(reason) = skipped {
                if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                    started.push(Started::Refused {
                        attempt: id,
                        reason: "cancelled before launch".into(),
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
                match self.requeue_starting_after_admission_skip(&record, call) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        if self.cancel_preparation_if_requested(
                            id,
                            &record,
                            &attempt_dir,
                            now()?,
                            call,
                        )? {
                            started.push(Started::Refused {
                                attempt: id,
                                reason: "cancelled before launch".into(),
                                kind: RefusalKind::Preparation,
                            });
                            continue;
                        }
                        continue;
                    }
                    // A failed release keeps the Starting record fenced for
                    // reconcile; it is never re-enqueued while custody is unknown.
                    Err(RunError::Conflict) => {}
                    Err(error) => return Err(error),
                }
                use provenant_records::warning::{REPOSITORY_ADMISSION_SKIPPED, Warning, warn};
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(
                    Warning::new(
                        REPOSITORY_ADMISSION_SKIPPED,
                        format!("repository admission skipped {id}: {reason}"),
                        provenant_records::warning::detected_source(REPOSITORY_ADMISSION_SKIPPED),
                    )
                    .subject(format!("attempt {id}")),
                );
                started.push(Started::Skipped {
                    attempt: id,
                    reason,
                });
                continue;
            }
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let prompt = match self
                .artifact_bytes(&record.prompt, RECORD_LIMIT)
                .and_then(|bytes| {
                    String::from_utf8(bytes)
                        .map_err(|_| RunError::Validation("prompt is not UTF-8"))
                }) {
                Ok(prompt) => prompt,
                Err(error) => {
                    if self.cancel_preparation_if_requested(
                        id,
                        &record,
                        &attempt_dir,
                        now()?,
                        call,
                    )? {
                        started.push(Started::Refused {
                            attempt: id,
                            reason: "cancelled before launch".into(),
                            kind: RefusalKind::Preparation,
                        });
                        continue;
                    }
                    let code = if matches!(
                        &error,
                        RunError::ArtifactMismatch(_) | RunError::TooLarge { .. }
                    ) {
                        "ARTIFACTS_RUNSTORE_ARTIFACT_BYTES_DATA_INTEGRITY"
                    } else {
                        "ARTIFACTS_RUNSTORE_ARTIFACT_BYTES_AUTHORITY"
                    };
                    let error = policy.boundary_error(
                        code,
                        provenant_records::policy::Facts::violation(
                            "stored prompt could not be reopened and verified against the attempt record",
                        ),
                        error,
                    );
                    // The live preparation owner settles unreadable input; it
                    // must not leave a stale Starting row for another launcher.
                    let reason = format!("stored prompt unreadable: {error}");
                    match self.withdraw_with_preparation_failure(
                        id,
                        crate::store::Withdrawal {
                            expected: AttemptState::Starting {
                                extra: Default::default(),
                            },
                            outcome: Outcome::Rejected,
                            reason: &reason,
                            finished_at: now()?,
                            failure: None,
                            cause: Some(CauseKind::StoredPromptUnreadable),
                            terminal_note: None,
                        },
                        call,
                    ) {
                        Ok(Some(_)) => {}
                        Ok(None) | Err(RunError::Conflict) => continue,
                        Err(error) => return Err(error),
                    }
                    started.push(Started::Refused {
                        attempt: id,
                        reason,
                        kind: RefusalKind::StoredPromptUnreadable,
                    });
                    continue;
                }
            };
            let directory = self
                .context
                .state_root
                .as_path()
                .join(format!("attempts/{id}"));
            let prompt = output_prompt(&directory, &prompt);
            let staging = (|| -> Result<std::path::PathBuf> {
                crate::artifacts::subdir(
                    &attempt_dir,
                    std::ffi::OsStr::new("provider-state"),
                    true,
                )?;
                let scratch = self.attempt_scratch_dir(id, true)?;
                let tmp = crate::artifacts::subdir(&scratch, std::ffi::OsStr::new("tmp"), true)?;
                crate::artifacts::subdir(&tmp, std::ffi::OsStr::new("scratch"), true)?;
                Ok(crate::plan::scratch_root(self.context.state_root.as_path(), id).join("tmp"))
            })();
            let temporary = match staging {
                Ok(temporary) => temporary,
                Err(error) => {
                    let reason = provenant_core::bounded_line(
                        &format!("launch staging failed: {error}"),
                        240,
                    );
                    self.settle_starting_refusal(id, &reason, now()?, call)?;
                    started.push(Started::Refused {
                        attempt: id,
                        reason,
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
            };
            let base = self
                .context
                .state_root
                .as_path()
                .join("attempts")
                .join(id.to_string());
            let temporary_alias = crate::short_temporary::ensure(&temporary);
            let paths = LaunchPaths {
                attempt_id: id,
                operation_id: operation.operation_id.clone(),
                task_index: 0,
                lane_depth: request.lane_depth.unwrap_or(1),
                deadline_unix_ms: u64::try_from(now()?.as_micros() / 1000)
                    .unwrap_or(0)
                    .saturating_add(task.provider_timeout_seconds.saturating_mul(1000)),
                scratch: temporary.join("scratch"),
                provider_state: base.join("provider-state"),
                temporary,
                temporary_alias,
            };
            let priority = providers.priority(task).unwrap_or_else(|reason| {
                let fallback = provenant_records::run::default_lane_priority(task.writer.is_some(), task.build_heavy);
                use provenant_records::warning::{LANE_PRIORITY_FALLBACK, Warning, warn};
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(
                    Warning::new(
                        LANE_PRIORITY_FALLBACK,
                        format!("lane priority unavailable ({reason}); using compiled domains.fabric.lanes.priority default {fallback}"),
                     provenant_records::warning::detected_source(LANE_PRIORITY_FALLBACK))
                    .once_per_change(),
                );
                fallback
            });
            let launch_policy = check_launch_policy(providers, task, &policy);
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let prepared = match launch_policy {
                Ok(()) => providers
                    .prepare(task, &prompt, &paths, resume.as_ref())
                    .map(|plan| (plan, priority))
                    .map_err(|reason| {
                        let refusal = check_preparation_refusal(&policy, reason);
                        Box::new((refusal.kind(), refusal))
                    }),
                Err(refusal) => {
                    let kind = if let Some(fact) = refusal.resource.clone() {
                        RefusalKind::PolicyResource(fact)
                    } else if refusal.boundary_class != provenant_records::policy::Boundary::None {
                        RefusalKind::PolicyBoundary(refusal.boundary_class)
                    } else {
                        RefusalKind::Policy
                    };
                    Err(Box::new((kind, refusal)))
                }
            };
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let (mut plan, priority) = match prepared {
                Ok(prepared) => prepared,
                Err(refused) => {
                    if self.cancel_preparation_if_requested(
                        id,
                        &record,
                        &attempt_dir,
                        now()?,
                        call,
                    )? {
                        started.push(Started::Refused {
                            attempt: id,
                            reason: "cancelled before launch".into(),
                            kind: RefusalKind::Preparation,
                        });
                        continue;
                    }
                    let (kind, refusal) = *refused;
                    if !refusal.skipped_routes.is_empty()
                        && let Err(error) =
                            self.record_skipped_routes(id, &refusal.skipped_routes, call)
                    {
                        let reason = provenant_core::bounded_line(
                            &format!("launch preparation failed: {error}"),
                            240,
                        );
                        self.settle_starting_refusal(id, &reason, now()?, call)?;
                        started.push(Started::Refused {
                            attempt: id,
                            reason,
                            kind: RefusalKind::Preparation,
                        });
                        continue;
                    }
                    let outcome = if refusal.auth_required() {
                        Outcome::AuthRequired
                    } else {
                        Outcome::Rejected
                    };
                    let cause = Some(refusal.cause);
                    let continuation_reason = resume.as_ref().map(|_| refusal.safe_reason());
                    let terminal_note = match (kind.terminal_note(), refusal.terminal_note) {
                        (Some(guidance), Some(note)) if guidance != note => {
                            Some(format!("{note}; {guidance}"))
                        }
                        (Some(note), _) | (_, Some(note)) => Some(note),
                        (None, None) => match refusal.skipped_routes.first() {
                            Some(route) => {
                                let diagnostic = route.diagnostic();
                                Some(format!(
                                    "first candidate: {}; {}",
                                    diagnostic.detail.unwrap_or_default(),
                                    diagnostic.next.unwrap_or_default()
                                ))
                            }
                            None => None,
                        },
                    };
                    let terminal_note = if let Some(host_message) = refusal.host_message.as_deref()
                    {
                        // A pacing refusal names its route, account, window and
                        // sourced setting; the usual short summary truncates those facts.
                        let limit = if matches!(
                            &refusal.resource,
                            Some(provenant_records::diagnostic::ResourceFact::PacingReserve { .. })
                        ) || refusal.skipped_routes.iter().any(|route| {
                            route.cause
                                == Some(provenant_records::RouteExclusionReason::PacingReserve)
                        }) {
                            4096
                        } else {
                            160
                        };
                        let host_message = provenant_core::bounded_line(host_message, limit);
                        Some(match terminal_note {
                            Some(note) => format!("{host_message}; {note}"),
                            None => host_message,
                        })
                    } else {
                        terminal_note
                    };
                    let terminal_note = if continuation_reason.is_some() {
                        continuation_reason.clone()
                    } else {
                        terminal_note
                    };
                    let reason = continuation_reason.unwrap_or(refusal.message);
                    match self.withdraw_with_preparation_failure(
                        id,
                        crate::store::Withdrawal {
                            expected: AttemptState::Starting {
                                extra: Default::default(),
                            },
                            outcome,
                            reason: &reason,
                            finished_at: now()?,
                            failure: refusal.fallback_outcome,
                            cause,
                            terminal_note,
                        },
                        call,
                    ) {
                        Ok(Some(_)) => crate::writer::settle_launch_staging(&attempt_dir)?,
                        Ok(None) => continue,
                        // The attempt changed while this preparation owner was
                        // settling its refusal.
                        Err(RunError::Conflict) => continue,
                        Err(error) => return Err(error),
                    }
                    started.push(Started::Refused {
                        attempt: id,
                        reason,
                        kind,
                    });
                    continue;
                }
            };
            let sandbox_binding = plan.bind_sandbox_backend(call.cancel, &policy);
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            if let Err(error) = sandbox_binding {
                if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                    started.push(Started::Refused {
                        attempt: id,
                        reason: "cancelled before launch".into(),
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
                let reason = provenant_core::bounded_line(
                    &format!("launch preparation failed: {error}"),
                    240,
                );
                self.settle_starting_refusal(id, &reason, now()?, call)?;
                started.push(Started::Refused {
                    attempt: id,
                    reason,
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let limits = plan.limits.clone();
            set_lane_environment(
                &mut plan.environment,
                request.lane_depth.unwrap_or(1),
                id,
                &plan.route,
            );
            let limit = providers.write_grant_limit(&policy).checked(&policy);
            plan.route.write_grant_limit = Some(limit.receipt());
            let write_grants = applied_write_grants(&plan, limit, &policy);
            let write_grants = match write_grants {
                Ok(grants) => grants,
                Err(error) => {
                    if self.cancel_preparation_if_requested(
                        id,
                        &record,
                        &attempt_dir,
                        now()?,
                        call,
                    )? {
                        started.push(Started::Refused {
                            attempt: id,
                            reason: "cancelled before launch".into(),
                            kind: RefusalKind::Preparation,
                        });
                        continue;
                    }
                    let reason = error.to_string();
                    let kind = match error {
                        WriteGrantError::InvalidPath => RefusalKind::InvalidWriteGrantPath,
                        WriteGrantError::Policy(fact) => RefusalKind::WriteGrantsLimit(fact),
                    };
                    match self.withdraw_with_preparation_failure(
                        id,
                        crate::store::Withdrawal {
                            expected: AttemptState::Starting {
                                extra: Default::default(),
                            },
                            outcome: Outcome::Rejected,
                            reason: &reason,
                            finished_at: now()?,
                            failure: None,
                            cause: Some(CauseKind::WriteGrantsRefused),
                            terminal_note: kind.terminal_note(),
                        },
                        call,
                    ) {
                        Ok(Some(_)) => crate::writer::settle_launch_staging(&attempt_dir)?,
                        Ok(None) => continue,
                        // Another launcher or cancellation changed the attempt
                        // while write grants were being recorded.
                        Err(RunError::Conflict) => continue,
                        Err(error) => return Err(error),
                    }
                    started.push(Started::Refused {
                        attempt: id,
                        reason,
                        kind,
                    });
                    continue;
                }
            };
            if !plan.skipped_routes.is_empty()
                && let Err(error) = self.record_skipped_routes(id, &plan.skipped_routes, call)
            {
                let reason = provenant_core::bounded_line(
                    &format!("launch preparation failed: {error}"),
                    240,
                );
                self.settle_starting_refusal(id, &reason, now()?, call)?;
                started.push(Started::Refused {
                    attempt: id,
                    reason,
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            record = self.record(id, call)?;
            let record = match self.claim_launch(
                &record,
                &plan,
                &write_grants,
                priority,
                &limits,
                call,
                &mut std::io::stderr().lock(),
            ) {
                Ok(record) => record,
                // A concurrent start already took this attempt.
                Err(RunError::Conflict) => {
                    if self.cancel_preparation_if_requested(
                        id,
                        &record,
                        &attempt_dir,
                        now()?,
                        call,
                    )? {
                        started.push(Started::Refused {
                            attempt: id,
                            reason: "cancelled before launch".into(),
                            kind: RefusalKind::Preparation,
                        });
                    }
                    continue;
                }
                Err(error) => {
                    let reason = provenant_core::bounded_line(
                        &format!("launch preparation failed: {error}"),
                        240,
                    );
                    self.settle_starting_refusal(id, &reason, now()?, call)?;
                    started.push(Started::Refused {
                        attempt: id,
                        reason,
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
            };
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            if let Err(error) = self.seal_work(&record, &plan, call) {
                if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                    started.push(Started::Refused {
                        attempt: id,
                        reason: "cancelled before launch".into(),
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
                let reason = format!("launch preparation failed: {error}");
                self.settle_starting_refusal(id, &reason, now()?, call)?;
                started.push(Started::Refused {
                    attempt: id,
                    reason,
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            started.push(
                match self.hand_off(
                    task,
                    plan,
                    requester,
                    context,
                    acquire_writer,
                    &record,
                    &limits,
                    call,
                ) {
                    Ok(()) => Started::Launched(id),
                    // Nothing reached a guardian: the attempt ends here.
                    Err(
                        error @ (HandOff::Local(_)
                        | HandOff::Boundary { .. }
                        | HandOff::Resource { .. }
                        | HandOff::TypedLocal { .. }
                        | HandOff::Busy
                        | HandOff::RetainedImport { .. }),
                    ) => {
                        // These handoff variants are returned only after the
                        // local lease has been withdrawn. Preserve that typed
                        // pre-provider failure for graph fallback; a boundary or
                        // retained import does not establish safe preparation.
                        let cause = match &error {
                            HandOff::TypedLocal { cause, .. } => Some(*cause),
                            HandOff::Boundary { cause, .. } => {
                                cause.or(Some(CauseKind::LaunchHandoffRefused))
                            }
                            HandOff::Busy => Some(CauseKind::WorktreeBusy),
                            HandOff::RetainedImport { .. } => Some(CauseKind::RetainedWriterImport),
                            _ => Some(CauseKind::LaunchHandoffRefused),
                        };
                        let (reason, kind) = match &error {
                            HandOff::Busy => ("worktree is busy".into(), RefusalKind::WorktreeBusy),
                            HandOff::Local(error) | HandOff::TypedLocal { message: error, .. } => (
                                format!("launch preparation failed: {error}"),
                                RefusalKind::Handoff,
                            ),
                            HandOff::Boundary { message, class, .. } => (
                                format!("launch preparation failed: {message}"),
                                RefusalKind::HandoffBoundary(*class),
                            ),
                            HandOff::Resource { message, fact } => (
                                format!("launch preparation failed: {message}"),
                                RefusalKind::HandoffResource(fact.clone()),
                            ),
                            HandOff::RetainedImport { fence, worktree } => (
                                RunError::RetainedWriterImport {
                                    fence: fence.clone(),
                                    worktree: worktree.clone(),
                                }
                                .to_string(),
                                RefusalKind::RetainedImport {
                                    fence: (**fence).clone(),
                                    worktree: worktree.clone(),
                                },
                            ),
                            _ => unreachable!(),
                        };
                        // The terminal cause retains the independent failure,
                        // but cancellation must not authorise route fallback.
                        let cancelled = self.attempt_dir(id, false).is_ok_and(|attempt| {
                            crate::store::cancel_requested(&attempt, Self::scope(&record))
                        });
                        let (outcome, reason) = if cancelled {
                            (
                                Outcome::Cancelled,
                                format!("cancelled before launch; {reason}"),
                            )
                        } else {
                            (Outcome::Rejected, reason.clone())
                        };
                        let (preparation_failure, preparation_note) =
                            handoff_preparation(&error, &reason);
                        match self.withdraw_with_preparation_failure(
                            id,
                            crate::store::Withdrawal {
                                expected: AttemptState::Starting {
                                    extra: Default::default(),
                                },
                                outcome,
                                reason: &reason,
                                finished_at: now()?,
                                failure: preparation_failure.filter(|_| !cancelled),
                                cause,
                                terminal_note: preparation_note.or_else(|| kind.terminal_note()),
                            },
                            call,
                        ) {
                            // Cancel already settled it from the other side.
                            Ok(_) | Err(RunError::Conflict) if cancelled => {}
                            Ok(_) => {}
                            Err(error) => return Err(error),
                        }
                        Started::Refused {
                            attempt: id,
                            reason,
                            kind,
                        }
                    }
                    Err(HandOff::Uncertain(error)) => {
                        self.launch_unknown(attempt, call)?;
                        Started::Unknown { attempt: id, error }
                    }
                    Err(HandOff::Protected(error)) => {
                        self.launch_blocked(attempt, &error, call)?;
                        Started::Blocked { attempt: id, error }
                    }
                },
            );
        }
        Ok(started)
    }
    /// The attempt's operation record and whether the attempt has settled,
    /// which makes the operation terminal.
    fn run_operation(
        &self,
        tx: &rusqlite::Connection,
        attempt: AttemptId,
    ) -> Result<(OperationRecord, bool)> {
        let (raw, settled): (Vec<u8>, bool) = tx.query_row(
            "SELECT operation_record, terminal FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
            params![attempt.to_string(), self.context.project_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((decode(&raw)?, settled))
    }
    fn launch_unknown(&self, attempt: AttemptId, call: &Request<'_>) -> Result<()> {
        self.write(call, |tx| {
            let (mut op, settled) = self.run_operation(tx, attempt)?;
            if settled {
                return Ok(());
            }
            op.state = OperationState::LaunchUnknown;
            tx.execute(
                "UPDATE attempts SET operation_record=?1 WHERE id=?2 AND project=?3",
                params![
                    crate::db::bytes(&op)?,
                    attempt.to_string(),
                    self.context.project_id.to_string()
                ],
            )?;
            Ok(())
        })
    }
    fn launch_blocked(&self, attempt: AttemptId, reason: &str, call: &Request<'_>) -> Result<()> {
        self.write(call, |tx| {
            let (mut op, settled) = self.run_operation(tx, attempt)?;
            if settled {
                return Err(RunError::Conflict);
            }
            op.state = OperationState::Blocked;
            op.blocked_reason = Some(reason.to_owned());
            tx.execute(
                "UPDATE attempts SET operation_record=?1 WHERE id=?2 AND project=?3",
                params![
                    crate::db::bytes(&op)?,
                    attempt.to_string(),
                    self.context.project_id.to_string()
                ],
            )?;
            Ok(())
        })
    }
}

fn set_lane_environment(
    environment: &mut Vec<(OsString, OsString)>,
    depth: u64,
    parent: AttemptId,
    route: &RouteRecord,
) {
    use provenant_core::consumer_env::{ACCOUNT, EFFORT, MODEL, ROUTE};
    let label = crate::route_label(
        route.provider.as_deref(),
        route.model.as_deref(),
        route.effort.as_deref(),
    );
    for (name, value) in [
        (ROUTE, label.as_deref()),
        (MODEL, route.model.as_deref()),
        (EFFORT, route.effort.as_deref()),
        (ACCOUNT, route.account_label.as_deref()),
    ] {
        environment.retain(|(existing, _)| existing != name);
        if let Some(value) = value {
            set_environment_value(environment, name, value.to_owned());
        }
    }
    set_environment_value(
        environment,
        provenant_core::consumer_env::LANE_DEPTH,
        depth.to_string(),
    );
    set_environment_value(
        environment,
        provenant_core::consumer_env::PARENT_ATTEMPT_ID,
        parent.to_string(),
    );
}

fn set_environment_value(environment: &mut Vec<(OsString, OsString)>, name: &str, value: String) {
    let name = OsString::from(name);
    environment.retain(|(existing, _)| existing != &name);
    environment.push((name, OsString::from(value)));
}
/// The configured lane port space, or none when allocation is switched off.
/// Writer lanes only: read-only lanes run no servers.
fn lane_port_plan(
    state: &std::path::Path,
) -> std::io::Result<Option<provenant_os::lane_ports::Plan>> {
    let settings = provenant_config::LaneBuildSettings::read(state)?;
    let blocks = settings.lane_port_blocks();
    if blocks == 0 {
        return Ok(None);
    }
    let narrow = |value: u64| u32::try_from(value).map_err(std::io::Error::other);
    Ok(Some(provenant_os::lane_ports::Plan {
        state: state.to_owned(),
        base: narrow(settings.lane_port_base_value())?,
        size: narrow(settings.lane_port_block_size())?,
        blocks: narrow(blocks)?,
        blocks_origin: Some(if settings.lane_port_blocks.is_some() {
            provenant_os::lane_ports::BlocksOrigin::HostConfig
        } else {
            provenant_os::lane_ports::BlocksOrigin::CompiledDefault
        }),
    }))
}

/// The lane's process-tree sampling cadence, resolved here because the
/// guardian starts with an empty environment and cannot read config. An
/// explicit `domains.run.resource_sample_interval_ms` wins. At 0 (the
/// default) an admission class with `memory_per_slot` needs a sample, so the
/// lane is sampled at the default cadence; otherwise no sampler runs.
/// `provenant config show --full` shows both settings and their sources.
fn process_tree_interval(
    state: &std::path::Path,
    project: &std::path::Path,
    policy: &provenant_records::policy::Policy,
) -> Option<std::time::Duration> {
    let configured = match provenant_confinement::run_config::RunConfig::read(state) {
        Ok(config) => config.resource_sample_interval_ms,
        Err(error) => {
            if let provenant_records::policy::Effect::Warn(message) = policy.check(
                "LAUNCH_PROCESS_TREE_INTERVAL_OR_WARN_NONE",
                provenant_records::policy::Facts::violation(format!(
                    "domains.run settings unavailable ({error}); no process-tree sampling"
                )),
            ) {
                use provenant_records::warning::warn;
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(message.once_per_change());
            }
            return None;
        }
    };
    if configured > 0 {
        return Some(std::time::Duration::from_millis(configured));
    }
    provenant_config::ResolvedAdmissionClass::resolve_all(state, Some(project))
        .into_values()
        .any(|class| class.memory_per_slot_bytes.is_some())
        .then_some(provenant_supervisor::process_tree::DEFAULT_SAMPLE_INTERVAL)
}

/// How long the guardian may retain custody of an unresolved exit, resolved
/// here for the same reason as the sampling cadence. Unreadable run settings
/// warn and keep the shipped limit; `provenant config show --full` shows
/// `domains.run.retained_custody_seconds` and its source.
fn retained_custody_ms(state: &std::path::Path, policy: &provenant_records::policy::Policy) -> u64 {
    let seconds = match provenant_confinement::run_config::RunConfig::read(state) {
        Ok(config) => config.retained_custody_seconds,
        Err(error) => {
            let seconds = provenant_supervisor::custody::DEFAULT_RETAINED_CUSTODY_SECONDS;
            if let provenant_records::policy::Effect::Warn(message) = policy.check(
                "LAUNCH_RETAINED_CUSTODY_LIMIT_OR_WARN_DEFAULT",
                provenant_records::policy::Facts::violation(format!(
                    "domains.run settings unavailable ({error}); retained custody keeps the shipped {seconds} s limit"
                )),
            ) {
                use provenant_records::warning::warn;
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(
                    message
                        .once_per_change(),
                );
            }
            seconds
        }
    };
    seconds.saturating_mul(1_000)
}

fn lane_port_plan_or_warn(
    state: &std::path::Path,
    policy: &provenant_records::policy::Policy,
) -> Option<provenant_os::lane_ports::Plan> {
    match lane_port_plan(state) {
        Ok(plan) => plan,
        Err(error) => {
            let effect = policy.check(
                "LAUNCH_LANE_PORT_PLAN_OR_WARN_NONE",
                provenant_records::policy::Facts::violation(format!(
                    "lane port settings unavailable ({error}); continuing without a port block"
                )),
            );
            if let provenant_records::policy::Effect::Warn(message) = effect {
                use provenant_records::warning::warn;
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(message.once_per_change());
            }
            None
        }
    }
}

#[cfg(test)]
#[path = "launch/lane_environment_tests.rs"]
mod lane_environment_tests;

pub(crate) mod hand_off;
#[cfg(test)]
#[path = "launch/policy_reporting_tests.rs"]
mod policy_reporting_tests;

#[cfg(test)]
#[path = "launch/resource_status_tests.rs"]
mod resource_status_tests;

#[cfg(test)]
#[path = "launch/admission_port_lifecycle_tests.rs"]
mod admission_port_lifecycle_tests;
