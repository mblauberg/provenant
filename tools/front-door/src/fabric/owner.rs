use super::*;
#[cfg(test)]
use provenant_records::command_result::ErrorClass;
use provenant_records::command_result::SettingRef;
use provenant_records::custody::{
    Admit, LeaseObservation, Sharing, Subject as CustodySubject, SubjectKind, admit_writer,
    default_sharing,
};
use provenant_records::lane_contract::{
    ChildLaunchRequest, Destination, DestinationChoice, NarrowedAuthority,
};
use provenant_records::provenance::{Layer, Source, Sourced};
use provenant_records::warning::{
    BRANCH_BEHIND_BASE, SETTING_LAYER_IGNORED, Warning, WarningCode, warn,
};

/// The launch setting that selects where a child writer is placed.
const DESTINATION_WRITE_KEY: &str = "lanes.destination.write";
/// The setting whose owning-lane override decides live sharing.
const WORKTREE_SHARING_KEY: &str = provenant_records::custody::WORKTREE_SHARING_KEY;

/// A path claim another live lane has already recorded. Claims are advisory:
/// an overlap warns and never refuses (§4, #3453/#4120).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveClaim {
    pub holder: provenant_core::AttemptId,
    pub path: provenant_core::RelativePath,
}

/// Where the host places a child writer after reading its custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChildPlacement {
    /// The parent's worktree, through its sealed shared-writer grant.
    ParentWorktree,
    /// A managed worktree from the parent's HEAD, on the child's own branch.
    ManagedWorktree,
}

fn child_setting(key: &str, value: impl Into<String>, layer: Layer, origin: &str) -> SettingRef {
    SettingRef {
        key: key.to_owned(),
        value: value.into(),
        layer,
        origin: origin.to_owned(),
    }
}

/// Whether `left` and `right` name the same path or one contains the other.
fn claims_overlap(
    left: &provenant_core::RelativePath,
    right: &provenant_core::RelativePath,
) -> bool {
    let left = left.as_path();
    let right = right.as_path();
    left.starts_with(right) || right.starts_with(left)
}

/// Resolve one child launch from its shared request.
///
/// `auto` follows the owning lane's lease: a live lease with `refuse` separates
/// the child into a managed worktree with `LANE_WRITER_SEPARATED`; otherwise the
/// child shares the parent worktree and `admit_writer` supplies `WORKTREE_SHARED`.
/// An explicit `parent` under `refuse` is the only Configured refusal here.
/// Path claims are compared against every live claim and only ever warn.
pub(crate) fn resolve_child_launch(
    tree: &CustodySubject,
    request: &ChildLaunchRequest,
    choice: DestinationChoice,
    sharing: &provenant_records::Sourced<Sharing>,
    lease: Option<&LeaseObservation>,
    live_claims: &[LiveClaim],
) -> Result<ChildPlacement, Box<provenant_records::CommandError>> {
    use provenant_records::policy::PATH_CLAIM_OVERLAP;

    let admission = admit_writer(tree, lease, sharing);
    let refusal = match &admission {
        Admit::Refuse { error } => Some(error.clone()),
        Admit::Proceed { .. } => None,
    };
    let refused = refusal.is_some();
    let placement = match choice {
        DestinationChoice::New => ChildPlacement::ManagedWorktree,
        DestinationChoice::Parent => {
            if let Some(error) = refusal {
                return Err(Box::new(error));
            }
            ChildPlacement::ParentWorktree
        }
        DestinationChoice::Auto if refused => {
            let setting = refusal
                .as_ref()
                .and_then(|error| error.setting.clone())
                .unwrap_or_else(|| provenant_records::custody::sharing_setting_ref(sharing));
            warn(Warning::new(
                WarningCode("LANE_WRITER_SEPARATED"),
                format!("LANE_WRITER_SEPARATED: {tree} is held by its owning lane; child writer placed in a managed worktree"),
                provenant_records::warning::detected_source(WarningCode("LANE_WRITER_SEPARATED")),
            )
            .subject(tree.to_string())
            .setting(setting));
            ChildPlacement::ManagedWorktree
        }
        DestinationChoice::Auto => {
            if let Admit::Proceed {
                warning: Some(warning),
            } = admission
            {
                warn(*warning);
            }
            ChildPlacement::ParentWorktree
        }
    };

    if let Some(claims) = request.path_claim.value.as_ref() {
        for claim in claims {
            let Ok(path) = claim.to_relative_path() else {
                continue;
            };
            for live in live_claims {
                if claims_overlap(&path, &live.path) {
                    warn(Warning::new(
                        WarningCode(PATH_CLAIM_OVERLAP),
                        format!(
                            "{PATH_CLAIM_OVERLAP}: claim {path:?} overlaps claim {:?} by attempt {}",
                            live.path, live.holder
                        ),
                        provenant_records::warning::detected_source(WarningCode(
                            PATH_CLAIM_OVERLAP,
                        )),
                    )
                    .subject(path.as_path().to_string_lossy().into_owned())
                    .setting(child_setting(
                        PATH_CLAIM_OVERLAP,
                        "warn",
                        Layer::Launch,
                        "--write-scope",
                    )));
                }
            }
        }
    }
    // `request.commit` is applied by the launch path that owns the commit
    // decision; placement and claims are the only decisions made here.
    Ok(placement)
}

pub(crate) struct BoundGraphOwner<'a> {
    /// Active node's sealed launch allowance, independent of settlement observation.
    pub(crate) writer_launch_budget: Option<WaitBudget>,
    /// Optional deadline for initial admission, supplied by a waited submission.
    pub(crate) admission_timeout: Option<WaitBudget>,
    pub(crate) factory: &'a Factory,
    pub(crate) repo_facts: provenant_confinement::worktrees::RepoFacts,
    pub(crate) launch_warnings: Vec<Warning>,
    // Both immutable graph bases travel together through every host owner.
    pub(crate) integration_base: Option<GraphBaseBinding>,
    pub(crate) integration_branch: Option<String>,
    /// The graph's explicit network reach for every node lane, such as
    /// `run --network off` bound before automatic preset selection.
    pub(crate) network: Option<provenant_cli::Switch>,
    pub(crate) capability_overrides: (Vec<String>, Vec<String>),
    /// The graph's route-exclusion selectors, passed to each node as the
    /// `routing.graph_exclude` knob.
    pub(crate) exclude: Vec<String>,
    pub(crate) budget: WaitBudget,
    pub(crate) cancel: &'a CancelToken,
    pub(crate) prices: Option<Result<provenant_telemetry::PriceTable, String>>,
    pub(crate) tick: bool,
}

struct QueueWriterPredecessor {
    attempt: provenant_core::AttemptId,
    source_head: Option<String>,
    head: Option<String>,
    warning: Option<String>,
    pr: Option<u64>,
}

/// Frozen intent for a launch refused before it has a canonical run request.
/// Only the digest is retained; prompts and host paths stay out of the record.
struct GraphRefusalScope {
    operation: provenant_core::OperationId,
    actor: provenant_core::ActorId,
    digest: provenant_core::Digest,
    worktree: Option<String>,
    chair: Option<provenant_records::custody::Chair>,
    /// The work key the refused launch would have taken, when the graph named it.
    work: Option<provenant_records::WorkKey>,
}

impl GraphRefusalScope {
    fn new(
        owner: &BoundGraphOwner<'_>,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: &provenant_fabric_runtime::graph::NodeInput<'_>,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        worktree: Option<&str>,
        replay: provenant_fabric_runtime::graph::Replay<'_>,
    ) -> Result<Self, FacadeError> {
        let actor = replay
            .launch_actor
            .parse()
            .map_err(|_| FacadeError::StateCorrupt)?;
        let context = &owner.factory.context;
        // Use the verified host inbox derivation, never node work labels. A
        // different launch actor cannot acquire the current chair's binding.
        let chair = (!provenant_cli::fabric::in_lane()
            && provenant_cli::workflow::session_actor(context, None).ok() == Some(actor))
        .then(|| provenant_records::custody::Chair {
            process: provenant_cli::fabric::chair_process(),
            extra: Default::default(),
            host: context.identity.inbox.host,
            actor,
            session: provenant_workflow::session::chair_session_for(
                &context.roots.state,
                context.identity.project,
                actor,
            )
            .map(|session| session.as_str().to_owned()),
        });
        let intent = serde_json::json!({
            "key":key, "route":route, "access":access, "worktree":worktree,
            "context":node.context, "prompt":node.prompt, "title":node.title,
            "source_commit":node.source_commit, "review_base":node.review_base,
            "attempt":node.attempt, "read_roots":node.read_roots,
            "attachments":node.attachments, "fixture":node.fixture,
            "capabilities":node.capabilities,
            "capability_sources":node.capability_sources,
            "skill_location":node.require_skill_location,
            "distinct_from_attempts":node.distinct_from_attempts,
            "require_distinct":node.require_distinct,
            "required_verdict":node.required_verdict, "deadline":node.deadline_seconds,
            "delivery":replay.delivery, "branch":replay.branch,
            "fast_forward_only":replay.fast_forward_only,
        });
        Ok(Self {
            operation: graph_operation("graph-attempt-operation", key)?,
            actor,
            digest: provenant_records::CanonicalV1::record_digest(
                "graph-dispatch-preflight",
                1,
                &intent,
            )
            .map_err(|_| FacadeError::StateCorrupt)?,
            worktree: worktree
                .and_then(|path| Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .map(str::to_owned),
            chair,
            work: node
                .context
                .graph_id
                .clone()
                .zip(node.context.node_id.clone())
                .zip(node.context.attempt)
                .and_then(|((graph, node), attempt)| {
                    provenant_records::WorkKey::new(context.identity.project, graph, node, attempt)
                        .ok()
                }),
        })
    }
}

/// A selected read-only checkout uses the ordinary launch root policy.
fn bind_child_directory(
    grants: &mut provenant_records::run::LaneAccessRequest,
    directory: &str,
    state: &Path,
    network: Option<provenant_cli::Switch>,
) -> Result<(), FacadeError> {
    let directory = PathBuf::from(directory);
    if !directory.is_absolute() {
        return Err(FacadeError::invalid(InvalidInputDetail::Field(
            "attempt.directory".into(),
        )));
    }
    let checked = provenant_cli::run::lane_access(
        &provenant_cli::Access {
            mode: None,
            worktree: None,
            read_root: vec![directory],
            attach: Vec::new(),
            write_root: Vec::new(),
            network,
            capability: Vec::new(),
            no_capability: Vec::new(),
        },
        state,
    )
    .map_err(dispatch_refused)?;
    let source = checked
        .read_roots
        .into_iter()
        .next()
        .ok_or(FacadeError::StateCorrupt)?;
    let directory = provenant_os::fs::Directory::open_anchor(&source)
        .map_err(|error| FacadeError::filesystem(&error))?;
    let metadata = directory
        .metadata()
        .map_err(|error| FacadeError::filesystem(&error))?;
    grants.source_identity = Some(provenant_records::run::SourceIdentity {
        device: metadata.device,
        inode: metadata.inode,
    });
    grants.source_root = Some(source);
    grants.source_live = true;
    Ok(())
}

/// Explicit writer checkouts carry their own repository, while their attempt
/// remains attributed to the parent graph's project. Relative paths resolve
/// from the project's primary root, never the invoking checkout, so a launch
/// from a linked worktree selects the same managed worktree.
fn selected_writer_project(
    factory: &Factory,
    path: &str,
) -> Result<(PathBuf, provenant_core::ProjectId), FacadeError> {
    let given = Path::new(path);
    let selected = if given.is_absolute() {
        given.to_path_buf()
    } else {
        factory.context.project_root.join(given)
    };
    if !selected.try_exists().map_err(|source| {
        FacadeError::io_failure("inspect writer worktree", Some(&selected), Some(&source))
    })? {
        return Err(run_error(provenant_run::RunError::WorktreeMissing(
            selected,
        )));
    }
    let mut checked = provenant_records::run::LaneAccessRequest::default();
    bind_child_directory(
        &mut checked,
        selected
            .to_str()
            .ok_or_else(|| FacadeError::field("worktree"))?,
        factory.context.roots.state.as_path(),
        None,
    )?;
    let directory = checked.source_root.ok_or(FacadeError::StateCorrupt)?;
    let context = Context::open(factory.context.roots.clone(), &factory.home, &directory)
        .map_err(|_| FacadeError::invalid(InvalidInputDetail::Field("worktree".into())))?;
    Ok((directory, context.identity.project))
}

/// Formatter transaction held under a fresh lease on the retained writer
/// checkout. The operation trailer reconciles a commit that reached Git before
/// the graph checkpoint recorded its result.
struct DeliveryHygieneTransaction {
    _lease: provenant_supervisor::custody::CustodyLease,
    worktree: PathBuf,
    branch: String,
    format_cwd: PathBuf,
    format_argv: Vec<String>,
    git: String,
    operation_id: String,
    expected_head: String,
}

fn delivery_git_command(
    worktree: &Path,
    args: &[&str],
) -> Result<provenant_os::exec::ExecOutcome, String> {
    let git = provenant_os::git::git_executable()
        .ok_or_else(|| "Git executable is unavailable".to_owned())?;
    let git = git
        .to_str()
        .ok_or_else(|| "Git executable path is not UTF-8".to_owned())?;
    let worktree = worktree
        .to_str()
        .ok_or_else(|| "writer worktree path is not UTF-8".to_owned())?;
    let mut argv = vec![git.to_owned(), "-C".into(), worktree.into()];
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    crate::pr::execute_for(
        Path::new(worktree),
        &refs,
        std::time::Duration::from_secs(60),
        false,
    )
    .map_err(|error| {
        let safe = provenant_records::diagnostic::safe_text(&error.to_string());
        format!(
            "Git operation failed: {}",
            delivery_hygiene_text(&safe, 512)
        )
    })
}

impl DeliveryHygieneTransaction {
    fn command(&self, args: &[&str]) -> Result<provenant_os::exec::ExecOutcome, String> {
        delivery_git_command(&self.worktree, args)
    }

    fn checked_git(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let output = self.command(args)?;
        if output.termination != provenant_os::exec::Termination::Exit(0) {
            let safe = provenant_records::diagnostic::safe_text(&String::from_utf8_lossy(
                &output.stderr.bytes,
            ));
            return Err(format!(
                "Git operation failed: {}",
                delivery_hygiene_text(&safe, 512)
            ));
        }
        Ok(output.stdout.bytes)
    }

    fn reconcile(&mut self) -> Result<Option<String>, String> {
        let head = self.head()?;
        if head == self.expected_head {
            return Ok(None);
        }
        let receipt = self.checked_git(&["show", "-s", "--format=%H%x00%B", "HEAD"])?;
        let receipt = String::from_utf8(receipt)
            .map_err(|_| "Git formatter receipt is not UTF-8".to_owned())?;
        let lineage = self.checked_git(&["rev-list", "--parents", "-n", "1", "HEAD"])?;
        let lineage = String::from_utf8(lineage)
            .map_err(|_| "Git formatter lineage is not UTF-8".to_owned())?;
        if !delivery_hygiene_receipt_matches(
            &head,
            &self.expected_head,
            &self.operation_id,
            &receipt,
            &lineage,
        ) {
            return Err("retained writer head moved without this hygiene operation receipt".into());
        }
        if self.has_edits()? {
            return Err("formatter commit was observed with additional worktree edits".into());
        }
        Ok(Some(head))
    }
}

fn delivery_hygiene_receipt_matches(
    head: &str,
    expected_head: &str,
    operation_id: &str,
    receipt: &str,
    lineage: &str,
) -> bool {
    let Some((commit, body)) = receipt.split_once('\0') else {
        return false;
    };
    let marker = format!("Delivery-Queue-Operation: {operation_id}");
    let has_marker = body.lines().any(|line| line.trim_end() == marker);
    let mut parents = lineage.split_whitespace();
    commit.trim() == head
        && has_marker
        && parents.next() == Some(head)
        && parents.next() == Some(expected_head)
        && parents.next().is_none()
}

fn delivery_git_output(worktree: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = delivery_git_command(worktree, args)?;
    if output.termination != provenant_os::exec::Termination::Exit(0) {
        let safe = provenant_records::diagnostic::safe_text(&String::from_utf8_lossy(
            &output.stderr.bytes,
        ));
        return Err(format!(
            "Git operation failed: {}",
            delivery_hygiene_text(&safe, 512)
        ));
    }
    Ok(output.stdout.bytes)
}

fn delivery_hygiene_text(value: &str, max_bytes: usize) -> String {
    let safe = provenant_records::diagnostic::safe_text(value);
    let mut end = safe.len().min(max_bytes);
    while !safe.is_char_boundary(end) {
        end -= 1;
    }
    safe[..end].to_owned()
}

impl super::graph_execution::HygieneTransaction for DeliveryHygieneTransaction {
    fn reconcile_commit(&mut self) -> Result<Option<String>, String> {
        self.reconcile()
    }

    fn head(&mut self) -> Result<String, String> {
        let bytes = self.checked_git(&["rev-parse", "--verify", "HEAD"])?;
        let head = String::from_utf8(bytes)
            .map_err(|_| "Git returned a non-UTF-8 writer head".to_owned())?
            .trim()
            .to_owned();
        head.parse::<provenant_core::GitOid>()
            .map_err(|_| "Git returned an invalid writer head".to_owned())?;
        Ok(head)
    }

    fn has_edits(&mut self) -> Result<bool, String> {
        Ok(!self
            .checked_git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?
            .is_empty())
    }

    fn format(&mut self) -> Result<(), String> {
        if self.format_argv.is_empty() {
            return Ok(());
        }
        let refs: Vec<&str> = self.format_argv.iter().map(String::as_str).collect();
        let output = crate::pr::execute_for(
            &self.format_cwd,
            &refs,
            std::time::Duration::from_secs(300),
            false,
        )
        .map_err(|error| {
            let safe = provenant_records::diagnostic::safe_text(&error.to_string());
            format!(
                "configured formatter failed: {}",
                delivery_hygiene_text(&safe, 512)
            )
        })?;
        if output.termination != provenant_os::exec::Termination::Exit(0) {
            let safe = provenant_records::diagnostic::safe_text(&String::from_utf8_lossy(
                &output.stderr.bytes,
            ));
            return Err(format!(
                "configured formatter failed: {}",
                delivery_hygiene_text(&safe, 512)
            ));
        }
        Ok(())
    }

