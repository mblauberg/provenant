//! `workflow session handoff`: start a fresh Claude session on a peer from a
//! hand-off prompt. The source checks its worktrees, saves the checkpoint and
//! sends one `session_handoff` peer action. The target checks the account
//! against its own registrations, recreates each branch's worktree from
//! origin, stores the prompt and opens Terminal through a launcher seam.
use provenant_cli::{
    Globals,
    fabric::{Context, PeerFailure, facade_outcome},
    workflow::Handoff,
};
use provenant_confinement::worktrees::{self, WorktreeCondition, WorktreeCreate, WriterState};
use provenant_core::{CancelToken, Clock, HostId, OperationId, ProjectId, WaitBudget};
use provenant_fabric_runtime::{
    coordination::facade::FacadeError,
    handoff::{
        ATTACHMENT_COUNT, ATTACHMENT_LIMIT, Attachment, HandoffReceipt, HandoffWorktree,
        PROMPT_HEADER, PROMPT_LIMIT, SessionHandoff, encode_base64, valid_attachment_name,
        valid_model,
    },
    hosts::{PeerAction, PeerResult, plain_text},
};
use provenant_records::{CommandResult, Outcome};
use provenant_supervisor::{
    clock::SystemClock,
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
};
use provenant_workflow::session::{HostSessionId, SessionStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fmt::Write as _,
    fs,
    io::{self, Read, Write as _},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const COMMAND: &str = "workflow session handoff";
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Kept back from the peer budget for staging, launch and the reply.
const RESERVE: Duration = Duration::from_secs(4);
/// The source waits `peer_seconds`; the peer works to less, so a slow
/// recreation is reported by the peer rather than timing the source out.
const PEER_MARGIN: u64 = 5;
pub(crate) fn peer_seconds(source_seconds: u64) -> u64 {
    source_seconds.saturating_sub(PEER_MARGIN).max(1)
}

#[derive(Debug)]
struct Refusal {
    status: Outcome,
    code: &'static str,
    message: String,
    /// Result data and human text kept with the failure.
    data: Option<(Value, String)>,
}
impl Refusal {
    fn new(status: Outcome, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            data: None,
        }
    }
    fn with_data(mut self, data: Value, human: String) -> Self {
        self.data = Some((data, human));
        self
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new(Outcome::Invalid, "INVALID", message)
    }
    fn refused(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Outcome::Refused, code, message)
    }
}
impl<E: std::fmt::Display> From<E> for Refusal {
    fn from(error: E) -> Self {
        Self::new(Outcome::Error, "IO", error.to_string())
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| i64::try_from(t.as_secs()).unwrap_or(i64::MAX))
}

