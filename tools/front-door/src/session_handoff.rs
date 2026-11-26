//! `workflow session handoff`: start a fresh Claude session on a peer from a
//! hand-off prompt. The source checks its worktrees, saves the checkpoint and
//! sends one `session_handoff` peer action. The target checks the account
//! against its own registrations, recreates each branch's worktree from the
//! resolved forge remote, stores the prompt and opens Terminal through a
//! launcher seam.
//! Fabric-runtime owns those rules; this edge composes Git, the worktree
//! owner, registered accounts, the run snapshot and the peer call.
use provenant_cli::{
    Globals,
    fabric::{Context, PeerFailure, facade_outcome},
    workflow::Handoff,
};
use provenant_confinement::worktrees::{
    self, Occupancy, WorkerObserver, WorktreeCondition, WorktreeCreate,
};
use provenant_core::{CancelToken, Clock, HostId, OperationId, WaitBudget};
use provenant_fabric_runtime::{
    coordination::facade::FacadeError,
    handoff::{
        ATTACHMENT_COUNT, ATTACHMENT_LIMIT, Attachment, HandoffReceipt, HandoffWorktree,
        PROMPT_LIMIT, SessionHandoff, encode_base64,
        session::{
            Delivery, Launcher, Owner, PromptInput, Tree, admit, compose, deliver, handoff_dir,
            read_owner, refusal_code, short_branch, takes_lease, vet_receipt, write_owner,
        },
        valid_attachment_name, valid_model,
    },
    hosts::{PeerAction, PeerResult},
};
use provenant_os::{
    clock::SystemClock,
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
};
use provenant_records::{CommandResult, Outcome};
use provenant_workflow::session::{HostSessionId, SessionStore};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fmt::Write as _,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};

const COMMAND: &str = "workflow session handoff";
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Kept back from the peer budget for staging, launch and the reply.
const RESERVE: Duration = Duration::from_secs(4);

#[derive(Debug)]
pub(crate) struct Refusal {
    status: Outcome,
    code: String,
    pub(crate) message: String,
    /// Result data and human text kept with the failure.
    data: Option<(Value, String)>,
}
impl Refusal {
    fn new(status: Outcome, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            data: None,
        }
    }
    fn with_data(mut self, data: Value, human: String) -> Self {
        self.data = Some((data, human));
        self
    }
    pub(crate) fn code(&self) -> &str {
        &self.code
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new(Outcome::Invalid, "INVALID", message)
    }
    fn refused(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Outcome::Refused, code, message)
    }
}
impl<E: std::fmt::Display> From<E> for Refusal {
    fn from(error: E) -> Self {
        Self::new(Outcome::Error, "IO", error.to_string())
    }
}

fn git(dir: &Path, args: &[&str], timeout: Duration) -> Result<(bool, String), Refusal> {
    let binary = provenant_os::git::git_executable()
        .ok_or_else(|| Refusal::new(Outcome::Error, "GIT", "Git executable unavailable"))?;
    let environment: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            !key.to_str()
                .is_some_and(|key| crate::fabric::GIT_REDIRECTS.contains(&key))
        })
        .chain([("GIT_TERMINAL_PROMPT".into(), "0".into())])
        .collect();
    let argv: Vec<OsString> = std::iter::once(binary.into_os_string())
        .chain(
            ["-c", "core.fsmonitor=false"]
                .iter()
                .chain(args)
                .map(OsString::from),
        )
        .collect();
    let mut options = ExecOptions::new(&argv, dir, timeout);
    options.env = Some(&environment);
    options.stdin = Some(b"");
    let limit = provenant_os::git::fact_capture_limit(args, 64 * 1024);
    options.stdout = StreamPolicy::Capture { limit };
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    let out = SystemExecutor::new(1)?.execute(options)?;
    if out.timed_out {
        return Err(Refusal::new(
            Outcome::Timeout,
            "GIT",
            format!("git {} timed out", args.join(" ")),
        ));
    }
    // A partial listing must not look complete to the caller. A mutating call
    // has already run, so its chatty output is not worth reporting as failure.
    let mutating = matches!(args.first(), Some(&("add" | "commit" | "push")));
    if out.stdout.truncated && !mutating {
        return Err(Refusal::new(
            Outcome::Error,
            "GIT",
            format!(
                "git {} output exceeded the {limit}-byte limit",
                args.join(" ")
            ),
        ));
    }
    let ok = out.termination == Termination::Exit(0);
    let text = if ok {
        String::from_utf8_lossy(&out.stdout.bytes).into_owned()
    } else {
        String::from_utf8_lossy(&out.stderr.bytes).into_owned()
    };
    Ok((ok, text))
}

fn inspect(
    src: &Source,
    workers: &dyn WorkerObserver,
    facts: &worktrees::RepoFacts,
    remote: Option<&provenant_config::RemoteName>,
) -> Result<Vec<Tree>, Refusal> {
    crate::worktrees::with_context_using_facts(
        &src.roots,
        &src.home,
        &src.cwd,
        GIT_TIMEOUT,
        workers,
        facts,
        |context, cancel| inspect_context(context, facts, cancel, remote),
    )
    .map_err(|error| {
        if matches!(
            error,
            crate::worktrees::Error::Worktree(worktrees::WorktreeError::Utility(
                provenant_confinement::utility::UtilityError::TimedOut
            ))
        ) {
            Refusal::new(Outcome::Timeout, "GIT", "repository fact read timed out")
        } else {
            Refusal::from(error)
        }
    })
}