    fn commit_format(&mut self, expected_head: &str) -> Result<(), String> {
        if self.head()? != expected_head || expected_head != self.expected_head {
            return Err("writer head changed before formatter commit".into());
        }
        if !self.has_edits()? {
            return Err("formatter reported edits but left the worktree clean".into());
        }
        self.checked_git(&["add", "--all"])?;
        if self.head()? != expected_head {
            return Err("writer head changed while staging formatter output".into());
        }
        let marker = format!("Delivery-Queue-Operation: {}", self.operation_id);
        let message = [
            self.git.clone(),
            "-C".into(),
            self.worktree
                .to_str()
                .ok_or_else(|| "writer worktree path is not UTF-8".to_owned())?
                .into(),
            "commit".into(),
            "-m".into(),
            "Apply configured formatter".into(),
            "-m".into(),
            marker,
        ];
        let refs: Vec<&str> = message.iter().map(String::as_str).collect();
        let output = crate::pr::execute_for(
            &self.worktree,
            &refs,
            std::time::Duration::from_secs(60),
            false,
        );
        match output {
            Ok(output) if output.termination == provenant_os::exec::Termination::Exit(0) => Ok(()),
            Ok(output) => {
                if self.reconcile()?.is_some() {
                    return Ok(());
                }
                let safe = provenant_records::diagnostic::safe_text(&String::from_utf8_lossy(
                    &output.stderr.bytes,
                ));
                Err(format!(
                    "formatter commit failed: {}",
                    delivery_hygiene_text(&safe, 512)
                ))
            }
            Err(error) => {
                if self.reconcile()?.is_some() {
                    return Ok(());
                }
                let safe = provenant_records::diagnostic::safe_text(&error.to_string());
                Err(format!(
                    "formatter commit failed: {}",
                    delivery_hygiene_text(&safe, 512)
                ))
            }
        }
    }
}

fn stored_graph_refusal(
    record: &provenant_run::OperationRecord,
) -> Result<FacadeError, FacadeError> {
    let diagnostic = record.refusal.clone().ok_or(FacadeError::StateCorrupt)?;
    Ok(match &record.refusal_code {
        Some(refusal_code) => FacadeError::RecordedRefusal {
            refusal_code: refusal_code.clone(),
            diagnostic: Box::new(diagnostic),
        },
        None => run_error(provenant_run::RunError::RecordedRefusal(Box::new(
            diagnostic,
        ))),
    })
}

/// The operator-facing WRITER_SCOPE refusal for a dirty writer checkout. The
/// typed path summary is the only untrusted text rendered here, and it is
/// display-sanitised and bounded before it is stored.
fn writer_dirty_diagnostic(
    error: &provenant_confinement::worktrees::WorktreeError,
) -> Option<provenant_records::diagnostic::Diagnostic> {
    match error {
        provenant_confinement::worktrees::WorktreeError::Dirty(paths) => {
            let detail = if paths.is_empty() {
                "the writer worktree has uncommitted edits".to_owned()
            } else {
                format!(
                    "the writer worktree has uncommitted edits: {}",
                    provenant_records::diagnostic::safe_text(paths)
                )
            };
            Some(
                provenant_records::diagnostic::Diagnostic::new("WRITER_SCOPE")
                    .category("refused")
                    .detail(detail)
                    .next("commit or preserve the worktree changes before retrying"),
            )
        }
        _ => None,
    }
}

impl BoundGraphOwner<'_> {
    fn fast_forward_writer(
        &self,
        directory: &Path,
        path: &Path,
        head: provenant_core::GitOid,
        dirty_preflight: bool,
    ) -> Result<(), provenant_confinement::worktrees::WorktreeError> {
        let budget = self.writer_launch_budget.unwrap_or(self.budget);
        if budget.remaining(self.factory.clock.mono_now()).as_micros() == 0 {
            return Err(
                provenant_confinement::worktrees::WorktreeError::RegistryLockTimeout {
                    holder: provenant_confinement::worktrees::recorded_writer_holder(
                        self.factory.context.roots.state.as_path(),
                        self.factory.context.identity.project,
                        path,
                    ),
                },
            );
        }
        provenant_confinement::worktrees::with_context_using_facts(
            &self.factory.context.roots,
            &self.factory.home,
            directory,
            &self.factory.clock,
            self.cancel,
            &self.repo_facts,
            |context| {
                if budget.remaining(self.factory.clock.mono_now()).as_micros() == 0 {
                    return Err(
                        provenant_confinement::worktrees::WorktreeError::RegistryLockTimeout {
                            holder: provenant_confinement::worktrees::writer_holder(
                                context.project_state,
                                path,
                            ),
                        },
                    );
                }
                let context = provenant_confinement::worktrees::WorktreeContext {
                    lock_budget: budget,
                    ..*context
                };
                let edits = if dirty_preflight {
                    provenant_confinement::worktrees::WriterEdits::ContinueInPlace
                } else {
                    provenant_confinement::worktrees::WriterEdits::Refuse
                };
                provenant_confinement::worktrees::fast_forward(
                    &context,
                    path,
                    head,
                    edits,
                    self.cancel,
                )
            },
        )
    }

    fn refusal_store(
        &self,
        scope: &GraphRefusalScope,
    ) -> Result<provenant_run::RunStore, FacadeError> {
        BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: Some(scope.actor),
            chair: None,
        }
        .store_with_facts(self.cancel, &self.repo_facts)
    }

    /// Retain a submission-time dirty writer refusal under the graph attempt's
    /// stable identity, before the scheduler can start any node.
    pub(crate) fn retain_submission_refusal(
        &self,
        graph_id: &str,
        node_id: &str,
        worktree: &str,
        refusal: &provenant_run::writer_dirty::WriterRefusal,
    ) -> Result<(), FacadeError> {
        let key = provenant_fabric_runtime::graph::AttemptKey {
            extra: Default::default(),
            retry_generation: None,
            previous_attempt_ref: None,
            graph_id: graph_id.to_owned(),
            node_id: node_id.to_owned(),
            attempt: 1,
            route_index: 0,
        };
        let digest =
            provenant_records::CanonicalV1::record_digest("graph-submission-refusal", 1, &key)
                .map_err(|_| FacadeError::StateCorrupt)?;
        let scope = GraphRefusalScope {
            operation: graph_operation("graph-attempt-operation", &key)?,
            actor: self.factory.context.identity.inbox.actor,
            digest,
            worktree: Path::new(worktree)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned),
            chair: (!provenant_cli::fabric::in_lane()
                && provenant_cli::workflow::session_actor(&self.factory.context, None).ok()
                    == Some(self.factory.context.identity.inbox.actor))
            .then(|| provenant_records::custody::Chair {
                process: provenant_cli::fabric::chair_process(),
                extra: Default::default(),
                host: self.factory.context.identity.inbox.host,
                actor: self.factory.context.identity.inbox.actor,
                session: provenant_workflow::session::chair_session_for(
                    &self.factory.context.roots.state,
                    self.factory.context.identity.project,
                    self.factory.context.identity.inbox.actor,
                )
                .map(|session| session.as_str().to_owned()),
            }),
            work: key.work_key(self.factory.context.identity.project).ok(),
        };
        let error = FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: refusal.reason.clone(),
        };
        let diagnostic = provenant_records::diagnostic::Diagnostic::new("WRITER_SCOPE")
            .category("refused")
            .detail(refusal.reason.clone())
            .next(refusal.next.clone());
        self.retain_graph_refusal(&scope, &error, diagnostic)
    }

    fn post_graph_refusal(
        &self,
        store: &provenant_run::RunStore,
        operation: &provenant_core::OperationId,
        call: &provenant_run::Request<'_>,
    ) {
        if crate::settlement::post_operation_refusal(operation, &self.factory.context, store, call)
            .is_err()
        {
            warn(
                Warning::new(
                    provenant_records::warning::REFUSAL_NOTICE_UNDELIVERED,
                    "refusal notice could not be delivered",
                    provenant_records::warning::detected_source(
                        provenant_records::warning::REFUSAL_NOTICE_UNDELIVERED,
                    ),
                )
                .repair("run `provenant reconcile`"),
            );
        }
    }

    fn retain_graph_refusal(
        &self,
        scope: &GraphRefusalScope,
        error: &FacadeError,
        diagnostic: provenant_records::diagnostic::Diagnostic,
    ) -> Result<(), FacadeError> {
        let store = self.refusal_store(scope)?;
        let call = provenant_run::Request {
            clock: &self.factory.clock,
            cancel: self.cancel,
        };
        let config = provenant_cli::config_reads::read_for(&self.factory.context.roots, "run")
            .map_err(|_| FacadeError::io("refusal notice configuration"))?;
        let notice = config
            .knobs
            .get("chair.refusal_notice")
            .map_or_else(|| serde_json::json!("inbox"), |knob| knob.value.clone());
        let notice = serde_json::from_value(notice).map_err(|_| FacadeError::StateCorrupt)?;
        let record = provenant_run::OperationRecord {
            extra: Default::default(),
            preflight_digest_version: None,
            refusal_notice: Some(notice),
            refusal_code: Some(error.code().to_owned()),
            refusal_notice_done: false,
            origin_intent_digest: Some(scope.digest),
            schema_version: 1,
            project_id: self.factory.context.identity.project,
            actor_id: scope.actor,
            operation_id: scope.operation.clone(),
            kind: provenant_run::OperationKind::Dispatch,
            request_digest: scope.digest,
            parent_attempt: None,
            state: provenant_run::OperationState::Refused,
            blocked_reason: None,
            refusal: Some(diagnostic),
            worktree: scope.worktree.clone(),
            refused_at: Some(
                self.factory
                    .clock
                    .wall_now()
                    .map_err(|_| FacadeError::io("refusal time"))?,
            ),
            refusal_chair: scope.chair.clone(),
        };
        let saved = match &scope.work {
            Some(key) => store.insert_refusal_under(key, record, &call),
            None => store.insert_refusal(record, &call),
        }
        .map_err(run_error)?;
        if saved.state != provenant_run::OperationState::Refused {
            return Err(run_error(provenant_run::RunError::OperationConflict));
        }
        self.post_graph_refusal(&store, &scope.operation, &call);
        Ok(())
    }
}

/// The inbox operation a graph ask files under: one per graph instance, ask ID
/// and session, so filing the same ask again finds the request it filed first.
fn graph_ask_operation(
    filing: &provenant_fabric_runtime::graph::AskFiling<'_>,
) -> Result<provenant_core::OperationId, FacadeError> {
    let ask = filing.ask;
    let operation = provenant_records::CanonicalV1::record_digest(
        "graph-node-ask",
        1,
        &(
            filing.graph_id,
            filing.instance_id,
            &ask.ask_id,
            &ask.session,
        ),
    )
    .map_err(|_| FacadeError::StateCorrupt)?;
    provenant_core::OperationId::parse(&format!(
        "ask-{}",
        provenant_core::hex(operation.as_bytes())
    ))
    .map_err(|_| FacadeError::StateCorrupt)
}

/// The inbox request for an ask to file.
pub(crate) fn graph_ask_input(
    filing: &provenant_fabric_runtime::graph::AskFiling<'_>,
) -> Result<provenant_fabric_runtime::inbox::InboxInput, FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    use provenant_records::inbox::{RequestDraft, RequestKind, RequestOption};
    let ask = filing.ask;
    let request = RequestDraft {
        extra: Default::default(),
        supersedes: None,
        deadline: ask.wake_at,
        work: None,
        kind: RequestKind::Decision,
        title: ask.title.clone(),
        body: ask.body.clone(),
        blocking: true,
        options: ask
            .options
            .iter()
            .map(|option| RequestOption {
                extra: Default::default(),
                id: option.id.clone(),
                label: option.label.clone(),
                // The inbox needs a description; a decide option may leave it out.
                description: if option.description.is_empty() {
                    option.label.clone()
                } else {
                    option.description.clone()
                },
            })
            .collect(),
        recommended_option_id: ask.default.clone(),
        proceeding_with: None,
    };
    Ok(InboxInput::Ask {
        extra: Default::default(),
        session: ask.session.clone(),
        request: Box::new(request),
        operation_id: Some(graph_ask_operation(filing)?),
    })
}

/// A notification uses its own immutable inbox operation and never asks for an
/// answer. Replaying it after a checkpoint failure returns the same receipt.
pub(crate) fn graph_notification_input(
    filing: &provenant_fabric_runtime::graph::AskFiling<'_>,
) -> Result<provenant_fabric_runtime::inbox::InboxInput, FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    let mut input = graph_ask_input(filing)?;
    if let InboxInput::Ask { request, .. } = &mut input {
        request.kind = provenant_records::inbox::RequestKind::Question;
        request.blocking = false;
        request.options.clear();
        request.recommended_option_id = None;
    }
    Ok(input)
}

fn graph_ask_refused(ask_id: &str, error: &dyn std::fmt::Display) -> FacadeError {
    FacadeError::DispatchRefused {
        fact: None,
        disk: None,
        reason: format!("graph ask {ask_id}: {error}"),
    }
}

/// The request filed under `operation` by `actor`, found through the normal
/// bounded inbox list.
fn graph_ask_find(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    session: &str,
    operation: &provenant_core::OperationId,
    actor: &str,
    ask_id: &str,
    cancel: &CancelToken,
) -> Result<Option<provenant_records::inbox::HumanRequest>, FacadeError> {
    use provenant_fabric_runtime::inbox::{InboxFilter, InboxInput, InboxResult};
    let mut cursor = None;
    loop {
        let InboxResult::Listed {
            items, next_cursor, ..
        } = service
            .execute(
                InboxInput::List {
                    extra: Default::default(),
                    id: None,
                    session: Some(session.into()),
                    state: InboxFilter::All,
                    limit: 50,
                    cursor,
                },
                cancel,
            )
            .map_err(|error| graph_ask_refused(ask_id, &error))?
        else {
            return Err(FacadeError::StateCorrupt);
        };
        if let Some(found) = items
            .into_iter()
            .find(|item| &item.operation_id == operation && item.chair_actor.to_string() == actor)
        {
            return Ok(Some(found));
        }
        let Some(next) = next_cursor else {
            return Ok(None);
        };
        cursor = Some(next);
    }
}

/// File the ask as one open request. The request is found by its operation
/// before anything is sent, so a replay returns the request, and the deadline,
/// it was first filed with: a deadline recomputed from the clock never reaches
/// the inbox as a second, conflicting request.
pub(crate) fn graph_ask_file(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    filing: &provenant_fabric_runtime::graph::AskFiling<'_>,
    cancel: &CancelToken,
) -> Result<provenant_fabric_runtime::graph::AskFiled, FacadeError> {
    use provenant_fabric_runtime::graph::AskFiled;
    use provenant_fabric_runtime::inbox::InboxResult;
    use provenant_records::inbox::RequestState;
    let ask = filing.ask;
    let input = graph_ask_input(filing)?;
    let operation = graph_ask_operation(filing)?;
    if let Some(found) = graph_ask_find(
        service,
        &ask.session,
        &operation,
        filing.actor,
        &ask.ask_id,
        cancel,
    )? {
        return Ok(AskFiled {
            request: found.id.to_string(),
            deadline: found.request.deadline,
            // An expired request keeps its deadline, and the graph acts on that.
            closed: matches!(
                found.state,
                RequestState::Withdrawn | RequestState::Superseded
            ),
        });
    }
    let InboxResult::Saved { receipt } = service
        .execute(input, cancel)
        .map_err(|error| graph_ask_refused(&ask.ask_id, &error))?
    else {
        return Err(FacadeError::StateCorrupt);
    };
    Ok(AskFiled {
        request: receipt.request_id.to_string(),
        deadline: ask.wake_at,
        closed: false,
    })
}

/// The linked request as the inbox holds it.
fn graph_ask_item(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
    cancel: &CancelToken,
) -> Result<Option<provenant_records::inbox::InboxItem>, FacadeError> {
    use provenant_fabric_runtime::inbox::{InboxFilter, InboxInput, InboxResult};
    if scope.ask.request.is_empty() {
        return Ok(None);
    }
    let Ok(id) = scope.ask.request.parse::<provenant_core::HumanRequestId>() else {
        return Ok(None);
    };
    let outcome = service.execute(
        InboxInput::List {
            extra: Default::default(),
            id: Some(id),
            session: None,
            state: InboxFilter::All,
            limit: 1,
            cursor: None,
        },
        cancel,
    );
    let item = match outcome {
        Ok(InboxResult::Item { item, .. }) => item,
        Err(provenant_fabric_runtime::inbox::InboxError::Store(
            provenant_fabric_store::Error::NotFound,
        )) => return Ok(None),
        Err(provenant_fabric_runtime::inbox::InboxError::Store(
            ref err @ provenant_fabric_store::Error::Decode(_),
        )) => return Err(FacadeError::from(err)),
        Err(error) => return Err(graph_ask_refused(&scope.ask.ask_id, &error)),
        _ => return Err(FacadeError::StateCorrupt),
    };
    if item.request.chair_actor.to_string() != scope.actor {
        return Err(graph_ask_refused(
            &scope.ask.ask_id,
            &"the request belongs to another actor",
        ));
    }
    Ok(Some(*item))
}

/// Check whether a linked ask has an answer without acknowledging or applying
/// it. Queue admission uses this read-only probe to wake answered parked graphs
/// without reserving capacity for unanswered owner waits.
pub(crate) fn graph_asks_have_answer(
    factory: &Factory,
    actor: &str,
    asks: &[provenant_fabric_runtime::graph::AskLink],
    cancel: &CancelToken,
) -> Result<bool, FacadeError> {
    let Some(first) = asks.first() else {
        return Ok(false);
    };
    graph_ask_with(factory, cancel, actor, &first.ask_id, |service| {
        use provenant_records::inbox::RequestState;
        for ask in asks {
            let scope = provenant_fabric_runtime::graph::AskScope { actor, ask };
            let Some(item) = graph_ask_item(service, &scope, cancel)? else {
                continue;
            };
            if !matches!(
                item.request.state,
                RequestState::Withdrawn | RequestState::Superseded | RequestState::Expired
            ) && item.answer.is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    })
}

/// Read the answer to the linked request, whichever channel gave it.
pub(crate) fn graph_ask_poll(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
    cancel: &CancelToken,
) -> Result<Option<provenant_fabric_runtime::graph::AskReply>, FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    use provenant_records::inbox::RequestState;
    let Some(item) = graph_ask_item(service, scope, cancel)? else {
        return Ok(None);
    };
    if matches!(
        item.request.state,
        RequestState::Withdrawn | RequestState::Superseded | RequestState::Expired
    ) {
        return Ok(None);
    }
    let Some(answer) = item.answer else {
        return Ok(None);
    };
    // Acknowledgement precedes action. The immutable answer stays readable after
    // acknowledgement, so a failed graph checkpoint save loses no decision.
    service
        .execute(
            InboxInput::Acknowledge {
                extra: Default::default(),
                id: item.request.id,
                answer_id: answer.id,
                session: scope.ask.session.clone(),
                operation_id: Some(
                    provenant_core::OperationId::parse(&format!("ask-ack-{}", answer.id))
                        .map_err(|_| FacadeError::StateCorrupt)?,
                ),
            },
            cancel,
        )
        .map_err(|error| graph_ask_refused(&scope.ask.ask_id, &error))?;
    Ok(Some(provenant_fabric_runtime::graph::AskReply {
        option: answer.option_id,
        text: answer.text,
        actor: answer.actor.to_string(),
        at: Some(answer.answered_at),
    }))
}

/// Supersede the linked request while it is still open. Closing never creates a
/// request, and an answered or already closed one is left as it is.
pub(crate) fn graph_ask_close(
    service: &provenant_fabric_runtime::inbox::InboxService<'_>,
    scope: &provenant_fabric_runtime::graph::AskScope<'_>,
    cancel: &CancelToken,
) -> Result<(), FacadeError> {
    use provenant_fabric_runtime::inbox::InboxInput;
    use provenant_records::inbox::RequestState;
    let Some(item) = graph_ask_item(service, scope, cancel)? else {
        return Ok(());
    };
    if item.request.state != RequestState::Open {
        return Ok(());
    }
    service
        .execute(
            InboxInput::Close {
                extra: Default::default(),
                id: item.request.id,
                state: RequestState::Superseded,
                reason: "Graph decision resolved or cancelled".into(),
                expect_revision: item.request.revision,
                operation_id: Some(
                    provenant_core::OperationId::parse(&format!("ask-close-{}", item.request.id))
                        .map_err(|_| FacadeError::StateCorrupt)?,
                ),
            },
            cancel,
        )
        .map_err(|error| graph_ask_refused(&scope.ask.ask_id, &error))?;
    Ok(())
}