/// Which host owns this project's chair or session. Each host keeps its own
/// record, and both flip it to the target on a delivered hand-off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Owner {
    pub schema_version: u64,
    pub host: HostId,
    pub previous: Option<HostId>,
    pub operation_id: OperationId,
    pub session_id: Option<String>,
    pub at_unix: i64,
}
fn handoff_dir(state: &Path, project: ProjectId) -> PathBuf {
    state.join("handoff").join(project.to_string())
}
/// The prefix keeps an operation ID from naming `owner.json` or a staged file.
fn operation_dir(lease_dir: &Path, operation: &OperationId) -> PathBuf {
    lease_dir.join(format!("op-{}", operation.as_str()))
}
fn private_dirs(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}
fn read_owner(dir: &Path) -> Option<Owner> {
    let bytes = fs::read(dir.join("owner.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}
fn write_owner(dir: &Path, owner: &Owner) -> io::Result<()> {
    private_dirs(dir)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| t.as_nanos());
    let staged = dir.join(format!(".owner.{}.{nanos}.tmp", std::process::id()));
    let result = write_new(&staged, &serde_json::to_vec_pretty(owner)?, 0o600)
        .and_then(|()| fs::rename(&staged, dir.join("owner.json")));
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn git(dir: &Path, args: &[&str], timeout: Duration) -> Result<(bool, String), Refusal> {
    let binary = crate::roots::git_binary()
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

/// One worktree of this project, as the hand-off sees it.
#[derive(Debug, Clone, Serialize)]
struct Tree {
    path: PathBuf,
    branch: Option<String>,
    head: String,
    primary: bool,
    dirty: bool,
    unknown: bool,
    live_writer: bool,
    /// `origin/<branch>` when it exists locally.
    upstream: Option<String>,
    /// The head is on `origin/<branch>`.
    pushed: bool,
}
impl Tree {
    fn problems(&self) -> Vec<&'static str> {
        let mut problems = Vec::new();
        if self.branch.is_none() {
            problems.push("detached HEAD");
        }
        if self.unknown {
            problems.push("state unknown");
        }
        if self.dirty {
            problems.push("uncommitted changes");
        }
        if self.branch.is_some() && self.upstream.is_none() {
            problems.push("no origin branch");
        } else if self.branch.is_some() && !self.pushed {
            problems.push("commits not on origin");
        }
        problems
    }
    fn describe(&self) -> String {
        format!(
            "{} ({}): {}",
            self.path.display(),
            self.branch.as_deref().unwrap_or("detached"),
            self.problems().join(", ")
        )
    }
}

fn inspect(roots: &provenant_core::Roots, project: &Path) -> Result<Vec<Tree>, Refusal> {
    let infos = crate::worktrees::with_context(roots, project, GIT_TIMEOUT, |context, cancel| {
        Ok(worktrees::list(context, cancel)?)
    })?;
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
        let (upstream, pushed) = match info.branch.as_deref() {
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
            branch: info.branch,
            head,
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
    step(&["push", "--set-upstream", "origin", branch])
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

#[derive(Clone, Copy)]
struct PromptInput<'a> {
    source: HostId,
    agent: &'a str,
    session: Option<&'a str>,
    body: Option<&'a str>,
    checkpoint: Option<&'a str>,
    trees: &'a [Tree],
    lanes: &'a Result<Vec<String>, String>,
    notes: &'a [String],
}
fn build_prompt(input: &PromptInput<'_>) -> String {
    let mut text = String::new();
    if let Some(body) = input.body {
        if !body.starts_with(PROMPT_HEADER) {
            let _ = writeln!(text, "{PROMPT_HEADER}\n");
        }
        text.push_str(body.trim_end());
        text.push('\n');
    } else {
        let _ = writeln!(text, "{PROMPT_HEADER}\n");
        let _ = writeln!(
            text,
            "You are taking over a session handed off from host `{}` (agent `{}`{}). This is a \
             fresh session with no transcript: treat this prompt as the briefing. Keep the \
             authority limits recorded in the checkpoint; the hand-off grants nothing new.",
            input.source,
            input.agent,
            input
                .session
                .map(|id| format!(", session `{id}`"))
                .unwrap_or_default()
        );
        let _ = writeln!(text, "\n## Checkpoint\n");
        match input.checkpoint {
            Some(checkpoint) => {
                let _ = writeln!(text, "{}", checkpoint.trim_end());
            }
            None => {
                let _ = writeln!(text, "No checkpoint was saved for this session.");
            }
        }
        let _ = writeln!(text, "\n## Branches\n");
        for tree in input.trees {
            let branch = tree.branch.as_deref().unwrap_or("detached");
            let role = if tree.primary {
                " (primary checkout)"
            } else {
                ""
            };
            let problems = tree.problems();
            if problems.is_empty() {
                let _ = writeln!(text, "- `{branch}`{role} at `{}`, pushed", tree.head);
            } else {
                let _ = writeln!(
                    text,
                    "- `{branch}`{role} at `{}`: {} on the source host",
                    tree.head,
                    problems.join(", ")
                );
            }
        }
        let _ = writeln!(text, "\n## Lanes running on the source host\n");
        match input.lanes {
            Ok(lanes) if lanes.is_empty() => {
                let _ = writeln!(text, "None.");
            }
            Ok(lanes) => {
                for lane in lanes {
                    let _ = writeln!(text, "- `{lane}`");
                }
            }
            Err(error) => {
                let _ = writeln!(text, "Lane snapshot unavailable: {error}");
            }
        }
    }
    if !input.notes.is_empty() {
        let _ = writeln!(text, "\n## Notes\n");
        for note in input.notes {
            let _ = writeln!(text, "- {}", note.trim());
        }
    }
    text
}

/// [`build_prompt`], cutting the checkpoint so the whole prompt fits
/// [`PROMPT_LIMIT`]. A prompt file or notes that cannot fit are refused.
fn compose(input: &PromptInput<'_>) -> Result<String, Refusal> {
    let full = build_prompt(input);
    if full.len() <= PROMPT_LIMIT {
        return Ok(full);
    }
    let too_large = || {
        Refusal::invalid("the hand-off prompt exceeds 32 KiB; shorten the prompt file or the notes")
    };
    let Some(checkpoint) = input.checkpoint.filter(|_| input.body.is_none()) else {
        return Err(too_large());
    };
    const CUT: &str = "\n\n[Checkpoint cut to fit the hand-off. Read the rest with \
                       `provenant workflow session show` on the source host.]";
    let overhead = build_prompt(&PromptInput {
        checkpoint: Some(""),
        ..*input
    })
    .len();
    let budget = PROMPT_LIMIT
        .checked_sub(overhead + CUT.len())
        .filter(|budget| *budget > 0)
        .ok_or_else(too_large)?;
    let mut end = budget.min(checkpoint.len());
    while !checkpoint.is_char_boundary(end) {
        end -= 1;
    }
    let cut = format!("{}{CUT}", &checkpoint[..end]);
    let prompt = build_prompt(&PromptInput {
        checkpoint: Some(&cut),
        ..*input
    });
    if prompt.len() > PROMPT_LIMIT {
        return Err(too_large());
    }
    Ok(prompt)
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
            name: name.to_owned(),
            base64: encode_base64(&bytes),
        });
    }
    Ok(attachments)
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
    let cwd = crate::roots::selected_project(globals.project.as_deref(), false)?;
    let assets = crate::roots::product_root()?;
    let roots = crate::roots::roots(globals, &assets, &cwd)?;
    let home = crate::roots::home()?;
    let context = Context::open(roots.clone(), &home, &cwd)?;
    Ok(Source {
        roots,
        home,
        assets,
        cwd,
        context,
    })
}

fn execute(args: &Handoff, globals: &Globals) -> Result<(Value, String), Refusal> {
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
    let src = open_source(globals)?;
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
    let store = session
        .as_ref()
        .filter(|_| !args.no_checkpoint)
        .map(|id| SessionStore::new(src.roots.state.clone(), project, id.clone()));
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
        })?;
        let input = SessionHandoff {
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
    let mut trees = inspect(&src.roots, &src.cwd)?;
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
                trees = inspect(&src.roots, &src.cwd)?;
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
            input: Box::new(input),
        },
        budget,
        &cancel,
    );
    let receipt = match reply {
        Ok(PeerResult::SessionHandoff(receipt)) => receipt,
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
            let (status, code) = facade_outcome(&error);
            return Err(Refusal::new(
                status,
                code,
                format!(
                    "peer {} refused or is unavailable: {error}{detail}",
                    args.to
                ),
            ));
        }
    };
    let receipt = vet_receipt(receipt, &branches)?;
    match &receipt {
        HandoffReceipt::Refused {
            code,
            message,
            accounts,
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
                "HANDOFF_REFUSED",
                format!(
                    "{} refused the hand-off ({code}): {message}{listed}",
                    args.to
                ),
            ));
        }
        HandoffReceipt::Delivered { .. } => {}
    }
    // The peer holds the hand-off: record it as this session's checkpoint.
    let checkpoint_error = store.as_ref().and_then(|store| {
        WaitBudget::cli(clock.mono_now(), 30)
            .map_err(|e| e.to_string())
            .and_then(|budget| {
                store
                    .save(prompt.as_bytes(), budget, &cancel, &clock)
                    .map_err(|e| e.to_string())
            })
            .err()
    });
    data["checkpoint_error"] = json!(checkpoint_error);
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

