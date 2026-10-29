//! `workflow session handoff`: start a fresh Claude session on a peer from a
//! hand-off prompt. The source checks its worktrees, saves the checkpoint and
//! sends one `session_handoff` peer action. The target checks the account
//! against its own registrations, recreates each branch's worktree from
//! origin, stores the prompt and opens Terminal through a launcher seam.
//! Fabric-runtime owns those rules; this edge composes Git, the worktree
//! owner, registered accounts, the run snapshot and the peer call.
use provenant_cli::{
    Globals,
    fabric::{Context, PeerFailure, facade_outcome},
    workflow::Handoff,
};
use provenant_confinement::worktrees::{
    self, WorkerObserver, WorktreeCondition, WorktreeCreate, WriterState,
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
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const COMMAND: &str = "workflow session handoff";
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Kept back from the peer budget for staging, launch and the reply.
const RESERVE: Duration = Duration::from_secs(4);

#[derive(Debug)]
struct Refusal {
    status: Outcome,
    code: String,
    message: String,
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
    fn code(&self) -> &str {
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

// Handoff lease records use current UTC wall time at this production edge.
#[allow(clippy::disallowed_methods)]
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| i64::try_from(t.as_secs()).unwrap_or(i64::MAX))
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
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    let out = SystemExecutor::new(1)?.execute(options)?;
    if out.timed_out {
        return Err(Refusal::new(
            Outcome::Timeout,
            "GIT",
            format!("git {} timed out", args.join(" ")),
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

/// Plain-text Git output for callers outside the handoff owner.
pub(crate) fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    match git(dir, args, GIT_TIMEOUT) {
        Ok((true, text)) => Ok(text),
        Ok((false, text)) => Err(text.trim().to_owned()),
        Err(refusal) => Err(refusal.message),
    }
}

fn inspect(src: &Source, workers: &dyn WorkerObserver) -> Result<Vec<Tree>, Refusal> {
    let project = src.cwd.as_path();
    let infos = crate::worktrees::with_context(
        &src.roots,
        &src.home,
        project,
        GIT_TIMEOUT,
        workers,
        |context, cancel| Ok(worktrees::list(context, cancel)?),
    )?;
    let (_, common) = git(
        project,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        GIT_TIMEOUT,
    )?;
    let common = PathBuf::from(common.trim());
    let primary = common
        .file_name()
        .is_some_and(|name| name == ".git")
        .then(|| common.parent().map(fs::canonicalize))
        .flatten()
        .transpose()?;
    let mut trees = Vec::with_capacity(infos.len());
    for info in infos {
        let head = info.head.to_string();
        // Worktree listings carry full refs; the hand-off speaks branch names.
        let branch = info.branch.as_deref().map(short_branch);
        let (upstream, pushed) = match branch.as_deref() {
            Some(branch) => {
                let remote = format!("refs/remotes/origin/{branch}");
                let (found, oid) = git(
                    project,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("{remote}^{{commit}}"),
                    ],
                    GIT_TIMEOUT,
                )?;
                if found {
                    let (contained, _) = git(
                        project,
                        &["merge-base", "--is-ancestor", &head, &remote],
                        GIT_TIMEOUT,
                    )?;
                    (Some(oid.trim().to_owned()), contained)
                } else {
                    (None, false)
                }
            }
            None => (None, false),
        };
        let path = fs::canonicalize(&info.path).unwrap_or(info.path);
        trees.push(Tree {
            primary: primary.as_deref() == Some(path.as_path()),
            path,
            branch,
            head,
            // Ignored files, such as build output and `.worktrees/`, are not
            // lost work.
            dirty: info.dirty == WorktreeCondition::Dirty,
            unknown: matches!(
                info.dirty,
                WorktreeCondition::Unknown | WorktreeCondition::GitOperationInProgress
            ),
            live_writer: info.writer == WriterState::Live,
            upstream,
            pushed,
        });
    }
    Ok(trees)
}

/// Commit a dirty tree as WIP and push its branch. Hooks run as usual.
fn wip(tree: &Tree) -> Result<(), Refusal> {
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
    step(&["push", "--set-upstream", "origin", &refspec])
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

struct Source {
    roots: provenant_core::Roots,
    home: PathBuf,
    assets: PathBuf,
    cwd: PathBuf,
    context: Context,
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

// Operation identity includes the current wall-time sample.
#[allow(clippy::disallowed_methods)]
fn execute(
    args: &Handoff,
    globals: &Globals,
    workers: &dyn WorkerObserver,
) -> Result<(Value, String), Refusal> {
    if provenant_cli::fabric::in_lane() {
        return Err(Refusal::refused(
            "lane_coordination_refused",
            "coordination mutations are not allowed inside a lane",
        ));
    }
    if args.provider != "claude" {
        return Err(Refusal::new(
            Outcome::Unavailable,
            "UNSUPPORTED",
            "only the claude provider can take a hand-off",
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
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |t| t.as_micros()),
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
    let mut trees = inspect(&src, workers)?;
    let mut input = request(&trees)?;
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
                .filter(|t| t.primary || t.branch.is_none() || t.live_writer || t.unknown)
                .map(|t| format!("  {}", t.describe()))
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
                        "--wip commits tracked changes in branch worktrees with no live writer, \
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
                    if let Err(mut refusal) = wip(tree) {
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
                trees = inspect(&src, workers)?;
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
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), context.config.limits.peer_seconds)?;
    let reply = context.peer(
        &clock,
        target,
        PeerAction::SessionHandoff {
            extra: Default::default(),
            input: Box::new(input),
            budget_seconds: Some(context.config.limits.peer_seconds),
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
        Err(PeerFailure {
            error: FacadeError::LaunchUnknown { operation_id },
            ..
        }) => {
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
            at_unix: now_unix(),
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
            result.error = Some(provenant_records::CommandError {
                code,
                message,
                cause: None,
                repair: None,
            });
            crate::emit(&result, globals, &human, false)
        }
        Err(refusal) => crate::fail(
            COMMAND,
            refusal.status,
            refusal.code(),
            &refusal.message,
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
/// Recreate each branch's worktree with this host's own worktree owner, from
/// `origin/<branch>`. A worktree already on the branch is kept. Failures are
/// reported per branch; the hand-off still lands.
fn recreate(target: &Target<'_>, branches: &[String]) -> Vec<HandoffWorktree> {
    let remaining = || {
        let left =
            Duration::from_micros(target.budget.remaining(target.clock.mono_now()).as_micros());
        left.saturating_sub(RESERVE)
    };
    let mut fetched = false;
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
            let base = crate::fabric::remote_base(
                target.cwd,
                &format!("origin/{branch}"),
                time / 2,
                !fetched,
            );
            fetched = true;
            let base = match base {
                Ok(base) => base,
                Err(error) => return failed(format!("origin/{branch}: {error}")),
            };
            let made = crate::worktrees::with_context(
                &target.context.roots,
                target.home,
                target.cwd,
                remaining(),
                target.workers,
                |context, cancel| {
                    if let Some(info) = worktrees::list(context, cancel)?
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
                    error: worktree_warning(&info, &base),
                },
                Err(error) => failed(error.to_string()),
            }
        })
        .collect()
}

/// Why a reused worktree is not a clean copy of `origin/<branch>`.
fn worktree_warning(
    info: &worktrees::WorktreeInfo,
    base: &provenant_core::GitOid,
) -> Option<String> {
    let mut found = Vec::new();
    if info.head != *base {
        found.push(format!(
            "local branch differs from origin (local {}, origin {base})",
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
    let accepted = provenant_config::read_with_domains(
        &target.context.roots,
        &provenant_cli::domains::SECTIONS,
    )
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
    let worktrees = recreate(target, &input.branches);
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
                at_unix: now_unix(),
            },
        );
    }
    receipt
}

#[cfg(test)]
// The worktree driver compiles this module without its harness, which leaves
// the shared helpers unused there.
#[allow(dead_code, unused_imports)]
mod tests {
    use super::*;

    #[test]
    fn account_directory_returns_only_a_registered_match() {
        use provenant_providers::identity::RegisteredAccount;
        let accounts = vec![
            RegisteredAccount::new("claude", "work", "/Users/me/.claude-work".into()).unwrap(),
            RegisteredAccount::new("claude", "home", "/Users/me/.claude-home".into()).unwrap(),
        ];
        assert_eq!(
            account_directory(&accounts, "work").unwrap(),
            PathBuf::from("/Users/me/.claude-work")
        );
        assert_eq!(account_directory(&accounts, "other"), None);
    }

    #[test]
    fn reused_worktrees_name_a_differing_head_or_dirty_tree() {
        let base = provenant_core::GitOid::parse(&"a".repeat(40)).unwrap();
        let mut info = worktrees::WorktreeInfo {
            path: "/repo/.worktrees/x".into(),
            branch: Some("feat/x".into()),
            head: base,
            dirty: WorktreeCondition::Clean,
            writer: WriterState::Free,
        };
        assert_eq!(worktree_warning(&info, &base), None);
        info.head = provenant_core::GitOid::parse(&"b".repeat(40)).unwrap();
        info.dirty = WorktreeCondition::Dirty;
        let warning = worktree_warning(&info, &base).unwrap();
        assert!(
            warning.contains("local branch differs from origin"),
            "{warning}"
        );
        assert!(warning.contains("uncommitted changes"), "{warning}");
    }

    #[test]
    fn a_delivery_that_did_not_launch_fails_unless_no_launch_was_given() {
        let receipt = HandoffReceipt::Delivered {
            extra: Default::default(),
            prompt_path: "/state/prompt.md".into(),
            project: "/repo".into(),
            worktrees: Vec::new(),
            attachments: Vec::new(),
            script_path: Some("/state/launch.command".into()),
            launched: false,
            launch_error: Some("no Terminal".into()),
        };
        let mut data = json!({"receipt": serde_json::to_value(&receipt).unwrap()});
        let failure = launch_failure("workshop", &receipt, false, &mut data).unwrap();
        assert_eq!(failure.code, "LAUNCH_FAILED");
        assert!(matches!(failure.status, Outcome::Error));
        let (kept, _) = failure.data.unwrap();
        assert_eq!(kept["receipt"]["outcome"], "delivered");
        let mut data = json!({});
        assert!(launch_failure("workshop", &receipt, true, &mut data).is_none());
        let mut launched = receipt.clone();
        if let HandoffReceipt::Delivered { launched: l, .. } = &mut launched {
            *l = true;
        }
        assert!(launch_failure("workshop", &launched, false, &mut data).is_none());
    }

    #[test]
    fn attachments_are_capped_and_named_plainly() {
        let temp = tempfile::tempdir().unwrap();
        let small = temp.path().join("notes.md");
        fs::write(&small, b"hello").unwrap();
        let read = read_attachments(std::slice::from_ref(&small)).unwrap();
        assert_eq!(read[0].name, "notes.md");
        assert_eq!(read[0].base64, encode_base64(b"hello"));
        assert!(read_attachments(&[small.clone(), small.clone()]).is_err());
        let big = temp.path().join("big.bin");
        fs::write(&big, vec![0; ATTACHMENT_LIMIT]).unwrap();
        assert!(read_attachments(std::slice::from_ref(&big)).is_ok());
        let err = read_attachments(&[big, small]).err().unwrap();
        assert!(err.message.contains("512 KiB"), "{}", err.message);
        assert!(read_attachments(&[temp.path().to_path_buf()]).is_err());
    }

    #[test]
    fn a_handoff_checkpoint_keeps_the_configured_number_of_revisions() {
        use provenant_core::{CancelToken, Clock, PathResolver, ProjectId, RootInputs, WaitBudget};
        struct Plain;
        impl PathResolver for Plain {
            fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
                Ok(path.to_path_buf())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let mut inputs = RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Plain).unwrap();
        fs::create_dir_all(roots.state.as_path()).unwrap();
        fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[gc]\nfree_floor='1B'\n[retention]\nsession_revisions=2\n",
        )
        .unwrap();
        let store = checkpoint_store(
            &roots,
            ProjectId::from_bytes([3; 16]),
            HostSessionId::parse("handoff-session").unwrap(),
        )
        .with_chair(
            provenant_core::HostId::from_bytes([4; 16]),
            provenant_core::ActorId::from_bytes([5; 16]),
        );
        let (clock, cancel) = (SystemClock::new(), CancelToken::new());
        for text in ["one\n", "two\n", "three\n", "four\n"] {
            let budget = WaitBudget::cli(clock.mono_now(), 30).unwrap();
            store
                .save(text.as_bytes(), budget, &cancel, &clock)
                .unwrap();
        }
        let kept: Vec<u64> = store
            .revisions()
            .unwrap()
            .iter()
            .map(|revision| revision.revision)
            .collect();
        assert_eq!(kept, [3, 4], "[retention] session_revisions applies");
        let repair = checkpoint_repair(
            &Globals {
                state_dir: Some(PathBuf::from("/custom state")),
                project: Some(PathBuf::from("/selected project")),
                ..Default::default()
            },
            Path::new("/selected project"),
            &HostSessionId::parse("handoff-session").unwrap(),
            Some(provenant_records::Harness::Codex),
        );
        assert!(checkpoint_identity(&store, &repair).is_ok());
        let other = checkpoint_store(
            &roots,
            ProjectId::from_bytes([3; 16]),
            HostSessionId::parse("handoff-session").unwrap(),
        )
        .with_chair(
            provenant_core::HostId::from_bytes([4; 16]),
            provenant_core::ActorId::from_bytes([6; 16]),
        );
        let refusal = checkpoint_identity(&other, &repair).err().unwrap();
        assert_eq!(refusal.code(), "IDENTITY");
        assert!(refusal.message.contains("provenant --state-dir '/custom state' --project '/selected project' workflow session save handoff-session --client codex --rebind --file <checkpoint.md>"));
    }

    #[test]
    fn checkpoint_repair_without_a_client_requires_a_harness() {
        let repair = checkpoint_repair(
            &Globals::default(),
            Path::new("/project"),
            &HostSessionId::parse("chair").unwrap(),
            None,
        );
        assert!(repair.contains("workflow session save chair --client HARNESS --rebind"));
    }

    #[test]
    fn explicit_session_ids_are_validated() {
        // Only explicit IDs: tests never read the live session's environment.
        assert_eq!(
            session_id(Some("explicit-session"))
                .ok()
                .flatten()
                .map(String::from),
            Some("explicit-session".into())
        );
        assert!(session_id(Some("Not Valid")).is_err());
    }
}