/// Run `edge` against the inbox as the actor saved with the graph. Settlement
/// can run under another client label, so the saved actor is kept, just as an
/// attempt retains its launch actor. Graph asks obey the lane restriction like
/// any inbox edge.
pub(crate) fn graph_ask_with<T>(
    factory: &Factory,
    cancel: &CancelToken,
    actor: &str,
    ask_id: &str,
    edge: impl FnOnce(&provenant_fabric_runtime::inbox::InboxService<'_>) -> Result<T, FacadeError>,
) -> Result<T, FacadeError> {
    let context = &factory.context;
    let mut identity = context.identity.clone();
    identity.inbox.actor = actor.parse().map_err(|_| FacadeError::StateCorrupt)?;
    // Filing, reading and closing all write to the inbox.
    if context.in_lane {
        return Err(graph_ask_refused(
            ask_id,
            &provenant_fabric_runtime::inbox::InboxError::LaneMutation,
        ));
    }
    let service = provenant_fabric_runtime::inbox::InboxService::open(
        &context.roots.state,
        &identity,
        &factory.clock,
        cancel,
    )
    .map_err(|error| graph_ask_refused(ask_id, &error))?;
    edge(&service)
}

/// Read the bundled sheet with the same host and project config layers the
/// other cross-cutting domains use. Missing overlays leave the sheet unchanged.
pub(crate) fn load_graph_prices_layered(
    assets: &std::path::Path,
    roots: &provenant_core::Roots,
) -> Result<provenant_telemetry::PriceTable, String> {
    let project = roots.checkout();
    provenant_telemetry::prices_overlay::load(assets, roots.state.as_path(), Some(project))
        .map(|layered| layered.table)
        .map_err(|error| error.to_string())
}

pub(crate) fn graph_attempt_usage(
    record: provenant_records::run::AttemptRecord,
    prices: Option<&provenant_telemetry::PriceTable>,
    cost_unknown_reason: Option<String>,
) -> provenant_fabric_runtime::graph::AttemptUsage {
    let usage = &record.usage;
    let categories = [
        usage.tokens.input,
        usage.tokens.cache_read,
        usage.tokens.cache_write,
        usage.tokens.output,
        usage.tokens.reasoning,
    ];
    // Reasoning is counted inside output, so providers never report it apart.
    let tokens_partial = categories[..4].iter().any(Option::is_none);
    let tokens = categories.iter().any(Option::is_some).then(|| {
        categories
            .into_iter()
            .flatten()
            .fold(0u64, u64::saturating_add)
    });
    let cost = prices
        .and_then(|prices| {
            provenant_run::fact(record.clone()).ok().and_then(|fact| {
                provenant_telemetry::stats([fact], prices, Default::default()).ok()
            })
        })
        .map(|report| report.totals.cost);
    let cost_partial = cost.as_ref().is_none_or(|cost| cost.complete.is_none());
    let cost_micro_usd =
        cost.and_then(|cost| cost.known_lower_bound.map(|money| money.microdollars));
    // Persist only reporting facts. Provider text, paths and traces never enter events.
    let mut extra = provenant_records::ExtraFields::default();
    let mut resolved_route = None;
    let mut wall_time_seconds = record.provider_ms.map(|ms| ms.saturating_add(999) / 1000);
    if let Ok(fact) = provenant_run::fact(record.clone()) {
        if let Some(started) = fact.started_at {
            wall_time_seconds = fact
                .finished_at
                .as_micros()
                .checked_sub(started.as_micros())
                .and_then(|micros| u64::try_from(micros).ok())
                .map(|micros| micros.saturating_add(999_999) / 1_000_000);
        }
        let inputs = [
            fact.usage.uncached_input,
            fact.usage.cache_read,
            fact.usage.cache_write,
        ];
        let input_tokens = inputs
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .map(|counts| counts.into_iter().fold(0u64, u64::saturating_add));
        if let Some(route) = fact
            .observed_route
            .as_ref()
            .or(fact.applied_route.as_ref())
            .or(fact.resolved_route.as_ref())
        {
            resolved_route = Some(provenant_fabric_runtime::graph::AttemptResolvedRoute {
                extra: Default::default(),
                provider: route.provider.clone(),
                model: route.model.clone(),
                effort: route.effort.clone(),
            });
        }
        for (key, value) in [
            (
                "cache_read_tokens",
                serde_json::json!(fact.usage.cache_read),
            ),
            ("input_tokens", serde_json::json!(input_tokens)),
            ("queue_time_ms", serde_json::json!(fact.slot_wait_ms)),
            ("outcome", serde_json::json!(fact.terminal_state)),
            ("tier", serde_json::json!(fact.tier)),
            ("template", serde_json::json!(fact.template)),
        ] {
            if !value.is_null() {
                extra.insert(key.into(), value);
            }
        }
    }
    provenant_fabric_runtime::graph::AttemptUsage {
        extra,
        resolved_route,
        tokens,
        cost_micro_usd,
        wall_time_seconds,
        tokens_partial,
        cost_partial,
        cost_unknown_reason,
    }
}

pub(crate) fn decode_graph_document(
    bytes: &[u8],
) -> Result<provenant_fabric_runtime::graph::GraphDocument, FacadeError> {
    #[derive(serde::Deserialize)]
    struct Header {
        schema_version: Option<serde_json::Value>,
    }
    // Check the version before decoding the graph, so a future schema can
    // request an upgrade even when its node fields are unknown to this reader.
    let limits = provenant_records::Limits {
        max_bytes: provenant_fabric_runtime::config::GraphLimits::document_bytes_bound()
            .maximum_u64(),
        ..Default::default()
    };
    // Keep the parser's field path and message: a bare `document` hides the
    // field that actually failed, on the peer as much as on the host.
    let header: Header = provenant_records::decode(bytes, limits).map_err(|error| {
        FacadeError::invalid(InvalidInputDetail::graph_reason(format!(
            "document header: {error}"
        )))
    })?;
    match header
        .schema_version
        .as_ref()
        .and_then(serde_json::Value::as_u64)
    {
        Some(1 | 2) => provenant_records::decode(bytes, limits).map_err(|error| {
            FacadeError::invalid(InvalidInputDetail::graph_reason(format!(
                "document: {error}"
            )))
        }),
        Some(version) if version > 2 => Err(FacadeError::UpgradeRequired {
            reader_release: env!("CARGO_PKG_VERSION").into(),
            writer_release: "unknown".into(),
            record: "graph document schema".into(),
            version,
        }),
        _ => Err(FacadeError::invalid(
            provenant_fabric_runtime::coordination::facade::InvalidInputDetail::Schema,
        )),
    }
}

pub(crate) fn graph_journal_error(
    error: provenant_fabric_runtime::graph::JournalError,
) -> FacadeError {
    use provenant_fabric_runtime::graph::JournalError;
    let unsupported_version = match error.root_cause() {
        JournalError::Decode(decode) => match decode.root_cause() {
            provenant_records::DecodeError::UnsupportedVersion { found, .. } if *found > 1 => {
                Some(*found)
            }
            _ => None,
        },
        _ => None,
    };
    if let Some(version) = unsupported_version {
        return FacadeError::UpgradeRequired {
            reader_release: env!("CARGO_PKG_VERSION").into(),
            writer_release: "unknown".into(),
            record: "graph checkpoint schema".into(),
            version,
        };
    }
    if let JournalError::Graph { ref source, .. } = error {
        return match source.root_cause() {
            JournalError::TooLarge => FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason: error.to_string(),
            },
            JournalError::NameReserved => FacadeError::Conflict,
            JournalError::Filesystem(_)
            | JournalError::Io(_)
            | JournalError::DurabilityUnconfirmed(_) => FacadeError::io("access graph journal"),
            _ if source.is_unreadable() => FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason: error.to_string(),
            },
            _ => FacadeError::StateCorrupt,
        };
    }
    match error {
        JournalError::TooLarge => FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: "graph checkpoint exceeds its byte limit".into(),
        },
        JournalError::NameReserved => FacadeError::Conflict,
        JournalError::Filesystem(_)
        | JournalError::Io(_)
        | JournalError::DurabilityUnconfirmed(_) => FacadeError::io("access graph journal"),
        e if e.is_unreadable() => FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: e.to_string(),
        },
        _ => FacadeError::StateCorrupt,
    }
}

pub(crate) fn graph_result_warning(error: &provenant_run::RunError) -> &'static str {
    match error {
        provenant_run::RunError::ArtifactMismatch(_) => {
            "artifact digest or length does not match its record"
        }
        provenant_run::RunError::NotFound => "result artifact is missing",
        provenant_run::RunError::Filesystem(source) if source.is_not_found() => {
            "result artifact is missing"
        }
        provenant_run::RunError::TooLarge { .. } => "result artifact exceeds the byte limit",
        _ => "result artifact could not be verified",
    }
}

impl BoundGraphOwner<'_> {
    fn uncertain_peer_placement(
        &self,
        attempt: &provenant_fabric_runtime::graph::GraphAttempt,
    ) -> bool {
        let Some(name) = attempt.placement_host.as_deref() else {
            return false;
        };
        let local = self.factory.context.identity.host;
        !matches!(name, "local" | "localhost")
            && !name.parse::<provenant_core::HostId>().is_ok_and(|host| host == local)
            // A removed peer configuration cannot turn a retained placement
            // into proof that nothing was accepted on that peer.
            && self.factory.context.peer_host(name) != Some(local)
    }

    pub(crate) fn prior_models(&self, attempts: &[String]) -> Result<Vec<String>, FacadeError> {
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let mut models = Vec::new();
        for reference in attempts {
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            if let Some(model) = owner
                .snapshot(target, self.budget, self.cancel)?
                .applied_route
                .and_then(|route| prior_model(route.model.as_deref()))
            {
                models.push(model);
            }
        }
        Ok(models)
    }

    /// The graph exclusions plus, on an alias retry, the models that earlier
    /// attempts of this node already used. The runtime resolves those models
    /// from the attempt records, so they arrive as model ids, not references.
    pub(crate) fn route_exclusions(
        &self,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        prior_models: &[String],
        accepted: bool,
    ) -> Vec<String> {
        let mut exclude = self.exclude.clone();
        if !accepted && route.alias.is_some() {
            for model in prior_models {
                if !exclude.contains(model) {
                    exclude.push(model.clone());
                }
            }
        }
        exclude
    }

    /// Model families that served the writer attempts a reviewer must differ from.
    pub(crate) fn writer_families(&self, attempts: &[String]) -> Result<Vec<String>, FacadeError> {
        if attempts.is_empty() {
            return Ok(Vec::new());
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let mut families = Vec::new();
        for reference in attempts {
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            let record = owner.snapshot(target, self.budget, self.cancel)?;
            let family = record
                .applied_route
                .and_then(|route| route.model)
                .and_then(|model| provenant_cli::run::model_family(&model));
            if let Some(family) = family
                && !families.contains(&family)
            {
                families.push(family);
            }
        }
        Ok(families)
    }
}

impl provenant_fabric_runtime::graph::CiStatus for BoundGraphOwner<'_> {
    fn host_stamp(&self) -> Option<provenant_fabric_runtime::graph::CiHostStamp> {
        Some(provenant_fabric_runtime::graph::CiHostStamp {
            extra: Default::default(),
            boot_identity: self.factory.clock.boot_identity().ok()?,
            host_monotonic_micros: self.factory.clock.host_monotonic_micros().ok()?,
        })
    }
    fn observe_conflict(
        &mut self,
        target: &str,
    ) -> provenant_fabric_runtime::graph::ConflictObservation {
        crate::pr::status::observe_conflict(
            self.factory.context.roots.state.as_path(),
            &self.factory.cwd,
            target,
        )
    }
    fn observe_ci(
        &mut self,
        target: &str,
        checks: &[String],
        delivery_queue: bool,
        failed_checks_max_bytes: usize,
        max_failed_checks: usize,
    ) -> provenant_fabric_runtime::graph::CiObservation {
        crate::pr::status::observe_ci(
            self.factory.context.roots.state.as_path(),
            &self.factory.cwd,
            target,
            checks,
            delivery_queue,
            failed_checks_max_bytes,
            max_failed_checks,
        )
    }
}

// Review roles come from the shared role library. The framing role
// `plan.review` prepares the subject and is not an independent review.
pub(crate) fn graph_review_node(
    document: &provenant_fabric_runtime::graph::GraphDocument,
    id: &str,
) -> bool {
    document
        .nodes
        .iter()
        .any(|node| node.id == id && provenant_fabric_runtime::graph::is_review_node(node))
}

pub(crate) fn writer_source_identity_matches(
    checkpoint: &provenant_fabric_runtime::graph::GraphCheckpoint,
    source: &Path,
    identity: &provenant_records::run::SourceIdentity,
) -> bool {
    checkpoint
        .nodes
        .iter()
        .filter_map(|node| node.writer_binding.as_ref())
        .filter(|binding| Path::new(&binding.worktree) == source)
        .all(|binding| binding.source_directory_identity.as_ref() == Some(identity))
}

pub(crate) fn unanimous_preparation_cause(
    skipped: &[provenant_records::run::SkippedRoute],
) -> Option<provenant_records::RouteExclusionReason> {
    let mut causes = skipped.iter().map(|route| route.cause.as_ref());
    let first = causes.next()??;
    causes
        .all(|cause| cause == Some(first))
        .then(|| first.clone())
}

fn repair_scope_grew(
    review: &provenant_routing::ReviewConfig,
    before: &[String],
    after: &[String],
) -> bool {
    let rank = |tier: Option<&str>| match tier {
        Some("terminal") => 3,
        Some("crucial") => 2,
        Some("substantial") => 1,
        _ => 0,
    };
    let old_floor = review.path_floors(before);
    let new_floor = review.path_floors(after);
    after.iter().any(|path| !before.contains(path))
        || rank(new_floor.tier.as_deref()) > rank(old_floor.tier.as_deref())
}

/// Re-touching a protected surface can grow semantic risk even when the path
/// set and its tier are unchanged. This deliberately loose signal only replans.
fn repair_touches_risk(review: &provenant_routing::ReviewConfig, touched: &[String]) -> bool {
    review
        .path_floors(touched)
        .classes
        .iter()
        .any(|class| !matches!(class.as_str(), "routine" | "unknown"))
}

impl BoundGraphOwner<'_> {
    fn repair_git(&self, directory: &Path, args: &[&str]) -> Result<String, String> {
        use provenant_os::exec::{ExecOptions, Executor, Termination};
        let git = provenant_os::git::git_executable().ok_or("Git executable unavailable")?;
        let mut argv = vec![git.into_os_string()];
        argv.extend(args.iter().map(std::ffi::OsString::from));
        let remaining = std::time::Duration::from_micros(
            self.budget
                .remaining(self.factory.clock.mono_now())
                .as_micros(),
        )
        .min(std::time::Duration::from_secs(30));
        let mut options = ExecOptions::new(&argv, directory, remaining);
        options.cancel = Some(self.cancel);
        let output = self
            .factory
            .executor
            .execute(options)
            .map_err(|_| "repair Git command could not run".to_owned())?;
        if output.termination != Termination::Exit(0) || output.timed_out || output.stdout.truncated
        {
            return Err("repair Git command did not complete within its bounds".into());
        }
        String::from_utf8(output.stdout.bytes).map_err(|_| "repair Git output is not UTF-8".into())
    }

    /// The two layered settings that decide a child's destination. An absent
    /// or malformed knob reads as the declared default (`auto`, `warn`), so a
    /// child launch never refuses on a configuration read failure.
    fn child_launch_settings(&self) -> (DestinationChoice, provenant_records::Sourced<Sharing>) {
        let config = match provenant_cli::config_reads::read_for(&self.factory.context.roots, "run")
        {
            Ok(config) => config,
            Err(error) => {
                warn(Warning::new(
                    provenant_records::warning::CONFIG_UNAVAILABLE,
                    format!("child launch settings unavailable: {error}; using defaults"),
                    provenant_records::warning::detected_source(
                        provenant_records::warning::CONFIG_UNAVAILABLE,
                    ),
                ));
                return (DestinationChoice::Auto, default_sharing());
            }
        };
        let choice = config
            .knobs
            .get(DESTINATION_WRITE_KEY)
            .and_then(|knob| knob.value.as_str())
            .and_then(|value| match value {
                "auto" => Some(DestinationChoice::Auto),
                "parent" => Some(DestinationChoice::Parent),
                "new" => Some(DestinationChoice::New),
                _ => None,
            })
            .unwrap_or_default();
        let sharing = config
            .knobs
            .get(WORKTREE_SHARING_KEY)
            .and_then(|knob| {
                Some(provenant_records::Sourced {
                    value: knob.value.as_str()?.parse().ok()?,
                    source: knob.source.clone(),
                })
            })
            .unwrap_or_else(default_sharing);
        (choice, sharing)
    }
}

impl BoundGraphOwner<'_> {
    /// The skipped routes of this graph attempt's own unlaunched run record.
    fn preparation_skipped_routes(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        reference: &str,
    ) -> Result<Option<Vec<provenant_records::run::SkippedRoute>>, FacadeError> {
        let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
        if host != self.factory.context.identity.host.to_string() {
            return Ok(None);
        }
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let record = store
            .record(
                attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
                &call,
            )
            .map_err(run_error)?;
        if store
            .attempt_operation(record.attempt_id, &call)
            .map_err(run_error)?
            .operation_id
            != graph_operation("graph-attempt-operation", key)?
            || record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(key.graph_id.as_str())
                    || context.node_id.as_deref() != Some(key.node_id.as_str())
            })
        {
            return Ok(None);
        }
        Ok(Some(record.skipped_routes))
    }
}

#[path = "owner/attempt_owner.rs"]
mod attempt_owner;
mod carry;
mod claims;
mod commit_own;

/// Reconstruct an [`AttemptSpec`] from an accepted task request and the node's admitter,
/// round-tripping all fields that affect dispatch and request hashing.
pub(crate) fn recovered_attempt_spec(
    task: &provenant_records::run::TaskRequest,
    original: Option<&provenant_fabric_runtime::graph::AttemptSpec>,
) -> Result<provenant_fabric_runtime::graph::AttemptSpec, FacadeError> {
    Ok(provenant_fabric_runtime::graph::AttemptSpec {
        account: task.account.clone(),
        build_heavy: task.build_heavy,
        confidential: task.profile.confidential,
        parent_sandboxed: task.profile.parent_sandboxed
            || original.is_some_and(|spec| spec.parent_sandboxed),
        admitted_by: task
            .profile
            .admitted_by
            .clone()
            .or_else(|| original.and_then(|spec| spec.admitted_by.clone())),
        launch_policy: task.profile.repository_policy.clone(),
        policy_sealed: true,
        sealed_profile: Some(task.profile.clone()),
        provider_timeout: Some(task.provider_timeout_seconds),
        // Node-level choices the task request does not carry: a recovered
        // shared child still commits only its claims, and a recovered writer
        // keeps its carry setting.
        commit: original.and_then(|spec| spec.commit.clone()),
        carry_uncommitted: original.and_then(|spec| spec.carry_uncommitted),
        ..Default::default()
    })
}