fn inspect_context(
    context: &worktrees::WorktreeContext<'_>,
    facts: &worktrees::RepoFacts,
    cancel: &CancelToken,
    remote_name: Option<&provenant_config::RemoteName>,
) -> Result<Vec<Tree>, crate::worktrees::Error> {
    let context = &worktrees::WorktreeContext { facts, ..*context };
    let observations = worktrees::list(context, cancel)?;
    let mut trees = Vec::with_capacity(observations.len());
    for observation in observations {
        let info = observation;
        // `list` resolves the primary against the context's verified project
        // root and verifies linked worktrees separately. Do not pass the
        // primary back through `verify_worktree`, which admits linked writers
        // only and correctly refuses the primary checkout.
        let primary = info.primary;
        let branch = info.branch.as_deref().map(short_branch);
        let (upstream, pushed) = match (branch.as_deref(), remote_name) {
            (Some(branch), Some(remote_name)) => {
                let remote = provenant_confinement::repo::GitRefName::remote_tracking(format!(
                    "{remote_name}/{branch}"
                ))
                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
                match facts.resolve_commit(context, remote, cancel) {
                    Ok(oid) => (
                        Some(oid.to_string()),
                        worktrees::is_ancestor(context, info.head, oid, cancel)?,
                    ),
                    Err(worktrees::WorktreeError::NotFound) => (None, false),
                    Err(error) => return Err(error.into()),
                }
            }
            _ => (None, false),
        };
        let path = fs::canonicalize(&info.path).unwrap_or(info.path);
        let (live_writer, writer_unknown) = handoff_writer_state(&info.occupancy);
        let writer_evidence = match &info.occupancy {
            Occupancy::Unknown {
                scope,
                evidence,
                source: worktrees::OccupancyUnknownSource::Custody,
            } => Some(format!("{}: {evidence}", scope.display())),
            _ => None,
        };
        let writer_advisory = match &info.occupancy {
            Occupancy::Observed { process } => Some(process.as_ref().map_or_else(
                || format!("{}: a process was observed", path.display()),
                |process| format!("{}: process {process} ({})", path.display(), process.kind),
            )),
            Occupancy::Unknown {
                scope,
                evidence,
                source: worktrees::OccupancyUnknownSource::Census,
            } => Some(format!("{}: {evidence}", scope.display())),
            _ => None,
        };
        trees.push(Tree {
            primary,
            path,
            branch,
            head: info.head.to_string(),
            dirty: info.dirty == WorktreeCondition::Dirty,
            unknown: matches!(
                info.dirty,
                WorktreeCondition::Unknown | WorktreeCondition::GitOperationInProgress
            ),
            live_writer,
            writer_unknown,
            writer_evidence,
            writer_advisory,
            upstream,
            pushed,
        });
    }
    Ok(trees)
}

fn handoff_writer_state(occupancy: &Occupancy) -> (bool, bool) {
    (
        matches!(occupancy, Occupancy::Held),
        matches!(
            occupancy,
            Occupancy::Unknown {
                source: worktrees::OccupancyUnknownSource::Custody,
                ..
            }
        ),
    )
}

/// WIP mutates Git, so an unresolved writer state cannot grant custody.
fn unsafe_wip(tree: &Tree) -> bool {
    tree.primary || tree.branch.is_none() || tree.live_writer || tree.writer_unknown || tree.unknown
}

/// An advisory observation about a writer worktree is a typed warning: it does
/// not block the handoff, and it travels once per change in the JSON envelope.
/// Spec: docs/specs/cli.md#warnings-are-typed-values
fn emit_writer_advisory(advisory: &str) {
    provenant_records::warning::warn(
        provenant_records::warning::Warning::new(
            provenant_records::warning::WRITER_ADVISORY,
            provenant_fabric_runtime::hosts::plain_text(advisory, 4096),
            provenant_records::warning::detected_source(
                provenant_records::warning::WRITER_ADVISORY,
            ),
        )
        .once_per_change(),
    );
}

fn wip_unsafe_description(tree: &Tree) -> String {
    let detail = match (tree.writer_unknown, tree.writer_evidence.as_deref()) {
        (true, Some(evidence)) => format!("; writer custody unknown: {evidence}"),
        (true, None) => "; writer custody unknown".to_owned(),
        (false, _) => String::new(),
    };
    format!("  {}{detail}", tree.describe())
}

/// Hold writer custody through WIP Git operations, not just the earlier preview.
fn with_wip_custody(
    roots: &provenant_core::Roots,
    home: &Path,
    project: &Path,
    workers: &dyn WorkerObserver,
    facts: &worktrees::RepoFacts,
    tree: &Tree,
    action: impl FnOnce() -> Result<(), Refusal>,
) -> Result<(), Refusal> {
    crate::worktrees::with_context_using_facts(
        roots,
        home,
        project,
        GIT_TIMEOUT,
        workers,
        facts,
        |context, cancel| {
            let verified = provenant_confinement::verify_worktree(context.repo, &tree.path)
                .map_err(worktrees::WorktreeError::from)?;
            let _writer = worktrees::cache_writer_guard(context.project_state, verified.path())
                .map_err(worktrees::custody_refusal)?;
            worktrees::check_writer_terminal(verified.repo())?;
            let branch = tree
                .branch
                .as_deref()
                .ok_or(worktrees::WorktreeError::IdentityChanged)?;
            let reference = provenant_confinement::repo::GitRefName::branch(branch)
                .map_err(|_| worktrees::WorktreeError::IdentityChanged)?;
            let head = verified
                .repo()
                .private_dir()
                .read_stable(std::ffi::OsStr::new("HEAD"), 4096)
                .map_err(worktrees::WorktreeError::from)?;
            if head != format!("ref: {}\n", reference.as_str()).as_bytes()
                || facts
                    .resolve_commit(context, reference, cancel)?
                    .to_string()
                    != tree.head
            {
                return Err(worktrees::WorktreeError::IdentityChanged.into());
            }
            facts.invalidate_inventory(context);
            Ok(action())
        },
    )
    .map_err(|error| Refusal::refused("WIP_REFUSED", error.to_string()))?
}