pub fn run(args: Handoff, globals: &Globals) -> u8 {
    let dry_run = args.dry_run;
    match execute(&args, globals) {
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
                code: code.into(),
                message,
                cause: None,
                repair: None,
            });
            crate::emit(&result, globals, &human, false)
        }
        Err(refusal) => crate::fail(
            COMMAND,
            refusal.status,
            refusal.code,
            &refusal.message,
            globals,
        ),
    }
}

/// Opens the stored launch script. Tests substitute a recorder, so no test
/// ever opens Terminal or starts `claude`.
pub(crate) trait Launcher {
    fn launch(&self, script: &Path) -> Result<(), String>;
}
/// `open -a Terminal <script>`: a new Terminal window runs the script in the
/// user's login session.
pub(crate) struct Terminal;
impl Launcher for Terminal {
    fn launch(&self, script: &Path) -> Result<(), String> {
        let argv: Vec<OsString> = vec![
            "/usr/bin/open".into(),
            "-a".into(),
            "Terminal".into(),
            script.as_os_str().to_owned(),
        ];
        let dir = script.parent().unwrap_or(Path::new("/"));
        let mut options = ExecOptions::new(&argv, dir, Duration::from_secs(10));
        options.stdout = StreamPolicy::Discard;
        options.stderr = StreamPolicy::Capture { limit: 4096 };
        let out = SystemExecutor::new(1)
            .and_then(|executor| executor.execute(options))
            .map_err(|e| e.to_string())?;
        if out.termination != Termination::Exit(0) || out.timed_out {
            return Err(format!(
                "open -a Terminal failed: {}",
                String::from_utf8_lossy(&out.stderr.bytes).trim()
            ));
        }
        Ok(())
    }
}