impl BoundGraphOwner<'_> {
    fn execute_delivery_hygiene_inner(
        &mut self,
        request: &provenant_fabric_runtime::graph::HygieneRequest<'_>,
    ) -> Result<provenant_fabric_runtime::graph::HygieneExecution, FacadeError> {
        use provenant_fabric_runtime::graph::HygieneExecution;

        let (host, attempt) = request
            .source_attempt
            .split_once('/')
            .ok_or(FacadeError::StateCorrupt)?;
        if host
            .parse::<provenant_core::HostId>()
            .map_err(|_| FacadeError::StateCorrupt)?
            != self.factory.context.identity.host
        {
            return Err(FacadeError::OwnerUnavailable);
        }
        let attempt_id = attempt
            .parse::<provenant_core::AttemptId>()
            .map_err(|_| FacadeError::StateCorrupt)?;
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let record = store.record(attempt_id, &call).map_err(run_error)?;
        let operation = store
            .attempt_operation(record.attempt_id, &call)
            .map_err(run_error)?;
        let expected_work = request
            .source_key
            .work_key(self.factory.context.identity.project)
            .map_err(|_| FacadeError::StateCorrupt)?;
        if record.key != expected_work
            || operation.operation_id
                != graph_operation("graph-attempt-operation", request.source_key)?
            || record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(request.graph_id)
                    || context.node_id.as_deref() != Some(request.source_node)
            })
            || !matches!(
                record.state,
                provenant_records::run::AttemptState::Terminal { .. }
            )
            || !store.custody_released(&record, &call).map_err(run_error)?
        {
            return Err(FacadeError::Conflict);
        }

        let worktree = PathBuf::from(request.worktree);
        if !worktree.is_absolute() {
            return Err(FacadeError::StateCorrupt);
        }
        let expected_identity = request.worktree_identity.ok_or(FacadeError::StateCorrupt)?;
        let (project_directory, _) = selected_writer_project(self.factory, request.worktree)?;
        let ports = provenant_config::PortTable::read_for(
            Some(&project_directory),
            self.factory.context.roots.state.as_path(),
        )
        .map_err(|error| {
            FacadeError::io_failure("read delivery formatter configuration", None, Some(&error))
        })?;
        let sourced_format = ports.verify_format();
        let formatter_source = format!(
            "{}{}",
            sourced_format.source.layer,
            sourced_format
                .source
                .origin
                .as_ref()
                .map_or_else(String::new, |origin| format!(" ({origin})"))
        );
        let (format_argv, format_cwd) = match &sourced_format.value {
            provenant_config::VerifyFormat::None => (Vec::new(), project_directory.clone()),
            provenant_config::VerifyFormat::Command(command) => {
                if command.argv.first().is_none_or(String::is_empty) {
                    return Err(FacadeError::field("verify.format"));
                }
                let cwd = project_directory
                    .join(&ports.verify.workspace)
                    .join(&command.cwd);
                let root = std::fs::canonicalize(&project_directory)
                    .map_err(|_| FacadeError::field("verify.workspace"))?;
                let cwd = std::fs::canonicalize(&cwd)
                    .map_err(|_| FacadeError::field("verify.workspace"))?;
                if !cwd.starts_with(&root) || !cwd.is_dir() {
                    return Err(FacadeError::field("verify.workspace"));
                }
                (command.argv.clone(), cwd)
            }
        };
        let git = provenant_os::git::git_executable()
            .and_then(|git| git.to_str().map(str::to_owned))
            .ok_or_else(|| FacadeError::io("locate git for delivery hygiene"))?;
        let scope_attempt = provenant_core::AttemptId::from_bytes(
            provenant_os::fs::entropy_128().map_err(|_| FacadeError::StateCorrupt)?,
        );

        let prepared = provenant_confinement::worktrees::with_context_using_workers_and_facts(
            &self.factory.context.roots,
            &self.factory.home,
            &project_directory,
            &self.factory.clock,
            self.cancel,
            self.factory.workers,
            &self.repo_facts,
            |context| {
                let prepared = (|| -> Result<DeliveryHygieneTransaction, String> {
                    let verified =
                        provenant_confinement::repo::verify_worktree(context.repo, &worktree)
                            .map_err(|error| error.to_string())?;
                    let metadata = std::fs::metadata(&worktree)
                        .map_err(|error| format!("inspect retained worktree: {error}"))?;
                    use std::os::unix::fs::MetadataExt;
                    if metadata.dev() != expected_identity.device
                        || metadata.ino() != expected_identity.inode
                    {
                        return Err("retained writer worktree identity changed".into());
                    }
                    let branch = String::from_utf8(delivery_git_output(
                        &worktree,
                        &["symbolic-ref", "--short", "HEAD"],
                    )?)
                    .map_err(|_| "retained writer branch is not UTF-8".to_owned())?
                    .trim()
                    .to_owned();
                    if branch.is_empty() || branch == "HEAD" {
                        return Err("retained writer worktree has no branch".into());
                    }
                    let branch_ref = provenant_confinement::repo::GitRefName::branch(&branch)
                        .map_err(|error| error.to_string())?;
                    let head = String::from_utf8(delivery_git_output(
                        &worktree,
                        &["rev-parse", "--verify", "HEAD"],
                    )?)
                    .map_err(|_| "retained writer head is not UTF-8".to_owned())?
                    .trim()
                    .to_owned();
                    let base = head
                        .parse::<provenant_core::GitOid>()
                        .map_err(|_| "retained writer head is invalid".to_owned())?;
                    let inventory = context
                        .facts
                        .writer_inventory(
                            context.repo,
                            context.executor,
                            context.utility,
                            context.utility_budget,
                            self.cancel,
                        )
                        .map_err(|error| error.to_string())?;
                    let (slot, binding) = provenant_confinement::worktrees::writer_custody(
                        context.project_state,
                        &verified,
                        &worktree,
                        &branch_ref,
                        base,
                        || Ok(inventory),
                    )
                    .map_err(|error| error.to_string())?;
                    let lease = provenant_supervisor::custody::acquire_writer(
                        slot,
                        provenant_records::custody::Scope {
                            attempt: scope_attempt,
                            generation: 1,
                        },
                        binding,
                        self.budget,
                        self.cancel,
                        &self.factory.clock,
                    )
                    .map_err(|error| format!("writer custody unavailable: {error}"))?;
                    let metadata = std::fs::metadata(&worktree)
                        .map_err(|error| format!("recheck retained worktree: {error}"))?;
                    if metadata.dev() != expected_identity.device
                        || metadata.ino() != expected_identity.inode
                    {
                        return Err(
                            "retained writer worktree identity changed under custody".into()
                        );
                    }
                    let branch_after = String::from_utf8(delivery_git_output(
                        &worktree,
                        &["symbolic-ref", "--short", "HEAD"],
                    )?)
                    .map_err(|_| "retained writer branch is not UTF-8".to_owned())?
                    .trim()
                    .to_owned();
                    let head_after = String::from_utf8(delivery_git_output(
                        &worktree,
                        &["rev-parse", "--verify", "HEAD"],
                    )?)
                    .map_err(|_| "retained writer head is not UTF-8".to_owned())?
                    .trim()
                    .to_owned();
                    if branch_after != branch || head_after != head {
                        return Err("retained writer moved while custody was acquired".into());
                    }
                    Ok(DeliveryHygieneTransaction {
                        _lease: lease,
                        worktree: worktree.clone(),
                        branch,
                        format_cwd: format_cwd.clone(),
                        format_argv: format_argv.clone(),
                        git: git.clone(),
                        operation_id: request.operation_id.to_owned(),
                        expected_head: request.expected_head.to_owned(),
                    })
                })();
                Ok(prepared)
            },
        )
        .map_err(|error| FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: format!("retained writer could not be verified: {error}"),
        })?;
        let mut transaction = prepared.map_err(|reason| FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason,
        })?;
        let (outcome, diagnostic) = super::graph_execution::run_hygiene(
            &mut transaction,
            request.input_head,
            request.expected_head,
            request.handoff.map(str::to_owned),
        );
        Ok(HygieneExecution {
            outcome,
            branch: Some(transaction.branch.clone()),
            formatter_source: Some(formatter_source),
            diagnostic,
        })
    }

    fn execute_delivery_publication_inner(
        &mut self,
        request: &provenant_fabric_runtime::graph::PublicationRequest<'_>,
    ) -> Result<provenant_fabric_runtime::graph::DeliveryPublication, FacadeError> {
        let (host, attempt) = request
            .source_attempt
            .split_once('/')
            .ok_or(FacadeError::StateCorrupt)?;
        if host
            .parse::<provenant_core::HostId>()
            .map_err(|_| FacadeError::StateCorrupt)?
            != self.factory.context.identity.host
        {
            return Err(FacadeError::OwnerUnavailable);
        }
        let attempt_id = attempt
            .parse::<provenant_core::AttemptId>()
            .map_err(|_| FacadeError::StateCorrupt)?;
        let owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let record = store.record(attempt_id, &call).map_err(run_error)?;
        let operation = store
            .attempt_operation(record.attempt_id, &call)
            .map_err(run_error)?;
        let expected_work = request
            .source_key
            .work_key(self.factory.context.identity.project)
            .map_err(|_| FacadeError::StateCorrupt)?;
        if record.key != expected_work
            || operation.operation_id
                != graph_operation("graph-attempt-operation", request.source_key)?
            || record.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(request.graph_id)
                    || context.node_id.as_deref() != Some(request.source_node)
            })
            || !matches!(
                record.state,
                provenant_records::run::AttemptState::Terminal { .. }
            )
            || !store.custody_released(&record, &call).map_err(run_error)?
        {
            return Err(FacadeError::Conflict);
        }

        let worktree = PathBuf::from(request.worktree);
        if !worktree.is_absolute() {
            return Err(FacadeError::StateCorrupt);
        }
        let expected_identity = request.worktree_identity.ok_or(FacadeError::StateCorrupt)?;
        let (project_directory, _) = selected_writer_project(self.factory, request.worktree)?;
        let expected_head = request
            .expected_head
            .parse::<provenant_core::GitOid>()
            .map_err(|_| FacadeError::StateCorrupt)?;
        let integration_base = request
            .integration_base
            .parse::<provenant_core::GitOid>()
            .map_err(|_| FacadeError::StateCorrupt)?;
        let expected_remote_head = request
            .expected_remote_head
            .map(|head| head.parse::<provenant_core::GitOid>())
            .transpose()
            .map_err(|_| FacadeError::StateCorrupt)?;
        let adoption_target = if let Some(adoption) = request.adoption {
            if adoption.branch != request.writer_branch
                || adoption.branch != request.source_branch
                || adoption.worktree != request.worktree
                || request.worktree_identity != Some(&adoption.source_directory_identity)
                || adoption.head != request.integration_base
                || expected_remote_head.is_none()
            {
                return Err(FacadeError::Conflict);
            }
            Some(provenant_run::GraphAdoptionTarget {
                repository: &adoption.repository,
                pull_request: adoption.pull_request,
                pull_request_url: &adoption.pull_request_url,
                branch: &adoption.branch,
                base_repository: &adoption.base_repository,
                base_branch: &adoption.base_branch,
            })
        } else {
            None
        };
        let scope_attempt = provenant_core::AttemptId::from_bytes(
            provenant_os::fs::entropy_128().map_err(|_| FacadeError::StateCorrupt)?,
        );
        let forge = provenant_run::resolve_forge(
            self.factory.context.roots.state.as_path(),
            self.factory.cwd.as_path(),
            &self.factory.context.port_table,
        )
        .map_err(run_error)?;
        let body = format!(
            "Delivery queue graph {}\n\nNode {} published clean head {}.",
            request.graph_id, request.node_id, request.expected_head
        );
        let host_delivery =
            provenant_run::HostDeliveryContext::capture().with_repo_facts(&self.repo_facts);
        let receipt = provenant_confinement::worktrees::with_context_using_workers_and_facts(
            &self.factory.context.roots,
            &self.factory.home,
            &project_directory,
            &self.factory.clock,
            self.cancel,
            self.factory.workers,
            &self.repo_facts,
            |context| {
                let prepared = (|| -> Result<provenant_run::GraphPublicationReceipt, String> {
                    let verified =
                        provenant_confinement::repo::verify_worktree(context.repo, &worktree)
                            .map_err(|error| error.to_string())?;
                    let metadata = std::fs::metadata(&worktree)
                        .map_err(|error| format!("inspect retained worktree: {error}"))?;
                    use std::os::unix::fs::MetadataExt;
                    if metadata.dev() != expected_identity.device
                        || metadata.ino() != expected_identity.inode
                    {
                        return Err("retained writer worktree identity changed".into());
                    }
                    let branch = String::from_utf8(delivery_git_output(
                        &worktree,
                        &["symbolic-ref", "--short", "HEAD"],
                    )?)
                    .map_err(|_| "retained writer branch is not UTF-8".to_owned())?
                    .trim()
                    .to_owned();
                    if branch != request.writer_branch || branch.is_empty() {
                        return Err("retained writer branch differs from hygiene receipt".into());
                    }
                    let branch_ref = provenant_confinement::repo::GitRefName::branch(&branch)
                        .map_err(|error| error.to_string())?;
                    let head = String::from_utf8(delivery_git_output(
                        &worktree,
                        &["rev-parse", "--verify", "HEAD"],
                    )?)
                    .map_err(|_| "retained writer head is not UTF-8".to_owned())?
                    .trim()
                    .to_owned();
                    let observed_head = head
                        .parse::<provenant_core::GitOid>()
                        .map_err(|_| "retained writer head is invalid".to_owned())?;
                    if observed_head != expected_head {
                        return Err("retained writer head differs from hygiene receipt".into());
                    }
                    let status = delivery_git_output(
                        &worktree,
                        &["status", "--porcelain=2", "--untracked-files=all"],
                    )?;
                    if !status.is_empty() {
                        return Err("retained writer worktree changed after hygiene".into());
                    }
                    let inventory = context
                        .facts
                        .writer_inventory(
                            context.repo,
                            context.executor,
                            context.utility,
                            context.utility_budget,
                            self.cancel,
                        )
                        .map_err(|error| error.to_string())?;
                    let (slot, binding) = provenant_confinement::worktrees::writer_custody(
                        context.project_state,
                        &verified,
                        &worktree,
                        &branch_ref,
                        observed_head,
                        || Ok(inventory),
                    )
                    .map_err(|error| error.to_string())?;
                    let _lease = provenant_supervisor::custody::acquire_writer(
                        slot,
                        provenant_records::custody::Scope {
                            attempt: scope_attempt,
                            generation: 1,
                        },
                        binding,
                        self.budget,
                        self.cancel,
                        &self.factory.clock,
                    )
                    .map_err(|error| format!("writer custody unavailable: {error}"))?;
                    let metadata = std::fs::metadata(&worktree)
                        .map_err(|error| format!("recheck retained worktree: {error}"))?;
                    if metadata.dev() != expected_identity.device
                        || metadata.ino() != expected_identity.inode
                    {
                        return Err(
                            "retained writer worktree identity changed under custody".into()
                        );
                    }
                    let branch_after = String::from_utf8(delivery_git_output(
                        &worktree,
                        &["symbolic-ref", "--short", "HEAD"],
                    )?)
                    .map_err(|_| "retained writer branch is not UTF-8".to_owned())?
                    .trim()
                    .to_owned();
                    let head_after = String::from_utf8(delivery_git_output(
                        &worktree,
                        &["rev-parse", "--verify", "HEAD"],
                    )?)
                    .map_err(|_| "retained writer head is not UTF-8".to_owned())?
                    .trim()
                    .to_owned();
                    let status_after = delivery_git_output(
                        &worktree,
                        &["status", "--porcelain=2", "--untracked-files=all"],
                    )?;
                    if branch_after != branch
                        || head_after != request.expected_head
                        || !status_after.is_empty()
                    {
                        return Err(
                            "retained writer changed while publication custody was acquired".into(),
                        );
                    }
                    let delivery = host_delivery.clone();
                    match adoption_target {
                        Some(target) => delivery.update_adopted_graph_pr_at_head(
                            &project_directory,
                            expected_head,
                            expected_remote_head.ok_or_else(|| {
                                "adopted publication has no persisted expected remote head"
                                    .to_owned()
                            })?,
                            &target,
                            &forge,
                        ),
                        None => delivery.deliver_graph_pr_at_head(
                            &project_directory,
                            request.writer_branch,
                            integration_base,
                            expected_head,
                            request.publication_identity,
                            expected_remote_head,
                            &body,
                            &forge,
                        ),
                    }
                    .map_err(|error| error.to_string())
                })();
                Ok(prepared)
            },
        )
        .map_err(|error| FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: format!("retained worktree could not be verified for publication: {error}"),
        })?;
        let receipt = receipt.map_err(|reason| FacadeError::DispatchRefused {
            fact: None,
            disk: None,
            reason: delivery_hygiene_text(&reason, 1024),
        })?;
        if request.adoption.is_some_and(|adoption| {
            receipt.branch != adoption.branch
                || receipt.pull_request != adoption.pull_request_url
                || receipt.head != expected_head
        }) {
            return Err(FacadeError::Conflict);
        }
        Ok(provenant_fabric_runtime::graph::DeliveryPublication {
            branch: receipt.branch,
            head: receipt.head.to_string(),
            pull_request: receipt.pull_request,
        })
    }

    #[allow(clippy::too_many_arguments)] // Launch facts stay outside the kernel's public input shape.
    fn observe_or_start_inner(
        &mut self,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: provenant_fabric_runtime::graph::NodeInput<'_>,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        worktree: Option<&str>,
        replay: provenant_fabric_runtime::graph::Replay<'_>,
        refusal: Option<&GraphRefusalScope>,
    ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, FacadeError> {
        use provenant_fabric_runtime::graph::AttemptObservation;
        let mut owner = BinaryRunOwner {
            factory: self.factory,
            allow_remote: true,
            actor: None,
            chair: None,
        };
        let own_commit = node.attempt.and_then(|spec| spec.commit.clone());
        let (required_verdict, deadline_seconds, source_commit) = (
            node.required_verdict,
            node.deadline_seconds,
            node.source_commit,
        );
        let (target, reference) = if let Some(reference) = replay.stored_ref {
            // An accepted attempt is observed by reference: operation replay is
            // actor-scoped, so resubmitting under another actor would launch again.
            let (host, attempt) = reference.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            let target = AttemptTarget {
                extra: Default::default(),
                host: Some(host.parse().map_err(|_| FacadeError::StateCorrupt)?),
                attempt: attempt.parse().map_err(|_| FacadeError::StateCorrupt)?,
            };
            (target, Some(reference.to_owned()))
        } else {
            match self.start_attempt(
                &mut owner, key, node, route, access, worktree, replay, refusal,
            )? {
                StartedAttempt::Submitted { target, reference } => (target, reference),
                StartedAttempt::Refused(observation) => return Ok(observation),
            }
        };
        let attempt = owner.snapshot(target.clone(), self.budget, self.cancel)?;
        // A local attempt carries this node's exact work key. A remote attempt
        // lives in the receiver's own graph and carries its immutable origin.
        let remote_target = target
            .host
            .is_some_and(|host| host != self.factory.context.identity.host);
        let expected_origin = key
            .work_key(self.factory.context.identity.project)
            .map_err(|_| FacadeError::StateCorrupt)?;
        let identity_matches = if remote_target {
            attempt.key.node() == key.node_id
                && super::peer::peer_origin_matches(&attempt.peer, &expected_origin)
        } else {
            key.work_key(attempt.key.project()).ok().as_ref() == Some(&attempt.key)
        };
        if !identity_matches {
            return Err(FacadeError::StateCorrupt);
        }
        // A remote parent is one origin node until its receiver-owned graph
        // has drained, including children that outlive the provider process.
        if remote_target
            && attempt.peer.peer_subtree.as_ref().is_some_and(|subtree| {
                matches!(
                    subtree.status,
                    provenant_records::graph::GraphStatus::Pending
                        | provenant_records::graph::GraphStatus::Paused
                )
            })
        {
            return Ok(AttemptObservation::Pending {
                attempt_ref: reference,
            });
        }
        self.observe_terminal(
            &mut owner,
            attempt,
            target,
            reference,
            expected_origin,
            TerminalFacts {
                required_verdict,
                deadline_seconds,
                source_commit,
                access,
                own_commit,
            },
        )
    }

    fn reconcile_queue_writer_predecessor(
        &self,
        owner: &BinaryRunOwner<'_>,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: &provenant_fabric_runtime::graph::NodeInput<'_>,
        worktree: &str,
        writer_directory: &Path,
    ) -> Result<Option<QueueWriterPredecessor>, FacadeError> {
        use provenant_confinement::worktrees::{WorktreeOrigin, load_owner};

        if !node.context.delivery_queue_writer_adoption {
            return Ok(None);
        }
        let state = self.factory.context.roots.state.as_path();
        let checkpoint = provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
            &self.factory.context.roots.state,
            self.factory.context.identity.project,
            &key.graph_id,
        )
        .map_err(graph_journal_error)?
        .ok_or(FacadeError::StateCorrupt)?;
        let binding = checkpoint
            .nodes
            .iter()
            .find(|progress| progress.id == key.node_id)
            .and_then(|progress| progress.writer_binding.as_ref())
            .ok_or(FacadeError::StateCorrupt)?;
        if binding.worktree != worktree {
            return Err(FacadeError::Conflict);
        }
        let expected_identity = binding
            .source_directory_identity
            .as_ref()
            .ok_or(FacadeError::StateCorrupt)?;
        let metadata = std::fs::metadata(worktree).map_err(|source| {
            FacadeError::io_failure(
                "inspect queue writer worktree",
                Some(Path::new(worktree)),
                Some(&source),
            )
        })?;
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != expected_identity.device || metadata.ino() != expected_identity.inode {
            return Err(FacadeError::Conflict);
        }

        let request = provenant_run::resolve_writer(state, writer_directory, Path::new(worktree))
            .map_err(run_error)?;
        let launch = provenant_run::prepare_writer_with_facts(
            &request,
            state,
            self.factory.context.identity.project,
            &self.repo_facts,
        )
        .map_err(run_error)?;
        let identity = launch.worktree.identity();
        if identity.device != expected_identity.device || identity.inode != expected_identity.inode
        {
            return Err(FacadeError::Conflict);
        }
        let Some(worktree_owner) = load_owner(&launch.worktree)
            .map_err(|error| run_error(provenant_run::RunError::Writer(error)))?
        else {
            return Ok(None);
        };
        if worktree_owner.origin != WorktreeOrigin::Lane {
            return Ok(None);
        }
        let predecessor = worktree_owner.attempt_id.ok_or(FacadeError::StateCorrupt)?;
        if worktree_owner.host != self.factory.context.identity.host {
            return Err(FacadeError::OwnerUnavailable);
        }
        let call = owner.call(self.cancel);
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let record = store.record(predecessor, &call).map_err(run_error)?;
        if !matches!(
            record.state,
            provenant_records::run::AttemptState::Terminal { .. }
        ) || !store.custody_released(&record, &call).map_err(run_error)?
        {
            return Err(FacadeError::Conflict);
        }
        let operation = store
            .attempt_operation(predecessor, &call)
            .map_err(run_error)?;
        if worktree_owner.operation_id.as_ref() != Some(&operation.operation_id) {
            return Err(FacadeError::Conflict);
        }
        let previous_task = store.request(predecessor, &call).map_err(run_error)?.task;
        if !previous_task
            .writer
            .as_ref()
            .is_some_and(|previous| provenant_run::equivalent_writer(previous, &request))
            || request.custody_project
                != previous_task
                    .writer
                    .as_ref()
                    .and_then(|writer| writer.custody_project)
        {
            return Err(FacadeError::Conflict);
        }
        store
            .check_continuation_writer(predecessor, &call)
            .map_err(run_error)?;

        let branch = launch.branch.as_str().trim_start_matches("refs/heads/");
        if worktree_owner.branch != branch
            || previous_task
                .deliver
                .as_ref()
                .is_some_and(|delivery| delivery.branch != branch)
        {
            return Err(FacadeError::Conflict);
        }
        let previous_context = previous_task.context.as_ref();
        let pr = previous_context.and_then(|context| context.pr);
        if pr.is_some_and(|previous| node.context.pr.is_some_and(|current| current != previous)) {
            return Err(FacadeError::Conflict);
        }
        let source_head = previous_task
            .source_head
            .as_ref()
            .or(previous_task.head.as_ref())
            .cloned()
            .or_else(|| node.source_commit.map(str::to_owned));
        let mut head = record
            .head
            .map(|head| head.to_string())
            .or_else(|| previous_task.head.clone())
            .ok_or(FacadeError::Conflict)?;
        let pinned = head
            .parse::<provenant_core::GitOid>()
            .map_err(|_| FacadeError::Conflict)?;
        let mut warning = None;
        let git = provenant_os::git::git_executable()
            .ok_or_else(|| FacadeError::io("git for queue writer reconciliation"))?;
        match launch.repin_head(&git, pinned).map_err(run_error)? {
            provenant_run::RepinOutcome::Unchanged { .. } => {}
            outcome @ provenant_run::RepinOutcome::Repinned { head: current, .. } => {
                warning = outcome.warning();
                head = current.to_string();
            }
            outcome @ provenant_run::RepinOutcome::NotDescendant { .. } => {
                return Err(FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason: outcome.refusal().unwrap_or_else(|| {
                        "retained queue writer does not descend from its prior head".into()
                    }),
                });
            }
        }
        // Re-entry may retain uncommitted edits, but the committed checkout
        // must still be the exact head selected by the graph's source binding.
        if node.source_commit.is_some_and(|expected| expected != head) {
            return Err(FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason: "retained queue writer head differs from the graph source".into(),
            });
        }
        Ok(Some(QueueWriterPredecessor {
            attempt: predecessor,
            source_head,
            head: Some(head),
            warning,
            pr,
        }))
    }

    /// Submit a new graph attempt, or report the pre-launch refusal that
    /// replaces it. Its own frame keeps the launch facts off the observation
    /// path (#3542).
    #[allow(clippy::too_many_arguments)] // Launch facts stay outside the kernel's public input shape.
    fn start_attempt(
        &mut self,
        owner: &mut BinaryRunOwner<'_>,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        node: provenant_fabric_runtime::graph::NodeInput<'_>,
        route: &provenant_fabric_runtime::graph::GraphRoute,
        access: provenant_fabric_runtime::graph::AccessMode,
        worktree: Option<&str>,
        replay: provenant_fabric_runtime::graph::Replay<'_>,
        refusal: Option<&GraphRefusalScope>,
    ) -> Result<StartedAttempt, FacadeError> {
        use provenant_fabric_runtime::graph::{AttemptObservation, GraphAttemptOutcome};
        let operation_id = graph_operation("graph-attempt-operation", key)?;
        let cancel = self.cancel;
        let blocked = |owner: &BinaryRunOwner<'_>| -> Result<Option<String>, FacadeError> {
            let store = owner.store_with_facts(cancel, &self.repo_facts)?;
            Ok(store
                .operation(&operation_id, &owner.call(cancel))
                .map_err(run_error)?
                .filter(|operation| operation.state == provenant_run::OperationState::Blocked)
                .map(|operation| {
                    operation
                        .blocked_reason
                        .unwrap_or_else(|| "custody release failed".into())
                }))
        };
        let refusal = refusal.ok_or(FacadeError::StateCorrupt)?;
        let host = node
            .attempt
            .and_then(|spec| spec.host.as_deref())
            .map(|name| {
                let local = self.factory.context.identity.host;
                if matches!(name, "local" | "localhost")
                    || name
                        .parse::<provenant_core::HostId>()
                        .is_ok_and(|host| host == local)
                {
                    Ok(local)
                } else {
                    self.factory
                        .context
                        .peer_host(name)
                        .ok_or(FacadeError::UnknownHost)
                }
            })
            .transpose()?;
        if host.is_some_and(|host| host != self.factory.context.identity.host) {
            // A peer provisions its own worktree from a branch and a base ref.
            // A caller path is never admitted across the boundary.
            let caller_path = node.worktree.is_some_and(|path| !path.is_empty());
            let peer_writer = access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                && !caller_path
                && node
                    .attempt
                    .as_ref()
                    .and_then(|spec| spec.new_worktree.as_ref())
                    .is_some();
            if (access != provenant_fabric_runtime::graph::AccessMode::ReadOnly && !peer_writer)
                || caller_path
            {
                return Err(FacadeError::WorktreeNotApplicable);
            }
        }
        if node
            .attempt
            .is_some_and(|spec| spec.names_cloud_placement())
        {
            return Err(FacadeError::invalid(
                InvalidInputDetail::CloudPlacementRemoved,
            ));
        }
        // Replaying an accepted operation must observe its existing writer,
        // even after that writer has advanced beyond the pinned start.
        owner.actor = Some(
            replay
                .launch_actor
                .parse()
                .map_err(|_| FacadeError::StateCorrupt)?,
        );
        if let Some(reason) = blocked(&*owner)? {
            return Err(FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason,
            });
        }
        let operation_id = graph_operation("graph-attempt-operation", key)?;
        let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
        let call = owner.call(self.cancel);
        let operation = store.operation(&operation_id, &call).map_err(run_error)?;
        let accepted = operation.as_ref().is_some_and(|operation| {
            !matches!(
                operation.state,
                provenant_run::OperationState::Prepared | provenant_run::OperationState::Refused
            )
        });
        if !accepted
            && host.is_some_and(|host| host != self.factory.context.identity.host)
            && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
            && node
                .attempt
                .and_then(|spec| spec.new_worktree.as_ref())
                .is_some_and(|tree| tree.default_base)
        {
            let warnings = carry::peer_unsupported_warnings(
                self,
                node.attempt.and_then(|spec| spec.carry_uncommitted),
            );
            self.launch_warnings.extend(warnings);
        }
        let writer_project = if access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
        {
            worktree
                .map(|path| selected_writer_project(self.factory, path))
                .transpose()?
        } else {
            None
        };
        let writer_directory = writer_project
            .as_ref()
            .map_or(self.factory.cwd.as_path(), |(directory, _)| {
                directory.as_path()
            });
        let foreign_writer = writer_project
            .as_ref()
            .is_some_and(|(_, project)| *project != self.factory.context.identity.project);

        // A graph retry is a new attempt, but retains the predecessor's
        // original source pin and terminal writer head. Mark continuations
        // even when both heads agree so preparation checks dirty remnants.
        let mut submission_prompt = node.prompt.to_owned();
        let mut writer_predecessor = None;
        let mut repin_warning = None;
        let mut reconciled_pr = None;
        let (mut source_head, mut successor_head) = if !accepted
            && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
            && let Some(previous) = key.previous_attempt_ref.as_deref()
        {
            let (host, previous) = previous.split_once('/').ok_or(FacadeError::StateCorrupt)?;
            if host != self.factory.context.identity.host.to_string() {
                return Err(FacadeError::OwnerUnavailable);
            }
            let previous = store
                .record(
                    previous.parse().map_err(|_| FacadeError::StateCorrupt)?,
                    &call,
                )
                .map_err(run_error)?;
            if previous.context.as_ref().is_none_or(|context| {
                context.graph_id.as_deref() != Some(key.graph_id.as_str())
                    || context.node_id.as_deref() != Some(key.node_id.as_str())
            }) || !matches!(
                previous.state,
                provenant_records::run::AttemptState::Terminal { .. }
            ) || !store
                .custody_released(&previous, &call)
                .map_err(run_error)?
            {
                return Err(FacadeError::Conflict);
            }
            if matches!(
                previous.requested_route,
                provenant_records::run::RequestedRoute::Named { .. }
            ) && matches!(
                previous.state,
                provenant_records::run::AttemptState::Terminal {
                    outcome: provenant_records::run::Outcome::QuotaLimited,
                    ..
                }
            ) {
                submission_prompt.push_str("\n\nThe previous member stopped on quota. Inspect the retained worktree and any commits, then continue the original task from that work rather than restart. Use another eligible member of the same alias.");
            }
            let request = store
                .request(previous.attempt_id, &call)
                .map_err(run_error)?;
            let previous_task = &request.task;
            let writer = provenant_run::resolve_writer(
                self.factory.context.roots.state.as_path(),
                writer_directory,
                Path::new(worktree.ok_or(FacadeError::StateCorrupt)?),
            )
            .map_err(run_error)?;
            if !previous_task
                .writer
                .as_ref()
                .is_some_and(|previous| provenant_run::equivalent_writer(previous, &writer))
            {
                return Err(FacadeError::Conflict);
            }
            store
                .check_continuation_writer(previous.attempt_id, &call)
                .map_err(run_error)?;
            writer_predecessor = Some(previous.attempt_id);
            let original = previous_task
                .source_head
                .as_ref()
                .or(previous_task.head.as_ref())
                .cloned()
                .or_else(|| node.source_commit.map(str::to_owned));
            let current = previous
                .head
                .map(|head| head.to_string())
                .or_else(|| previous_task.head.clone());
            // A manual retry keeps the predecessor pin. When the worktree
            // head has advanced through the writer's own commits, adopt it
            // and warn with both ids; any other head is a hard refusal.
            if let Some(pinned) = current
                .as_deref()
                .and_then(|head| provenant_core::GitOid::parse(head).ok())
            {
                let git = provenant_os::git::git_executable()
                    .ok_or_else(|| FacadeError::io("git for retry re-pin"))?;
                let launch = provenant_run::prepare_writer_with_facts(
                    &writer,
                    self.factory.context.roots.state.as_path(),
                    self.factory.context.identity.project,
                    &self.repo_facts,
                )
                .map_err(run_error)?;
                match launch.repin_head(&git, pinned).map_err(run_error)? {
                    provenant_run::RepinOutcome::Unchanged { .. } => (original, current),
                    outcome @ provenant_run::RepinOutcome::Repinned { head, .. } => {
                        repin_warning = outcome.warning();
                        (original, Some(head.to_string()))
                    }
                    outcome @ provenant_run::RepinOutcome::NotDescendant { .. } => {
                        return Err(FacadeError::DispatchRefused {
                            fact: None,
                            disk: None,
                            reason: outcome.refusal().unwrap_or_else(|| {
                                "writer head does not descend from its pinned commit".into()
                            }),
                        });
                    }
                }
            } else {
                (original, current)
            }
        } else if !accepted
            && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
            && node.context.delivery_queue_writer_adoption
            && let Some(worktree) = worktree
        {
            if let Some(predecessor) = self.reconcile_queue_writer_predecessor(
                owner,
                key,
                &node,
                worktree,
                writer_directory,
            )? {
                writer_predecessor = Some(predecessor.attempt);
                repin_warning = predecessor.warning;
                reconciled_pr = predecessor.pr;
                (predecessor.source_head, predecessor.head)
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };
        if let Some(warning) = repin_warning.take() {
            warn(
                Warning::new(
                    BRANCH_BEHIND_BASE,
                    warning.clone(),
                    provenant_records::warning::detected_source(BRANCH_BEHIND_BASE),
                )
                .source(provenant_records::Source {
                    layer: provenant_records::Layer::Detected,
                    origin: Some("writer branch repin".to_owned()),
                }),
            );
            submission_prompt.push_str("\n\n");
            submission_prompt.push_str(&warning);
        }

        // A crash can leave an accepted operation without a checkpoint
        // reference. Preserve its immutable labels, including legacy None.
        let mut accepted_knobs = None;
        let mut accepted_route = None;
        let mut accepted_retry_limit = None;
        let mut accepted_access = None;
        let mut accepted_skill_location = None;
        let mut accepted_attempt = None;
        let mut context = match &operation {
            Some(operation) => match store
                .operation_attempt(&operation.operation_id, &call)
                .and_then(|admitted| Ok((admitted, store.request(admitted, &call)?)))
            {
                Ok((admitted, request)) => {
                    let task = &request.task;
                    if accepted {
                        let session = owner.session(self.cancel)?;
                        let record = session.store.record(admitted, &call).map_err(run_error)?;
                        if record.prompt.bytes > provenant_records::run::RECORD_LIMIT {
                            return Err(FacadeError::StateCorrupt);
                        }
                        let mut bytes = Vec::new();
                        loop {
                            let offset = bytes.len() as u64;
                            let prompt = session
                                .store
                                .read_output(
                                    admitted,
                                    provenant_run::ArtifactPart::Prompt,
                                    provenant_run::ByteRange {
                                        offset,
                                        ..Default::default()
                                    },
                                    &session.context,
                                    &call,
                                )
                                .map_err(run_error)?;
                            if !prompt.complete
                                || prompt.len != record.prompt.bytes
                                || prompt.digest != task.prompt_digest
                                || prompt.offset != offset
                                || prompt.next_offset != offset + prompt.bytes.len() as u64
                                || prompt.next_offset > record.prompt.bytes
                                || (!prompt.eof && prompt.next_offset <= offset)
                                || prompt.eof != (prompt.next_offset == record.prompt.bytes)
                            {
                                return Err(FacadeError::StateCorrupt);
                            }
                            bytes.extend(prompt.bytes);
                            if prompt.eof {
                                break;
                            }
                        }
                        submission_prompt =
                            String::from_utf8(bytes).map_err(|_| FacadeError::StateCorrupt)?;
                        source_head = task.source_head.clone();
                        successor_head = task.head.clone();
                        accepted_knobs = Some(task.profile.overrides.clone());
                        accepted_route = Some(task.route.clone());
                        accepted_retry_limit = Some(task.profile.settlement_retry_limit);
                        accepted_access = Some(task.access.clone());
                        accepted_skill_location = Some(task.profile.require_skill_location);
                        accepted_attempt = Some(recovered_attempt_spec(task, node.attempt)?);
                    }
                    task.context.clone()
                }
                Err(provenant_run::RunError::NotFound)
                    if operation.state == provenant_run::OperationState::Prepared =>
                {
                    Some(node.context.clone())
                }
                Err(error) => return Err(run_error(error)),
            },
            None => Some(node.context.clone()),
        };
        if !accepted && let Some(context) = &mut context {
            context.writer_predecessor = writer_predecessor;
            context.delivery_queue_writer_adoption =
                node.context.delivery_queue_writer_adoption && writer_predecessor.is_some();
            if context.pr.is_none() {
                context.pr = reconciled_pr;
            }
            if let Some(spec) = node.attempt {
                for (field, configured) in [
                    ("token_budget", spec.token_budget.is_some()),
                    ("on_budget", spec.on_budget.is_some()),
                    ("budget_grace", spec.budget_grace.is_some()),
                ] {
                    if configured {
                        context.budget_sources.insert(
                            field.into(),
                            spec.budget_sources
                                .get(field)
                                .copied()
                                .unwrap_or(provenant_records::Layer::Template),
                        );
                    }
                }
            }
        }
        let source_commit = successor_head.as_deref().or(node.source_commit);
        let excluded_families = self.writer_families(node.distinct_from_attempts)?;
        // An exact route cannot be substituted, so required separation is
        // decided here, before any worktree advance or submission. An
        // accepted operation is observed as it was launched.
        if !accepted
            && let Some(model) = &route.model
            && exact_route_refused(model.as_str(), &excluded_families, node.require_distinct)
        {
            return Ok(StartedAttempt::Refused(AttemptObservation::Terminal {
                attempt_ref: None,
                outcome: GraphAttemptOutcome::AdapterUnavailable,
                launched: false,
                result: None,
                result_warning: None,
                result_path: None,
                branch: None,
                commit: None,
                stop: None,
                settlement_class: Some(provenant_records::run::SettlementClass::PreLaunchRefusal),
                cause: None,
            }));
        }
        // A shared writer was authorised and directory-bound by its
        // owning lane's guardian. Revalidate that grant before any Git
        // preparation, and never advance a checkout its parent still uses.
        let shared_writer = if !accepted && worktree.is_some() {
            let checkpoint = provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
                &self.factory.context.roots.state,
                self.factory.context.identity.project,
                &key.graph_id,
            )
            .map_err(graph_journal_error)?;
            let progress = checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.nodes.iter().find(|row| row.id == key.node_id));
            let shared = progress
                .and_then(|row| row.writer_binding.as_ref())
                .and_then(|binding| binding.shared_worktree.as_ref());
            if let Some(expected) = shared {
                let current = provenant_cli::fabric::inspect_checkout_subject(
                    &self.factory.context.roots,
                    writer_directory,
                )
                .map_err(|_| FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason: "shared worktree identity unavailable".into(),
                })?;
                if current.as_ref() != Some(expected) {
                    return Err(FacadeError::DispatchRefused {
                        fact: None,
                        disk: None,
                        reason: "shared worktree incarnation changed".into(),
                    });
                }
                Some(expected.clone())
            } else if let Some(creator) = progress.and_then(|row| row.provisioned_owner.as_ref()) {
                Some(
                    provenant_cli::fabric::provisioned_checkout_subject(
                        &self.factory.context.roots,
                        writer_directory,
                        creator,
                    )
                    .map_err(|_| FacadeError::DispatchRefused {
                        fact: None,
                        disk: None,
                        reason: "nested worktree creation authority changed".into(),
                    })?,
                )
            } else {
                None
            }
        } else {
            None
        };
        if let (Some(subject), Some(spec), Some(path)) =
            (shared_writer.as_ref(), node.attempt, worktree)
        {
            match claims::overlap_warnings(
                &store,
                &call,
                subject,
                Path::new(path),
                &spec.write_scope,
            ) {
                Ok(warnings) => {
                    for warning in warnings {
                        warn(warning.clone());
                        self.launch_warnings.push(warning);
                    }
                }
                Err(error) => {
                    let code = WarningCode("PATH_CLAIM_CHECK_UNAVAILABLE");
                    let warning = Warning::new(
                        code,
                        format!("could not inspect live path claims: {error}"),
                        provenant_records::warning::detected_source(code),
                    );
                    warn(warning.clone());
                    self.launch_warnings.push(warning);
                }
            }
        }
        // Preserve the specific detached-writer refusal in node status.
        if !accepted
            && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
            && worktree.is_some()
        {
            let writer = provenant_run::resolve_writer(
                    self.factory.context.roots.state.as_path(),
                    writer_directory,
                    writer_directory,
                )
                .map_err(|error| match error {
                    provenant_run::RunError::Validation("writer needs a branch") => {
                        FacadeError::DispatchRefused {
                            fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::DetachedWriter),
                            disk: None,
                            reason: "writer needs a branch".into(),
                        }
                    }
                    other => run_error(other),
                })?;
            if let Err(provenant_run::RunError::Validation("writer needs a branch")) =
                provenant_run::prepare_writer_with_facts(
                    &writer,
                    self.factory.context.roots.state.as_path(),
                    self.factory.context.identity.project,
                    &self.repo_facts,
                )
            {
                return Err(FacadeError::DispatchRefused {
                        fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::DetachedWriter),
                        disk: None,
                        reason: "writer needs a branch".into(),
                    });
            }
        }
        if !accepted
            && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
            && matches!(
                replay.delivery,
                Some(provenant_records::run::DeliveryMode::Integrate(_))
            )
            && let (Some(base), Some(commit)) = (
                self.integration_base.and_then(|bases| bases.integration),
                source_commit,
            )
        {
            let head = commit
                .parse()
                .map_err(|_| FacadeError::field("source.commit"))?;
            let descends = provenant_confinement::worktrees::with_context_using_facts(
                &self.factory.context.roots,
                &self.factory.home,
                &self.factory.cwd,
                &self.factory.clock,
                self.cancel,
                &self.repo_facts,
                |context| {
                    provenant_confinement::worktrees::is_ancestor(context, base, head, self.cancel)
                },
            )
            .map_err(|error| {
                let diagnostic = writer_dirty_diagnostic(&error);
                let error = graph_writer_initialisation_error(commit, error);
                match diagnostic {
                    Some(diagnostic) => self
                        .retain_graph_refusal(refusal, &error, diagnostic)
                        .err()
                        .unwrap_or(error),
                    None => error,
                }
            })?;
            if !descends {
                return Err(FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason: format!(
                        "node {}: lane source {head} does not descend from integration base {base}",
                        key.node_id
                    ),
                });
            }
        }
        if !accepted
                && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                // A continuation already owns its predecessor's checkout.
                // Preparation verifies its exact head and ancestry; advancing
                // it here would refuse the uncommitted work it must retain.
                && source_head.is_none()
                && shared_writer.is_none()
                && let (Some(path), Some(commit)) = (worktree, source_commit)
        {
            let head = commit
                .parse()
                .map_err(|_| FacadeError::field("source.commit"))?;
            self.fast_forward_writer(
                writer_directory,
                Path::new(path),
                head,
                node.writer_dirty_preflight,
            )
            .map_err(|error| {
                let diagnostic = writer_dirty_diagnostic(&error);
                let error = graph_writer_initialisation_error(commit, error);
                match diagnostic {
                    Some(diagnostic) => self
                        .retain_graph_refusal(refusal, &error, diagnostic)
                        .err()
                        .unwrap_or(error),
                    None => error,
                }
            })?;
        }
        // Delivery is a writer obligation independent of the receipt preset.
        let delivery = if let Some(mode) = replay.delivery {
            let branch = if let Some(path) = worktree {
                let writer = provenant_run::resolve_writer(
                    self.factory.context.roots.state.as_path(),
                    writer_directory,
                    Path::new(path),
                )
                .map_err(run_error)?;
                provenant_run::prepare_writer_with_facts(
                    &writer,
                    self.factory.context.roots.state.as_path(),
                    self.factory.context.identity.project,
                    &self.repo_facts,
                )
                .map_err(run_error)?
                .branch
                .as_str()
                .trim_start_matches("refs/heads/")
                .to_owned()
            } else {
                String::new()
            };
            // A continuing writer pushes only to the branch it continues.
            if let Some(bound) = replay.branch
                && bound != branch
            {
                return Err(FacadeError::DispatchRefused {
                    fact: None,
                    disk: None,
                    reason: format!(
                        "graph writer worktree is on {branch}, not the continued branch {bound}"
                    ),
                });
            }
            // A writer with no delivery never needs a forge: `destination`
            // returns `None` for `DeliveryMode::None` before reading any
            // remote. Resolving one here refuses a valid launch in a checkout
            // that declares none, leaving the sealed attempt without a
            // reference. Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
            if matches!(&mode, provenant_records::run::DeliveryMode::None) {
                None
            } else {
                let state = self.factory.context.roots.state.as_path();
                let ports = &self.factory.context.port_table;
                let forge = provenant_run::resolve_forge(state, self.factory.cwd.as_path(), ports)
                    .map_err(run_error)?;
                provenant_run::HostDeliveryContext::destination(mode.clone(), &branch, &forge)
                    .map_err(run_error)?
                    .map(|mut request| {
                        request.fast_forward_only = replay.fast_forward_only;
                        request
                    })
            }
        } else {
            None
        };
        let execution = provenant_fabric_runtime::graph::FileJournal::execution(
            &self.factory.context.roots.state,
            self.factory.context.identity.project,
            &key.graph_id,
        )
        .map_err(graph_journal_error)?;
        // Only the review fallback's empty range leaves a task without a base;
        // a read subject carries none, so its tasks keep the read base below.
        let subject_base_missing = execution
            .as_ref()
            .and_then(|execution| execution.document.subject.as_ref())
            .is_some_and(super::subject::is_empty_review_range);
        let review_launch = node.review_launch;
        let review_base = provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
            &self.factory.context.roots.state,
            self.factory.context.identity.project,
            &key.graph_id,
        )
        .map_err(graph_journal_error)?
        .and_then(|checkpoint| checkpoint.bases)
        .and_then(|bases| bases.review);
        // Writer separation is mandatory; sibling diversity remains a preference.
        let enforced = node.require_distinct && !excluded_families.is_empty();
        let mut avoided_families = excluded_families;
        let mut preferred_families = Vec::new();
        for family in node.family_peers {
            let target = if enforced {
                &mut preferred_families
            } else {
                &mut avoided_families
            };
            if !target.contains(family) {
                target.push(family.clone());
            }
        }
        let exclude = self.route_exclusions(route, node.prior_attempts, accepted);
        if !accepted {
            for warning in provenant_cli::route::discarded_graph_exclusion_warnings(
                &exclude,
                &self.factory.context.roots,
            ) {
                let warning = Warning::new(
                    provenant_records::warning::EXCLUSION_ADVISORY,
                    warning,
                    provenant_records::warning::detected_source(
                        provenant_records::warning::EXCLUSION_ADVISORY,
                    ),
                );
                warn(warning.clone());
                self.launch_warnings.push(warning);
            }
        }
        let base = if accepted {
            let operation = operation.as_ref().ok_or(FacadeError::StateCorrupt)?;
            let admitted = store
                .operation_attempt(&operation.operation_id, &call)
                .map_err(run_error)?;
            store
                .request(admitted, &call)
                .map_err(run_error)?
                .task
                .base_ref
                .clone()
        } else if subject_base_missing && node.review_base.is_none() {
            None
        } else if access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
            && source_commit.is_some()
        {
            let base = node
                .review_base
                .map(str::to_owned)
                // A review base recorded with the graph execution comes next,
                // then the integration base.
                .or_else(|| review_base.map(|base| base.to_string()))
                .or_else(|| {
                    self.integration_base
                        .and_then(|bases| bases.integration)
                        .map(|base| base.to_string())
                });
            if review_launch {
                let directory = worktree.map_or_else(
                    || self.factory.cwd.clone(),
                    |path| self.factory.cwd.join(path),
                );
                Some(self.factory.review_comparison_base(
                    &directory,
                    base.as_deref(),
                    source_commit.ok_or(FacadeError::StateCorrupt)?,
                )?)
            } else {
                // Ordinary source readers retain their source-commit fallback.
                base.or_else(|| source_commit.map(str::to_owned))
            }
        } else if foreign_writer {
            source_commit.map(str::to_owned)
        } else {
            source_commit.map(|head| {
                self.integration_base
                    .and_then(|bases| bases.integration)
                    .map_or_else(|| head.to_owned(), |base| base.to_string())
            })
        };
        // Peer roots retain the exact requested route, including family
        // exclusions, rather than resolving a second, broader alias.
        let peer_root = execution.as_ref().is_some_and(|execution| {
            execution
                .peer_origin_context
                .as_ref()
                .and_then(|origin| origin.node_id.as_deref())
                == Some(key.node_id.as_str())
        });
        // Tasks carry the admitted subject so a peer can compare its head with
        // the pinned one; graphs admitted before subjects leave it absent.
        let admitted_subject = execution
            .as_ref()
            .and_then(|execution| execution.document.subject.clone());
        if !accepted {
            // This owner is admitting a new leaf from the host-owned graph
            // scheduler. Bind the graph marker to the work key here; the
            // caller's WorkContext flag is never evidence, and accepted
            // operation replay keeps its immutable stored request above.
            let context = context.as_mut().ok_or(FacadeError::StateCorrupt)?;
            if context.graph_id.as_deref() != Some(key.graph_id.as_str())
                || context.node_id.as_deref() != Some(key.node_id.as_str())
                || context.attempt != Some(key.attempt)
            {
                return Err(FacadeError::StateCorrupt);
            }
            context.graph_template_launch = true;
        }
        if peer_root && !accepted {
            // The receiver's graph provenance was authenticated at peer
            // ingress; retain its source-head binding for the local attempt.
            source_head = route.peer_source_head.clone();
        }
        let accepted_route = accepted_route.or(if peer_root {
            route.peer_requested_route.clone()
        } else {
            None
        });
        // A node may name a small project-relative fixture. Stage its bytes
        // with the submission so the same file is placed in every host's
        // scratch; a peer never needs a path into this host's checkout. Bytes
        // already carried across a boundary take precedence over a path.
        let fixture = node.fixture_bytes.cloned().or_else(|| {
            node.fixture.and_then(|name| {
                let path = std::path::Path::new(name);
                if path.is_absolute() || name.contains("..") {
                    return None;
                }
                let content = std::fs::read(self.factory.cwd.join(path)).ok()?;
                let staged = provenant_records::run::StagedFixture {
                    name: path.file_name()?.to_string_lossy().into_owned(),
                    content,
                };
                staged.is_valid().then_some(staged)
            })
        });
        let input = SubmitInput::Submit {
            extra: Default::default(),
            origin_work_key: Some(
                key.work_key(self.factory.context.identity.project)
                    .map_err(|_| FacadeError::StateCorrupt)?,
            ),
            operation_id,
            task: SubmittedTask {
                require_skill_location: accepted_skill_location
                    .unwrap_or(node.require_skill_location),
                context,
                extra: Default::default(),
                build_heavy: node.attempt.is_some_and(|spec| spec.build_heavy),
                base,
                fixture,
                head: source_commit.map(str::to_owned),
                subject: admitted_subject,
                source_head,
                account: route
                    .account
                    .clone()
                    .or_else(|| node.attempt.and_then(|spec| spec.account.clone())),
                access,
                capabilities: None,
                capability_sources: Default::default(),
                // A peer creates its writer in its own clone from the retained
                // branch and base; a caller worktree path never crosses.
                worktree: (access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
                    && !host.is_some_and(|host| host != self.factory.context.identity.host))
                .then(|| worktree.map(str::to_owned))
                .flatten(),
                prompt: submission_prompt,
                title: Some(node.title.to_owned()),
                route: accepted_route.unwrap_or(match route.selector() {
                    provenant_fabric_runtime::graph::GraphRouteSelector::Adapter(name) => {
                        provenant_records::run::RequestedRoute::Adapter {
                            extra: Default::default(),
                            required_family: route.required_family.clone(),
                            provider: route.adapter.clone().ok_or_else(|| {
                                FacadeError::invalid(InvalidInputDetail::FieldDecode {
                                    field: "route.adapter".into(),
                                    missing: true,
                                })
                            })?,
                            name: name.to_owned(),
                        }
                    }
                    provenant_fabric_runtime::graph::GraphRouteSelector::Model(model) => {
                        let adapter = route.adapter.as_ref().ok_or_else(|| {
                            FacadeError::invalid(InvalidInputDetail::FieldDecode {
                                field: "route.adapter".into(),
                                missing: true,
                            })
                        })?;
                        provenant_records::run::RequestedRoute::Model {
                            extra: Default::default(),
                            required_family: route.required_family.clone(),
                            provider: adapter.clone(),
                            model: model.to_owned(),
                            effort: route.effort.clone(),
                        }
                    }
                    provenant_fabric_runtime::graph::GraphRouteSelector::Alias(_)
                        if !avoided_families.is_empty() =>
                    {
                        provenant_records::run::RequestedRoute::Review {
                            extra: Default::default(),
                            required_family: route.required_family.clone(),
                            name: route.alias.clone().ok_or_else(|| {
                                FacadeError::invalid(InvalidInputDetail::FieldDecode {
                                    field: "route.alias".into(),
                                    missing: true,
                                })
                            })?,
                            effort: route.effort.clone(),
                            excluded_families: avoided_families,
                            distinct_families: true,
                            preferred_families,
                            require_distinct: enforced,
                        }
                    }
                    provenant_fabric_runtime::graph::GraphRouteSelector::Alias(_) => {
                        provenant_records::run::RequestedRoute::Named {
                            extra: Default::default(),
                            required_family: route.required_family.clone(),
                            name: route.alias.clone().ok_or_else(|| {
                                FacadeError::invalid(InvalidInputDetail::FieldDecode {
                                    field: "route.alias".into(),
                                    missing: true,
                                })
                            })?,
                            effort: route.effort.clone(),
                        }
                    }
                }),
                timeout_seconds: lane_timeout(
                    node.deadline_seconds,
                    self.factory.context.config.limits.graph_timeout_seconds,
                ),
                delivery_id: None,
                deliver: delivery,
                // A peer provisions the writer itself, so it needs the branch
                // and base intent. A local graph has already provisioned it.
                new_worktree: if host.is_some_and(|host| host != self.factory.context.identity.host)
                {
                    node.attempt.and_then(|spec| spec.new_worktree.clone())
                } else {
                    None
                },
            },
            knobs: if let Some(knobs) = accepted_knobs {
                knobs
            } else {
                let mut knobs = Vec::new();
                // Isolation and receipts, from the node or its preset.
                if let Some(spec) = node.attempt {
                    knobs.extend(spec.lane_knobs());
                }
                if !exclude.is_empty() {
                    knobs.push(format!(
                        "routing.graph_exclude={}",
                        serde_json::Value::from(exclude)
                    ));
                }
                knobs
            },
            host,
        };
        // Operation replay is actor-scoped: submit under the recorded launch actor.
        owner.actor = Some(
            provenant_core::ActorId::parse(replay.launch_actor)
                .map_err(|_| FacadeError::StateCorrupt)?,
        );
        let mut grants = self.launch_grants(
            &node,
            key,
            access,
            worktree,
            accepted,
            accepted_access,
            source_commit,
        )?;
        if !accepted
            && access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
            && source_commit.is_none()
            && let Some(warning) = provenant_cli::run::read_only_checkout_warning(
                self.factory.context.roots.state.as_path(),
                grants
                    .source_root
                    .as_deref()
                    .unwrap_or(self.factory.context.selected_project()),
                &provenant_cli::run::ReadSourceChoice::automatic(),
            )
        {
            let warning = Warning::new(
                SETTING_LAYER_IGNORED,
                warning,
                provenant_records::warning::detected_source(SETTING_LAYER_IGNORED),
            );
            warn(warning.clone());
            self.launch_warnings.push(warning);
        }
        if !accepted {
            grants.shared_worktree = shared_writer;
        }
        if peer_root && let Some(network) = route.peer_network {
            grants.network = Some(network);
        }
        let peer_retry = peer_root.then_some(route.peer_retry_limit).flatten();
        let retry_limit = accepted_retry_limit.unwrap_or(peer_retry.or(node.settlement_retries));
        let mut attempt = accepted_attempt.unwrap_or_else(|| {
            let mut attempt = node.attempt.cloned().unwrap_or_default();
            attempt.account = route.account.clone().or(attempt.account);
            attempt
        });
        if !accepted
            && host.is_some_and(|host| host != self.factory.context.identity.host)
            && access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite
            && attempt
                .new_worktree
                .as_ref()
                .is_some_and(|tree| tree.default_base)
        {
            let (mode, warnings) = carry::advisory_mode(self, attempt.carry_uncommitted);
            self.launch_warnings.extend(warnings);
            attempt.carry_uncommitted = Some(mode);
        }
        // The graph checkpoint owns worktree provisioning on this host;
        // submission receives the resolved checkout, never a second request to
        // create it. A peer has no access to this host's checkout, so it keeps
        // the branch and base and creates the worktree in its own clone.
        if !host.is_some_and(|host| host != self.factory.context.identity.host) {
            attempt.new_worktree = None;
        }
        // These fields have already been resolved into the sealed access grants.
        attempt.write_roots.clear();
        attempt.capabilities.clear();
        // Preparation runs in a detached launcher whose stderr is not
        // inherited. Warn at the graph host and retain it in the checkpoint.
        if !accepted
                && let Some(model) = route.model.as_deref()
                && let Ok(model) = provenant_routing::ModelId::parse(model)
                // Resolve against the same effective (bundled + discovered)
                // catalogue and revision that `route list` uses, not just the
                // bundled catalogue. One lookup shared with the CLI warning.
                && let Ok(catalogue) =
                    provenant_cli::route::effective_catalogue(&self.factory.context.roots)
                // A family alias such as `claude/sonnet` resolves to an exact
                // catalogue model; only a truly unknown model warns here.
                && catalogue.resolve_family_alias(&model).is_none()
        {
            let nearer = catalogue.nearest_models(&model);
            let repair = if nearer.is_empty() {
                "check `provenant route list` for catalogued model ids".to_owned()
            } else {
                format!("did you mean {}?", nearer.join(", "))
            };
            let warning = format!(
                "model {} is not in the catalogue; {repair}; the provider will check it",
                model.as_str()
            );
            let warning = Warning::new(
                provenant_records::warning::MODEL_SEGMENT_ADVISORY,
                warning,
                provenant_records::warning::detected_source(
                    provenant_records::warning::MODEL_SEGMENT_ADVISORY,
                ),
            )
            .subject(model.as_str());
            warn(warning.clone());
            self.launch_warnings.push(warning);
        }
        let receipt = owner.submit_with_access(
            input,
            self.writer_launch_budget.unwrap_or(self.budget),
            self.cancel,
            grants,
            retry_limit,
            Some(attempt),
            &self.repo_facts,
            provenant_run::RefusalContext {
                chair: refusal.chair.clone(),
                intent_digest: Some(refusal.digest),
            },
        )?;
        if let Some(reason) = blocked(&*owner)? {
            return Err(FacadeError::DispatchRefused {
                fact: None,
                disk: None,
                reason,
            });
        }
        owner.actor = None;
        let target = receipt
            .targets
            .into_iter()
            .next()
            .ok_or(FacadeError::StateCorrupt)?;
        let reference = Some(format!(
            "{}/{}",
            target.host.unwrap_or(self.factory.context.identity.host),
            target.attempt
        ));
        Ok(StartedAttempt::Submitted { target, reference })
    }

    /// Resolve the access grants for a graph attempt: the sealed grants of an
    /// accepted operation, else the node's own. Its own frame keeps the
    /// capability and source-binding temporaries off the launch frame (#3542).
    #[allow(clippy::too_many_arguments)]
    fn launch_grants(
        &self,
        node: &provenant_fabric_runtime::graph::NodeInput<'_>,
        key: &provenant_fabric_runtime::graph::AttemptKey,
        access: provenant_fabric_runtime::graph::AccessMode,
        worktree: Option<&str>,
        accepted: bool,
        accepted_access: Option<provenant_records::run::LaneAccessRequest>,
        source_commit: Option<&str>,
    ) -> Result<provenant_records::run::LaneAccessRequest, FacadeError> {
        let mut grants = if accepted {
            accepted_access.ok_or(FacadeError::StateCorrupt)?
        } else {
            let configured =
                    provenant_cli::config_reads::read_for(&self.factory.context.roots, "run")
                        .map_err(|error| {
                            FacadeError::invalid(
                                provenant_fabric_runtime::coordination::facade::InvalidInputDetail::Configuration(
                                    error.to_string(),
                                ),
                            )
                        })?;
            let default_grants = configured
                .domains
                .lanes
                .projects
                .get(&self.factory.context.identity.project.to_string())
                .map(|settings| settings.grants.as_slice())
                .unwrap_or_default();
            let mut capabilities = node
                .capabilities
                .unwrap_or_else(|| {
                    if access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite {
                        default_grants
                    } else {
                        &[]
                    }
                })
                .to_vec();
            capabilities.retain(|name| {
                if name == "postgres" {
                    true
                } else {
                    provenant_records::warning::warn(
                        provenant_records::warning::Warning::new(
                            provenant_records::warning::CONFIG_KEY_IGNORED,
                            format!("ignored unknown node or project capability {name}"),
                            provenant_records::warning::detected_source(
                                provenant_records::warning::CONFIG_KEY_IGNORED,
                            ),
                        )
                        .subject(name.clone()),
                    );
                    false
                }
            });
            if access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite {
                for capability in &self.capability_overrides.0 {
                    if capability != "postgres" {
                        provenant_records::warning::warn(
                            provenant_records::warning::Warning::new(
                                provenant_records::warning::CONFIG_KEY_IGNORED,
                                format!("ignored unknown flag capability {capability}"),
                                provenant_records::warning::detected_source(
                                    provenant_records::warning::CONFIG_KEY_IGNORED,
                                ),
                            )
                            .subject(capability.clone()),
                        );
                        continue;
                    }
                    if !capabilities.contains(capability) {
                        capabilities.push(capability.clone());
                    }
                }
            }
            capabilities.retain(|name| !self.capability_overrides.1.contains(name));
            let mut seen = std::collections::BTreeSet::new();
            capabilities.retain(|name| seen.insert(name.clone()));
            let mut grants = provenant_cli::run::lane_access(
                &provenant_cli::Access {
                    attach: node
                        .attachments
                        .iter()
                        .map(|path| {
                            let path = Path::new(path);
                            if path.is_absolute() {
                                path.to_owned()
                            } else {
                                self.factory.cwd.join(path)
                            }
                        })
                        .collect(),
                    mode: Some(provenant_cli::Mode::ReadOnly),
                    worktree: (access
                        == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite)
                        .then(|| worktree.map(PathBuf::from))
                        .flatten(),
                    read_root: node
                        .read_roots
                        .iter()
                        .map(|root| {
                            let path = Path::new(root);
                            if path.is_absolute() {
                                path.to_path_buf()
                            } else {
                                self.factory.cwd.join(path)
                            }
                        })
                        .collect(),
                    write_root: node
                        .attempt
                        .into_iter()
                        .flat_map(|spec| &spec.write_roots)
                        .map(|root| {
                            let path = Path::new(root);
                            if path.is_absolute() {
                                path.to_owned()
                            } else {
                                self.factory.cwd.join(path)
                            }
                        })
                        .collect(),
                    network: self.network,
                    capability: capabilities.clone(),
                    no_capability: Vec::new(),
                },
                self.factory.context.roots.state.as_path(),
            )
            .map_err(dispatch_refused)?;
            if !capabilities.is_empty() {
                grants.capability_sources = capabilities
                    .iter()
                    .map(|name| {
                        let inherited = node
                            .capability_sources
                            .and_then(|sources| sources.get(name));
                        let source = if self.capability_overrides.0.contains(name) {
                            "flag"
                        } else {
                            inherited.map(String::as_str).unwrap_or(
                                if node.capabilities.is_some() {
                                    "node"
                                } else {
                                    "project default"
                                },
                            )
                        };
                        (name.clone(), source.to_owned())
                    })
                    .collect();
            }
            grants
        };
        if !accepted {
            grants.inputs_root = node.inputs_dir.map(Path::to_path_buf);
            if access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
                && source_commit.is_none()
                && let Some(directory) = node.attempt.and_then(|spec| spec.directory.as_deref())
            {
                bind_child_directory(
                    &mut grants,
                    directory,
                    self.factory.context.roots.state.as_path(),
                    self.network,
                )?;
            }
            // Every pinned reader resolves and verifies its snapshot source.
            if access == provenant_fabric_runtime::graph::AccessMode::ReadOnly
                && source_commit.is_some()
            {
                grants.source_root = {
                    let path = worktree.map_or(self.factory.context.selected_project(), Path::new);
                    let source = provenant_run::resolve_reader_source(
                        self.factory.context.roots.state.as_path(),
                        self.factory.context.selected_project(),
                        path,
                    )
                    .map_err(|_| FacadeError::DispatchRefused {
                        fact: None,
                        disk: None,
                        reason: "graph source checkout is no longer registered with the project"
                            .into(),
                    })?;
                    let directory = provenant_os::fs::Directory::open_anchor(&source)
                        .map_err(|error| FacadeError::filesystem(&error))?;
                    let identity = directory
                        .metadata()
                        .map_err(|error| FacadeError::filesystem(&error))?;
                    let source_identity = provenant_records::run::SourceIdentity {
                        device: identity.device,
                        inode: identity.inode,
                    };
                    if worktree.is_some()
                        && let Some(checkpoint) =
                            provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
                                &self.factory.context.roots.state,
                                self.factory.context.identity.project,
                                &key.graph_id,
                            )
                            .map_err(graph_journal_error)?
                        && !writer_source_identity_matches(&checkpoint, &source, &source_identity)
                    {
                        return Err(FacadeError::DispatchRefused {
                            fact: None,
                            disk: None,
                            reason: "graph source checkout identity changed or is unavailable"
                                .into(),
                        });
                    }
                    grants.source_identity = Some(source_identity);
                    grants.source_live = false;
                    // The immutable view replaces live-source grants.
                    grants.read_roots.retain(|root| !root.starts_with(&source));
                    Some(source)
                };
            }
        }
        Ok(grants)
    }

    /// The commit that proves a writer left new work. A pushed delivery receipt
    /// carries the published head even when the sealed writer head is absent or
    /// still names the launch commit, so both are settlement evidence (#4065).
    fn observed_writer_commit(
        writer_head: Option<provenant_core::GitOid>,
        delivery: Option<&provenant_records::run::WriterDeliveryReceipt>,
        access: provenant_fabric_runtime::graph::AccessMode,
        source_commit: Option<&str>,
    ) -> Option<String> {
        if access != provenant_fabric_runtime::graph::AccessMode::WorktreeWrite {
            return source_commit.map(str::to_owned);
        }
        delivery
            .and_then(|receipt| receipt.head)
            .or(writer_head)
            .map(|head| head.to_string())
    }

    /// Report a terminal attempt as its graph observation.
    fn observe_terminal(
        &mut self,
        owner: &mut BinaryRunOwner<'_>,
        attempt: provenant_records::run::AttemptRecord,
        target: AttemptTarget,
        reference: Option<String>,
        expected_origin: provenant_records::WorkKey,
        facts: TerminalFacts<'_>,
    ) -> Result<provenant_fabric_runtime::graph::AttemptObservation, FacadeError> {
        use provenant_fabric_runtime::graph::{AttemptObservation, GraphAttemptOutcome};
        let TerminalFacts {
            required_verdict,
            deadline_seconds,
            source_commit,
            access,
            own_commit,
        } = facts;
        use provenant_records::run::{AttemptState, Outcome};
        match attempt.state {
            AttemptState::Terminal { outcome, .. } => {
                let remote = target
                    .host
                    .is_some_and(|host| host != self.factory.context.identity.host);
                let store = owner.store_with_facts(self.cancel, &self.repo_facts)?;
                let call = owner.call(self.cancel);
                let custody = if remote {
                    attempt
                        .peer
                        .graph_custody_released
                        .ok_or(provenant_run::RunError::NotFound)
                } else {
                    store.custody_released(&attempt, &call)
                };
                match custody {
                    Ok(true) => {}
                    Ok(false) => {
                        return Ok(AttemptObservation::Pending {
                            attempt_ref: reference,
                        });
                    }
                    Err(_) => {
                        return Ok(AttemptObservation::Unknown {
                            attempt_ref: reference,
                        });
                    }
                }
                // Admission is not a launch receipt. Keep observing the
                // predecessor while its accepted steer is unresolved, so a
                // concurrently advancing graph cannot adopt a refused child.
                if attempt.steering.as_ref().is_some_and(|steering| {
                    steering.state == provenant_records::run::SteeringState::Queued
                }) {
                    return Ok(AttemptObservation::Pending {
                        attempt_ref: reference,
                    });
                }
                let own_head = if !remote && launched_for_own_commit(&attempt, own_commit.as_ref())
                {
                    // The commit is idempotent by its attempt marker. A failure
                    // keeps this attempt observed, with its cause, instead of
                    // aborting the whole advance; the next observation retries.
                    match commit_own::commit_shared_child(self, &store, &call, &attempt) {
                        Ok(head) => head,
                        Err(error) => {
                            let code = WarningCode("OWN_COMMIT_PENDING");
                            let warning = Warning::new(
                                code,
                                format!(
                                    "OWN_COMMIT_PENDING: claimed paths are not committed yet: {error}"
                                ),
                                provenant_records::warning::detected_source(code),
                            );
                            warn(warning.clone());
                            self.launch_warnings.push(warning);
                            return Ok(AttemptObservation::Unknown {
                                attempt_ref: reference,
                            });
                        }
                    }
                } else {
                    None
                };
                let pickup = if remote {
                    None
                } else {
                    store
                        .delivered(attempt.attempt_id, &call)
                        .map_err(run_error)?
                };
                let effective_delivery = pickup
                    .as_ref()
                    .map(|(_, receipt)| receipt)
                    .or(attempt.delivery.as_ref());
                let launched = attempt.provider_launched.unwrap_or(true);
                let preparation_cause = attempt.state.cause();
                // The sealed stop, else the cause the outcome names.
                let stop = attempt
                    .state
                    .stop()
                    .map(|stop| stop.cause())
                    // A model unavailability keeps its own fallback scope even
                    // when a preparation failure was also recorded: the graph
                    // outcome folds the two, so read the sealed projection.
                    .or_else(|| {
                        provenant_records::settlement::project::model_unavailable(&attempt.state)
                            .then_some(provenant_records::StopCause::ModelUnavailable)
                    })
                    .or_else(|| {
                        attempt.preparation_failure.map(|failure| match failure {
                            provenant_records::run::PreparationFailure::AuthRequired => {
                                provenant_records::StopCause::AuthRequired
                            }
                            provenant_records::run::PreparationFailure::UsageLimited => {
                                provenant_records::StopCause::UsageLimited
                            }
                            provenant_records::run::PreparationFailure::AdapterUnavailable => {
                                provenant_records::StopCause::AdapterUnavailable
                            }
                            provenant_records::run::PreparationFailure::GitSetup => {
                                provenant_records::StopCause::PreparationRefused
                            }
                        })
                    })
                    .or_else(|| preparation_cause.map(provenant_records::StopCause::from))
                    .or(provenant_records::StopCause::of_outcome(outcome));
                let (result, result_warning) = if attempt.result.is_some()
                    || outcome == Outcome::Succeeded
                {
                    let captured = if remote {
                        remote_graph_result(
                            owner,
                            &target,
                            &expected_origin,
                            required_verdict,
                            self.budget,
                            self.cancel,
                        )
                        .map_err(|_| provenant_run::RunError::NotFound)
                    } else {
                        store.stream_result(attempt.attempt_id, &call, required_verdict)
                    };
                    match captured {
                        Ok(mut result) => {
                            if remote && let Some(artifact) = &attempt.result {
                                // Output pages prove the redacted stream. Graph evidence
                                // still identifies the peer's sealed original artifact.
                                result.digest = artifact.digest.to_string();
                                // The peer's sealed settlement handoff, carried in its
                                // status; the output pages hold only text.
                                result.handoff = peer_handoff(attempt.peer.graph_handoff.as_ref());
                            }
                            (Some(Box::new(result)), None)
                        }
                        Err(error) => (None, Some(graph_result_warning(&error).to_owned())),
                    }
                } else {
                    (None, None)
                };
                let result_warning = result_warning.or_else(|| {
                    if let Some(delivery) = effective_delivery
                        && let provenant_records::run::WriterDeliveryState::Failed {
                            stage: provenant_records::run::WriterDeliveryStage::Integrate,
                            reason: Some(reason),
                            ..
                        } = &delivery.state
                    {
                        Some(reason.clone())
                    } else {
                        None
                    }
                });
                let stop = if effective_delivery.as_ref().is_some_and(|delivery| {
                    matches!(
                        &delivery.state,
                        provenant_records::run::WriterDeliveryState::Failed {
                            stage: provenant_records::run::WriterDeliveryStage::Integrate,
                            reason: Some(reason),
                            ..
                        } if reason.contains("integration_conflict") || reason.contains("merge conflict")
                    )
                }) {
                    Some(provenant_records::StopCause::Blocked)
                } else {
                    stop
                };
                // An unverifiable artifact is an integrity failure regardless
                // of the provider outcome. In particular, it must not trigger
                // route fallback for authentication or quota failures.
                Ok(AttemptObservation::Terminal {
                    launched,
                    attempt_ref: reference,
                    outcome: if result_warning.is_some()
                        || (outcome == Outcome::Succeeded
                            && effective_delivery.is_some_and(|delivery| {
                                matches!(
                                    delivery.state,
                                    provenant_records::run::WriterDeliveryState::Failed { .. }
                                        | provenant_records::run::WriterDeliveryState::NotAttempted { .. }
                                )
                            }))
                    {
                        GraphAttemptOutcome::OtherFailure
                    } else if outcome == Outcome::TimedOut
                        && deadline_seconds.is_some()
                        && preparation_cause != Some(provenant_records::run::CauseKind::TokenBudget)
                    {
                        GraphAttemptOutcome::DeadlineExceeded
                    } else {
                        provenant_records::settlement::project::graph_outcome(&attempt)
                            .unwrap_or(GraphAttemptOutcome::OtherFailure)
                    },
                    result,
                    result_warning,
                    result_path: attempt.result.as_ref().and_then(|_| {
                        (target.host.unwrap_or(self.factory.context.identity.host)
                            == self.factory.context.identity.host)
                            .then(|| {
                                self.factory
                                    .context
                                    .roots
                                    .state
                                    .as_path()
                                    .join(format!(
                                        "attempts/{}/result.md", attempt.attempt_id
                                    ))
                                    .to_string_lossy()
                                    .into_owned()
                            })
                    }),
                    branch: effective_delivery.map(|receipt| receipt.branch.clone()),
                    commit: own_head.or_else(|| Self::observed_writer_commit(
                        attempt.head,
                        effective_delivery,
                        access,
                        source_commit,
                    )),
                    stop,
                    settlement_class: attempt.settlement_class,
                    cause: attempt.state.cause().map(Box::new),
                })
            }
            _ => Ok(AttemptObservation::Pending {
                attempt_ref: reference,
            }),
        }
    }
}