/// Commit a dirty tree as WIP and push its branch. Hooks run as usual.
pub(crate) fn wip(
    src: &Source,
    workers: &dyn WorkerObserver,
    facts: &worktrees::RepoFacts,
    tree: &Tree,
    remote: Option<provenant_config::RemoteName>,
) -> Result<(), Refusal> {
    // A WIP commit is published to the checkout's remote; without one there is
    // nowhere to push and no name to pass to Git.
    let Some(remote) = remote else {
        return Err(Refusal::refused(
            "WIP_REFUSED",
            "a committed WIP hand-off needs a Git remote to push to",
        ));
    };
    with_wip_custody(
        &src.roots,
        &src.home,
        &src.cwd,
        workers,
        facts,
        tree,
        || {
            if tree.dirty {
                let files = untracked(tree)?;
                if !files.is_empty() {
                    return Err(Refusal::refused(
                        "WIP_REFUSED",
                        format!(
                            "untracked files in {}: {}",
                            tree.path.display(),
                            files.join(", ")
                        ),
                    ));
                }
            }
            wip_git(tree, &remote)
        },
    )
}

fn wip_git(tree: &Tree, remote: &provenant_config::RemoteName) -> Result<(), Refusal> {
    let branch = tree.branch.as_deref().ok_or_else(|| {
        Refusal::refused("WIP_REFUSED", "a detached worktree has no branch to push")
    })?;
    let step = |args: &[&str]| -> Result<(), Refusal> {
        match git(&tree.path, args, GIT_TIMEOUT)? {
            (true, _) => Ok(()),
            (false, error) => Err(Refusal::refused(
                "WIP_FAILED",
                format!(
                    "git {} failed in {}: {}",
                    args.join(" "),
                    tree.path.display(),
                    error.trim()
                ),
            )),
        }
    };
    if tree.dirty {
        step(&["add", "--update"])?;
        step(&["commit", "--message", "WIP: session hand-off"])?;
    }
    // A full refspec cannot be confused with a tag of the same name.
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    step(&["push", "--set-upstream", remote.as_str(), &refspec])
}
/// Untracked files Git does not ignore. `--wip` commits tracked changes only,
/// so these would stay behind.
fn untracked(tree: &Tree) -> Result<Vec<String>, Refusal> {
    match git(
        &tree.path,
        &["ls-files", "--others", "--exclude-standard"],
        GIT_TIMEOUT,
    )? {
        (true, out) => Ok(out.lines().map(String::from).collect()),
        (false, error) => Err(Refusal::new(
            Outcome::Error,
            "GIT",
            format!(
                "cannot list untracked files in {}: {}",
                tree.path.display(),
                error.trim()
            ),
        )),
    }
}

fn read_attachments(paths: &[PathBuf]) -> Result<Vec<Attachment>, Refusal> {
    if paths.len() > ATTACHMENT_COUNT {
        return Err(Refusal::invalid("at most 32 attachments are allowed"));
    }
    let mut total = 0usize;
    let mut attachments: Vec<Attachment> = Vec::with_capacity(paths.len());
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| valid_attachment_name(n) && *n != "prompt.md")
            .ok_or_else(|| {
                Refusal::invalid(format!(
                    "attachment {} needs a plain file name of letters, digits, '.', '_', '-' or spaces",
                    path.display()
                ))
            })?;
        if attachments.iter().any(|a| a.name == name) {
            return Err(Refusal::invalid(format!(
                "two attachments are named {name}"
            )));
        }
        let file = fs::File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(Refusal::invalid(format!(
                "attachment {} is not a regular file",
                path.display()
            )));
        }
        let mut bytes = Vec::new();
        file.take((ATTACHMENT_LIMIT - total + 1) as u64)
            .read_to_end(&mut bytes)?;
        total += bytes.len();
        if total > ATTACHMENT_LIMIT {
            return Err(Refusal::invalid("attachments exceed 512 KiB in total"));
        }
        attachments.push(Attachment {
            extra: Default::default(),
            name: name.to_owned(),
            base64: encode_base64(&bytes),
        });
    }
    Ok(attachments)
}

/// The checkpoint store a hand-off saves to, keeping the configured number of
/// revisions like `workflow session save`.
fn checkpoint_store(
    roots: &provenant_core::Roots,
    project: provenant_core::ProjectId,
    session: HostSessionId,
) -> SessionStore {
    SessionStore::new(roots.state.clone(), project, session)
        .with_revisions(provenant_cli::workflow::session_revisions(roots))
}

fn checkpoint_repair(
    globals: &Globals,
    cwd: &Path,
    session: &HostSessionId,
    client: Option<provenant_records::Harness>,
) -> String {
    let client = client
        .map(|harness| format!(" --client {}", harness.spec().name))
        .unwrap_or_else(|| " --client HARNESS".into());
    let rest = format!(
        "workflow session save {}{client} --rebind",
        session.as_str()
    );
    let commands =
        provenant_workflow::session::CommandPrefix::new(globals.state_dir.as_deref(), cwd);
    let project = if globals.project.is_some() {
        cwd.to_str()
            .map(|path| format!("--project '{}' ", path.replace('\'', "'\\''")))
    } else {
        Some(String::new())
    };
    match project.and_then(|project| commands.runnable(&format!("{project}{rest}"))) {
        Some(command) => format!("{command} --file <checkpoint.md>"),
        None => format!(
            "{rest} --file <checkpoint.md> with this handoff's --project and --state-dir options (a path cannot be shown safely)"
        ),
    }
}