pub(crate) struct Target<'a> {
    pub context: &'a Context,
    /// The project's primary checkout on this host.
    pub cwd: &'a Path,
    pub source: HostId,
    pub clock: &'a SystemClock,
    pub budget: WaitBudget,
}
fn refused(code: &str, message: impl Into<String>) -> HandoffReceipt {
    HandoffReceipt::Refused {
        code: code.into(),
        message: message.into(),
        accounts: Vec::new(),
    }
}
/// The registered account directory for `label`, or the labels this host
/// has registered.
fn account_directory(
    accounts: &[provenant_providers::identity::RegisteredAccount],
    label: &str,
) -> Result<PathBuf, Vec<String>> {
    accounts
        .iter()
        .find(|a| a.key() == label)
        .map(|a| a.directory().to_path_buf())
        .ok_or_else(|| accounts.iter().map(|a| a.key().to_owned()).collect())
}
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
fn script_path(path: &Path) -> Result<&str, String> {
    path.to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .ok_or_else(|| format!("{} is not UTF-8 without control characters", path.display()))
}
/// The Terminal script. The prompt is read from its stored file, so no prompt
/// text passes through shell quoting, and `--` ends `claude`'s options, so
/// the prompt is never read as one.
fn script(
    project: &Path,
    account: &Path,
    state: &Path,
    model: Option<&str>,
    source: &str,
    prompt: &Path,
) -> Result<String, String> {
    let model = model
        .map(|m| format!(" --model {}", quote(m)))
        .unwrap_or_default();
    Ok(format!(
        "#!/bin/sh\n# Provenant session hand-off.\ncd {} || exit 1\nexport CLAUDE_CONFIG_DIR={}\nexport PROVENANT_STATE_DIR={}\nprintf '%s\\n%s\\n' {} {}\nexec claude{model} -- \"$(cat {})\"\n",
        quote(script_path(project)?),
        quote(script_path(account)?),
        quote(script_path(state)?),
        quote(&format!(
            "Provenant session hand-off from {}.",
            plain_text(source, 200)
        )),
        quote(&format!("Prompt: {}", script_path(prompt)?)),
        quote(script_path(prompt)?),
    ))
}