/// Resolve the host/project `on_stop` defaults once, before the graph is
/// admitted. Configured keys are sealed in the checkpoint; a pinned cause is
/// reported with its source and keeps its table row.
fn resolve_on_stop_defaults(
    roots: &provenant_core::Roots,
    project: &str,
) -> provenant_records::OnStop {
    let config = match provenant_cli::config_reads::read_for(roots, "fabric") {
        Ok(config) => config,
        Err(error) => {
            let error = provenant_records::diagnostic::safe_text(&error.to_string());
            warn(Warning::new(
                provenant_records::warning::CONFIG_UNAVAILABLE,
                format!("graph on_stop defaults unavailable ({error}); using the stop table"),
                provenant_records::warning::detected_source(
                    provenant_records::warning::CONFIG_UNAVAILABLE,
                ),
            ));
            return provenant_records::OnStop::default();
        }
    };
    // A project entry overrides the host entry for the same cause; both are
    // typed domain settings, so an unknown cause or step is refused where
    // the configuration is read, with its file and line.
    let graph = &config.domains.fabric.graph;
    // The layer is the scope of the entry; the origin names where it was set.
    let source = |layer: provenant_records::Layer, key: String| provenant_records::Source {
        layer,
        origin: provenant_cli::config_reads::origin_source(config.origins.get(&key), &key)
            .origin
            .or(Some(key)),
    };
    let host = graph.on_stop.iter().map(|(cause, step)| {
        (
            cause,
            step,
            source(
                provenant_records::Layer::Host,
                format!("domains.fabric.graph.on_stop.{cause}"),
            ),
        )
    });
    let scoped = graph
        .projects
        .get(project)
        .into_iter()
        .flat_map(|scoped| scoped.on_stop.iter())
        .map(|(cause, step)| {
            (
                cause,
                step,
                source(
                    provenant_records::Layer::Project,
                    format!("domains.fabric.graph.projects.{project}.on_stop.{cause}"),
                ),
            )
        });
    let mut steps = std::collections::BTreeMap::new();
    for (cause, step, source) in host.chain(scoped) {
        if cause.rule().pinned {
            warn(Warning::new(
                provenant_records::warning::ON_STOP_PINNED_IGNORED,
                format!("on_stop.{cause} is pinned; the table default is kept"),
                source,
            ));
            continue;
        }
        steps.insert(cause, step);
    }
    provenant_records::OnStop::new(steps)
}