fn checkpoint_identity(store: &SessionStore, repair: &str) -> Result<(), Refusal> {
    store.validate_chair().map_err(|error| match error {
        provenant_workflow::session::SessionError::DiscoveryIdentity => Refusal::refused(
            "IDENTITY",
            format!(
                "session discovery identity does not match the registered chair; first run {repair}"
            ),
        ),
        error => error.into(),
    })
}

fn session_id(explicit: Option<&str>) -> Result<Option<HostSessionId>, Refusal> {
    let text = match explicit {
        Some(text) => Some(text.to_owned()),
        None => std::env::var("CLAUDE_CODE_SESSION_ID")
            .or_else(|_| std::env::var("CLAUDE_SESSION_ID"))
            .ok(),
    };
    text.map(|text| HostSessionId::parse(&text).map_err(|e| Refusal::invalid(e.to_string())))
        .transpose()
}

pub(crate) struct Source {
    pub(crate) roots: provenant_core::Roots,
    pub(crate) home: PathBuf,
    pub(crate) assets: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) context: Context,
}

fn open_source(globals: &Globals) -> Result<Source, Refusal> {
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    )?;
    let context = Context::open(resolved.roots.clone(), &resolved.home, &resolved.project)?;
    Ok(Source {
        roots: resolved.roots,
        home: resolved.home,
        assets: resolved.assets,
        cwd: resolved.project,
        context,
    })
}

/// The one remote for this command, when the checkout has one. Each owner calls
/// this exactly once and passes the result down; no helper rediscovers it. A
/// checkout with no remote has no name rather than an empty one; hand-off only
/// pushes and fetches, so it never needs the default branch or protected set.
/// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
pub(crate) fn resolve_remote(
    state: &Path,
    project: &Path,
    ports: &provenant_config::PortTable,
) -> Result<Option<provenant_config::RemoteName>, Refusal> {
    let policy = provenant_config::operation_policy(state, project, ports);
    let config = provenant_config::ForgeConfig::new(ports, (*policy).clone());
    let git = provenant_config::ForgeGit::new(project).map_err(|error| {
        Refusal::new(
            Outcome::Error,
            "GIT",
            format!("cannot read the checkout's Git metadata: {error}"),
        )
    })?;
    provenant_config::ForgeContext::remote_if_any(&config, &git).map_err(|error| {
        Refusal::new(
            Outcome::Refused,
            "FORGE_CONTEXT_UNRESOLVED",
            error.to_string(),
        )
    })
}