/// What the target has settled before it writes anything.
struct Delivery<'a> {
    dir: PathBuf,
    project: &'a Path,
    state: &'a Path,
    account_label: &'a str,
    account: PathBuf,
    model: Option<&'a str>,
    prompt: &'a str,
    source_label: String,
    worktrees: Vec<HandoffWorktree>,
    attachments: Vec<(String, Vec<u8>)>,
    launch: bool,
}
/// Claim the operation directory. False means a replay, which never launches
/// again.
fn reserve(dir: &Path) -> io::Result<bool> {
    if let Some(parent) = dir.parent() {
        private_dirs(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}
/// Store the prompt and attachments in the reserved operation directory, then
/// write and open the launch script.
fn deliver(delivery: Delivery<'_>, launcher: &dyn Launcher) -> HandoffReceipt {
    let staged = (|| -> io::Result<(PathBuf, Vec<String>)> {
        let mut stored = Vec::new();
        if !delivery.attachments.is_empty() {
            let folder = delivery.dir.join("attachments");
            fs::DirBuilder::new().mode(0o700).create(&folder)?;
            for (name, bytes) in &delivery.attachments {
                let path = folder.join(name);
                write_new(&path, bytes, 0o600)?;
                stored.push(path.to_string_lossy().into_owned());
            }
        }
        // The stored prompt always opens with the fixed header, whatever the
        // request held, so `claude` never reads it as an option.
        let mut prompt = String::new();
        if !delivery.prompt.starts_with(PROMPT_HEADER) {
            let _ = writeln!(prompt, "{PROMPT_HEADER}\n");
        }
        prompt.push_str(delivery.prompt.trim_end());
        let _ = write!(
            prompt,
            "\n\n## On this host\n\n- Project checkout: `{}`\n- Claude account: `{}`\n",
            delivery.project.display(),
            delivery.account_label
        );
        if !delivery.worktrees.is_empty() {
            prompt.push_str("- Worktrees:\n");
            for tree in &delivery.worktrees {
                let _ = match (&tree.path, &tree.error) {
                    (Some(path), Some(warning)) => writeln!(
                        prompt,
                        "  - `{}`: `{path}` (warning: {warning}; settle this before you work)",
                        tree.branch
                    ),
                    (Some(path), None) => writeln!(
                        prompt,
                        "  - `{}`: `{path}`{}",
                        tree.branch,
                        if tree.created {
                            " (created from origin)"
                        } else {
                            " (already present at origin's commit)"
                        }
                    ),
                    (None, error) => writeln!(
                        prompt,
                        "  - `{}`: not recreated ({}); create it with `provenant worktree create`",
                        tree.branch,
                        error.as_deref().unwrap_or("unknown")
                    ),
                };
            }
        }
        if !stored.is_empty() {
            prompt.push_str("- Attachments:\n");
            for path in &stored {
                let _ = writeln!(prompt, "  - `{path}`");
            }
        }
        let _ = writeln!(
            prompt,
            "- Lanes started before the hand-off keep running on the source host. Watch them \
             with `provenant lanes --host {}`, collect their results and never re-run completed \
             lanes.",
            delivery.source_label
        );
        let path = delivery.dir.join("prompt.md");
        write_new(&path, prompt.as_bytes(), 0o600)?;
        Ok((path, stored))
    })();
    let (prompt_path, attachments) = match staged {
        Ok(staged) => staged,
        Err(error) => return refused("io", format!("cannot store the hand-off: {error}")),
    };
    // The script is written either way, so `--no-launch` can run it by hand.
    let mut launched = false;
    let path = delivery.dir.join("launch.command");
    let written = script(
        delivery.project,
        &delivery.account,
        delivery.state,
        delivery.model,
        &delivery.source_label,
        &prompt_path,
    )
    .and_then(|text| write_new(&path, text.as_bytes(), 0o700).map_err(|e| e.to_string()));
    let launch_error = match written {
        Ok(()) if delivery.launch => match launcher.launch(&path) {
            Ok(()) => {
                launched = true;
                None
            }
            Err(error) => Some(error),
        },
        Ok(()) => None,
        Err(error) => Some(error),
    };
    let script_file = path.exists().then(|| path.to_string_lossy().into_owned());
    HandoffReceipt::Delivered {
        prompt_path: prompt_path.to_string_lossy().into_owned(),
        project: delivery.project.to_string_lossy().into_owned(),
        worktrees: delivery.worktrees,
        attachments,
        script_path: script_file,
        launched,
        launch_error,
    }
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
                target.cwd,
                remaining(),
                |context, cancel| {
                    if let Some(info) = worktrees::list(context, cancel)?
                        .into_iter()
                        .find(|w| w.branch.as_deref() == Some(branch.as_str()))
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
    if info.dirty != WorktreeCondition::Clean {
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

/// A hand-off opens a Claude session here, so the operator must opt in.
fn gate(accept_handoff: bool) -> Option<HandoffReceipt> {
    (!accept_handoff).then(|| {
        refused(
            "HANDOFF_DISABLED",
            "this host does not accept session hand-offs; turn it on here with \
             `provenant config set fabric.accept_handoff true`",
        )
    })
}

/// Longest receipt string shown to the caller.
const RECEIPT_TEXT: usize = 512;
const RECEIPT_ROWS: usize = 64;
fn plain(text: &str) -> String {
    plain_text(text, RECEIPT_TEXT)
}
fn plain_opt(text: Option<String>) -> Option<String> {
    text.map(|t| plain(&t))
}
/// The peer's receipt as the caller may show it: the worktrees are the
/// branches that were requested, and every string is plain and bounded.
fn vet_receipt(receipt: HandoffReceipt, requested: &[String]) -> Result<HandoffReceipt, Refusal> {
    Ok(match receipt {
        HandoffReceipt::Refused {
            code,
            message,
            accounts,
        } => HandoffReceipt::Refused {
            code: plain(&code),
            message: plain(&message),
            accounts: accounts
                .iter()
                .take(RECEIPT_ROWS)
                .map(|a| plain(a))
                .collect(),
        },
        HandoffReceipt::Delivered {
            prompt_path,
            project,
            worktrees,
            attachments,
            script_path,
            launched,
            launch_error,
        } => {
            let mut expected = requested.to_vec();
            expected.sort();
            let mut got: Vec<&str> = worktrees.iter().map(|t| t.branch.as_str()).collect();
            got.sort_unstable();
            if got != expected {
                return Err(Refusal::new(
                    Outcome::Error,
                    "HANDOFF_RECEIPT",
                    format!(
                        "the peer's receipt names branches ({}) that differ from the requested \
                         ones ({}); the peer may already have opened a session, so check it \
                         before retrying",
                        plain(&got.join(", ")),
                        expected.join(", ")
                    ),
                ));
            }
            HandoffReceipt::Delivered {
                prompt_path: plain(&prompt_path),
                project: plain(&project),
                worktrees: worktrees
                    .into_iter()
                    .map(|t| HandoffWorktree {
                        branch: plain(&t.branch),
                        path: plain_opt(t.path),
                        head: plain_opt(t.head),
                        created: t.created,
                        error: plain_opt(t.error),
                    })
                    .collect(),
                attachments: attachments
                    .iter()
                    .take(ATTACHMENT_COUNT)
                    .map(|a| plain(a))
                    .collect(),
                script_path: plain_opt(script_path),
                launched,
                launch_error: plain_opt(launch_error),
            }
        }
    })
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
        &provenant_supervisor::fs::LocalFs,
        &provenant_cli::domains::SECTIONS,
    )
    .ok()
    .and_then(|config| {
        config
            .knobs
            .get("fabric.accept_handoff")
            .and_then(|k| k.value.as_bool())
    });
    if let Some(receipt) = gate(accepted == Some(true)) {
        return receipt;
    }
    let attachments = match input.validate() {
        Ok(attachments) => attachments,
        Err(error) => return refused("invalid_input", error.to_string()),
    };
    let context = target.context;
    let accounts = match crate::quota_live::registered_claude(&context.roots) {
        Ok(accounts) => accounts,
        Err(error) => {
            return refused(
                "accounts_unavailable",
                format!("cannot read this host's registered accounts: {error}"),
            );
        }
    };
    let account = match account_directory(&accounts, &input.account) {
        Ok(account) => account,
        Err(labels) => {
            return HandoffReceipt::Refused {
                code: "account_unregistered".into(),
                message: format!(
                    "{} is not a registered Claude account on this host",
                    input.account
                ),
                accounts: labels,
            };
        }
    };
    let lease_dir = handoff_dir(context.roots.state.as_path(), context.identity.project);
    let dir = operation_dir(&lease_dir, &input.operation_id);
    match reserve(&dir) {
        Ok(true) => {}
        Ok(false) => {
            return refused(
                "already_delivered",
                format!(
                    "{} already holds this hand-off; it is never launched twice",
                    dir.display()
                ),
            );
        }
        Err(error) => return refused("io", format!("cannot store the hand-off: {error}")),
    }
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
    if matches!(receipt, HandoffReceipt::Delivered { launched, .. } if launched || !input.launch) {
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
#[allow(dead_code)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records launches instead of opening Terminal.
    #[derive(Default)]
    struct Recorder(RefCell<Vec<PathBuf>>);
    impl Launcher for Recorder {
        fn launch(&self, script: &Path) -> Result<(), String> {
            self.0.borrow_mut().push(script.to_path_buf());
            Ok(())
        }
    }
    fn tree(branch: Option<&str>, primary: bool, dirty: bool, pushed: bool) -> Tree {
        Tree {
            path: PathBuf::from(format!("/repo/{}", branch.unwrap_or("detached"))),
            branch: branch.map(Into::into),
            head: "a".repeat(40),
            primary,
            dirty,
            unknown: false,
            live_writer: false,
            upstream: branch.filter(|_| pushed).map(|_| "a".repeat(40)),
            pushed,
        }
    }
    fn delivery(dir: &Path, launch: bool) -> Delivery<'static> {
        Delivery {
            dir: operation_dir(dir, &OperationId::parse("handoff-1").unwrap()),
            project: Path::new("/Users/me/Repos/project"),
            state: Path::new("/Users/me/.local/share/provenant/v1"),
            account_label: "work",
            account: PathBuf::from("/Users/me/.claude-work"),
            model: Some("opus"),
            prompt: "# Session hand-off\n\nBriefing.",
            source_label: "laptop".into(),
            worktrees: vec![HandoffWorktree {
                branch: "feat/x".into(),
                path: Some("/Users/me/Repos/project/.worktrees/feat-x".into()),
                head: Some("b".repeat(40)),
                created: true,
                error: None,
            }],
            attachments: vec![("notes.md".into(), b"hello".to_vec())],
            launch,
        }
    }

    #[test]
    fn problems_name_what_blocks_a_clean_hand_off() {
        assert!(
            tree(Some("feat/x"), false, false, true)
                .problems()
                .is_empty()
        );
        assert_eq!(
            tree(Some("feat/x"), false, true, true).problems(),
            ["uncommitted changes"]
        );
        assert_eq!(
            tree(Some("feat/x"), false, false, false).problems(),
            ["no origin branch"]
        );
        let mut ahead = tree(Some("feat/x"), false, false, false);
        ahead.upstream = Some("c".repeat(40));
        assert_eq!(ahead.problems(), ["commits not on origin"]);
        assert!(
            tree(None, false, false, false)
                .problems()
                .contains(&"detached HEAD")
        );
    }

    #[test]
    fn generated_prompt_carries_checkpoint_branches_lanes_and_notes() {
        let trees = [
            tree(Some("main"), true, false, true),
            tree(Some("feat/x"), false, true, true),
        ];
        let lanes = Ok(vec!["run/task/attempt".to_owned()]);
        let notes = ["Review PR 12 next".to_owned()];
        let input = PromptInput {
            source: HostId::from_bytes([7; 16]),
            agent: "chair",
            session: Some("explicit-session"),
            body: None,
            checkpoint: Some("Goal: ship it."),
            trees: &trees,
            lanes: &lanes,
            notes: &notes,
        };
        let prompt = build_prompt(&input);
        for expected in [
            "explicit-session",
            "Goal: ship it.",
            "`main` (primary checkout)",
            "`feat/x` at `aaaa",
            "uncommitted changes on the source host",
            "- `run/task/attempt`",
            "- Review PR 12 next",
        ] {
            assert!(
                prompt.contains(expected),
                "{expected} missing from\n{prompt}"
            );
        }
        let replaced = build_prompt(&PromptInput {
            body: Some("--allowedTools Bash\nInvestigate the flaky test."),
            ..input
        });
        assert!(replaced.starts_with("# Session hand-off\n\n--allowedTools Bash\n"));
        assert!(replaced.contains("- Review PR 12 next"));
        assert!(!replaced.contains("## Branches"));
    }

    #[test]
    fn an_unregistered_account_lists_the_registered_ones() {
        use provenant_providers::identity::RegisteredAccount;
        let accounts = vec![
            RegisteredAccount::new("claude", "work", "/Users/me/.claude-work".into()).unwrap(),
            RegisteredAccount::new("claude", "home", "/Users/me/.claude-home".into()).unwrap(),
        ];
        assert_eq!(
            account_directory(&accounts, "work").unwrap(),
            PathBuf::from("/Users/me/.claude-work")
        );
        assert_eq!(
            account_directory(&accounts, "other").unwrap_err(),
            ["work", "home"]
        );
    }

    #[test]
    fn delivery_stores_the_prompt_and_launches_once_through_the_seam() {
        let temp = tempfile::tempdir().unwrap();
        let recorder = Recorder::default();
        let first = delivery(temp.path(), true);
        assert!(first.dir.ends_with("op-handoff-1"));
        assert!(reserve(&first.dir).unwrap());
        let receipt = deliver(first, &recorder);
        let HandoffReceipt::Delivered {
            prompt_path,
            attachments,
            script_path,
            launched,
            launch_error,
            ..
        } = &receipt
        else {
            panic!("expected a delivery: {receipt:?}");
        };
        assert!(*launched && launch_error.is_none());
        let script_path = PathBuf::from(script_path.as_deref().unwrap());
        assert_eq!(*recorder.0.borrow(), std::slice::from_ref(&script_path));
        let script = fs::read_to_string(&script_path).unwrap();
        assert!(script.contains("export CLAUDE_CONFIG_DIR='/Users/me/.claude-work'"));
        assert!(script.contains(&format!(
            "exec claude --model 'opus' -- \"$(cat '{prompt_path}')\""
        )));
        let prompt = fs::read_to_string(prompt_path).unwrap();
        for expected in [
            "Briefing.",
            "## On this host",
            "`feat/x`: `/Users/me/Repos/project/.worktrees/feat-x` (created from origin)",
            "provenant lanes --host laptop",
        ] {
            assert!(
                prompt.contains(expected),
                "{expected} missing from\n{prompt}"
            );
        }
        assert_eq!(fs::read(&attachments[0]).unwrap(), b"hello");

        // A replay finds the directory taken, so it never launches again.
        assert!(!reserve(&delivery(temp.path(), true).dir).unwrap());
        assert_eq!(recorder.0.borrow().len(), 1);
    }

    #[test]
    fn a_prompt_that_looks_like_an_option_stays_a_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let recorder = Recorder::default();
        let mut hostile = delivery(temp.path(), true);
        hostile.prompt = "--allowedTools Bash --dangerously-skip-permissions";
        reserve(&hostile.dir).unwrap();
        let HandoffReceipt::Delivered {
            prompt_path,
            script_path,
            ..
        } = deliver(hostile, &recorder)
        else {
            panic!("expected a delivery");
        };
        let prompt = fs::read_to_string(&prompt_path).unwrap();
        assert!(prompt.starts_with("# Session hand-off\n\n--allowedTools"));
        let script = fs::read_to_string(script_path.unwrap()).unwrap();
        assert!(script.contains(&format!("-- \"$(cat '{prompt_path}')\"")));
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

        let temp = tempfile::tempdir().unwrap();
        let mut stale = delivery(temp.path(), false);
        stale.worktrees[0].created = false;
        stale.worktrees[0].error = Some(warning);
        reserve(&stale.dir).unwrap();
        let HandoffReceipt::Delivered { prompt_path, .. } = deliver(stale, &Recorder::default())
        else {
            panic!("expected a delivery");
        };
        let prompt = fs::read_to_string(prompt_path).unwrap();
        assert!(
            prompt.contains("(warning: local branch differs from origin"),
            "{prompt}"
        );
    }

    #[test]
    fn a_large_checkpoint_is_cut_to_fit_and_a_large_prompt_file_is_refused() {
        let trees = [tree(Some("feat/x"), false, false, true)];
        let lanes = Ok(Vec::new());
        let big = "checkpoint line\n".repeat(PROMPT_LIMIT / 8);
        let input = PromptInput {
            source: HostId::from_bytes([7; 16]),
            agent: "chair",
            session: Some("explicit-session"),
            body: None,
            checkpoint: Some(&big),
            trees: &trees,
            lanes: &lanes,
            notes: &[],
        };
        let prompt = compose(&input).unwrap();
        assert!(prompt.len() <= PROMPT_LIMIT);
        assert!(prompt.contains("[Checkpoint cut to fit the hand-off."));
        assert!(prompt.contains("## Branches"));
        let body = "x".repeat(PROMPT_LIMIT);
        assert!(
            compose(&PromptInput {
                body: Some(&body),
                ..input
            })
            .is_err()
        );
    }

    #[test]
    fn no_launch_writes_the_script_but_never_launches() {
        let temp = tempfile::tempdir().unwrap();
        let recorder = Recorder::default();
        let quiet = delivery(temp.path(), false);
        reserve(&quiet.dir).unwrap();
        let receipt = deliver(quiet, &recorder);
        assert!(matches!(
            receipt,
            HandoffReceipt::Delivered {
                launched: false,
                script_path: Some(_),
                launch_error: None,
                ..
            }
        ));
        assert!(recorder.0.borrow().is_empty());
    }

    #[test]
    fn script_quotes_paths_and_refuses_control_characters() {
        let text = script(
            Path::new("/Users/me/it's"),
            Path::new("/Users/me/.claude-work"),
            Path::new("/state"),
            None,
            "laptop",
            Path::new("/state/prompt.md"),
        )
        .unwrap();
        let notice = text.find("printf ").unwrap();
        assert!(text.contains("'Provenant session hand-off from laptop.'"));
        assert!(text.contains("'Prompt: /state/prompt.md'"));
        assert!(notice < text.find("exec claude").unwrap());
        assert!(text.contains("cd '/Users/me/it'\\''s' || exit 1"));
        assert!(text.contains("exec claude -- \"$(cat '/state/prompt.md')\""));
        assert!(
            script(
                Path::new("/a\nb"),
                Path::new("/c"),
                Path::new("/d"),
                None,
                "laptop",
                Path::new("/e")
            )
            .is_err()
        );
    }

    #[test]
    fn receive_refuses_until_the_host_opts_in() {
        let Some(HandoffReceipt::Refused { code, message, .. }) = gate(false) else {
            panic!("a host that has not opted in must refuse");
        };
        assert_eq!(code, "HANDOFF_DISABLED");
        assert!(message.contains("fabric.accept_handoff"));
        assert!(gate(true).is_none());
    }

    fn delivered(branch: &str, path: &str) -> HandoffReceipt {
        HandoffReceipt::Delivered {
            prompt_path: "/state/prompt.md".into(),
            project: "/repo".into(),
            worktrees: vec![HandoffWorktree {
                branch: branch.into(),
                path: Some(path.into()),
                head: None,
                created: true,
                error: Some("warn\x1b[2J".into()),
            }],
            attachments: Vec::new(),
            script_path: Some("/state/launch.command\x07".into()),
            launched: false,
            launch_error: Some("x".repeat(5000)),
        }
    }

    #[test]
    fn receipt_text_is_plain_and_bounded() {
        let requested = vec!["feat/x".to_owned()];
        let HandoffReceipt::Delivered {
            worktrees,
            script_path,
            launch_error,
            ..
        } = vet_receipt(delivered("feat/x", "/w\x1b]0;owned\x07"), &requested).unwrap()
        else {
            panic!("expected a delivery");
        };
        assert_eq!(worktrees[0].path.as_deref(), Some("/w]0;owned"));
        assert_eq!(worktrees[0].error.as_deref(), Some("warn[2J"));
        assert_eq!(script_path.as_deref(), Some("/state/launch.command"));
        assert_eq!(launch_error.unwrap().chars().count(), RECEIPT_TEXT);
        let HandoffReceipt::Refused { message, .. } = vet_receipt(
            HandoffReceipt::Refused {
                code: "c\x1b".into(),
                message: "m\x1b[31m".into(),
                accounts: vec!["a\n".into()],
            },
            &requested,
        )
        .unwrap() else {
            panic!("expected a refusal");
        };
        assert_eq!(message, "m[31m");
    }

    #[test]
    fn a_receipt_with_an_unrequested_branch_is_refused() {
        let requested = vec!["feat/x".to_owned()];
        let refusal = vet_receipt(delivered("evil\x1b[2J", "/w"), &requested).unwrap_err();
        assert_eq!(refusal.code, "HANDOFF_RECEIPT");
        assert!(!refusal.message.contains('\x1b'));
        assert!(vet_receipt(delivered("feat/x", "/w"), &[]).is_err());
    }

    #[test]
    fn a_delivery_that_did_not_launch_fails_unless_no_launch_was_given() {
        let mut receipt = delivered("feat/x", "/w");
        if let HandoffReceipt::Delivered { launch_error, .. } = &mut receipt {
            *launch_error = Some("no Terminal".into());
        }
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
    fn the_peer_works_to_less_than_the_source_waits() {
        assert_eq!(peer_seconds(30), 25);
        assert_eq!(peer_seconds(3), 1);
    }

    #[test]
    fn owner_lease_flips_and_reads_back() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("handoff/project");
        assert!(read_owner(&dir).is_none());
        let mut owner = Owner {
            schema_version: 1,
            host: HostId::from_bytes([2; 16]),
            previous: Some(HostId::from_bytes([1; 16])),
            operation_id: OperationId::parse("handoff-1").unwrap(),
            session_id: Some("explicit-session".into()),
            at_unix: 1,
        };
        write_owner(&dir, &owner).unwrap();
        assert_eq!(read_owner(&dir).unwrap(), owner);
        owner.host = HostId::from_bytes([1; 16]);
        owner.previous = Some(HostId::from_bytes([2; 16]));
        write_owner(&dir, &owner).unwrap();
        assert_eq!(read_owner(&dir).unwrap(), owner);
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