/// How a new graph attempt was admitted.
enum StartedAttempt {
    /// Submitted; observe the attempt it created.
    Submitted {
        target: AttemptTarget,
        reference: Option<String>,
    },
    /// Refused before launch; this is the node's observation.
    Refused(provenant_fabric_runtime::graph::AttemptObservation),
}

/// The node facts a terminal observation reads.
struct TerminalFacts<'a> {
    required_verdict: Option<&'a str>,
    deadline_seconds: Option<u64>,
    source_commit: Option<&'a str>,
    access: provenant_fabric_runtime::graph::AccessMode,
    own_commit: Option<provenant_records::lane_contract::CommitChoice>,
}

fn launched_for_own_commit(
    attempt: &provenant_records::run::AttemptRecord,
    choice: Option<&provenant_records::lane_contract::CommitChoice>,
) -> bool {
    choice == Some(&provenant_records::lane_contract::CommitChoice::OwnPaths)
        && attempt.provider_launched.unwrap_or(true)
}

/// Resolve the existing routing allowance, retaining explicit zero overrides.
pub(crate) fn graph_retry_allowance(
    node: &provenant_records::ExtraFields,
    graph: &provenant_records::ExtraFields,
    roots: &provenant_core::Roots,
) -> (u8, String) {
    let mut warnings = Vec::new();
    let mut read = |extra: &provenant_records::ExtraFields, key: &str| -> Option<u8> {
        let retry = extra.get(key)?;
        let value = retry
            .get("retries")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value <= provenant_routing::MAX_SETTLEMENT_RETRIES);
        if value.is_none() {
            warnings.push(format!("ignored invalid {key}.retries"));
        }
        value
    };
    let node_retry = read(node, "retry");
    let preset_retry = read(node, "preset_retry").or_else(|| read(graph, "retry"));
    let host = match provenant_cli::config_reads::read_for(roots, "run") {
        Ok(config) => matches!(
            config.origins.get("domains.routing.settlement_retries"),
            Some(provenant_config::Origin::User { .. })
        )
        .then_some(config.domains.routing.settlement_retries),
        Err(_) => {
            warnings.push("host retry setting unavailable".into());
            None
        }
    };
    let allowance = provenant_routing::resolve_retry_allowance(node_retry, preset_retry, host);
    let origin = match allowance.origin {
        provenant_routing::RetryAllowanceOrigin::Node => "node",
        provenant_routing::RetryAllowanceOrigin::Preset => "preset",
        provenant_routing::RetryAllowanceOrigin::HostConfig => "host_config",
        provenant_routing::RetryAllowanceOrigin::ShippedDefault => "shipped_default",
    };
    let origin = if warnings.is_empty() {
        origin.to_owned()
    } else {
        format!("{origin}; {}", warnings.join("; "))
    };
    (allowance.retries, origin)
}