// The writer advisory is part of the handoff hook's stderr protocol.
#[allow(clippy::disallowed_methods)]
fn execute(
    args: &Handoff,
    globals: &Globals,
    workers: &dyn WorkerObserver,
) -> Result<(Value, String), Refusal> {
    let clock = SystemClock::new();
    if provenant_cli::fabric::in_lane() {
        return Err(Refusal::refused(
            "lane_coordination_refused",
            "coordination mutations are not allowed inside a lane",
        ));
    }
    if !provenant_records::AdapterSpec::get(&args.provider).is_some_and(|spec| spec.session_handoff)
    {
        return Err(Refusal::new(
            Outcome::Unavailable,
            "UNSUPPORTED",
            "the selected provider cannot take a hand-off",
        ));
    }
    if args.model.as_deref().is_some_and(|m| !valid_model(m)) {
        return Err(Refusal::invalid("invalid --model"));
    }
    let mut src = open_source(globals)?;
    src.context.identity.inbox = src
        .context
        .inbox_address(args.client)
        .map_err(|error| Refusal::invalid(error.to_string()))?;
    let context = &src.context;
    let local = context.identity.host;
    let peers: Vec<&str> = context.config.peers.keys().map(String::as_str).collect();
    let target = context.peer_host(&args.to).ok_or_else(|| {
        Refusal::new(
            Outcome::Invalid,
            "UNKNOWN_HOST",
            format!(
                "no configured peer named {}; configured peers: {}",
                args.to,
                if peers.is_empty() {
                    "none".into()
                } else {
                    peers.join(", ")
                }
            ),
        )
    })?;
    if target == local {
        return Err(Refusal::invalid(
            "the target is this host; a hand-off needs another host",
        ));
    }
    if context.identity.federation.is_none() {
        return Err(Refusal::refused(
            "REFUSED",
            "a hand-off needs a Git project inside the home directory",
        ));
    }
    let session = session_id(args.session_id.as_deref())?;
    if !args.no_checkpoint && !args.dry_run && session.is_none() {
        return Err(Refusal::invalid(
            "saving the checkpoint needs --session-id or CLAUDE_CODE_SESSION_ID; pass \
             --no-checkpoint to skip it",
        ));
    }

    // 1. Everything the prompt needs, read before any tree is touched.
    let edge = provenant_cli::run::Edge {
        roots: &src.roots,
        home: &src.home,
        assets: &src.assets,
        cwd: &src.cwd,
    };
    let lanes = provenant_cli::run::chair_snapshot(&edge)
        .map(|(_, lanes)| lanes)
        .map_err(|fail| fail.message);
    let project = context.identity.project;
    let remote = resolve_remote(src.roots.state.as_path(), &src.cwd, &context.port_table)?;
    let actor = context.identity.inbox.actor;
    let store = session.as_ref().filter(|_| !args.no_checkpoint).map(|id| {
        checkpoint_store(&src.roots, project, id.clone()).with_chair(context.identity.host, actor)
    });
    if let (Some(store), Some(session)) = (&store, &session) {
        checkpoint_identity(
            store,
            &checkpoint_repair(globals, &src.cwd, session, args.client),
        )?;
    }
    let checkpoint = store
        .as_ref()
        .map(SessionStore::read)
        .transpose()?
        .flatten()
        .map(|c| c.markdown);
    let body = args
        .prompt_file
        .as_deref()
        .map(|path| {
            let mut text = String::new();
            fs::File::open(path)?
                .take(PROMPT_LIMIT as u64 + 1)
                .read_to_string(&mut text)?;
            Ok::<_, io::Error>(text)
        })
        .transpose()?;
    let attachments = read_attachments(&args.attachments)?;
    let operation_id = OperationId::parse(&format!(
        "handoff-{}-{}",
        clock
            .wall_now()
            .map_or(0, provenant_core::WallTime::as_micros),
        std::process::id()
    ))
    .map_err(|e| Refusal::invalid(e.to_string()))?;
    // The whole request, built and checked for the given trees.
    let request = |trees: &[Tree]| -> Result<SessionHandoff, Refusal> {
        let prompt = compose(&PromptInput {
            source: local,
            agent: &context.identity.inbox.agent,
            session: session.as_ref().map(HostSessionId::as_str),
            body: body.as_deref(),
            checkpoint: checkpoint.as_deref(),
            trees,
            lanes: &lanes,
            notes: &args.notes,
        })
        .map_err(|error| Refusal::invalid(error.to_string()))?;
        let input = SessionHandoff {
            extra: Default::default(),
            operation_id: operation_id.clone(),
            account: args.account.clone(),
            model: args.model.clone(),
            prompt,
            branches: trees
                .iter()
                .filter(|t| !t.primary && t.upstream.is_some())
                .filter_map(|t| t.branch.clone())
                .collect(),
            attachments: attachments.clone(),
            launch: !args.no_launch,
        };
        input
            .validate()
            .map_err(|e| Refusal::invalid(e.to_string()))?;
        Ok(input)
    };

    // 2. Worktrees must be clean and pushed, unless the caller says otherwise.
    let facts = worktrees::RepoFacts::default();
    let mut trees = inspect(&src, workers, &facts, remote.as_ref())?;
    let mut input = request(&trees)?;
    for advisory in trees
        .iter()
        .filter_map(|tree| tree.writer_advisory.as_deref())
    {
        emit_writer_advisory(advisory);
    }
    let problems: Vec<Tree> = trees
        .iter()
        .filter(|t| !t.problems().is_empty())
        .cloned()
        .collect();
    let mut committed = Vec::new();
    if !problems.is_empty() {
        let list = problems
            .iter()
            .map(|t| format!("  {}", t.describe()))
            .collect::<Vec<_>>()
            .join("\n");
        if args.wip {
            let mut unsafe_trees: Vec<String> = problems
                .iter()
                .filter(|tree| unsafe_wip(tree))
                .map(wip_unsafe_description)
                .collect();
            for tree in problems.iter().filter(|t| t.dirty) {
                let files = untracked(tree)?;
                if !files.is_empty() {
                    unsafe_trees.push(format!(
                        "  {}: untracked files {}",
                        tree.path.display(),
                        files.join(", ")
                    ));
                }
            }
            if !unsafe_trees.is_empty() {
                return Err(Refusal::refused(
                    "WIP_REFUSED",
                    format!(
                        "--wip commits tracked changes in branch worktrees with available writer custody, \
                         never the primary checkout or untracked files. Settle these first or \
                         pass --allow-dirty:\n{}",
                        unsafe_trees.join("\n")
                    ),
                ));
            }
            committed = problems.iter().map(Tree::describe).collect();
            if !args.dry_run {
                let mut pushed: Vec<String> = Vec::new();
                for tree in &problems {
                    if let Err(mut refusal) = wip(&src, workers, &facts, tree, remote.clone()) {
                        if !pushed.is_empty() {
                            let _ = write!(
                                refusal.message,
                                "\nAlready committed and pushed as WIP: {}",
                                pushed.join(", ")
                            );
                        }
                        return Err(refusal);
                    }
                    pushed.push(tree.branch.clone().unwrap_or_default());
                }
                trees = inspect(&src, workers, &facts, remote.as_ref())?;
                let left: Vec<String> = trees
                    .iter()
                    .filter(|t| !t.problems().is_empty())
                    .map(|t| format!("  {}", t.describe()))
                    .collect();
                if !left.is_empty() {
                    return Err(Refusal::refused(
                        "WORKTREE_DIRTY",
                        format!(
                            "worktrees still need attention after --wip:\n{}",
                            left.join("\n")
                        ),
                    ));
                }
                input = request(&trees)?;
            }
        } else if !args.allow_dirty {
            return Err(Refusal::refused(
                "WORKTREE_DIRTY",
                format!(
                    "these worktrees are dirty or unpushed; push them, or pass --wip to commit \
                     and push them, or --allow-dirty to list them in the prompt:\n{list}"
                ),
            ));
        }
    }

    // 3. The plan.
    let prompt = input.prompt.clone();
    let branches = input.branches.clone();
    let lease_dir = handoff_dir(src.roots.state.as_path(), project);
    let owner = read_owner(&lease_dir);
    let warning = owner.as_ref().filter(|o| o.host != local).map(|o| {
        format!(
            "the owner lease names host {} as this session's owner",
            o.host
        )
    });
    let mut data = json!({
        "dry_run": args.dry_run,
        "operation_id": operation_id,
        "to": args.to,
        "target_host": target,
        "account": args.account,
        "model": args.model,
        "branches": branches,
        "worktrees": trees,
        "wip": committed,
        "allowed_dirty": if args.allow_dirty {
            trees.iter().filter(|t| !t.problems().is_empty()).map(Tree::describe).collect()
        } else {
            Vec::new()
        },
        "lanes": lanes.as_ref().ok(),
        "lanes_error": lanes.as_ref().err(),
        "attachments": input.attachments.iter().map(|a| &a.name).collect::<Vec<_>>(),
        "checkpoint": !args.no_checkpoint,
        "launch": !args.no_launch,
        "owner": owner,
        "warning": warning,
        "prompt": prompt,
    });
    if args.dry_run {
        let mut human = format!(
            "Dry run: hand off to {} ({target}) as account {}. Nothing was sent.\n\
             Branches to recreate: {}\nAttachments: {}\n",
            args.to,
            args.account,
            if branches.is_empty() {
                "none".into()
            } else {
                branches.join(", ")
            },
            input.attachments.len(),
        );
        if !committed.is_empty() {
            let _ = writeln!(
                human,
                "--wip would commit and push:\n  {}",
                committed.join("\n  ")
            );
        }
        if let Some(warning) = &data["warning"].as_str() {
            let _ = writeln!(human, "Warning: {warning}");
        }
        let _ = write!(human, "\n--- prompt ---\n{prompt}");
        return Ok((data, human));
    }

    // 4. Send one peer action. The checkpoint and lease change only after
    // the peer accepts, so a retry starts from the same checkpoint.
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), context.config.limits.peer_seconds)?;
    let reply = context.peer(
        &clock,
        target,
        PeerAction::SessionHandoff {
            extra: Default::default(),
            input: Box::new(input),
            budget_seconds: context.config.limits.peer_seconds,
        },
        budget,
        &cancel,
    );
    let receipt = match reply {
        Ok(PeerResult::SessionHandoff {
            data: receipt,
            extra: _,
        }) => receipt,
        Ok(_) => {
            return Err(Refusal::new(
                Outcome::Error,
                "STATE_CORRUPT",
                "peer reply does not match the request",
            ));
        }
        Err(PeerFailure { error, .. }) if matches!(*error, FacadeError::LaunchUnknown { .. }) => {
            let FacadeError::LaunchUnknown { operation_id } = *error else {
                unreachable!()
            };
            return Err(Refusal::new(
                Outcome::Error,
                "LAUNCH_UNKNOWN",
                format!(
                    "the hand-off outcome is unknown; look for operation {operation_id} under \
                     handoff/ in the peer's state directory before retrying, so the peer does \
                     not open a second session"
                ),
            ));
        }
        Err(PeerFailure { error, detail }) => {
            let detail = detail.map(|d| format!(" ({d})")).unwrap_or_default();
            let (status, kind) = facade_outcome(&error);
            return Err(Refusal::new(
                status,
                kind.code(),
                format!(
                    "peer {} refused or is unavailable: {error}{detail}",
                    args.to
                ),
            ));
        }
    };
    let receipt = vet_receipt(receipt, &branches)
        .map_err(|error| Refusal::new(Outcome::Error, "HANDOFF_RECEIPT", error.to_string()))?;
    match &receipt {
        HandoffReceipt::Refused {
            code,
            message,
            accounts,
            ..
        } => {
            let listed = if accounts.is_empty() {
                String::new()
            } else {
                format!(
                    "; registered Claude accounts on {}: {}",
                    args.to,
                    accounts.join(", ")
                )
            };
            return Err(Refusal::refused(
                refusal_code(code),
                format!(
                    "{} refused the hand-off ({code}): {message}{listed}",
                    args.to
                ),
            ));
        }
        HandoffReceipt::Delivered { .. } => {}
    }
    // The peer holds the hand-off: record it as this session's checkpoint.
    let mut checkpoint_warnings = Vec::new();
    let checkpoint_error = store.as_ref().and_then(|store| {
        WaitBudget::cli(clock.mono_now(), 30)
            .map_err(|e| e.to_string())
            .and_then(|budget| {
                store
                    .save_revision(prompt.as_bytes(), budget, &cancel, &clock)
                    .map(|saved| checkpoint_warnings = saved.warnings)
                    .map_err(|e| e.to_string())
            })
            .err()
    });
    data["checkpoint_error"] = json!(checkpoint_error);
    data["checkpoint_warnings"] = json!(checkpoint_warnings);
    data["receipt"] = serde_json::to_value(&receipt)?;
    if let Some(failure) = launch_failure(&args.to, &receipt, args.no_launch, &mut data) {
        return Err(failure);
    }
    // The lease moves only once the new session is opening, or the caller
    // chose to open it by hand.
    let started =
        args.no_launch || matches!(receipt, HandoffReceipt::Delivered { launched: true, .. });
    if started {
        let lease = Owner {
            schema_version: 1,
            host: target,
            previous: Some(local),
            operation_id,
            session_id: session.map(String::from),
            at_unix: clock.unix_seconds(),
        };
        let lease_error = write_owner(&lease_dir, &lease).err().map(|e| e.to_string());
        data["owner"] = serde_json::to_value(&lease)?;
        data["owner_error"] = json!(lease_error);
    }
    data.as_object_mut().map(|o| o.remove("prompt"));
    let HandoffReceipt::Delivered {
        prompt_path,
        worktrees,
        script_path,
        launched,
        launch_error,
        ..
    } = &receipt
    else {
        return Err(Refusal::new(
            Outcome::Error,
            "STATE_CORRUPT",
            "unexpected receipt",
        ));
    };
    let mut human = format!(
        "Handed off to {} as account {}. Prompt on the peer: {prompt_path}\n",
        args.to, args.account
    );
    for warning in &checkpoint_warnings {
        let _ = writeln!(human, "Warning: {warning}");
    }
    for tree in worktrees {
        let _ = match (&tree.path, &tree.error) {
            (Some(path), Some(warning)) => {
                writeln!(human, "  {}: {path} (warning: {warning})", tree.branch)
            }
            (Some(path), None) => writeln!(
                human,
                "  {}: {path}{}",
                tree.branch,
                if tree.created {
                    " (created)"
                } else {
                    " (existing)"
                }
            ),
            (None, error) => writeln!(
                human,
                "  {}: not recreated: {}",
                tree.branch,
                error.as_deref().unwrap_or("unknown")
            ),
        };
    }
    if *launched {
        let _ = writeln!(
            human,
            "Opened Terminal on the peer; opening is not proof that the session is ready."
        );
    } else if let Some(error) = launch_error {
        let _ = writeln!(human, "Terminal launch failed on the peer: {error}");
    } else {
        let _ = writeln!(
            human,
            "Not launched (--no-launch). Run {} on the peer to start the session.",
            script_path.as_deref().unwrap_or("its launch.command")
        );
    }
    if let Some(error) = &checkpoint_error {
        let _ = writeln!(human, "Checkpoint not saved: {error}");
    }
    if started {
        let _ = writeln!(
            human,
            "This host's lease now names {} as owner. Stop this session once the new one has \
             started.",
            args.to
        );
    } else {
        let _ = writeln!(
            human,
            "The lease still names this host. Run the peer's launch script by hand, or retry with \
             a new hand-off once the launch problem is fixed."
        );
    }
    Ok((data, human))
}

