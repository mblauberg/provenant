//! Both ends' local hand-off rules: the source's prompt and its worktree
//! view, the target's admission, delivery staging and launch script, receipt
//! vetting and the per-project owner lease. The command edge composes Git,
//! worktrees, accounts and the peer call around them.
use super::{
    ATTACHMENT_COUNT, HandoffReceipt, HandoffWorktree, PROMPT_HEADER, PROMPT_LIMIT, SessionHandoff,
};
use crate::hosts::plain_text;
use provenant_core::{HostId, OperationId, ProjectId};
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fmt::Write as _,
    fs,
    io::{self, Write as _},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

/// The source waits `peer_seconds`; the peer works to less, so a slow
/// recreation is reported by the peer rather than timing the source out.
const PEER_MARGIN: u64 = 5;
/// The peer's budget: the source's less a margin, never above the peer's own
/// limit. A source that sent no budget gets the peer's limit less the margin.
pub fn peer_seconds(source_seconds: Option<u64>, local_limit: u64) -> u64 {
    source_seconds
        .map_or(local_limit, |s| s.min(local_limit))
        .saturating_sub(PEER_MARGIN)
        .max(1)
}

/// Which host owns this project's chair or session. Each host keeps its own
/// record, and both flip it to the target on a delivered hand-off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub schema_version: u64,
    pub host: HostId,
    pub previous: Option<HostId>,
    pub operation_id: OperationId,
    pub session_id: Option<String>,
    pub at_unix: i64,
}
pub fn handoff_dir(state: &Path, project: ProjectId) -> PathBuf {
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
pub fn read_owner(dir: &Path) -> Option<Owner> {
    let bytes = fs::read(dir.join("owner.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}
pub fn write_owner(dir: &Path, owner: &Owner) -> io::Result<()> {
    private_dirs(dir)?;
    let staged = dir.join(format!(
        ".owner.{}.{}.tmp",
        std::process::id(),
        provenant_os::fs::temporary_name()?.to_string_lossy()
    ));
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

/// The branch name of a full `refs/heads/` ref.
pub fn short_branch(reference: &str) -> String {
    reference
        .strip_prefix("refs/heads/")
        .unwrap_or(reference)
        .to_owned()
}

/// One worktree of this project, as the hand-off sees it.
#[derive(Debug, Clone, Serialize)]
pub struct Tree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub primary: bool,
    pub dirty: bool,
    pub unknown: bool,
    pub live_writer: bool,
    /// `origin/<branch>` when it exists locally.
    pub upstream: Option<String>,
    /// The head is on `origin/<branch>`.
    pub pushed: bool,
}
impl Tree {
    pub fn problems(&self) -> Vec<&'static str> {
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
    pub fn describe(&self) -> String {
        format!(
            "{} ({}): {}",
            self.path.display(),
            self.branch.as_deref().unwrap_or("detached"),
            self.problems().join(", ")
        )
    }
}

#[derive(Clone, Copy)]
pub struct PromptInput<'a> {
    pub source: HostId,
    pub agent: &'a str,
    pub session: Option<&'a str>,
    pub body: Option<&'a str>,
    pub checkpoint: Option<&'a str>,
    pub trees: &'a [Tree],
    pub lanes: &'a Result<Vec<String>, String>,
    pub notes: &'a [String],
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

#[derive(Debug, thiserror::Error)]
#[error("the hand-off prompt exceeds 32 KiB; shorten the prompt file or the notes")]
pub struct PromptTooLarge;

/// The hand-off prompt, cutting the checkpoint so the whole prompt fits
/// [`PROMPT_LIMIT`]. A prompt file or notes that cannot fit are refused.
pub fn compose(input: &PromptInput<'_>) -> Result<String, PromptTooLarge> {
    let full = build_prompt(input);
    if full.len() <= PROMPT_LIMIT {
        return Ok(full);
    }
    let Some(checkpoint) = input.checkpoint.filter(|_| input.body.is_none()) else {
        return Err(PromptTooLarge);
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
        .ok_or(PromptTooLarge)?;
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
        return Err(PromptTooLarge);
    }
    Ok(prompt)
}

/// Opens the stored launch script. Tests substitute a recorder, so no test
/// ever opens Terminal or starts `claude`.
pub trait Launcher {
    fn launch(&self, script: &Path) -> Result<(), String>;
}
/// `open -a Terminal <script>`: a new Terminal window runs the script in the
/// user's login session.
pub struct Terminal;
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

fn refused(code: &str, message: impl Into<String>) -> HandoffReceipt {
    HandoffReceipt::Refused {
        code: code.into(),
        message: message.into(),
        accounts: Vec::new(),
    }
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
pub struct Delivery<'a> {
    pub dir: PathBuf,
    pub project: &'a Path,
    pub state: &'a Path,
    pub account_label: &'a str,
    pub account: PathBuf,
    pub model: Option<&'a str>,
    pub prompt: &'a str,
    pub source_label: String,
    pub worktrees: Vec<HandoffWorktree>,
    pub attachments: Vec<(String, Vec<u8>)>,
    pub launch: bool,
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
pub fn deliver(delivery: Delivery<'_>, launcher: &dyn Launcher) -> HandoffReceipt {
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

/// The peer's refusal code as this command's error code: upper-case letters
/// and underscores only, so it cannot carry text. The peer's own lower-case
/// codes read as their upper-case form.
pub fn refusal_code(code: &str) -> String {
    let code: String = code
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .filter(|c| c.is_ascii_uppercase() || *c == '_')
        .take(64)
        .collect();
    if code.is_empty() {
        "HANDOFF_REFUSED".into()
    } else {
        code
    }
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

/// The peer's receipt names other branches than the source requested.
#[derive(Debug, thiserror::Error)]
#[error(
    "the peer's receipt names branches ({got}) that differ from the requested ones \
     ({expected}); the peer may already have opened a session, so check it before retrying"
)]
pub struct ReceiptMismatch {
    got: String,
    expected: String,
}

/// The peer's receipt as the caller may show it: the worktrees are the
/// branches that were requested, and every string is plain and bounded.
pub fn vet_receipt(
    receipt: HandoffReceipt,
    requested: &[String],
) -> Result<HandoffReceipt, ReceiptMismatch> {
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
                return Err(ReceiptMismatch {
                    got: plain(&got.join(", ")),
                    expected: expected.join(", "),
                });
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

/// Attachments to store, the account's directory and the claimed operation
/// directory.
pub type Admitted = (Vec<(String, Vec<u8>)>, PathBuf, PathBuf);
/// Everything `receive` settles before it writes or opens anything: the
/// opt-in, the input, the account and the operation directory, in that order.
/// Only the last step touches the disk. `account` looks up a registered
/// Claude account's directory by label.
pub fn admit(
    accepted: bool,
    input: &SessionHandoff,
    lease_dir: &Path,
    account: impl FnOnce(&str) -> Result<Option<PathBuf>, String>,
) -> Result<Admitted, Box<HandoffReceipt>> {
    if let Some(receipt) = gate(accepted) {
        return Err(Box::new(receipt));
    }
    let attachments = input
        .validate()
        .map_err(|error| Box::new(refused("invalid_input", error.to_string())))?;
    let account = account(&input.account)
        .map_err(|error| {
            Box::new(refused(
                "accounts_unavailable",
                format!("cannot read this host's registered accounts: {error}"),
            ))
        })?
        .ok_or_else(|| HandoffReceipt::Refused {
            code: "account_unregistered".into(),
            message: format!(
                "{} is not a registered Claude account on this host",
                input.account
            ),
            accounts: Vec::new(),
        })?;
    let dir = operation_dir(lease_dir, &input.operation_id);
    match reserve(&dir) {
        Ok(true) => Ok((attachments, account, dir)),
        Ok(false) => Err(Box::new(refused(
            "already_delivered",
            format!(
                "{} already holds this hand-off; it is never launched twice",
                dir.display()
            ),
        ))),
        Err(error) => Err(Box::new(refused(
            "io",
            format!("cannot store the hand-off: {error}"),
        ))),
    }
}
/// The target records itself as owner only for a session that opened, or for
/// a `--no-launch` delivery the operator will start by hand.
pub fn takes_lease(receipt: &HandoffReceipt, launch: bool) -> bool {
    matches!(receipt, HandoffReceipt::Delivered { launched, .. } if *launched || !launch)
}

#[cfg(test)]
mod tests;