/// A graph document's optional top-level `exclude` list of route-exclusion
/// selectors, inherited by every node it launches.
pub(crate) fn graph_exclude(
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Vec<String> {
    document.exclude.clone()
}
/// A graph document's optional top-level `network` (`"on"` or `"off"`),
/// applied to every node lane it launches.
pub(crate) fn graph_network(
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> Option<provenant_cli::Switch> {
    document.network.map(|network| match network {
        provenant_fabric_runtime::graph::GraphNetwork::On => provenant_cli::Switch::On,
        provenant_fabric_runtime::graph::GraphNetwork::Off => provenant_cli::Switch::Off,
    })
}

pub(crate) fn graph_capability_overrides(
    document: &provenant_fabric_runtime::graph::GraphDocument,
) -> (Vec<String>, Vec<String>) {
    (document.capability.clone(), document.no_capability.clone())
}

/// The full catalogue id a prior attempt ran, as stored on its applied route.
/// Graph admission already fixed this spelling; parsing it here only confirms
/// it, and never derives an id from the adapter and the text again.
fn prior_model(model: Option<&str>) -> Option<String> {
    let model = model?;
    Some(
        provenant_routing::ModelId::parse(model)
            .ok()?
            .as_str()
            .to_owned(),
    )
}

// Spec: docs/specs/fabric.md#one-model-spelling-per-route
#[test]
fn retry_prior_model_uses_the_stored_full_id() {
    assert_eq!(
        prior_model(Some("claude/claude-sonnet-5-5")).as_deref(),
        Some("claude/claude-sonnet-5-5")
    );
    assert_eq!(
        prior_model(Some("opencode/opencode-go/hy4-flash-free")).as_deref(),
        Some("opencode/opencode-go/hy4-flash-free")
    );
    assert_eq!(prior_model(None), None);
    assert_eq!(prior_model(Some("claude-sonnet-5-5")), None);
}

// A pushed delivery head is settlement evidence even when the writer sealed no
// head of its own (#4065). Without this the inline implement writer's done
// handoff and pushed commit cannot clear commits_ahead, so a successful attempt
// settles other_failure and its dependants stay closed.
#[test]
fn pushed_delivery_head_proves_writer_commit_when_sealed_head_is_absent() {
    use provenant_fabric_runtime::graph::AccessMode;
    let base = "1".repeat(40);
    let pushed = "2".repeat(40);
    let receipt = provenant_records::run::WriterDeliveryReceipt {
        extra: Default::default(),
        schema_version: 1,
        remote: provenant_records::run::Destination::from_name("origin").expect("fixture remote"),
        branch: "fix/4065".into(),
        head: Some(provenant_core::GitOid::parse(&pushed).unwrap()),
        review_acceptance: None,
        state: provenant_records::run::WriterDeliveryState::Delivered {
            extra: Default::default(),
            pull_request: None,
        },
        integration: None,
    };
    // The published head stands in for a missing sealed writer head.
    assert_eq!(
        BoundGraphOwner::observed_writer_commit(
            None,
            Some(&receipt),
            AccessMode::WorktreeWrite,
            Some(&base),
        )
        .as_deref(),
        Some(pushed.as_str())
    );
    // The sealed writer head still works when nothing was pushed.
    let sealed = provenant_core::GitOid::parse(&pushed).unwrap();
    assert_eq!(
        BoundGraphOwner::observed_writer_commit(
            Some(sealed),
            None,
            AccessMode::WorktreeWrite,
            Some(&base),
        )
        .as_deref(),
        Some(pushed.as_str())
    );
    // Read-only nodes keep their source commit as before.
    assert_eq!(
        BoundGraphOwner::observed_writer_commit(
            Some(sealed),
            Some(&receipt),
            AccessMode::ReadOnly,
            Some(&base),
        )
        .as_deref(),
        Some(base.as_str())
    );
}

/// Whether required family separation refuses an exact route before launch.
/// As for routed reviews, a model whose family is unknown is treated as
/// excluded once any family is.
pub(crate) fn exact_route_refused(
    model: &str,
    excluded_families: &[String],
    require_distinct: bool,
) -> bool {
    require_distinct
        && !excluded_families.is_empty()
        && provenant_cli::run::model_family(model)
            .is_none_or(|family| excluded_families.contains(&family))
}

pub(crate) fn restore_scratch_source(
    directory: &Path,
    name: &std::ffi::OsStr,
    bytes: &[u8],
) -> Result<(), FacadeError> {
    let directory = provenant_os::fs::Directory::open_anchor(directory)
        .map_err(|_| FacadeError::io("access graph scratch"))?;
    match directory.metadata_at(name, false) {
        Ok(_) => Ok(()),
        Err(error) if error.is_not_found() => {
            match directory.atomic_write(
                name,
                ".graph-recover.",
                bytes,
                provenant_os::fs::PublishMode::NoReplace,
            ) {
                Ok(publication) => publication
                    .require_durable()
                    .map_err(|_| FacadeError::io("restore graph scratch")),
                Err(error) if error.is_exists() => Ok(()),
                Err(_) => Err(FacadeError::io("restore graph scratch")),
            }
        }
        Err(_) => Err(FacadeError::io("access graph scratch")),
    }
}

#[cfg(test)]
#[path = "owner_journal_error_tests.rs"]
mod journal_error_tests;

/// Stream the result through the existing authenticated, bounded output API.
/// The serving host verifies each artifact read; capture remains on the chair.
/// A peer handoff has already crossed the typed record decoder. Revalidate its
/// bounds without rewriting authorised execution context.
fn peer_handoff(
    sealed: Option<&provenant_records::handoff::HandoffResult>,
) -> provenant_records::handoff::HandoffResult {
    let Some(sealed) = sealed else {
        return Ok(None);
    };
    if let Ok(Some(handoff)) = sealed {
        handoff
            .validate()
            .map_err(|reason| provenant_records::handoff::HandoffError::new("$", reason))?;
    }
    sealed.clone()
}

fn remote_graph_result(
    owner: &mut BinaryRunOwner<'_>,
    target: &AttemptTarget,
    origin: &provenant_records::WorkKey,
    required: Option<&str>,
    budget: WaitBudget,
    cancel: &CancelToken,
) -> Result<provenant_records::result_capture::CapturedResult, FacadeError> {
    capture_graph_output(target, origin, required, |offset| {
        owner.output(
            OutputInput::Read {
                extra: Default::default(),
                target: target.clone(),
                part: OutputPart::Result,
                offset,
                max_bytes: provenant_records::run::OUTPUT_LIMIT,
            },
            budget,
            cancel,
        )
    })
}

fn capture_graph_output(
    target: &AttemptTarget,
    origin: &provenant_records::WorkKey,
    required: Option<&str>,
    mut read: impl FnMut(u64) -> Result<OutputResult, FacadeError>,
) -> Result<provenant_records::result_capture::CapturedResult, FacadeError> {
    let mut capture = provenant_records::result_capture::ResultCapture::new(required);
    let mut offset = 0;
    let mut expected_digest = None;
    loop {
        let response = super::peer::checked_output_origin(read(offset)?, origin)?;
        let OutputResult::Chunk {
            target: observed,
            part: OutputPart::Result,
            offset: observed_offset,
            next_offset,
            eof,
            digest,
            bytes,
            ..
        } = response
        else {
            return Err(FacadeError::StateCorrupt);
        };
        if observed != *target
            || observed_offset != offset
            || next_offset < offset
            || next_offset > provenant_records::run::RESULT_LIMIT
            || expected_digest.is_some_and(|expected| expected != digest)
        {
            return Err(FacadeError::StateCorrupt);
        }
        expected_digest = Some(digest);
        let bytes = match bytes {
            OutputBytes::Utf8(text) => text.into_bytes(),
            OutputBytes::Base64(encoded) => {
                provenant_fabric_runtime::handoff::decode_base64(&encoded)
                    .ok_or(FacadeError::StateCorrupt)?
            }
        };
        if next_offset - offset != bytes.len() as u64 || (!eof && next_offset == offset) {
            return Err(FacadeError::StateCorrupt);
        }
        capture.push(&bytes);
        offset = next_offset;
        if eof {
            let result = capture.finish();
            if result.digest != digest.to_string() {
                return Err(FacadeError::StateCorrupt);
            }
            return Ok(result);
        }
    }
}

#[cfg(test)]
#[path = "owner_delivery_hygiene_receipt_tests.rs"]
mod delivery_hygiene_receipt_tests;
#[cfg(test)]
#[path = "owner_peer_capture_tests.rs"]
mod peer_capture_tests;
#[cfg(test)]
#[path = "owner_peer_handoff_tests.rs"]
mod peer_handoff_tests;
#[cfg(test)]
#[path = "owner_recovery_rebuild_tests.rs"]
mod recovery_rebuild_tests;

#[cfg(test)]
mod child_launch_tests {
    use super::*;
    use provenant_core::{RelativePath, WallTime};
    use provenant_records::PathWire;
    use provenant_records::custody::{LeaseState, WorktreeLease};
    use provenant_records::warning::take_unhandled;

    fn tree() -> CustodySubject {
        CustodySubject::new(SubjectKind::Worktree, "worktree:/tmp/child")
    }

    fn request(claims: Option<Vec<PathWire>>) -> ChildLaunchRequest {
        ChildLaunchRequest {
            parent_attempt: provenant_core::AttemptId::from_bytes([7; 16]),
            authority: NarrowedAuthority {
                routes: Default::default(),
                budgets: Default::default(),
                paths: Vec::new(),
                extra: Default::default(),
            },
            destination: Destination::ParentOwnedPath {
                path: "/tmp/child".to_owned(),
            },
            path_claim: Sourced {
                value: claims,
                source: Source::default(),
            },
            commit: Sourced {
                value: None,
                source: Source::default(),
            },
            extra: Default::default(),
        }
    }

    fn lease(sharing: Sharing) -> LeaseObservation {
        LeaseObservation {
            lease: WorktreeLease {
                holder: provenant_core::AttemptId::from_bytes([3; 16]),
                since: WallTime::from_unix_seconds(1).unwrap(),
                sharing,
            },
            state: LeaseState::Live {
                attempt_state: "nonterminal".to_owned(),
            },
        }
    }

    fn sharing(value: Sharing) -> provenant_records::Sourced<Sharing> {
        provenant_records::Sourced {
            value,
            source: Source::default(),
        }
    }

    #[test]
    fn auto_shares_the_parent_when_no_live_lease_holds_it() {
        let _ = take_unhandled();
        let placement = resolve_child_launch(
            &tree(),
            &request(None),
            DestinationChoice::Auto,
            &sharing(Sharing::Warn),
            None,
            &[],
        )
        .unwrap();
        assert_eq!(placement, ChildPlacement::ParentWorktree);
        assert!(take_unhandled().is_empty());
    }

    #[test]
    fn auto_separates_a_child_when_the_owner_refuses_sharing() {
        let _ = take_unhandled();
        let placement = resolve_child_launch(
            &tree(),
            &request(None),
            DestinationChoice::Auto,
            &sharing(Sharing::Warn),
            Some(&lease(Sharing::Refuse)),
            &[],
        )
        .unwrap();
        assert_eq!(placement, ChildPlacement::ManagedWorktree);
        let warnings = take_unhandled();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.code.as_ref() == "LANE_WRITER_SEPARATED")
        );
    }

    #[test]
    fn explicit_parent_under_owner_refuse_is_a_configured_refusal() {
        let error = resolve_child_launch(
            &tree(),
            &request(None),
            DestinationChoice::Parent,
            &sharing(Sharing::Warn),
            Some(&lease(Sharing::Refuse)),
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error.code,
            provenant_records::policy::WORKTREE_SHARING_REFUSED
        );
        assert_eq!(error.class, ErrorClass::Configured);
        let setting = error
            .setting
            .as_ref()
            .expect("a configured refusal names its setting");
        assert_eq!(setting.key, WORKTREE_SHARING_KEY);
        assert!(!error.next.is_empty());
    }

    #[test]
    fn overlapping_path_claims_warn_never_refuse() {
        let _ = take_unhandled();
        let claim = PathWire::from_relative(&RelativePath::parse("src/lib.rs").unwrap()).unwrap();
        let placement = resolve_child_launch(
            &tree(),
            &request(Some(vec![claim])),
            DestinationChoice::Auto,
            &sharing(Sharing::Allow),
            None,
            &[LiveClaim {
                holder: provenant_core::AttemptId::from_bytes([9; 16]),
                path: RelativePath::parse("src").unwrap(),
            }],
        )
        .unwrap();
        assert_eq!(placement, ChildPlacement::ParentWorktree);
        let warnings = take_unhandled();
        let warning = warnings
            .iter()
            .find(|warning| warning.code.as_ref() == provenant_records::policy::PATH_CLAIM_OVERLAP)
            .expect("overlap warning");
        assert!(warning.message.contains("src/lib.rs"));
        assert!(warning.message.contains("src"));
    }
}