pub fn run(args: Handoff, globals: &Globals, workers: &dyn WorkerObserver) -> u8 {
    let dry_run = args.dry_run;
    match execute(&args, globals, workers) {
        Ok((data, human)) => crate::emit(
            &CommandResult::new(COMMAND, Outcome::Ok, data),
            globals,
            &human,
            dry_run,
        ),
        Err(Refusal {
            status,
            code,
            message,
            data: Some((data, human)),
        }) => {
            let mut result = CommandResult::new(COMMAND, status, data);
            result.error = Some(provenant_records::CommandError::new(
                code,
                message,
                None,
                "correct the reported handoff condition and rerun `provenant workflow session handoff`",
            ));
            crate::emit(&result, globals, &human, false)
        }
        Err(refusal) => crate::fail(
            COMMAND,
            refusal.status,
            refusal.code(),
            &refusal.message,
            "correct the reported handoff condition and rerun `provenant workflow session handoff`",
            globals,
        ),
    }
}

pub(crate) struct Target<'a> {
    pub context: &'a Context,
    pub home: &'a Path,
    /// The project's primary checkout on this host.
    pub cwd: &'a Path,
    pub workers: &'a dyn WorkerObserver,
    pub source: HostId,
    pub clock: &'a SystemClock,
    pub budget: WaitBudget,
}
/// The registered account directory for `label`, if this host has it.
fn account_directory(
    accounts: &[provenant_providers::identity::RegisteredAccount],
    label: &str,
) -> Option<PathBuf> {
    accounts
        .iter()
        .find(|a| a.key() == label)
        .map(|a| a.directory().to_path_buf())
}
/// Reuse tracking refs only after a base has been fetched and verified.
fn fetch_recreation_base<T, E>(
    fetched: &mut bool,
    resolve: impl FnOnce(bool) -> Result<T, E>,
) -> Result<T, E> {
    let result = resolve(!*fetched);
    if result.is_ok() {
        *fetched = true;
    }
    result
}