#[cfg(test)]
mod on_stop_defaults_tests {
    #[test]
    fn host_defaults_are_read_and_a_pinned_cause_is_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let state = temp.path().join("state");
        for dir in [&home, &project, &state] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            state.join("config.toml"),
            "schema_version = 1\n[domains.fabric.graph.on_stop]\n\
             stopped_no_work = \"chair\"\n\
             blocked = \"new_route\"\n\
             [domains.fabric.graph.projects.other.on_stop]\n\
             stopped_no_work = \"new_route\"\n",
        )
        .unwrap();
        let roots = crate::roots::child(&home, &project, state).unwrap();
        let defaults = super::resolve_on_stop_defaults(&roots, "project");
        assert_eq!(
            defaults.get(provenant_records::StopCause::StoppedNoWork),
            Some(provenant_records::NextStep::Chair)
        );
        // `blocked` is pinned, so its configured default is ignored with
        // ON_STOP_PINNED_IGNORED and the table row stands.
        assert_eq!(defaults.get(provenant_records::StopCause::Blocked), None);
        // A project entry overrides the host entry for its own project only.
        let scoped = super::resolve_on_stop_defaults(&roots, "other");
        assert_eq!(
            scoped.get(provenant_records::StopCause::StoppedNoWork),
            Some(provenant_records::NextStep::NewRoute)
        );
    }
}