/// Recreate each branch's worktree with this host's own worktree owner, from
/// `<remote>/<branch>`. A worktree already on the branch is kept. Failures are
/// reported per branch; the hand-off still lands.
pub(crate) fn recreate(
    target: &Target<'_>,
    branches: &[String],
    remote: Option<provenant_config::RemoteName>,
) -> Vec<HandoffWorktree> {
    // Branch recreation starts from `<remote>/<branch>`; without a remote there
    // is no base to fetch and no name to build that ref from.
    let Some(remote) = remote else {
        return branches
            .iter()
            .map(|branch| HandoffWorktree {
                extra: Default::default(),
                branch: branch.clone(),
                path: None,
                head: None,
                created: false,
                error: Some("a hand-off needs a Git remote to recreate branches".into()),
            })
            .collect();
    };
    let remaining = || {
        let left =
            Duration::from_micros(target.budget.remaining(target.clock.mono_now()).as_micros());
        left.saturating_sub(RESERVE)
    };
    let mut fetched = false;
    let facts = worktrees::RepoFacts::default();
    branches
        .iter()
        .map(|branch| {
            let failed = |error: String| HandoffWorktree {
                extra: Default::default(),
                branch: branch.clone(),
                path: None,
                head: None,
                created: false,
                error: Some(error),
            };
            let time = remaining();
            if time.is_zero() {
                return failed("the peer call budget ran out".into());
            }
            let base = fetch_recreation_base(&mut fetched, |fetch| {
                crate::worktrees::with_context_using_facts(
                    &target.context.roots,
                    target.home,
                    target.cwd,
                    remaining(),
                    target.workers,
                    &facts,
                    |context, cancel| {
                        Ok(crate::fabric::remote_base(
                            context,
                            &format!("{remote}/{branch}"),
                            time / 2,
                            fetch,
                            cancel,
                        ))
                    },
                )
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())
            });
            let base = match base {
                Ok(base) => base,
                Err(error) => {
                    return failed(format!("{remote}/{branch}: {error}"));
                }
            };
            let made = crate::worktrees::with_context_using_facts(
                &target.context.roots,
                target.home,
                target.cwd,
                remaining(),
                target.workers,
                &facts,
                |context, cancel| {
                    if let Some(info) = worktrees::list_cached(context, cancel)?
                        .into_iter()
                        .find(|w| w.branch.as_deref().map(short_branch).as_ref() == Some(branch))
                    {
                        return Ok((info, false));
                    }
                    let request = WorktreeCreate {
                        branch: branch.clone(),
                        base: Some(base.to_string()),
                    };
                    match worktrees::create(context, request, cancel) {
                        // The local branch exists without a worktree: check it out.
                        Err(worktrees::WorktreeError::InvalidRequest) => {
                            let request = WorktreeCreate {
                                branch: branch.clone(),
                                base: None,
                            };
                            Ok((worktrees::create(context, request, cancel)?, false))
                        }
                        result => Ok((result?, true)),
                    }
                },
            );
            match made {
                Ok((info, created)) => HandoffWorktree {
                    extra: Default::default(),
                    branch: branch.clone(),
                    path: Some(info.path.to_string_lossy().into_owned()),
                    head: Some(info.head.to_string()),
                    created,
                    error: worktree_warning(&info, &base, &remote),
                },
                Err(error) => failed(error.to_string()),
            }
        })
        .collect()
}

/// Why a reused worktree is not a clean copy of `<remote>/<branch>`.
fn worktree_warning(
    info: &worktrees::WorktreeInfo,
    base: &provenant_core::GitOid,
    remote: &provenant_config::RemoteName,
) -> Option<String> {
    let mut found = Vec::new();
    if info.head != *base {
        found.push(format!(
            "local branch differs from {remote} (local {}, {remote} {base})",
            info.head
        ));
    }
    if !matches!(
        info.dirty,
        WorktreeCondition::Clean | WorktreeCondition::Ignored
    ) {
        found.push("the worktree has uncommitted changes or unknown state".to_owned());
    }
    (!found.is_empty()).then(|| found.join("; "))
}

/// A delivered hand-off whose Terminal did not open, without `--no-launch`,
/// is a failure: the caller must not stop its own session. The receipt stays
/// in `data`.
fn launch_failure(
    to: &str,
    receipt: &HandoffReceipt,
    no_launch: bool,
    data: &mut Value,
) -> Option<Refusal> {
    let HandoffReceipt::Delivered {
        launched: false,
        launch_error,
        script_path,
        ..
    } = receipt
    else {
        return None;
    };
    if no_launch {
        return None;
    }
    let reason = launch_error.as_deref().unwrap_or("unknown");
    let script = script_path.as_deref().unwrap_or("its launch.command");
    data.as_object_mut().map(|o| o.remove("prompt"));
    let human = format!(
        "The peer stored the hand-off but did not open Terminal: {reason}\nRun {script} on the \
         peer to start the session. The lease still names this host.\n"
    );
    Some(
        Refusal::new(
            Outcome::Error,
            "LAUNCH_FAILED",
            format!(
                "{to} stored the hand-off but did not open the session ({reason}); run {script} \
                 there, or retry with a new hand-off. This host keeps the lease."
            ),
        )
        .with_data(std::mem::take(data), human),
    )
}

/// Peer ingress for `PeerAction::SessionHandoff`.
pub(crate) fn receive(
    target: &Target<'_>,
    input: &SessionHandoff,
    launcher: &dyn Launcher,
) -> HandoffReceipt {
    // A config that cannot be read counts as off.
    let accepted = provenant_cli::config_reads::read_for(&target.context.roots, "knobs")
        .ok()
        .and_then(|config| {
            config
                .knobs
                .get("fabric.accept_handoff")
                .and_then(|k| k.value.as_bool())
        });
    let context = target.context;
    let lease_dir = handoff_dir(context.roots.state.as_path(), context.identity.project);
    let (attachments, account, dir) =
        match admit(accepted == Some(true), input, &lease_dir, |label| {
            crate::registered_claude(&context.roots)
                .map(|accounts| account_directory(&accounts, label))
                .map_err(|e| e.to_string())
        }) {
            Ok(admitted) => admitted,
            Err(receipt) => return *receipt,
        };
    // Worktrees are recreated only once this operation owns its directory.
    let remote = match resolve_remote(
        context.roots.state.as_path(),
        target.cwd,
        &context.port_table,
    ) {
        Ok(remote) => remote,
        Err(error) => {
            return HandoffReceipt::Refused {
                extra: Default::default(),
                code: "FORGE_CONTEXT_UNRESOLVED".into(),
                message: error.message,
                accounts: Vec::new(),
            };
        }
    };
    let worktrees = recreate(target, &input.branches, remote);
    let source_label = context
        .config
        .peers
        .iter()
        .find(|(_, peer)| peer.host == target.source)
        .map_or_else(|| target.source.to_string(), |(label, _)| label.clone());
    let receipt = deliver(
        Delivery {
            dir,
            project: target.cwd,
            state: context.roots.state.as_path(),
            account_label: &input.account,
            account,
            model: input.model.as_deref(),
            prompt: &input.prompt,
            source_label,
            worktrees,
            attachments,
            launch: input.launch,
        },
        launcher,
    );
    // A failed launch leaves the lease with the source, which keeps its session.
    if takes_lease(&receipt, input.launch) {
        // The lease is a record, not a gate: a failed write leaves the hand-off.
        let _ = write_owner(
            &lease_dir,
            &Owner {
                schema_version: 1,
                host: context.identity.host,
                previous: Some(target.source),
                operation_id: input.operation_id.clone(),
                session_id: None,
                at_unix: target.clock.unix_seconds(),
            },
        );
    }
    receipt
}

#[cfg(test)]
// The worktree driver compiles this module without its harness, which leaves
// the shared helpers unused there.
#[allow(
    dead_code,
    unused_imports,
    reason = "the custom worktree driver omits libtest functions"
)]
#[path = "handoff_tests.rs"]
mod tests;
